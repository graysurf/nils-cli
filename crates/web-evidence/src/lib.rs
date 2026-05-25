mod cli;
mod completion;

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use clap::error::ErrorKind;
use reqwest::Url;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, HeaderMap, USER_AGENT};
use serde::Serialize;
use serde_json::{Value, json};

use cli::{CaptureArgs, Cli, Command, HttpMethod, OutputFormat};
use nils_common::cli_contract::exit;
use nils_common::fs::{display_path, normalize_path as normalize_absolute_path};
use nils_common::redact::{RedactedString, redact_text};

const EXIT_OK: i32 = exit::SUCCESS;
const EXIT_RUNTIME: i32 = exit::RUNTIME;
const EXIT_USAGE: i32 = exit::USAGE;

const CAPTURE_SCHEMA_VERSION: &str = "cli.web-evidence.capture.v1";
const SUMMARY_SCHEMA_VERSION: &str = "web-evidence.summary.v1";
const CAPTURE_COMMAND: &str = "web-evidence capture";

const SUMMARY_FILE: &str = "summary.json";
const HEADERS_FILE: &str = "headers.redacted.json";
const BODY_PREVIEW_FILE: &str = "body-preview.redacted.txt";

pub fn run() -> i32 {
    run_with_args(env::args_os())
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            let code = match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => err.exit_code(),
                _ => EXIT_USAGE,
            };
            let _ = err.print();
            return code;
        }
    };

    dispatch(cli)
}

fn dispatch(cli: Cli) -> i32 {
    match cli.command {
        Command::Capture(args) => run_capture(args),
        Command::Completion(args) => completion::run(args.shell),
    }
}

fn run_capture(args: CaptureArgs) -> i32 {
    let format = args.format;
    match capture(&args) {
        Ok(outcome) => match outcome.error {
            None => render_capture_success(format, &outcome.result),
            Some(err) => render_capture_error(format, err),
        },
        Err(err) => render_capture_error(format, err),
    }
}

fn capture(args: &CaptureArgs) -> Result<CaptureOutcome, CliError> {
    let request = prepare_request(args)?;
    let out_dir = prepare_artifact_dir(&request.out_dir)?;
    let client = build_client(request.timeout_seconds)?;

    let response = match client
        .request(request.method.reqwest_method(), request.url.clone())
        .header(ACCEPT, "*/*")
        .send()
    {
        Ok(response) => response,
        Err(err) => {
            let code = classify_request_error(&err);
            let message = format!(
                "{} failed for {}",
                request.method.as_str(),
                request.safe_url
            );
            let artifacts = write_failure_summary(
                &out_dir,
                &request,
                CliErrorView {
                    code,
                    message: &message,
                    details: Some(json!({
                        "url": request.safe_url,
                        "method": request.method.as_str(),
                    })),
                },
            )?;
            let details = json!({
                "url": request.safe_url,
                "method": request.method.as_str(),
                "artifact_dir": display_path(&out_dir),
                "artifacts": artifacts,
            });
            return Ok(CaptureOutcome {
                result: None,
                error: Some(CliError::runtime(code, message, Some(details))),
            });
        }
    };

    let status_code = response.status().as_u16();
    let status_class = status_class(status_code).to_string();
    let final_url_redacted = redact_url(response.url()).value;
    let response_headers = redact_headers(response.headers());
    let content_type = header_value(response.headers(), "content-type");
    let content_length_header = header_value(response.headers(), "content-length");
    let mut reader = response.take((request.max_body_bytes as u64).saturating_add(1));
    let mut body = Vec::new();
    reader.read_to_end(&mut body).map_err(|err| {
        CliError::runtime(
            "body-read-failed",
            format!(
                "failed to read response body for {}: {err}",
                request.safe_url
            ),
            Some(json!({ "url": request.safe_url })),
        )
    })?;
    let body_truncated = body.len() > request.max_body_bytes;
    if body_truncated {
        body.truncate(request.max_body_bytes);
    }

    let request_headers = request_headers();
    let body_artifact = body_preview_artifact(&out_dir, &body, content_type.as_deref(), &request)?;
    let header_artifact = write_headers_artifact(
        &out_dir,
        HeaderArtifact {
            schema_version: "web-evidence.headers.v1",
            request: RequestHeaderSummary {
                method: request.method.as_str(),
                url: &request.safe_url,
                headers: request_headers.entries.clone(),
            },
            response: ResponseHeaderSummary {
                status_code,
                final_url: &final_url_redacted,
                headers: response_headers.entries.clone(),
            },
        },
    )?;

    let mut artifacts = vec![
        summary_artifact_ref(&out_dir),
        header_artifact,
        body_artifact.ref_,
    ];
    artifacts.sort_by(|left, right| left.name.cmp(&right.name));

    let redaction = RedactionReport {
        query_values_redacted: request.url_redactions,
        request_header_values_redacted: request_headers.redacted_values,
        response_header_values_redacted: response_headers.redacted_values,
        body_replacements: body_artifact.redaction_replacements,
    };

    let result = CaptureResult {
        artifact_dir: display_path(&out_dir),
        label: request.label.clone(),
        method: request.method.as_str().to_string(),
        requested_url: request.safe_url.clone(),
        final_url: final_url_redacted,
        status_code,
        status_class: status_class.clone(),
        content_type,
        content_length_header,
        body_bytes_captured: body.len(),
        body_truncated,
        artifacts,
        redaction,
    };

    let error = if status_code >= 400 {
        Some(CliError::runtime(
            "http-status-error",
            format!("HTTP status {status_code} for {}", request.safe_url),
            Some(json!({
                "url": request.safe_url,
                "status_code": status_code,
                "status_class": status_class,
                "artifact_dir": result.artifact_dir,
                "artifacts": result.artifacts,
            })),
        ))
    } else {
        None
    };

    write_result_summary(&out_dir, &result, error.as_ref())?;

    Ok(CaptureOutcome {
        result: Some(result),
        error,
    })
}

fn prepare_request(args: &CaptureArgs) -> Result<PreparedRequest, CliError> {
    if args.timeout_seconds == 0 {
        return Err(CliError::usage(
            "invalid-timeout",
            "--timeout-seconds must be greater than 0",
            Some(json!({ "flag": "--timeout-seconds" })),
        ));
    }
    if args.max_body_bytes == 0 {
        return Err(CliError::usage(
            "invalid-max-body-bytes",
            "--max-body-bytes must be greater than 0",
            Some(json!({ "flag": "--max-body-bytes" })),
        ));
    }
    if args.body_preview_bytes == 0 {
        return Err(CliError::usage(
            "invalid-body-preview-bytes",
            "--body-preview-bytes must be greater than 0",
            Some(json!({ "flag": "--body-preview-bytes" })),
        ));
    }

    let url = Url::parse(&args.url).map_err(|err| {
        CliError::usage(
            "invalid-url",
            format!("invalid URL: {err}"),
            Some(json!({ "url": redact_text(&args.url).value })),
        )
    })?;
    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(CliError::usage(
                "unsupported-url-scheme",
                format!("unsupported URL scheme: {scheme}"),
                Some(json!({ "scheme": scheme })),
            ));
        }
    }

    let redacted_url = redact_url(&url);
    let label = args
        .label
        .as_ref()
        .map(|value| redact_text(value).value)
        .filter(|value| !value.trim().is_empty());

    Ok(PreparedRequest {
        url,
        safe_url: redacted_url.value,
        url_redactions: redacted_url.replacements,
        out_dir: absolute_path(&args.out_dir)?,
        label,
        method: args.method,
        timeout_seconds: args.timeout_seconds,
        max_body_bytes: args.max_body_bytes,
        body_preview_bytes: args.body_preview_bytes,
    })
}

fn prepare_artifact_dir(out_dir: &Path) -> Result<PathBuf, CliError> {
    fs::create_dir_all(out_dir).map_err(|err| {
        CliError::runtime(
            "artifact-dir-create-failed",
            format!(
                "failed to create artifact directory {}: {err}",
                out_dir.display()
            ),
            Some(json!({ "artifact_dir": display_path(out_dir) })),
        )
    })?;
    Ok(out_dir.to_path_buf())
}

fn build_client(timeout_seconds: u64) -> Result<Client, CliError> {
    Client::builder()
        .connect_timeout(Duration::from_secs(timeout_seconds.min(10)))
        .timeout(Duration::from_secs(timeout_seconds))
        .user_agent(format!("web-evidence/{}", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|err| {
            CliError::runtime(
                "http-client-build-failed",
                format!("failed to build HTTP client: {err}"),
                None,
            )
        })
}

fn write_headers_artifact(
    out_dir: &Path,
    artifact: HeaderArtifact<'_>,
) -> Result<ArtifactRef, CliError> {
    let path = out_dir.join(HEADERS_FILE);
    write_json_file(&path, &artifact)?;
    Ok(ArtifactRef::new(HEADERS_FILE, &path, "headers", true))
}

fn body_preview_artifact(
    out_dir: &Path,
    body: &[u8],
    content_type: Option<&str>,
    request: &PreparedRequest,
) -> Result<BodyArtifact, CliError> {
    let path = out_dir.join(BODY_PREVIEW_FILE);
    let (preview, replacements) = if request.method == HttpMethod::Head {
        ("HEAD request; no response body captured.\n".to_string(), 0)
    } else if body.is_empty() {
        ("Response body was empty.\n".to_string(), 0)
    } else if is_text_like(content_type) {
        let preview_len = request.body_preview_bytes.min(body.len());
        let raw_preview = String::from_utf8_lossy(&body[..preview_len]).to_string();
        let mut redacted = redact_text(&raw_preview);
        if body.len() > preview_len {
            redacted
                .value
                .push_str("\n[body preview truncated before redaction]\n");
        }
        (redacted.value, redacted.replacements)
    } else {
        (
            format!(
                "Non-text response body omitted. content_type={}\n",
                content_type.unwrap_or("unknown")
            ),
            0,
        )
    };

    fs::write(&path, preview).map_err(|err| {
        CliError::runtime(
            "artifact-write-failed",
            format!("failed to write {}: {err}", path.display()),
            Some(json!({ "path": display_path(&path) })),
        )
    })?;

    Ok(BodyArtifact {
        ref_: ArtifactRef::new(BODY_PREVIEW_FILE, &path, "body-preview", true),
        redaction_replacements: replacements,
    })
}

fn write_result_summary(
    out_dir: &Path,
    result: &CaptureResult,
    error: Option<&CliError>,
) -> Result<(), CliError> {
    let summary = SummaryDocument {
        schema_version: SUMMARY_SCHEMA_VERSION,
        command: CAPTURE_COMMAND,
        ok: error.is_none(),
        captured_at_unix_seconds: unix_seconds_now(),
        result: Some(result),
        error: error.map(|err| SummaryError {
            code: err.code,
            message: err.message.clone(),
            details: err.details.clone(),
        }),
        redaction_policy: RedactionPolicy::default(),
    };
    write_json_file(&out_dir.join(SUMMARY_FILE), &summary)
}

fn write_failure_summary(
    out_dir: &Path,
    request: &PreparedRequest,
    error: CliErrorView<'_>,
) -> Result<Vec<ArtifactRef>, CliError> {
    let summary_error = SummaryError {
        code: error.code,
        message: error.message.to_string(),
        details: error.details,
    };
    let summary = SummaryDocument::<CaptureResult> {
        schema_version: SUMMARY_SCHEMA_VERSION,
        command: CAPTURE_COMMAND,
        ok: false,
        captured_at_unix_seconds: unix_seconds_now(),
        result: None,
        error: Some(summary_error),
        redaction_policy: RedactionPolicy::default(),
    };
    write_json_file(&out_dir.join(SUMMARY_FILE), &summary)?;

    let request_headers = request_headers();
    let headers = HeaderArtifact {
        schema_version: "web-evidence.headers.v1",
        request: RequestHeaderSummary {
            method: request.method.as_str(),
            url: &request.safe_url,
            headers: request_headers.entries,
        },
        response: ResponseHeaderSummary {
            status_code: 0,
            final_url: &request.safe_url,
            headers: Vec::new(),
        },
    };
    write_headers_artifact(out_dir, headers)?;

    let body_path = out_dir.join(BODY_PREVIEW_FILE);
    fs::write(&body_path, "No response body captured.\n").map_err(|err| {
        CliError::runtime(
            "artifact-write-failed",
            format!("failed to write {}: {err}", body_path.display()),
            Some(json!({ "path": display_path(&body_path) })),
        )
    })?;

    let mut artifacts = vec![
        summary_artifact_ref(out_dir),
        ArtifactRef::new(HEADERS_FILE, &out_dir.join(HEADERS_FILE), "headers", true),
        ArtifactRef::new(BODY_PREVIEW_FILE, &body_path, "body-preview", true),
    ];
    artifacts.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(artifacts)
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), CliError> {
    let mut contents = serde_json::to_string_pretty(value).map_err(|err| {
        CliError::runtime(
            "json-render-failed",
            format!("failed to render JSON for {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    contents.push('\n');
    fs::write(path, contents).map_err(|err| {
        CliError::runtime(
            "artifact-write-failed",
            format!("failed to write {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })
}

fn render_capture_success(format: OutputFormat, result: &Option<CaptureResult>) -> i32 {
    let result = result
        .as_ref()
        .expect("successful capture should include result");
    match format {
        OutputFormat::Json => print_json_success(CAPTURE_SCHEMA_VERSION, CAPTURE_COMMAND, result)
            .unwrap_or_else(render_json_failure),
        OutputFormat::Text => {
            println!("web evidence captured: {}", result.artifact_dir);
            if let Some(label) = result.label.as_deref() {
                println!("label: {label}");
            }
            println!("status: {} {}", result.status_code, result.status_class);
            println!("url: {}", result.final_url);
            println!("artifacts:");
            for artifact in &result.artifacts {
                println!("  - {} ({})", artifact.name, artifact.kind);
            }
            EXIT_OK
        }
    }
}

fn render_capture_error(format: OutputFormat, err: CliError) -> i32 {
    if format == OutputFormat::Json {
        return print_json_error(
            CAPTURE_SCHEMA_VERSION,
            CAPTURE_COMMAND,
            err.code,
            &err.message,
            err.details,
            err.exit_code,
        )
        .unwrap_or_else(render_json_failure);
    }

    eprintln!("web-evidence: error: {}", err.message);
    if let Some(details) = err.details
        && let Some(artifact_dir) = details.get("artifact_dir").and_then(Value::as_str)
    {
        eprintln!("artifact dir: {artifact_dir}");
    }
    err.exit_code
}

fn print_json_success<T: Serialize>(
    schema_version: &'static str,
    command: &'static str,
    result: &T,
) -> Result<i32, serde_json::Error> {
    let envelope = SuccessEnvelope {
        schema_version,
        command,
        ok: true,
        result,
    };
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(EXIT_OK)
}

fn print_json_error(
    schema_version: &'static str,
    command: &'static str,
    code: &'static str,
    message: &str,
    details: Option<Value>,
    exit_code: i32,
) -> Result<i32, serde_json::Error> {
    let envelope = ErrorEnvelope {
        schema_version,
        command,
        ok: false,
        error: ErrorBody {
            code,
            message,
            details,
        },
    };
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(exit_code)
}

fn render_json_failure(err: serde_json::Error) -> i32 {
    eprintln!("web-evidence: error: failed to render json: {err}");
    EXIT_RUNTIME
}

fn request_headers() -> RedactedHeaders {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, "*/*".parse().expect("static header"));
    headers.insert(
        USER_AGENT,
        format!("web-evidence/{}", env!("CARGO_PKG_VERSION"))
            .parse()
            .expect("static user-agent"),
    );
    redact_headers(&headers)
}

fn redact_headers(headers: &HeaderMap) -> RedactedHeaders {
    let mut entries = Vec::new();
    let mut redacted_values = 0usize;

    for (name, value) in headers {
        let name_string = name.as_str().to_ascii_lowercase();
        let raw_value = value
            .to_str()
            .map(str::to_string)
            .unwrap_or_else(|_| "[NON_UTF8]".to_string());
        let sensitive = is_sensitive_header(&name_string);
        let (value, replacements) = if sensitive {
            ("[REDACTED]".to_string(), 1)
        } else {
            let redacted = redact_text(&raw_value);
            (redacted.value, redacted.replacements)
        };
        redacted_values += replacements;
        entries.push(HeaderEntry {
            name: name_string,
            value,
            redacted: sensitive || replacements > 0,
        });
    }

    entries.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.value.cmp(&right.value))
    });

    RedactedHeaders {
        entries,
        redacted_values,
    }
}

fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name,
        "authorization"
            | "cookie"
            | "proxy-authorization"
            | "set-cookie"
            | "x-api-key"
            | "x-auth-token"
            | "x-csrf-token"
    )
}

fn redact_url(url: &Url) -> RedactedString {
    let mut safe = url.clone();
    let mut replacements = 0usize;

    if !safe.username().is_empty() && safe.set_username("[REDACTED]").is_ok() {
        replacements += 1;
    }
    if safe.password().is_some() && safe.set_password(Some("[REDACTED]")).is_ok() {
        replacements += 1;
    }

    let pairs: Vec<(String, String)> = safe
        .query_pairs()
        .map(|(key, value)| {
            if is_sensitive_key(&key) {
                replacements += 1;
                (key.to_string(), "[REDACTED]".to_string())
            } else {
                let redacted = redact_text(&value);
                replacements += redacted.replacements;
                (key.to_string(), redacted.value)
            }
        })
        .collect();

    if !pairs.is_empty() {
        safe.set_query(None);
        {
            let mut query = safe.query_pairs_mut();
            for (key, value) in pairs {
                query.append_pair(&key, &value);
            }
        }
    }

    RedactedString {
        value: safe.to_string(),
        replacements,
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("api_key")
        || lower.contains("api-key")
        || lower.contains("apikey")
        || lower.contains("authorization")
        || lower.contains("auth")
        || lower.contains("cookie")
        || lower.contains("session")
        || lower == "key"
        || lower == "sig"
        || lower == "signature"
}

fn header_value(headers: &HeaderMap, key: &str) -> Option<String> {
    headers
        .get(key)
        .and_then(|value| value.to_str().ok())
        .map(|value| redact_text(value).value)
}

fn is_text_like(content_type: Option<&str>) -> bool {
    let Some(content_type) = content_type else {
        return true;
    };
    let content_type = content_type.to_ascii_lowercase();
    content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("html")
        || content_type.contains("javascript")
        || content_type.contains("x-www-form-urlencoded")
        || content_type.contains("svg")
}

fn classify_request_error(err: &reqwest::Error) -> &'static str {
    let message = err.to_string().to_ascii_lowercase();
    if err.is_timeout() || message.contains("timed out") || message.contains("timeout") {
        "request-timeout"
    } else if err.is_connect()
        || message.contains("connection refused")
        || message.contains("dns")
        || message.contains("connect")
    {
        "network-connect-failed"
    } else if err.is_redirect() || message.contains("redirect") {
        "redirect-error"
    } else if err.is_body() {
        "body-read-failed"
    } else {
        "request-failed"
    }
}

fn status_class(status: u16) -> &'static str {
    match status {
        100..=199 => "informational",
        200..=299 => "success",
        300..=399 => "redirect",
        400..=499 => "client-error",
        500..=599 => "server-error",
        _ => "unknown",
    }
}

fn summary_artifact_ref(out_dir: &Path) -> ArtifactRef {
    ArtifactRef::new(SUMMARY_FILE, &out_dir.join(SUMMARY_FILE), "summary", true)
}

fn absolute_path(path: &Path) -> Result<PathBuf, CliError> {
    if path.is_absolute() {
        return Ok(normalize_absolute_path(path));
    }

    let current_dir = env::current_dir().map_err(|err| {
        CliError::runtime(
            "cwd-unavailable",
            format!("failed to read current directory: {err}"),
            None,
        )
    })?;
    Ok(normalize_absolute_path(&current_dir.join(path)))
}

fn unix_seconds_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug)]
struct PreparedRequest {
    url: Url,
    safe_url: String,
    url_redactions: usize,
    out_dir: PathBuf,
    label: Option<String>,
    method: HttpMethod,
    timeout_seconds: u64,
    max_body_bytes: usize,
    body_preview_bytes: usize,
}

#[derive(Debug)]
struct CaptureOutcome {
    result: Option<CaptureResult>,
    error: Option<CliError>,
}

#[derive(Debug)]
struct BodyArtifact {
    ref_: ArtifactRef,
    redaction_replacements: usize,
}

#[derive(Debug)]
struct RedactedHeaders {
    entries: Vec<HeaderEntry>,
    redacted_values: usize,
}

#[derive(Debug, Serialize)]
struct CaptureResult {
    artifact_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    method: String,
    requested_url: String,
    final_url: String,
    status_code: u16,
    status_class: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_length_header: Option<String>,
    body_bytes_captured: usize,
    body_truncated: bool,
    artifacts: Vec<ArtifactRef>,
    redaction: RedactionReport,
}

#[derive(Clone, Debug, Serialize)]
struct ArtifactRef {
    name: String,
    path: String,
    kind: String,
    redacted: bool,
}

impl ArtifactRef {
    fn new(name: &str, path: &Path, kind: &str, redacted: bool) -> Self {
        Self {
            name: name.to_string(),
            path: display_path(path),
            kind: kind.to_string(),
            redacted,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct HeaderEntry {
    name: String,
    value: String,
    redacted: bool,
}

#[derive(Debug, Serialize)]
struct RedactionReport {
    query_values_redacted: usize,
    request_header_values_redacted: usize,
    response_header_values_redacted: usize,
    body_replacements: usize,
}

#[derive(Debug, Serialize)]
struct HeaderArtifact<'a> {
    schema_version: &'static str,
    request: RequestHeaderSummary<'a>,
    response: ResponseHeaderSummary<'a>,
}

#[derive(Debug, Serialize)]
struct RequestHeaderSummary<'a> {
    method: &'static str,
    url: &'a str,
    headers: Vec<HeaderEntry>,
}

#[derive(Debug, Serialize)]
struct ResponseHeaderSummary<'a> {
    status_code: u16,
    final_url: &'a str,
    headers: Vec<HeaderEntry>,
}

#[derive(Debug, Serialize)]
struct SummaryDocument<'a, T: Serialize> {
    schema_version: &'static str,
    command: &'static str,
    ok: bool,
    captured_at_unix_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<SummaryError>,
    redaction_policy: RedactionPolicy,
}

#[derive(Debug, Serialize)]
struct SummaryError {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
}

#[derive(Debug, Serialize)]
struct RedactionPolicy {
    url: &'static str,
    headers: &'static str,
    body_preview: &'static str,
    raw_cookies_or_auth_headers_persisted: bool,
    raw_network_logs_persisted: bool,
}

impl Default for RedactionPolicy {
    fn default() -> Self {
        Self {
            url: "userinfo and sensitive query values are redacted",
            headers: "authorization, cookie, set-cookie, proxy authorization, and token-like headers are redacted",
            body_preview: "text previews are truncated and secret-like assignments/tokens are redacted; binary bodies are omitted",
            raw_cookies_or_auth_headers_persisted: false,
            raw_network_logs_persisted: false,
        }
    }
}

#[derive(Debug)]
struct CliError {
    code: &'static str,
    message: String,
    details: Option<Value>,
    exit_code: i32,
}

impl CliError {
    fn usage(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            exit_code: EXIT_USAGE,
        }
    }

    fn runtime(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            exit_code: EXIT_RUNTIME,
        }
    }
}

struct CliErrorView<'a> {
    code: &'static str,
    message: &'a str,
    details: Option<Value>,
}

#[derive(Serialize)]
struct SuccessEnvelope<'a, T: Serialize> {
    schema_version: &'static str,
    command: &'static str,
    ok: bool,
    result: &'a T,
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    schema_version: &'static str,
    command: &'static str,
    ok: bool,
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'static str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::{is_sensitive_key, redact_text, redact_url, status_class};
    use reqwest::Url;

    #[test]
    fn redacts_sensitive_query_values_and_userinfo() {
        let url = Url::parse("https://user:pass@example.test/path?token=abc&ok=1").unwrap();
        let redacted = redact_url(&url);

        assert_eq!(
            redacted.value,
            "https://%5BREDACTED%5D:%5BREDACTED%5D@example.test/path?token=%5BREDACTED%5D&ok=1"
        );
        assert_eq!(redacted.replacements, 3);
    }

    #[test]
    fn redacts_secret_like_text() {
        let redacted = redact_text("access_token=abc123 secret: sk-proj-abcdefghi");

        assert!(redacted.value.contains("access_token=[REDACTED]"));
        assert!(!redacted.value.contains("abc123"));
        assert!(!redacted.value.contains("sk-proj-abcdefghi"));
        assert!(redacted.replacements >= 2);
    }

    #[test]
    fn sensitive_key_matching_covers_common_auth_names() {
        assert!(is_sensitive_key("access_token"));
        assert!(is_sensitive_key("X-API-Key"));
        assert!(is_sensitive_key("signature"));
        assert!(!is_sensitive_key("page"));
    }

    #[test]
    fn status_class_is_stable() {
        assert_eq!(status_class(200), "success");
        assert_eq!(status_class(302), "redirect");
        assert_eq!(status_class(404), "client-error");
        assert_eq!(status_class(500), "server-error");
    }
}

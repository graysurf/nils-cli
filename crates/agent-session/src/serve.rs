//! `agent-session serve`: a per-machine control plane (HTTP) plus PTY attach
//! (WebSocket) exposed over loopback for the agent-console edge.
//!
//! The rest of the crate is synchronous; this module builds its own tokio
//! runtime inside the `serve` subcommand and calls the existing synchronous
//! lifecycle functions from `tokio::task::spawn_blocking`, so there is no
//! duplicate state model. Reads are open on loopback; writes and the WebSocket
//! attach require a bearer token (fail closed when no token is configured).
//! Every response carries the daemon's `machine` identity so the edge can
//! aggregate multiple machines. Literal keystroke text is never echoed.

use std::fmt;
use std::io::{self, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use jiff::Timestamp;
use nils_common::cli_contract::{exit, schema_version_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::{self, AgentKind, SpecialKey};
use crate::{
    BINARY, CliContext, CliError, ProviderResumeImportArgs, WorkdirSearchOptions, delete_session,
    glance_session, list_sessions, load_session_record, non_empty_env, repo_remote_url_from_cwd,
    resolve_tmux_bin, resume_session_by_id, run_capture_pane, search_workdirs, send_input,
    session_clipboard_buffer, session_dir, session_status, short_hostname,
    start_provider_resume_session, start_session, update_session_title, write_private_file,
    write_session_attachment,
};

/// Monotonic counter giving each attach connection a private pipe file, so
/// concurrent attaches to one session never delete each other's file.
static ATTACH_SEQ: AtomicU64 = AtomicU64::new(0);
const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
const MAX_STDIN_TOKEN_BYTES: u64 = 8 * 1024;
const USAGE_SCHEMA_VERSION: &str = "agent-session.usage.v1";
const DEFAULT_USAGE_TIMEOUT_MS: u64 = 45_000;
const RESET_AT_KEYS: &[&str] = &["reset_at", "resetAt", "resets_at", "resetsAt"];
const RESET_AT_EPOCH_KEYS: &[&str] = &[
    "reset_at_epoch",
    "resetAtEpoch",
    "resets_at_epoch",
    "resetsAtEpoch",
    "reset_at",
    "resetAt",
    "resets_at",
    "resetsAt",
];

/// Shared daemon state handed to every request handler.
struct ServeState {
    context: CliContext,
    machine: String,
    token: Option<String>,
    tmux_bin: PathBuf,
}

pub fn run_serve(context: &CliContext, args: cli::ServeArgs) -> i32 {
    let bind: SocketAddr = match args.bind.parse() {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("error: invalid --bind {}: {err}", args.bind);
            return exit::USAGE;
        }
    };
    if !bind.ip().is_loopback() && !args.allow_non_loopback {
        eprintln!(
            "error: refusing to bind a non-loopback address ({bind}); this endpoint drives a \
             remote shell. Keep it tailnet-only behind `tailscale serve`, or pass \
             --allow-non-loopback to override deliberately."
        );
        return exit::USAGE;
    }
    if !bind.ip().is_loopback() {
        eprintln!(
            "warning: binding a non-loopback address ({bind}) exposes a remote shell; \
             keep it tailnet-only behind `tailscale serve` and off the public internet"
        );
    }

    let token = match resolve_serve_token(&args) {
        Ok(token) => token,
        Err(err) => {
            eprintln!("error: {err}");
            return exit::USAGE;
        }
    };
    if token.is_none() {
        eprintln!(
            "warning: no --token / --token-stdin / AGENT_SESSION_TOKEN set; write and attach endpoints are disabled"
        );
    }

    let machine = args
        .machine
        .clone()
        .or_else(|| non_empty_env("AGENT_SESSION_MACHINE"))
        .or_else(|| context.host.clone())
        .or_else(short_hostname)
        .unwrap_or_else(|| "unknown".to_string());

    let tmux_bin = resolve_tmux_bin(args.tmux_bin.as_deref());
    let state = Arc::new(ServeState {
        context: context.clone(),
        machine,
        token,
        tmux_bin,
    });

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("error: failed to build async runtime: {err}");
            return exit::RUNTIME;
        }
    };

    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(bind).await {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("error: failed to bind {bind}: {err}");
                return exit::RUNTIME;
            }
        };
        eprintln!(
            "agent-session serve listening on http://{bind} (machine={})",
            state.machine
        );
        let app = router(state);
        match axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
        {
            Ok(()) => exit::SUCCESS,
            Err(err) => {
                eprintln!("error: serve failed: {err}");
                exit::RUNTIME
            }
        }
    })
}

fn router(state: Arc<ServeState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/sessions", get(list_handler).post(create_handler))
        .route("/usage", get(usage_handler))
        .route("/workdirs", get(workdirs_handler))
        .route("/repos/remote-url", get(repo_remote_url_handler))
        .route("/sessions/{id}/glance", get(glance_handler))
        .route("/sessions/{id}/buffer", get(buffer_handler))
        .route("/sessions/{id}/send", post(send_handler))
        .route("/sessions/{id}/resume", post(resume_handler))
        .route(
            "/sessions/{id}/attachments",
            post(upload_attachment_handler),
        )
        .route("/sessions/{id}/attach", get(attach_handler))
        .route(
            "/sessions/{id}",
            patch(update_session_handler).delete(delete_handler),
        )
        .with_state(state)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[derive(Debug)]
enum ServeTokenError {
    EmptyStdin,
    MultipleStdinTokens,
    StdinTooLarge,
    ReadStdin(io::Error),
}

impl fmt::Display for ServeTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyStdin => write!(f, "--token-stdin received an empty token"),
            Self::MultipleStdinTokens => write!(f, "--token-stdin expects exactly one token"),
            Self::StdinTooLarge => write!(f, "--token-stdin input exceeds 8192 bytes"),
            Self::ReadStdin(err) => write!(f, "failed to read --token-stdin input: {err}"),
        }
    }
}

fn resolve_serve_token(args: &cli::ServeArgs) -> Result<Option<String>, ServeTokenError> {
    if args.token_stdin {
        return read_token_from_stdin(io::stdin().lock()).map(Some);
    }
    Ok(sanitize_token(args.token.clone()).or_else(|| non_empty_env("AGENT_SESSION_TOKEN")))
}

fn read_token_from_stdin<R: Read>(reader: R) -> Result<String, ServeTokenError> {
    let mut input = String::new();
    let mut limited = reader.take(MAX_STDIN_TOKEN_BYTES + 1);
    limited
        .read_to_string(&mut input)
        .map_err(ServeTokenError::ReadStdin)?;
    if input.len() as u64 > MAX_STDIN_TOKEN_BYTES {
        return Err(ServeTokenError::StdinTooLarge);
    }

    let token = input.trim();
    if token.is_empty() {
        return Err(ServeTokenError::EmptyStdin);
    }
    if token.contains(['\n', '\r']) {
        return Err(ServeTokenError::MultipleStdinTokens);
    }
    Ok(token.to_string())
}

/// Treat an empty/whitespace explicit `--token` as "no token" (fail closed),
/// matching the env-var sanitization in `non_empty_env`.
fn sanitize_token(token: Option<String>) -> Option<String> {
    token.filter(|value| !value.trim().is_empty())
}

// --- response helpers ---------------------------------------------------------

// Every serve response uses one wire contract, `cli.agent-session.serve.v1`,
// distinct from the CLI stdout envelopes: the `data` shape ({machine, ...}) is
// serve-specific, so it must NOT reuse the CLI's per-command schema versions.
fn serve_schema() -> String {
    schema_version_for(BINARY, "serve", 1)
}

fn envelope_ok(data: Value) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "schema_version": serve_schema(),
            "ok": true,
            "data": data,
        })),
    )
        .into_response()
}

fn envelope_err(err: CliError) -> Response {
    let data = err.into_inner();
    let status = match data.code.as_str() {
        "session-not-found" => StatusCode::NOT_FOUND,
        "session-exists" => StatusCode::CONFLICT,
        _ => match data.exit_code {
            exit::USAGE => StatusCode::BAD_REQUEST,
            exit::DATA => StatusCode::UNPROCESSABLE_ENTITY,
            exit::UNAVAILABLE => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        },
    };
    let mut error = json!({ "code": data.code, "message": data.message });
    if let Some(details) = data.details
        && let Some(map) = error.as_object_mut()
    {
        map.insert("details".to_string(), details);
    }
    (
        status,
        Json(json!({
            "schema_version": serve_schema(),
            "ok": false,
            "error": error,
        })),
    )
        .into_response()
}

fn status_json(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "schema_version": serve_schema(),
            "ok": false,
            "error": { "code": code, "message": message },
        })),
    )
        .into_response()
}

fn join_err() -> Response {
    envelope_err(CliError::runtime(
        "serve-task-failed",
        "internal task failed",
        None,
    ))
}

#[derive(Debug, Serialize)]
struct UsageReport {
    schema_version: String,
    ok: bool,
    providers: Vec<UsageProvider>,
}

#[derive(Debug, Serialize)]
struct UsageProvider {
    id: String,
    label: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    windows: Vec<UsageWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<UsageProviderError>,
}

#[derive(Debug, Serialize)]
struct UsageWindow {
    label: String,
    used_percent: i64,
    remaining_percent: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reset_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reset_at_epoch: Option<i64>,
}

#[derive(Debug, Serialize)]
struct UsageProviderError {
    code: String,
    message: String,
}

struct UsageHelperOutput {
    status_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

async fn usage_handler(State(state): State<Arc<ServeState>>) -> Response {
    let timeout = usage_timeout();
    let codex = tokio::task::spawn_blocking(move || collect_codex_usage(timeout));
    let claude = tokio::task::spawn_blocking(move || collect_claude_usage(timeout));
    let (codex, claude) = tokio::join!(codex, claude);

    let providers = vec![
        codex.unwrap_or_else(|_| provider_internal_error("codex", "Codex")),
        claude.unwrap_or_else(|_| provider_internal_error("claude", "Claude")),
    ];
    let ok = providers.iter().all(|provider| provider.ok);
    envelope_ok(json!({
        "machine": state.machine,
        "usage": UsageReport {
            schema_version: USAGE_SCHEMA_VERSION.to_string(),
            ok,
            providers,
        },
    }))
}

fn usage_timeout() -> Duration {
    std::env::var("AGENT_SESSION_USAGE_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_USAGE_TIMEOUT_MS))
}

fn collect_codex_usage(timeout: Duration) -> UsageProvider {
    match run_usage_helper(
        "codex-cli",
        &[
            "diag",
            "rate-limits",
            "--all",
            "--format",
            "json",
            "--no-refresh-auth",
        ],
        timeout,
    ) {
        Ok(output) => normalize_codex_usage(output),
        Err(message) => provider_error("codex", "Codex", "helper-spawn-failed", message),
    }
}

fn collect_claude_usage(timeout: Duration) -> UsageProvider {
    match run_usage_helper(
        "claude-cli",
        &["usage", "--format", "json", "--source", "auto"],
        timeout,
    ) {
        Ok(output) => normalize_claude_usage(output),
        Err(message) => provider_error("claude", "Claude", "helper-spawn-failed", message),
    }
}

fn run_usage_helper(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<UsageHelperOutput, String> {
    let mut child = ProcessCommand::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to start {program}: {err}"))?;

    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status.code().unwrap_or(1)),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) => {
                let _ = child.kill();
                return Err(format!("failed to wait for {program}: {err}"));
            }
        }
    };

    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout);
    }
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_end(&mut stderr);
    }

    Ok(UsageHelperOutput {
        status_code: status,
        stdout,
        stderr,
        timed_out,
    })
}

fn normalize_codex_usage(output: UsageHelperOutput) -> UsageProvider {
    if output.timed_out {
        return provider_error(
            "codex",
            "Codex",
            "helper-timeout",
            "codex usage helper timed out".to_string(),
        );
    }

    let value: Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(_) => {
            return provider_error(
                "codex",
                "Codex",
                "helper-invalid-json",
                helper_failure_message("codex usage unavailable", &output),
            );
        }
    };

    let results = value
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| value.get("result").map(|result| vec![result.clone()]))
        .unwrap_or_default();

    let mut windows = Vec::new();
    for result in results.iter().filter(|result| {
        result
            .get("ok")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| result.get("status").and_then(Value::as_str) == Some("ok"))
    }) {
        windows.extend(windows_from_value(result));
        if windows.is_empty()
            && let Some(summary) = result.get("summary")
        {
            windows.extend(windows_from_codex_summary(summary));
        }
    }

    if !windows.is_empty() {
        return UsageProvider {
            id: "codex".to_string(),
            label: "Codex".to_string(),
            ok: true,
            source: Some("codex-cli".to_string()),
            windows,
            error: None,
        };
    }

    let (code, message) =
        error_from_helper_json(&value, output.status_code, "codex usage unavailable");
    provider_error("codex", "Codex", &code, message)
}

fn normalize_claude_usage(output: UsageHelperOutput) -> UsageProvider {
    if output.timed_out {
        return provider_error(
            "claude",
            "Claude",
            "helper-timeout",
            "claude usage helper timed out".to_string(),
        );
    }

    let value: Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(_) => {
            return provider_error(
                "claude",
                "Claude",
                "helper-invalid-json",
                helper_failure_message("claude usage unavailable", &output),
            );
        }
    };

    let result = value.get("result").unwrap_or(&value);
    let windows = windows_from_value(result);
    if !windows.is_empty() {
        return UsageProvider {
            id: "claude".to_string(),
            label: "Claude".to_string(),
            ok: true,
            source: Some("claude-cli".to_string()),
            windows,
            error: None,
        };
    }

    let (code, message) =
        error_from_helper_json(&value, output.status_code, "claude usage unavailable");
    provider_error("claude", "Claude", &code, message)
}

fn windows_from_value(value: &Value) -> Vec<UsageWindow> {
    value
        .get("windows")
        .and_then(Value::as_array)
        .map(|windows| windows.iter().filter_map(usage_window_from_value).collect())
        .unwrap_or_default()
}

fn usage_window_from_value(value: &Value) -> Option<UsageWindow> {
    let label = value.get("label").and_then(Value::as_str)?.to_string();
    let used_percent = i64_field(value, &["used_percent"])?;
    let remaining_percent = i64_field(value, &["remaining_percent"])
        .unwrap_or_else(|| (100 - used_percent).clamp(0, 100));
    let reset_at = reset_at_text_field(value, RESET_AT_KEYS);
    let reset_at_epoch = epoch_field(value, RESET_AT_EPOCH_KEYS);
    Some(UsageWindow {
        label,
        used_percent,
        remaining_percent,
        reset_at,
        reset_at_epoch,
    })
}

fn windows_from_codex_summary(summary: &Value) -> Vec<UsageWindow> {
    let mut windows = Vec::new();
    if let Some(remaining) = i64_field(summary, &["non_weekly_remaining"]) {
        windows.push(UsageWindow {
            label: summary
                .get("non_weekly_label")
                .and_then(Value::as_str)
                .unwrap_or("Non-weekly")
                .to_string(),
            used_percent: (100 - remaining).clamp(0, 100),
            remaining_percent: remaining,
            reset_at: None,
            reset_at_epoch: i64_field(
                summary,
                &["non_weekly_reset_epoch", "non_weekly_reset_at_epoch"],
            ),
        });
    }
    if let Some(remaining) = i64_field(summary, &["weekly_remaining"]) {
        windows.push(UsageWindow {
            label: "Weekly".to_string(),
            used_percent: (100 - remaining).clamp(0, 100),
            remaining_percent: remaining,
            reset_at: None,
            reset_at_epoch: i64_field(summary, &["weekly_reset_epoch", "weekly_reset_at_epoch"]),
        });
    }
    windows
}

fn reset_at_text_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
            .filter(|raw| safe_reset_at_text(raw))
            .map(ToString::to_string)
    })
}

fn safe_reset_at_text(raw: &str) -> bool {
    if raw.len() > 256 || raw.chars().any(char::is_control) {
        return false;
    }
    let lower = raw.to_ascii_lowercase();
    if lower.contains("access_token")
        || lower.contains("refresh_token")
        || lower.contains("authorization")
        || lower.contains("bearer")
        || lower.contains("sk-")
        || lower.contains("account_id")
        || lower.contains("account-id")
        || lower.contains("acct_")
        || raw.contains('@')
    {
        return false;
    }
    !(raw.starts_with('/')
        || raw.starts_with("~/")
        || raw.starts_with("$HOME/")
        || raw.contains("/home/")
        || raw.contains("/Users/"))
}

fn epoch_field(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(epoch_seconds_from_value))
}

fn epoch_seconds_from_value(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().and_then(epoch_seconds_from_f64))
            .map(normalize_epoch_seconds),
        Value::String(raw) => epoch_seconds_from_str(raw),
        _ => None,
    }
}

fn epoch_seconds_from_str(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    raw.parse::<i64>()
        .ok()
        .map(normalize_epoch_seconds)
        .or_else(|| raw.parse::<f64>().ok().and_then(epoch_seconds_from_f64))
        .or_else(|| {
            raw.parse::<Timestamp>()
                .ok()
                .map(|timestamp| timestamp.as_second())
        })
}

fn epoch_seconds_from_f64(raw: f64) -> Option<i64> {
    raw.is_finite()
        .then(|| normalize_epoch_seconds(raw.round() as i64))
}

fn normalize_epoch_seconds(raw: i64) -> i64 {
    if raw.unsigned_abs() >= 10_000_000_000 {
        raw / 1_000
    } else {
        raw
    }
}

fn i64_field(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_f64().map(|n| n.round() as i64))
        })
    })
}

fn error_from_helper_json(
    value: &Value,
    status_code: Option<i32>,
    fallback: &str,
) -> (String, String) {
    let code = value
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if status_code.unwrap_or(0) == 0 {
                "usage-unavailable"
            } else {
                "helper-failed"
            }
        })
        .to_string();
    let message = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(fallback);
    (code, sanitize_helper_message(message))
}

fn helper_failure_message(prefix: &str, output: &UsageHelperOutput) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let message = stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(prefix);
    sanitize_helper_message(message)
}

fn provider_internal_error(id: &str, label: &str) -> UsageProvider {
    provider_error(
        id,
        label,
        "serve-task-failed",
        "usage provider task failed".to_string(),
    )
}

fn provider_error(id: &str, label: &str, code: &str, message: String) -> UsageProvider {
    UsageProvider {
        id: id.to_string(),
        label: label.to_string(),
        ok: false,
        source: None,
        windows: Vec::new(),
        error: Some(UsageProviderError {
            code: code.to_string(),
            message: sanitize_helper_message(&message),
        }),
    }
}

fn sanitize_helper_message(message: &str) -> String {
    let normalized = message.replace(['\n', '\r', '\t'], " ");
    let mut cleaned = Vec::new();
    let mut redact_next = false;
    for token in normalized.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        let should_redact = redact_next
            || lower.contains("access_token")
            || lower.contains("refresh_token")
            || lower.contains("authorization")
            || lower.contains("bearer")
            || lower.contains("sk-")
            || lower.contains("account_id")
            || lower.contains("account-id")
            || lower.contains("acct_")
            || token.contains('@');
        redact_next = lower.contains("bearer") || lower.contains("authorization:");

        if should_redact {
            cleaned.push("[redacted]".to_string());
        } else if token.starts_with('/')
            || token.starts_with("~/")
            || token.starts_with("$HOME/")
            || token.contains("/home/")
            || token.contains("/Users/")
        {
            cleaned.push("[path]".to_string());
        } else {
            cleaned.push(token.to_string());
        }
        if cleaned.len() >= 24 {
            break;
        }
    }
    if cleaned.is_empty() {
        "usage unavailable".to_string()
    } else {
        cleaned.join(" ")
    }
}

/// XOR-accumulating byte comparison so a correct-length wrong token is compared
/// in constant time (no early return on the first differing byte). The length
/// check is intentionally NOT constant-time — the token length is not treated as
/// a secret; the token itself is the high-entropy secret. Never echoed anywhere.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Enforce the bearer token on write / attach endpoints. Returns `Some(denial)`
/// to reject (401), or `Some(503)` when the daemon has no token configured
/// (fail closed); returns `None` when the request is authorized.
fn deny_unauthorized(state: &ServeState, headers: &HeaderMap) -> Option<Response> {
    let Some(expected) = state.token.as_deref() else {
        return Some(status_json(
            StatusCode::SERVICE_UNAVAILABLE,
            "token-not-configured",
            "server has no token configured; write and attach endpoints are disabled",
        ));
    };
    let provided = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    match provided {
        Some(token) if constant_time_eq(token, expected) => None,
        _ => Some(status_json(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid bearer token",
        )),
    }
}

// --- request bodies -----------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GlanceQuery {
    tail: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    agent: String,
    cwd: Option<String>,
    title: Option<String>,
    id: Option<String>,
    prompt: Option<String>,
    #[serde(default, alias = "resume_id")]
    provider_resume_id: Option<String>,
    #[serde(default)]
    agent_args: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SendBody {
    text: Option<String>,
    #[serde(default)]
    keys: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AttachmentQuery {
    filename: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkdirQuery {
    q: Option<String>,
    limit: Option<usize>,
    #[serde(default)]
    git_only: bool,
    #[serde(default)]
    exclude_worktrees: bool,
}

#[derive(Debug, Deserialize)]
struct RepoRemoteUrlQuery {
    cwd: Option<String>,
}

// --- handlers -----------------------------------------------------------------

async fn healthz(State(state): State<Arc<ServeState>>) -> Response {
    envelope_ok(json!({ "status": "ok", "machine": state.machine }))
}

async fn list_handler(State(state): State<Arc<ServeState>>) -> Response {
    let context = state.context.clone();
    let tmux = state.tmux_bin.clone();
    match tokio::task::spawn_blocking(move || list_sessions(&context, Some(&tmux))).await {
        Ok(Ok(sessions)) => envelope_ok(json!({ "machine": state.machine, "sessions": sessions })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn glance_handler(
    State(state): State<Arc<ServeState>>,
    AxPath(id): AxPath<String>,
    Query(query): Query<GlanceQuery>,
) -> Response {
    let context = state.context.clone();
    let args = cli::GlanceArgs {
        id,
        tail: query.tail.unwrap_or(cli::DEFAULT_GLANCE_TAIL),
        tmux_bin: Some(state.tmux_bin.clone()),
        format: nils_common::cli_contract::OutputFormat::Json,
    };
    match tokio::task::spawn_blocking(move || glance_session(&context, args)).await {
        Ok(Ok(glance)) => envelope_ok(json!({ "machine": state.machine, "glance": glance })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

// Return the session server's tmux paste buffer (`tmux show-buffer`) — the text a
// mouse-selection-copying TUI (e.g. Claude Code) placed there. Read-only, open on
// loopback like `glance`; the browser edge uses it so a right-click Copy can reach
// the on-screen selection that a live TUI never exposes to the DOM.
async fn buffer_handler(
    State(state): State<Arc<ServeState>>,
    AxPath(id): AxPath<String>,
) -> Response {
    let context = state.context.clone();
    let tmux = state.tmux_bin.clone();
    match tokio::task::spawn_blocking(move || session_clipboard_buffer(&context, &id, &tmux)).await
    {
        Ok(Ok(text)) => envelope_ok(json!({ "machine": state.machine, "text": text })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn create_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<CreateBody>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let Some(agent) = AgentKind::from_name(&body.agent) else {
        return envelope_err(CliError::usage(
            "invalid-agent",
            format!("unknown agent: {}", body.agent),
            None,
        ));
    };
    let context = state.context.clone();
    if let Some(provider_resume_id) = body.provider_resume_id {
        if body.cwd.is_some() {
            return envelope_err(CliError::usage(
                "provider-resume-cwd-conflict",
                "provider_resume_id mode resolves cwd from provider history; omit cwd",
                None,
            ));
        }
        if body.prompt.is_some() {
            return envelope_err(CliError::usage(
                "provider-resume-prompt-conflict",
                "provider_resume_id mode imports an existing provider session; omit prompt",
                None,
            ));
        }
        let args = ProviderResumeImportArgs {
            agent,
            provider_resume_id,
            title: body.title,
            id: body.id,
            tmux_bin: Some(state.tmux_bin.clone()),
            agent_bin: None,
            agent_args: body.agent_args,
            format: nils_common::cli_contract::OutputFormat::Json,
        };
        return match tokio::task::spawn_blocking(move || {
            start_provider_resume_session(&context, args)
        })
        .await
        {
            Ok(Ok(view)) => {
                envelope_ok(json!({ "machine": state.machine, "session": view.result }))
            }
            Ok(Err(err)) => envelope_err(err),
            Err(_) => join_err(),
        };
    }
    let args = cli::StartArgs {
        agent,
        cwd: body.cwd.map(PathBuf::from),
        title: body.title,
        id: body.id,
        prompt: body.prompt,
        prompt_file: None,
        prompt_stdin: false,
        tmux_bin: Some(state.tmux_bin.clone()),
        agent_bin: None,
        agent_args: body.agent_args,
        paste_delay_ms: cli::DEFAULT_PASTE_DELAY_MS,
        format: nils_common::cli_contract::OutputFormat::Json,
    };
    match tokio::task::spawn_blocking(move || start_session(&context, args)).await {
        Ok(Ok(view)) => envelope_ok(json!({ "machine": state.machine, "session": view.result })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn send_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
    Json(body): Json<SendBody>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let mut keys = Vec::with_capacity(body.keys.len());
    for name in &body.keys {
        match SpecialKey::from_name(name) {
            Some(key) => keys.push(key),
            None => {
                return envelope_err(CliError::usage(
                    "invalid-key",
                    format!("unknown key: {name}"),
                    None,
                ));
            }
        }
    }
    let context = state.context.clone();
    let args = cli::SendArgs {
        id,
        text: body.text,
        text_stdin: false,
        keys,
        tmux_bin: Some(state.tmux_bin.clone()),
        format: nils_common::cli_contract::OutputFormat::Json,
    };
    match tokio::task::spawn_blocking(move || crate::send_to_session(&context, args)).await {
        Ok(Ok(sent)) => envelope_ok(json!({ "machine": state.machine, "sent": sent })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn resume_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let context = state.context.clone();
    let tmux = state.tmux_bin.clone();
    match tokio::task::spawn_blocking(move || resume_session_by_id(&context, &id, &tmux)).await {
        Ok(Ok(session)) => envelope_ok(json!({ "machine": state.machine, "session": session })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn update_session_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let Some(object) = body.as_object() else {
        return envelope_err(CliError::usage(
            "invalid-session-update",
            "session update body must be a JSON object",
            None,
        ));
    };
    let title = match object.get("title") {
        Some(Value::String(title)) => Some(title.clone()),
        Some(Value::Null) => None,
        Some(_) => {
            return envelope_err(CliError::usage(
                "invalid-title",
                "session title must be a string or null",
                Some(json!({ "field": "title" })),
            ));
        }
        None => {
            return envelope_err(CliError::usage(
                "missing-title",
                "session update requires a title field",
                Some(json!({ "field": "title" })),
            ));
        }
    };
    let context = state.context.clone();
    let tmux = state.tmux_bin.clone();
    match tokio::task::spawn_blocking(move || update_session_title(&context, &id, title, &tmux))
        .await
    {
        Ok(Ok(session)) => envelope_ok(json!({ "machine": state.machine, "session": session })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn upload_attachment_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
    Query(query): Query<AttachmentQuery>,
    body: Body,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = match to_bytes(body, MAX_ATTACHMENT_BYTES + 1).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return status_json(
                StatusCode::PAYLOAD_TOO_LARGE,
                "attachment-too-large",
                "attachment exceeds the maximum allowed size",
            );
        }
    };
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return status_json(
            StatusCode::PAYLOAD_TOO_LARGE,
            "attachment-too-large",
            "attachment exceeds the maximum allowed size",
        );
    }
    let context = state.context.clone();
    let filename = query.filename.clone();
    let bytes = bytes.to_vec();
    match tokio::task::spawn_blocking(move || {
        write_session_attachment(&context, &id, filename.as_deref(), content_type, &bytes)
    })
    .await
    {
        Ok(Ok(attachment)) => {
            envelope_ok(json!({ "machine": state.machine, "attachment": attachment }))
        }
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn workdirs_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    Query(query): Query<WorkdirQuery>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let q = query.q.clone();
    let limit = query.limit;
    let options = WorkdirSearchOptions {
        git_only: query.git_only,
        exclude_worktrees: query.exclude_worktrees,
    };
    let context = state.context.clone();
    match tokio::task::spawn_blocking(move || {
        search_workdirs(&context, q.as_deref(), limit, options)
    })
    .await
    {
        Ok(Ok(workdirs)) => envelope_ok(json!({ "machine": state.machine, "workdirs": workdirs })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn repo_remote_url_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    Query(query): Query<RepoRemoteUrlQuery>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let Some(cwd) = query
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return envelope_err(CliError::usage(
            "missing-cwd",
            "missing cwd query parameter",
            None,
        ));
    };
    let cwd = cwd.to_string();
    match tokio::task::spawn_blocking(move || repo_remote_url_from_cwd(&cwd)).await {
        Ok(url) => envelope_ok(json!({ "machine": state.machine, "url": url })),
        Err(_) => join_err(),
    }
}

async fn delete_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let context = state.context.clone();
    let tmux = state.tmux_bin.clone();
    match tokio::task::spawn_blocking(move || delete_session(&context, &id, tmux)).await {
        Ok(Ok(result)) => envelope_ok(json!({ "machine": state.machine, "deleted": result })),
        Ok(Err(err)) => envelope_err(err),
        Err(_) => join_err(),
    }
}

async fn attach_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
    ws: WebSocketUpgrade,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let context = state.context.clone();
    let tmux = state.tmux_bin.clone();
    let lookup = tokio::task::spawn_blocking({
        let context = context.clone();
        let id = id.clone();
        let tmux = tmux.clone();
        move || {
            let record = load_session_record(&context, &id)?;
            let status = session_status(&tmux, &record);
            Ok::<_, CliError>((record, status))
        }
    })
    .await;
    let (record, status) = match lookup {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => return envelope_err(err),
        Err(_) => return join_err(),
    };
    if status != "running" {
        return envelope_err(attach_unavailable_error(&record, &status));
    }
    ws.on_upgrade(move |socket| attach_socket(socket, context, tmux, record))
}

fn attach_unavailable_error(record: &crate::SessionRecord, status: &str) -> CliError {
    if status == "unknown" {
        return CliError::runtime(
            "session-status-unknown",
            format!("session status could not be checked: {}", record.id),
            Some(json!({ "id": record.id.clone() })),
        );
    }
    CliError::data(
        "session-not-running",
        format!("session is not running: {}", record.id),
        Some(json!({ "id": record.id.clone(), "status": status })),
    )
}

// --- websocket attach ---------------------------------------------------------

async fn attach_socket(
    socket: WebSocket,
    context: CliContext,
    tmux: PathBuf,
    record: crate::SessionRecord,
) {
    let target = format!("{}:0.0", record.tmux_session);
    let (mut sender, mut receiver) = socket.split();

    // Initial screen snapshot so the client renders current pane state before
    // the live stream begins.
    let snapshot = tokio::task::spawn_blocking({
        let record = record.clone();
        let tmux = tmux.clone();
        move || run_capture_pane(&record, 200, &tmux)
    })
    .await;
    if let Ok(Ok(Some(text))) = snapshot {
        let _ = sender.send(Message::Binary(text.into_bytes().into())).await;
    }

    // Stream live pane output via `tmux pipe-pane` into a private per-connection
    // file we tail. The file is 0600 (honoring the crate's secret-file model,
    // since pane output can contain secrets) and uniquely named so concurrent
    // attaches never remove each other's file. Note: tmux allows one pipe-pane
    // per pane, so simultaneous attaches to the SAME session share the pane and
    // the last to connect wins the live pipe (a documented tmux limitation).
    let seq = ATTACH_SEQ.fetch_add(1, Ordering::Relaxed);
    let pipe_file = session_dir(&context, &record.id).join(format!("attach-{seq}.pipe"));
    let enabled = {
        let tmux = tmux.clone();
        let target = target.clone();
        let pipe_file = pipe_file.clone();
        tokio::task::spawn_blocking(move || {
            if write_private_file(&pipe_file, b"").is_err() {
                return false;
            }
            ProcessCommand::new(&tmux)
                .arg("pipe-pane")
                .arg("-o")
                .arg("-t")
                .arg(&target)
                .arg(format!(
                    "cat >> {}",
                    shell_words::quote(&pipe_file.to_string_lossy())
                ))
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false)
    };
    let tail_task = enabled.then(|| tokio::spawn(tail_pipe(pipe_file.clone(), sender)));

    // Client -> pane: JSON control frames { text?, key?, keys?, resize{cols,rows} }.
    // The first resize after attach forces a full-frame repaint (see resize_pane).
    let mut initial_repaint_pending = true;
    while let Some(Ok(message)) = receiver.next().await {
        match message {
            Message::Text(text) => {
                handle_input(
                    &context,
                    &tmux,
                    &record,
                    &target,
                    text.as_str(),
                    &mut initial_repaint_pending,
                )
                .await;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    // Teardown: stop the tail, disable the pipe, remove this connection's private
    // file. The tmux session itself stays alive (durability across disconnect).
    if let Some(handle) = tail_task {
        handle.abort();
    }
    let _ = tokio::task::spawn_blocking(move || {
        let _ = ProcessCommand::new(&tmux)
            .arg("pipe-pane")
            .arg("-t")
            .arg(&target)
            .status();
        let _ = std::fs::remove_file(&pipe_file);
    })
    .await;
}

/// Tail an appended-to file, forwarding new bytes as binary WebSocket frames.
/// A regular file's offset does not stick at EOF, so a zero-length read simply
/// means "no new data yet" — sleep briefly and read again.
async fn tail_pipe(path: PathBuf, mut sender: SplitSink<WebSocket, Message>) {
    use tokio::io::AsyncReadExt;
    let mut file = loop {
        match tokio::fs::File::open(&path).await {
            Ok(file) => break file,
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    };
    let mut buf = vec![0u8; 8192];
    loop {
        match file.read(&mut buf).await {
            Ok(0) => tokio::time::sleep(Duration::from_millis(60)).await,
            Ok(n) => {
                if sender
                    .send(Message::Binary(buf[..n].to_vec().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

/// After a fresh (re)attach the client rebuilds its terminal emulator from
/// scratch and sends its real size as the first resize frame. A `resize-window`
/// to dimensions the tmux pane already has is a no-op — no SIGWINCH — so a
/// full-screen agent (codex/claude/hermes) never repaints, and the client is
/// left rendering the stale pre-attach snapshot mis-wrapped against its new
/// grid. Force exactly one guaranteed size change on that first resize so the
/// agent redraws its whole frame into the fresh grid. The short pause lets the
/// agent observe the intermediate size, so the two changes are not coalesced
/// into "no net change" (which would skip the redraw).
const INITIAL_REPAINT_NUDGE_DELAY: Duration = Duration::from_millis(150);

/// Apply a client resize to the pane. On the first resize after attach
/// (`force_repaint`), nudge to an off-by-one height first so the final resize to
/// `rows` is always a real change that repaints the agent's frame; afterwards a
/// single `resize-window` is issued so ordinary mid-session resizes don't flicker.
async fn resize_pane(tmux: &Path, target: &str, cols: u64, rows: u64, force_repaint: bool) {
    if force_repaint {
        let nudge_rows = if rows > 1 { rows - 1 } else { rows + 1 };
        run_resize_window(tmux, target, cols, nudge_rows).await;
        tokio::time::sleep(INITIAL_REPAINT_NUDGE_DELAY).await;
    }
    run_resize_window(tmux, target, cols, rows).await;
}

async fn run_resize_window(tmux: &Path, target: &str, cols: u64, rows: u64) {
    let tmux = tmux.to_path_buf();
    let target = target.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        ProcessCommand::new(&tmux)
            .arg("resize-window")
            .arg("-t")
            .arg(&target)
            .arg("-x")
            .arg(cols.to_string())
            .arg("-y")
            .arg(rows.to_string())
            .status()
    })
    .await;
}

async fn handle_input(
    context: &CliContext,
    tmux: &Path,
    record: &crate::SessionRecord,
    target: &str,
    frame: &str,
    initial_repaint_pending: &mut bool,
) {
    let Ok(value) = serde_json::from_str::<Value>(frame) else {
        return;
    };

    if let Some(resize) = value.get("resize") {
        let cols = resize.get("cols").and_then(Value::as_u64);
        let rows = resize.get("rows").and_then(Value::as_u64);
        if let (Some(cols), Some(rows)) = (cols, rows) {
            let force_repaint = std::mem::take(initial_repaint_pending);
            resize_pane(tmux, target, cols, rows, force_repaint).await;
        }
        return;
    }

    let text = value
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut keys: Vec<SpecialKey> = value
        .get("keys")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter_map(SpecialKey::from_name)
                .collect()
        })
        .unwrap_or_default();
    if let Some(key) = value
        .get("key")
        .and_then(Value::as_str)
        .and_then(SpecialKey::from_name)
    {
        keys.push(key);
    }
    if text.is_none() && keys.is_empty() {
        return;
    }

    let context = context.clone();
    let tmux = tmux.to_path_buf();
    let record = record.clone();
    let _ = tokio::task::spawn_blocking(move || {
        send_input(&context, &record, text.as_deref(), &keys, &tmux)
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use tower::ServiceExt;

    const MACHINE: &str = "test-machine";
    const TOKEN: &str = "s3cr3t-token";

    fn state(state_dir: &Path, token: Option<&str>, tmux_bin: PathBuf) -> Arc<ServeState> {
        Arc::new(ServeState {
            context: CliContext {
                state_dir: state_dir.to_path_buf(),
                host: None,
            },
            machine: MACHINE.to_string(),
            token: token.map(str::to_string),
            tmux_bin,
        })
    }

    fn minimal_tmux(dir: &Path) -> PathBuf {
        let bin = dir.join("tmux");
        std::fs::write(
            &bin,
            "#!/usr/bin/env sh\ncase \"$1\" in\n  has-session) exit 0 ;;\n  capture-pane) printf 'pane\\n'; exit 0 ;;\n  show-buffer) printf 'buffered selection\\n'; exit 0 ;;\n  *) exit 0 ;;\nesac\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        bin
    }

    fn seed_session(state_dir: &Path, id: &str, agent: &str, tmux_session: &str) {
        let dir = state_dir.join("sessions").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("session.json"),
            format!(
                r#"{{"schema_version":"agent-session.session.v1","id":"{id}","agent":"{agent}","mode":"interactive","title":null,"cwd":"/tmp","tmux_session":"{tmux_session}","prompt_file":null,"log_file":null,"created_at":"2000-01-01T00:00:00Z","updated_at":"2000-01-01T00:00:00Z"}}"#
            ),
        )
        .unwrap();
    }

    fn seed_resumable_session(
        state_dir: &Path,
        id: &str,
        agent: &str,
        tmux_session: &str,
        cwd: &Path,
        resume_args: &[&str],
    ) {
        let dir = state_dir.join("sessions").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let resume_args = serde_json::to_string(resume_args).unwrap();
        std::fs::write(
            dir.join("session.json"),
            format!(
                r#"{{
  "schema_version": "agent-session.session.v1",
  "id": "{id}",
  "agent": "{agent}",
  "mode": "interactive",
  "title": null,
  "cwd": "{}",
  "tmux_session": "{tmux_session}",
  "prompt_file": null,
  "log_file": null,
  "created_at": "2000-01-01T00:00:00Z",
  "updated_at": "2000-01-01T00:00:00Z",
  "provider_resume": {{
    "provider": "{agent}",
    "session_id": "resume-session-id",
    "captured_at": "2000-01-01T00:00:00Z",
    "capture_method": "fixture",
    "resume_args": {resume_args}
  }},
  "runtime": {{
    "kind": "tmux",
    "tmux_session": "{tmux_session}",
    "generation": 1,
    "started_at": "2000-01-01T00:00:00Z"
  }},
  "agent_args": []
}}"#,
                cwd.to_string_lossy()
            ),
        )
        .unwrap();
    }

    fn add_provider_resume_extra(state_dir: &Path, id: &str) {
        let path = state_dir.join("sessions").join(id).join("session.json");
        let mut record: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        record["provider_resume"]["storage_only"] = json!({ "keep": true });
        std::fs::write(&path, serde_json::to_string(&record).unwrap()).unwrap();
    }

    fn executable(path: &Path, body: &str) -> PathBuf {
        std::fs::write(path, body).unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
        path.to_path_buf()
    }

    fn resume_tmux(dir: &Path, log: &Path) -> PathBuf {
        executable(
            &dir.join("tmux"),
            &format!(
                "#!/usr/bin/env sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$1\" in\n  has-session) exit 1 ;;\n  *) exit 0 ;;\nesac\n",
                shell_words::quote(&log.to_string_lossy())
            ),
        )
    }

    fn fake_agent(dir: &Path, name: &str) -> PathBuf {
        executable(
            &dir.join(name),
            "#!/usr/bin/env sh\nprintf 'fake agent started\\n'\n",
        )
    }

    fn init_git_remote(repo: &Path, remote: &str) {
        std::fs::create_dir_all(repo).unwrap();
        let init = ProcessCommand::new("git")
            .arg("-C")
            .arg(repo)
            .arg("init")
            .output()
            .unwrap();
        assert!(
            init.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        let add_remote = ProcessCommand::new("git")
            .arg("-C")
            .arg(repo)
            .args(["remote", "add", "origin", remote])
            .output()
            .unwrap();
        assert!(
            add_remote.status.success(),
            "git remote add failed: {}",
            String::from_utf8_lossy(&add_remote.stderr)
        );
    }

    #[test]
    fn normalize_claude_usage_preserves_reset_aliases() {
        let provider = normalize_claude_usage(UsageHelperOutput {
            status_code: Some(0),
            stdout: br#"{
  "schema_version": "claude-cli.usage.v1",
  "command": "usage",
  "ok": true,
  "result": {
    "windows": [
      { "label": "5h", "used_percent": 3, "remaining_percent": 97, "resets_at": "2030-01-01T00:00:00Z" },
      { "label": "Weekly", "used_percent": 0, "remaining_percent": 100, "resetsAtEpoch": 1805000000 },
      { "label": "Month", "used_percent": 9, "remaining_percent": 91, "resetsAt": "2030-01-01T00:00:00.950339+00:00" },
      { "label": "Year", "used_percent": 1, "remaining_percent": 99, "resets_at_epoch": "1893456000000" },
      { "label": "Human", "used_percent": 4, "remaining_percent": 96, "resets_at": "Jul 12, 9pm (Asia/Taipei)" },
      { "label": "Invalid", "used_percent": 2, "remaining_percent": 98, "resets_at": "/Users/terry/.claude/token" }
    ]
  }
}"#
            .to_vec(),
            stderr: Vec::new(),
            timed_out: false,
        });

        let value = serde_json::to_value(provider).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["windows"][0]["reset_at"], "2030-01-01T00:00:00Z");
        assert_eq!(value["windows"][0]["reset_at_epoch"], 1_893_456_000);
        assert_eq!(value["windows"][1]["reset_at_epoch"], 1_805_000_000);
        assert!(value["windows"][1].get("reset_at").is_none());
        assert_eq!(
            value["windows"][2]["reset_at"],
            "2030-01-01T00:00:00.950339+00:00"
        );
        assert_eq!(value["windows"][2]["reset_at_epoch"], 1_893_456_000);
        assert_eq!(value["windows"][3]["reset_at_epoch"], 1_893_456_000);
        assert_eq!(value["windows"][4]["reset_at"], "Jul 12, 9pm (Asia/Taipei)");
        assert!(value["windows"][4].get("reset_at_epoch").is_none());
        assert!(value["windows"][5].get("reset_at").is_none());
        assert!(value["windows"][5].get("reset_at_epoch").is_none());
    }

    #[test]
    fn default_usage_timeout_covers_slow_claude_usage_helpers() {
        assert!(
            Duration::from_millis(DEFAULT_USAGE_TIMEOUT_MS) >= Duration::from_secs(30),
            "m4 claude-cli usage can take more than 20s before returning reset-bearing windows"
        );
    }

    fn write_codex_session_meta(codex_home: &Path, session_id: &str, cwd: &Path) {
        write_codex_session_meta_file(codex_home, "session.jsonl", session_id, cwd);
    }

    fn write_codex_session_meta_file(
        codex_home: &Path,
        filename: &str,
        session_id: &str,
        cwd: &Path,
    ) {
        let path = codex_home.join("sessions/2026/07/05").join(filename);
        std::fs::create_dir_all(path.parent().expect("parent")).unwrap();
        std::fs::write(
            path,
            format!(
                r#"{{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{{"id":"{session_id}","session_id":"{session_id}","cwd":"{}","source":"cli","timestamp":"2099-01-01T00:00:00Z"}}}}
"#,
                cwd.to_string_lossy()
            ),
        )
        .unwrap();
    }

    fn write_claude_session_transcript(
        claude_config: &Path,
        project_slug: &str,
        session_id: &str,
        cwd: &Path,
    ) {
        let path = claude_config
            .join("projects")
            .join(project_slug)
            .join(format!("{session_id}.jsonl"));
        std::fs::create_dir_all(path.parent().expect("parent")).unwrap();
        std::fs::write(
            path,
            format!(
                r#"{{"type":"mode","mode":"normal","sessionId":"{session_id}"}}
{{"type":"user","isSidechain":false,"cwd":"{}","sessionId":"{session_id}"}}
"#,
                cwd.to_string_lossy()
            ),
        )
        .unwrap();
    }

    async fn call(app: Router, req: Request<Body>) -> (StatusCode, Value) {
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, value)
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    fn get_auth(uri: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method("GET").uri(uri);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    fn auth_headers(token: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(token) = token {
            headers.insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
        }
        headers
    }

    fn post_json(uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    fn patch_json(uri: &str, token: Option<&str>, body: Value) -> Request<Body> {
        let mut builder = Request::builder()
            .method("PATCH")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    fn post_bytes(uri: &str, token: Option<&str>, body: &[u8]) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/octet-stream");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::from(body.to_vec())).unwrap()
    }

    #[tokio::test]
    async fn healthz_reports_machine() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let (status, body) = call(router(st.clone()), get("/healthz")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["machine"], MACHINE);
    }

    #[tokio::test]
    async fn list_empty_reports_machine_and_open_on_reads() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        // No Authorization header: reads are open on loopback.
        let (status, body) = call(router(st.clone()), get("/sessions")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        assert_eq!(body["data"]["machine"], MACHINE);
        assert_eq!(body["data"]["sessions"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_preserves_provider_resume_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        seed_resumable_session(
            tmp.path(),
            "recover",
            "codex",
            "hs-codex-recover",
            &cwd,
            &[
                "resume",
                "resume-session-id",
                "--cd",
                cwd.to_str().unwrap(),
                "--no-alt-screen",
            ],
        );
        add_provider_resume_extra(tmp.path(), "recover");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(router(st.clone()), get("/sessions")).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let session = &body["data"]["sessions"][0];
        assert_eq!(session["provider_resume"]["provider"], "codex");
        assert_eq!(
            session["provider_resume"]["session_id"],
            "resume-session-id"
        );
        assert_eq!(
            session["provider_resume"]["resume_args"],
            json!([
                "resume",
                "resume-session-id",
                "--cd",
                cwd.to_str().unwrap(),
                "--no-alt-screen"
            ])
        );
        assert!(session["provider_resume"].get("storage_only").is_none());
    }

    #[tokio::test]
    async fn writes_require_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));

        // POST create without a token.
        let (status, body) = call(
            router(st.clone()),
            post_json("/sessions", None, json!({"agent": "codex"})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        // DELETE without a token.
        let del = Request::builder()
            .method("DELETE")
            .uri("/sessions/abc")
            .body(Body::empty())
            .unwrap();
        let (status, _) = call(router(st.clone()), del).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Wrong token is rejected.
        let (status, _) = call(
            router(st.clone()),
            post_json("/sessions", Some("nope"), json!({"agent": "codex"})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // The WebSocket attach handler guards with the same `deny_unauthorized` as
    // the write handlers; the `WebSocketUpgrade` extractor cannot be driven
    // through `oneshot` (it needs a real upgradable connection), so the guard is
    // tested directly here and the streaming path is covered by a manual smoke.
    #[test]
    fn deny_unauthorized_enforces_bearer_token() {
        let tmp = tempfile::TempDir::new().unwrap();

        // No token configured -> fail closed (503) even with a bearer presented.
        let st = state(tmp.path(), None, PathBuf::from("tmux"));
        assert!(deny_unauthorized(&st, &auth_headers(Some(TOKEN))).is_some());

        // Token configured: missing / wrong -> denied; correct -> allowed.
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        assert!(deny_unauthorized(&st, &auth_headers(None)).is_some());
        assert!(deny_unauthorized(&st, &auth_headers(Some("nope"))).is_some());
        // Equal-length but wrong token exercises the constant-time byte loop's
        // deny path (a shorter wrong token short-circuits on the length check).
        let same_len_wrong = "X".repeat(TOKEN.len());
        assert_eq!(same_len_wrong.len(), TOKEN.len());
        assert!(deny_unauthorized(&st, &auth_headers(Some(&same_len_wrong))).is_some());
        assert!(deny_unauthorized(&st, &auth_headers(Some(TOKEN))).is_none());
    }

    #[test]
    fn attach_status_errors_distinguish_unknown_from_non_running() {
        let record = test_record("attach-me", "hs-codex-attach-me");
        let stopped = attach_unavailable_error(&record, "stopped").into_inner();
        assert_eq!(stopped.code, "session-not-running");
        assert_eq!(stopped.exit_code, exit::DATA);
        assert_eq!(
            stopped.details.unwrap()["status"],
            Value::String("stopped".to_string())
        );

        let unknown = attach_unavailable_error(&record, "unknown").into_inner();
        assert_eq!(unknown.code, "session-status-unknown");
        assert_eq!(unknown.exit_code, exit::RUNTIME);
    }

    #[test]
    fn sanitize_token_blanks_fail_closed() {
        // An empty/whitespace --token collapses to None so serve fails closed
        // instead of authorizing an empty bearer.
        assert_eq!(sanitize_token(Some(String::new())), None);
        assert_eq!(sanitize_token(Some("   ".to_string())), None);
        assert_eq!(sanitize_token(None), None);
        assert_eq!(
            sanitize_token(Some("tok".to_string())),
            Some("tok".to_string())
        );
    }

    #[tokio::test]
    async fn missing_token_config_disables_writes_fail_closed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), None, PathBuf::from("tmux"));
        let (status, body) = call(
            router(st.clone()),
            post_json("/sessions", Some(TOKEN), json!({"agent": "codex"})),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "token-not-configured");
    }

    #[tokio::test]
    async fn create_imports_codex_session_from_provider_resume_id() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("repo");
        let codex_home = tmp.path().join("codex-home");
        std::fs::create_dir_all(&cwd).unwrap();
        write_codex_session_meta(&codex_home, "external-codex-id", &cwd);
        let log = tmp.path().join("tmux.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let codex = fake_agent(tmp.path(), "codex");
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        let _codex_bin = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_BIN", codex.to_str().unwrap());
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "id": "imported-codex",
                    "provider_resume_id": "external-codex-id",
                    "title": "Imported Codex"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let session = &body["data"]["session"];
        assert_eq!(session["id"], "imported-codex");
        assert_eq!(session["agent"], "codex");
        assert_eq!(session["cwd"], cwd.to_string_lossy().as_ref());
        assert_eq!(session["status"], "running");
        assert_eq!(session["resumable"], true);
        assert_eq!(
            session["provider_resume"]["session_id"],
            "external-codex-id"
        );
        assert_eq!(
            session["provider_resume"]["resume_args"],
            json!([
                "resume",
                "external-codex-id",
                "--cd",
                cwd.to_string_lossy(),
                "--no-alt-screen"
            ])
        );

        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            calls.contains("new-session -d -s hs-codex-imported-codex"),
            "import must create a tmux runtime: {calls:?}"
        );
        assert!(
            calls.contains("resume external-codex-id"),
            "import must use the external provider id: {calls:?}"
        );
        assert!(
            calls.contains("--cd") && calls.contains(cwd.to_string_lossy().as_ref()),
            "import must resume in the provider cwd: {calls:?}"
        );
        assert!(
            tmp.path()
                .join("sessions/imported-codex/resume.json")
                .is_file(),
            "import must persist durable resume metadata"
        );
    }

    #[tokio::test]
    async fn create_imports_claude_session_from_resume_id_alias() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("repo");
        let claude_config = tmp.path().join("claude-config");
        std::fs::create_dir_all(&cwd).unwrap();
        write_claude_session_transcript(
            &claude_config,
            "-fixture-project",
            "external-claude-id",
            &cwd,
        );
        let log = tmp.path().join("tmux.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let claude = fake_agent(tmp.path(), "claude");
        let _claude_config =
            EnvGuard::set(&lock, "CLAUDE_CONFIG_DIR", claude_config.to_str().unwrap());
        let _claude_bin =
            EnvGuard::set(&lock, "AGENT_SESSION_CLAUDE_BIN", claude.to_str().unwrap());
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "claude",
                    "id": "imported-claude",
                    "resume_id": "external-claude-id",
                    "title": "Imported Claude"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let session = &body["data"]["session"];
        assert_eq!(session["id"], "imported-claude");
        assert_eq!(session["agent"], "claude");
        assert_eq!(session["cwd"], cwd.to_string_lossy().as_ref());
        assert_eq!(session["status"], "running");
        assert_eq!(session["resumable"], true);
        assert_eq!(
            session["provider_resume"]["session_id"],
            "external-claude-id"
        );
        assert_eq!(
            session["provider_resume"]["resume_args"],
            json!(["--resume", "external-claude-id"])
        );

        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            calls.contains("new-session -d -s hs-claude-imported-claude"),
            "import must create a tmux runtime: {calls:?}"
        );
        assert!(
            calls.contains("--resume external-claude-id"),
            "import must use the external provider id: {calls:?}"
        );
        assert!(
            !calls.contains("--session-id"),
            "imported Claude resume must not create a new provider session: {calls:?}"
        );
        assert!(
            tmp.path()
                .join("sessions/imported-claude/resume.json")
                .is_file(),
            "import must persist durable resume metadata"
        );
    }

    #[tokio::test]
    async fn create_provider_resume_id_rejects_cwd_and_prompt_overrides() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "provider_resume_id": "external-id",
                    "cwd": "/should-not-override"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "provider-resume-cwd-conflict");

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "provider_resume_id": "external-id",
                    "prompt": "continue"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "provider-resume-prompt-conflict");

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "provider_resume_id": "external-id",
                    "agent_args": ["--cd", "/should-not-override"]
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "provider-resume-agent-args-conflict");
    }

    #[tokio::test]
    async fn create_provider_resume_id_reports_not_found() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let codex_home = tmp.path().join("codex-home");
        std::fs::create_dir_all(codex_home.join("sessions")).unwrap();
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "provider_resume_id": "missing-codex-id"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "provider-resume-not-found");
        assert_eq!(
            body["error"]["details"]["provider_resume_id"],
            "missing-codex-id"
        );
    }

    #[tokio::test]
    async fn create_provider_resume_id_rejects_invalid_values() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        for provider_resume_id in ["", "bad\nid"] {
            let (status, body) = call(
                router(st.clone()),
                post_json(
                    "/sessions",
                    Some(TOKEN),
                    json!({
                        "agent": "codex",
                        "provider_resume_id": provider_resume_id
                    }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["error"]["code"], "invalid-provider-resume-id");
        }
    }

    #[tokio::test]
    async fn create_provider_resume_id_reports_ambiguous_cwd_matches() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let codex_home = tmp.path().join("codex-home");
        let cwd_a = tmp.path().join("repo-a");
        let cwd_b = tmp.path().join("repo-b");
        std::fs::create_dir_all(&cwd_a).unwrap();
        std::fs::create_dir_all(&cwd_b).unwrap();
        write_codex_session_meta_file(&codex_home, "first.jsonl", "shared-codex-id", &cwd_a);
        write_codex_session_meta_file(&codex_home, "second.jsonl", "shared-codex-id", &cwd_b);
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "provider_resume_id": "shared-codex-id"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "provider-resume-ambiguous");
        assert_eq!(body["error"]["details"]["cwd_count"], 2);
    }

    #[tokio::test]
    async fn create_provider_resume_id_reports_scan_truncation() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let codex_home = tmp.path().join("codex-home");
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        write_codex_session_meta(&codex_home, "external-codex-id", &cwd);
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        let _max_entries = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_RESUME_SCAN_MAX_ENTRIES", "1");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "provider_resume_id": "external-codex-id"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "provider-resume-scan-truncated");
    }

    #[tokio::test]
    async fn create_imports_claude_session_after_oversized_transcript_line() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("repo");
        let claude_config = tmp.path().join("claude-config");
        let bad_transcript = claude_config
            .join("projects")
            .join("-fixture-project")
            .join("oversized.jsonl");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(bad_transcript.parent().expect("parent")).unwrap();
        std::fs::write(
            &bad_transcript,
            "x".repeat(crate::CLAUDE_SESSION_META_MAX_LINE_BYTES + 1),
        )
        .unwrap();
        write_claude_session_transcript(
            &claude_config,
            "-fixture-project",
            "external-claude-id",
            &cwd,
        );
        let log = tmp.path().join("tmux.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let claude = fake_agent(tmp.path(), "claude");
        let _claude_config =
            EnvGuard::set(&lock, "CLAUDE_CONFIG_DIR", claude_config.to_str().unwrap());
        let _claude_bin =
            EnvGuard::set(&lock, "AGENT_SESSION_CLAUDE_BIN", claude.to_str().unwrap());
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "claude",
                    "id": "imported-claude-oversized",
                    "provider_resume_id": "external-claude-id"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(
            body["data"]["session"]["cwd"],
            cwd.to_string_lossy().as_ref()
        );
    }

    #[tokio::test]
    async fn create_provider_resume_id_rejects_unsupported_agent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "hermes",
                    "provider_resume_id": "external-id"
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "unsupported-provider-resume-agent");
    }

    #[tokio::test]
    async fn send_with_token_delivers_without_leaking_text() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "steer", "codex", "hs-codex-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let secret = "serve-secret-approval";
        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions/steer/send",
                Some(TOKEN),
                json!({ "text": secret, "keys": ["enter"] }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        assert_eq!(body["data"]["machine"], MACHINE);
        assert_eq!(body["data"]["sent"]["sent_text"], true);
        assert_eq!(body["data"]["sent"]["keys"][0], "enter");
        // The literal text is never echoed back into the response contract.
        assert!(
            !serde_json::to_string(&body).unwrap().contains(secret),
            "secret text leaked into serve response: {body}"
        );
    }

    #[tokio::test]
    async fn send_rejects_unknown_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "steer", "codex", "hs-codex-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);
        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions/steer/send",
                Some(TOKEN),
                json!({ "keys": ["banana"] }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid-key");
    }

    #[tokio::test]
    async fn resume_recreates_missing_runtime_with_token() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let log = tmp.path().join("tmux.log");
        let tmux = resume_tmux(tmp.path(), &log);
        let codex = fake_agent(tmp.path(), "codex");
        let _codex_bin = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_BIN", codex.to_str().unwrap());
        seed_resumable_session(
            tmp.path(),
            "recover",
            "codex",
            "hs-codex-recover",
            &cwd,
            &[
                "resume",
                "resume-session-id",
                "--cd",
                cwd.to_str().unwrap(),
                "--no-alt-screen",
            ],
        );
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            post_json("/sessions/recover/resume", None, json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        let (status, body) = call(
            router(st.clone()),
            post_json("/sessions/recover/resume", Some(TOKEN), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let session = &body["data"]["session"];
        assert_eq!(session["id"], "recover");
        assert_eq!(session["status"], "running");
        assert_eq!(session["resumable"], true);

        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            calls.contains("new-session -d -s hs-codex-recover"),
            "resume must create a tmux runtime: {calls:?}"
        );
        assert!(
            calls.contains("resume resume-session-id"),
            "resume must use the exact provider id: {calls:?}"
        );
    }

    #[tokio::test]
    async fn resume_refuses_non_resumable_session_with_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("tmux.log");
        let tmux = resume_tmux(tmp.path(), &log);
        seed_session(tmp.path(), "plain", "codex", "hs-codex-plain");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            post_json("/sessions/plain/resume", Some(TOKEN), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"]["code"], "session-not-resumable");
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            !calls.contains("new-session"),
            "non-resumable sessions must not create tmux runtimes: {calls:?}"
        );
    }

    #[tokio::test]
    async fn update_session_title_persists_and_clears_custom_title() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "steer", "codex", "hs-codex-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/steer",
                Some(TOKEN),
                json!({ "title": "Reviewed title" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        assert_eq!(body["data"]["machine"], MACHINE);
        assert_eq!(body["data"]["session"]["title"], "Reviewed title");

        let record_path = tmp.path().join("sessions/steer/session.json");
        let record: Value =
            serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
        assert_eq!(record["title"], "Reviewed title");
        assert_ne!(record["updated_at"], "2000-01-01T00:00:00Z");

        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", Some(TOKEN), json!({ "title": null })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], Value::Null);

        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", Some(TOKEN), json!({ "title": "" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], Value::Null);

        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", Some(TOKEN), json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "missing-title");

        let too_long = "x".repeat(121);
        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", Some(TOKEN), json!({ "title": too_long })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "title-too-long");

        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/ghost", Some(TOKEN), json!({ "title": "Ghost" })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "session-not-found");
    }

    #[tokio::test]
    async fn update_session_title_renames_live_claude_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let calls_log = tmp.path().join("tmux-calls.log");
        let pasted_log = tmp.path().join("tmux-pasted.log");
        let tmux = rename_probe_tmux(tmp.path(), &calls_log, &pasted_log, true);
        seed_session(tmp.path(), "steer", "claude", "hs-claude-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/steer",
                Some(TOKEN),
                json!({ "title": "Cleaned up title" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], "Cleaned up title");

        let calls = std::fs::read_to_string(&calls_log).unwrap();
        assert!(
            calls.contains("paste-buffer -b steer-send"),
            "a live Claude rename must paste into the pane: {calls:?}"
        );
        assert!(
            calls.contains("send-keys -t hs-claude-steer:0.0 Enter"),
            "a live Claude rename must be submitted with Enter: {calls:?}"
        );
        let pasted = std::fs::read_to_string(&pasted_log).unwrap();
        assert_eq!(
            pasted.trim_end(),
            "/rename Cleaned up title",
            "the pasted rename command must carry the new title: {pasted:?}"
        );
    }

    #[tokio::test]
    async fn update_session_title_does_not_rename_non_claude_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let calls_log = tmp.path().join("tmux-calls.log");
        let pasted_log = tmp.path().join("tmux-pasted.log");
        let tmux = rename_probe_tmux(tmp.path(), &calls_log, &pasted_log, true);
        seed_session(tmp.path(), "steer", "codex", "hs-codex-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/steer",
                Some(TOKEN),
                json!({ "title": "Cleaned up title" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], "Cleaned up title");

        let calls = std::fs::read_to_string(&calls_log).unwrap_or_default();
        assert!(
            !calls.contains("paste-buffer"),
            "Codex has no prompt-bar display name; it must not receive /rename: {calls:?}"
        );
        assert!(
            !pasted_log.exists(),
            "no rename text should be pasted for a non-Claude session"
        );
    }

    #[tokio::test]
    async fn update_session_title_skips_rename_for_stopped_claude_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let calls_log = tmp.path().join("tmux-calls.log");
        let pasted_log = tmp.path().join("tmux-pasted.log");
        let tmux = rename_probe_tmux(tmp.path(), &calls_log, &pasted_log, false);
        seed_session(tmp.path(), "steer", "claude", "hs-claude-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            patch_json(
                "/sessions/steer",
                Some(TOKEN),
                json!({ "title": "New title" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], "New title");
        assert_eq!(body["data"]["session"]["status"], "stopped");

        let calls = std::fs::read_to_string(&calls_log).unwrap_or_default();
        assert!(
            !calls.contains("paste-buffer"),
            "a stopped session has no live pane to rename: {calls:?}"
        );
        assert!(
            !pasted_log.exists(),
            "no rename text should be pasted for a stopped session"
        );
    }

    #[tokio::test]
    async fn update_session_title_clearing_to_null_does_not_rename() {
        let tmp = tempfile::TempDir::new().unwrap();
        let calls_log = tmp.path().join("tmux-calls.log");
        let pasted_log = tmp.path().join("tmux-pasted.log");
        let tmux = rename_probe_tmux(tmp.path(), &calls_log, &pasted_log, true);
        seed_session(tmp.path(), "steer", "claude", "hs-claude-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, _body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", Some(TOKEN), json!({ "title": "First" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Clearing the title must not fire `/rename` with no argument (which would
        // make Claude auto-generate an unrelated name).
        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", Some(TOKEN), json!({ "title": null })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], Value::Null);

        let pasted = std::fs::read_to_string(&pasted_log).unwrap();
        assert_eq!(
            pasted.matches("/rename").count(),
            1,
            "only the non-empty title should have been pushed as a rename: {pasted:?}"
        );
        assert!(
            pasted.contains("/rename First"),
            "the one rename must be the non-empty title: {pasted:?}"
        );
    }

    #[tokio::test]
    async fn new_write_routes_require_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "steer", "codex", "hs-codex-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            patch_json("/sessions/steer", None, json!({ "title": "Nope" })),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        let (status, body) = call(
            router(st.clone()),
            post_bytes(
                "/sessions/steer/attachments?filename=secret.png",
                None,
                b"bytes",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn upload_attachment_writes_private_session_file_with_safe_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "steer", "codex", "hs-codex-steer");
        let st = state(tmp.path(), Some(TOKEN), tmux);
        let payload = b"not actually a png";

        let (status, body) = call(
            router(st.clone()),
            post_bytes(
                "/sessions/steer/attachments?filename=..%2FScreen%20Shot.png",
                Some(TOKEN),
                payload,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        let attachment = &body["data"]["attachment"];
        assert_eq!(attachment["bytes"], payload.len());
        assert_eq!(attachment["filename"], "Screen_Shot.png");
        let path = PathBuf::from(attachment["path"].as_str().expect("attachment path"));
        assert_eq!(std::fs::read(&path).unwrap(), payload);
        assert!(path.starts_with(tmp.path().join("sessions/steer/attachments")));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "attachment file must be private");

        let (status, second) = call(
            router(st.clone()),
            post_bytes(
                "/sessions/steer/attachments?filename=Screen%20Shot.png",
                Some(TOKEN),
                b"second payload",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={second}");
        let second_path = PathBuf::from(second["data"]["attachment"]["path"].as_str().unwrap());
        assert_ne!(second_path, path);
        assert_eq!(std::fs::read(&path).unwrap(), payload);
        assert_eq!(std::fs::read(&second_path).unwrap(), b"second payload");

        let (status, body) = call(
            router(st.clone()),
            post_bytes(
                "/sessions/ghost/attachments?filename=ghost.png",
                Some(TOKEN),
                b"ghost",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "session-not-found");

        let before_oversize = std::fs::read_dir(tmp.path().join("sessions/steer/attachments"))
            .unwrap()
            .count();
        let oversize = vec![0u8; MAX_ATTACHMENT_BYTES + 1];
        let (status, body) = call(
            router(st.clone()),
            post_bytes(
                "/sessions/steer/attachments?filename=oversize.bin",
                Some(TOKEN),
                &oversize,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["error"]["code"], "attachment-too-large");
        let after_oversize = std::fs::read_dir(tmp.path().join("sessions/steer/attachments"))
            .unwrap()
            .count();
        assert_eq!(after_oversize, before_oversize);
    }

    #[tokio::test]
    async fn workdir_search_filters_default_project_and_config_roots() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let project_match = home.join("Project/sympoies/agent-console");
        let second_match = home.join("Project/agent-tools");
        let too_deep_match = home.join("Project/a/b/c/d/e/agent-deep");
        let config_match = home.join(".config/zsh");
        let outside = home.join("Downloads/agent-console-copy");
        std::fs::create_dir_all(project_match.join(".git")).unwrap();
        std::fs::create_dir_all(&second_match).unwrap();
        std::fs::create_dir_all(&too_deep_match).unwrap();
        std::fs::create_dir_all(&config_match).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().unwrap());

        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let (status, body) = call(router(st.clone()), get("/workdirs?q=agent&limit=20")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        let (status, body) = call(
            router(st.clone()),
            get_auth("/workdirs?q=agent&limit=20", Some(TOKEN)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        let workdirs = body["data"]["workdirs"].as_array().expect("workdirs array");
        assert!(
            workdirs
                .iter()
                .any(|item| item["path"] == project_match.to_string_lossy().as_ref()),
            "Project match missing: {workdirs:?}"
        );
        assert!(
            !workdirs
                .iter()
                .any(|item| item["path"] == outside.to_string_lossy().as_ref()),
            "outside root must not be returned: {workdirs:?}"
        );
        assert!(
            !workdirs
                .iter()
                .any(|item| item["path"] == too_deep_match.to_string_lossy().as_ref()),
            "matches beyond max depth must not be returned: {workdirs:?}"
        );

        let (status, limited) = call(
            router(st.clone()),
            get_auth("/workdirs?q=agent&limit=1", Some(TOKEN)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={limited}");
        assert_eq!(
            limited["data"]["workdirs"]
                .as_array()
                .expect("workdirs")
                .len(),
            1
        );

        let (status, body) = call(
            router(st.clone()),
            get_auth("/workdirs?q=zsh&limit=20", Some(TOKEN)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let workdirs = body["data"]["workdirs"].as_array().expect("workdirs array");
        assert!(
            workdirs
                .iter()
                .any(|item| item["path"] == config_match.to_string_lossy().as_ref()),
            ".config match missing: {workdirs:?}"
        );
    }

    #[tokio::test]
    async fn repo_remote_url_resolves_github_and_gitlab_remotes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let github_repo = tmp.path().join("home/Project/sympoies/agent-console");
        let gitlab_repo = tmp.path().join("home/Project/acme/backend");
        init_git_remote(&github_repo, "git@github.com:sympoies/agent-console.git");
        init_git_remote(
            &gitlab_repo,
            "ssh://git@gitlab.example.com:2222/group/sub/project.git",
        );

        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let unauthorized = format!("/repos/remote-url?cwd={}", github_repo.to_string_lossy());
        let (status, body) = call(router(st.clone()), get(&unauthorized)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "unauthorized");

        let github_path = format!("/repos/remote-url?cwd={}", github_repo.to_string_lossy());
        let (status, body) = call(router(st.clone()), get_auth(&github_path, Some(TOKEN))).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["machine"], "test-machine");
        assert_eq!(
            body["data"]["url"],
            "https://github.com/sympoies/agent-console"
        );

        let gitlab_path = format!("/repos/remote-url?cwd={}", gitlab_repo.to_string_lossy());
        let (status, body) = call(router(st.clone()), get_auth(&gitlab_path, Some(TOKEN))).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(
            body["data"]["url"],
            "https://gitlab.example.com/group/sub/project"
        );

        let missing = tmp.path().join("home/Project/no-remote");
        std::fs::create_dir_all(&missing).unwrap();
        let missing_path = format!("/repos/remote-url?cwd={}", missing.to_string_lossy());
        let (status, body) = call(router(st), get_auth(&missing_path, Some(TOKEN))).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert!(body["data"]["url"].is_null());
    }

    #[tokio::test]
    async fn workdir_search_can_return_git_repos_without_linked_worktrees() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let project_repo = home.join("Project/sympoies/agent-console");
        let linked_worktree = home.join("Project/sympoies/wt-agent-console-21");
        let non_git_dir = home.join("Project/sympoies/notes");
        let config_repo = home.join(".config/zsh");
        std::fs::create_dir_all(project_repo.join(".git")).unwrap();
        std::fs::create_dir_all(&linked_worktree).unwrap();
        std::fs::write(
            linked_worktree.join(".git"),
            "gitdir: ../agent-console/.git/worktrees/wt-agent-console-21\n",
        )
        .unwrap();
        std::fs::create_dir_all(&non_git_dir).unwrap();
        std::fs::create_dir_all(config_repo.join(".git")).unwrap();
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().unwrap());

        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let (status, body) = call(
            router(st.clone()),
            get_auth(
                "/workdirs?q=&limit=20&git_only=true&exclude_worktrees=true",
                Some(TOKEN),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let workdirs = body["data"]["workdirs"].as_array().expect("workdirs array");
        let paths: Vec<&str> = workdirs
            .iter()
            .filter_map(|item| item["path"].as_str())
            .collect();
        assert!(
            paths.contains(&project_repo.to_string_lossy().as_ref()),
            "primary project repo missing: {workdirs:?}"
        );
        assert!(
            paths.contains(&config_repo.to_string_lossy().as_ref()),
            "config repo missing: {workdirs:?}"
        );
        assert!(
            !paths.contains(&non_git_dir.to_string_lossy().as_ref()),
            "non-git dir must not be returned: {workdirs:?}"
        );
        assert!(
            !paths.contains(&linked_worktree.to_string_lossy().as_ref()),
            "linked worktree must not be returned: {workdirs:?}"
        );
        assert!(
            workdirs.iter().all(|item| item["is_git_repo"] == true),
            "all returned rows must be git repos: {workdirs:?}"
        );
        assert!(
            workdirs.iter().all(|item| item.get("last_used").is_some()),
            "last_used field must be present for every row: {workdirs:?}"
        );
    }

    #[tokio::test]
    async fn create_records_workdir_usage_for_recent_first_search() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let alpha_repo = home.join("Project/sympoies/alpha");
        let beta_repo = home.join("Project/sympoies/beta");
        std::fs::create_dir_all(alpha_repo.join(".git")).unwrap();
        std::fs::create_dir_all(beta_repo.join(".git")).unwrap();
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().unwrap());
        let tmux = minimal_tmux(tmp.path());
        let st = state(tmp.path(), Some(TOKEN), tmux);

        let (status, body) = call(
            router(st.clone()),
            post_json(
                "/sessions",
                Some(TOKEN),
                json!({
                    "agent": "codex",
                    "id": "uses-beta",
                    "cwd": beta_repo.to_string_lossy(),
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");

        let (status, body) = call(
            router(st.clone()),
            get_auth(
                "/workdirs?q=&limit=20&git_only=true&exclude_worktrees=true",
                Some(TOKEN),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let workdirs = body["data"]["workdirs"].as_array().expect("workdirs array");
        assert_eq!(
            workdirs
                .first()
                .and_then(|item| item["path"].as_str())
                .unwrap_or_default(),
            beta_repo.to_string_lossy().as_ref(),
            "created cwd must rank first: {workdirs:?}"
        );
        assert!(
            workdirs
                .first()
                .and_then(|item| item["last_used"].as_str())
                .is_some_and(|value| !value.is_empty()),
            "created cwd must expose last_used: {workdirs:?}"
        );
        let alpha = workdirs
            .iter()
            .find(|item| item["path"] == alpha_repo.to_string_lossy().as_ref())
            .expect("unused repo row");
        assert_eq!(alpha["last_used"], Value::Null);
    }

    #[tokio::test]
    async fn workdir_search_rejects_symlink_roots() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(outside.join("agent-secret")).unwrap();
        symlink(&outside, home.join("Project")).unwrap();
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().unwrap());

        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let (status, body) = call(
            router(st.clone()),
            get_auth("/workdirs?q=agent&limit=20", Some(TOKEN)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let workdirs = body["data"]["workdirs"].as_array().expect("workdirs array");
        assert!(
            workdirs.is_empty(),
            "symlink roots must not expose outside directories: {workdirs:?}"
        );
    }

    #[test]
    fn serve_default_bind_is_loopback() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::try_parse_from(["agent-session", "serve"]).unwrap();
        let Command::Serve(args) = cli.command else {
            panic!("expected serve command");
        };
        let addr: SocketAddr = args.bind.parse().unwrap();
        assert!(addr.ip().is_loopback(), "default bind must be loopback");
    }

    #[test]
    fn serve_token_stdin_conflicts_with_token_without_leaking_token() {
        use crate::cli::Cli;
        use clap::Parser;

        let err = Cli::try_parse_from([
            "agent-session",
            "serve",
            "--token",
            "secret-from-argv",
            "--token-stdin",
        ])
        .unwrap_err();
        let message = err.to_string();

        assert!(message.contains("--token"));
        assert!(message.contains("--token-stdin"));
        assert!(
            !message.contains("secret-from-argv"),
            "parse error leaked token material: {message}"
        );
    }

    #[test]
    fn token_stdin_reads_single_trimmed_token() {
        assert_eq!(
            read_token_from_stdin("  stdin-token\n".as_bytes()).unwrap(),
            "stdin-token"
        );
    }

    #[test]
    fn token_stdin_rejects_empty_input_without_leaking_material() {
        let err = read_token_from_stdin("   \n".as_bytes()).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("empty"));
        assert!(!message.contains("\\n"));
    }

    #[test]
    fn token_stdin_rejects_multiple_lines_without_leaking_material() {
        let err = read_token_from_stdin("first-token\nsecond-token\n".as_bytes()).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("exactly one token"));
        assert!(!message.contains("first-token"));
        assert!(!message.contains("second-token"));
    }

    #[test]
    fn token_stdin_rejects_oversized_input() {
        let oversized = "a".repeat((MAX_STDIN_TOKEN_BYTES + 1) as usize);
        let err = read_token_from_stdin(oversized.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("8192 bytes"));
    }

    #[tokio::test]
    async fn create_rejects_unknown_agent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let (status, body) = call(
            router(st.clone()),
            post_json("/sessions", Some(TOKEN), json!({"agent": "banana"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid-agent");
    }

    #[tokio::test]
    async fn glance_reads_open_with_machine() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "look", "codex", "hs-codex-look");
        let st = state(tmp.path(), Some(TOKEN), tmux);
        // No auth header: glance is a read, open on loopback.
        let (status, body) = call(router(st.clone()), get("/sessions/look/glance")).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        assert_eq!(body["data"]["machine"], MACHINE);
        assert!(body["data"]["glance"].is_object());
    }

    #[tokio::test]
    async fn buffer_returns_tmux_show_buffer_text_open_on_reads() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        seed_session(tmp.path(), "look", "claude", "hs-claude-look");
        let st = state(tmp.path(), Some(TOKEN), tmux);
        // No auth header: buffer is a read, open on loopback like glance.
        let (status, body) = call(router(st.clone()), get("/sessions/look/buffer")).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["schema_version"], "cli.agent-session.serve.v1");
        assert_eq!(body["data"]["machine"], MACHINE);
        assert_eq!(body["data"]["text"], "buffered selection\n");
    }

    #[tokio::test]
    async fn buffer_unknown_session_is_not_found() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tmux = minimal_tmux(tmp.path());
        let st = state(tmp.path(), Some(TOKEN), tmux);
        let (status, body) = call(router(st.clone()), get("/sessions/ghost/buffer")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "body={body}");
        assert_eq!(body["error"]["code"], "session-not-found");
    }

    #[tokio::test]
    async fn buffer_reports_empty_when_no_buffer_set() {
        // A fresh tmux server has no paste buffer yet: `show-buffer` exits non-zero
        // ("no buffers"), which must surface as an empty selection, not an error.
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("tmux");
        std::fs::write(
            &bin,
            "#!/usr/bin/env sh\ncase \"$1\" in\n  show-buffer) echo 'no buffers' >&2; exit 1 ;;\n  *) exit 0 ;;\nesac\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        seed_session(tmp.path(), "look", "claude", "hs-claude-look");
        let st = state(tmp.path(), Some(TOKEN), bin);
        let (status, body) = call(router(st.clone()), get("/sessions/look/buffer")).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["text"], "");
    }

    #[tokio::test]
    async fn glance_preserves_provider_resume_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        seed_resumable_session(
            tmp.path(),
            "recover",
            "claude",
            "hs-claude-recover",
            &cwd,
            &["--resume", "resume-session-id"],
        );
        add_provider_resume_extra(tmp.path(), "recover");
        let st = state(tmp.path(), Some(TOKEN), minimal_tmux(tmp.path()));

        let (status, body) = call(router(st.clone()), get("/sessions/recover/glance")).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let glance = &body["data"]["glance"];
        assert_eq!(glance["provider_resume"]["provider"], "claude");
        assert_eq!(
            glance["provider_resume"]["resume_args"],
            json!(["--resume", "resume-session-id"])
        );
        assert!(glance["provider_resume"].get("storage_only").is_none());
    }

    #[tokio::test]
    async fn delete_missing_session_maps_to_404() {
        let tmp = tempfile::TempDir::new().unwrap();
        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let del = Request::builder()
            .method("DELETE")
            .uri("/sessions/ghost")
            .header("authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        let (status, body) = call(router(st.clone()), del).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "session-not-found");
    }

    #[tokio::test]
    async fn handle_input_resizes_and_ignores_malformed_frames() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("calls.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let ctx = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = test_record("look", "hs-codex-look");
        let target = "hs-codex-look:0.0";

        // Malformed JSON and an empty object must not touch tmux.
        let mut pending = false;
        handle_input(&ctx, &tmux, &record, target, "{ not json", &mut pending).await;
        handle_input(&ctx, &tmux, &record, target, "{}", &mut pending).await;
        let after_noops = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            after_noops.trim().is_empty(),
            "malformed/empty frames must not call tmux: {after_noops:?}"
        );

        // A non-initial resize frame maps to a single `tmux resize-window
        // -x <cols> -y <rows>` (no repaint nudge once the flag is spent).
        handle_input(
            &ctx,
            &tmux,
            &record,
            target,
            r#"{"resize":{"cols":123,"rows":45}}"#,
            &mut pending,
        )
        .await;
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        let resizes: Vec<&str> = calls
            .lines()
            .filter(|line| line.contains("resize-window"))
            .collect();
        assert_eq!(
            resizes.len(),
            1,
            "a non-initial resize must issue exactly one resize-window: {calls:?}"
        );
        assert!(
            resizes[0].contains("123") && resizes[0].contains("45"),
            "resize frame must invoke resize-window with the requested size: {calls:?}"
        );
    }

    #[tokio::test]
    async fn first_resize_forces_a_repaint_nudge_then_plain_resizes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("calls.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let ctx = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = test_record("look", "hs-codex-look");
        let target = "hs-codex-look:0.0";

        // First resize after attach: nudge to rows-1, then the real rows, so the
        // agent is guaranteed a SIGWINCH and repaints its whole frame into the
        // freshly rebuilt client grid (fixes stale layout on re-entry).
        let mut pending = true;
        handle_input(
            &ctx,
            &tmux,
            &record,
            target,
            r#"{"resize":{"cols":80,"rows":24}}"#,
            &mut pending,
        )
        .await;
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        let resizes: Vec<&str> = calls
            .lines()
            .filter(|line| line.contains("resize-window"))
            .collect();
        assert_eq!(
            resizes.len(),
            2,
            "the first resize must nudge then set the real size: {calls:?}"
        );
        assert!(
            resizes[0].contains("-y 23"),
            "nudge step must use rows-1: {:?}",
            resizes[0]
        );
        assert!(
            resizes[1].contains("-y 24"),
            "final step must use the requested rows: {:?}",
            resizes[1]
        );
        assert!(
            !pending,
            "the initial-repaint flag must be consumed after the first resize"
        );

        // Subsequent resizes are a single plain resize-window (no flicker).
        std::fs::write(&log, "").unwrap();
        handle_input(
            &ctx,
            &tmux,
            &record,
            target,
            r#"{"resize":{"cols":100,"rows":40}}"#,
            &mut pending,
        )
        .await;
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        let resizes: Vec<&str> = calls
            .lines()
            .filter(|line| line.contains("resize-window"))
            .collect();
        assert_eq!(resizes.len(), 1, "later resizes must not nudge: {calls:?}");
        assert!(
            resizes[0].contains("-y 40"),
            "later resize must use the requested rows: {:?}",
            resizes[0]
        );
    }

    #[test]
    fn from_name_round_trips_agent_and_key() {
        use clap::ValueEnum;
        for agent in AgentKind::value_variants() {
            assert_eq!(AgentKind::from_name(agent.as_str()), Some(*agent));
        }
        assert_eq!(AgentKind::from_name("banana"), None);
        for key in SpecialKey::value_variants() {
            assert_eq!(SpecialKey::from_name(key.as_str()), Some(*key));
        }
        assert_eq!(SpecialKey::from_name("f13"), None);
    }

    #[test]
    fn private_files_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        // The attach pipe is created via write_private_file; assert the mode is
        // 0600 so live pane output is not group/world-readable.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("attach.pipe");
        write_private_file(&path, b"").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "pipe file must not be group/world readable"
        );
    }

    fn logging_tmux(dir: &Path, log: &Path) -> PathBuf {
        let bin = dir.join("tmux");
        std::fs::write(
            &bin,
            format!(
                "#!/usr/bin/env sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$1\" in\n  has-session) exit 0 ;;\n  capture-pane) printf 'pane\\n'; exit 0 ;;\n  *) exit 0 ;;\nesac\n",
                shell_words::quote(&log.to_string_lossy())
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        bin
    }

    /// tmux stub that logs every invocation to `calls` and, on `load-buffer`,
    /// appends the pasted buffer file's content to `pasted`. `running` toggles
    /// the `has-session` result so title-rename gating can be exercised for both
    /// live and stopped sessions.
    fn rename_probe_tmux(dir: &Path, calls: &Path, pasted: &Path, running: bool) -> PathBuf {
        let bin = dir.join("tmux");
        let has_session_exit = if running { 0 } else { 1 };
        std::fs::write(
            &bin,
            format!(
                "#!/usr/bin/env sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$1\" in\n  has-session) exit {} ;;\n  load-buffer) cat \"$4\" >> {}; exit 0 ;;\n  capture-pane) printf 'pane\\n'; exit 0 ;;\n  *) exit 0 ;;\nesac\n",
                shell_words::quote(&calls.to_string_lossy()),
                has_session_exit,
                shell_words::quote(&pasted.to_string_lossy()),
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        bin
    }

    fn test_record(id: &str, tmux_session: &str) -> crate::SessionRecord {
        crate::SessionRecord {
            schema_version: crate::SESSION_DOCUMENT_VERSION.to_string(),
            id: id.to_string(),
            agent: "codex".to_string(),
            mode: "interactive".to_string(),
            title: None,
            cwd: "/tmp".to_string(),
            tmux_session: tmux_session.to_string(),
            prompt_file: None,
            log_file: None,
            created_at: "2000-01-01T00:00:00Z".to_string(),
            updated_at: "2000-01-01T00:00:00Z".to_string(),
            provider_resume: None,
            runtime: None,
            agent_args: Vec::new(),
            agent_bin: None,
            extra: std::collections::BTreeMap::new(),
            resume_sidecar_extra: std::collections::BTreeMap::new(),
        }
    }
}

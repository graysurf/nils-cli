use std::io::Write;
use std::path::{Path, PathBuf};

use api_testing_core::cli_history::{
    RequestCallHistoryAuth, RequestCallHistoryFlag, RequestCallHistoryRecord,
    build_request_call_history_record,
};
use api_testing_core::{Result, auth_env, cli_endpoint, cli_util, config, history, jwt};
use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};

use crate::cli::{CallArgs, OutputFormat};
use api_testing_core::cli_util::{history_timestamp_now, parse_u64_default, trim_non_empty};

const CALL_SCHEMA_VERSION: &str = "cli.api-websocket.call.v1";

#[derive(Debug, Clone)]
pub(crate) struct EndpointSelection {
    pub(crate) websocket_url: String,
    pub(crate) endpoint_label_used: String,
    pub(crate) endpoint_value_used: String,
}

pub(crate) type AuthSourceUsed = auth_env::CliAuthSource;

#[derive(Debug, Clone)]
pub(crate) struct AuthSelection {
    pub(crate) bearer_token: Option<String>,
    pub(crate) token_name: String,
    pub(crate) auth_source_used: AuthSourceUsed,
}

fn endpoint_from_env_passthrough(args: &CallArgs) -> Option<EndpointSelection> {
    let raw = args.env.as_deref().and_then(trim_non_empty)?;
    if raw.starts_with("ws://") || raw.starts_with("wss://") {
        return Some(EndpointSelection {
            websocket_url: raw.clone(),
            endpoint_label_used: "url".to_string(),
            endpoint_value_used: raw,
        });
    }
    None
}

pub(crate) fn resolve_endpoint_for_call(
    args: &CallArgs,
    setup: &api_testing_core::config::ResolvedSetup,
) -> Result<EndpointSelection> {
    if let Some(selected) = endpoint_from_env_passthrough(args) {
        return Ok(selected);
    }

    let endpoints_env = &setup.endpoints_env;
    let endpoints_local = &setup.endpoints_local_env;
    let endpoints_files = setup.endpoints_files();

    let selection = cli_endpoint::resolve_cli_endpoint(cli_endpoint::EndpointConfig {
        explicit_url: args.url.as_deref(),
        env_name: args.env.as_deref(),
        endpoints_env,
        endpoints_local,
        endpoints_files: &endpoints_files,
        url_env_var: "WS_URL",
        env_default_var: "WS_ENV_DEFAULT",
        url_prefix: "WS_URL_",
        default_url: "ws://127.0.0.1:9001/ws",
        setup_dir_label: "setup/websocket/",
    })?;

    Ok(EndpointSelection {
        websocket_url: selection.url,
        endpoint_label_used: selection.endpoint_label_used,
        endpoint_value_used: selection.endpoint_value_used,
    })
}

pub(crate) fn resolve_auth_for_call(
    args: &CallArgs,
    setup: &api_testing_core::config::ResolvedSetup,
) -> Result<AuthSelection> {
    let tokens_env = setup.tokens_env.as_ref().expect("tokens_env");
    let tokens_local = setup.tokens_local_env.as_ref().expect("tokens_local_env");
    let tokens_files = setup.tokens_files();
    let token_resolution = auth_env::resolve_profile_or_env_fallback(
        auth_env::ProfileTokenConfig {
            token_name_arg: args.token.as_deref(),
            token_name_env_var: "WS_TOKEN_NAME",
            token_name_file_var: "WS_TOKEN_NAME",
            token_var_prefix: "WS_TOKEN_",
            tokens_env,
            tokens_local,
            tokens_files: &tokens_files,
            missing_profile_hint: "Set it in setup/websocket/tokens.local.env or use ACCESS_TOKEN without selecting a token profile.",
            env_fallback_keys: &["ACCESS_TOKEN", "SERVICE_TOKEN"],
        },
    )?;

    Ok(AuthSelection {
        bearer_token: token_resolution.bearer_token,
        token_name: token_resolution.token_name,
        auth_source_used: token_resolution.source.into(),
    })
}

pub(crate) fn validate_bearer_token_if_jwt(
    bearer_token: &str,
    auth_source: &AuthSourceUsed,
    token_name: &str,
    stderr: &mut dyn Write,
) -> Result<()> {
    let enabled = cli_util::bool_from_env(
        std::env::var("WS_JWT_VALIDATE_ENABLED").ok(),
        "WS_JWT_VALIDATE_ENABLED",
        true,
        Some("api-websocket"),
        stderr,
    );
    let strict = cli_util::bool_from_env(
        std::env::var("WS_JWT_VALIDATE_STRICT").ok(),
        "WS_JWT_VALIDATE_STRICT",
        false,
        Some("api-websocket"),
        stderr,
    );
    let leeway_seconds =
        parse_u64_default(std::env::var("WS_JWT_VALIDATE_LEEWAY_SECONDS").ok(), 0, 0);

    let label = match auth_source {
        AuthSourceUsed::TokenProfile => format!("token profile '{token_name}'"),
        AuthSourceUsed::EnvFallback { env_name } => env_name.to_string(),
        AuthSourceUsed::None => "token".to_string(),
    };

    let opts = jwt::JwtValidationOptions {
        enabled,
        strict,
        leeway_seconds: i64::try_from(leeway_seconds).unwrap_or(i64::MAX),
    };

    match jwt::check_bearer_jwt(bearer_token, &label, opts)? {
        jwt::JwtCheck::Ok => Ok(()),
        jwt::JwtCheck::Warn(msg) => {
            let _ = writeln!(stderr, "api-websocket: warning: {msg}");
            Ok(())
        }
    }
}

pub(crate) fn cmd_call(
    args: &CallArgs,
    invocation_dir: &Path,
    stdout_is_tty: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    cmd_call_internal(args, invocation_dir, stdout_is_tty, true, stdout, stderr)
}

pub(crate) fn cmd_call_internal(
    args: &CallArgs,
    invocation_dir: &Path,
    stdout_is_tty: bool,
    history_enabled_by_command: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let request_path = PathBuf::from(&args.request);
    if !request_path.is_file() {
        return fail_call(
            args,
            stdout,
            stderr,
            "request_not_found",
            format!("Request file not found: {}", request_path.display()),
            None,
        );
    }

    let request_file =
        match api_testing_core::websocket::schema::WebsocketRequestFile::load(&request_path) {
            Ok(v) => v,
            Err(err) => {
                return fail_call(
                    args,
                    stdout,
                    stderr,
                    "request_parse_error",
                    format!("{err}"),
                    None,
                );
            }
        };

    let config_dir = args
        .config_dir
        .as_deref()
        .and_then(trim_non_empty)
        .map(PathBuf::from);
    let setup_dir = match config::resolve_websocket_setup_dir_for_call(
        invocation_dir,
        invocation_dir,
        &request_path,
        config_dir.as_deref(),
    ) {
        Ok(v) => v,
        Err(err) => {
            return fail_call(
                args,
                stdout,
                stderr,
                "setup_resolve_error",
                format!("{err}"),
                None,
            );
        }
    };

    let mut exit_code = 1;

    let history_enabled = history_enabled_by_command
        && !args.no_history
        && cli_util::bool_from_env(
            std::env::var("WS_HISTORY_ENABLED").ok(),
            "WS_HISTORY_ENABLED",
            true,
            Some("api-websocket"),
            stderr,
        );

    let history_file_override = std::env::var("WS_HISTORY_FILE")
        .ok()
        .and_then(|s| trim_non_empty(&s))
        .map(PathBuf::from);

    let setup = api_testing_core::config::ResolvedSetup::websocket(
        setup_dir.clone(),
        history_file_override.as_deref(),
    );

    let rotation = history::RotationPolicy {
        max_mb: parse_u64_default(std::env::var("WS_HISTORY_MAX_MB").ok(), 10, 0),
        keep: parse_u64_default(std::env::var("WS_HISTORY_ROTATE_COUNT").ok(), 5, 1)
            .try_into()
            .unwrap_or(u32::MAX),
    };

    let log_url = cli_util::bool_from_env(
        std::env::var("WS_HISTORY_LOG_URL_ENABLED").ok(),
        "WS_HISTORY_LOG_URL_ENABLED",
        true,
        Some("api-websocket"),
        stderr,
    );

    let history_writer = history::HistoryWriter::new(setup.history_file.clone(), rotation);

    let mut history_ctx = CallHistoryContext {
        enabled: history_enabled,
        setup_dir: setup_dir.clone(),
        history_writer,
        invocation_dir: invocation_dir.to_path_buf(),
        request_arg: args.request.clone(),
        endpoint_label_used: String::new(),
        endpoint_value_used: String::new(),
        log_url,
        auth_source_used: AuthSourceUsed::None,
        token_name_for_log: String::new(),
        output_format: args.format,
    };

    let endpoint = match resolve_endpoint_for_call(args, &setup) {
        Ok(v) => {
            history_ctx.endpoint_label_used = v.endpoint_label_used.clone();
            history_ctx.endpoint_value_used = v.endpoint_value_used.clone();
            v
        }
        Err(err) => {
            let code = fail_call(
                args,
                stdout,
                stderr,
                "endpoint_resolve_error",
                format!("{err}"),
                None,
            );
            append_history_best_effort(&history_ctx, exit_code, stderr);
            return code;
        }
    };

    let auth = match resolve_auth_for_call(args, &setup) {
        Ok(v) => {
            history_ctx.auth_source_used = v.auth_source_used.clone();
            history_ctx.token_name_for_log = v.token_name.clone();
            v
        }
        Err(err) => {
            let code = fail_call(
                args,
                stdout,
                stderr,
                "auth_resolve_error",
                format!("{err}"),
                None,
            );
            append_history_best_effort(&history_ctx, exit_code, stderr);
            return code;
        }
    };

    if let Some(token) = auth.bearer_token.as_deref()
        && let Err(err) = validate_bearer_token_if_jwt(
            token,
            &history_ctx.auth_source_used,
            &auth.token_name,
            stderr,
        )
    {
        let code = fail_call(
            args,
            stdout,
            stderr,
            "jwt_validation_error",
            format!("{err}"),
            None,
        );
        append_history_best_effort(&history_ctx, exit_code, stderr);
        return code;
    }

    let json_mode = matches!(args.format, OutputFormat::Json);
    let spinner = if json_mode {
        None
    } else {
        let spinner = Progress::spinner(
            ProgressOptions::default()
                .with_prefix("api-websocket ")
                .with_finish(ProgressFinish::Clear),
        );
        spinner.set_message("request");
        spinner.tick();
        Some(spinner)
    };

    let executed = match api_testing_core::websocket::runner::execute_websocket_request(
        &request_file,
        &endpoint.websocket_url,
        auth.bearer_token.as_deref(),
    ) {
        Ok(v) => v,
        Err(err) => {
            if let Some(s) = &spinner {
                s.finish_and_clear();
            }
            let code = fail_call(
                args,
                stdout,
                stderr,
                "websocket_execute_error",
                format!("{err}"),
                Some(serde_json::json!({"target": endpoint.websocket_url})),
            );
            append_history_best_effort(&history_ctx, exit_code, stderr);
            return code;
        }
    };

    let last_received = executed.last_received.clone().unwrap_or_default();
    if !json_mode {
        let _ = stdout.write_all(last_received.as_bytes());
    }

    if let Err(err) = api_testing_core::websocket::expect::evaluate_main_response(
        &request_file.request,
        &executed,
    ) {
        if let Some(s) = &spinner {
            s.finish_and_clear();
        }
        if json_mode {
            let _ = write_json_failure(
                stdout,
                "expectation_failed",
                &format!("{err}"),
                Some(serde_json::json!({
                    "target": executed.target,
                    "last_received": executed.last_received,
                })),
            );
        } else {
            let _ = writeln!(stderr, "{err}");
            maybe_print_failure_body_to_stderr(&last_received, 8192, stdout_is_tty, stderr);
        }
        append_history_best_effort(&history_ctx, exit_code, stderr);
        return 1;
    }

    if let Some(s) = &spinner {
        s.finish_and_clear();
    }

    if json_mode {
        let payload = serde_json::json!({
            "schema_version": CALL_SCHEMA_VERSION,
            "command": "api-websocket call",
            "ok": true,
            "result": {
                "target": executed.target,
                "last_received": executed.last_received,
                "transcript": executed.transcript,
            }
        });
        let _ = serde_json::to_writer_pretty(&mut *stdout, &payload);
        let _ = stdout.write_all(b"\n");
    }

    exit_code = 0;
    append_history_best_effort(&history_ctx, exit_code, stderr);

    exit_code
}

fn fail_call(
    args: &CallArgs,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    error_code: &str,
    message: String,
    details: Option<serde_json::Value>,
) -> i32 {
    if matches!(args.format, OutputFormat::Json) {
        let _ = write_json_failure(stdout, error_code, &message, details);
    } else {
        let _ = writeln!(stderr, "{message}");
    }
    1
}

fn write_json_failure(
    stdout: &mut dyn Write,
    error_code: &str,
    message: &str,
    details: Option<serde_json::Value>,
) -> Result<()> {
    let mut error = serde_json::json!({
        "code": error_code,
        "message": message,
    });
    if let Some(details) = details {
        error["details"] = details;
    }

    let payload = serde_json::json!({
        "schema_version": CALL_SCHEMA_VERSION,
        "command": "api-websocket call",
        "ok": false,
        "error": error,
    });

    serde_json::to_writer_pretty(&mut *stdout, &payload)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

#[derive(Debug, Clone)]
struct CallHistoryContext {
    enabled: bool,
    setup_dir: PathBuf,
    history_writer: history::HistoryWriter,
    invocation_dir: PathBuf,
    request_arg: String,
    endpoint_label_used: String,
    endpoint_value_used: String,
    log_url: bool,
    auth_source_used: AuthSourceUsed,
    token_name_for_log: String,
    output_format: OutputFormat,
}

fn append_history_best_effort(ctx: &CallHistoryContext, exit_code: i32, stderr: &mut dyn Write) {
    if !ctx.enabled {
        return;
    }

    let history_writer = &ctx.history_writer;
    let stamp = history_timestamp_now().unwrap_or_default();
    let auth = match &ctx.auth_source_used {
        AuthSourceUsed::TokenProfile => RequestCallHistoryAuth::HeaderAndFlag {
            header_key: "token",
            header_value: &ctx.token_name_for_log,
            flag_name: "token",
            flag_value: &ctx.token_name_for_log,
        },
        AuthSourceUsed::EnvFallback { env_name } => RequestCallHistoryAuth::HeaderOnly {
            key: "auth",
            value: env_name,
        },
        AuthSourceUsed::None => RequestCallHistoryAuth::None,
    };
    let json_flag = [RequestCallHistoryFlag::raw("format", "json")];
    let extra_flags: &[RequestCallHistoryFlag<'_>] =
        if matches!(ctx.output_format, OutputFormat::Json) {
            &json_flag
        } else {
            &[]
        };

    let record = build_request_call_history_record(RequestCallHistoryRecord {
        stamp: &stamp,
        exit_code,
        setup_dir: &ctx.setup_dir,
        invocation_dir: &ctx.invocation_dir,
        command_name: "api-websocket",
        endpoint_label_used: &ctx.endpoint_label_used,
        endpoint_value_used: &ctx.endpoint_value_used,
        log_url: ctx.log_url,
        auth,
        request_arg: &ctx.request_arg,
        extra_flags,
    });

    if let Err(err) = history_writer.append(&record) {
        let _ = writeln!(
            stderr,
            "warning: failed to append api-websocket history: {err}"
        );
    }
}

fn maybe_print_failure_body_to_stderr(
    body: &str,
    max_bytes: usize,
    stdout_is_tty: bool,
    stderr: &mut dyn Write,
) {
    if stdout_is_tty || body.trim().is_empty() {
        return;
    }

    if serde_json::from_str::<serde_json::Value>(body).is_ok() {
        return;
    }

    let bytes = body.as_bytes();
    let _ = writeln!(stderr, "Response body (non-JSON; first {max_bytes} bytes):");
    let _ = stderr.write_all(&bytes[..bytes.len().min(max_bytes)]);
    let _ = writeln!(stderr);
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use std::fs;
    use tempfile::tempdir;

    fn write_file(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir parent");
        fs::write(path, contents).expect("write");
    }

    fn test_history_writer(path: &Path) -> history::HistoryWriter {
        history::HistoryWriter::new(
            path.to_path_buf(),
            history::RotationPolicy {
                max_mb: 10,
                keep: 5,
            },
        )
    }

    #[test]
    fn resolve_auth_for_call_prefers_profile_then_env_fallback() {
        let lock = GlobalStateLock::new();
        let _access = EnvGuard::set(&lock, "ACCESS_TOKEN", "env-token");
        let _name = EnvGuard::remove(&lock, "WS_TOKEN_NAME");

        let tmp = tempdir().expect("tempdir");
        let setup_dir = tmp.path().join("setup/websocket");
        fs::create_dir_all(&setup_dir).expect("mkdir setup");
        write_file(&setup_dir.join("tokens.env"), "WS_TOKEN_SVC=svc-token\n");
        let setup = api_testing_core::config::ResolvedSetup::websocket(setup_dir, None);

        let args = CallArgs {
            env: None,
            url: None,
            token: Some("svc".to_string()),
            config_dir: None,
            no_history: false,
            format: OutputFormat::Text,
            request: "requests/health.ws.json".to_string(),
        };
        let auth = resolve_auth_for_call(&args, &setup).expect("token profile resolution");
        assert_eq!(auth.bearer_token.as_deref(), Some("svc-token"));
        assert!(matches!(
            auth.auth_source_used,
            AuthSourceUsed::TokenProfile
        ));

        let args = CallArgs {
            env: None,
            url: None,
            token: None,
            config_dir: None,
            no_history: false,
            format: OutputFormat::Text,
            request: "requests/health.ws.json".to_string(),
        };
        let auth = resolve_auth_for_call(&args, &setup).expect("env fallback resolution");
        assert_eq!(auth.bearer_token.as_deref(), Some("env-token"));
        assert!(matches!(
            auth.auth_source_used,
            AuthSourceUsed::EnvFallback { .. }
        ));
    }

    #[test]
    fn append_history_writes_env_and_token_command() {
        let tmp = tempdir().expect("tempdir");
        let setup_dir = tmp.path().join("setup/websocket");
        fs::create_dir_all(&setup_dir).expect("mkdir setup");
        let history_file = tmp.path().join(".ws_history");

        let ctx = CallHistoryContext {
            enabled: true,
            setup_dir: setup_dir.clone(),
            history_writer: test_history_writer(&history_file),
            invocation_dir: tmp.path().to_path_buf(),
            request_arg: "requests/health.ws.json".to_string(),
            endpoint_label_used: "env".to_string(),
            endpoint_value_used: "local".to_string(),
            log_url: true,
            auth_source_used: AuthSourceUsed::TokenProfile,
            token_name_for_log: "default".to_string(),
            output_format: OutputFormat::Text,
        };

        append_history_best_effort(&ctx, 0, &mut std::io::sink());

        let text = fs::read_to_string(&history_file).expect("history text");
        assert!(text.contains("api-websocket call \\\n"));
        assert!(text.contains("--config-dir"));
        assert!(text.contains("--env "));
        assert!(text.contains("local"));
        assert!(text.contains("--token "));
        assert!(text.contains("default"));
        assert!(text.contains("requests/health.ws.json"));
        assert!(text.contains("exit=0"));
    }

    #[test]
    fn append_history_omits_url_value_when_log_url_disabled() {
        let tmp = tempdir().expect("tempdir");
        let setup_dir = tmp.path().join("setup/websocket");
        fs::create_dir_all(&setup_dir).expect("mkdir setup");
        let history_file = tmp.path().join(".ws_history");

        let ctx = CallHistoryContext {
            enabled: true,
            setup_dir: setup_dir.clone(),
            history_writer: test_history_writer(&history_file),
            invocation_dir: tmp.path().to_path_buf(),
            request_arg: "/abs/requests/health.ws.json".to_string(),
            endpoint_label_used: "url".to_string(),
            endpoint_value_used: "ws://127.0.0.1:9001/ws".to_string(),
            log_url: false,
            auth_source_used: AuthSourceUsed::EnvFallback {
                env_name: "ACCESS_TOKEN".to_string(),
            },
            token_name_for_log: String::new(),
            output_format: OutputFormat::Text,
        };

        append_history_best_effort(&ctx, 7, &mut std::io::sink());

        let text = fs::read_to_string(&history_file).expect("history text");
        assert!(text.contains("url=<omitted>"));
        assert!(!text.contains("--url "));
        assert!(text.contains("auth=ACCESS_TOKEN"));
        assert!(text.contains("exit=7"));
    }

    #[test]
    fn append_history_records_json_format_flag() {
        let tmp = tempdir().expect("tempdir");
        let setup_dir = tmp.path().join("setup/websocket");
        fs::create_dir_all(&setup_dir).expect("mkdir setup");
        let history_file = tmp.path().join(".ws_history");

        let ctx = CallHistoryContext {
            enabled: true,
            setup_dir,
            history_writer: test_history_writer(&history_file),
            invocation_dir: tmp.path().to_path_buf(),
            request_arg: "req.ws.json".to_string(),
            endpoint_label_used: String::new(),
            endpoint_value_used: String::new(),
            log_url: true,
            auth_source_used: AuthSourceUsed::None,
            token_name_for_log: String::new(),
            output_format: OutputFormat::Json,
        };

        append_history_best_effort(&ctx, 0, &mut std::io::sink());
        let text = fs::read_to_string(&history_file).expect("history text");
        assert!(text.contains("--format json"));
    }

    #[test]
    fn append_history_disabled_does_not_create_history_file() {
        let tmp = tempdir().expect("tempdir");
        let setup_dir = tmp.path().join("setup/websocket");
        fs::create_dir_all(&setup_dir).expect("mkdir setup");
        let history_file = tmp.path().join(".ws_history");

        let ctx = CallHistoryContext {
            enabled: false,
            setup_dir,
            history_writer: test_history_writer(&history_file),
            invocation_dir: tmp.path().to_path_buf(),
            request_arg: "req.ws.json".to_string(),
            endpoint_label_used: String::new(),
            endpoint_value_used: String::new(),
            log_url: true,
            auth_source_used: AuthSourceUsed::None,
            token_name_for_log: String::new(),
            output_format: OutputFormat::Text,
        };

        append_history_best_effort(&ctx, 0, &mut std::io::sink());
        assert!(!history_file.exists());
    }

    #[test]
    fn maybe_print_failure_body_skips_when_stdout_is_tty() {
        let mut stderr = Vec::new();
        maybe_print_failure_body_to_stderr("not-json", 16, true, &mut stderr);
        assert!(stderr.is_empty());
    }

    #[test]
    fn maybe_print_failure_body_skips_when_response_is_json() {
        let mut stderr = Vec::new();
        maybe_print_failure_body_to_stderr("{\"ok\":true}", 16, false, &mut stderr);
        assert!(stderr.is_empty());
    }

    #[test]
    fn maybe_print_failure_body_prints_non_json_preview() {
        let mut stderr = Vec::new();
        maybe_print_failure_body_to_stderr("abcdef", 4, false, &mut stderr);
        let text = String::from_utf8(stderr).expect("utf8");
        assert!(text.contains("Response body (non-JSON; first 4 bytes):"));
        assert!(text.contains("abcd"));
    }
}

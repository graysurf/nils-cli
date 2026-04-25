use std::io::Write;
use std::path::{Path, PathBuf};

use api_testing_core::cli_history::{
    RequestCallHistoryAuth, RequestCallHistoryRecord, build_request_call_history_record,
};
use api_testing_core::{Result, auth_env, cli_endpoint, cli_io, cli_util, config, history, jwt};
use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};

use crate::cli::CallArgs;
use api_testing_core::cli_util::{history_timestamp_now, parse_u64_default, trim_non_empty};

#[derive(Debug, Clone)]
pub(crate) struct EndpointSelection {
    pub(crate) grpc_target: String,
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

pub(crate) fn resolve_endpoint_for_call(
    args: &CallArgs,
    setup: &api_testing_core::config::ResolvedSetup,
) -> Result<EndpointSelection> {
    let endpoints_env = &setup.endpoints_env;
    let endpoints_local = &setup.endpoints_local_env;
    let endpoints_files = setup.endpoints_files();

    let selection = cli_endpoint::resolve_cli_endpoint(cli_endpoint::EndpointConfig {
        explicit_url: args.url.as_deref(),
        env_name: args.env.as_deref(),
        endpoints_env,
        endpoints_local,
        endpoints_files: &endpoints_files,
        url_env_var: "GRPC_URL",
        env_default_var: "GRPC_ENV_DEFAULT",
        url_prefix: "GRPC_URL_",
        default_url: "127.0.0.1:50051",
        setup_dir_label: "setup/grpc/",
    })?;

    Ok(EndpointSelection {
        grpc_target: selection.url,
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
            token_name_env_var: "GRPC_TOKEN_NAME",
            token_name_file_var: "GRPC_TOKEN_NAME",
            token_var_prefix: "GRPC_TOKEN_",
            tokens_env,
            tokens_local,
            tokens_files: &tokens_files,
            missing_profile_hint: "Set it in setup/grpc/tokens.local.env or use ACCESS_TOKEN without selecting a token profile.",
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
        std::env::var("GRPC_JWT_VALIDATE_ENABLED").ok(),
        "GRPC_JWT_VALIDATE_ENABLED",
        true,
        Some("api-grpc"),
        stderr,
    );
    let strict = cli_util::bool_from_env(
        std::env::var("GRPC_JWT_VALIDATE_STRICT").ok(),
        "GRPC_JWT_VALIDATE_STRICT",
        false,
        Some("api-grpc"),
        stderr,
    );
    let leeway_seconds =
        parse_u64_default(std::env::var("GRPC_JWT_VALIDATE_LEEWAY_SECONDS").ok(), 0, 0);

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
            let _ = writeln!(stderr, "api-grpc: warning: {msg}");
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
        let _ = writeln!(stderr, "Request file not found: {}", request_path.display());
        return 1;
    }

    let request_file = match api_testing_core::grpc::schema::GrpcRequestFile::load(&request_path) {
        Ok(v) => v,
        Err(err) => {
            let _ = writeln!(stderr, "{err}");
            return 1;
        }
    };

    let config_dir = args
        .config_dir
        .as_deref()
        .and_then(trim_non_empty)
        .map(PathBuf::from);
    let setup_dir = match config::resolve_grpc_setup_dir_for_call(
        invocation_dir,
        invocation_dir,
        &request_path,
        config_dir.as_deref(),
    ) {
        Ok(v) => v,
        Err(err) => {
            let _ = writeln!(stderr, "{err}");
            return 1;
        }
    };

    let mut exit_code = 1;

    let history_enabled = history_enabled_by_command
        && !args.no_history
        && cli_util::bool_from_env(
            std::env::var("GRPC_HISTORY_ENABLED").ok(),
            "GRPC_HISTORY_ENABLED",
            true,
            Some("api-grpc"),
            stderr,
        );

    let history_file_override = std::env::var("GRPC_HISTORY_FILE")
        .ok()
        .and_then(|s| trim_non_empty(&s))
        .map(PathBuf::from);

    let setup = api_testing_core::config::ResolvedSetup::grpc(
        setup_dir.clone(),
        history_file_override.as_deref(),
    );

    let rotation = history::RotationPolicy {
        max_mb: parse_u64_default(std::env::var("GRPC_HISTORY_MAX_MB").ok(), 10, 0),
        keep: parse_u64_default(std::env::var("GRPC_HISTORY_ROTATE_COUNT").ok(), 5, 1)
            .try_into()
            .unwrap_or(u32::MAX),
    };

    let log_url = cli_util::bool_from_env(
        std::env::var("GRPC_HISTORY_LOG_URL_ENABLED").ok(),
        "GRPC_HISTORY_LOG_URL_ENABLED",
        true,
        Some("api-grpc"),
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
    };

    let endpoint = match resolve_endpoint_for_call(args, &setup) {
        Ok(v) => {
            history_ctx.endpoint_label_used = v.endpoint_label_used.clone();
            history_ctx.endpoint_value_used = v.endpoint_value_used.clone();
            v
        }
        Err(err) => {
            let _ = writeln!(stderr, "{err}");
            append_history_best_effort(&history_ctx, exit_code, stderr);
            return 1;
        }
    };

    let auth = match resolve_auth_for_call(args, &setup) {
        Ok(v) => {
            history_ctx.auth_source_used = v.auth_source_used.clone();
            history_ctx.token_name_for_log = v.token_name.clone();
            v
        }
        Err(err) => {
            let _ = writeln!(stderr, "{err}");
            append_history_best_effort(&history_ctx, exit_code, stderr);
            return 1;
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
        let _ = writeln!(stderr, "{err}");
        append_history_best_effort(&history_ctx, exit_code, stderr);
        return 1;
    }

    let spinner = Progress::spinner(
        ProgressOptions::default()
            .with_prefix("api-grpc ")
            .with_finish(ProgressFinish::Clear),
    );
    spinner.set_message("request");
    spinner.tick();

    let executed = match api_testing_core::grpc::runner::execute_grpc_request(
        &request_file,
        &endpoint.grpc_target,
        auth.bearer_token.as_deref(),
    ) {
        Ok(v) => v,
        Err(err) => {
            spinner.finish_and_clear();
            let _ = writeln!(stderr, "{err}");
            append_history_best_effort(&history_ctx, exit_code, stderr);
            return 1;
        }
    };

    let _ = stdout.write_all(&executed.response_body);

    if let Err(err) =
        api_testing_core::grpc::expect::evaluate_main_response(&request_file.request, &executed)
    {
        spinner.finish_and_clear();
        let _ = writeln!(stderr, "{err}");
        cli_io::maybe_print_failure_body_to_stderr(
            &executed.response_body,
            8192,
            stdout_is_tty,
            stderr,
        );
        append_history_best_effort(&history_ctx, exit_code, stderr);
        return 1;
    }

    spinner.finish_and_clear();
    exit_code = 0;
    append_history_best_effort(&history_ctx, exit_code, stderr);

    exit_code
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

    let record = build_request_call_history_record(RequestCallHistoryRecord {
        stamp: &stamp,
        exit_code,
        setup_dir: &ctx.setup_dir,
        invocation_dir: &ctx.invocation_dir,
        command_name: "api-grpc",
        endpoint_label_used: &ctx.endpoint_label_used,
        endpoint_value_used: &ctx.endpoint_value_used,
        log_url: ctx.log_url,
        auth,
        request_arg: &ctx.request_arg,
        extra_flags: &[],
    });

    if let Err(err) = history_writer.append(&record) {
        let _ = writeln!(stderr, "warning: failed to append api-grpc history: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use std::fs;
    use tempfile::tempdir;

    fn b64url_json(value: &serde_json::Value) -> String {
        let bytes = serde_json::to_vec(value).expect("json");
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn make_jwt(payload: serde_json::Value) -> String {
        let header = serde_json::json!({"alg":"none","typ":"JWT"});
        format!("{}.{}.sig", b64url_json(&header), b64url_json(&payload))
    }

    fn write_file(path: &Path, contents: &str) {
        fs::write(path, contents).expect("write file");
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
    fn resolve_endpoint_for_call_honors_url_and_env() {
        let tmp = tempdir().expect("tempdir");
        let setup_dir = tmp.path().join("setup/grpc");
        fs::create_dir_all(&setup_dir).expect("mkdir setup");
        write_file(
            &setup_dir.join("endpoints.env"),
            "GRPC_ENV_DEFAULT=prod\nGRPC_URL_PROD=prod:50051\nGRPC_URL_STAGING=staging:50051\n",
        );
        let setup = api_testing_core::config::ResolvedSetup::grpc(setup_dir, None);

        let args = CallArgs {
            env: None,
            url: Some("explicit:50051".to_string()),
            token: None,
            config_dir: None,
            no_history: false,
            request: "requests/health.grpc.json".to_string(),
        };
        let sel = resolve_endpoint_for_call(&args, &setup).expect("resolve explicit");
        assert_eq!(sel.grpc_target, "explicit:50051");
        assert_eq!(sel.endpoint_label_used, "url");

        let args = CallArgs {
            env: Some("staging".to_string()),
            url: None,
            token: None,
            config_dir: None,
            no_history: false,
            request: "requests/health.grpc.json".to_string(),
        };
        let sel = resolve_endpoint_for_call(&args, &setup).expect("resolve env");
        assert_eq!(sel.grpc_target, "staging:50051");
        assert_eq!(sel.endpoint_label_used, "env");

        let args = CallArgs {
            env: Some("https://example.test".to_string()),
            url: None,
            token: None,
            config_dir: None,
            no_history: false,
            request: "requests/health.grpc.json".to_string(),
        };
        let sel = resolve_endpoint_for_call(&args, &setup).expect("resolve env passthrough");
        assert_eq!(sel.grpc_target, "https://example.test");
        assert_eq!(sel.endpoint_label_used, "url");
    }

    #[test]
    fn resolve_endpoint_for_call_unknown_env_lists_available() {
        let tmp = tempdir().expect("tempdir");
        let setup_dir = tmp.path().join("setup/grpc");
        fs::create_dir_all(&setup_dir).expect("mkdir setup");
        write_file(
            &setup_dir.join("endpoints.env"),
            "GRPC_URL_PROD=prod:50051\nGRPC_URL_DEV=dev:50051\n",
        );
        let setup = api_testing_core::config::ResolvedSetup::grpc(setup_dir, None);

        let args = CallArgs {
            env: Some("missing".to_string()),
            url: None,
            token: None,
            config_dir: None,
            no_history: false,
            request: "requests/health.grpc.json".to_string(),
        };

        let err = resolve_endpoint_for_call(&args, &setup).expect_err("unknown env must fail");
        assert!(err.to_string().contains("Unknown --env 'missing'"));
        assert!(err.to_string().contains("prod"));
    }

    #[test]
    fn resolve_auth_for_call_prefers_profile_then_env_fallback() {
        let lock = GlobalStateLock::new();
        let _access = EnvGuard::set(&lock, "ACCESS_TOKEN", "env-token");
        let _name = EnvGuard::remove(&lock, "GRPC_TOKEN_NAME");

        let tmp = tempdir().expect("tempdir");
        let setup_dir = tmp.path().join("setup/grpc");
        fs::create_dir_all(&setup_dir).expect("mkdir setup");
        write_file(&setup_dir.join("tokens.env"), "GRPC_TOKEN_SVC=svc-token\n");
        let setup = api_testing_core::config::ResolvedSetup::grpc(setup_dir, None);

        let args = CallArgs {
            env: None,
            url: None,
            token: Some("svc".to_string()),
            config_dir: None,
            no_history: false,
            request: "requests/health.grpc.json".to_string(),
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
            request: "requests/health.grpc.json".to_string(),
        };
        let auth = resolve_auth_for_call(&args, &setup).expect("env fallback resolution");
        assert_eq!(auth.bearer_token.as_deref(), Some("env-token"));
        assert!(matches!(
            auth.auth_source_used,
            AuthSourceUsed::EnvFallback { .. }
        ));
    }

    #[test]
    fn validate_bearer_token_warns_when_non_strict() {
        let lock = GlobalStateLock::new();
        let _enabled = EnvGuard::set(&lock, "GRPC_JWT_VALIDATE_ENABLED", "true");
        let _strict = EnvGuard::set(&lock, "GRPC_JWT_VALIDATE_STRICT", "false");

        let mut stderr = Vec::new();
        let res = validate_bearer_token_if_jwt(
            "not.a.jwt",
            &AuthSourceUsed::None,
            "default",
            &mut stderr,
        );
        assert!(res.is_ok());
        let msg = String::from_utf8_lossy(&stderr);
        assert!(msg.contains("not a valid JWT"));
    }

    #[test]
    fn validate_bearer_token_errors_when_strict_invalid() {
        let lock = GlobalStateLock::new();
        let _enabled = EnvGuard::set(&lock, "GRPC_JWT_VALIDATE_ENABLED", "true");
        let _strict = EnvGuard::set(&lock, "GRPC_JWT_VALIDATE_STRICT", "true");

        let mut stderr = Vec::new();
        let err = validate_bearer_token_if_jwt(
            "not.a.jwt",
            &AuthSourceUsed::None,
            "default",
            &mut stderr,
        )
        .expect_err("strict mode must fail invalid token");
        assert!(err.to_string().contains("invalid JWT"));
    }

    #[test]
    fn validate_bearer_token_errors_when_expired() {
        let lock = GlobalStateLock::new();
        let _enabled = EnvGuard::set(&lock, "GRPC_JWT_VALIDATE_ENABLED", "true");
        let _strict = EnvGuard::set(&lock, "GRPC_JWT_VALIDATE_STRICT", "false");

        let token = make_jwt(serde_json::json!({ "exp": 1 }));
        let mut stderr = Vec::new();
        let err =
            validate_bearer_token_if_jwt(&token, &AuthSourceUsed::TokenProfile, "svc", &mut stderr)
                .expect_err("expired token must fail");
        assert!(err.to_string().contains("JWT expired"));
    }

    #[test]
    fn append_history_writes_env_and_token_command() {
        let tmp = tempdir().expect("tempdir");
        let setup_dir = tmp.path().join("setup/grpc");
        fs::create_dir_all(&setup_dir).expect("mkdir setup");
        let history_file = tmp.path().join(".grpc_history");

        let ctx = CallHistoryContext {
            enabled: true,
            setup_dir: setup_dir.clone(),
            history_writer: test_history_writer(&history_file),
            invocation_dir: tmp.path().to_path_buf(),
            request_arg: "requests/health.grpc.json".to_string(),
            endpoint_label_used: "env".to_string(),
            endpoint_value_used: "local".to_string(),
            log_url: true,
            auth_source_used: AuthSourceUsed::TokenProfile,
            token_name_for_log: "default".to_string(),
        };

        append_history_best_effort(&ctx, 0, &mut std::io::sink());

        let text = fs::read_to_string(&history_file).expect("history text");
        assert!(text.contains("api-grpc call \\\n"));
        assert!(text.contains("--config-dir"));
        assert!(text.contains("--env "));
        assert!(text.contains("local"));
        assert!(text.contains("--token "));
        assert!(text.contains("default"));
        assert!(text.contains("requests/health.grpc.json"));
        assert!(text.contains("exit=0"));
    }

    #[test]
    fn append_history_omits_url_value_when_log_url_disabled() {
        let tmp = tempdir().expect("tempdir");
        let setup_dir = tmp.path().join("setup/grpc");
        fs::create_dir_all(&setup_dir).expect("mkdir setup");
        let history_file = tmp.path().join(".grpc_history");

        let ctx = CallHistoryContext {
            enabled: true,
            setup_dir: setup_dir.clone(),
            history_writer: test_history_writer(&history_file),
            invocation_dir: tmp.path().to_path_buf(),
            request_arg: "/abs/requests/health.grpc.json".to_string(),
            endpoint_label_used: "url".to_string(),
            endpoint_value_used: "127.0.0.1:50051".to_string(),
            log_url: false,
            auth_source_used: AuthSourceUsed::EnvFallback {
                env_name: "ACCESS_TOKEN".to_string(),
            },
            token_name_for_log: String::new(),
        };

        append_history_best_effort(&ctx, 7, &mut std::io::sink());

        let text = fs::read_to_string(&history_file).expect("history text");
        assert!(text.contains("url=<omitted>"));
        assert!(!text.contains("--url "));
        assert!(text.contains("auth=ACCESS_TOKEN"));
        assert!(text.contains("exit=7"));
    }

    #[test]
    fn append_history_disabled_does_not_create_history_file() {
        let tmp = tempdir().expect("tempdir");
        let setup_dir = tmp.path().join("setup/grpc");
        fs::create_dir_all(&setup_dir).expect("mkdir setup");
        let history_file = tmp.path().join(".grpc_history");

        let ctx = CallHistoryContext {
            enabled: false,
            setup_dir,
            history_writer: test_history_writer(&history_file),
            invocation_dir: tmp.path().to_path_buf(),
            request_arg: "req.grpc.json".to_string(),
            endpoint_label_used: String::new(),
            endpoint_value_used: String::new(),
            log_url: true,
            auth_source_used: AuthSourceUsed::None,
            token_name_for_log: String::new(),
        };

        append_history_best_effort(&ctx, 0, &mut std::io::sink());
        assert!(!history_file.exists());
    }

    #[test]
    fn maybe_print_failure_body_skips_when_stdout_is_tty() {
        let mut stderr = Vec::new();
        api_testing_core::cli_io::maybe_print_failure_body_to_stderr(
            b"not-json",
            16,
            true,
            &mut stderr,
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn maybe_print_failure_body_skips_when_response_is_json() {
        let mut stderr = Vec::new();
        api_testing_core::cli_io::maybe_print_failure_body_to_stderr(
            br#"{"ok":true}"#,
            16,
            false,
            &mut stderr,
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn maybe_print_failure_body_prints_non_json_preview() {
        let mut stderr = Vec::new();
        api_testing_core::cli_io::maybe_print_failure_body_to_stderr(
            b"abcdef",
            4,
            false,
            &mut stderr,
        );
        let text = String::from_utf8(stderr).expect("utf8");
        assert!(text.contains("Response body (non-JSON; first 4 bytes):"));
        assert!(text.contains("abcd"));
    }
}

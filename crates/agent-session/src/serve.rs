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

use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::net::SocketAddr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use futures_util::{SinkExt, StreamExt};
use nils_common::cli_contract::{exit, schema_version_for};
use nils_common::usage_time::{
    epoch_seconds_from_f64, normalize_epoch_seconds, reset_epoch_seconds_from_str,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::cli::{self, AgentKind, SpecialKey};
use crate::provider_prompt::{
    MAX_PROVIDER_PROMPT_BYTES, PROVIDER_PROMPT_CAPABILITY, ProviderKind, ProviderPromptEvent,
    ProviderPromptTail,
};
use crate::{
    BINARY, CliContext, CliError, ProviderResumeImportArgs, WorkdirSearchOptions, delete_session,
    glance_session, list_sessions, load_session_record, non_empty_env, repo_remote_url_from_cwd,
    resolve_tmux_bin, resume_session_by_id, search_workdirs, send_input, session_clipboard_buffer,
    session_dir, session_status, short_hostname, start_provider_resume_session, start_session,
    update_session_title, write_session_attachment,
};

const ATTACH_LIVE_FIFO_NAME: &str = "attach-live.fifo";
const ATTACH_BROADCAST_CAPACITY: usize = 128;
const ATTACH_PIPE_STARTUP_GRACE: Duration = Duration::from_secs(2);
const ATTACH_FIFO_POLL_INTERVAL: Duration = Duration::from_millis(100);
const ATTACH_READER_STOP_TIMEOUT: Duration = Duration::from_millis(250);
const ATTACH_TMUX_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const ATTACH_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(2);
const ATTACH_RESIZE_TIMEOUT: Duration = Duration::from_secs(5);
const ATTACH_WEBSOCKET_SEND_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
const MAX_STDIN_TOKEN_BYTES: u64 = 8 * 1024;
const USAGE_SCHEMA_VERSION: &str = "agent-session.usage.v1";
const DEFAULT_USAGE_TIMEOUT_MS: u64 = 45_000;
const CLAUDE_USAGE_CLEANUP_SLACK_SECONDS: u64 = 5;
const ATTACH_TERMINAL_QUEUE_CAPACITY: usize = 64;
const ATTACH_CONTROL_QUEUE_CAPACITY: usize = 8;
const ATTACH_TERMINAL_BURST: usize = 32;
const ATTACH_HANDOFF_BUFFER_CAPACITY: usize = ATTACH_BROADCAST_CAPACITY;
const ATTACH_HANDOFF_MAX_RECAPTURES: usize = 1;
const PROVIDER_PROMPT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const PROVIDER_PROMPT_PENDING_POLL_INTERVAL: Duration = Duration::from_millis(500);
const PROVIDER_PROMPT_PENDING_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const ATTACH_SCHEMA_VERSION: &str = "agent-session.attach.v1";
const ATTACH_EVENT_SCHEMA_VERSION: &str = "agent-session.attach.event.v1";
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
    attach_brokers: AttachBrokerRegistry,
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
        attach_brokers: AttachBrokerRegistry::default(),
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
        let app = router(state.clone());
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await;
        state.attach_brokers.shutdown_all().await;
        match result {
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
        &[],
        timeout,
    ) {
        Ok(output) => normalize_codex_usage(output),
        Err(message) => provider_error("codex", "Codex", "helper-spawn-failed", message),
    }
}

fn collect_claude_usage(timeout: Duration) -> UsageProvider {
    let explicit_timeout = non_empty_env("CLAUDE_PROMPT_SEGMENT_CLAUDE_TIMEOUT_SECONDS");
    let timeout_seconds =
        claude_inner_timeout_seconds(timeout, explicit_timeout.as_deref()).to_string();
    match run_usage_helper(
        "claude-cli",
        &["usage", "--format", "json", "--source", "auto"],
        &[(
            "CLAUDE_PROMPT_SEGMENT_CLAUDE_TIMEOUT_SECONDS",
            timeout_seconds.as_str(),
        )],
        timeout,
    ) {
        Ok(output) => normalize_claude_usage(output),
        Err(message) => provider_error("claude", "Claude", "helper-spawn-failed", message),
    }
}

fn claude_inner_timeout_seconds(timeout: Duration, explicit: Option<&str>) -> u64 {
    let max_inner_seconds = timeout
        .checked_sub(Duration::from_secs(CLAUDE_USAGE_CLEANUP_SLACK_SECONDS))
        .map(|remaining| remaining.as_secs())
        .unwrap_or(0)
        .max(1);
    explicit
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(max_inner_seconds)
        .min(max_inner_seconds)
}

fn run_usage_helper(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    timeout: Duration,
) -> Result<UsageHelperOutput, String> {
    let mut child = ProcessCommand::new(program)
        .args(args)
        .envs(envs.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
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
                    unsafe {
                        let pgid = -(child.id() as libc::pid_t);
                        let _ = libc::kill(pgid, libc::SIGKILL);
                    }
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
        windows.extend(windows_from_value(result, None));
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
    let reference_epoch = i64_field(result, &["updated_at", "updatedAt"]);
    let windows = windows_from_value(result, reference_epoch);
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

fn windows_from_value(value: &Value, reference_epoch: Option<i64>) -> Vec<UsageWindow> {
    value
        .get("windows")
        .and_then(Value::as_array)
        .map(|windows| {
            windows
                .iter()
                .filter_map(|window| usage_window_from_value(window, reference_epoch))
                .collect()
        })
        .unwrap_or_default()
}

fn usage_window_from_value(value: &Value, reference_epoch: Option<i64>) -> Option<UsageWindow> {
    let label = value.get("label").and_then(Value::as_str)?.to_string();
    let used_percent = i64_field(value, &["used_percent"])?;
    let remaining_percent = i64_field(value, &["remaining_percent"])
        .unwrap_or_else(|| (100 - used_percent).clamp(0, 100));
    let reset_at = reset_at_text_field(value, RESET_AT_KEYS);
    let reset_at_epoch = epoch_field(value, RESET_AT_EPOCH_KEYS, reference_epoch);
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

fn epoch_field(value: &Value, keys: &[&str], reference_epoch: Option<i64>) -> Option<i64> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(|value| epoch_seconds_from_value(value, reference_epoch))
    })
}

fn epoch_seconds_from_value(value: &Value, reference_epoch: Option<i64>) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().and_then(epoch_seconds_from_f64))
            .map(normalize_epoch_seconds),
        Value::String(raw) => reset_epoch_seconds_from_str(raw, reference_epoch),
        _ => None,
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
    let delete_id = id.clone();
    match tokio::task::spawn_blocking(move || delete_session(&context, &delete_id, tmux)).await {
        Ok(Ok(result)) => {
            state.attach_brokers.shutdown_session(&id).await;
            envelope_ok(json!({ "machine": state.machine, "deleted": result }))
        }
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
    ws.on_upgrade(move |socket| attach_socket(socket, state, record))
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

#[derive(Clone, Debug)]
enum AttachEvent {
    Output(Bytes),
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachWriterExit {
    Drained,
    SendFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachPumpExit {
    BrokerClosed,
    Lagged,
    WriterClosed,
}

struct AttachHandoff {
    snapshot: Option<String>,
    live: VecDeque<Bytes>,
    broker_closed: bool,
}

struct AbortOnDropTask<T> {
    handle: Option<tokio::task::JoinHandle<T>>,
}

impl<T> AbortOnDropTask<T> {
    fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    fn handle_mut(&mut self) -> &mut tokio::task::JoinHandle<T> {
        self.handle.as_mut().expect("task handle is present")
    }

    fn abort(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }

    async fn abort_and_wait(mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        handle.abort();
        let _ = handle.await;
    }
}

impl<T> Drop for AbortOnDropTask<T> {
    fn drop(&mut self) {
        self.abort();
    }
}

struct AttachSubscription {
    receiver: Option<tokio::sync::broadcast::Receiver<AttachEvent>>,
    lease: Option<AttachLease>,
    resize_lock: Arc<tokio::sync::Mutex<()>>,
}

struct AttachLease {
    slot: Arc<tokio::sync::Mutex<AttachBrokerSlot>>,
    generation: u64,
    released: bool,
}

#[derive(Default)]
struct AttachBrokerRegistry {
    entries: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<AttachBrokerSlot>>>>,
}

struct AttachBrokerSlot {
    accepting: bool,
    next_generation: u64,
    active: Option<AttachBrokerGeneration>,
}

struct AttachBrokerGeneration {
    generation: u64,
    subscribers: usize,
    broker: AttachBroker,
}

struct AttachBroker {
    target: String,
    fifo_path: PathBuf,
    tmux: PathBuf,
    sender: tokio::sync::broadcast::Sender<AttachEvent>,
    reader_task: tokio::task::JoinHandle<()>,
    closed: Arc<AtomicBool>,
    resize_lock: Arc<tokio::sync::Mutex<()>>,
}

struct AttachFifoCleanup {
    path: PathBuf,
    armed: bool,
}

impl AttachFifoCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for AttachFifoCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Default for AttachBrokerSlot {
    fn default() -> Self {
        Self {
            accepting: true,
            next_generation: 1,
            active: None,
        }
    }
}

impl AttachSubscription {
    fn receiver_mut(&mut self) -> &mut tokio::sync::broadcast::Receiver<AttachEvent> {
        self.receiver.as_mut().expect("attach receiver is present")
    }

    fn take_receiver(&mut self) -> tokio::sync::broadcast::Receiver<AttachEvent> {
        self.receiver.take().expect("attach receiver is present")
    }

    async fn release(mut self) {
        if let Some(lease) = self.lease.take() {
            lease.release().await;
        }
    }
}

impl Drop for AttachSubscription {
    fn drop(&mut self) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        drop(lease);
    }
}

impl AttachLease {
    async fn release(mut self) {
        let cleanup = self.schedule_release();
        self.released = true;
        if let Some(cleanup) = cleanup {
            let _ = cleanup.await;
        }
    }

    fn schedule_release(&self) -> Option<tokio::task::JoinHandle<()>> {
        let handle = tokio::runtime::Handle::try_current().ok()?;
        let slot = self.slot.clone();
        let generation = self.generation;
        Some(handle.spawn(async move {
            let mut slot = slot.lock().await;
            let Some(active) = slot.active.as_mut() else {
                return;
            };
            if active.generation != generation {
                return;
            }
            if active.subscribers > 1 {
                active.subscribers -= 1;
                return;
            }
            if let Some(active) = slot.active.take() {
                // This supervisor task owns teardown independently of the
                // caller, so cancellation cannot strand the pipe or FIFO.
                // Keep the per-session lock so an old generation cannot close
                // a replacement pane pipe for the same session.
                active.broker.stop().await;
            }
        }))
    }
}

impl Drop for AttachLease {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let _ = self.schedule_release();
    }
}

impl AttachBrokerRegistry {
    async fn subscribe(
        &self,
        context: &CliContext,
        tmux: &Path,
        record: &crate::SessionRecord,
    ) -> io::Result<AttachSubscription> {
        let slot = {
            let mut entries = self.entries.lock().await;
            entries
                .entry(record.id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(AttachBrokerSlot::default())))
                .clone()
        };
        let mut slot_state = slot.lock().await;
        if !slot_state.accepting {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "attach broker is shutting down",
            ));
        }

        let target = format!("{}:0.0", record.tmux_session);
        if let Some(active) = slot_state.active.as_mut()
            && active.broker.target == target
        {
            // Subscribe before checking `closed`. If the reader exits between
            // these operations, this receiver observes its terminal event.
            let receiver = active.broker.sender.subscribe();
            if !active.broker.closed.load(Ordering::Acquire) {
                active.subscribers += 1;
                return Ok(AttachSubscription {
                    receiver: Some(receiver),
                    lease: Some(AttachLease {
                        slot: slot.clone(),
                        generation: active.generation,
                        released: false,
                    }),
                    resize_lock: active.broker.resize_lock.clone(),
                });
            }
        }

        if let Some(active) = slot_state.active.take() {
            active.broker.stop().await;
        }
        let generation = slot_state.next_generation;
        slot_state.next_generation = slot_state.next_generation.wrapping_add(1).max(1);
        let (broker, receiver) = AttachBroker::start(context, tmux, record).await?;
        let resize_lock = broker.resize_lock.clone();
        slot_state.active = Some(AttachBrokerGeneration {
            generation,
            subscribers: 1,
            broker,
        });
        Ok(AttachSubscription {
            receiver: Some(receiver),
            lease: Some(AttachLease {
                slot: slot.clone(),
                generation,
                released: false,
            }),
            resize_lock,
        })
    }

    async fn shutdown_all(&self) {
        let mut entries = self.entries.lock().await;
        let slots: Vec<_> = entries.drain().map(|(_, slot)| slot).collect();
        drop(entries);
        for slot in slots {
            let mut slot = slot.lock().await;
            slot.accepting = false;
            if let Some(active) = slot.active.take() {
                active.broker.stop().await;
            }
        }
    }

    async fn shutdown_session(&self, session_id: &str) {
        let slot = self.entries.lock().await.remove(session_id);
        if let Some(slot) = slot {
            let mut slot = slot.lock().await;
            slot.accepting = false;
            if let Some(active) = slot.active.take() {
                active.broker.stop().await;
            }
        }
    }

    #[cfg(test)]
    async fn subscriber_count(&self, session_id: &str) -> usize {
        let slot = self.entries.lock().await.get(session_id).cloned();
        let Some(slot) = slot else { return 0 };
        let slot = slot.lock().await;
        slot.active.as_ref().map_or(0, |active| active.subscribers)
    }
}

impl AttachBroker {
    async fn start(
        context: &CliContext,
        tmux: &Path,
        record: &crate::SessionRecord,
    ) -> io::Result<(Self, tokio::sync::broadcast::Receiver<AttachEvent>)> {
        Self::start_with_fifo_opener(context, tmux, record, |fifo_path| {
            let fifo = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(fifo_path)?;
            tokio::io::unix::AsyncFd::new(fifo)
        })
        .await
    }

    async fn start_with_fifo_opener<F>(
        context: &CliContext,
        tmux: &Path,
        record: &crate::SessionRecord,
        open_fifo: F,
    ) -> io::Result<(Self, tokio::sync::broadcast::Receiver<AttachEvent>)>
    where
        F: FnOnce(&Path) -> io::Result<tokio::io::unix::AsyncFd<std::fs::File>>,
    {
        let target = format!("{}:0.0", record.tmux_session);
        let fifo_path = session_dir(context, &record.id).join(ATTACH_LIVE_FIFO_NAME);
        create_private_fifo(&fifo_path)?;
        let mut fifo_cleanup = AttachFifoCleanup::new(fifo_path.clone());
        let fifo = open_fifo(&fifo_path)?;

        let (sender, receiver) = tokio::sync::broadcast::channel(ATTACH_BROADCAST_CAPACITY);
        enable_tmux_pipe(tmux, &target, &fifo_path).await?;

        let reader_sender = sender.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let reader_closed = closed.clone();
        let reader_task = tokio::spawn(async move {
            read_attach_fifo(fifo, reader_sender, reader_closed).await;
        });
        let resize_lock = Arc::new(tokio::sync::Mutex::new(()));
        fifo_cleanup.disarm();
        Ok((
            Self {
                target,
                fifo_path,
                tmux: tmux.to_path_buf(),
                sender,
                reader_task,
                closed,
                resize_lock,
            },
            receiver,
        ))
    }

    async fn stop(self) {
        let _ = close_tmux_pipe(&self.tmux, &self.target).await;
        let mut reader_task = self.reader_task;
        if tokio::time::timeout(ATTACH_READER_STOP_TIMEOUT, &mut reader_task)
            .await
            .is_err()
        {
            reader_task.abort();
            let _ = reader_task.await;
        }
        let _ = std::fs::remove_file(&self.fifo_path);
    }
}

fn create_private_fifo(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => std::fs::remove_file(path)?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "attach FIFO path contains a NUL byte",
        )
    })?;
    // SAFETY: `path` is a valid, NUL-terminated C string and mode 0600 keeps
    // terminal bytes private to the daemon user.
    if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

async fn enable_tmux_pipe(tmux: &Path, target: &str, fifo_path: &Path) -> io::Result<()> {
    let tmux = tmux.to_path_buf();
    let target = target.to_string();
    let fifo_path = fifo_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut command = ProcessCommand::new(&tmux);
        command.arg("pipe-pane").arg("-t").arg(&target).arg(format!(
            "cat > {}",
            shell_words::quote(&fifo_path.to_string_lossy())
        ));
        let status = run_process_with_timeout(&mut command, ATTACH_TMUX_COMMAND_TIMEOUT)?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "tmux pipe-pane failed with status {status}"
            )))
        }
    })
    .await
    .map_err(|err| io::Error::other(format!("tmux pipe task failed: {err}")))?
}

async fn close_tmux_pipe(tmux: &Path, target: &str) -> io::Result<()> {
    let tmux = tmux.to_path_buf();
    let target = target.to_string();
    tokio::task::spawn_blocking(move || {
        let mut command = ProcessCommand::new(&tmux);
        command.arg("pipe-pane").arg("-t").arg(&target);
        run_process_with_timeout(&mut command, ATTACH_TMUX_COMMAND_TIMEOUT).map(|_| ())
    })
    .await
    .map_err(|err| io::Error::other(format!("tmux close task failed: {err}")))?
}

fn run_process_with_timeout(
    command: &mut ProcessCommand,
    timeout: Duration,
) -> io::Result<std::process::ExitStatus> {
    let mut child = command.spawn()?;
    wait_process_with_timeout(&mut child, timeout)
}

fn run_process_output_with_timeout(
    command: &mut ProcessCommand,
    timeout: Duration,
) -> io::Result<std::process::Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = command.spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("child stdout pipe is unavailable"))?;
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let status = wait_process_with_timeout(&mut child, timeout);
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("child stdout reader panicked"))??;
    Ok(std::process::Output {
        status: status?,
        stdout,
        stderr: Vec::new(),
    })
}

fn wait_process_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> io::Result<std::process::ExitStatus> {
    let started_at = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started_at.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("process exceeded {} ms", timeout.as_millis()),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

async fn capture_attach_snapshot(
    tmux: &Path,
    record: &crate::SessionRecord,
) -> io::Result<Option<String>> {
    capture_attach_snapshot_with_timeout(tmux, record, ATTACH_SNAPSHOT_TIMEOUT).await
}

async fn capture_attach_snapshot_with_timeout(
    tmux: &Path,
    record: &crate::SessionRecord,
    timeout: Duration,
) -> io::Result<Option<String>> {
    let tmux = tmux.to_path_buf();
    let tmux_session = record.tmux_session.clone();
    tokio::task::spawn_blocking(move || {
        let mut command = ProcessCommand::new(&tmux);
        command
            .arg("capture-pane")
            .arg("-p")
            .arg("-t")
            .arg(&tmux_session)
            .arg("-S")
            .arg("-200");
        let output = run_process_output_with_timeout(&mut command, timeout)?;
        if output.status.success() {
            Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
        } else {
            Ok(None)
        }
    })
    .await
    .map_err(|err| io::Error::other(format!("tmux capture task failed: {err}")))?
}

async fn capture_attach_handoff(
    tmux: &Path,
    record: &crate::SessionRecord,
    receiver: &mut tokio::sync::broadcast::Receiver<AttachEvent>,
) -> io::Result<AttachHandoff> {
    for recaptures in 0..=ATTACH_HANDOFF_MAX_RECAPTURES {
        let capture = capture_attach_snapshot(tmux, record);
        tokio::pin!(capture);
        let mut live = VecDeque::with_capacity(ATTACH_HANDOFF_BUFFER_CAPACITY);
        let mut overflowed = false;
        let mut broker_closed = false;

        let snapshot = loop {
            tokio::select! {
                snapshot = &mut capture => break snapshot?,
                event = receiver.recv(), if !broker_closed => {
                    match event {
                        Ok(AttachEvent::Output(bytes)) => {
                            if !overflowed && live.len() < ATTACH_HANDOFF_BUFFER_CAPACITY {
                                live.push_back(bytes);
                            } else {
                                // Continue draining so output generated by the
                                // daemon's own snapshot handoff cannot make this
                                // client appear slow. A fresh snapshot below
                                // becomes the new terminal-state baseline.
                                overflowed = true;
                                live.clear();
                            }
                        }
                        Ok(AttachEvent::Closed)
                        | Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            broker_closed = true;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            overflowed = true;
                            live.clear();
                        }
                    }
                }
            }
        };

        if !overflowed {
            return Ok(AttachHandoff {
                snapshot,
                live,
                broker_closed,
            });
        }
        if recaptures == ATTACH_HANDOFF_MAX_RECAPTURES {
            return Err(io::Error::other(
                "attach snapshot handoff overflowed after bounded recapture",
            ));
        }
    }
    unreachable!("bounded snapshot handoff loop returns")
}

async fn pump_attach_events(
    mut receiver: tokio::sync::broadcast::Receiver<AttachEvent>,
    terminal_tx: mpsc::Sender<Message>,
) -> AttachPumpExit {
    loop {
        match receiver.recv().await {
            Ok(AttachEvent::Output(bytes)) => {
                if terminal_tx.send(Message::Binary(bytes)).await.is_err() {
                    return AttachPumpExit::WriterClosed;
                }
            }
            Ok(AttachEvent::Closed) | Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                return AttachPumpExit::BrokerClosed;
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                return AttachPumpExit::Lagged;
            }
        }
    }
}

async fn read_attach_fifo(
    fifo: tokio::io::unix::AsyncFd<std::fs::File>,
    sender: tokio::sync::broadcast::Sender<AttachEvent>,
    closed: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; 8192];
    let started_at = Instant::now();
    let mut writer_seen = false;
    loop {
        let read = match tokio::time::timeout(ATTACH_FIFO_POLL_INTERVAL, fifo.readable()).await {
            Ok(Ok(mut ready)) => match ready.try_io(|inner| {
                let mut file = inner.get_ref();
                file.read(&mut buf)
            }) {
                Ok(read) => read,
                Err(_would_block) => continue,
            },
            Ok(Err(err)) => Err(err),
            Err(_) => {
                // Some kqueue implementations do not emit another readable
                // notification when a FIFO's final writer disappears. The
                // descriptor is nonblocking, so a low-frequency direct read
                // distinguishes an idle writer (WouldBlock) from EOF without
                // tying up the async runtime.
                let mut file = fifo.get_ref();
                file.read(&mut buf)
            }
        };
        match read {
            Ok(0) if !writer_seen && started_at.elapsed() < ATTACH_PIPE_STARTUP_GRACE => {
                // `pipe-pane` starts its shell asynchronously. A nonblocking
                // FIFO reader briefly sees EOF until that writer opens.
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Ok(0) => break,
            Ok(n) => {
                writer_seen = true;
                let _ = sender.send(AttachEvent::Output(Bytes::copy_from_slice(&buf[..n])));
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
    }
    closed.store(true, Ordering::Release);
    let _ = sender.send(AttachEvent::Closed);
}

#[cfg(test)]
fn attach_event_bytes(
    event: Result<AttachEvent, tokio::sync::broadcast::error::RecvError>,
) -> Option<Bytes> {
    match event {
        Ok(AttachEvent::Output(bytes)) => Some(bytes),
        Ok(AttachEvent::Closed)
        | Err(tokio::sync::broadcast::error::RecvError::Closed)
        | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
    }
}

async fn send_attach_message<S>(sender: &mut S, message: Message, timeout: Duration) -> bool
where
    S: futures_util::Sink<Message> + Unpin,
{
    matches!(
        tokio::time::timeout(timeout, sender.send(message)).await,
        Ok(Ok(()))
    )
}

async fn attach_socket(
    socket: WebSocket,
    state: Arc<ServeState>,
    mut record: crate::SessionRecord,
) {
    let target = format!("{}:0.0", record.tmux_session);
    let (sender, mut receiver) = socket.split();
    let (terminal_tx, terminal_rx) = mpsc::channel(ATTACH_TERMINAL_QUEUE_CAPACITY);
    let (control_tx, control_rx) = mpsc::channel(ATTACH_CONTROL_QUEUE_CAPACITY);
    let mut writer_task = Some(AbortOnDropTask::new(tokio::spawn(outbound_writer(
        sender,
        terminal_rx,
        control_rx,
        ATTACH_WEBSOCKET_SEND_TIMEOUT,
    ))));

    // Subscribe before capturing the initial screen so pane output produced
    // during capture is buffered instead of silently lost. Snapshot and live
    // bytes may overlap; preserving every byte is preferable to a gap.
    let mut subscription = match state
        .attach_brokers
        .subscribe(&state.context, &state.tmux_bin, &record)
        .await
    {
        Ok(subscription) => subscription,
        Err(err) => {
            eprintln!(
                "warning: failed to start live attach broker for {}: {err}",
                record.id
            );
            return;
        }
    };
    let handoff =
        match capture_attach_handoff(&state.tmux_bin, &record, subscription.receiver_mut()).await {
            Ok(handoff) => handoff,
            Err(err) => {
                eprintln!(
                    "warning: failed to establish attach snapshot handoff for {}: {err}",
                    record.id
                );
                subscription.release().await;
                if let Some(task) = writer_task.take() {
                    task.abort_and_wait().await;
                }
                return;
            }
        };
    if let Some(text) = handoff.snapshot
        && terminal_tx
            .send(Message::Binary(text.into_bytes().into()))
            .await
            .is_err()
    {
        subscription.release().await;
        if let Some(task) = writer_task.take() {
            task.abort_and_wait().await;
        }
        return;
    }
    for bytes in handoff.live {
        if terminal_tx.send(Message::Binary(bytes)).await.is_err() {
            subscription.release().await;
            if let Some(task) = writer_task.take() {
                task.abort_and_wait().await;
            }
            return;
        }
    }

    let mut pump_task = (!handoff.broker_closed).then(|| {
        AbortOnDropTask::new(tokio::spawn(pump_attach_events(
            subscription.take_receiver(),
            terminal_tx.clone(),
        )))
    });

    // Client -> pane: JSON control frames { text?, key?, keys?, resize{cols,rows} }.
    // The first resize after attach forces a full-frame repaint (see resize_pane).
    let mut initial_repaint_pending = true;
    let resize_lock = subscription.resize_lock.clone();
    let mut provider_prompt_task: Option<AbortOnDropTask<()>> = None;
    let mut provider_open_task: Option<
        AbortOnDropTask<(crate::SessionRecord, Option<ProviderPromptTail>)>,
    > = None;
    let mut provider_refresh_pending = false;
    let mut provider_pending_deadline: Option<Instant> = None;
    let mut drain_writer = handoff.broker_closed;
    while pump_task.is_some() {
        tokio::select! {
            writer_result = async {
                writer_task
                    .as_mut()
                    .expect("writer task select guard")
                    .handle_mut()
                    .await
            }, if writer_task.is_some() => {
                writer_task.take();
                if matches!(writer_result, Ok(AttachWriterExit::SendFailed) | Err(_)) {
                    eprintln!(
                        "warning: outbound attach writer stopped for {}",
                        record.id
                    );
                }
                break;
            }
            pump_result = async {
                pump_task
                    .as_mut()
                    .expect("attach pump select guard")
                    .handle_mut()
                    .await
            }, if pump_task.is_some() => {
                pump_task.take();
                drain_writer = matches!(pump_result, Ok(AttachPumpExit::BrokerClosed));
                break;
            }
            opened = async {
                provider_open_task
                    .as_mut()
                    .expect("provider open task select guard")
                    .handle_mut()
                    .await
            }, if provider_open_task.is_some() => {
                provider_open_task.take();
                let prompt_tail = match opened {
                    Ok((updated_record, tail)) => {
                        record = updated_record;
                        tail
                    }
                    Err(_) => None,
                };
                let capability = ProviderPromptCapabilityState {
                    supported: prompt_tail.is_some(),
                    provider: prompt_tail.as_ref().map(ProviderPromptTail::provider),
                };
                if control_tx
                    .send(Message::Text(
                        provider_prompt_capability_frame(capability).into(),
                    ))
                    .await
                    .is_err()
                {
                    break;
                }

                if provider_refresh_pending && prompt_tail.is_none() {
                    provider_refresh_pending = false;
                    provider_open_task = Some(open_provider_prompt_task(
                        state.context.clone(),
                        record.clone(),
                        *provider_pending_deadline
                            .get_or_insert_with(|| Instant::now() + PROVIDER_PROMPT_PENDING_TIMEOUT),
                    ));
                } else if let Some(tail) = prompt_tail {
                    provider_refresh_pending = false;
                    provider_prompt_task = Some(AbortOnDropTask::new(tokio::spawn(
                        provider_prompt_loop(tail, control_tx.clone()),
                    )));
                }
            }
            message = receiver.next() => {
                let Some(Ok(message)) = message else { break; };
                match message {
                    Message::Text(text) => {
                        if provider_prompt_subscription_requested(text.as_str()) {
                            if let Some(mut task) = provider_prompt_task.take() {
                                task.abort();
                            }
                            if provider_open_task.is_some() {
                                // Preserve the old serial refresh behavior without
                                // pausing broker consumption or launching overlapping
                                // transcript scans for repeated subscriptions.
                                provider_refresh_pending = true;
                            } else {
                                provider_open_task = Some(open_provider_prompt_task(
                                    state.context.clone(),
                                    record.clone(),
                                    *provider_pending_deadline.get_or_insert_with(|| {
                                        Instant::now() + PROVIDER_PROMPT_PENDING_TIMEOUT
                                    }),
                                ));
                            }
                            continue;
                        }
                        handle_input(
                            &state.context,
                            &state.tmux_bin,
                            &record,
                            &target,
                            text.as_str(),
                            &mut initial_repaint_pending,
                            &resize_lock,
                        )
                        .await;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }

    if let Some(task) = pump_task {
        task.abort_and_wait().await;
    }
    if let Some(task) = provider_prompt_task {
        task.abort_and_wait().await;
    }
    if let Some(task) = provider_open_task {
        task.abort_and_wait().await;
    }
    subscription.release().await;
    drop(terminal_tx);
    drop(control_tx);
    if let Some(task) = writer_task {
        finish_outbound_writer(task, drain_writer).await;
    }
}

fn open_provider_prompt_task(
    context: CliContext,
    record: crate::SessionRecord,
    pending_deadline: Instant,
) -> AbortOnDropTask<(crate::SessionRecord, Option<ProviderPromptTail>)> {
    AbortOnDropTask::new(tokio::spawn(resolve_provider_prompt_tail(
        context,
        record,
        pending_deadline,
    )))
}

async fn resolve_provider_prompt_tail(
    context: CliContext,
    record: crate::SessionRecord,
    pending_deadline: Instant,
) -> (crate::SessionRecord, Option<ProviderPromptTail>) {
    let pending_fresh_runtime = provider_prompt_pending_fresh_runtime(&record);
    let initial = record.clone();
    let opened = tokio::task::spawn_blocking(move || ProviderPromptTail::open(&initial))
        .await
        .ok()
        .flatten();
    if opened.is_some() || !pending_fresh_runtime {
        return (record, opened);
    }

    let mut current = record;
    while Instant::now() < pending_deadline {
        tokio::time::sleep(PROVIDER_PROMPT_PENDING_POLL_INTERVAL).await;
        let load_context = context.clone();
        let id = current.id.clone();
        let loaded =
            tokio::task::spawn_blocking(move || load_session_record(&load_context, &id)).await;
        let Ok(Ok(updated)) = loaded else {
            continue;
        };
        if !same_provider_prompt_runtime(&current, &updated) {
            return (updated, None);
        }
        current = updated;
        let candidate = current.clone();
        let tail =
            tokio::task::spawn_blocking(move || ProviderPromptTail::open_new_runtime(&candidate))
                .await
                .ok()
                .flatten();
        if tail.is_some() {
            return (current, tail);
        }
    }
    (current, None)
}

fn provider_prompt_pending_fresh_runtime(record: &crate::SessionRecord) -> bool {
    if record
        .runtime
        .as_ref()
        .is_none_or(|runtime| runtime.generation != 1)
    {
        return false;
    }
    match AgentKind::from_name(&record.agent) {
        Some(AgentKind::Codex) => record.provider_resume.is_none(),
        Some(AgentKind::Claude) => record.provider_resume.as_ref().is_some_and(|resume| {
            resume.provider == "claude"
                && resume.capture_method == "claude-explicit-session-id"
                && !resume.session_id.trim().is_empty()
        }),
        Some(AgentKind::Hermes) | None => false,
    }
}

fn same_provider_prompt_runtime(
    expected: &crate::SessionRecord,
    current: &crate::SessionRecord,
) -> bool {
    match (&expected.runtime, &current.runtime) {
        (Some(expected), Some(current)) => {
            expected.generation == current.generation
                && expected.launch_id == current.launch_id
                && expected.tmux_session == current.tmux_session
        }
        _ => false,
    }
}

async fn finish_outbound_writer(mut writer_task: AbortOnDropTask<AttachWriterExit>, drain: bool) {
    if drain
        && tokio::time::timeout(ATTACH_WEBSOCKET_SEND_TIMEOUT, writer_task.handle_mut())
            .await
            .is_ok()
    {
        return;
    }
    writer_task.abort_and_wait().await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProviderPromptCapabilityState {
    supported: bool,
    provider: Option<ProviderKind>,
}

fn provider_prompt_subscription_requested(frame: &str) -> bool {
    serde_json::from_str::<Value>(frame)
        .ok()
        .and_then(|value| value.get("subscribe").and_then(Value::as_array).cloned())
        .is_some_and(|capabilities| {
            capabilities
                .iter()
                .any(|capability| capability.as_str() == Some(PROVIDER_PROMPT_CAPABILITY))
        })
}

fn provider_prompt_capability_frame(capability: ProviderPromptCapabilityState) -> String {
    json!({
        "schema_version": ATTACH_SCHEMA_VERSION,
        "type": "capability",
        "capability": PROVIDER_PROMPT_CAPABILITY,
        "supported": capability.supported,
        "provider": capability.provider.map(ProviderKind::as_str),
        "prompt_max_bytes": MAX_PROVIDER_PROMPT_BYTES,
    })
    .to_string()
}

fn provider_prompt_event_frame(provider: ProviderKind, event: ProviderPromptEvent) -> String {
    json!({
        "schema_version": ATTACH_EVENT_SCHEMA_VERSION,
        "type": "prompt_submitted",
        "event_id": event.id,
        "provider": provider.as_str(),
        "submitted_at": event.submitted_at,
        "text": event.prompt,
        "truncated": event.truncated,
    })
    .to_string()
}

async fn provider_prompt_loop(mut tail: ProviderPromptTail, sender: mpsc::Sender<Message>) {
    let provider = tail.provider();
    let mut interval = tokio::time::interval(PROVIDER_PROMPT_POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = sender.closed() => break,
            _ = interval.tick() => {}
        }
        let polled = tokio::task::spawn_blocking(move || {
            let result = tail.poll();
            (tail, result)
        })
        .await;
        let Ok((returned_tail, result)) = polled else {
            break;
        };
        tail = returned_tail;
        let Ok(events) = result else {
            break;
        };
        for event in events {
            let frame = Message::Text(provider_prompt_event_frame(provider, event).into());
            match sender.try_send(frame) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // Prompt events are advisory. Queue saturation must never
                    // apply backpressure to terminal output.
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => return,
            }
        }
    }
}

async fn outbound_writer<S>(
    mut sender: S,
    mut terminal_rx: mpsc::Receiver<Message>,
    mut control_rx: mpsc::Receiver<Message>,
    send_timeout: Duration,
) -> AttachWriterExit
where
    S: futures_util::Sink<Message> + Unpin,
{
    let mut terminal_burst = 0;
    while let Some(message) =
        next_outbound_message(&mut terminal_rx, &mut control_rx, &mut terminal_burst).await
    {
        if !send_attach_message(&mut sender, message, send_timeout).await {
            return AttachWriterExit::SendFailed;
        }
    }
    AttachWriterExit::Drained
}

async fn next_outbound_message(
    terminal_rx: &mut mpsc::Receiver<Message>,
    control_rx: &mut mpsc::Receiver<Message>,
    terminal_burst: &mut usize,
) -> Option<Message> {
    if *terminal_burst >= ATTACH_TERMINAL_BURST
        && let Ok(message) = control_rx.try_recv()
    {
        *terminal_burst = 0;
        return Some(message);
    }
    if let Ok(message) = terminal_rx.try_recv() {
        *terminal_burst += 1;
        return Some(message);
    }
    if let Ok(message) = control_rx.try_recv() {
        *terminal_burst = 0;
        return Some(message);
    }
    tokio::select! {
        biased;
        message = terminal_rx.recv() => {
            match message {
                Some(message) => {
                    *terminal_burst += 1;
                    Some(message)
                }
                None => {
                    let message = control_rx.recv().await;
                    if message.is_some() {
                        *terminal_burst = 0;
                    }
                    message
                },
            }
        }
        message = control_rx.recv() => {
            match message {
                Some(message) => {
                    *terminal_burst = 0;
                    Some(message)
                }
                None => {
                    let message = terminal_rx.recv().await;
                    if message.is_some() {
                        *terminal_burst += 1;
                    }
                    message
                },
            }
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
    resize_pane_with_timeout(
        tmux,
        target,
        cols,
        rows,
        force_repaint,
        ATTACH_RESIZE_TIMEOUT,
    )
    .await;
}

async fn resize_pane_with_timeout(
    tmux: &Path,
    target: &str,
    cols: u64,
    rows: u64,
    force_repaint: bool,
    timeout: Duration,
) {
    if force_repaint {
        let nudge_rows = if rows > 1 { rows - 1 } else { rows + 1 };
        let _ = run_resize_window(tmux, target, cols, nudge_rows, timeout).await;
        tokio::time::sleep(INITIAL_REPAINT_NUDGE_DELAY).await;
    }
    let _ = run_resize_window(tmux, target, cols, rows, timeout).await;
}

async fn run_resize_window(
    tmux: &Path,
    target: &str,
    cols: u64,
    rows: u64,
    timeout: Duration,
) -> io::Result<()> {
    let tmux = tmux.to_path_buf();
    let target = target.to_string();
    tokio::task::spawn_blocking(move || {
        let mut command = ProcessCommand::new(&tmux);
        command
            .arg("resize-window")
            .arg("-t")
            .arg(&target)
            .arg("-x")
            .arg(cols.to_string())
            .arg("-y")
            .arg(rows.to_string());
        run_process_with_timeout(&mut command, timeout).map(|_| ())
    })
    .await
    .map_err(|err| io::Error::other(format!("tmux resize task failed: {err}")))?
}

async fn handle_input(
    context: &CliContext,
    tmux: &Path,
    record: &crate::SessionRecord,
    target: &str,
    frame: &str,
    initial_repaint_pending: &mut bool,
    resize_lock: &tokio::sync::Mutex<()>,
) {
    let Ok(value) = serde_json::from_str::<Value>(frame) else {
        return;
    };

    if let Some(resize) = value.get("resize") {
        let cols = resize.get("cols").and_then(Value::as_u64);
        let rows = resize.get("rows").and_then(Value::as_u64);
        if let (Some(cols), Some(rows)) = (cols, rows) {
            let force_repaint = std::mem::take(initial_repaint_pending);
            // Concurrent attach clients share one tmux pane. Keep each resize
            // sequence atomic; the last completed client resize wins.
            let _guard = resize_lock.lock().await;
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
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt, symlink};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::{Message as ClientMessage, client::IntoClientRequest};
    use tower::ServiceExt;

    const MACHINE: &str = "test-machine";
    const TOKEN: &str = "s3cr3t-token";

    #[test]
    fn provider_prompt_subscription_requires_the_versioned_capability() {
        assert!(provider_prompt_subscription_requested(
            r#"{"subscribe":["provider-prompt.v1"]}"#
        ));
        assert!(provider_prompt_subscription_requested(
            r#"{"subscribe":["other.v1","provider-prompt.v1"]}"#
        ));
        assert!(!provider_prompt_subscription_requested(
            r#"{"subscribe":["provider-prompt.v2"]}"#
        ));
        assert!(!provider_prompt_subscription_requested(
            r#"{"text":"provider-prompt.v1"}"#
        ));
        assert!(!provider_prompt_subscription_requested("not json"));
    }

    #[test]
    fn provider_prompt_frames_use_the_versioned_text_contract() {
        let capability = provider_prompt_capability_frame(ProviderPromptCapabilityState {
            supported: true,
            provider: Some(ProviderKind::Codex),
        });
        let capability: Value = serde_json::from_str(&capability).expect("capability json");
        assert_eq!(
            capability,
            json!({
                "schema_version": ATTACH_SCHEMA_VERSION,
                "type": "capability",
                "capability": PROVIDER_PROMPT_CAPABILITY,
                "supported": true,
                "provider": "codex",
                "prompt_max_bytes": MAX_PROVIDER_PROMPT_BYTES,
            })
        );
        let unsupported: Value = serde_json::from_str(&provider_prompt_capability_frame(
            ProviderPromptCapabilityState {
                supported: false,
                provider: None,
            },
        ))
        .expect("unsupported capability json");
        assert_eq!(
            unsupported,
            json!({
                "schema_version": ATTACH_SCHEMA_VERSION,
                "type": "capability",
                "capability": PROVIDER_PROMPT_CAPABILITY,
                "supported": false,
                "provider": null,
                "prompt_max_bytes": MAX_PROVIDER_PROMPT_BYTES,
            })
        );

        let event = provider_prompt_event_frame(
            ProviderKind::Claude,
            ProviderPromptEvent {
                id: "pp-opaque".to_string(),
                prompt: "submitted prompt".to_string(),
                submitted_at: "2099-01-01T00:00:00Z".to_string(),
                truncated: false,
            },
        );
        let event: Value = serde_json::from_str(&event).expect("event json");
        assert_eq!(
            event,
            json!({
                "schema_version": ATTACH_EVENT_SCHEMA_VERSION,
                "type": "prompt_submitted",
                "event_id": "pp-opaque",
                "provider": "claude",
                "submitted_at": "2099-01-01T00:00:00Z",
                "text": "submitted prompt",
                "truncated": false,
            })
        );
        assert!(!event.to_string().contains("/home/"));
    }

    #[test]
    fn pending_provider_prompt_runtime_guard_rejects_replacement_identity() {
        let mut expected = test_record("pending-runtime", "hs-pending-runtime");
        expected.runtime = Some(crate::RuntimeInfo {
            kind: "tmux".to_string(),
            tmux_session: "hs-pending-runtime".to_string(),
            generation: 1,
            started_at: "2026-07-11T00:00:00Z".to_string(),
            launch_id: "launch-1".to_string(),
            extra: std::collections::BTreeMap::new(),
        });
        let mut current = expected.clone();
        assert!(same_provider_prompt_runtime(&expected, &current));

        current.runtime.as_mut().expect("runtime").launch_id = "launch-2".to_string();
        assert!(!same_provider_prompt_runtime(&expected, &current));
        current = expected.clone();
        current.runtime.as_mut().expect("runtime").generation = 2;
        assert!(!same_provider_prompt_runtime(&expected, &current));
        current = expected.clone();
        current.runtime.as_mut().expect("runtime").tmux_session = "hs-replaced".to_string();
        assert!(!same_provider_prompt_runtime(&expected, &current));
    }

    #[tokio::test]
    async fn outbound_writer_prioritizes_terminal_bytes_over_control_events() {
        let (terminal_tx, mut terminal_rx) = mpsc::channel(2);
        let (control_tx, mut control_rx) = mpsc::channel(2);
        control_tx
            .send(Message::Text("control".into()))
            .await
            .expect("queue control");
        terminal_tx
            .send(Message::Binary(vec![1, 2, 3].into()))
            .await
            .expect("queue terminal");

        let mut terminal_burst = 0;
        let first = next_outbound_message(&mut terminal_rx, &mut control_rx, &mut terminal_burst)
            .await
            .expect("first frame");
        let second = next_outbound_message(&mut terminal_rx, &mut control_rx, &mut terminal_burst)
            .await
            .expect("second frame");
        assert!(matches!(first, Message::Binary(_)));
        assert!(matches!(second, Message::Text(_)));
    }

    #[tokio::test]
    async fn outbound_writer_delivers_control_within_a_bounded_terminal_burst() {
        let (terminal_tx, mut terminal_rx) = mpsc::channel(ATTACH_TERMINAL_QUEUE_CAPACITY);
        let (control_tx, mut control_rx) = mpsc::channel(2);
        for byte in 0..ATTACH_TERMINAL_QUEUE_CAPACITY {
            terminal_tx
                .send(Message::Binary(vec![byte as u8].into()))
                .await
                .expect("queue terminal");
        }
        control_tx
            .send(Message::Text("capability".into()))
            .await
            .expect("queue control");

        let mut selected_control = false;
        let mut terminal_burst = 0;
        for _ in 0..=32 {
            let message =
                next_outbound_message(&mut terminal_rx, &mut control_rx, &mut terminal_burst)
                    .await
                    .expect("frame");
            if matches!(message, Message::Text(_)) {
                selected_control = true;
                break;
            }
        }
        assert!(
            selected_control,
            "control frame exceeded the terminal burst bound"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_subscription_emits_capability_and_new_provider_prompt_only() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let codex_home = tmp.path().join("codex-home");
        let transcript = codex_home.join("sessions/2026/07/session.jsonl");
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
            .expect("transcript dir");
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                json!({
                    "timestamp":"2099-01-01T00:00:00Z",
                    "type":"session_meta",
                    "payload":{
                        "id":"resume-session-id",
                        "session_id":"resume-session-id",
                        "cwd":cwd.to_string_lossy(),
                        "source":"cli",
                        "timestamp":"2099-01-01T00:00:00Z"
                    }
                }),
                json!({
                    "type":"event_msg",
                    "payload":{"type":"user_message","message":"pre-subscription prompt"}
                })
            ),
        )
        .expect("transcript");
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        seed_resumable_session(
            &state_dir,
            "ws-prompt",
            "codex",
            "hs-codex-ws-prompt",
            &cwd,
            &["resume", "resume-session-id"],
        );
        let tmux = minimal_tmux(tmp.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("address");
        let server_state_dir = state_dir.clone();
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                router(state(&server_state_dir, Some(TOKEN), tmux)),
            )
            .await;
        });

        let mut request = format!("ws://{addr}/sessions/ws-prompt/attach")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut socket, _) = connect_async(request).await.expect("connect");
        let snapshot = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("snapshot timeout")
            .expect("snapshot frame")
            .expect("snapshot message");
        assert!(matches!(snapshot, ClientMessage::Binary(_)));

        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("subscribe");
        let capability = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("capability timeout")
            .expect("capability frame")
            .expect("capability message");
        let ClientMessage::Text(capability) = capability else {
            panic!("capability must be a text frame");
        };
        let capability: Value = serde_json::from_str(capability.as_str()).expect("capability json");
        assert_eq!(capability["type"], "capability");
        assert_eq!(capability["supported"], true);

        OpenOptions::new()
            .append(true)
            .open(&transcript)
            .expect("append transcript")
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "type":"event_msg",
                        "payload":{"type":"user_message","message":"websocket prompt"}
                    })
                )
                .as_bytes(),
            )
            .expect("write prompt");
        let event = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("event timeout")
            .expect("event frame")
            .expect("event message");
        let ClientMessage::Text(event) = event else {
            panic!("prompt event must be a text frame");
        };
        let event: Value = serde_json::from_str(event.as_str()).expect("event json");
        assert_eq!(event["type"], "prompt_submitted");
        assert_eq!(event["text"], "websocket prompt");
        assert!(event["event_id"].is_string());
        assert!(event["submitted_at"].is_string());
        assert_eq!(event["provider"], "codex");
        assert!(
            !event
                .to_string()
                .contains(transcript.to_string_lossy().as_ref())
        );

        let rotated = transcript.with_extension("jsonl.old");
        std::fs::rename(&transcript, &rotated).expect("rotate transcript");
        std::fs::write(
            &transcript,
            format!(
                "{}\n",
                json!({
                    "timestamp":"2099-01-01T00:00:00Z",
                    "type":"session_meta",
                    "payload":{
                        "id":"different-session-id",
                        "cwd":cwd.to_string_lossy(),
                        "source":"cli",
                        "timestamp":"2099-01-01T00:00:00Z"
                    }
                })
            ),
        )
        .expect("mismatched replacement");
        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("resubscribe");
        let capability = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("resubscribe timeout")
            .expect("resubscribe frame")
            .expect("resubscribe message");
        let ClientMessage::Text(capability) = capability else {
            panic!("capability must be a text frame");
        };
        let capability: Value = serde_json::from_str(capability.as_str()).expect("capability json");
        assert_eq!(capability["supported"], false);

        let _ = socket.close(None).await;
        let session_dir = state_dir.join("sessions/ws-prompt");
        let mut cleaned = false;
        for _ in 0..20 {
            cleaned = std::fs::read_dir(&session_dir)
                .expect("session dir")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().starts_with("attach-"));
            if cleaned {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(cleaned, "disconnect must remove the private attach pipe");
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_subscription_recovers_fresh_codex_identity_and_first_prompt() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let codex_home = tmp.path().join("codex-home");
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let created_at = seed_fresh_provider_session(
            &state_dir,
            "ws-codex-pending",
            "codex",
            "hs-codex-pending",
            &cwd,
            None,
        );
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        let tmux = minimal_tmux(tmp.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("address");
        let server_state_dir = state_dir.clone();
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                router(state(&server_state_dir, Some(TOKEN), tmux)),
            )
            .await;
        });

        let mut request = format!("ws://{addr}/sessions/ws-codex-pending/attach")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut socket, _) = connect_async(request).await.expect("connect");
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), socket.next())
                .await
                .expect("snapshot timeout")
                .expect("snapshot frame")
                .expect("snapshot message"),
            ClientMessage::Binary(_)
        ));
        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("subscribe before identity");

        tokio::time::sleep(Duration::from_millis(50)).await;
        let transcript = codex_home.join("sessions/2026/07/pending.jsonl");
        std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
            .expect("transcript dir");
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                json!({
                    "timestamp":created_at,
                    "type":"session_meta",
                    "payload":{
                        "id":"fresh-codex-id",
                        "session_id":"fresh-codex-id",
                        "cwd":cwd.to_string_lossy(),
                        "source":"cli",
                        "timestamp":created_at
                    }
                }),
                json!({
                    "timestamp":created_at,
                    "type":"event_msg",
                    "payload":{"type":"user_message","message":"fresh codex first prompt"}
                })
            ),
        )
        .expect("fresh transcript");
        persist_provider_resume_identity(
            &state_dir,
            "ws-codex-pending",
            "codex",
            "fresh-codex-id",
            "codex-user-prompt-submit-hook",
        );

        assert_supported_provider_prompt(&mut socket, "codex").await;
        assert_provider_prompt(&mut socket, "codex", "fresh codex first prompt").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(250), socket.next())
                .await
                .is_err(),
            "fresh first prompt must be emitted exactly once"
        );
        let _ = socket.close(None).await;

        let mut request = format!("ws://{addr}/sessions/ws-codex-pending/attach")
            .into_client_request()
            .expect("reconnect request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut reconnect, _) = connect_async(request).await.expect("reconnect");
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), reconnect.next())
                .await
                .expect("reconnect snapshot timeout")
                .expect("reconnect snapshot frame")
                .expect("reconnect snapshot message"),
            ClientMessage::Binary(_)
        ));
        reconnect
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("reconnect subscribe");
        assert_supported_provider_prompt(&mut reconnect, "codex").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(250), reconnect.next())
                .await
                .is_err(),
            "reconnect must baseline at EOF instead of replaying the recovered prompt"
        );
        let _ = reconnect.close(None).await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_pending_codex_rejects_unrelated_same_cwd_transcript() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let codex_home = tmp.path().join("codex-home");
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let created_at = seed_fresh_provider_session(
            &state_dir,
            "ws-codex-unrelated",
            "codex",
            "hs-codex-unrelated",
            &cwd,
            None,
        );
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        let tmux = minimal_tmux(tmp.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("address");
        let server_state_dir = state_dir.clone();
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                router(state(&server_state_dir, Some(TOKEN), tmux)),
            )
            .await;
        });

        let mut request = format!("ws://{addr}/sessions/ws-codex-unrelated/attach")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut socket, _) = connect_async(request).await.expect("connect");
        let _snapshot = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("snapshot timeout")
            .expect("snapshot frame")
            .expect("snapshot message");
        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("subscribe");

        let transcript = codex_home.join("sessions/2026/07/unrelated.jsonl");
        std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
            .expect("transcript dir");
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                json!({
                    "timestamp":created_at,
                    "type":"session_meta",
                    "payload":{
                        "id":"unrelated-codex-id",
                        "session_id":"unrelated-codex-id",
                        "cwd":cwd.to_string_lossy(),
                        "source":"cli",
                        "timestamp":created_at
                    }
                }),
                json!({
                    "timestamp":created_at,
                    "type":"event_msg",
                    "payload":{"type":"user_message","message":"unrelated private prompt"}
                })
            ),
        )
        .expect("unrelated transcript");

        if let Ok(Some(Ok(ClientMessage::Text(frame)))) =
            tokio::time::timeout(Duration::from_millis(500), socket.next()).await
        {
            let frame: Value = serde_json::from_str(frame.as_str()).expect("control json");
            assert_eq!(
                frame["supported"], false,
                "unrelated transcript must not resolve capability"
            );
        }
        let record: Value = serde_json::from_slice(
            &std::fs::read(state_dir.join("sessions/ws-codex-unrelated/session.json"))
                .expect("session record"),
        )
        .expect("session json");
        assert!(
            record.get("provider_resume").is_none(),
            "pending provider discovery must not persist same-cwd history"
        );
        let _ = socket.close(None).await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_subscription_recovers_fresh_claude_transcript_and_first_prompt() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let claude_config = tmp.path().join("claude-config");
        let cwd = tmp.path().join("repo");
        let provider_session_id = "fresh-claude-id";
        std::fs::create_dir_all(&cwd).expect("cwd");
        let created_at = seed_fresh_provider_session(
            &state_dir,
            "ws-claude-pending",
            "claude",
            "hs-claude-pending",
            &cwd,
            Some((provider_session_id, "claude-explicit-session-id")),
        );
        let _claude_config =
            EnvGuard::set(&lock, "CLAUDE_CONFIG_DIR", claude_config.to_str().unwrap());
        let tmux = minimal_tmux(tmp.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("address");
        let server_state_dir = state_dir.clone();
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                router(state(&server_state_dir, Some(TOKEN), tmux)),
            )
            .await;
        });

        let mut request = format!("ws://{addr}/sessions/ws-claude-pending/attach")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut socket, _) = connect_async(request).await.expect("connect");
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), socket.next())
                .await
                .expect("snapshot timeout")
                .expect("snapshot frame")
                .expect("snapshot message"),
            ClientMessage::Binary(_)
        ));
        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("subscribe before transcript");

        tokio::time::sleep(Duration::from_millis(50)).await;
        let transcript = claude_config
            .join("projects/-pending")
            .join(format!("{provider_session_id}.jsonl"));
        std::fs::create_dir_all(transcript.parent().expect("transcript parent"))
            .expect("transcript dir");
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                json!({
                    "type":"user",
                    "uuid":"claude-turn-1",
                    "sessionId":provider_session_id,
                    "cwd":cwd.to_string_lossy(),
                    "timestamp":created_at,
                    "message":{"role":"user","content":"fresh claude first prompt"}
                }),
                json!({
                    "type":"last-prompt",
                    "sessionId":provider_session_id,
                    "leafUuid":"claude-turn-1",
                    "timestamp":created_at,
                    "lastPrompt":"fresh claude first prompt"
                })
            ),
        )
        .expect("fresh transcript");

        assert_supported_provider_prompt(&mut socket, "claude").await;
        assert_provider_prompt(&mut socket, "claude", "fresh claude first prompt").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(250), socket.next())
                .await
                .is_err(),
            "fresh first prompt must be emitted exactly once"
        );
        let _ = socket.close(None).await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_unsubscribed_client_gets_binary_only_and_unsupported_ack() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        seed_session(&state_dir, "ws-unsupported", "hermes", "hs-hermes-ws");
        let tmux = minimal_tmux(tmp.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router(state(&state_dir, Some(TOKEN), tmux))).await;
        });

        let mut request = format!("ws://{addr}/sessions/ws-unsupported/attach")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut socket, _) = connect_async(request).await.expect("connect");
        let snapshot = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("snapshot timeout")
            .expect("snapshot frame")
            .expect("snapshot message");
        assert!(matches!(snapshot, ClientMessage::Binary(_)));
        assert!(
            tokio::time::timeout(Duration::from_millis(200), socket.next())
                .await
                .is_err(),
            "unsubscribed clients must not receive text events"
        );

        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("subscribe");
        let capability = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("capability timeout")
            .expect("capability frame")
            .expect("capability message");
        let ClientMessage::Text(capability) = capability else {
            panic!("capability must be a text frame");
        };
        let capability: Value = serde_json::from_str(capability.as_str()).expect("capability json");
        assert_eq!(capability["supported"], false);
        let _ = socket.close(None).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_subscription_rejects_heuristic_same_cwd_codex_backfill() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let state_dir = tmp.path().join("state");
        let codex_home = tmp.path().join("codex-home");
        let transcript = codex_home.join("sessions/2000/01/session.jsonl");
        std::fs::create_dir_all(transcript.parent().expect("parent")).expect("dirs");
        std::fs::write(
            &transcript,
            format!(
                "{}\n",
                json!({
                    "timestamp":"2000-01-01T00:00:00Z",
                    "type":"session_meta",
                    "payload":{
                        "id":"backfill-resume-id",
                        "cwd":"/tmp",
                        "source":"cli",
                        "timestamp":"2000-01-01T00:00:00Z"
                    }
                })
            ),
        )
        .expect("transcript");
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().unwrap());
        seed_session(&state_dir, "ws-backfill", "codex", "hs-codex-backfill");
        let record_path = state_dir.join("sessions/ws-backfill/session.json");
        let tmux = minimal_tmux(tmp.path());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let addr = listener.local_addr().expect("address");
        let server_state_dir = state_dir.clone();
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                router(state(&server_state_dir, Some(TOKEN), tmux)),
            )
            .await;
        });

        let mut request = format!("ws://{addr}/sessions/ws-backfill/attach")
            .into_client_request()
            .expect("request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().expect("authorization"),
        );
        let (mut socket, _) = connect_async(request).await.expect("connect");
        let _snapshot = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("snapshot timeout")
            .expect("snapshot frame")
            .expect("snapshot message");
        let before: Value = serde_json::from_slice(
            &std::fs::read(&record_path).expect("record before subscription"),
        )
        .expect("record json");
        assert!(before.get("provider_resume").is_none());

        socket
            .send(ClientMessage::Text(
                json!({"subscribe":[PROVIDER_PROMPT_CAPABILITY]})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("subscribe");
        let capability = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("capability timeout")
            .expect("capability frame")
            .expect("capability message");
        let ClientMessage::Text(capability) = capability else {
            panic!("capability must be a text frame");
        };
        let capability: Value = serde_json::from_str(capability.as_str()).expect("capability json");
        assert_eq!(capability["supported"], false);
        let after: Value = serde_json::from_slice(
            &std::fs::read(&record_path).expect("record after subscription"),
        )
        .expect("record json");
        assert!(after.get("provider_resume").is_none());

        OpenOptions::new()
            .append(true)
            .open(&transcript)
            .expect("open unrelated transcript")
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "timestamp":"2099-01-01T00:00:00Z",
                        "type":"event_msg",
                        "payload":{
                            "type":"user_message",
                            "message":"unrelated same-cwd prompt"
                        }
                    })
                )
                .as_bytes(),
            )
            .expect("append unrelated prompt");
        assert!(
            tokio::time::timeout(Duration::from_millis(250), socket.next())
                .await
                .is_err(),
            "an unrelated same-cwd transcript must not emit prompt events"
        );
        let _ = socket.close(None).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provider_prompt_loop_drops_events_when_control_queue_is_saturated() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let transcript = tmp.path().join("codex.jsonl");
        std::fs::write(&transcript, "").expect("baseline");
        let tail = ProviderPromptTail::open_path(
            ProviderKind::Codex,
            "codex-id",
            transcript.clone(),
            Duration::ZERO,
        )
        .expect("tail");
        let (control_tx, mut control_rx) = mpsc::channel(1);
        control_tx
            .send(Message::Text("capability".into()))
            .await
            .expect("fill queue");
        let task = tokio::spawn(provider_prompt_loop(tail, control_tx));
        OpenOptions::new()
            .append(true)
            .open(&transcript)
            .expect("open transcript")
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "timestamp":"2099-01-01T00:00:00Z",
                        "type":"event_msg",
                        "payload":{"type":"user_message","message":"dropped prompt"}
                    })
                )
                .as_bytes(),
            )
            .expect("append prompt");
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(
            control_rx.recv().await,
            Some(Message::Text("capability".into()))
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(150), control_rx.recv())
                .await
                .is_err(),
            "advisory event should be dropped while the queue is full"
        );
        task.abort();
    }

    #[tokio::test]
    async fn provider_prompt_loop_stops_when_control_receiver_closes() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let transcript = tmp.path().join("codex.jsonl");
        std::fs::write(&transcript, "").expect("baseline");
        let tail = ProviderPromptTail::open_path(
            ProviderKind::Codex,
            "codex-id",
            transcript,
            Duration::ZERO,
        )
        .expect("tail");
        let (control_tx, control_rx) = mpsc::channel(1);
        let task = tokio::spawn(provider_prompt_loop(tail, control_tx));
        drop(control_rx);

        tokio::time::timeout(Duration::from_millis(250), task)
            .await
            .expect("provider watcher must stop when its writer queue closes")
            .expect("provider watcher task");
    }

    fn state(state_dir: &Path, token: Option<&str>, tmux_bin: PathBuf) -> Arc<ServeState> {
        Arc::new(ServeState {
            context: CliContext {
                state_dir: state_dir.to_path_buf(),
                host: None,
            },
            machine: MACHINE.to_string(),
            token: token.map(str::to_string),
            tmux_bin,
            attach_brokers: AttachBrokerRegistry::default(),
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

    fn seed_fresh_provider_session(
        state_dir: &Path,
        id: &str,
        agent: &str,
        tmux_session: &str,
        cwd: &Path,
        provider_resume: Option<(&str, &str)>,
    ) -> String {
        let created_at = jiff::Timestamp::now().to_string();
        let dir = state_dir.join("sessions").join(id);
        std::fs::create_dir_all(&dir).expect("session dir");
        let mut record = json!({
            "schema_version":"agent-session.session.v1",
            "id":id,
            "agent":agent,
            "mode":"interactive",
            "title":null,
            "cwd":cwd.to_string_lossy(),
            "tmux_session":tmux_session,
            "prompt_file":null,
            "log_file":null,
            "created_at":created_at,
            "updated_at":created_at,
            "runtime":{
                "kind":"tmux",
                "tmux_session":tmux_session,
                "generation":1,
                "started_at":created_at,
                "launch_id":format!("launch-{id}")
            },
            "agent_args":[]
        });
        if let Some((session_id, capture_method)) = provider_resume {
            record["provider_resume"] = json!({
                "provider":agent,
                "session_id":session_id,
                "captured_at":created_at,
                "capture_method":capture_method,
                "resume_args":[]
            });
        }
        std::fs::write(
            dir.join("session.json"),
            serde_json::to_vec_pretty(&record).expect("session json"),
        )
        .expect("session record");
        created_at
    }

    fn persist_provider_resume_identity(
        state_dir: &Path,
        id: &str,
        provider: &str,
        session_id: &str,
        capture_method: &str,
    ) {
        let path = state_dir.join("sessions").join(id).join("session.json");
        let mut record: Value =
            serde_json::from_slice(&std::fs::read(&path).expect("session record"))
                .expect("session json");
        record["provider_resume"] = json!({
            "provider":provider,
            "session_id":session_id,
            "captured_at":jiff::Timestamp::now().to_string(),
            "capture_method":capture_method,
            "resume_args":[]
        });
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&record).expect("session json"),
        )
        .expect("persist provider identity");
    }

    async fn assert_supported_provider_prompt(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        provider: &str,
    ) {
        let frame = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .expect("capability timeout")
            .expect("capability frame")
            .expect("capability message");
        let ClientMessage::Text(frame) = frame else {
            panic!("capability must be a text frame");
        };
        let frame: Value = serde_json::from_str(frame.as_str()).expect("capability json");
        assert_eq!(frame["type"], "capability");
        assert_eq!(frame["capability"], PROVIDER_PROMPT_CAPABILITY);
        assert_eq!(frame["supported"], true);
        assert_eq!(frame["provider"], provider);
    }

    async fn assert_provider_prompt(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        provider: &str,
        prompt: &str,
    ) {
        let frame = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .expect("prompt timeout")
            .expect("prompt frame")
            .expect("prompt message");
        let ClientMessage::Text(frame) = frame else {
            panic!("prompt must be a text frame");
        };
        let frame: Value = serde_json::from_str(frame.as_str()).expect("prompt json");
        assert_eq!(frame["type"], "prompt_submitted");
        assert_eq!(frame["provider"], provider);
        assert_eq!(frame["text"], prompt);
        assert!(frame["event_id"].is_string());
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
    "updated_at": 1783666800,
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
        assert_eq!(value["windows"][4]["reset_at_epoch"], 1_783_861_200);
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

    #[test]
    fn claude_inner_timeout_leaves_cleanup_slack_and_clamps_overrides() {
        let outer = Duration::from_secs(45);
        assert_eq!(claude_inner_timeout_seconds(outer, None), 40);
        assert_eq!(
            claude_inner_timeout_seconds(Duration::from_millis(45_001), None),
            40
        );
        assert_eq!(
            claude_inner_timeout_seconds(Duration::from_millis(1_001), None),
            1
        );
        assert_eq!(claude_inner_timeout_seconds(outer, Some("9")), 9);
        assert_eq!(claude_inner_timeout_seconds(outer, Some("90")), 40);
        assert_eq!(claude_inner_timeout_seconds(outer, Some("invalid")), 40);
    }

    #[test]
    fn usage_helper_timeout_kills_descendant_process_group() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let helper = tmp.path().join("hanging-helper");
        let pid_file = tmp.path().join("descendant.pid");
        fs::write(
            &helper,
            "#!/usr/bin/env sh\nsleep 60 &\nprintf '%s' \"$!\" > \"$PID_FILE\"\nwait\n",
        )
        .expect("write helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).expect("chmod helper");

        let output = run_usage_helper(
            helper.to_str().expect("helper path"),
            &[],
            &[("PID_FILE", pid_file.to_str().expect("pid path"))],
            Duration::from_millis(250),
        )
        .expect("run helper");
        assert!(output.timed_out);

        let pid = fs::read_to_string(&pid_file)
            .expect("descendant pid")
            .parse::<libc::pid_t>()
            .expect("numeric pid");
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "timed-out helper descendant must not survive"
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
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
            "x".repeat(nils_provider_resume::CLAUDE_SESSION_META_MAX_LINE_BYTES + 1),
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
        let resize_lock = tokio::sync::Mutex::new(());

        // Malformed JSON and an empty object must not touch tmux.
        let mut pending = false;
        handle_input(
            &ctx,
            &tmux,
            &record,
            target,
            "{ not json",
            &mut pending,
            &resize_lock,
        )
        .await;
        handle_input(
            &ctx,
            &tmux,
            &record,
            target,
            "{}",
            &mut pending,
            &resize_lock,
        )
        .await;
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
            &resize_lock,
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
        let resize_lock = tokio::sync::Mutex::new(());

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
            &resize_lock,
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
            &resize_lock,
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

    #[tokio::test]
    async fn concurrent_first_resize_sequences_do_not_interleave() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("calls.log");
        let tmux = logging_tmux(tmp.path(), &log);
        let ctx = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = test_record("look", "hs-codex-look");
        let target = "hs-codex-look:0.0";
        let resize_lock = tokio::sync::Mutex::new(());
        let mut first_pending = true;
        let mut second_pending = true;

        tokio::join!(
            handle_input(
                &ctx,
                &tmux,
                &record,
                target,
                r#"{"resize":{"cols":80,"rows":24}}"#,
                &mut first_pending,
                &resize_lock,
            ),
            handle_input(
                &ctx,
                &tmux,
                &record,
                target,
                r#"{"resize":{"cols":100,"rows":40}}"#,
                &mut second_pending,
                &resize_lock,
            ),
        );

        let calls = std::fs::read_to_string(&log).unwrap();
        let rows: Vec<&str> = calls
            .lines()
            .filter(|line| line.contains("resize-window"))
            .map(|line| line.rsplit_once("-y ").unwrap().1)
            .collect();
        assert!(
            rows == ["23", "24", "39", "40"] || rows == ["39", "40", "23", "24"],
            "each client's nudge/final resize pair must stay contiguous: {calls:?}"
        );
    }

    #[tokio::test]
    async fn timed_out_resize_releases_the_shared_lock() {
        let tmp = tempfile::TempDir::new().unwrap();
        let log = tmp.path().join("calls.log");
        let first_started = tmp.path().join("first-started");
        let tmux = executable(
            &tmp.path().join("tmux"),
            &format!(
                r#"#!/usr/bin/env sh
printf '%s\n' "$*" >> {}
if [ "$1" = "resize-window" ] && [ ! -e {} ]; then
  : > {}
  sleep 1
fi
exit 0
"#,
                shell_words::quote(&log.to_string_lossy()),
                shell_words::quote(&first_started.to_string_lossy()),
                shell_words::quote(&first_started.to_string_lossy()),
            ),
        );
        let resize_lock = tokio::sync::Mutex::new(());

        let first = async {
            let _guard = resize_lock.lock().await;
            resize_pane_with_timeout(
                &tmux,
                "hs-codex-look:0.0",
                80,
                24,
                false,
                Duration::from_millis(20),
            )
            .await;
        };
        let second = async {
            while !first_started.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::time::timeout(Duration::from_millis(250), async {
                let _guard = resize_lock.lock().await;
                resize_pane_with_timeout(
                    &tmux,
                    "hs-codex-look:0.0",
                    100,
                    40,
                    false,
                    Duration::from_millis(20),
                )
                .await;
            })
            .await
            .expect("a timed-out resize kept the shared lock");
        };
        tokio::join!(first, second);

        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.contains("resize-window"))
                .count(),
            2,
            "the second resize must run after the first command times out: {calls:?}"
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
    fn lagged_live_output_disconnects_the_slow_client() {
        assert!(
            attach_event_bytes(Err(tokio::sync::broadcast::error::RecvError::Lagged(1))).is_none(),
            "a lagged subscriber must disconnect instead of blocking the broker"
        );
        assert!(
            attach_event_bytes(Ok(AttachEvent::Closed)).is_none(),
            "a closed broker must disconnect its subscribers"
        );
    }

    #[tokio::test]
    async fn stalled_websocket_send_is_bounded() {
        struct PendingSink;

        impl futures_util::Sink<Message> for PendingSink {
            type Error = io::Error;

            fn poll_ready(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Pending
            }

            fn start_send(
                self: std::pin::Pin<&mut Self>,
                _item: Message,
            ) -> Result<(), Self::Error> {
                Ok(())
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Pending
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        assert!(
            !send_attach_message(
                &mut PendingSink,
                Message::Binary(Bytes::from_static(b"blocked")),
                Duration::from_millis(20),
            )
            .await,
            "a stalled WebSocket sink must time out"
        );
    }

    #[tokio::test]
    async fn outbound_writer_reports_a_stalled_sink() {
        struct PendingSink;

        impl futures_util::Sink<Message> for PendingSink {
            type Error = io::Error;

            fn poll_ready(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Pending
            }

            fn start_send(
                self: std::pin::Pin<&mut Self>,
                _item: Message,
            ) -> Result<(), Self::Error> {
                Ok(())
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Pending
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        let (terminal_tx, terminal_rx) = mpsc::channel(1);
        let (control_tx, control_rx) = mpsc::channel(1);
        terminal_tx
            .send(Message::Binary(Bytes::from_static(b"blocked")))
            .await
            .unwrap();
        drop(terminal_tx);
        drop(control_tx);

        assert_eq!(
            outbound_writer(
                PendingSink,
                terminal_rx,
                control_rx,
                Duration::from_millis(20),
            )
            .await,
            AttachWriterExit::SendFailed
        );
    }

    #[tokio::test]
    async fn normal_broker_close_drains_frames_already_accepted_by_the_writer() {
        #[derive(Clone)]
        struct RecordingSink(Arc<std::sync::Mutex<Vec<Message>>>);

        impl futures_util::Sink<Message> for RecordingSink {
            type Error = io::Error;

            fn poll_ready(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn start_send(
                self: std::pin::Pin<&mut Self>,
                item: Message,
            ) -> Result<(), Self::Error> {
                self.0.lock().unwrap().push(item);
                Ok(())
            }

            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }
        }

        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (terminal_tx, terminal_rx) = mpsc::channel(2);
        let (control_tx, control_rx) = mpsc::channel(1);
        terminal_tx
            .send(Message::Binary(Bytes::from_static(b"first")))
            .await
            .unwrap();
        terminal_tx
            .send(Message::Binary(Bytes::from_static(b"second")))
            .await
            .unwrap();
        drop(terminal_tx);
        drop(control_tx);

        let task = AbortOnDropTask::new(tokio::spawn(outbound_writer(
            RecordingSink(written.clone()),
            terminal_rx,
            control_rx,
            Duration::from_secs(1),
        )));
        finish_outbound_writer(task, true).await;

        let written = written.lock().unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(written[0], Message::Binary(Bytes::from_static(b"first")));
        assert_eq!(written[1], Message::Binary(Bytes::from_static(b"second")));
    }

    #[tokio::test]
    async fn attach_pump_drains_beyond_the_broadcast_ring_capacity() {
        let (broker_tx, broker_rx) = tokio::sync::broadcast::channel(ATTACH_BROADCAST_CAPACITY);
        let (terminal_tx, mut terminal_rx) = mpsc::channel(ATTACH_BROADCAST_CAPACITY * 2);
        let task = tokio::spawn(pump_attach_events(broker_rx, terminal_tx));

        for index in 0..=ATTACH_BROADCAST_CAPACITY {
            broker_tx
                .send(AttachEvent::Output(Bytes::from(vec![index as u8])))
                .unwrap();
            tokio::task::yield_now().await;
        }
        broker_tx.send(AttachEvent::Closed).unwrap();

        assert_eq!(task.await.unwrap(), AttachPumpExit::BrokerClosed);
        let mut received = 0;
        while terminal_rx.recv().await.is_some() {
            received += 1;
        }
        assert_eq!(received, ATTACH_BROADCAST_CAPACITY + 1);
    }

    #[tokio::test]
    async fn dropping_an_attach_child_task_aborts_it() {
        struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

        impl Drop for DropSignal {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let task = AbortOnDropTask::new(tokio::spawn(async move {
            let _signal = DropSignal(Some(dropped_tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        }));
        started_rx.await.unwrap();
        drop(task);

        tokio::time::timeout(Duration::from_millis(250), dropped_rx)
            .await
            .expect("aborted task must drop its owned state")
            .expect("drop signal");
    }

    #[tokio::test]
    async fn snapshot_handoff_recaptures_after_internal_buffer_overflow() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("capture-calls");
        let first_started = tmp.path().join("first-started");
        let first_release = tmp.path().join("first-release");
        let second_started = tmp.path().join("second-started");
        let second_release = tmp.path().join("second-release");
        seed_session(&state_dir, "recapture", "codex", "hs-codex-recapture");
        let tmux = executable(
            &tmp.path().join("tmux"),
            &format!(
                r#"#!/usr/bin/env sh
if [ "$1" = "capture-pane" ]; then
  printf 'capture\n' >> {calls}
  count=$(wc -l < {calls})
  if [ "$count" -eq 1 ]; then
    : > {first_started}
    while [ ! -f {first_release} ]; do sleep 0.01; done
    printf 'stale\n'
  else
    : > {second_started}
    while [ ! -f {second_release} ]; do sleep 0.01; done
    printf 'fresh\n'
  fi
fi
exit 0
"#,
                calls = shell_words::quote(&calls.to_string_lossy()),
                first_started = shell_words::quote(&first_started.to_string_lossy()),
                first_release = shell_words::quote(&first_release.to_string_lossy()),
                second_started = shell_words::quote(&second_started.to_string_lossy()),
                second_release = shell_words::quote(&second_release.to_string_lossy()),
            ),
        );
        let context = CliContext {
            state_dir,
            host: None,
        };
        let record = load_session_record(&context, "recapture").unwrap();
        let (broker_tx, mut broker_rx) = tokio::sync::broadcast::channel(ATTACH_BROADCAST_CAPACITY);
        let task =
            tokio::spawn(
                async move { capture_attach_handoff(&tmux, &record, &mut broker_rx).await },
            );

        wait_for_file(&first_started).await;
        for index in 0..=ATTACH_HANDOFF_BUFFER_CAPACITY {
            broker_tx
                .send(AttachEvent::Output(Bytes::from(vec![index as u8])))
                .unwrap();
            tokio::task::yield_now().await;
        }
        std::fs::write(&first_release, b"").unwrap();
        wait_for_file(&second_started).await;
        broker_tx
            .send(AttachEvent::Output(Bytes::from_static(b"after-recapture")))
            .unwrap();
        tokio::task::yield_now().await;
        std::fs::write(&second_release, b"").unwrap();

        let handoff = task.await.unwrap().unwrap();
        assert_eq!(handoff.snapshot.as_deref(), Some("fresh\n"));
        assert_eq!(handoff.live.len(), 1);
        assert_eq!(handoff.live[0], Bytes::from_static(b"after-recapture"));
        assert_eq!(std::fs::read_to_string(calls).unwrap().lines().count(), 2);
    }

    #[tokio::test]
    async fn attach_broker_fans_out_and_only_last_subscriber_tears_down_pipe() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let source = tmp.path().join("pane-output.fifo");
        let writer_pid = tmp.path().join("writer.pid");
        create_private_fifo(&source).unwrap();
        seed_session(&state_dir, "fanout", "codex", "hs-codex-fanout");

        let tmux = fanout_tmux(tmp.path(), &calls, &source, &writer_pid);
        let context = CliContext {
            state_dir: state_dir.clone(),
            host: None,
        };
        let record = load_session_record(&context, "fanout").unwrap();
        let registry = AttachBrokerRegistry::default();
        let live_fifo = session_dir(&context, "fanout").join(ATTACH_LIVE_FIFO_NAME);
        crate::write_private_file(&live_fifo, b"stale daemon bytes").unwrap();

        let mut first = registry.subscribe(&context, &tmux, &record).await.unwrap();
        let mut second = registry.subscribe(&context, &tmux, &record).await.unwrap();
        assert!(
            std::fs::symlink_metadata(&live_fifo)
                .unwrap()
                .file_type()
                .is_fifo(),
            "the live broker transport must be an ephemeral FIFO"
        );

        let mut source_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap();
        source_writer.write_all(b"hello").unwrap();
        source_writer.flush().unwrap();

        assert_eq!(recv_attach_bytes(&mut first).await, b"hello");
        assert_eq!(recv_attach_bytes(&mut second).await, b"hello");

        let closes_before_detach = pipe_close_count(&calls);
        first.release().await;
        assert_eq!(
            pipe_close_count(&calls),
            closes_before_detach,
            "disconnecting one subscriber must not close the shared tmux pipe"
        );
        assert!(live_fifo.exists());

        source_writer.write_all(b"again").unwrap();
        source_writer.flush().unwrap();
        assert_eq!(recv_attach_bytes(&mut second).await, b"again");

        second.release().await;
        assert_eq!(
            pipe_close_count(&calls),
            closes_before_detach + 1,
            "the final subscriber must close the shared tmux pipe"
        );
        assert!(
            !live_fifo.exists(),
            "broker teardown must remove the ephemeral FIFO"
        );

        let calls = std::fs::read_to_string(&calls).unwrap();
        let enables: Vec<_> = calls
            .lines()
            .filter(|line| line.contains("cat >"))
            .collect();
        assert_eq!(
            enables.len(),
            1,
            "simultaneous subscribers must share one tmux pipe: {calls:?}"
        );
        assert!(
            !enables[0].split_whitespace().any(|arg| arg == "-o"),
            "a fresh broker must replace a stale tmux pipe instead of preserving it: {calls:?}"
        );
    }

    #[tokio::test]
    async fn websocket_attach_fans_out_live_output_after_snapshot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let source = tmp.path().join("pane-output.fifo");
        let writer_pid = tmp.path().join("writer.pid");
        create_private_fifo(&source).unwrap();
        seed_session(&state_dir, "socket", "codex", "hs-codex-socket");

        let tmux = fanout_tmux(tmp.path(), &calls, &source, &writer_pid);
        let state = state(&state_dir, Some(TOKEN), tmux);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let connect = || async {
            let mut request = format!("ws://{address}/sessions/socket/attach")
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
            tokio_tungstenite::connect_async(request).await.unwrap().0
        };
        let mut first = connect().await;
        let mut second = connect().await;
        assert_eq!(recv_websocket_binary(&mut first).await, b"pane\n");
        assert_eq!(recv_websocket_binary(&mut second).await, b"pane\n");
        wait_for_subscriber_count(&state.attach_brokers, "socket", 2).await;

        let mut source_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap();
        source_writer.write_all(b"both-clients").unwrap();
        source_writer.flush().unwrap();
        assert_eq!(recv_websocket_binary(&mut first).await, b"both-clients");
        assert_eq!(recv_websocket_binary(&mut second).await, b"both-clients");

        first.close(None).await.unwrap();
        wait_for_subscriber_count(&state.attach_brokers, "socket", 1).await;
        source_writer.write_all(b"remaining-client").unwrap();
        source_writer.flush().unwrap();
        assert_eq!(
            recv_websocket_binary(&mut second).await,
            b"remaining-client"
        );

        second.close(None).await.unwrap();
        wait_for_subscriber_count(&state.attach_brokers, "socket", 0).await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn websocket_attach_buffers_live_output_during_snapshot_capture() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let fifo_destination = tmp.path().join("fifo-destination");
        let writer_pid = tmp.path().join("writer.pid");
        let capture_started = tmp.path().join("capture-started");
        let capture_release = tmp.path().join("capture-release");
        seed_session(&state_dir, "handoff", "codex", "hs-codex-handoff");

        let tmux = snapshot_handoff_tmux(
            tmp.path(),
            &calls,
            &fifo_destination,
            &writer_pid,
            &capture_started,
            &capture_release,
        );
        let state = state(&state_dir, Some(TOKEN), tmux);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mut request = format!("ws://{address}/sessions/handoff/attach")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
        let mut socket = tokio_tungstenite::connect_async(request).await.unwrap().0;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !capture_started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("capture-pane never reached its controlled wait");

        let live_fifo = PathBuf::from(std::fs::read_to_string(&fifo_destination).unwrap());
        let mut live_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(live_fifo)
            .unwrap();
        live_writer.write_all(b"during-capture").unwrap();
        live_writer.flush().unwrap();
        std::fs::write(&capture_release, b"").unwrap();

        assert_eq!(recv_websocket_binary(&mut socket).await, b"pane\n");
        assert_eq!(recv_websocket_binary(&mut socket).await, b"during-capture");
        socket.close(None).await.unwrap();
        wait_for_subscriber_count(&state.attach_brokers, "handoff", 0).await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn snapshot_timeout_releases_the_final_attach_broker() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let fifo_destination = tmp.path().join("fifo-destination");
        let writer_pid = tmp.path().join("writer.pid");
        let capture_started = tmp.path().join("capture-started");
        let capture_release = tmp.path().join("never-release");
        seed_session(&state_dir, "timeout", "codex", "hs-codex-timeout");

        let tmux = snapshot_handoff_tmux(
            tmp.path(),
            &calls,
            &fifo_destination,
            &writer_pid,
            &capture_started,
            &capture_release,
        );
        let state = state(&state_dir, Some(TOKEN), tmux);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let mut request = format!("ws://{address}/sessions/timeout/attach")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
        let mut socket = tokio_tungstenite::connect_async(request).await.unwrap().0;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !capture_started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("capture-pane never started");
        assert_eq!(state.attach_brokers.subscriber_count("timeout").await, 1);
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if state.attach_brokers.subscriber_count("timeout").await == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("snapshot timeout did not release the final broker lease");
        assert!(
            !session_dir(&state.context, "timeout")
                .join(ATTACH_LIVE_FIFO_NAME)
                .exists(),
            "snapshot timeout must remove the broker FIFO"
        );
        let _ = socket.close(None).await;
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn dropping_last_attach_subscription_releases_the_broker() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let source = tmp.path().join("pane-output.fifo");
        let writer_pid = tmp.path().join("writer.pid");
        create_private_fifo(&source).unwrap();
        seed_session(&state_dir, "cancel", "codex", "hs-codex-cancel");

        let tmux = fanout_tmux(tmp.path(), &calls, &source, &writer_pid);
        let context = CliContext {
            state_dir: state_dir.clone(),
            host: None,
        };
        let record = load_session_record(&context, "cancel").unwrap();
        let registry = AttachBrokerRegistry::default();
        let live_fifo = session_dir(&context, "cancel").join(ATTACH_LIVE_FIFO_NAME);

        let subscription = registry.subscribe(&context, &tmux, &record).await.unwrap();
        assert_eq!(registry.subscriber_count("cancel").await, 1);
        drop(subscription);

        wait_for_subscriber_count(&registry, "cancel", 0).await;
        assert!(
            !live_fifo.exists(),
            "cancelling an attach task must remove its final broker FIFO"
        );
        assert_eq!(pipe_close_count(&calls), 1);
    }

    #[tokio::test]
    async fn aborting_release_during_teardown_does_not_strand_the_broker() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let close_started = tmp.path().join("close-started");
        seed_session(&state_dir, "abort", "codex", "hs-codex-abort");
        let tmux = executable(
            &tmp.path().join("tmux"),
            &format!(
                r#"#!/usr/bin/env sh
printf '%s\n' "$*" >> {}
if [ "$1" = "pipe-pane" ]; then
  last=""
  for arg in "$@"; do last="$arg"; done
  case "$last" in
    "cat > "*) ;;
    *)
      if [ ! -e {} ]; then
        : > {}
        sleep 1
      fi
      ;;
  esac
fi
exit 0
"#,
                shell_words::quote(&calls.to_string_lossy()),
                shell_words::quote(&close_started.to_string_lossy()),
                shell_words::quote(&close_started.to_string_lossy()),
            ),
        );
        let context = CliContext {
            state_dir,
            host: None,
        };
        let record = load_session_record(&context, "abort").unwrap();
        let registry = AttachBrokerRegistry::default();
        let live_fifo = session_dir(&context, "abort").join(ATTACH_LIVE_FIFO_NAME);
        let subscription = registry.subscribe(&context, &tmux, &record).await.unwrap();

        let release_task = tokio::spawn(subscription.release());
        tokio::time::timeout(Duration::from_secs(1), async {
            while !close_started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("broker teardown never reached the blocking close command");
        release_task.abort();
        let _ = release_task.await;

        tokio::time::timeout(Duration::from_secs(2), async {
            while live_fifo.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("supervised teardown did not remove the broker FIFO");
        assert_eq!(pipe_close_count(&calls), 1);

        let replacement = registry.subscribe(&context, &tmux, &record).await.unwrap();
        replacement.release().await;
        assert_eq!(pipe_close_count(&calls), 2);
    }

    #[tokio::test]
    async fn slow_broker_start_does_not_block_an_unrelated_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let slow_started = tmp.path().join("slow-started");
        seed_session(&state_dir, "slow", "codex", "hs-codex-slow");
        seed_session(&state_dir, "fast", "codex", "hs-codex-fast");
        let tmux = executable(
            &tmp.path().join("tmux"),
            &format!(
                "#!/usr/bin/env sh\ncase \"$*\" in\n  *hs-codex-slow*\\ cat\\ \\>* ) : > {}; sleep 1 ;;\nesac\nexit 0\n",
                shell_words::quote(&slow_started.to_string_lossy())
            ),
        );
        let context = CliContext {
            state_dir,
            host: None,
        };
        let slow_record = load_session_record(&context, "slow").unwrap();
        let fast_record = load_session_record(&context, "fast").unwrap();
        let registry = Arc::new(AttachBrokerRegistry::default());

        let slow_task = tokio::spawn({
            let registry = registry.clone();
            let context = context.clone();
            let tmux = tmux.clone();
            async move {
                registry
                    .subscribe(&context, &tmux, &slow_record)
                    .await
                    .unwrap()
            }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !slow_started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("slow session never began broker startup");

        let fast = tokio::time::timeout(
            Duration::from_millis(250),
            registry.subscribe(&context, &tmux, &fast_record),
        )
        .await
        .expect("an unrelated session was blocked by slow broker startup")
        .unwrap();
        fast.release().await;

        let slow = slow_task.await.unwrap();
        slow.release().await;
    }

    #[tokio::test]
    async fn late_subscriber_restarts_a_closed_broker_generation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        let calls = tmp.path().join("calls.log");
        let source = tmp.path().join("pane-output.fifo");
        let writer_pid = tmp.path().join("writer.pid");
        create_private_fifo(&source).unwrap();
        seed_session(&state_dir, "restart", "codex", "hs-codex-restart");

        let tmux = fanout_tmux(tmp.path(), &calls, &source, &writer_pid);
        let context = CliContext {
            state_dir,
            host: None,
        };
        let record = load_session_record(&context, "restart").unwrap();
        let registry = AttachBrokerRegistry::default();
        let mut first = registry.subscribe(&context, &tmux, &record).await.unwrap();

        let mut source_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap();
        source_writer.write_all(b"before-close").unwrap();
        source_writer.flush().unwrap();
        assert_eq!(recv_attach_bytes(&mut first).await, b"before-close");
        drop(source_writer);
        terminate_process_from_file(&writer_pid);

        let closed = tokio::time::timeout(Duration::from_secs(5), first.receiver_mut().recv())
            .await
            .expect("timed out waiting for the old broker to close");
        assert!(attach_event_bytes(closed).is_none());

        let mut second = registry.subscribe(&context, &tmux, &record).await.unwrap();
        let mut source_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap();
        source_writer.write_all(b"after-restart").unwrap();
        source_writer.flush().unwrap();
        assert_eq!(recv_attach_bytes(&mut second).await, b"after-restart");

        first.release().await;
        second.release().await;
        let enables = std::fs::read_to_string(&calls)
            .unwrap()
            .lines()
            .filter(|line| line.contains("cat >"))
            .count();
        assert_eq!(enables, 2, "a closed generation must be replaced once");
    }

    async fn recv_attach_bytes(subscription: &mut AttachSubscription) -> Vec<u8> {
        let event =
            tokio::time::timeout(Duration::from_secs(2), subscription.receiver_mut().recv())
                .await
                .expect("timed out waiting for broker output");
        attach_event_bytes(event)
            .expect("broker closed before delivering output")
            .to_vec()
    }

    async fn recv_websocket_binary<S>(socket: &mut S) -> Vec<u8>
    where
        S: futures_util::Stream<
                Item = Result<
                    tokio_tungstenite::tungstenite::Message,
                    tokio_tungstenite::tungstenite::Error,
                >,
            > + Unpin,
    {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("timed out waiting for WebSocket output")
            .expect("WebSocket closed before output")
            .expect("WebSocket output failed");
        match message {
            tokio_tungstenite::tungstenite::Message::Binary(bytes) => bytes.to_vec(),
            other => panic!("expected binary WebSocket output, got {other:?}"),
        }
    }

    async fn wait_for_subscriber_count(
        registry: &AttachBrokerRegistry,
        session_id: &str,
        expected: usize,
    ) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if registry.subscriber_count(session_id).await == expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for subscriber cleanup");
    }

    async fn wait_for_file(path: &Path) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed out waiting for test marker");
    }

    fn pipe_close_count(calls: &Path) -> usize {
        std::fs::read_to_string(calls)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with("pipe-pane") && !line.contains("cat >"))
            .count()
    }

    fn terminate_process_from_file(pid_file: &Path) {
        let pid = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse::<libc::pid_t>()
            .unwrap();
        // SAFETY: the pid comes from the test-owned tmux stub. ESRCH is fine:
        // closing the source FIFO may have already ended the writer naturally.
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            let err = io::Error::last_os_error();
            assert_eq!(err.raw_os_error(), Some(libc::ESRCH));
        }
    }

    fn fanout_tmux(dir: &Path, calls: &Path, source: &Path, writer_pid: &Path) -> PathBuf {
        executable(
            &dir.join("tmux"),
            &format!(
                r#"#!/usr/bin/env sh
printf '%s\n' "$*" >> {}
if [ "$1" = "capture-pane" ]; then
  printf 'pane\n'
  exit 0
fi
if [ "$1" = "pipe-pane" ]; then
  last=""
  for arg in "$@"; do last="$arg"; done
  case "$last" in
    "cat > "*)
      dest=${{last#cat > }}
      (exec 3>"$dest"; exec cat {} >&3) &
      printf '%s\n' "$!" > {}
      ;;
    *)
      if [ -s {} ]; then
        kill "$(cat {})" 2>/dev/null || true
        rm -f {}
      fi
      ;;
  esac
fi
exit 0
"#,
                shell_words::quote(&calls.to_string_lossy()),
                shell_words::quote(&source.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
            ),
        )
    }

    fn snapshot_handoff_tmux(
        dir: &Path,
        calls: &Path,
        fifo_destination: &Path,
        writer_pid: &Path,
        capture_started: &Path,
        capture_release: &Path,
    ) -> PathBuf {
        executable(
            &dir.join("tmux"),
            &format!(
                r#"#!/usr/bin/env sh
printf '%s\n' "$*" >> {}
if [ "$1" = "capture-pane" ]; then
  : > {}
  while [ ! -e {} ]; do sleep 0.01; done
  printf 'pane\n'
  exit 0
fi
if [ "$1" = "pipe-pane" ]; then
  last=""
  for arg in "$@"; do last="$arg"; done
  case "$last" in
    "cat > "*)
      dest=${{last#cat > }}
      printf '%s' "$dest" > {}
      (exec 3>"$dest"; exec sleep 30) &
      printf '%s\n' "$!" > {}
      ;;
    *)
      if [ -s {} ]; then
        kill "$(cat {})" 2>/dev/null || true
        rm -f {}
      fi
      ;;
  esac
fi
exit 0
"#,
                shell_words::quote(&calls.to_string_lossy()),
                shell_words::quote(&capture_started.to_string_lossy()),
                shell_words::quote(&capture_release.to_string_lossy()),
                shell_words::quote(&fifo_destination.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
                shell_words::quote(&writer_pid.to_string_lossy()),
            ),
        )
    }

    #[test]
    fn private_files_and_attach_fifos_are_0600() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file_path = tmp.path().join("private.file");
        crate::write_private_file(&file_path, b"").unwrap();
        let mode = std::fs::metadata(&file_path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "private files must not be group/world readable"
        );

        let fifo_path = tmp.path().join("attach.fifo");
        create_private_fifo(&fifo_path).unwrap();
        let metadata = std::fs::metadata(&fifo_path).unwrap();
        assert!(metadata.file_type().is_fifo());
        assert_eq!(
            metadata.permissions().mode() & 0o077,
            0,
            "live terminal FIFO must not be group/world readable"
        );
    }

    #[tokio::test]
    async fn broker_start_removes_fifo_after_post_create_failure() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state_dir = tmp.path().join("state");
        seed_session(&state_dir, "rollback", "codex", "hs-codex-rollback");
        let context = CliContext {
            state_dir,
            host: None,
        };
        let record = load_session_record(&context, "rollback").unwrap();
        let fifo_path = session_dir(&context, "rollback").join(ATTACH_LIVE_FIFO_NAME);

        let result = AttachBroker::start_with_fifo_opener(
            &context,
            Path::new("unused-tmux"),
            &record,
            |_| Err(io::Error::other("injected FIFO open failure")),
        )
        .await;
        assert!(result.is_err());
        assert!(
            !fifo_path.exists(),
            "broker startup failure must roll back its ephemeral FIFO"
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

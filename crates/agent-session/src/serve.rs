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

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

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
use nils_common::cli_contract::{exit, schema_version_for};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::cli::{self, AgentKind, SpecialKey};
use crate::{
    BINARY, CliContext, CliError, delete_session, glance_session, list_sessions,
    load_session_record, non_empty_env, resolve_tmux_bin, run_capture_pane, search_workdirs,
    send_input, session_dir, short_hostname, start_session, update_session_title,
    write_private_file, write_session_attachment,
};

/// Monotonic counter giving each attach connection a private pipe file, so
/// concurrent attaches to one session never delete each other's file.
static ATTACH_SEQ: AtomicU64 = AtomicU64::new(0);
const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;

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

    // Sanitize the explicit --token the same way the env fallback is sanitized,
    // so an empty/whitespace `--token ""` fails closed instead of authorizing an
    // empty bearer.
    let token = sanitize_token(args.token.clone()).or_else(|| non_empty_env("AGENT_SESSION_TOKEN"));
    if token.is_none() {
        eprintln!(
            "warning: no --token / AGENT_SESSION_TOKEN set; write and attach endpoints are disabled"
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
        .route("/workdirs", get(workdirs_handler))
        .route("/sessions/{id}/glance", get(glance_handler))
        .route("/sessions/{id}/send", post(send_handler))
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
struct UpdateSessionBody {
    #[serde(default)]
    title: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
struct AttachmentQuery {
    filename: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkdirQuery {
    q: Option<String>,
    limit: Option<usize>,
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

async fn update_session_handler(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    AxPath(id): AxPath<String>,
    Json(body): Json<UpdateSessionBody>,
) -> Response {
    if let Some(resp) = deny_unauthorized(&state, &headers) {
        return resp;
    }
    let Some(title) = body.title else {
        return envelope_err(CliError::usage(
            "missing-title",
            "session update requires a title field",
            Some(json!({ "field": "title" })),
        ));
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
    Query(query): Query<WorkdirQuery>,
) -> Response {
    let q = query.q.clone();
    let limit = query.limit;
    match tokio::task::spawn_blocking(move || search_workdirs(q.as_deref(), limit)).await {
        Ok(Ok(workdirs)) => envelope_ok(json!({ "machine": state.machine, "workdirs": workdirs })),
        Ok(Err(err)) => envelope_err(err),
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
    let lookup = tokio::task::spawn_blocking({
        let context = context.clone();
        let id = id.clone();
        move || load_session_record(&context, &id)
    })
    .await;
    let record = match lookup {
        Ok(Ok(record)) => record,
        Ok(Err(err)) => return envelope_err(err),
        Err(_) => return join_err(),
    };
    let tmux = state.tmux_bin.clone();
    ws.on_upgrade(move |socket| attach_socket(socket, context, tmux, record))
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
    use std::os::unix::fs::PermissionsExt;
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
            "#!/usr/bin/env sh\ncase \"$1\" in\n  has-session) exit 0 ;;\n  capture-pane) printf 'pane\\n'; exit 0 ;;\n  *) exit 0 ;;\nesac\n",
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

    fn post_bytes(uri: &str, token: Option<&str>, body: &'static [u8]) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/octet-stream");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::from(body)).unwrap()
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
            patch_json("/sessions/steer", Some(TOKEN), json!({ "title": "" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["data"]["session"]["title"], Value::Null);
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
    }

    #[tokio::test]
    async fn workdir_search_filters_default_project_and_config_roots() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let project_match = home.join("Project/sympoies/agent-console");
        let config_match = home.join(".config/zsh");
        let outside = home.join("Downloads/agent-console-copy");
        std::fs::create_dir_all(project_match.join(".git")).unwrap();
        std::fs::create_dir_all(&config_match).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().unwrap());

        let st = state(tmp.path(), Some(TOKEN), PathBuf::from("tmux"));
        let (status, body) = call(router(st.clone()), get("/workdirs?q=agent&limit=20")).await;
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

        let (status, body) = call(router(st.clone()), get("/workdirs?q=zsh&limit=20")).await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        let workdirs = body["data"]["workdirs"].as_array().expect("workdirs array");
        assert!(
            workdirs
                .iter()
                .any(|item| item["path"] == config_match.to_string_lossy().as_ref()),
            ".config match missing: {workdirs:?}"
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
        }
    }
}

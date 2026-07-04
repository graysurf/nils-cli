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
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxPath, Query, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use nils_common::cli_contract::{exit, schema_version_for};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::cli::{self, AgentKind, SpecialKey};
use crate::{
    BINARY, CliContext, CliError, delete_session, glance_session, list_sessions,
    load_session_record, non_empty_env, resolve_tmux_bin, run_capture_pane, send_input,
    session_dir, short_hostname, start_session,
};

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
    if !bind.ip().is_loopback() {
        eprintln!(
            "warning: binding a non-loopback address ({bind}) exposes a remote shell; \
             keep it tailnet-only behind `tailscale serve` and off the public internet"
        );
    }

    let token = args
        .token
        .clone()
        .or_else(|| non_empty_env("AGENT_SESSION_TOKEN"));
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
        .route("/sessions/{id}/glance", get(glance_handler))
        .route("/sessions/{id}/send", post(send_handler))
        .route("/sessions/{id}/attach", get(attach_handler))
        .route("/sessions/{id}", delete(delete_handler))
        .with_state(state)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

// --- response helpers ---------------------------------------------------------

fn envelope_ok(command: &str, data: Value) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "schema_version": schema_version_for(BINARY, command, 1),
            "ok": true,
            "data": data,
        })),
    )
        .into_response()
}

fn envelope_err(command: &str, err: CliError) -> Response {
    let data = err.into_inner();
    let status = match data.code.as_str() {
        "session-not-found" => StatusCode::NOT_FOUND,
        "ambiguous-session-id" => StatusCode::CONFLICT,
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
            "schema_version": schema_version_for(BINARY, command, 1),
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
            "schema_version": schema_version_for(BINARY, "serve", 1),
            "ok": false,
            "error": { "code": code, "message": message },
        })),
    )
        .into_response()
}

fn join_err(command: &str) -> Response {
    envelope_err(
        command,
        CliError::runtime("serve-task-failed", "internal task failed", None),
    )
}

/// Length-checked, XOR-accumulating comparison to avoid leaking the token length
/// difference through early-return timing. Not echoed anywhere.
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

// --- handlers -----------------------------------------------------------------

async fn healthz(State(state): State<Arc<ServeState>>) -> Response {
    envelope_ok("serve", json!({ "status": "ok", "machine": state.machine }))
}

async fn list_handler(State(state): State<Arc<ServeState>>) -> Response {
    let context = state.context.clone();
    let tmux = state.tmux_bin.clone();
    match tokio::task::spawn_blocking(move || list_sessions(&context, Some(&tmux))).await {
        Ok(Ok(sessions)) => envelope_ok(
            "list",
            json!({ "machine": state.machine, "sessions": sessions }),
        ),
        Ok(Err(err)) => envelope_err("list", err),
        Err(_) => join_err("list"),
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
        tail: query.tail.unwrap_or(40),
        tmux_bin: Some(state.tmux_bin.clone()),
        format: nils_common::cli_contract::OutputFormat::Json,
    };
    match tokio::task::spawn_blocking(move || glance_session(&context, args)).await {
        Ok(Ok(glance)) => envelope_ok(
            "glance",
            json!({ "machine": state.machine, "glance": glance }),
        ),
        Ok(Err(err)) => envelope_err("glance", err),
        Err(_) => join_err("glance"),
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
        return envelope_err(
            "start",
            CliError::usage(
                "invalid-agent",
                format!("unknown agent: {}", body.agent),
                None,
            ),
        );
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
        paste_delay_ms: 1200,
        format: nils_common::cli_contract::OutputFormat::Json,
    };
    match tokio::task::spawn_blocking(move || start_session(&context, args)).await {
        Ok(Ok(view)) => envelope_ok(
            "start",
            json!({ "machine": state.machine, "session": view.result }),
        ),
        Ok(Err(err)) => envelope_err("start", err),
        Err(_) => join_err("start"),
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
                return envelope_err(
                    "send",
                    CliError::usage("invalid-key", format!("unknown key: {name}"), None),
                );
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
        Ok(Ok(sent)) => envelope_ok("send", json!({ "machine": state.machine, "sent": sent })),
        Ok(Err(err)) => envelope_err("send", err),
        Err(_) => join_err("send"),
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
        Ok(Ok(result)) => envelope_ok(
            "delete",
            json!({ "machine": state.machine, "deleted": result }),
        ),
        Ok(Err(err)) => envelope_err("delete", err),
        Err(_) => join_err("delete"),
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
        Ok(Err(err)) => return envelope_err("serve", err),
        Err(_) => return join_err("serve"),
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

    // Stream live pane output via `tmux pipe-pane` into a private file we tail.
    let pipe_file = session_dir(&context, &record.id).join("attach.pipe");
    let _ = std::fs::write(&pipe_file, b"");
    let enabled = ProcessCommand::new(&tmux)
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
        .unwrap_or(false);
    let tail_task = enabled.then(|| tokio::spawn(tail_pipe(pipe_file.clone(), sender)));

    // Client -> pane: JSON control frames { text?, key?, keys?, resize{cols,rows} }.
    while let Some(Ok(message)) = receiver.next().await {
        match message {
            Message::Text(text) => {
                handle_input(&context, &tmux, &record, &target, text.as_str()).await;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    // Teardown: disable the pipe, stop the tail, remove the private file. The
    // tmux session itself stays alive (durability across disconnect).
    if let Some(handle) = tail_task {
        handle.abort();
    }
    let _ = ProcessCommand::new(&tmux)
        .arg("pipe-pane")
        .arg("-t")
        .arg(&target)
        .status();
    let _ = std::fs::remove_file(&pipe_file);
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

async fn handle_input(
    context: &CliContext,
    tmux: &Path,
    record: &crate::SessionRecord,
    target: &str,
    frame: &str,
) {
    let Ok(value) = serde_json::from_str::<Value>(frame) else {
        return;
    };

    if let Some(resize) = value.get("resize") {
        let cols = resize.get("cols").and_then(Value::as_u64);
        let rows = resize.get("rows").and_then(Value::as_u64);
        if let (Some(cols), Some(rows)) = (cols, rows) {
            let _ = ProcessCommand::new(tmux)
                .arg("resize-window")
                .arg("-t")
                .arg(target)
                .arg("-x")
                .arg(cols.to_string())
                .arg("-y")
                .arg(rows.to_string())
                .status();
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
        assert_eq!(body["schema_version"], "cli.agent-session.list.v1");
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
        assert!(deny_unauthorized(&st, &auth_headers(Some(TOKEN))).is_none());
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
        assert_eq!(body["schema_version"], "cli.agent-session.send.v1");
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
}

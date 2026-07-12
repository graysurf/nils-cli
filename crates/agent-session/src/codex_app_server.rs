//! Strict metadata-only projection of the Codex app-server v2 protocol.
//!
//! The interactive TUI's human-readable error text is intentionally ignored.
//! Auto-resume is armed only when the live protocol reports both an exact
//! `usageLimitExceeded` error and a matching terminal `failed` completion for
//! the same bound thread and turn.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use jiff::Timestamp;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    CliContext, CliError, SessionRecord, auto_resume::UsageSnapshot, display_path,
    ensure_private_dir, write_private_file, write_session_record,
};

pub(crate) const RUNTIME_KIND: &str = "codex_app_server";
pub(crate) const PROTOCOL_KEY: &str = "codex_app_server_protocol";
pub(crate) const PROTOCOL_VERSION: &str = "v2";
pub(crate) const SOCKET_KEY: &str = "codex_app_server_socket";
pub(crate) const THREAD_HANDOFF_KEY: &str = "codex_app_server_thread_handoff";
pub(crate) const THREAD_ATTACHED_KEY: &str = "codex_app_server_thread_attached";

const UNIX_SOCKET_PATH_BUDGET: usize = 100;
const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_SUBMISSION_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) fn configure_runtime(
    context: &CliContext,
    agent_bin: &Path,
    record: &mut SessionRecord,
    managed: bool,
) -> Result<(), CliError> {
    if !managed || record.agent != "codex" {
        return Ok(());
    }
    let preference = env::var("AGENT_SESSION_CODEX_RUNTIME").unwrap_or_else(|_| "auto".into());
    if !matches!(preference.as_str(), "auto" | "app-server") {
        return Ok(());
    }
    let forced = preference == "app-server";
    if !app_server_capability_available(agent_bin) {
        if forced {
            return Err(CliError::data(
                "codex-app-server-capability-unavailable",
                "installed Codex does not advertise app-server Unix listen support",
                None,
            ));
        }
        return Ok(());
    }
    let socket = match allocate_socket_path(&record.id) {
        Ok(socket) => socket,
        Err(_) if !forced => return Ok(()),
        Err(err) => return Err(err),
    };
    let runtime = record.runtime.as_mut().ok_or_else(|| {
        CliError::data(
            "runtime-id-missing",
            "session runtime is missing its launch metadata",
            Some(json!({ "id": record.id })),
        )
    })?;
    runtime.kind = RUNTIME_KIND.to_string();
    runtime
        .extra
        .insert(PROTOCOL_KEY.to_string(), json!(PROTOCOL_VERSION));
    runtime
        .extra
        .insert(SOCKET_KEY.to_string(), json!(display_path(&socket)));
    runtime.extra.insert(
        THREAD_HANDOFF_KEY.to_string(),
        json!(display_path(&socket.with_extension("thread"))),
    );
    runtime.extra.insert(
        THREAD_ATTACHED_KEY.to_string(),
        json!(display_path(&socket.with_extension("attached"))),
    );
    write_session_record(context, record)
}

fn app_server_capability_available(agent_bin: &Path) -> bool {
    let Ok(mut child) = Command::new(agent_bin)
        .args(["app-server", "--help"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    else {
        return false;
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_millis(250) => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                let pid = child.id();
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
                let _ = child.wait();
                return false;
            }
        }
    }
    let Ok(output) = child.wait_with_output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    [output.stdout, output.stderr]
        .into_iter()
        .any(|bytes| String::from_utf8_lossy(&bytes).contains("--listen <URL>"))
}

fn allocate_socket_path(id: &str) -> Result<PathBuf, CliError> {
    let runtime_root = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir())
        .ok_or_else(|| {
            CliError::runtime(
                "codex-app-server-runtime-dir-unavailable",
                "Codex app-server requires a private XDG_RUNTIME_DIR",
                None,
            )
        })?;
    let dir = runtime_root.join("agent-session");
    ensure_private_dir(&dir)?;
    let mut digest = Sha256::new();
    digest.update(id.as_bytes());
    let suffix = digest
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let path = dir.join(format!("cx-{suffix}.sock"));
    if path.as_os_str().as_encoded_bytes().len() > UNIX_SOCKET_PATH_BUDGET {
        return Err(CliError::runtime(
            "codex-app-server-socket-path-too-long",
            "XDG_RUNTIME_DIR is too long for a private Unix socket",
            None,
        ));
    }
    Ok(path)
}

pub(crate) fn launch_script() -> &'static str {
    r#"socket=$1
handoff=$2
attached=$3
agent=$4
cwd=$5
shift 5
rm -f -- "$socket" "$handoff" "$attached"
"$agent" app-server --listen "unix://$socket" </dev/null >/dev/null 2>&1 &
server=$!
cleanup() {
  kill "$server" 2>/dev/null || true
  wait "$server" 2>/dev/null || true
  rm -f -- "$socket" "$handoff" "$attached"
}
trap cleanup EXIT HUP INT TERM
i=0
while [ ! -S "$socket" ]; do
  if ! kill -0 "$server" 2>/dev/null; then
    exit 1
  fi
  i=$((i + 1))
  if [ "$i" -ge 100 ]; then
    exit 1
  fi
  sleep 0.05
done
i=0
while [ ! -s "$handoff" ]; do
  if ! kill -0 "$server" 2>/dev/null; then
    exit 1
  fi
  i=$((i + 1))
  if [ "$i" -ge 600 ]; then
    exit 1
  fi
  sleep 0.05
done
IFS= read -r thread_id < "$handoff"
rm -f -- "$handoff"
(umask 077 && : > "$attached")
"$agent" --remote "unix://$socket" resume "$thread_id" --cd "$cwd" --no-alt-screen "$@"
"#
}

pub(crate) fn runtime_is_supported(record: &SessionRecord) -> bool {
    record.agent == "codex"
        && record.runtime.as_ref().is_some_and(|runtime| {
            runtime.kind == RUNTIME_KIND
                && runtime.extra.get(PROTOCOL_KEY).and_then(Value::as_str) == Some(PROTOCOL_VERSION)
                && runtime
                    .extra
                    .get(SOCKET_KEY)
                    .and_then(Value::as_str)
                    .is_some_and(|socket| Path::new(socket).is_absolute())
                && runtime
                    .extra
                    .get(THREAD_HANDOFF_KEY)
                    .and_then(Value::as_str)
                    .is_some_and(|path| Path::new(path).is_absolute())
                && runtime
                    .extra
                    .get(THREAD_ATTACHED_KEY)
                    .and_then(Value::as_str)
                    .is_some_and(|path| Path::new(path).is_absolute())
        })
}

fn runtime_path<'a>(record: &'a SessionRecord, key: &str) -> Option<&'a Path> {
    runtime_is_supported(record).then(|| {
        record
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.extra.get(key))
            .and_then(Value::as_str)
            .map(Path::new)
    })?
}

pub(crate) fn thread_handoff_path(record: &SessionRecord) -> Option<&Path> {
    runtime_path(record, THREAD_HANDOFF_KEY)
}

pub(crate) fn thread_attached_path(record: &SessionRecord) -> Option<&Path> {
    runtime_path(record, THREAD_ATTACHED_KEY)
}

pub(crate) fn socket_path(record: &SessionRecord) -> Option<&str> {
    runtime_is_supported(record).then(|| {
        record
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.extra.get(SOCKET_KEY))
            .and_then(Value::as_str)
    })?
}

pub(crate) fn cleanup_runtime_files(record: &SessionRecord) -> Result<(), CliError> {
    if !runtime_is_supported(record) {
        return Ok(());
    }
    let socket = allocate_socket_path(&record.id)?;
    for path in [
        socket.clone(),
        socket.with_extension("thread"),
        socket.with_extension("attached"),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(CliError::runtime(
                    "codex-app-server-cleanup-failed",
                    format!("failed to remove a private Codex runtime file: {err}"),
                    None,
                ));
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageFailure {
    pub(crate) thread_id: String,
    pub(crate) turn_id: String,
}

#[derive(Debug)]
pub(crate) struct FailureReducer {
    thread_id: String,
    exhausted_turns: BTreeSet<String>,
    completed_turns: BTreeSet<String>,
}

impl FailureReducer {
    pub(crate) fn new(thread_id: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            exhausted_turns: BTreeSet::new(),
            completed_turns: BTreeSet::new(),
        }
    }

    pub(crate) fn ingest(&mut self, message: &Value) -> Option<UsageFailure> {
        match message.get("method").and_then(Value::as_str) {
            Some("error") => {
                let params = message.get("params")?;
                if params.get("threadId").and_then(Value::as_str) != Some(self.thread_id.as_str())
                    || params.get("willRetry").and_then(Value::as_bool) != Some(false)
                    || params
                        .pointer("/error/codexErrorInfo")
                        .and_then(Value::as_str)
                        != Some("usageLimitExceeded")
                {
                    return None;
                }
                let turn_id = params.get("turnId").and_then(Value::as_str)?;
                if !self.completed_turns.contains(turn_id) {
                    self.exhausted_turns.insert(turn_id.to_string());
                }
                None
            }
            Some("turn/completed") => {
                let params = message.get("params")?;
                if params.get("threadId").and_then(Value::as_str) != Some(self.thread_id.as_str())
                    || params.pointer("/turn/status").and_then(Value::as_str) != Some("failed")
                {
                    return None;
                }
                let turn_id = params.pointer("/turn/id").and_then(Value::as_str)?;
                self.completed_turns.insert(turn_id.to_string());
                let embedded_usage_exhaustion = params
                    .pointer("/turn/error/codexErrorInfo")
                    .and_then(Value::as_str)
                    == Some("usageLimitExceeded");
                (embedded_usage_exhaustion || self.exhausted_turns.remove(turn_id)).then(|| {
                    UsageFailure {
                        thread_id: self.thread_id.clone(),
                        turn_id: turn_id.to_string(),
                    }
                })
            }
            _ => None,
        }
    }
}

pub(crate) fn initialize_request(id: u64) -> Value {
    json!({
        "id": id,
        "method": "initialize",
        "params": {
            "clientInfo": { "name": "agent-session", "title": "agent-session", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": {
                "experimentalApi": true,
                "requestAttestation": false
            }
        }
    })
}

pub(crate) fn initialized_notification() -> Value {
    json!({ "method": "initialized" })
}

pub(crate) fn loaded_threads_request(id: u64) -> Value {
    json!({ "id": id, "method": "thread/loaded/list", "params": {} })
}

pub(crate) fn start_thread_request(id: u64, cwd: &str) -> Value {
    json!({ "id": id, "method": "thread/start", "params": { "cwd": cwd } })
}

pub(crate) fn bootstrap_thread_request(id: u64, thread_id: &str) -> Value {
    json!({
        "id": id,
        "method": "thread/shellCommand",
        "params": { "threadId": thread_id, "command": ":" }
    })
}

pub(crate) fn resume_thread_request(id: u64, thread_id: &str, cwd: &str) -> Value {
    json!({
        "id": id,
        "method": "thread/resume",
        "params": { "threadId": thread_id, "cwd": cwd }
    })
}

pub(crate) fn rate_limits_request(id: u64) -> Value {
    json!({ "id": id, "method": "account/rateLimits/read" })
}

pub(crate) fn continuation_request(id: u64, thread_id: &str, message: &str) -> Value {
    json!({
        "id": id,
        "method": "turn/start",
        "params": {
            "threadId": thread_id,
            "input": [{ "type": "text", "text": message, "text_elements": [] }]
        }
    })
}

pub(crate) fn loaded_thread_ids(result: &Value) -> Option<Vec<String>> {
    let data = result.get("data")?.as_array()?;
    data.iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect()
}

pub(crate) fn usage_snapshot(result: &Value) -> UsageSnapshot {
    let Some(snapshot) = result.get("rateLimits") else {
        return UsageSnapshot {
            authoritative: false,
            has_exhausted_windows: false,
            exhausted_reset_epochs: Vec::new(),
        };
    };
    let mut exhausted_reset_epochs = Vec::new();
    let mut has_exhausted_windows = false;
    let mut observed_window = false;
    for key in ["primary", "secondary"] {
        let Some(window) = snapshot.get(key).filter(|value| !value.is_null()) else {
            continue;
        };
        let Some(used_percent) = window.get("usedPercent").and_then(Value::as_f64) else {
            continue;
        };
        observed_window = true;
        if used_percent >= 100.0 {
            has_exhausted_windows = true;
            if let Some(epoch) = window.get("resetsAt").and_then(Value::as_i64) {
                exhausted_reset_epochs.push(epoch);
            }
        }
    }
    UsageSnapshot {
        authoritative: observed_window,
        has_exhausted_windows,
        exhausted_reset_epochs,
    }
}

#[derive(Clone)]
pub(crate) struct ControlHandle {
    sender: mpsc::Sender<ControlCommand>,
}

pub(crate) enum ControlCommand {
    Usage(oneshot::Sender<Result<UsageSnapshot, String>>),
    Continue {
        message: String,
        response: oneshot::Sender<Result<String, String>>,
    },
}

impl ControlHandle {
    pub(crate) async fn usage(&self) -> Result<UsageSnapshot, String> {
        let (response, receive) = oneshot::channel();
        self.sender
            .send(ControlCommand::Usage(response))
            .await
            .map_err(|_| "codex control connection unavailable".to_string())?;
        tokio::time::timeout(CONTROL_RESPONSE_TIMEOUT, receive)
            .await
            .map_err(|_| "codex rate-limit request timed out".to_string())?
            .map_err(|_| "codex control connection closed".to_string())?
    }

    pub(crate) async fn submit(&self, message: &str) -> Result<String, String> {
        let (response, receive) = oneshot::channel();
        self.sender
            .send(ControlCommand::Continue {
                message: message.to_string(),
                response,
            })
            .await
            .map_err(|_| "codex control connection unavailable".to_string())?;
        tokio::time::timeout(CONTROL_SUBMISSION_TIMEOUT, receive)
            .await
            .map_err(|_| "codex turn submission timed out".to_string())?
            .map_err(|_| "codex control connection closed".to_string())?
    }
}

pub(crate) fn control_channel() -> (ControlHandle, mpsc::Receiver<ControlCommand>) {
    let (sender, receive) = mpsc::channel(4);
    (ControlHandle { sender }, receive)
}

pub(crate) async fn run_control(
    context: CliContext,
    record: SessionRecord,
    mut commands: mpsc::Receiver<ControlCommand>,
) -> Result<(), String> {
    let socket = socket_path(&record)
        .map(PathBuf::from)
        .ok_or_else(|| "Codex app-server socket metadata is missing".to_string())?;
    let stream = connect_socket(&socket).await?;
    let (mut websocket, _) = tokio_tungstenite::client_async("ws://localhost", stream)
        .await
        .map_err(|err| format!("Codex app-server WebSocket handshake failed: {err}"))?;

    let mut request_id = 1_u64;
    send_json(&mut websocket, initialize_request(request_id)).await?;
    receive_response_with_timeout(&mut websocket, request_id, None, CONTROL_RESPONSE_TIMEOUT)
        .await
        .map_err(|err| format!("initialize failed: {err}"))?;
    send_json(&mut websocket, initialized_notification()).await?;

    request_id = request_id.saturating_add(1);
    send_json(&mut websocket, loaded_threads_request(request_id)).await?;
    let result =
        receive_response_with_timeout(&mut websocket, request_id, None, CONTROL_RESPONSE_TIMEOUT)
            .await
            .map_err(|err| format!("thread/loaded/list failed: {err}"))?;
    let ids = loaded_thread_ids(&result)
        .ok_or_else(|| "Codex loaded-thread response was malformed".to_string())?;
    let thread_id = match ids.as_slice() {
        [id] => {
            request_id = request_id.saturating_add(1);
            send_json(
                &mut websocket,
                resume_thread_request(request_id, id, &record.cwd),
            )
            .await?;
            receive_response_with_timeout(
                &mut websocket,
                request_id,
                None,
                CONTROL_RESPONSE_TIMEOUT,
            )
            .await
            .map_err(|err| format!("thread/resume failed: {err}"))?;
            id.clone()
        }
        [] => {
            request_id = request_id.saturating_add(1);
            send_json(
                &mut websocket,
                start_thread_request(request_id, &record.cwd),
            )
            .await?;
            let result = receive_response_with_timeout(
                &mut websocket,
                request_id,
                None,
                CONTROL_RESPONSE_TIMEOUT,
            )
            .await
            .map_err(|err| format!("thread/start failed: {err}"))?;
            let thread_id = result
                .pointer("/thread/id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| "Codex thread/start response omitted thread id".to_string())?;
            request_id = request_id.saturating_add(1);
            send_json(
                &mut websocket,
                bootstrap_thread_request(request_id, &thread_id),
            )
            .await?;
            receive_response_with_timeout(
                &mut websocket,
                request_id,
                None,
                CONTROL_RESPONSE_TIMEOUT,
            )
            .await
            .map_err(|err| format!("thread bootstrap failed: {err}"))?;
            tokio::time::timeout(
                CONTROL_SUBMISSION_TIMEOUT,
                receive_bootstrap_completion(&mut websocket, &thread_id),
            )
            .await
            .map_err(|_| "Codex bootstrap completion timed out".to_string())??;
            thread_id
        }
        _ => return Err("Codex app-server exposed more than one loaded thread".to_string()),
    };
    handoff_thread_if_needed(&record, &thread_id)?;
    let mut reducer = FailureReducer::new(thread_id.clone());

    // A daemon may reconnect after an earned/manual provider reset moved the
    // account ahead of the reset epoch captured in durable state. Re-read the
    // exact bound account once so that an existing scheduled claim becomes due
    // without waiting for a notification that happened while disconnected.
    request_id = request_id.saturating_add(1);
    send_json(&mut websocket, rate_limits_request(request_id)).await?;
    let initial_usage = receive_response_with_timeout(
        &mut websocket,
        request_id,
        Some((&context, &record, &mut reducer)),
        CONTROL_RESPONSE_TIMEOUT,
    )
    .await
    .map(|value| usage_snapshot(&value))?;
    wake_from_open_usage(&context, &record, &initial_usage).await?;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()); };
                request_id = request_id.saturating_add(1);
                match command {
                    ControlCommand::Usage(response) => {
                        if let Err(err) = send_json(&mut websocket, rate_limits_request(request_id)).await {
                            let _ = response.send(Err(err));
                            return Err("Codex usage request write failed".to_string());
                        }
                        let result = receive_response_with_timeout(
                            &mut websocket,
                            request_id,
                            Some((&context, &record, &mut reducer)),
                            CONTROL_RESPONSE_TIMEOUT,
                        ).await;
                        let _ = response.send(result.map(|value| usage_snapshot(&value)));
                    }
                    ControlCommand::Continue { message, response } => {
                        if let Err(err) = send_json(
                            &mut websocket,
                            continuation_request(request_id, &thread_id, &message),
                        ).await {
                            let _ = response.send(Err(err));
                            return Err("Codex continuation request write failed".to_string());
                        }
                        let result = receive_response_with_timeout(
                            &mut websocket,
                            request_id,
                            Some((&context, &record, &mut reducer)),
                            CONTROL_SUBMISSION_TIMEOUT,
                        ).await.and_then(|value| {
                            value.pointer("/turn/id")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                                .ok_or_else(|| "Codex turn/start response omitted the acknowledged turn id".to_string())
                        });
                        let _ = response.send(result);
                    }
                }
            }
            message = websocket.next() => {
                let value = decode_message(message).await?;
                process_live_message(&context, &record, &mut reducer, &value).await?;
            }
        }
    }
}

async fn receive_bootstrap_completion<S>(websocket: &mut S, thread_id: &str) -> Result<(), String>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let value = decode_message(websocket.next().await).await?;
        if value.get("method").and_then(Value::as_str) != Some("turn/completed")
            || value.pointer("/params/threadId").and_then(Value::as_str) != Some(thread_id)
        {
            continue;
        }
        return match value.pointer("/params/turn/status").and_then(Value::as_str) {
            Some("completed") => Ok(()),
            _ => Err("Codex bootstrap turn did not complete".to_string()),
        };
    }
}

fn handoff_thread_if_needed(record: &SessionRecord, thread_id: &str) -> Result<(), String> {
    let attached = thread_attached_path(record)
        .ok_or_else(|| "Codex attached marker metadata is missing".to_string())?;
    if attached.is_file() {
        return Ok(());
    }
    let handoff = thread_handoff_path(record)
        .ok_or_else(|| "Codex thread handoff metadata is missing".to_string())?;
    write_private_file(handoff, thread_id.as_bytes())
        .map_err(|err| format!("Codex thread handoff failed: {}", err.code()))
}

async fn connect_socket(path: &Path) -> Result<UnixStream, String> {
    let mut attempts = 0_u16;
    loop {
        match UnixStream::connect(path).await {
            Ok(stream) => return Ok(stream),
            Err(err) if attempts < 100 => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
                if attempts == 100 {
                    return Err(format!("Codex app-server socket unavailable: {err}"));
                }
            }
            Err(err) => return Err(format!("Codex app-server socket unavailable: {err}")),
        }
    }
}

async fn send_json<S>(websocket: &mut S, value: Value) -> Result<(), String>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    websocket
        .send(Message::Text(value.to_string().into()))
        .await
        .map_err(|err| format!("Codex app-server write failed: {err}"))
}

async fn receive_response_with_timeout<S>(
    websocket: &mut S,
    id: u64,
    live: Option<(&CliContext, &SessionRecord, &mut FailureReducer)>,
    timeout: Duration,
) -> Result<Value, String>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    tokio::time::timeout(timeout, receive_response(websocket, id, live))
        .await
        .map_err(|_| "Codex app-server request timed out".to_string())?
}

async fn receive_response<S>(
    websocket: &mut S,
    id: u64,
    mut live: Option<(&CliContext, &SessionRecord, &mut FailureReducer)>,
) -> Result<Value, String>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        let value = decode_message(websocket.next().await).await?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            if let Some(error) = value.get("error") {
                let category = error
                    .get("message")
                    .and_then(Value::as_str)
                    .map(protocol_error_category)
                    .unwrap_or("unknown");
                return Err(format!(
                    "Codex app-server rejected request: {} ({category})",
                    error
                        .get("code")
                        .and_then(Value::as_i64)
                        .unwrap_or_default()
                ));
            }
            return value
                .get("result")
                .cloned()
                .ok_or_else(|| "Codex app-server response omitted result".to_string());
        }
        if let Some((context, record, reducer)) = live.as_mut() {
            process_live_message(context, record, reducer, &value).await?;
        }
    }
}

fn protocol_error_category(message: &str) -> &'static str {
    for (needle, category) in [
        ("no rollout", "no_rollout"),
        ("already running", "already_running"),
        ("different rollout path", "rollout_path_mismatch"),
        ("stale path", "stale_rollout_path"),
        ("not found", "not_found"),
        ("missing field", "missing_field"),
        ("unknown field", "unknown_field"),
        ("invalid type", "invalid_type"),
        ("AbsolutePathBuf", "invalid_absolute_path"),
    ] {
        if message.contains(needle) {
            return category;
        }
    }
    "other"
}

async fn decode_message(
    message: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
) -> Result<Value, String> {
    let message = message
        .ok_or_else(|| "Codex app-server connection closed".to_string())?
        .map_err(|err| format!("Codex app-server read failed: {err}"))?;
    match message {
        Message::Text(text) => serde_json::from_str(&text)
            .map_err(|_| "Codex app-server emitted malformed JSON".to_string()),
        Message::Binary(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| "Codex app-server emitted malformed JSON".to_string()),
        Message::Close(_) => Err("Codex app-server connection closed".to_string()),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => Ok(json!({})),
    }
}

async fn process_live_message(
    context: &CliContext,
    record: &SessionRecord,
    reducer: &mut FailureReducer,
    value: &Value,
) -> Result<(), String> {
    if value.get("method").and_then(Value::as_str) == Some("account/rateLimits/updated") {
        let snapshot = value
            .get("params")
            .map(usage_snapshot)
            .unwrap_or(UsageSnapshot {
                authoritative: false,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
            });
        wake_from_open_usage(context, record, &snapshot).await?;
    }
    let Some(failure) = reducer.ingest(value) else {
        return Ok(());
    };
    let context = context.clone();
    let id = record.id.clone();
    let runtime_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
        .ok_or_else(|| "Codex runtime identity is missing".to_string())?;
    tokio::task::spawn_blocking(move || {
        crate::activity::ingest_codex_app_server_failure(
            &context,
            &id,
            &runtime_id,
            &failure.thread_id,
            &failure.turn_id,
        )
    })
    .await
    .map_err(|_| "Codex failure ingestion worker failed".to_string())?
    .map_err(|err| format!("Codex failure ingestion failed: {}", err.code()))?;
    Ok(())
}

async fn wake_from_open_usage(
    context: &CliContext,
    record: &SessionRecord,
    usage: &UsageSnapshot,
) -> Result<(), String> {
    if !usage.authoritative || usage.has_exhausted_windows {
        return Ok(());
    }
    let context = context.clone();
    let id = record.id.clone();
    tokio::task::spawn_blocking(move || {
        crate::auto_resume::wake_scheduled_if_usage_open(
            &context,
            &id,
            Timestamp::now().as_second(),
        )
    })
    .await
    .map_err(|_| "Codex usage wake worker failed".to_string())?
    .map(|_| ())
    .map_err(|err| format!("Codex usage wake failed: {}", err.code()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn record_with_runtime(id: &str, socket: &Path) -> SessionRecord {
        SessionRecord {
            schema_version: crate::SESSION_DOCUMENT_VERSION.to_string(),
            id: id.to_string(),
            agent: "codex".to_string(),
            mode: "interactive".to_string(),
            title: None,
            cwd: "/repo".to_string(),
            tmux_session: format!("hs-{id}"),
            prompt_file: None,
            log_file: None,
            created_at: "2030-01-01T00:00:00Z".to_string(),
            updated_at: "2030-01-01T00:00:00Z".to_string(),
            provider_resume: None,
            runtime: Some(crate::RuntimeInfo {
                kind: RUNTIME_KIND.to_string(),
                tmux_session: format!("hs-{id}"),
                generation: 1,
                started_at: "2030-01-01T00:00:00Z".to_string(),
                launch_id: format!("runtime-{id}"),
                extra: BTreeMap::from([
                    (PROTOCOL_KEY.to_string(), json!(PROTOCOL_VERSION)),
                    (SOCKET_KEY.to_string(), json!(display_path(socket))),
                    (
                        THREAD_HANDOFF_KEY.to_string(),
                        json!(display_path(&socket.with_extension("thread"))),
                    ),
                    (
                        THREAD_ATTACHED_KEY.to_string(),
                        json!(display_path(&socket.with_extension("attached"))),
                    ),
                ]),
            }),
            agent_args: Vec::new(),
            agent_bin: None,
            extra: BTreeMap::new(),
            resume_sidecar_extra: BTreeMap::new(),
        }
    }

    #[test]
    fn forced_runtime_still_requires_the_installed_capability() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let runtime_dir = tmp.path().join("run");
        fs::create_dir(&runtime_dir).unwrap();
        let _runtime_dir = EnvGuard::set(&lock, "XDG_RUNTIME_DIR", runtime_dir.to_str().unwrap());
        let _preference = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_RUNTIME", "app-server");
        let agent = tmp.path().join("codex");
        fs::write(&agent, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut record = record_with_runtime("forced-probe", &runtime_dir.join("placeholder"));
        record.runtime.as_mut().unwrap().kind = "tmux".to_string();
        record.runtime.as_mut().unwrap().extra.clear();

        let err = configure_runtime(&context, &agent, &mut record, true).unwrap_err();
        assert_eq!(err.code(), "codex-app-server-capability-unavailable");
        assert_eq!(record.runtime.unwrap().kind, "tmux");
    }

    #[test]
    fn explicit_cleanup_removes_only_the_derived_runtime_files() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let runtime_dir = tmp.path().join("run");
        fs::create_dir(&runtime_dir).unwrap();
        let _runtime_dir = EnvGuard::set(&lock, "XDG_RUNTIME_DIR", runtime_dir.to_str().unwrap());
        let socket = allocate_socket_path("cleanup-runtime").unwrap();
        let record = record_with_runtime("cleanup-runtime", &socket);
        for path in [
            socket.clone(),
            socket.with_extension("thread"),
            socket.with_extension("attached"),
        ] {
            fs::write(path, b"stale").unwrap();
        }
        let unrelated = socket.with_extension("unrelated");
        fs::write(&unrelated, b"keep").unwrap();

        cleanup_runtime_files(&record).unwrap();

        assert!(!socket.exists());
        assert!(!socket.with_extension("thread").exists());
        assert!(!socket.with_extension("attached").exists());
        assert!(unrelated.exists());
    }

    async fn receive_json<S>(socket: &mut S) -> Value
    where
        S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        let message = socket.next().await.unwrap().unwrap();
        serde_json::from_str(message.to_text().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn response_wait_is_bounded_for_reconnect() {
        let mut stream = futures_util::stream::pending::<
            Result<Message, tokio_tungstenite::tungstenite::Error>,
        >();
        let err = receive_response_with_timeout(&mut stream, 1, None, Duration::from_millis(5))
            .await
            .unwrap_err();
        assert_eq!(err, "Codex app-server request timed out");
    }

    async fn respond<S>(socket: &mut S, request: &Value, result: Value)
    where
        S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
    {
        socket
            .send(Message::Text(
                json!({ "id": request["id"], "result": result })
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    }

    #[test]
    fn reducer_requires_exact_error_and_matching_failed_completion() {
        let mut reducer = FailureReducer::new("thread-a");
        let error = json!({
            "method": "error",
            "params": {
                "threadId": "thread-a",
                "turnId": "turn-a",
                "willRetry": false,
                "error": { "message": "ignored", "codexErrorInfo": "usageLimitExceeded" }
            }
        });
        assert_eq!(reducer.ingest(&error), None);
        assert_eq!(
            reducer.ingest(&json!({
                "method": "turn/completed",
                "params": { "threadId": "thread-a", "turn": { "id": "turn-a", "status": "failed" } }
            })),
            Some(UsageFailure {
                thread_id: "thread-a".into(),
                turn_id: "turn-a".into()
            })
        );
        assert_eq!(
            reducer.ingest(&error),
            None,
            "a completed turn cannot be re-armed"
        );
    }

    #[test]
    fn reducer_fails_closed_for_wrong_thread_status_reason_retry_and_order() {
        for mutation in [
            json!({"threadId":"other","turnId":"turn-a","willRetry":false,"error":{"codexErrorInfo":"usageLimitExceeded"}}),
            json!({"threadId":"thread-a","turnId":"turn-a","willRetry":true,"error":{"codexErrorInfo":"usageLimitExceeded"}}),
            json!({"threadId":"thread-a","turnId":"turn-a","willRetry":false,"error":{"codexErrorInfo":"other"}}),
        ] {
            let mut reducer = FailureReducer::new("thread-a");
            assert_eq!(
                reducer.ingest(&json!({"method":"error","params":mutation})),
                None
            );
            assert_eq!(reducer.ingest(&json!({"method":"turn/completed","params":{"threadId":"thread-a","turn":{"id":"turn-a","status":"failed"}}})), None);
        }
        let mut reordered = FailureReducer::new("thread-a");
        assert_eq!(reordered.ingest(&json!({"method":"turn/completed","params":{"threadId":"thread-a","turn":{"id":"turn-a","status":"failed"}}})), None);
        assert_eq!(reordered.ingest(&json!({"method":"error","params":{"threadId":"thread-a","turnId":"turn-a","willRetry":false,"error":{"codexErrorInfo":"usageLimitExceeded"}}})), None);

        let mut embedded = FailureReducer::new("thread-a");
        assert_eq!(
            embedded.ingest(&json!({
                "method": "turn/completed",
                "params": {
                    "threadId": "thread-a",
                    "turn": {
                        "id": "turn-b",
                        "status": "failed",
                        "error": { "codexErrorInfo": "usageLimitExceeded" }
                    }
                }
            })),
            Some(UsageFailure {
                thread_id: "thread-a".into(),
                turn_id: "turn-b".into()
            })
        );
    }

    #[test]
    fn usage_projection_is_authoritative_only_for_well_formed_response() {
        assert!(!usage_snapshot(&json!({})).authoritative);
        assert!(!usage_snapshot(&json!({ "rateLimits": {} })).authoritative);
        let snapshot = usage_snapshot(&json!({
            "rateLimits": {
                "primary": { "usedPercent": 100.0, "resetsAt": 1_900_000_000 },
                "secondary": { "usedPercent": 42.0, "resetsAt": 1_900_000_100 }
            }
        }));
        assert!(snapshot.authoritative);
        assert!(snapshot.has_exhausted_windows);
        assert_eq!(snapshot.exhausted_reset_epochs, vec![1_900_000_000]);
    }

    #[test]
    fn exact_runtime_failure_never_arms_a_sibling_session() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let target = record_with_runtime("codex-target", &tmp.path().join("target.sock"));
        let sibling = record_with_runtime("codex-sibling", &tmp.path().join("sibling.sock"));
        for record in [&target, &sibling] {
            fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
            crate::write_session_record(&context, record).unwrap();
            crate::activity::activate_runtime(&context, record).unwrap();
            crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
                .unwrap();
        }

        let mut reducer = FailureReducer::new("target-thread");
        assert_eq!(
            reducer.ingest(&json!({
                "method": "error",
                "params": {
                    "threadId": "target-thread",
                    "turnId": "target-turn",
                    "willRetry": false,
                    "error": { "codexErrorInfo": "usageLimitExceeded" }
                }
            })),
            None
        );
        let failure = reducer
            .ingest(&json!({
                "method": "turn/completed",
                "params": {
                    "threadId": "target-thread",
                    "turn": { "id": "target-turn", "status": "failed" }
                }
            }))
            .unwrap();
        crate::activity::ingest_codex_app_server_failure(
            &context,
            &target.id,
            &target.runtime.as_ref().unwrap().launch_id,
            &failure.thread_id,
            &failure.turn_id,
        )
        .unwrap();

        assert_eq!(
            crate::auto_resume::pending_sessions(&context, 1_893_456_000)
                .unwrap()
                .usage_ids,
            vec![target.id]
        );
        let sibling_view = crate::auto_resume::view_for_record(&context, &sibling);
        assert!(sibling_view.enabled);
        assert_eq!(sibling_view.state, "enabled");
    }

    #[tokio::test]
    async fn unix_control_projects_live_failure_and_acknowledges_exact_turn_without_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("codex.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let id = "codex-control";
        fs::create_dir_all(crate::session_dir(&context, id)).unwrap();
        let record = SessionRecord {
            schema_version: crate::SESSION_DOCUMENT_VERSION.to_string(),
            id: id.to_string(),
            agent: "codex".to_string(),
            mode: "interactive".to_string(),
            title: None,
            cwd: "/repo".to_string(),
            tmux_session: "hs-codex-control".to_string(),
            prompt_file: None,
            log_file: None,
            created_at: "2030-01-01T00:00:00Z".to_string(),
            updated_at: "2030-01-01T00:00:00Z".to_string(),
            provider_resume: None,
            runtime: Some(crate::RuntimeInfo {
                kind: RUNTIME_KIND.to_string(),
                tmux_session: "hs-codex-control".to_string(),
                generation: 1,
                started_at: "2030-01-01T00:00:00Z".to_string(),
                launch_id: "runtime-control".to_string(),
                extra: BTreeMap::from([
                    (PROTOCOL_KEY.to_string(), json!(PROTOCOL_VERSION)),
                    (SOCKET_KEY.to_string(), json!(display_path(&socket))),
                    (
                        THREAD_HANDOFF_KEY.to_string(),
                        json!(display_path(&socket.with_extension("thread"))),
                    ),
                    (
                        THREAD_ATTACHED_KEY.to_string(),
                        json!(display_path(&socket.with_extension("attached"))),
                    ),
                ]),
            }),
            agent_args: Vec::new(),
            agent_bin: None,
            extra: BTreeMap::new(),
            resume_sidecar_extra: BTreeMap::new(),
        };
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, id, true, "2030-01-01T00:00:00Z").unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let initialize = receive_json(&mut socket).await;
            assert_eq!(initialize["method"], "initialize");
            respond(&mut socket, &initialize, json!({})).await;
            assert_eq!(receive_json(&mut socket).await["method"], "initialized");
            let loaded = receive_json(&mut socket).await;
            respond(
                &mut socket,
                &loaded,
                json!({ "data": [], "nextCursor": null }),
            )
            .await;
            let start = receive_json(&mut socket).await;
            assert_eq!(start["method"], "thread/start");
            assert_eq!(start["params"]["cwd"], "/repo");
            respond(
                &mut socket,
                &start,
                json!({ "thread": { "id": "raw-thread-a" } }),
            )
            .await;
            let bootstrap = receive_json(&mut socket).await;
            assert_eq!(bootstrap["method"], "thread/shellCommand");
            assert_eq!(bootstrap["params"]["command"], ":");
            respond(&mut socket, &bootstrap, json!({})).await;
            socket
                .send(Message::Text(
                    json!({
                        "method": "turn/completed",
                        "params": {
                            "threadId": "raw-thread-a",
                            "turn": { "id": "bootstrap-turn", "status": "completed" }
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "method": "error",
                        "params": {
                            "threadId": "raw-thread-a",
                            "turnId": "raw-turn-a",
                            "willRetry": false,
                            "error": {
                                "message": "localized secret human error",
                                "codexErrorInfo": "usageLimitExceeded"
                            }
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    json!({
                        "method": "turn/completed",
                        "params": {
                            "threadId": "raw-thread-a",
                            "turn": { "id": "raw-turn-a", "status": "failed" }
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let usage = receive_json(&mut socket).await;
            assert_eq!(usage["method"], "account/rateLimits/read");
            respond(
                &mut socket,
                &usage,
                json!({
                    "rateLimits": {
                        "primary": { "usedPercent": 100.0, "resetsAt": 1_900_000_000 },
                        "secondary": { "usedPercent": 10.0, "resetsAt": 1_900_000_100 }
                    }
                }),
            )
            .await;
            let explicit_usage = receive_json(&mut socket).await;
            assert_eq!(explicit_usage["method"], "account/rateLimits/read");
            respond(
                &mut socket,
                &explicit_usage,
                json!({
                    "rateLimits": {
                        "primary": { "usedPercent": 100.0, "resetsAt": 1_900_000_000 },
                        "secondary": { "usedPercent": 10.0, "resetsAt": 1_900_000_100 }
                    }
                }),
            )
            .await;
            let continuation = receive_json(&mut socket).await;
            assert_eq!(continuation["method"], "turn/start");
            assert_eq!(continuation["params"]["threadId"], "raw-thread-a");
            respond(
                &mut socket,
                &continuation,
                json!({ "turn": { "id": "acknowledged-turn", "status": "inProgress" } }),
            )
            .await;
        });

        let (handle, commands) = control_channel();
        let control_context = context.clone();
        let control_record = record.clone();
        let control =
            tokio::spawn(
                async move { run_control(control_context, control_record, commands).await },
            );
        let usage = handle.usage().await.unwrap();
        assert!(usage.authoritative);
        assert!(usage.has_exhausted_windows);
        assert_eq!(usage.exhausted_reset_epochs, vec![1_900_000_000]);
        assert_eq!(
            handle.submit("private continuation").await.unwrap(),
            "acknowledged-turn"
        );
        server.await.unwrap();
        drop(handle);
        control.abort();
        let _ = control.await;

        let session_dir = crate::session_dir(&context, id);
        let activity = format!(
            "{}\n{}",
            fs::read_to_string(session_dir.join("activity.json")).unwrap(),
            fs::read_to_string(session_dir.join("activity.journal.jsonl")).unwrap()
        );
        assert!(activity.contains("provider_protocol"));
        assert!(activity.contains("usage_exhausted"));
        for secret in [
            "raw-thread-a",
            "raw-turn-a",
            "localized secret human error",
            "private continuation",
        ] {
            assert!(!activity.contains(secret));
        }
        assert_eq!(
            crate::auto_resume::pending_sessions(&context, 1_893_456_000)
                .unwrap()
                .usage_ids,
            vec![id.to_string()]
        );
    }
}

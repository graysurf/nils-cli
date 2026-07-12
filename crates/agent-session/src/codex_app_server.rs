//! Strict metadata-only projection of the Codex app-server v2 protocol.
//!
//! The interactive TUI's human-readable error text is intentionally ignored.
//! Auto-resume is armed only when the live protocol reports both an exact
//! `usageLimitExceeded` error and a matching terminal `failed` completion for
//! the same bound thread and turn.

use std::collections::{BTreeSet, VecDeque};
use std::env;
use std::fs;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
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
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

use crate::{
    CliContext, CliError, SessionRecord, auto_resume::UsageSnapshot, display_path,
    write_private_file, write_session_record,
};

pub(crate) const RUNTIME_KIND: &str = "codex_app_server";
pub(crate) const PROTOCOL_KEY: &str = "codex_app_server_protocol";
pub(crate) const PROTOCOL_VERSION: &str = "v2";
pub(crate) const SOCKET_KEY: &str = "codex_app_server_socket";
pub(crate) const PROXY_KEY: &str = "codex_app_server_proxy";
pub(crate) const THREAD_HANDOFF_KEY: &str = "codex_app_server_thread_handoff";
pub(crate) const THREAD_ATTACHED_KEY: &str = "codex_app_server_thread_attached";

const UNIX_SOCKET_PATH_BUDGET: usize = 100;
const MAX_PROTOCOL_ID_BYTES: usize = 256;
const MAX_REDUCER_PENDING_TURNS: usize = 64;
const MAX_PROXY_OBSERVATIONS: usize = 16;
const MAX_PROXY_OBSERVATION_BYTES: usize = 64 * 1024;
const MAX_PROXY_RAW_OBSERVATION_BYTES: usize = 256 * 1024;
const MAX_PROXY_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROXY_FRAME_BYTES: usize = 4 * 1024 * 1024;
const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_SUBMISSION_TIMEOUT: Duration = Duration::from_secs(15);
const CONTROL_SUBMIT_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
const MINIMUM_AUDITED_CODEX_VERSION: (u64, u64, u64) = (0, 144, 1);

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
    let socket = match allocate_socket_path(context, record) {
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
        PROXY_KEY.to_string(),
        json!(display_path(&socket.with_extension("proxy"))),
    );
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
    let Some(version) = bounded_command_output(agent_bin, &["--version"]) else {
        return false;
    };
    let version_text = String::from_utf8_lossy(&version.stdout);
    if parse_version_triplet(&version_text)
        .is_none_or(|version| version < MINIMUM_AUDITED_CODEX_VERSION)
    {
        return false;
    }
    let Some(output) = bounded_command_output(agent_bin, &["app-server", "--help"]) else {
        return false;
    };
    [output.stdout, output.stderr].into_iter().any(|bytes| {
        let text = String::from_utf8_lossy(&bytes);
        text.contains("--listen <URL>") && text.contains("unix://")
    })
}

fn bounded_command_output(agent_bin: &Path, args: &[&str]) -> Option<std::process::Output> {
    let Ok(mut child) = Command::new(agent_bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    else {
        return None;
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
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output)
}

fn parse_version_triplet(raw: &str) -> Option<(u64, u64, u64)> {
    raw.split_whitespace().find_map(|token| {
        let token = token.trim_matches(|ch: char| !ch.is_ascii_digit() && ch != '.');
        let mut parts = token.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        Some((major, minor, patch))
    })
}

fn runtime_namespace(context: &CliContext, record: &SessionRecord) -> Result<String, CliError> {
    let launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|launch_id| !launch_id.is_empty())
        .ok_or_else(|| {
            CliError::data(
                "runtime-id-missing",
                "session runtime is missing its launch metadata",
                Some(json!({ "id": record.id })),
            )
        })?;
    let mut digest = Sha256::new();
    digest.update(context.state_dir.as_os_str().as_bytes());
    digest.update([0]);
    digest.update(record.id.as_bytes());
    digest.update([0]);
    digest.update(launch_id.as_bytes());
    Ok(digest
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn validate_private_runtime_dir(path: &Path) -> Result<(), CliError> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        CliError::runtime(
            "codex-app-server-runtime-dir-unavailable",
            format!("Codex app-server runtime directory is unavailable: {err}"),
            None,
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(CliError::runtime(
            "codex-app-server-runtime-dir-unsafe",
            "Codex app-server requires an owned, non-symlinked 0700 runtime directory",
            None,
        ));
    }
    Ok(())
}

fn private_runtime_dir() -> Result<PathBuf, CliError> {
    let runtime_root = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            CliError::runtime(
                "codex-app-server-runtime-dir-unavailable",
                "Codex app-server requires a private XDG_RUNTIME_DIR",
                None,
            )
        })?;
    validate_private_runtime_dir(&runtime_root)?;
    let dir = runtime_root.join("agent-session");
    match fs::create_dir(&dir) {
        Ok(()) => fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|err| {
            CliError::runtime(
                "codex-app-server-runtime-dir-unavailable",
                format!("failed to secure the Codex app-server runtime directory: {err}"),
                None,
            )
        })?,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => {
            return Err(CliError::runtime(
                "codex-app-server-runtime-dir-unavailable",
                format!("failed to create the Codex app-server runtime directory: {err}"),
                None,
            ));
        }
    }
    validate_private_runtime_dir(&dir)?;
    Ok(dir)
}

fn allocate_socket_path(context: &CliContext, record: &SessionRecord) -> Result<PathBuf, CliError> {
    let dir = private_runtime_dir()?;
    let suffix = runtime_namespace(context, record)?;
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

fn persisted_runtime_paths(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<[PathBuf; 4], CliError> {
    let socket = socket_path(record).map(PathBuf::from).ok_or_else(|| {
        CliError::data(
            "codex-app-server-runtime-path-invalid",
            "Codex app-server socket metadata is missing",
            None,
        )
    })?;
    let proxy = proxy_path(record).map(PathBuf::from).ok_or_else(|| {
        CliError::data(
            "codex-app-server-runtime-path-invalid",
            "Codex app-server proxy metadata is missing",
            None,
        )
    })?;
    let handoff = thread_handoff_path(record)
        .map(PathBuf::from)
        .ok_or_else(|| {
            CliError::data(
                "codex-app-server-runtime-path-invalid",
                "Codex app-server handoff metadata is missing",
                None,
            )
        })?;
    let attached = thread_attached_path(record)
        .map(PathBuf::from)
        .ok_or_else(|| {
            CliError::data(
                "codex-app-server-runtime-path-invalid",
                "Codex app-server attached metadata is missing",
                None,
            )
        })?;
    let expected_name = format!("cx-{}.sock", runtime_namespace(context, record)?);
    let valid = socket.file_name().and_then(|name| name.to_str()) == Some(expected_name.as_str())
        && socket
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("agent-session")
        && proxy == socket.with_extension("proxy")
        && handoff == socket.with_extension("thread")
        && attached == socket.with_extension("attached");
    if !valid {
        return Err(CliError::data(
            "codex-app-server-runtime-path-invalid",
            "Codex app-server runtime paths do not match the session runtime identity",
            None,
        ));
    }
    Ok([socket, proxy, handoff, attached])
}

pub(crate) fn launch_script() -> &'static str {
    r#"socket=$1
proxy=$2
handoff=$3
attached=$4
proxy_bin=$5
state_dir=$6
session_id=$7
agent=$8
cwd=$9
shift 9
rm -f -- "$socket" "$proxy" "$attached"
"$agent" app-server --listen "unix://$socket" </dev/null >/dev/null 2>&1 &
server=$!
proxy_pid=
cleanup() {
  if [ -n "$proxy_pid" ]; then
    kill "$proxy_pid" 2>/dev/null || true
    wait "$proxy_pid" 2>/dev/null || true
  fi
  kill "$server" 2>/dev/null || true
  wait "$server" 2>/dev/null || true
  rm -f -- "$socket" "$proxy" "$handoff" "$attached"
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
(umask 077; "$proxy_bin" --state-dir "$state_dir" codex-app-server-proxy --id "$session_id" --upstream "$socket" --listen "$proxy" </dev/null >/dev/null 2>&1) &
proxy_pid=$!
i=0
while [ ! -S "$proxy" ]; do
  if ! kill -0 "$proxy_pid" 2>/dev/null; then
    exit 1
  fi
  i=$((i + 1))
  if [ "$i" -ge 100 ]; then
    exit 1
  fi
  sleep 0.05
done
"$agent" --remote "unix://$proxy" --cd "$cwd" --no-alt-screen "$@"
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
                    .get(PROXY_KEY)
                    .and_then(Value::as_str)
                    .is_some_and(|proxy| Path::new(proxy).is_absolute())
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

pub(crate) struct CreateBootstrapGuard {
    path: PathBuf,
    file: fs::File,
}

impl Drop for CreateBootstrapGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl CreateBootstrapGuard {
    pub(crate) fn finish(self, release_lifecycle_lock: impl FnOnce()) {
        if lock_bootstrap_file(&self.file) {
            release_lifecycle_lock();
            let _ = fs::remove_file(&self.path);
            unlock_bootstrap_file(&self.file);
        } else {
            // Lock failure must sacrifice bootstrap availability, never expose
            // a marker that can authorize arbitrary record-lock contention.
            let _ = fs::remove_file(&self.path);
            release_lifecycle_lock();
        }
    }
}

struct CreateBootstrapGate {
    file: fs::File,
}

impl Drop for CreateBootstrapGate {
    fn drop(&mut self) {
        unlock_bootstrap_file(&self.file);
    }
}

fn lock_bootstrap_file(file: &fs::File) -> bool {
    loop {
        // SAFETY: `flock` observes the valid descriptor borrowed for this call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return true;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return false;
        }
    }
}

fn unlock_bootstrap_file(file: &fs::File) {
    // SAFETY: `flock` observes the valid descriptor borrowed for this call.
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
}

pub(crate) fn begin_create_bootstrap(
    record: &SessionRecord,
) -> Result<Option<CreateBootstrapGuard>, CliError> {
    if !runtime_is_supported(record) {
        return Ok(None);
    }
    let path = thread_handoff_path(record)
        .map(PathBuf::from)
        .ok_or_else(|| {
            CliError::data(
                "codex-app-server-handoff-missing",
                "Codex app-server runtime is missing its create bootstrap marker",
                Some(json!({ "id": record.id })),
            )
        })?;
    let launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_bytes())
        .ok_or_else(|| {
            CliError::data(
                "runtime-id-missing",
                "session runtime is missing its launch metadata",
                Some(json!({ "id": record.id })),
            )
        })?;
    write_private_file(&path, launch_id)?;
    let file = fs::File::open(&path).map_err(|err| {
        CliError::runtime(
            "codex-app-server-handoff-open-failed",
            format!("failed to open the create bootstrap marker: {err}"),
            Some(json!({ "id": record.id })),
        )
    })?;
    Ok(Some(CreateBootstrapGuard { path, file }))
}

fn create_bootstrap_is_live(record: &SessionRecord) -> bool {
    #[cfg(test)]
    BOOTSTRAP_LIVE_CHECKS.with(|checks| checks.set(checks.get() + 1));
    let Some(path) = thread_handoff_path(record) else {
        return false;
    };
    let Some(runtime) = record.runtime.as_ref() else {
        return false;
    };
    fs::read(path).is_ok_and(|bytes| bytes == runtime.launch_id.as_bytes())
}

fn acquire_create_bootstrap_gate(record: &SessionRecord) -> Option<CreateBootstrapGate> {
    let path = thread_handoff_path(record)?;
    let mut file = fs::File::open(path).ok()?;
    #[cfg(test)]
    BOOTSTRAP_GATE_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if !lock_bootstrap_file(&file) {
        return None;
    }
    let own = file.metadata().ok()?;
    let current = fs::metadata(path).ok()?;
    if own.dev() != current.dev() || own.ino() != current.ino() {
        return None;
    }
    let mut token = Vec::new();
    file.read_to_end(&mut token).ok()?;
    let runtime = record.runtime.as_ref()?;
    (token == runtime.launch_id.as_bytes()).then_some(CreateBootstrapGate { file })
}

#[cfg(test)]
std::thread_local! {
    static BOOTSTRAP_LIVE_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
static NORMAL_CANCELLATION_ATTEMPTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
static BOOTSTRAP_GATE_ATTEMPTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

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

pub(crate) fn proxy_path(record: &SessionRecord) -> Option<&Path> {
    runtime_path(record, PROXY_KEY)
}

pub(crate) fn cleanup_runtime_files(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<(), CliError> {
    if !runtime_is_supported(record) {
        return Ok(());
    }
    for path in persisted_runtime_paths(context, record)? {
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
    exhausted_order: VecDeque<String>,
    completed_turns: BTreeSet<String>,
    completed_order: VecDeque<String>,
}

impl FailureReducer {
    pub(crate) fn new(thread_id: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            exhausted_turns: BTreeSet::new(),
            exhausted_order: VecDeque::new(),
            completed_turns: BTreeSet::new(),
            completed_order: VecDeque::new(),
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
                let turn_id = params
                    .get("turnId")
                    .and_then(Value::as_str)
                    .filter(|turn_id| protocol_id_is_valid(turn_id))?;
                if !self.completed_turns.contains(turn_id) {
                    insert_bounded_id(
                        &mut self.exhausted_turns,
                        &mut self.exhausted_order,
                        turn_id,
                    );
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
                let turn_id = params
                    .pointer("/turn/id")
                    .and_then(Value::as_str)
                    .filter(|turn_id| protocol_id_is_valid(turn_id))?;
                let embedded_usage_exhaustion = params
                    .pointer("/turn/error/codexErrorInfo")
                    .and_then(Value::as_str)
                    == Some("usageLimitExceeded");
                let matched_error = remove_bounded_id(
                    &mut self.exhausted_turns,
                    &mut self.exhausted_order,
                    turn_id,
                );
                insert_bounded_id(
                    &mut self.completed_turns,
                    &mut self.completed_order,
                    turn_id,
                );
                (embedded_usage_exhaustion || matched_error).then(|| UsageFailure {
                    thread_id: self.thread_id.clone(),
                    turn_id: turn_id.to_string(),
                })
            }
            _ => None,
        }
    }
}

fn insert_bounded_id(set: &mut BTreeSet<String>, order: &mut VecDeque<String>, id: &str) {
    if set.contains(id) {
        return;
    }
    while set.len() >= MAX_REDUCER_PENDING_TURNS {
        let Some(oldest) = order.pop_front() else {
            break;
        };
        set.remove(&oldest);
    }
    let owned = id.to_string();
    set.insert(owned.clone());
    order.push_back(owned);
}

fn remove_bounded_id(set: &mut BTreeSet<String>, order: &mut VecDeque<String>, id: &str) -> bool {
    if !set.remove(id) {
        return false;
    }
    if let Some(index) = order.iter().position(|item| item == id) {
        order.remove(index);
    }
    true
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
        .map(|value| {
            value
                .as_str()
                .filter(|id| protocol_id_is_valid(id))
                .map(str::to_string)
        })
        .collect()
}

fn protocol_id_is_valid(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_PROTOCOL_ID_BYTES
}

pub(crate) fn usage_snapshot(result: &Value) -> UsageSnapshot {
    let Some(legacy_snapshot) = result.get("rateLimits").filter(|value| value.is_object()) else {
        return UsageSnapshot {
            authoritative: false,
            has_exhausted_windows: false,
            exhausted_reset_epochs: Vec::new(),
        };
    };
    let mut exhausted_reset_epochs = Vec::new();
    let mut has_exhausted_windows = false;
    let mut observed_window = false;
    let mut snapshots = vec![legacy_snapshot];
    match result.get("rateLimitsByLimitId") {
        None | Some(Value::Null) => {}
        Some(Value::Object(by_limit_id)) => snapshots.extend(by_limit_id.values()),
        Some(_) => {
            return UsageSnapshot {
                authoritative: false,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
            };
        }
    }
    for snapshot in snapshots {
        if !snapshot.is_object() {
            return UsageSnapshot {
                authoritative: false,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
            };
        }
        for key in ["primary", "secondary"] {
            let Some(window) = snapshot.get(key).filter(|value| !value.is_null()) else {
                continue;
            };
            let Some(used_percent) = window.get("usedPercent").and_then(Value::as_f64) else {
                return UsageSnapshot {
                    authoritative: false,
                    has_exhausted_windows: false,
                    exhausted_reset_epochs: Vec::new(),
                };
            };
            observed_window = true;
            if used_percent >= 100.0 {
                has_exhausted_windows = true;
                if let Some(epoch) = window.get("resetsAt").and_then(Value::as_i64) {
                    exhausted_reset_epochs.push(epoch);
                }
            }
        }
    }
    exhausted_reset_epochs.sort_unstable();
    exhausted_reset_epochs.dedup();
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
        tokio::time::timeout(CONTROL_RESPONSE_TIMEOUT, async {
            self.sender
                .send(ControlCommand::Usage(response))
                .await
                .map_err(|_| "codex control connection unavailable".to_string())?;
            receive
                .await
                .map_err(|_| "codex control connection closed".to_string())?
        })
        .await
        .map_err(|_| "codex rate-limit request timed out".to_string())?
    }

    pub(crate) async fn submit(&self, message: &str) -> Result<String, String> {
        let (response, receive) = oneshot::channel();
        tokio::time::timeout(CONTROL_SUBMIT_TOTAL_TIMEOUT, async {
            self.sender
                .send(ControlCommand::Continue {
                    message: message.to_string(),
                    response,
                })
                .await
                .map_err(|_| "codex control connection unavailable".to_string())?;
            receive
                .await
                .map_err(|_| "codex control connection closed".to_string())?
        })
        .await
        .map_err(|_| "codex turn submission timed out".to_string())?
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
    let (mut websocket, _) = tokio::time::timeout(
        CONTROL_RESPONSE_TIMEOUT,
        tokio_tungstenite::client_async("ws://localhost", stream),
    )
    .await
    .map_err(|_| "Codex app-server WebSocket handshake timed out".to_string())?
    .map_err(|err| format!("Codex app-server WebSocket handshake failed: {err}"))?;

    let mut request_id = 1_u64;
    send_json(&mut websocket, initialize_request(request_id)).await?;
    receive_response_with_timeout(&mut websocket, request_id, None, CONTROL_RESPONSE_TIMEOUT)
        .await
        .map_err(|err| format!("initialize failed: {err}"))?;
    send_json(&mut websocket, initialized_notification()).await?;

    let mut discovery_attempts = 0_u8;
    let thread_id = loop {
        request_id = request_id.saturating_add(1);
        send_json(&mut websocket, loaded_threads_request(request_id)).await?;
        let result = receive_response_with_timeout(
            &mut websocket,
            request_id,
            None,
            CONTROL_RESPONSE_TIMEOUT,
        )
        .await
        .map_err(|err| format!("thread/loaded/list failed: {err}"))?;
        let ids = loaded_thread_ids(&result)
            .ok_or_else(|| "Codex loaded-thread response was malformed".to_string())?;
        match ids.as_slice() {
            [id] => break id.clone(),
            [] if discovery_attempts < 100 => {
                discovery_attempts = discovery_attempts.saturating_add(1);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            [] => return Err("Codex TUI did not create a loaded thread".to_string()),
            _ => return Err("Codex app-server exposed more than one loaded thread".to_string()),
        }
    };
    let reconnecting = thread_attached_path(&record).is_some_and(Path::is_file);
    bind_thread(&record, &thread_id)?;
    let mut thread_resumed = false;
    if reconnecting {
        request_id = request_id.saturating_add(1);
        send_json(
            &mut websocket,
            resume_thread_request(request_id, &thread_id, &record.cwd),
        )
        .await?;
        match receive_response_with_timeout(
            &mut websocket,
            request_id,
            None,
            CONTROL_RESPONSE_TIMEOUT,
        )
        .await
        {
            Ok(_) => thread_resumed = true,
            Err(err) if err.ends_with("(no_rollout)") => {}
            Err(err) => return Err(format!("thread/resume failed: {err}")),
        }
    }
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
                match command {
                    ControlCommand::Usage(response) => {
                        request_id = request_id.saturating_add(1);
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
                        if !thread_resumed {
                            request_id = request_id.saturating_add(1);
                            if let Err(err) = send_json(
                                &mut websocket,
                                resume_thread_request(request_id, &thread_id, &record.cwd),
                            ).await {
                                let _ = response.send(Err(err));
                                return Err("Codex continuation resume write failed".to_string());
                            }
                            if let Err(err) = receive_response_with_timeout(
                                &mut websocket,
                                request_id,
                                Some((&context, &record, &mut reducer)),
                                CONTROL_RESPONSE_TIMEOUT,
                            ).await {
                                let _ = response.send(Err(err));
                                continue;
                            }
                            thread_resumed = true;
                        }
                        request_id = request_id.saturating_add(1);
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

fn bind_thread(record: &SessionRecord, thread_id: &str) -> Result<(), String> {
    let attached = thread_attached_path(record)
        .ok_or_else(|| "Codex attached marker metadata is missing".to_string())?;
    if attached.is_file() {
        let observed = fs::read_to_string(attached)
            .map_err(|_| "Codex attached thread binding was unreadable".to_string())?;
        return (observed == projected_thread_binding(thread_id))
            .then_some(())
            .ok_or_else(|| "Codex loaded thread did not match the attached runtime".to_string());
    }
    write_private_file(attached, projected_thread_binding(thread_id).as_bytes())
        .map_err(|err| format!("Codex thread binding failed: {}", err.code()))
}

fn projected_thread_binding(thread_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"agent-session-codex-thread-v1\0");
    digest.update(thread_id.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn run_proxy(context: &CliContext, args: crate::cli::CodexAppServerProxyArgs) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("error: failed to start Codex app-server proxy runtime: {err}");
            return nils_common::cli_contract::exit::RUNTIME;
        }
    };
    match runtime.block_on(run_proxy_session(context.clone(), args)) {
        Ok(()) => nils_common::cli_contract::exit::SUCCESS,
        Err(err) => {
            eprintln!("error: Codex app-server proxy failed: {err}");
            nils_common::cli_contract::exit::RUNTIME
        }
    }
}

struct ProxySocketGuard(PathBuf);

impl Drop for ProxySocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct ProxyObserver {
    pending_thread_starts: BTreeSet<String>,
    reducer: Option<FailureReducer>,
}

impl ProxyObserver {
    fn new() -> Self {
        Self {
            pending_thread_starts: BTreeSet::new(),
            reducer: None,
        }
    }

    fn observe_client(&mut self, record: &SessionRecord, value: &Value) -> Result<(), String> {
        match value.get("method").and_then(Value::as_str) {
            Some("thread/start") => {
                if let Some(key) = value.get("id").and_then(json_id_key) {
                    if self.pending_thread_starts.len() >= MAX_REDUCER_PENDING_TURNS
                        && !self.pending_thread_starts.contains(&key)
                        && let Some(oldest) = self.pending_thread_starts.iter().next().cloned()
                    {
                        self.pending_thread_starts.remove(&oldest);
                    }
                    self.pending_thread_starts.insert(key);
                }
            }
            Some("turn/start") => {
                if let Some(thread_id) = value
                    .pointer("/params/threadId")
                    .and_then(Value::as_str)
                    .filter(|id| protocol_id_is_valid(id))
                {
                    self.bind(record, thread_id)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn observe_server(
        &mut self,
        context: &CliContext,
        record: &SessionRecord,
        value: &Value,
    ) -> Result<(), String> {
        if let Some(key) = value.get("id").and_then(json_id_key)
            && self.pending_thread_starts.remove(&key)
            && let Some(thread_id) = value
                .pointer("/result/thread/id")
                .and_then(Value::as_str)
                .filter(|id| protocol_id_is_valid(id))
        {
            self.bind(record, thread_id)?;
        }
        if let Some(reducer) = self.reducer.as_mut() {
            process_live_message(context, record, reducer, value).await?;
        }
        Ok(())
    }

    fn bind(&mut self, record: &SessionRecord, thread_id: &str) -> Result<(), String> {
        if let Some(reducer) = self.reducer.as_ref() {
            return (reducer.thread_id == thread_id)
                .then_some(())
                .ok_or_else(|| "Codex TUI proxy switched to a different thread".to_string());
        }
        bind_thread(record, thread_id)?;
        self.reducer = Some(FailureReducer::new(thread_id));
        Ok(())
    }
}

fn json_id_key(value: &Value) -> Option<String> {
    match value {
        Value::String(id) if id.len() <= MAX_PROTOCOL_ID_BYTES => Some(value.to_string()),
        Value::Number(_) => Some(value.to_string()),
        _ => None,
    }
}

fn message_value(message: &Message) -> Option<Value> {
    match message {
        Message::Text(text) => serde_json::from_str(text).ok(),
        Message::Binary(bytes) => serde_json::from_slice(bytes).ok(),
        _ => None,
    }
}

fn message_payload_len(message: &Message) -> usize {
    match message {
        Message::Text(text) => text.len(),
        Message::Binary(bytes) | Message::Ping(bytes) | Message::Pong(bytes) => bytes.len(),
        Message::Close(_) | Message::Frame(_) => 0,
    }
}

enum ProxyObservation {
    Client(Value),
    Server(Message),
}

struct ProxyProjection {
    sender: Option<mpsc::Sender<ProxyObservation>>,
    task: Option<tokio::task::JoinHandle<()>>,
    context: CliContext,
    record: SessionRecord,
}

impl ProxyProjection {
    fn new(context: CliContext, record: SessionRecord) -> Self {
        let (sender, mut receiver) = mpsc::channel(MAX_PROXY_OBSERVATIONS);
        let worker_context = context.clone();
        let worker_record = record.clone();
        let task = tokio::spawn(async move {
            let mut observer = ProxyObserver::new();
            while let Some(observation) = receiver.recv().await {
                let result = match observation {
                    ProxyObservation::Client(value) => {
                        observer.observe_client(&worker_record, &value)
                    }
                    ProxyObservation::Server(message) => {
                        match message_value(&message).and_then(|value| server_observation(&value)) {
                            Some(value) => {
                                observer
                                    .observe_server(&worker_context, &worker_record, &value)
                                    .await
                            }
                            None => Ok(()),
                        }
                    }
                };
                if result.is_err() {
                    fail_closed_projection(&worker_context, &worker_record).await;
                    break;
                }
            }
        });
        Self {
            sender: Some(sender),
            task: Some(task),
            context,
            record,
        }
    }

    fn observe_client(&mut self, value: &Value) {
        if let Some(value) = client_observation(value) {
            self.enqueue(ProxyObservation::Client(value));
        }
    }

    fn observe_server(&mut self, message: &Message) {
        if message_payload_len(message) <= MAX_PROXY_RAW_OBSERVATION_BYTES {
            self.enqueue(ProxyObservation::Server(message.clone()));
        } else {
            self.disable();
        }
    }

    fn enqueue(&mut self, observation: ProxyObservation) {
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        if sender.try_send(observation).is_err() {
            self.disable();
        }
    }

    fn disable(&mut self) {
        self.sender = None;
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let context = self.context.clone();
        let record = self.record.clone();
        tokio::spawn(async move { fail_closed_projection(&context, &record).await });
    }

    async fn finish(&mut self) {
        self.sender = None;
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(CONTROL_RESPONSE_TIMEOUT, &mut task)
                .await
                .is_err()
        {
            task.abort();
        }
    }
}

impl Drop for ProxyProjection {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn client_observation(value: &Value) -> Option<Value> {
    let observation = match value.get("method").and_then(Value::as_str)? {
        "thread/start" => {
            let id = value.get("id")?;
            json_id_key(id)?;
            json!({ "id": id, "method": "thread/start" })
        }
        "turn/start" => {
            let thread_id = value.pointer("/params/threadId")?.as_str()?;
            if !protocol_id_is_valid(thread_id) {
                return None;
            }
            json!({
                "method": "turn/start",
                "params": { "threadId": thread_id }
            })
        }
        _ => return None,
    };
    bounded_observation(observation)
}

fn server_observation(value: &Value) -> Option<Value> {
    if let (Some(id), Some(thread_id)) = (value.get("id"), value.pointer("/result/thread/id")) {
        json_id_key(id)?;
        let thread_id = thread_id.as_str()?;
        if !protocol_id_is_valid(thread_id) {
            return None;
        }
        return bounded_observation(json!({
            "id": id,
            "result": { "thread": { "id": thread_id } }
        }));
    }
    let observation = match value.get("method").and_then(Value::as_str)? {
        "error" => {
            let thread_id = value.pointer("/params/threadId")?.as_str()?;
            let turn_id = value.pointer("/params/turnId")?.as_str()?;
            if !protocol_id_is_valid(thread_id) || !protocol_id_is_valid(turn_id) {
                return None;
            }
            json!({
                "method": "error",
                "params": {
                    "threadId": thread_id,
                    "turnId": turn_id,
                    "willRetry": value.pointer("/params/willRetry"),
                    "error": {
                        "codexErrorInfo": value.pointer("/params/error/codexErrorInfo")
                    }
                }
            })
        }
        "turn/completed" => {
            let thread_id = value.pointer("/params/threadId")?.as_str()?;
            let turn_id = value.pointer("/params/turn/id")?.as_str()?;
            if !protocol_id_is_valid(thread_id) || !protocol_id_is_valid(turn_id) {
                return None;
            }
            json!({
                "method": "turn/completed",
                "params": {
                    "threadId": thread_id,
                    "turn": {
                        "id": turn_id,
                        "status": value.pointer("/params/turn/status"),
                        "error": {
                            "codexErrorInfo": value.pointer("/params/turn/error/codexErrorInfo")
                        }
                    }
                }
            })
        }
        "account/rateLimits/updated" => json!({
            "method": "account/rateLimits/updated",
            "params": {
                "rateLimits": value.pointer("/params/rateLimits"),
                "rateLimitsByLimitId": value.pointer("/params/rateLimitsByLimitId")
            }
        }),
        _ => return None,
    };
    bounded_observation(observation)
}

fn bounded_observation(value: Value) -> Option<Value> {
    (serde_json::to_vec(&value).ok()?.len() <= MAX_PROXY_OBSERVATION_BYTES).then_some(value)
}

async fn fail_closed_projection(context: &CliContext, record: &SessionRecord) {
    let context = context.clone();
    let id = record.id.clone();
    let Some(launch_id) = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
    else {
        return;
    };
    let mut retry_delay = Duration::from_millis(100);
    loop {
        let context = context.clone();
        let id = id.clone();
        let launch_id = launch_id.clone();
        match tokio::task::spawn_blocking(move || {
            crate::auto_resume::fail_closed_projection_for_runtime(
                &context,
                &id,
                &launch_id,
                &Timestamp::now().to_string(),
            )
        })
        .await
        {
            Ok(Ok(())) => return,
            Ok(Err(error)) if error.code() == "session-record-lock-timeout" => {
                tokio::time::sleep(retry_delay).await;
                retry_delay = (retry_delay * 2).min(Duration::from_secs(1));
            }
            Ok(Err(error)) => {
                eprintln!(
                    "warning: Codex projection fail-close stopped after permanent error: {}",
                    error.code()
                );
                return;
            }
            Err(error) => {
                eprintln!(
                    "warning: Codex projection fail-close worker failed permanently: {error}"
                );
                return;
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FreshBootstrap {
    ThreadStart,
    ThreadResponse { request_id: String },
    FirstTurn { thread_id: String },
    Closed,
}

impl FreshBootstrap {
    fn for_runtime(context: &CliContext, record: &SessionRecord) -> Self {
        let starting =
            crate::activity::activity_status(context, &record.id).is_ok_and(|activity| {
                activity.turn_state.phase == crate::activity::TurnPhase::Starting
            });
        let auto_resume_disabled = auto_resume_is_healthy_disabled(context, record);
        if record.provider_resume.is_none()
            && thread_attached_path(record).is_some_and(|path| !path.is_file())
            && starting
            && auto_resume_disabled
            && create_bootstrap_is_live(record)
        {
            Self::ThreadStart
        } else {
            Self::Closed
        }
    }

    fn bypasses_create_lock(
        &mut self,
        context: &CliContext,
        record: &SessionRecord,
        value: &Value,
    ) -> bool {
        if matches!(self, Self::Closed) {
            return false;
        }
        if !auto_resume_is_healthy_disabled(context, record) {
            *self = Self::Closed;
            return false;
        }
        match self {
            Self::ThreadStart
                if value.get("method").and_then(Value::as_str) == Some("thread/start") =>
            {
                let Some(request_id) = value.get("id").and_then(json_id_key) else {
                    *self = Self::Closed;
                    return false;
                };
                *self = Self::ThreadResponse { request_id };
                true
            }
            Self::FirstTurn { thread_id }
                if value.get("method").and_then(Value::as_str) == Some("turn/start") =>
            {
                let matches_bound_thread = value
                    .pointer("/params/threadId")
                    .and_then(Value::as_str)
                    .is_some_and(|candidate| candidate == thread_id);
                *self = Self::Closed;
                matches_bound_thread
            }
            Self::ThreadResponse { .. } | Self::FirstTurn { .. } | Self::ThreadStart => {
                *self = Self::Closed;
                false
            }
            Self::Closed => false,
        }
    }

    fn observe_server(&mut self, value: &Value) {
        let Self::ThreadResponse { request_id } = self else {
            return;
        };
        if value.get("id").and_then(json_id_key).as_deref() != Some(request_id.as_str()) {
            return;
        }
        *self = value
            .pointer("/result/thread/id")
            .and_then(Value::as_str)
            .filter(|thread_id| protocol_id_is_valid(thread_id))
            .map(|thread_id| Self::FirstTurn {
                thread_id: thread_id.to_string(),
            })
            .unwrap_or(Self::Closed);
    }

    fn close(&mut self) {
        *self = Self::Closed;
    }
}

struct MutationAuthorization {
    _bootstrap_gate: Option<CreateBootstrapGate>,
}

fn auto_resume_is_healthy_disabled(context: &CliContext, record: &SessionRecord) -> bool {
    let view = crate::auto_resume::view_for_record(context, record);
    view.supported && !view.enabled && view.state == "disabled" && view.failure_reason.is_none()
}

async fn cancel_before_tui_mutation(
    context: &CliContext,
    record: &SessionRecord,
    bootstrap: &mut FreshBootstrap,
    value: &Value,
) -> Option<MutationAuthorization> {
    let method = value.get("method").and_then(Value::as_str);
    if !matches!(method, Some("thread/start" | "turn/start")) {
        return Some(MutationAuthorization {
            _bootstrap_gate: None,
        });
    }
    // A fresh Codex TUI emits `thread/start`, then the initial prompt emits
    // `turn/start`, while the parent create path still owns the lifecycle lock.
    // The create-owned marker and exact disabled auto-resume state are checked
    // live for both requests. The first turn is armed only by a successful
    // matching thread/start response and must target the returned thread id.
    #[cfg(test)]
    NORMAL_CANCELLATION_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bootstrap_gate = if matches!(bootstrap, FreshBootstrap::Closed) {
        None
    } else {
        let gate_record = record.clone();
        tokio::task::spawn_blocking(move || acquire_create_bootstrap_gate(&gate_record))
            .await
            .ok()
            .flatten()
    };
    let cancellation_context = context.clone();
    let id = record.id.clone();
    let launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())?;
    let cancellation = tokio::task::spawn_blocking(move || {
        crate::auto_resume::try_cancel_for_manual_input_for_runtime(
            &cancellation_context,
            &id,
            &launch_id,
            &Timestamp::now().to_string(),
        )
    })
    .await
    .ok()
    .and_then(Result::ok);
    match cancellation {
        Some(crate::auto_resume::ManualInputCancelOutcome::Ready) => {
            bootstrap.close();
            Some(MutationAuthorization {
                _bootstrap_gate: bootstrap_gate,
            })
        }
        Some(crate::auto_resume::ManualInputCancelOutcome::Busy)
            if bootstrap_gate.is_some()
                && bootstrap.bypasses_create_lock(context, record, value) =>
        {
            Some(MutationAuthorization {
                _bootstrap_gate: bootstrap_gate,
            })
        }
        Some(crate::auto_resume::ManualInputCancelOutcome::Busy) => None,
        Some(crate::auto_resume::ManualInputCancelOutcome::RuntimeChanged) | None => {
            bootstrap.close();
            None
        }
    }
}

fn proxy_websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_PROXY_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_PROXY_FRAME_BYTES))
}

async fn run_proxy_session(
    context: CliContext,
    args: crate::cli::CodexAppServerProxyArgs,
) -> Result<(), String> {
    let record = crate::load_session_record(&context, &args.id)
        .map_err(|err| format!("session load failed: {}", err.code()))?;
    if !runtime_is_supported(&record)
        || socket_path(&record).map(Path::new) != Some(args.upstream.as_path())
        || proxy_path(&record) != Some(args.listen.as_path())
    {
        return Err("proxy paths did not match the active Codex runtime".to_string());
    }
    match fs::remove_file(&args.listen) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("failed to remove stale proxy socket: {err}")),
    }
    let listener = UnixListener::bind(&args.listen)
        .map_err(|err| format!("failed to bind private TUI proxy: {err}"))?;
    fs::set_permissions(&args.listen, fs::Permissions::from_mode(0o600))
        .map_err(|err| format!("failed to secure private TUI proxy: {err}"))?;
    let _guard = ProxySocketGuard(args.listen.clone());
    let (tui_stream, _) = tokio::time::timeout(CONTROL_RESPONSE_TIMEOUT, listener.accept())
        .await
        .map_err(|_| "remote TUI connection timed out".to_string())?
        .map_err(|err| format!("failed to accept remote TUI: {err}"))?;
    let upstream_stream = connect_socket(&args.upstream).await?;
    let mut tui = tokio::time::timeout(
        CONTROL_RESPONSE_TIMEOUT,
        tokio_tungstenite::accept_async_with_config(tui_stream, Some(proxy_websocket_config())),
    )
    .await
    .map_err(|_| "remote TUI WebSocket handshake timed out".to_string())?
    .map_err(|err| format!("remote TUI WebSocket handshake failed: {err}"))?;
    let (mut upstream, _) = tokio::time::timeout(
        CONTROL_RESPONSE_TIMEOUT,
        tokio_tungstenite::client_async_with_config(
            "ws://localhost",
            upstream_stream,
            Some(proxy_websocket_config()),
        ),
    )
    .await
    .map_err(|_| "upstream app-server WebSocket handshake timed out".to_string())?
    .map_err(|err| format!("upstream app-server handshake failed: {err}"))?;
    let mut projection = ProxyProjection::new(context.clone(), record.clone());
    let mut bootstrap = FreshBootstrap::for_runtime(&context, &record);
    loop {
        tokio::select! {
            message = tui.next() => {
                let message = message
                    .ok_or_else(|| "remote TUI closed the proxy".to_string())?
                    .map_err(|err| format!("remote TUI read failed: {err}"))?;
                let authorization = if let Some(value) = message_value(&message) {
                    let Some(authorization) = cancel_before_tui_mutation(
                        &context,
                        &record,
                        &mut bootstrap,
                        &value,
                    ).await else {
                        if let Some(id) = value
                            .get("id")
                            .filter(|id| json_id_key(id).is_some())
                        {
                            tui.send(Message::Text(
                                json!({
                                    "id": id,
                                    "error": {
                                        "code": -32001,
                                        "message": "agent-session state is busy; retry the request"
                                    }
                                })
                                .to_string()
                                .into(),
                            ))
                            .await
                            .map_err(|err| format!("remote TUI write failed: {err}"))?;
                        }
                        continue;
                    };
                    projection.observe_client(&value);
                    authorization
                } else {
                    MutationAuthorization { _bootstrap_gate: None }
                };
                let closed = matches!(message, Message::Close(_));
                upstream.send(message).await
                    .map_err(|err| format!("upstream app-server write failed: {err}"))?;
                drop(authorization);
                if closed {
                    projection.finish().await;
                    return Ok(());
                }
            }
            message = upstream.next() => {
                let message = message
                    .ok_or_else(|| "upstream app-server closed the proxy".to_string())?
                    .map_err(|err| format!("upstream app-server read failed: {err}"))?;
                let observed = message.clone();
                let closed = matches!(message, Message::Close(_));
                if let Some(value) = message_value(&observed) {
                    bootstrap.observe_server(&value);
                }
                tui.send(message).await
                    .map_err(|err| format!("remote TUI write failed: {err}"))?;
                projection.observe_server(&observed);
                if closed {
                    projection.finish().await;
                    return Ok(());
                }
            }
        }
    }
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
    let runtime_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
        .ok_or_else(|| "Codex runtime identity is missing".to_string())?;
    tokio::task::spawn_blocking(move || {
        crate::auto_resume::wake_scheduled_if_usage_open_for_runtime(
            &context,
            &id,
            &runtime_id,
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
            title_revision: 0,
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
                        PROXY_KEY.to_string(),
                        json!(display_path(&socket.with_extension("proxy"))),
                    ),
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

    fn write_create_bootstrap_marker(record: &SessionRecord) {
        let runtime = record.runtime.as_ref().unwrap();
        write_private_file(
            thread_handoff_path(record).unwrap(),
            runtime.launch_id.as_bytes(),
        )
        .unwrap();
    }

    #[test]
    fn create_bootstrap_guard_owns_the_marker_lifetime() {
        let tmp = tempfile::TempDir::new().unwrap();
        let record = record_with_runtime("guard", &tmp.path().join("guard.sock"));
        let marker = thread_handoff_path(&record).unwrap().to_path_buf();
        let guard = begin_create_bootstrap(&record).unwrap().unwrap();
        assert!(create_bootstrap_is_live(&record));
        drop(guard);
        assert!(!marker.exists());
        assert!(!create_bootstrap_is_live(&record));
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
    fn capability_probe_requires_the_audited_version_and_unix_transport() {
        let tmp = tempfile::TempDir::new().unwrap();
        for (name, version, help, expected) in [
            (
                "supported",
                "codex-cli 0.144.1",
                "  --listen <URL>  Supported values: stdio://, unix://PATH",
                true,
            ),
            (
                "old",
                "codex-cli 0.143.9",
                "  --listen <URL>  Supported values: stdio://, unix://PATH",
                false,
            ),
            (
                "no-unix",
                "codex-cli 0.144.1",
                "  --listen <URL>  Supported values: stdio://",
                false,
            ),
        ] {
            let path = tmp.path().join(name);
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\nif [ \"$1\" = --version ]; then printf '%s\\n' '{version}'; exit 0; fi\nif [ \"$1\" = app-server ] && [ \"$2\" = --help ]; then printf '%s\\n' '{help}'; exit 0; fi\nexit 1\n"
                ),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(app_server_capability_available(&path), expected, "{name}");
        }
    }

    #[test]
    fn launch_routes_the_visible_tui_through_the_private_proxy() {
        let script = launch_script();
        assert!(script.contains("codex-app-server-proxy"));
        assert!(script.contains("\"$agent\" --remote \"unix://$proxy\""));
        assert!(!script.contains("--remote \"unix://$socket\""));
        assert!(!script.contains("thread/shellCommand"));
        let cleanup_lines = script
            .lines()
            .filter(|line| line.trim_start().starts_with("rm -f --"))
            .collect::<Vec<_>>();
        assert!(!cleanup_lines[0].contains("$handoff"));
        assert!(cleanup_lines[1].contains("$handoff"));
    }

    #[test]
    fn explicit_cleanup_removes_only_the_derived_runtime_files() {
        let lock = GlobalStateLock::new();
        let runtime_dir = tempfile::Builder::new()
            .prefix("cx-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(runtime_dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let _runtime_dir = EnvGuard::set(
            &lock,
            "XDG_RUNTIME_DIR",
            runtime_dir.path().to_str().unwrap(),
        );
        let context = CliContext {
            state_dir: runtime_dir.path().join("state"),
            host: None,
        };
        let mut record = record_with_runtime("cleanup-runtime", Path::new("/placeholder"));
        let socket = allocate_socket_path(&context, &record).unwrap();
        record = record_with_runtime("cleanup-runtime", &socket);
        for path in [
            socket.clone(),
            socket.with_extension("proxy"),
            socket.with_extension("thread"),
            socket.with_extension("attached"),
        ] {
            fs::write(path, b"stale").unwrap();
        }
        let unrelated = socket.with_extension("unrelated");
        fs::write(&unrelated, b"keep").unwrap();

        let replacement_runtime = tempfile::Builder::new()
            .prefix("cx-")
            .tempdir_in("/tmp")
            .unwrap();
        let _replacement_runtime = EnvGuard::set(
            &lock,
            "XDG_RUNTIME_DIR",
            replacement_runtime.path().to_str().unwrap(),
        );

        cleanup_runtime_files(&context, &record).unwrap();

        assert!(!socket.exists());
        assert!(!socket.with_extension("proxy").exists());
        assert!(!socket.with_extension("thread").exists());
        assert!(!socket.with_extension("attached").exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn runtime_rejects_a_world_accessible_runtime_root() {
        let lock = GlobalStateLock::new();
        let runtime_dir = tempfile::Builder::new()
            .prefix("cx-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(runtime_dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let _runtime_dir = EnvGuard::set(
            &lock,
            "XDG_RUNTIME_DIR",
            runtime_dir.path().to_str().unwrap(),
        );

        let context = CliContext {
            state_dir: runtime_dir.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("unsafe-runtime", Path::new("/placeholder"));
        let err = allocate_socket_path(&context, &record).unwrap_err();
        assert_eq!(err.code(), "codex-app-server-runtime-dir-unsafe");
    }

    #[test]
    fn runtime_paths_are_isolated_by_state_and_launch_identity() {
        let lock = GlobalStateLock::new();
        let runtime_dir = tempfile::Builder::new()
            .prefix("cx-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(runtime_dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let _runtime_dir = EnvGuard::set(
            &lock,
            "XDG_RUNTIME_DIR",
            runtime_dir.path().to_str().unwrap(),
        );
        let record = record_with_runtime("shared-id", Path::new("/placeholder"));
        let context_a = CliContext {
            state_dir: runtime_dir.path().join("state-a"),
            host: None,
        };
        let context_b = CliContext {
            state_dir: runtime_dir.path().join("state-b"),
            host: None,
        };
        let first = allocate_socket_path(&context_a, &record).unwrap();
        let second = allocate_socket_path(&context_b, &record).unwrap();
        let mut next_launch = record.clone();
        next_launch.runtime.as_mut().unwrap().launch_id = "next-launch".to_string();
        let third = allocate_socket_path(&context_a, &next_launch).unwrap();

        assert_ne!(first, second);
        assert_ne!(first, third);
    }

    #[test]
    fn runtime_rejects_a_symlinked_runtime_root() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let _runtime_dir = EnvGuard::set(&lock, "XDG_RUNTIME_DIR", link.to_str().unwrap());
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("symlink-runtime", Path::new("/placeholder"));

        let err = allocate_socket_path(&context, &record).unwrap_err();
        assert_eq!(err.code(), "codex-app-server-runtime-dir-unsafe");
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

    #[tokio::test(start_paused = true)]
    async fn command_enqueue_is_included_in_the_control_timeout() {
        let (handle, _commands) = control_channel();
        let mut tasks = Vec::new();
        for _ in 0..5 {
            let handle = handle.clone();
            tasks.push(tokio::spawn(async move { handle.usage().await }));
        }
        tokio::task::yield_now().await;
        tokio::time::advance(CONTROL_RESPONSE_TIMEOUT + Duration::from_millis(1)).await;
        for task in tasks {
            assert_eq!(
                task.await.unwrap().unwrap_err(),
                "codex rate-limit request timed out"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn submit_timeout_covers_resume_plus_turn_acknowledgement_budget() {
        let (handle, mut commands) = control_channel();
        let responder = tokio::spawn(async move {
            let Some(ControlCommand::Continue { response, .. }) = commands.recv().await else {
                panic!("continuation command was not delivered");
            };
            tokio::time::sleep(Duration::from_secs(20)).await;
            let _ = response.send(Ok("acknowledged-turn".to_string()));
        });

        assert_eq!(
            handle.submit("fixed continuation").await.unwrap(),
            "acknowledged-turn"
        );
        responder.await.unwrap();
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
    fn reducer_bounds_provider_controlled_turn_identifiers() {
        let mut reducer = FailureReducer::new("thread-a");
        for index in 0..(MAX_REDUCER_PENDING_TURNS * 2) {
            assert_eq!(
                reducer.ingest(&json!({
                    "method": "error",
                    "params": {
                        "threadId": "thread-a",
                        "turnId": format!("turn-{index}"),
                        "willRetry": false,
                        "error": { "codexErrorInfo": "usageLimitExceeded" }
                    }
                })),
                None
            );
            assert_eq!(
                reducer.ingest(&json!({
                    "method": "turn/completed",
                    "params": {
                        "threadId": "thread-a",
                        "turn": { "id": format!("failed-{index}"), "status": "failed" }
                    }
                })),
                None
            );
        }
        assert_eq!(reducer.exhausted_turns.len(), MAX_REDUCER_PENDING_TURNS);
        assert_eq!(reducer.completed_turns.len(), MAX_REDUCER_PENDING_TURNS);
        let oversized = "x".repeat(MAX_PROTOCOL_ID_BYTES + 1);
        assert_eq!(
            reducer.ingest(&json!({
                "method": "error",
                "params": {
                    "threadId": "thread-a",
                    "turnId": oversized,
                    "willRetry": false,
                    "error": { "codexErrorInfo": "usageLimitExceeded" }
                }
            })),
            None
        );
        assert_eq!(reducer.exhausted_turns.len(), MAX_REDUCER_PENDING_TURNS);
    }

    #[test]
    fn reducer_detects_a_real_quota_failure_after_the_bounded_horizon() {
        let mut reducer = FailureReducer::new("thread-a");
        for index in 0..(MAX_REDUCER_PENDING_TURNS + 1) {
            assert_eq!(
                reducer.ingest(&json!({
                    "method": "turn/completed",
                    "params": {
                        "threadId": "thread-a",
                        "turn": { "id": format!("ordinary-failure-{index}"), "status": "failed" }
                    }
                })),
                None
            );
        }
        assert_eq!(
            reducer.ingest(&json!({
                "method": "error",
                "params": {
                    "threadId": "thread-a",
                    "turnId": "quota-after-horizon",
                    "willRetry": false,
                    "error": { "codexErrorInfo": "usageLimitExceeded" }
                }
            })),
            None
        );
        assert!(
            reducer
                .ingest(&json!({
                    "method": "turn/completed",
                    "params": {
                        "threadId": "thread-a",
                        "turn": { "id": "quota-after-horizon", "status": "failed" }
                    }
                }))
                .is_some()
        );
    }

    #[test]
    fn proxy_request_tracking_bounds_id_size_and_cardinality() {
        let record = record_with_runtime("proxy-bounds", Path::new("/tmp/proxy-bounds.sock"));
        let mut observer = ProxyObserver::new();
        assert!(json_id_key(&Value::String("x".repeat(MAX_PROTOCOL_ID_BYTES + 1))).is_none());
        for index in 0..(MAX_REDUCER_PENDING_TURNS * 2) {
            observer
                .observe_client(
                    &record,
                    &json!({ "id": index, "method": "thread/start", "params": {} }),
                )
                .unwrap();
        }
        assert!(observer.pending_thread_starts.len() <= MAX_REDUCER_PENDING_TURNS);
        assert!(
            client_observation(&json!({
                "method": "turn/start",
                "params": { "threadId": "x".repeat(MAX_PROTOCOL_ID_BYTES + 1) }
            }))
            .is_none()
        );
        assert!(
            server_observation(&json!({
                "method": "account/rateLimits/updated",
                "params": {
                    "rateLimits": {
                        "primary": {
                            "usedPercent": 100,
                            "oversized": "x".repeat(MAX_PROXY_OBSERVATION_BYTES)
                        }
                    }
                }
            }))
            .is_none()
        );
    }

    #[tokio::test]
    async fn oversized_server_message_fails_closed_with_method_after_padding() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("oversized-projection", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        let mut projection = ProxyProjection::new(context.clone(), record.clone());
        let raw = format!(
            "{{\"padding\":\"{}\",\"method\":\"turn/completed\"}}",
            "x".repeat(MAX_PROXY_RAW_OBSERVATION_BYTES)
        );

        projection.observe_server(&Message::Text(raw.into()));
        for _ in 0..20 {
            let view = crate::auto_resume::view_for_record(&context, &record);
            if view.state == "terminal_failure" {
                assert!(!view.enabled);
                assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("oversized projection did not fail closed");
    }

    #[tokio::test]
    async fn projection_fail_close_retries_after_timed_lock_contention() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("projection-retry", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        let lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        let task_context = context.clone();
        let task_record = record.clone();
        let fail = tokio::spawn(async move {
            fail_closed_projection(&task_context, &task_record).await;
        });

        tokio::time::sleep(Duration::from_millis(1_100)).await;
        drop(lock);
        tokio::time::timeout(Duration::from_secs(3), fail)
            .await
            .expect("fail-close retry did not finish")
            .unwrap();
        let view = crate::auto_resume::view_for_record(&context, &record);
        assert!(!view.enabled);
        assert_eq!(view.state, "terminal_failure");
        assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));
    }

    #[tokio::test]
    async fn projection_fail_close_stops_after_permanent_state_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("projection-permanent", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        fs::remove_dir_all(crate::session_dir(&context, &record.id)).unwrap();

        tokio::time::timeout(
            Duration::from_millis(250),
            fail_closed_projection(&context, &record),
        )
        .await
        .expect("permanent projection state error must terminate the retry task");
    }

    #[test]
    fn attached_thread_binding_rejects_a_different_reconnect_thread() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("codex.sock");
        let record = record_with_runtime("thread-binding", &socket);

        bind_thread(&record, "raw-thread-a").unwrap();
        let binding = fs::read_to_string(socket.with_extension("attached")).unwrap();
        assert_eq!(binding, projected_thread_binding("raw-thread-a"));
        assert!(!binding.contains("raw-thread-a"));
        let err = bind_thread(&record, "raw-thread-b").unwrap_err();
        assert_eq!(
            err,
            "Codex loaded thread did not match the attached runtime"
        );
    }

    #[test]
    fn usage_projection_is_authoritative_only_for_well_formed_response() {
        assert!(!usage_snapshot(&json!({})).authoritative);
        assert!(!usage_snapshot(&json!({ "rateLimits": {} })).authoritative);
        for malformed in [
            json!({ "usedPercent": "100" }),
            json!({ "resetsAt": 1_900_000_100 }),
            json!([]),
        ] {
            let snapshot = usage_snapshot(&json!({
                "rateLimits": {
                    "primary": { "usedPercent": 42.0, "resetsAt": 1_900_000_000 },
                    "secondary": malformed
                }
            }));
            assert!(!snapshot.authoritative, "snapshot={snapshot:?}");
        }
        let snapshot = usage_snapshot(&json!({
            "rateLimits": {
                "primary": { "usedPercent": 100.0, "resetsAt": 1_900_000_000 },
                "secondary": { "usedPercent": 42.0, "resetsAt": 1_900_000_100 }
            }
        }));
        assert!(snapshot.authoritative);
        assert!(snapshot.has_exhausted_windows);
        assert_eq!(snapshot.exhausted_reset_epochs, vec![1_900_000_000]);

        let snapshot = usage_snapshot(&json!({
            "rateLimits": {
                "primary": { "usedPercent": 42.0, "resetsAt": 1_900_000_000 }
            },
            "rateLimitsByLimitId": {
                "codex": {
                    "primary": { "usedPercent": 100.0, "resetsAt": 1_900_000_200 },
                    "secondary": { "usedPercent": 100.0, "resetsAt": 1_900_000_300 }
                }
            }
        }));
        assert!(snapshot.authoritative);
        assert!(snapshot.has_exhausted_windows);
        assert_eq!(
            snapshot.exhausted_reset_epochs,
            vec![1_900_000_200, 1_900_000_300]
        );

        assert!(
            !usage_snapshot(&json!({
                "rateLimits": { "primary": { "usedPercent": 42.0 } },
                "rateLimitsByLimitId": { "codex": { "primary": { "usedPercent": "100" } } }
            }))
            .authoritative
        );
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
    async fn control_reconnect_resumes_the_bound_loaded_thread() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("reconnect.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("reconnect", &socket);
        bind_thread(&record, "raw-thread-a").unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let initialize = receive_json(&mut socket).await;
            respond(&mut socket, &initialize, json!({})).await;
            assert_eq!(receive_json(&mut socket).await["method"], "initialized");
            let loaded = receive_json(&mut socket).await;
            respond(
                &mut socket,
                &loaded,
                json!({ "data": ["raw-thread-a"], "nextCursor": null }),
            )
            .await;
            let resume = receive_json(&mut socket).await;
            assert_eq!(resume["method"], "thread/resume");
            assert_eq!(resume["params"]["threadId"], "raw-thread-a");
            respond(&mut socket, &resume, json!({})).await;
            for _ in 0..2 {
                let usage = receive_json(&mut socket).await;
                assert_eq!(usage["method"], "account/rateLimits/read");
                respond(
                    &mut socket,
                    &usage,
                    json!({
                        "rateLimits": {
                            "primary": { "usedPercent": 100, "resetsAt": 1_900_000_000 }
                        }
                    }),
                )
                .await;
            }
        });
        let (handle, commands) = control_channel();
        let control = tokio::spawn(run_control(context, record, commands));

        let usage = handle.usage().await.unwrap();
        assert!(usage.authoritative);
        assert!(usage.has_exhausted_windows);
        server.await.unwrap();
        drop(handle);
        control.abort();
        let _ = control.await;
    }

    #[tokio::test]
    async fn tui_proxy_projects_exact_failure_from_the_tui_connection_without_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let upstream = tmp.path().join("upstream.sock");
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let id = "proxy-control";
        fs::create_dir_all(crate::session_dir(&context, id)).unwrap();
        let record = record_with_runtime(id, &upstream);
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, id, true, "2030-01-01T00:00:00Z").unwrap();
        let proxy_args = crate::cli::CodexAppServerProxyArgs {
            id: id.to_string(),
            upstream: upstream.clone(),
            listen: upstream.with_extension("proxy"),
        };
        let proxy_context = context.clone();
        let proxy = tokio::spawn(async move { run_proxy_session(proxy_context, proxy_args).await });
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let start = receive_json(&mut socket).await;
            assert_eq!(start["method"], "thread/start");
            respond(
                &mut socket,
                &start,
                json!({ "thread": { "id": "raw-proxy-thread" } }),
            )
            .await;
            for value in [
                json!({
                    "method": "error",
                    "params": {
                        "threadId": "raw-proxy-thread",
                        "turnId": "raw-proxy-turn",
                        "willRetry": false,
                        "error": {
                            "message": "localized secret proxy error",
                            "codexErrorInfo": "usageLimitExceeded"
                        }
                    }
                }),
                json!({
                    "method": "turn/completed",
                    "params": {
                        "threadId": "raw-proxy-thread",
                        "turn": { "id": "raw-proxy-turn", "status": "failed" }
                    }
                }),
            ] {
                socket
                    .send(Message::Text(value.to_string().into()))
                    .await
                    .unwrap();
            }
            socket.close(None).await.unwrap();
        });
        let proxy_stream = connect_socket(&upstream.with_extension("proxy"))
            .await
            .unwrap();
        let (mut tui, _) = tokio_tungstenite::client_async("ws://localhost", proxy_stream)
            .await
            .unwrap();
        tui.send(Message::Text(
            json!({
                "id": 7,
                "method": "thread/start",
                "params": { "cwd": "/repo", "developerInstructions": "secret prompt" }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        for _ in 0..3 {
            tui.next().await.unwrap().unwrap();
        }
        server.await.unwrap();
        proxy.await.unwrap().unwrap();

        let session_dir = crate::session_dir(&context, id);
        let activity = format!(
            "{}\n{}",
            fs::read_to_string(session_dir.join("activity.json")).unwrap(),
            fs::read_to_string(session_dir.join("activity.journal.jsonl")).unwrap()
        );
        assert!(activity.contains("provider_hook"));
        assert!(activity.contains("usage_exhausted"));
        for secret in [
            "raw-proxy-thread",
            "raw-proxy-turn",
            "localized secret proxy error",
            "secret prompt",
        ] {
            assert!(!activity.contains(secret));
        }
    }

    #[tokio::test]
    async fn tui_turn_start_cancels_a_scheduled_resume_before_forwarding() {
        let tmp = tempfile::TempDir::new().unwrap();
        let upstream = tmp.path().join("manual.sock");
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("manual-input", &upstream);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        crate::activity::ingest_codex_app_server_failure(
            &context,
            &record.id,
            &record.runtime.as_ref().unwrap().launch_id,
            "thread-a",
            "failed-turn",
        )
        .unwrap();
        assert_eq!(
            crate::auto_resume::tick_for_runtime(
                &context,
                &record.id,
                &record.runtime.as_ref().unwrap().launch_id,
                1_893_456_000,
                &UsageSnapshot {
                    authoritative: true,
                    has_exhausted_windows: true,
                    exhausted_reset_epochs: vec![1_893_456_600],
                },
                |_| panic!("blocked usage must not submit"),
            )
            .unwrap(),
            crate::auto_resume::TickOutcome::Scheduled
        );
        bind_thread(&record, "thread-a").unwrap();

        let proxy_args = crate::cli::CodexAppServerProxyArgs {
            id: record.id.clone(),
            upstream: upstream.clone(),
            listen: upstream.with_extension("proxy"),
        };
        let proxy_context = context.clone();
        let proxy = tokio::spawn(async move { run_proxy_session(proxy_context, proxy_args).await });
        let server_context = context.clone();
        let server_record = record.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let turn = receive_json(&mut socket).await;
            assert_eq!(turn["method"], "turn/start");
            let view = crate::auto_resume::view_for_record(&server_context, &server_record);
            assert_eq!(view.state, "cancelled");
            assert_eq!(view.failure_reason.as_deref(), Some("manual_input"));
            respond(
                &mut socket,
                &turn,
                json!({ "turn": { "id": "manual-turn", "status": "inProgress" } }),
            )
            .await;
            socket.close(None).await.unwrap();
        });
        let proxy_stream = connect_socket(&upstream.with_extension("proxy"))
            .await
            .unwrap();
        let (mut tui, _) = tokio_tungstenite::client_async("ws://localhost", proxy_stream)
            .await
            .unwrap();
        tui.send(Message::Text(
            json!({
                "id": 9,
                "method": "turn/start",
                "params": { "threadId": "thread-a", "input": [] }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        assert_eq!(receive_json(&mut tui).await["id"], 9);
        server.await.unwrap();
        proxy.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn busy_manual_cancellation_rejects_only_the_turn_and_keeps_proxy_alive() {
        let tmp = tempfile::TempDir::new().unwrap();
        let upstream = tmp.path().join("busy.sock");
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("busy-input", &upstream);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        bind_thread(&record, "thread-a").unwrap();
        let proxy_args = crate::cli::CodexAppServerProxyArgs {
            id: record.id.clone(),
            upstream: upstream.clone(),
            listen: upstream.with_extension("proxy"),
        };
        let proxy_context = context.clone();
        let proxy = tokio::spawn(async move { run_proxy_session(proxy_context, proxy_args).await });
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request = receive_json(&mut socket).await;
            assert_eq!(request["id"], 2);
            respond(&mut socket, &request, json!({ "ok": true })).await;
            socket.close(None).await.unwrap();
        });
        let proxy_stream = connect_socket(&upstream.with_extension("proxy"))
            .await
            .unwrap();
        let (mut tui, _) = tokio_tungstenite::client_async("ws://localhost", proxy_stream)
            .await
            .unwrap();
        let lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        tui.send(Message::Text(
            json!({
                "id": 1,
                "method": "turn/start",
                "params": { "threadId": "thread-a", "input": [] }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let rejected = receive_json(&mut tui).await;
        assert_eq!(rejected["id"], 1);
        assert_eq!(rejected["error"]["code"], -32001);
        drop(lock);
        tui.send(Message::Text(
            json!({ "id": 2, "method": "thread/read", "params": {} })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        assert_eq!(receive_json(&mut tui).await["id"], 2);
        server.await.unwrap();
        proxy.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn fresh_tui_thread_start_bypasses_the_create_lifecycle_lock() {
        let tmp = tempfile::TempDir::new().unwrap();
        let upstream = tmp.path().join("fresh-start.sock");
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("fresh-start", &upstream);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        write_create_bootstrap_marker(&record);
        let proxy_args = crate::cli::CodexAppServerProxyArgs {
            id: record.id.clone(),
            upstream: upstream.clone(),
            listen: upstream.with_extension("proxy"),
        };
        let proxy_context = context.clone();
        let proxy = tokio::spawn(async move { run_proxy_session(proxy_context, proxy_args).await });
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request = receive_json(&mut socket).await;
            assert_eq!(request["method"], "thread/start");
            respond(
                &mut socket,
                &request,
                json!({ "thread": { "id": "fresh-thread" } }),
            )
            .await;
            let request = receive_json(&mut socket).await;
            assert_eq!(request["id"], 2);
            assert_eq!(request["method"], "turn/start");
            respond(
                &mut socket,
                &request,
                json!({ "turn": { "id": "first-turn", "status": "inProgress" } }),
            )
            .await;
            let request = receive_json(&mut socket).await;
            assert_eq!(request["id"], 4);
            assert_eq!(request["method"], "thread/read");
            respond(
                &mut socket,
                &request,
                json!({ "thread": { "id": "fresh-thread" } }),
            )
            .await;
            socket.close(None).await.unwrap();
        });
        let proxy_stream = connect_socket(&upstream.with_extension("proxy"))
            .await
            .unwrap();
        let (mut tui, _) = tokio_tungstenite::client_async("ws://localhost", proxy_stream)
            .await
            .unwrap();
        let lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        tui.send(Message::Text(
            json!({
                "id": 1,
                "method": "thread/start",
                "params": { "cwd": "/repo" }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let response = receive_json(&mut tui).await;
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["thread"]["id"], "fresh-thread");
        tui.send(Message::Text(
            json!({
                "id": 2,
                "method": "turn/start",
                "params": { "threadId": "fresh-thread", "input": [] }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let response = receive_json(&mut tui).await;
        assert_eq!(response["id"], 2);
        assert_eq!(response["result"]["turn"]["id"], "first-turn");
        tui.send(Message::Text(
            json!({
                "id": 3,
                "method": "turn/start",
                "params": { "threadId": "fresh-thread", "input": [] }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let rejected = receive_json(&mut tui).await;
        assert_eq!(rejected["id"], 3);
        assert_eq!(rejected["error"]["code"], -32001);
        tui.send(Message::Text(
            json!({ "id": 4, "method": "thread/read", "params": {} })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        assert_eq!(receive_json(&mut tui).await["id"], 4);
        drop(lock);
        server.await.unwrap();
        proxy.await.unwrap().unwrap();
    }

    #[test]
    fn fresh_bootstrap_first_turn_must_match_the_successful_thread_start() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("fresh-bound.sock");
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("fresh-bound", &socket);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        write_create_bootstrap_marker(&record);
        let mut bootstrap = FreshBootstrap::for_runtime(&context, &record);
        assert!(bootstrap.bypasses_create_lock(
            &context,
            &record,
            &json!({ "id": 1, "method": "thread/start", "params": {} }),
        ));
        bootstrap
            .observe_server(&json!({ "id": 1, "result": { "thread": { "id": "fresh-thread" } } }));
        assert!(!bootstrap.bypasses_create_lock(
            &context,
            &record,
            &json!({
                "id": 2,
                "method": "turn/start",
                "params": { "threadId": "different-thread", "input": [] }
            }),
        ));
        assert_eq!(bootstrap, FreshBootstrap::Closed);
    }

    #[test]
    fn closed_fresh_bootstrap_skips_live_filesystem_validation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let record = record_with_runtime("closed-bootstrap", &tmp.path().join("closed.sock"));
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        BOOTSTRAP_LIVE_CHECKS.with(|checks| checks.set(0));
        let mut bootstrap = FreshBootstrap::Closed;
        assert!(!bootstrap.bypasses_create_lock(
            &context,
            &record,
            &json!({ "id": 1, "method": "turn/start", "params": {} }),
        ));
        assert_eq!(BOOTSTRAP_LIVE_CHECKS.with(std::cell::Cell::get), 0);
    }

    #[tokio::test]
    async fn marker_live_lock_free_turn_attempts_normal_cancellation_before_bypass() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("teardown-window.sock");
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("teardown-window", &socket);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        write_create_bootstrap_marker(&record);
        let mut bootstrap = FreshBootstrap::FirstTurn {
            thread_id: "fresh-thread".to_string(),
        };
        NORMAL_CANCELLATION_ATTEMPTS.store(0, std::sync::atomic::Ordering::Relaxed);
        assert!(
            cancel_before_tui_mutation(
                &context,
                &record,
                &mut bootstrap,
                &json!({
                    "id": 2,
                    "method": "turn/start",
                    "params": { "threadId": "fresh-thread", "input": [] }
                }),
            )
            .await
            .is_some()
        );
        assert_eq!(
            NORMAL_CANCELLATION_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert_eq!(bootstrap, FreshBootstrap::Closed);
    }

    #[tokio::test]
    async fn replacement_lock_cannot_reuse_marker_during_gated_teardown() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("gated-teardown.sock");
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("gated-teardown", &socket);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        let create_lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        let bootstrap_guard = begin_create_bootstrap(&record).unwrap().unwrap();
        assert!(lock_bootstrap_file(&bootstrap_guard.file));
        drop(create_lock);
        let replacement_lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();

        BOOTSTRAP_GATE_ATTEMPTS.store(0, std::sync::atomic::Ordering::Relaxed);
        let task_context = context.clone();
        let task_record = record.clone();
        let task = tokio::spawn(async move {
            let mut bootstrap = FreshBootstrap::FirstTurn {
                thread_id: "fresh-thread".to_string(),
            };
            let authorization = cancel_before_tui_mutation(
                &task_context,
                &task_record,
                &mut bootstrap,
                &json!({
                    "id": 2,
                    "method": "turn/start",
                    "params": { "threadId": "fresh-thread", "input": [] }
                }),
            )
            .await;
            (authorization.is_some(), bootstrap)
        });
        for _ in 0..100 {
            if BOOTSTRAP_GATE_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            BOOTSTRAP_GATE_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        fs::remove_file(thread_handoff_path(&record).unwrap()).unwrap();
        unlock_bootstrap_file(&bootstrap_guard.file);

        let (authorized, _) = task.await.unwrap();
        assert!(!authorized);
        drop(replacement_lock);
        drop(bootstrap_guard);
    }

    #[tokio::test]
    async fn fresh_tui_first_turn_does_not_bypass_after_create_marker_is_removed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let upstream = tmp.path().join("fresh-expired.sock");
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("fresh-expired", &upstream);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        write_create_bootstrap_marker(&record);
        let proxy_args = crate::cli::CodexAppServerProxyArgs {
            id: record.id.clone(),
            upstream: upstream.clone(),
            listen: upstream.with_extension("proxy"),
        };
        let proxy_context = context.clone();
        let proxy = tokio::spawn(async move { run_proxy_session(proxy_context, proxy_args).await });
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let start = receive_json(&mut socket).await;
            respond(
                &mut socket,
                &start,
                json!({ "thread": { "id": "fresh-thread" } }),
            )
            .await;
            loop {
                let request = receive_json(&mut socket).await;
                respond(&mut socket, &request, json!({ "ok": true })).await;
                if request["id"] == 3 {
                    break;
                }
            }
            socket.close(None).await.unwrap();
        });
        let proxy_stream = connect_socket(&upstream.with_extension("proxy"))
            .await
            .unwrap();
        let (mut tui, _) = tokio_tungstenite::client_async("ws://localhost", proxy_stream)
            .await
            .unwrap();
        let lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        tui.send(Message::Text(
            json!({ "id": 1, "method": "thread/start", "params": { "cwd": "/repo" } })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        assert_eq!(receive_json(&mut tui).await["id"], 1);
        fs::remove_file(thread_handoff_path(&record).unwrap()).unwrap();
        tui.send(Message::Text(
            json!({
                "id": 2,
                "method": "turn/start",
                "params": { "threadId": "fresh-thread", "input": [] }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let rejected = receive_json(&mut tui).await;
        assert_eq!(rejected["id"], 2);
        assert_eq!(rejected["error"]["code"], -32001);
        drop(lock);
        tui.send(Message::Text(
            json!({ "id": 3, "method": "thread/read", "params": {} })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        assert_eq!(receive_json(&mut tui).await["id"], 3);
        server.await.unwrap();
        proxy.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn fresh_tui_does_not_bypass_when_auto_resume_state_is_unavailable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let upstream = tmp.path().join("fresh-unavailable.sock");
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("fresh-unavailable", &upstream);
        let session_dir = crate::session_dir(&context, &record.id);
        fs::create_dir_all(&session_dir).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        write_create_bootstrap_marker(&record);
        fs::write(session_dir.join("auto-resume.json"), b"not-json").unwrap();
        let proxy_args = crate::cli::CodexAppServerProxyArgs {
            id: record.id.clone(),
            upstream: upstream.clone(),
            listen: upstream.with_extension("proxy"),
        };
        let proxy_context = context.clone();
        let proxy = tokio::spawn(async move { run_proxy_session(proxy_context, proxy_args).await });
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request = receive_json(&mut socket).await;
            respond(&mut socket, &request, json!({ "ok": true })).await;
            socket.close(None).await.unwrap();
        });
        let proxy_stream = connect_socket(&upstream.with_extension("proxy"))
            .await
            .unwrap();
        let (mut tui, _) = tokio_tungstenite::client_async("ws://localhost", proxy_stream)
            .await
            .unwrap();
        let lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        tui.send(Message::Text(
            json!({ "id": 1, "method": "thread/start", "params": { "cwd": "/repo" } })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        let rejected = receive_json(&mut tui).await;
        assert_eq!(rejected["id"], 1);
        assert_eq!(rejected["error"]["code"], -32001);
        drop(lock);
        tui.send(Message::Text(
            json!({ "id": 2, "method": "thread/read", "params": {} })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        assert_eq!(receive_json(&mut tui).await["id"], 2);
        server.await.unwrap();
        proxy.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn fresh_tui_first_turn_requires_a_successful_bound_thread_start() {
        let tmp = tempfile::TempDir::new().unwrap();
        let upstream = tmp.path().join("fresh-correlation.sock");
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("fresh-correlation", &upstream);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        write_create_bootstrap_marker(&record);
        let proxy_args = crate::cli::CodexAppServerProxyArgs {
            id: record.id.clone(),
            upstream: upstream.clone(),
            listen: upstream.with_extension("proxy"),
        };
        let proxy_context = context.clone();
        let proxy = tokio::spawn(async move { run_proxy_session(proxy_context, proxy_args).await });
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let start = receive_json(&mut socket).await;
            socket
                .send(Message::Text(
                    json!({ "id": start["id"], "error": { "code": -32000, "message": "rejected" } })
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            loop {
                let request = receive_json(&mut socket).await;
                respond(&mut socket, &request, json!({ "ok": true })).await;
                if request["id"] == 3 {
                    break;
                }
            }
            socket.close(None).await.unwrap();
        });
        let proxy_stream = connect_socket(&upstream.with_extension("proxy"))
            .await
            .unwrap();
        let (mut tui, _) = tokio_tungstenite::client_async("ws://localhost", proxy_stream)
            .await
            .unwrap();
        let lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        tui.send(Message::Text(
            json!({ "id": 1, "method": "thread/start", "params": { "cwd": "/repo" } })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        assert_eq!(receive_json(&mut tui).await["error"]["code"], -32000);
        tui.send(Message::Text(
            json!({
                "id": 2,
                "method": "turn/start",
                "params": { "threadId": "unbound-thread", "input": [] }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let rejected = receive_json(&mut tui).await;
        assert_eq!(rejected["id"], 2);
        assert_eq!(rejected["error"]["code"], -32001);
        drop(lock);
        tui.send(Message::Text(
            json!({ "id": 3, "method": "thread/read", "params": {} })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        assert_eq!(receive_json(&mut tui).await["id"], 3);
        server.await.unwrap();
        proxy.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn proxy_observer_failure_does_not_interrupt_later_tui_frames() {
        let tmp = tempfile::TempDir::new().unwrap();
        let upstream = tmp.path().join("observer.sock");
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("observer-failure", &upstream);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        crate::activity::ingest_codex_app_server_failure(
            &context,
            &record.id,
            &record.runtime.as_ref().unwrap().launch_id,
            "thread-a",
            "failed-turn",
        )
        .unwrap();
        crate::auto_resume::tick_for_runtime(
            &context,
            &record.id,
            &record.runtime.as_ref().unwrap().launch_id,
            1_893_456_000,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: true,
                exhausted_reset_epochs: vec![1_893_456_600],
            },
            |_| panic!("blocked usage must not submit"),
        )
        .unwrap();
        bind_thread(&record, "thread-a").unwrap();
        let proxy_args = crate::cli::CodexAppServerProxyArgs {
            id: record.id.clone(),
            upstream: upstream.clone(),
            listen: upstream.with_extension("proxy"),
        };
        let proxy_context = context.clone();
        let proxy = tokio::spawn(async move { run_proxy_session(proxy_context, proxy_args).await });
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            for expected_id in [1, 2] {
                let request = receive_json(&mut socket).await;
                assert_eq!(request["id"], expected_id);
                let result = if expected_id == 1 {
                    json!({ "thread": { "id": "thread-b" } })
                } else {
                    json!({ "ok": true })
                };
                respond(&mut socket, &request, result).await;
            }
            socket.close(None).await.unwrap();
        });
        let proxy_stream = connect_socket(&upstream.with_extension("proxy"))
            .await
            .unwrap();
        let (mut tui, _) = tokio_tungstenite::client_async("ws://localhost", proxy_stream)
            .await
            .unwrap();
        for (id, method) in [(1, "thread/start"), (2, "thread/read")] {
            tui.send(Message::Text(
                json!({
                    "id": id,
                    "method": method,
                    "params": { "threadId": "thread-b" }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
            assert_eq!(receive_json(&mut tui).await["id"], id);
        }
        server.await.unwrap();
        proxy.await.unwrap().unwrap();
        let view = crate::auto_resume::view_for_record(&context, &record);
        assert_eq!(view.state, "cancelled");
        assert_eq!(view.failure_reason.as_deref(), Some("manual_input"));
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
            title_revision: 0,
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
                        PROXY_KEY.to_string(),
                        json!(display_path(&socket.with_extension("proxy"))),
                    ),
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
            let loaded = receive_json(&mut socket).await;
            assert_eq!(loaded["method"], "thread/loaded/list");
            respond(
                &mut socket,
                &loaded,
                json!({ "data": ["raw-thread-a"], "nextCursor": null }),
            )
            .await;
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
            let resume = receive_json(&mut socket).await;
            assert_eq!(resume["method"], "thread/resume");
            assert_eq!(resume["params"]["threadId"], "raw-thread-a");
            respond(&mut socket, &resume, json!({})).await;
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
        assert!(activity.contains("provider_hook"));
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

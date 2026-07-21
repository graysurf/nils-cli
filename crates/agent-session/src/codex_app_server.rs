//! Strict metadata-only projection of the Codex app-server v2 protocol.
//!
//! The interactive TUI's human-readable error text is intentionally ignored.
//! Auto-resume is armed only when the live protocol reports both an exact
//! `usageLimitExceeded` error and a matching terminal `failed` completion for
//! the same bound thread and turn.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixDatagram;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
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
pub(crate) const ATTENTION_AUTHORITY_KEY: &str = "codex_attention_authority";
pub(crate) const ATTENTION_AUTHORITY_ENV: &str = "AGENT_SESSION_ATTENTION_AUTHORITY";
const ATTENTION_AUTHORITY_PROTOCOL: &str = "protocol";
const ATTENTION_AUTHORITY_HOOK: &str = "hook";

const UNIX_SOCKET_PATH_BUDGET: usize = 100;
const MAX_PROTOCOL_ID_BYTES: usize = 256;
const MAX_REDUCER_PENDING_TURNS: usize = 64;
const MAX_PENDING_ATTENTION_REQUESTS: usize = 64;
const MAX_PROXY_OBSERVATIONS: usize = 16;
const MAX_PROXY_OBSERVATION_BYTES: usize = 64 * 1024;
const MAX_PROXY_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROXY_FRAME_BYTES: usize = 4 * 1024 * 1024;
const CONTROL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_SUBMISSION_TIMEOUT: Duration = Duration::from_secs(15);
const CONTROL_SUBMIT_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
const MINIMUM_APP_SERVER_VERSION: (u64, u64, u64) = (0, 144, 1);
const AUDITED_EXACT_ATTENTION_VERSIONS: &[(u64, u64, u64)] = &[(0, 144, 1), (0, 144, 3)];
const APP_SERVER_CAPABILITY_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const APP_SERVER_CAPABILITY_PROBE_MAX_OUTPUT_BYTES: u64 = 64 * 1024;
const CODEX_ACCOUNT_READINESS_SCHEMA_VERSION: &str = "agent-session.codex-account-readiness.v1";
const MANUAL_INPUT_SECTION_FILE: &str = ".codex-app-server-manual-input";
const MANUAL_INPUT_GATE_FILE: &str = ".codex-app-server-manual-input-gate";
const MANUAL_INPUT_SECTION_VERSION: &str = "agent-session.codex-manual-input.v1";
const MANUAL_INPUT_SECTION_TTL: Duration = Duration::from_secs(30);
const MANUAL_INPUT_GATE_TIMEOUT: Duration = Duration::from_secs(12);
const MANUAL_INPUT_ACK_TIMEOUT: Duration = Duration::from_millis(250);
const PROXY_CAPABILITY_FILE: &str = ".codex-app-server-proxy-capability";
const PROXY_CAPABILITY_VERSION: &str = "agent-session.codex-manual-input-proxy.v1";
const PROXY_CAPABILITY_TTL: Duration = Duration::from_secs(365 * 24 * 60 * 60);
const PROXY_CAPABILITY_READY_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) fn attention_authority(record: &SessionRecord) -> &'static str {
    if record.runtime.as_ref().is_some_and(|runtime| {
        runtime.kind == RUNTIME_KIND
            && runtime
                .extra
                .get(ATTENTION_AUTHORITY_KEY)
                .and_then(Value::as_str)
                == Some(ATTENTION_AUTHORITY_PROTOCOL)
    }) {
        ATTENTION_AUTHORITY_PROTOCOL
    } else {
        ATTENTION_AUTHORITY_HOOK
    }
}

pub(crate) fn exact_attention_version_is_audited(version: (u64, u64, u64)) -> bool {
    AUDITED_EXACT_ATTENTION_VERSIONS.contains(&version)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AppServerCapabilities {
    transport: bool,
    exact_attention: bool,
    source_guard: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct CodexAccountReadiness {
    schema_version: &'static str,
    supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason_code: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AppServerProbe {
    capabilities: AppServerCapabilities,
    provider_version: Option<String>,
    reason_code: Option<&'static str>,
}

impl AppServerProbe {
    fn unavailable(reason_code: &'static str, provider_version: Option<String>) -> Self {
        Self {
            capabilities: AppServerCapabilities {
                transport: false,
                exact_attention: false,
                source_guard: false,
            },
            provider_version,
            reason_code: Some(reason_code),
        }
    }
}

pub(crate) fn account_binding_readiness(agent_bin: &Path) -> CodexAccountReadiness {
    let probe = app_server_probe(agent_bin);
    CodexAccountReadiness {
        schema_version: CODEX_ACCOUNT_READINESS_SCHEMA_VERSION,
        supported: probe.capabilities.transport,
        provider_version: probe.provider_version,
        reason_code: probe.reason_code,
    }
}

pub(crate) fn configure_runtime(
    context: &CliContext,
    agent_bin: &Path,
    record: &mut SessionRecord,
    managed: bool,
) -> Result<(), CliError> {
    if record.agent != "codex" {
        return Ok(());
    }
    let binding_required = crate::codex_account::binding_is_present(record);
    if !managed && !binding_required {
        return Ok(());
    }
    let preference = env::var("AGENT_SESSION_CODEX_RUNTIME").unwrap_or_else(|_| "auto".into());
    if !matches!(preference.as_str(), "auto" | "app-server") && !binding_required {
        return Ok(());
    }
    let forced = preference == "app-server" || binding_required;
    let probe = app_server_probe(agent_bin);
    let mut capabilities = probe.capabilities;
    capabilities.source_guard = crate::activity::codex_protocol_attention_source_guard_configured();
    if !capabilities.transport {
        if forced {
            return Err(CliError::data(
                "codex-app-server-capability-unavailable",
                "installed Codex does not advertise app-server Unix listen support",
                Some(json!({
                    "provider_version": probe.provider_version,
                    "reason_code": probe.reason_code,
                })),
            ));
        }
        return Ok(());
    }
    configure_runtime_with_capabilities(context, record, forced, capabilities)
}

fn configure_runtime_with_capabilities(
    context: &CliContext,
    record: &mut SessionRecord,
    forced: bool,
    capabilities: AppServerCapabilities,
) -> Result<(), CliError> {
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
    runtime.extra.insert(
        ATTENTION_AUTHORITY_KEY.to_string(),
        json!(
            if capabilities.exact_attention && capabilities.source_guard {
                ATTENTION_AUTHORITY_PROTOCOL
            } else {
                ATTENTION_AUTHORITY_HOOK
            }
        ),
    );
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

#[cfg(test)]
fn app_server_capabilities(agent_bin: &Path) -> AppServerCapabilities {
    app_server_probe(agent_bin).capabilities
}

fn app_server_probe(agent_bin: &Path) -> AppServerProbe {
    let Some(version) = bounded_command_output(agent_bin, &["--version"]) else {
        return AppServerProbe::unavailable("codex-unavailable", None);
    };
    let version_text = String::from_utf8_lossy(&version.stdout);
    let Some(version) = parse_version_triplet(&version_text) else {
        return AppServerProbe::unavailable("codex-version-unrecognized", None);
    };
    let provider_version = Some(format!("{}.{}.{}", version.0, version.1, version.2));
    if version < MINIMUM_APP_SERVER_VERSION {
        return AppServerProbe::unavailable("codex-version-too-old", provider_version);
    }
    let Some(output) = bounded_command_output(agent_bin, &["app-server", "--help"]) else {
        return AppServerProbe::unavailable(
            "codex-app-server-transport-unavailable",
            provider_version,
        );
    };
    let advertised_transport = [output.stdout, output.stderr].into_iter().any(|bytes| {
        let text = String::from_utf8_lossy(&bytes);
        text.contains("--listen <URL>") && text.contains("unix://")
    });
    if !advertised_transport {
        return AppServerProbe::unavailable(
            "codex-app-server-transport-unavailable",
            provider_version,
        );
    }
    AppServerProbe {
        capabilities: AppServerCapabilities {
            transport: true,
            exact_attention: exact_attention_version_is_audited(version),
            source_guard: false,
        },
        provider_version,
        reason_code: None,
    }
}

fn bounded_command_output(agent_bin: &Path, args: &[&str]) -> Option<std::process::Output> {
    bounded_command_output_with_timeout(agent_bin, args, APP_SERVER_CAPABILITY_PROBE_TIMEOUT)
}

fn bounded_command_output_with_timeout(
    agent_bin: &Path,
    args: &[&str],
    timeout: Duration,
) -> Option<std::process::Output> {
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
    let mut stdout_pipe = child.stdout.take()?;
    let mut stderr_pipe = child.stderr.take()?;
    let (output_tx, output_rx) = std::sync::mpsc::channel();
    let stdout_tx = output_tx.clone();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout_pipe
            .by_ref()
            .take(APP_SERVER_CAPABILITY_PROBE_MAX_OUTPUT_BYTES)
            .read_to_end(&mut bytes);
        let _ = stdout_tx.send((true, bytes));
    });
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr_pipe
            .by_ref()
            .take(APP_SERVER_CAPABILITY_PROBE_MAX_OUTPUT_BYTES)
            .read_to_end(&mut bytes);
        let _ = output_tx.send((false, bytes));
    });

    let deadline = Instant::now() + timeout;
    let mut status = None;
    let mut stdout = None;
    let mut stderr = None;
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exit)) if exit.success() => status = Some(exit),
                Ok(Some(_)) | Err(_) => {
                    terminate_probe_process_group(&mut child);
                    return None;
                }
                Ok(None) => {}
            }
        }
        while let Ok((is_stdout, bytes)) = output_rx.try_recv() {
            if is_stdout {
                stdout = Some(bytes);
            } else {
                stderr = Some(bytes);
            }
        }
        if status.is_some() && stdout.is_some() && stderr.is_some() {
            return Some(std::process::Output {
                status: status.take().expect("checked probe status"),
                stdout: stdout.take().expect("checked probe stdout"),
                stderr: stderr.take().expect("checked probe stderr"),
            });
        }
        if Instant::now() >= deadline {
            terminate_probe_process_group(&mut child);
            return None;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn terminate_probe_process_group(child: &mut std::process::Child) {
    let pid = child.id();
    // SAFETY: the probe is launched as the leader of a fresh process group.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn parse_version_triplet(raw: &str) -> Option<(u64, u64, u64)> {
    let mut fields = raw.split_whitespace();
    if fields.next()? != "codex-cli" {
        return None;
    }
    let token = fields.next()?;
    if fields.next().is_some() {
        return None;
    }
    let mut parts = token.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
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

const STARTUP_DIAGNOSTIC_COLLECTOR_SCRIPT: &str = r#"collect_startup_diagnostic() {
  if (umask 077; tail -c 16384 > "$startup_diagnostic_buffer"); then
    runtime_exit="$(cat "$runtime_exit_status" 2>/dev/null)"
    if { [ "$(cat "$startup_stage" 2>/dev/null)" != initial_connection ] ||
         { [ -n "$runtime_exit" ] && [ "$runtime_exit" != 0 ]; }; } &&
       [ -s "$startup_diagnostic_buffer" ]; then
      mv -f "$startup_diagnostic_buffer" "$startup_diagnostic" 2>/dev/null ||
        rm -f -- "$startup_diagnostic_buffer"
    else
      rm -f -- "$startup_diagnostic_buffer"
    fi
    if [ "$runtime_exit" = 0 ]; then
      rm -f -- "$runtime_exit_status"
    fi
  else
    rm -f -- "$startup_diagnostic_buffer"
  fi
}"#;

pub(crate) fn launch_script() -> String {
    [
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
startup_dir="$state_dir/sessions/$session_id"
startup_stage="$startup_dir/.startup-stage"
startup_failure="$startup_dir/.startup-failure"
startup_diagnostic="$startup_dir/.startup-diagnostic.log"
runtime_exit_status="$startup_dir/.runtime-exit-status"
startup_diagnostic_buffer="$startup_dir/.startup-diagnostic.buffer"
startup_diagnostic_pipe="$startup_dir/.startup-diagnostic.pipe"
provider_stderr_pipe="$startup_dir/.provider-stderr.pipe"
"#,
        STARTUP_DIAGNOSTIC_COLLECTOR_SCRIPT,
        r#"
write_startup_marker() {
  (umask 077; printf '%s\n' "$2" > "$1") 2>/dev/null || true
}
record_startup_failure() {
  write_startup_marker "$startup_failure" "$1"
}
rm -f -- "$startup_failure" "$startup_diagnostic" "$runtime_exit_status" "$startup_diagnostic_buffer" "$startup_diagnostic_pipe" "$provider_stderr_pipe"
write_startup_marker "$startup_stage" app_server
if ! (umask 077; mkfifo "$startup_diagnostic_pipe"); then
  record_startup_failure startup-exited
  exit 1
fi
(umask 077; collect_startup_diagnostic < "$startup_diagnostic_pipe") &
diagnostic_pid=$!
rm -f -- "$socket" "$proxy" "$attached"
"$agent" app-server --listen "unix://$socket" </dev/null >/dev/null 2>"$startup_diagnostic_pipe" &
server=$!
proxy_pid=
provider_stderr_pid=
diagnostic_hold_open=
cleanup_started=
cleanup() {
  if [ -n "$cleanup_started" ]; then
    return
  fi
  cleanup_started=1
  trap - EXIT
  trap '' HUP INT TERM
  if [ -n "$diagnostic_hold_open" ]; then
    exec 9>&-
    diagnostic_hold_open=
  fi
  if [ -n "$proxy_pid" ]; then
    owned_pid=$proxy_pid
    proxy_pid=
    kill "$owned_pid" 2>/dev/null || true
    wait "$owned_pid" 2>/dev/null || true
  fi
  if [ -n "$server" ]; then
    owned_pid=$server
    server=
    kill "$owned_pid" 2>/dev/null || true
    wait "$owned_pid" 2>/dev/null || true
  fi
  if [ -n "$provider_stderr_pid" ]; then
    owned_pid=$provider_stderr_pid
    provider_stderr_pid=
    kill "$owned_pid" 2>/dev/null || true
    sleep 0.25
    kill -9 "$owned_pid" 2>/dev/null || true
    wait "$owned_pid" 2>/dev/null || true
  fi
  if [ -n "$diagnostic_pid" ]; then
    owned_pid=$diagnostic_pid
    diagnostic_pid=
    wait "$owned_pid" 2>/dev/null || true
  fi
  rm -f -- "$socket" "$proxy" "$handoff" "$attached" "$startup_diagnostic_buffer" "$startup_diagnostic_pipe" "$provider_stderr_pipe"
}
handle_signal() {
  signal_status=$1
  cleanup
  exit "$signal_status"
}
trap cleanup EXIT
trap 'handle_signal 129' HUP
trap 'handle_signal 130' INT
trap 'handle_signal 143' TERM
i=0
while [ ! -S "$socket" ]; do
  if ! kill -0 "$server" 2>/dev/null; then
    record_startup_failure app-server-start-failed
    exit 1
  fi
  i=$((i + 1))
  if [ "$i" -ge 100 ]; then
    record_startup_failure startup-timeout
    exit 1
  fi
  sleep 0.05
done
write_startup_marker "$startup_stage" proxy
(umask 077; exec "$proxy_bin" --state-dir "$state_dir" codex-app-server-proxy --id "$session_id" --upstream "$socket" --listen "$proxy" </dev/null >/dev/null 2>"$startup_diagnostic_pipe") &
proxy_pid=$!
i=0
while [ ! -S "$proxy" ]; do
  if ! kill -0 "$proxy_pid" 2>/dev/null; then
    if [ ! -x "$proxy_bin" ]; then
      record_startup_failure runtime-helper-unavailable
    else
      record_startup_failure proxy-start-failed
    fi
    exit 1
  fi
  i=$((i + 1))
  if [ "$i" -ge 100 ]; then
    record_startup_failure startup-timeout
    exit 1
  fi
  sleep 0.05
done
write_startup_marker "$startup_stage" provider_client
if ! (umask 077; mkfifo "$provider_stderr_pipe"); then
  record_startup_failure startup-exited
  exit 1
fi
tee "$startup_diagnostic_pipe" < "$provider_stderr_pipe" >&2 &
provider_stderr_pid=$!
if ! exec 9>"$startup_diagnostic_pipe"; then
  record_startup_failure startup-exited
  exit 1
fi
diagnostic_hold_open=1
"$agent" -c check_for_update_on_startup=false --remote "unix://$proxy" "$@" 9>&- 2>"$provider_stderr_pipe"
status=$?
write_startup_marker "$runtime_exit_status" "$status"
exec 9>&-
diagnostic_hold_open=
sleep 0.25
kill "$provider_stderr_pid" 2>/dev/null || true
sleep 0.25
kill -9 "$provider_stderr_pid" 2>/dev/null || true
owned_pid=$provider_stderr_pid
provider_stderr_pid=
wait "$owned_pid" 2>/dev/null || true
rm -f -- "$provider_stderr_pipe"
if [ "$(cat "$startup_stage" 2>/dev/null)" != initial_connection ]; then
  record_startup_failure provider-client-exited
fi
exit "$status"
"#,
    ]
    .concat()
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

#[derive(Debug, Deserialize, Serialize)]
struct RuntimeProcessMarker {
    schema_version: String,
    launch_id: String,
    token: String,
    owner_pid: u32,
    expires_at_epoch_ms: u64,
}

#[derive(Debug)]
pub(crate) struct ManualInputMarker {
    path: PathBuf,
    token: String,
    _owner_file: fs::File,
    gate_path: Option<PathBuf>,
    ack_path: Option<PathBuf>,
    ack_socket: Option<UnixDatagram>,
    cleanup_on_drop: bool,
}

impl ManualInputMarker {
    pub(crate) fn finish(mut self, release_lifecycle_lock: impl FnOnce()) {
        self.finish_with_timeout(release_lifecycle_lock, MANUAL_INPUT_GATE_TIMEOUT);
    }

    fn finish_with_timeout(&mut self, release_lifecycle_lock: impl FnOnce(), timeout: Duration) {
        if let Some(socket) = self.ack_socket.as_ref() {
            let mut ack = [0_u8; 1];
            let _ = socket.recv(&mut ack);
        }
        let gate = self
            .gate_path
            .as_deref()
            .and_then(open_manual_input_gate_file)
            .filter(|file| lock_file_timed(file, timeout));
        if let Some(gate) = gate {
            self.remove_if_owned();
            self.remove_ack_path();
            release_lifecycle_lock();
            unlock_bootstrap_file(&gate);
        } else {
            // Invalidate before releasing lifecycle state. Even if unlink
            // fails, dropping this marker releases the continuously held owner
            // lease, so stale bytes cannot authorize a future Busy result.
            self.remove_if_owned();
            self.remove_ack_path();
            release_lifecycle_lock();
        }
        self.cleanup_on_drop = false;
    }

    fn remove_ack_path(&mut self) {
        if let Some(path) = self.ack_path.take() {
            let _ = fs::remove_file(path);
        }
    }

    fn remove_if_owned(&mut self) {
        let owned =
            read_runtime_process_marker(&self.path).is_some_and(|owner| owner.token == self.token);
        if owned {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl Drop for ManualInputMarker {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            self.remove_if_owned();
        }
        self.remove_ack_path();
    }
}

struct ProxyCapabilityGuard {
    _marker: ManualInputMarker,
}

fn epoch_millis() -> Option<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    u64::try_from(millis).ok()
}

fn manual_input_section_path(context: &CliContext, record: &SessionRecord) -> PathBuf {
    crate::session_dir(context, &record.id).join(MANUAL_INPUT_SECTION_FILE)
}

fn manual_input_gate_path(context: &CliContext, record: &SessionRecord) -> PathBuf {
    crate::session_dir(context, &record.id).join(MANUAL_INPUT_GATE_FILE)
}

fn manual_input_ack_path(record: &SessionRecord) -> Option<PathBuf> {
    proxy_path(record).map(|path| path.with_extension("ack"))
}

fn proxy_capability_path(context: &CliContext, record: &SessionRecord) -> PathBuf {
    crate::session_dir(context, &record.id).join(PROXY_CAPABILITY_FILE)
}

fn read_runtime_process_marker(path: &Path) -> Option<RuntimeProcessMarker> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > 1024 {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn read_runtime_process_marker_file(file: &mut fs::File) -> Option<RuntimeProcessMarker> {
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() || metadata.len() > 1024 {
        return None;
    }
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).ok()?);
    file.read_to_end(&mut bytes).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn process_is_live(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 probes process existence without delivering a signal.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn valid_runtime_process_marker(
    marker: &RuntimeProcessMarker,
    record: &SessionRecord,
    schema_version: &str,
    ttl: Duration,
) -> bool {
    let Some(runtime) = record.runtime.as_ref() else {
        return false;
    };
    let Some(now) = epoch_millis() else {
        return false;
    };
    let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
    marker.schema_version == schema_version
        && marker.launch_id == runtime.launch_id
        && uuid::Uuid::parse_str(&marker.token).is_ok()
        && process_is_live(marker.owner_pid)
        && marker.expires_at_epoch_ms >= now
        && marker.expires_at_epoch_ms <= now.saturating_add(ttl_ms)
}

fn write_runtime_process_marker(
    path: PathBuf,
    record: &SessionRecord,
    schema_version: &str,
    ttl: Duration,
) -> Result<ManualInputMarker, CliError> {
    let launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
        .ok_or_else(|| {
            CliError::data(
                "runtime-id-missing",
                "session runtime is missing its launch metadata",
                Some(json!({ "id": record.id })),
            )
        })?;
    let now = epoch_millis().ok_or_else(|| {
        CliError::runtime(
            "codex-input-section-time-unavailable",
            "system time is unavailable for Codex manual input",
            Some(json!({ "id": record.id })),
        )
    })?;
    let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
    let token = uuid::Uuid::new_v4().to_string();
    let marker = RuntimeProcessMarker {
        schema_version: schema_version.to_string(),
        launch_id,
        token: token.clone(),
        owner_pid: std::process::id(),
        expires_at_epoch_ms: now.saturating_add(ttl_ms),
    };
    let bytes = serde_json::to_vec(&marker).map_err(|err| {
        CliError::runtime(
            "codex-input-section-encode-failed",
            format!("failed to encode Codex manual input state: {err}"),
            Some(json!({ "id": record.id })),
        )
    })?;
    write_private_file(&path, &bytes)?;
    let file = fs::File::open(&path).map_err(|err| {
        CliError::runtime(
            "codex-input-section-open-failed",
            format!("failed to open Codex manual input state: {err}"),
            Some(json!({ "id": record.id })),
        )
    })?;
    // The marker inode is also an owner lease. A proxy authorizes Busy only
    // while this shared lock is continuously held by the serialized sender.
    // SAFETY: `flock` observes the valid descriptor borrowed for this call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) } != 0 {
        return Err(CliError::runtime(
            "codex-input-section-lease-failed",
            "failed to lock Codex manual input state",
            Some(json!({ "id": record.id })),
        ));
    }
    Ok(ManualInputMarker {
        path,
        token,
        _owner_file: file,
        gate_path: None,
        ack_path: None,
        ack_socket: None,
        cleanup_on_drop: true,
    })
}

pub(crate) fn input_contains_submission(
    text: Option<&str>,
    keys: &[crate::cli::SpecialKey],
) -> bool {
    text.is_some_and(|text| matches!(text, "\r" | "\n" | "\r\n"))
        || keys.contains(&crate::cli::SpecialKey::Enter)
}

pub(crate) fn ensure_manual_input_capability(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<(), CliError> {
    crate::codex_account::ensure_input_allowed(record)?;
    if !runtime_is_supported(record) {
        return Ok(());
    }
    let deadline = Instant::now() + PROXY_CAPABILITY_READY_TIMEOUT;
    loop {
        if live_proxy_capability(context, record) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(CliError::runtime(
        "codex-input-section-unavailable",
        "the Codex proxy did not advertise serialized input support; retry, then recreate the session if it persists",
        Some(json!({ "id": record.id, "retryable": true })),
    ))
}

fn live_proxy_capability(context: &CliContext, record: &SessionRecord) -> bool {
    let path = proxy_capability_path(context, record);
    let Ok(mut file) = fs::File::open(&path) else {
        return false;
    };
    let (Ok(own), Ok(current)) = (file.metadata(), fs::metadata(&path)) else {
        return false;
    };
    if own.dev() != current.dev() || own.ino() != current.ino() {
        return false;
    }
    // A live proxy holds a shared lock for its complete advertised lifetime.
    // Acquiring an exclusive lock therefore identifies an unlocked stale file.
    // SAFETY: `flock` observes the valid descriptor borrowed for this call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        unlock_bootstrap_file(&file);
        return false;
    }
    if std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock {
        return false;
    }
    read_runtime_process_marker_file(&mut file).is_some_and(|marker| {
        valid_runtime_process_marker(
            &marker,
            record,
            PROXY_CAPABILITY_VERSION,
            PROXY_CAPABILITY_TTL,
        )
    })
}

pub(crate) fn begin_manual_input_section(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<Option<ManualInputMarker>, CliError> {
    if !runtime_is_supported(record) {
        return Ok(None);
    }
    ensure_manual_input_capability(context, record)?;
    let mut marker = write_runtime_process_marker(
        manual_input_section_path(context, record),
        record,
        MANUAL_INPUT_SECTION_VERSION,
        MANUAL_INPUT_SECTION_TTL,
    )?;
    let gate_path = manual_input_gate_path(context, record);
    open_manual_input_gate_file(&gate_path).ok_or_else(|| {
        CliError::runtime(
            "codex-input-gate-open-failed",
            "failed to open the Codex manual input gate",
            Some(json!({ "id": record.id })),
        )
    })?;
    marker.gate_path = Some(gate_path);
    let ack_path = manual_input_ack_path(record).ok_or_else(|| {
        CliError::data(
            "codex-input-ack-path-missing",
            "Codex runtime is missing its private input acknowledgement path",
            Some(json!({ "id": record.id })),
        )
    })?;
    let _ = fs::remove_file(&ack_path);
    let ack_socket = UnixDatagram::bind(&ack_path).map_err(|err| {
        CliError::runtime(
            "codex-input-ack-bind-failed",
            format!("failed to bind the Codex manual input acknowledgement: {err}"),
            Some(json!({ "id": record.id })),
        )
    })?;
    ack_socket
        .set_read_timeout(Some(MANUAL_INPUT_ACK_TIMEOUT))
        .map_err(|err| {
            CliError::runtime(
                "codex-input-ack-timeout-failed",
                format!("failed to bound the Codex manual input acknowledgement: {err}"),
                Some(json!({ "id": record.id })),
            )
        })?;
    marker.ack_path = Some(ack_path);
    marker.ack_socket = Some(ack_socket);
    Ok(Some(marker))
}

fn manual_input_request_matches_bound_thread(
    context: &CliContext,
    record: &SessionRecord,
    value: &Value,
) -> bool {
    if value.get("method").and_then(Value::as_str) != Some("turn/start")
        || value.get("id").and_then(json_id_key).is_none()
        || !value.pointer("/params/input").is_some_and(Value::is_array)
    {
        return false;
    }
    let Some(thread_id) = value.pointer("/params/threadId").and_then(Value::as_str) else {
        return false;
    };
    if !protocol_id_is_valid(thread_id) {
        return false;
    }
    let Some(attached) = thread_attached_path(record) else {
        return false;
    };
    if fs::read_to_string(attached).ok().as_deref() != Some(&projected_thread_binding(thread_id)) {
        return false;
    }
    let _ = context;
    true
}

struct ManualInputGate {
    gate_file: fs::File,
    _owner_file: fs::File,
}

impl Drop for ManualInputGate {
    fn drop(&mut self) {
        unlock_bootstrap_file(&self.gate_file);
    }
}

fn open_manual_input_gate_file(path: &Path) -> Option<fs::File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .ok()
}

fn lock_file_timed(file: &fs::File, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: `flock` observes the valid descriptor borrowed for this call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return true;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock || Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn marker_has_live_shared_lock(file: &fs::File) -> bool {
    // SAFETY: `flock` observes the valid descriptor borrowed for this call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        unlock_bootstrap_file(file);
        return false;
    }
    std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock
}

fn acquire_manual_input_gate(
    context: &CliContext,
    record: &SessionRecord,
    value: &Value,
) -> Option<ManualInputGate> {
    if !manual_input_request_matches_bound_thread(context, record, value) {
        return None;
    }
    let path = manual_input_section_path(context, record);
    let mut owner_file = fs::File::open(&path).ok()?;
    if !marker_has_live_shared_lock(&owner_file) {
        return None;
    }
    let gate_file = open_manual_input_gate_file(&manual_input_gate_path(context, record))?;
    if !lock_file_timed(&gate_file, MANUAL_INPUT_GATE_TIMEOUT) {
        return None;
    }
    let own = owner_file.metadata().ok()?;
    let current = fs::metadata(&path).ok()?;
    if own.dev() != current.dev() || own.ino() != current.ino() {
        return None;
    }
    if !marker_has_live_shared_lock(&owner_file) {
        return None;
    }
    let marker = read_runtime_process_marker_file(&mut owner_file)?;
    if !valid_runtime_process_marker(
        &marker,
        record,
        MANUAL_INPUT_SECTION_VERSION,
        MANUAL_INPUT_SECTION_TTL,
    ) {
        return None;
    }
    UnixDatagram::unbound()
        .ok()?
        .send_to(&[1], manual_input_ack_path(record)?)
        .ok()?;
    Some(ManualInputGate {
        gate_file,
        _owner_file: owner_file,
    })
}

fn begin_proxy_capability(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<ProxyCapabilityGuard, CliError> {
    let mut marker = write_runtime_process_marker(
        proxy_capability_path(context, record),
        record,
        PROXY_CAPABILITY_VERSION,
        PROXY_CAPABILITY_TTL,
    )?;
    // An unlocked capability file is harmless stale state and may be replaced
    // by the next proxy. Avoid racy pathname cleanup across proxy generations.
    marker.cleanup_on_drop = false;
    Ok(ProxyCapabilityGuard { _marker: marker })
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
fn normal_cancellation_attempts()
-> &'static std::sync::Mutex<std::collections::HashMap<String, usize>> {
    static ATTEMPTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, usize>>,
    > = std::sync::OnceLock::new();
    ATTEMPTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

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

pub(crate) fn external_auth_login_request(
    id: u64,
    access_token: &str,
    chatgpt_account_id: &str,
    chatgpt_plan_type: Option<&str>,
) -> Value {
    json!({
        "id": id,
        "method": "account/login/start",
        "params": {
            "type": "chatgptAuthTokens",
            "accessToken": access_token,
            "chatgptAccountId": chatgpt_account_id,
            "chatgptPlanType": chatgpt_plan_type
        }
    })
}

pub(crate) fn external_auth_refresh_response(
    id: Value,
    access_token: &str,
    chatgpt_account_id: &str,
    chatgpt_plan_type: Option<&str>,
) -> Value {
    json!({
        "id": id,
        "result": {
            "accessToken": access_token,
            "chatgptAccountId": chatgpt_account_id,
            "chatgptPlanType": chatgpt_plan_type
        }
    })
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
            soonest_reset_epoch: None,
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
                soonest_reset_epoch: None,
            };
        }
    }
    for snapshot in snapshots {
        if !snapshot.is_object() {
            return UsageSnapshot {
                authoritative: false,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
                soonest_reset_epoch: None,
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
                    soonest_reset_epoch: None,
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
        soonest_reset_epoch: None,
    }
}

#[derive(Clone)]
pub(crate) struct ControlHandle {
    sender: mpsc::Sender<ControlCommand>,
}

pub(crate) enum ControlCommand {
    Usage(oneshot::Sender<Result<UsageSnapshot, String>>),
    Prompt {
        message: String,
        response: oneshot::Sender<Result<String, String>>,
    },
    Continue {
        message: String,
        response: oneshot::Sender<Result<String, String>>,
    },
    BindAccount {
        account: String,
        revision: u64,
        response: oneshot::Sender<Result<crate::codex_account::CodexAccountView, String>>,
    },
    /// Drain and apply a queued next-account intent if the turn is idle. Used by
    /// the periodic idle-boundary drive; a no-op when nothing is drainable.
    ApplyNext {
        response: oneshot::Sender<Result<(), String>>,
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

    pub(crate) async fn submit_prompt(&self, message: &str) -> Result<String, String> {
        let (response, receive) = oneshot::channel();
        tokio::time::timeout(CONTROL_SUBMIT_TOTAL_TIMEOUT, async {
            self.sender
                .send(ControlCommand::Prompt {
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

    pub(crate) async fn bind_account(
        &self,
        account: &str,
        revision: u64,
    ) -> Result<crate::codex_account::CodexAccountView, String> {
        let (response, receive) = oneshot::channel();
        tokio::time::timeout(CONTROL_SUBMIT_TOTAL_TIMEOUT, async {
            self.sender
                .send(ControlCommand::BindAccount {
                    account: account.to_string(),
                    revision,
                    response,
                })
                .await
                .map_err(|_| "codex control connection unavailable".to_string())?;
            receive
                .await
                .map_err(|_| "codex control connection closed".to_string())?
        })
        .await
        .map_err(|_| "Codex account binding timed out".to_string())?
    }

    pub(crate) async fn apply_next(&self) -> Result<(), String> {
        let (response, receive) = oneshot::channel();
        tokio::time::timeout(CONTROL_SUBMIT_TOTAL_TIMEOUT, async {
            self.sender
                .send(ControlCommand::ApplyNext { response })
                .await
                .map_err(|_| "codex control connection unavailable".to_string())?;
            receive
                .await
                .map_err(|_| "codex control connection closed".to_string())?
        })
        .await
        .map_err(|_| "Codex next-account apply timed out".to_string())?
    }
}

pub(crate) fn control_channel() -> (ControlHandle, mpsc::Receiver<ControlCommand>) {
    let (sender, receive) = mpsc::channel(4);
    (ControlHandle { sender }, receive)
}

/// Resolve credentials and drive the app-server external-auth login for
/// `account`. Mutates only the live app-server; never touches durable binding
/// or next-intent state. Returns a fail-closed reason code on failure.
async fn drive_external_auth_login(
    websocket: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
    request_id: &mut u64,
    account: &str,
) -> Result<(), &'static str> {
    let resolve_account = account.to_string();
    let credentials = tokio::task::spawn_blocking(move || {
        crate::codex_account::resolve_account(&resolve_account, false)
    })
    .await
    .map_err(|_| "broker_failed")?
    .map_err(|_| "broker_failed")?;

    *request_id = request_id.saturating_add(1);
    if send_json(
        websocket,
        external_auth_login_request(
            *request_id,
            &credentials.access_token,
            &credentials.chatgpt_account_id,
            credentials.chatgpt_plan_type.as_deref(),
        ),
    )
    .await
    .is_err()
    {
        return Err("apply_failed");
    }
    let result =
        receive_response_with_timeout(websocket, *request_id, None, None, CONTROL_RESPONSE_TIMEOUT)
            .await;
    if !result
        .as_ref()
        .is_ok_and(|result| result.get("type").and_then(Value::as_str) == Some("chatgptAuthTokens"))
    {
        return Err("apply_failed");
    }
    Ok(())
}

async fn apply_account_binding(
    websocket: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
    context: &CliContext,
    record: &SessionRecord,
    request_id: &mut u64,
    account: &str,
    revision: u64,
) -> Result<crate::codex_account::CodexAccountView, String> {
    let launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
        .ok_or_else(|| "Codex runtime identity is missing".to_string())?;
    match drive_external_auth_login(websocket, request_id, account).await {
        Ok(()) => {
            finish_account_binding(context, record, &launch_id, account, revision, Ok(())).await
        }
        Err(reason) => {
            let _ =
                finish_account_binding(context, record, &launch_id, account, revision, Err(reason))
                    .await;
            Err(format!("Codex external-auth login failed: {reason}"))
        }
    }
}

/// At the idle boundary, apply a queued next-account intent before the next
/// prompt: transition it to `applying`, drive the app-server login, and record
/// success (which flips the applied binding and clears the intent) or failure
/// (which marks the intent failed and keeps the prompt fenced). Returns the
/// account now applied to the live runtime, if it changed.
async fn apply_pending_next_account(
    websocket: &mut tokio_tungstenite::WebSocketStream<UnixStream>,
    context: &CliContext,
    record: &SessionRecord,
    request_id: &mut u64,
) -> Option<String> {
    let launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())?;
    // Only drain while the turn is authoritatively idle.
    let idle_context = context.clone();
    let idle_id = record.id.clone();
    let idle_launch = launch_id.clone();
    let idle = tokio::task::spawn_blocking(move || {
        let current = crate::load_session_record(&idle_context, &idle_id).ok()?;
        if current
            .runtime
            .as_ref()
            .is_none_or(|runtime| runtime.launch_id != idle_launch)
        {
            return None;
        }
        if !matches!(
            crate::codex_account::pending_next_apply(&current),
            Ok(Some(_))
        ) {
            return None;
        }
        crate::activity::state_for_view(&idle_context, &current).map(|state| state.phase)
    })
    .await
    .ok()
    .flatten();
    if idle != Some(crate::activity::TurnPhase::Waiting) {
        return None;
    }

    let begin_context = context.clone();
    let begin_id = record.id.clone();
    let begin_launch = launch_id.clone();
    let queued = tokio::task::spawn_blocking(move || {
        crate::codex_account::begin_next_apply(&begin_context, &begin_id, &begin_launch)
    })
    .await
    .ok()?;
    let (account, revision) = match queued {
        Ok(Some(pair)) => pair,
        Ok(None) => return None,
        Err(_) => return None, // malformed intent stays fenced for explicit repair
    };

    let outcome = drive_external_auth_login(websocket, request_id, &account).await;
    let succeeded = outcome.is_ok();
    let finish_context = context.clone();
    let finish_id = record.id.clone();
    let finish_launch = launch_id.clone();
    let finish_account = account.clone();
    let finished = tokio::task::spawn_blocking(move || {
        crate::codex_account::finish_next_apply(
            &finish_context,
            &finish_id,
            &finish_launch,
            &finish_account,
            revision,
            outcome,
        )
    })
    .await;
    match finished {
        Ok(Ok(_)) if succeeded => Some(account),
        _ => None,
    }
}

async fn finish_account_binding(
    context: &CliContext,
    record: &SessionRecord,
    launch_id: &str,
    account: &str,
    revision: u64,
    result: Result<(), &'static str>,
) -> Result<crate::codex_account::CodexAccountView, String> {
    let finish_context = context.clone();
    let finish_id = record.id.clone();
    let finish_launch_id = launch_id.to_string();
    let finish_account = account.to_string();
    tokio::task::spawn_blocking(move || {
        crate::codex_account::finish_binding(
            &finish_context,
            &finish_id,
            &finish_launch_id,
            &finish_account,
            revision,
            result,
        )
    })
    .await
    .map_err(|_| "Codex account binding worker failed".to_string())?
    .map_err(|err| format!("Codex account binding persistence failed: {}", err.code()))
}

async fn control_account_ready(
    context: &CliContext,
    record: &SessionRecord,
    external_auth_account: Option<&str>,
) -> Result<(), String> {
    let check_context = context.clone();
    let id = record.id.clone();
    let launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
        .ok_or_else(|| "Codex runtime identity is missing".to_string())?;
    let selected = tokio::task::spawn_blocking(move || {
        let current = crate::load_session_record(&check_context, &id)?;
        if current
            .runtime
            .as_ref()
            .is_none_or(|runtime| runtime.launch_id != launch_id)
        {
            return Err(CliError::runtime(
                "codex-account-runtime-changed",
                "Codex session runtime changed while checking its account binding",
                Some(json!({ "id": current.id })),
            ));
        }
        crate::codex_account::ensure_input_allowed(&current)?;
        Ok::<_, CliError>(crate::codex_account::selected_account(&current))
    })
    .await
    .map_err(|_| "Codex account binding worker failed".to_string())?
    .map_err(|err| format!("Codex account binding is not ready: {}", err.code()))?;
    if selected.as_deref() == external_auth_account
        || (selected.is_none() && external_auth_account.is_none())
    {
        Ok(())
    } else {
        Err("Codex account binding is not ready".to_string())
    }
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
    receive_response_with_timeout(
        &mut websocket,
        request_id,
        None,
        None,
        CONTROL_RESPONSE_TIMEOUT,
    )
    .await
    .map_err(|err| format!("initialize failed: {err}"))?;
    send_json(&mut websocket, initialized_notification()).await?;
    let mut external_auth_account = None;
    if let Some((account, revision)) = crate::codex_account::account_for_control_rebind(&record)
        .map_err(|err| format!("Codex account binding is invalid: {}", err.code()))?
    {
        match apply_account_binding(
            &mut websocket,
            &context,
            &record,
            &mut request_id,
            &account,
            revision,
        )
        .await
        {
            Ok(_) => external_auth_account = Some(account),
            Err(error) => eprintln!("warning: Codex account binding failed: {error}"),
        }
    }
    if crate::codex_account::binding_is_present(&record) && external_auth_account.is_none() {
        loop {
            let command = tokio::select! {
                command = commands.recv() => command,
                message = websocket.next() => {
                    let value = decode_message(message).await?;
                    if respond_to_external_auth_refresh(&mut websocket, &value, None).await? {
                        continue;
                    }
                    continue;
                }
            };
            let Some(command) = command else {
                return Ok(());
            };
            match command {
                ControlCommand::BindAccount {
                    account,
                    revision,
                    response,
                } => {
                    let result = apply_account_binding(
                        &mut websocket,
                        &context,
                        &record,
                        &mut request_id,
                        &account,
                        revision,
                    )
                    .await;
                    match result {
                        Ok(view) => {
                            external_auth_account = Some(account);
                            let _ = response.send(Ok(view));
                            break;
                        }
                        Err(error) => {
                            let _ = response.send(Err(error));
                        }
                    }
                }
                ControlCommand::Usage(response) => {
                    let _ = response.send(Err(
                        "Codex account binding is not ready; retry the account switch".to_string(),
                    ));
                }
                ControlCommand::Prompt { response, .. }
                | ControlCommand::Continue { response, .. } => {
                    let _ = response.send(Err(
                        "Codex account binding is not ready; retry the account switch".to_string(),
                    ));
                }
                ControlCommand::ApplyNext { response } => {
                    let _ = response.send(Err(
                        "Codex account binding is not ready; retry the account switch".to_string(),
                    ));
                }
            }
        }
    }

    let mut discovery_attempts = 0_u8;
    let thread_id = loop {
        request_id = request_id.saturating_add(1);
        send_json(&mut websocket, loaded_threads_request(request_id)).await?;
        let result = receive_response_with_timeout(
            &mut websocket,
            request_id,
            None,
            external_auth_account
                .as_deref()
                .map(|account| (&context, &record, account)),
            CONTROL_RESPONSE_TIMEOUT,
        )
        .await
        .map_err(|err| format!("thread/loaded/list failed: {err}"))?;
        let ids = loaded_thread_ids(&result)
            .ok_or_else(|| "Codex loaded-thread response was malformed".to_string())?;
        if let Some(id) = attached_loaded_thread(&record, &ids)? {
            break id;
        }
        match ids.as_slice() {
            [id] => break id.clone(),
            _ if discovery_attempts < 100 => {
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
            external_auth_account
                .as_deref()
                .map(|account| (&context, &record, account)),
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
    if control_account_ready(&context, &record, external_auth_account.as_deref())
        .await
        .is_ok()
    {
        request_id = request_id.saturating_add(1);
        send_json(&mut websocket, rate_limits_request(request_id)).await?;
        let initial_usage = receive_response_with_timeout(
            &mut websocket,
            request_id,
            Some((&context, &record, &mut reducer)),
            external_auth_account
                .as_deref()
                .map(|account| (&context, &record, account)),
            CONTROL_RESPONSE_TIMEOUT,
        )
        .await
        .map(|value| usage_snapshot(&value))?;
        wake_from_open_usage(&context, &record, &initial_usage).await?;
    }

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()); };
                match command {
                    ControlCommand::Usage(response) => {
                        if let Err(error) = control_account_ready(
                            &context,
                            &record,
                            external_auth_account.as_deref(),
                        )
                        .await
                        {
                            let _ = response.send(Err(error));
                            continue;
                        }
                        request_id = request_id.saturating_add(1);
                        if let Err(err) = send_json(&mut websocket, rate_limits_request(request_id)).await {
                            let _ = response.send(Err(err));
                            return Err("Codex usage request write failed".to_string());
                        }
                        let result = receive_response_with_timeout(
                            &mut websocket,
                            request_id,
                            Some((&context, &record, &mut reducer)),
                            external_auth_account
                                .as_deref()
                                .map(|account| (&context, &record, account)),
                            CONTROL_RESPONSE_TIMEOUT,
                        ).await;
                        let _ = response.send(result.map(|value| usage_snapshot(&value)));
                    }
                    ControlCommand::Prompt { message, response } => {
                        if let Err(error) = control_account_ready(
                            &context,
                            &record,
                            external_auth_account.as_deref(),
                        )
                        .await
                        {
                            let _ = response.send(Err(error));
                            continue;
                        }
                        request_id = request_id.saturating_add(1);
                        if let Err(err) = send_json(
                            &mut websocket,
                            continuation_request(request_id, &thread_id, &message),
                        ).await {
                            let _ = response.send(Err(err));
                            return Err("Codex prompt request write failed".to_string());
                        }
                        let result = receive_response_with_timeout(
                            &mut websocket,
                            request_id,
                            Some((&context, &record, &mut reducer)),
                            external_auth_account
                                .as_deref()
                                .map(|account| (&context, &record, account)),
                            CONTROL_SUBMISSION_TIMEOUT,
                        ).await.and_then(|value| {
                            value.pointer("/turn/id")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                                .ok_or_else(|| "Codex turn/start response omitted the acknowledged turn id".to_string())
                        });
                        let _ = response.send(result);
                    }
                    ControlCommand::Continue { message, response } => {
                        if let Err(error) = control_account_ready(
                            &context,
                            &record,
                            external_auth_account.as_deref(),
                        )
                        .await
                        {
                            let _ = response.send(Err(error));
                            continue;
                        }
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
                                external_auth_account
                                    .as_deref()
                                    .map(|account| (&context, &record, account)),
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
                            external_auth_account
                                .as_deref()
                                .map(|account| (&context, &record, account)),
                            CONTROL_SUBMISSION_TIMEOUT,
                        ).await.and_then(|value| {
                            value.pointer("/turn/id")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                                .ok_or_else(|| "Codex turn/start response omitted the acknowledged turn id".to_string())
                        });
                        let _ = response.send(result);
                    }
                    ControlCommand::BindAccount { account, revision, response } => {
                        external_auth_account = None;
                        let result = apply_account_binding(
                            &mut websocket,
                            &context,
                            &record,
                            &mut request_id,
                            &account,
                            revision,
                        )
                        .await;
                        if result.is_ok() {
                            external_auth_account = Some(account);
                        }
                        let _ = response.send(result);
                    }
                    ControlCommand::ApplyNext { response } => {
                        if let Some(applied) = apply_pending_next_account(
                            &mut websocket, &context, &record, &mut request_id,
                        )
                        .await
                        {
                            external_auth_account = Some(applied);
                        }
                        let _ = response.send(Ok(()));
                    }
                }
            }
            message = websocket.next() => {
                let value = decode_message(message).await?;
                if respond_to_external_auth_refresh(
                    &mut websocket,
                    &value,
                    external_auth_account
                        .as_deref()
                        .map(|account| (&context, &record, account)),
                )
                .await?
                {
                    continue;
                }
                process_live_message(&context, &record, &mut reducer, None, &value).await?;
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

fn attached_loaded_thread(
    record: &SessionRecord,
    loaded_thread_ids: &[String],
) -> Result<Option<String>, String> {
    let attached = thread_attached_path(record)
        .ok_or_else(|| "Codex attached marker metadata is missing".to_string())?;
    let observed = match fs::read_to_string(attached) {
        Ok(observed) => observed,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Codex attached thread binding was unreadable".to_string()),
    };
    Ok(loaded_thread_ids
        .iter()
        .find(|thread_id| projected_thread_binding(thread_id) == observed)
        .cloned())
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
    pending_attention_requests: BTreeMap<String, String>,
    reducer: Option<FailureReducer>,
}

impl ProxyObserver {
    fn new() -> Self {
        Self {
            pending_thread_starts: BTreeSet::new(),
            pending_attention_requests: BTreeMap::new(),
            reducer: None,
        }
    }

    fn observe_client(&mut self, record: &SessionRecord, value: &Value) -> Result<(), String> {
        match value.get("method").and_then(Value::as_str) {
            Some("thread/start") => {
                track_thread_start(&mut self.pending_thread_starts, value);
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
        persisted_thread: Option<&str>,
    ) -> Result<(), String> {
        match (
            completed_thread_start(&mut self.pending_thread_starts, value),
            persisted_thread,
        ) {
            (Some(thread_id), Some(persisted_thread)) if thread_id == persisted_thread => {
                self.bind_persisted(thread_id)?;
            }
            (Some(thread_id), None) => self.bind(record, thread_id)?,
            (None, None) => {}
            _ => {
                return Err("Codex persisted thread binding did not match the response".to_string());
            }
        }
        if matches!(
            value.get("method").and_then(Value::as_str),
            Some("agent-session/attention/requested" | "agent-session/attention/resolved")
        ) && attention_authority(record) != ATTENTION_AUTHORITY_PROTOCOL
        {
            // Transport support and exact-attention completeness are separate
            // capabilities. Hook-authoritative app-server runtimes ignore the
            // private attention projection and keep lifecycle hooks as their
            // sole source.
            return Ok(());
        }
        if let Some(reducer) = self.reducer.as_mut() {
            process_live_message(
                context,
                record,
                reducer,
                Some(&mut self.pending_attention_requests),
                value,
            )
            .await?;
        } else if matches!(
            value.get("method").and_then(Value::as_str),
            Some("agent-session/attention/requested" | "agent-session/attention/resolved")
        ) {
            return Err(
                "Codex attention request arrived before the runtime thread was bound".to_string(),
            );
        }
        Ok(())
    }

    fn bind(&mut self, record: &SessionRecord, thread_id: &str) -> Result<(), String> {
        if self.reducer.is_some() {
            return self.bind_persisted(thread_id);
        }
        bind_thread(record, thread_id)?;
        self.bind_persisted(thread_id)
    }

    fn bind_persisted(&mut self, thread_id: &str) -> Result<(), String> {
        if let Some(reducer) = self.reducer.as_ref() {
            return (reducer.thread_id == thread_id)
                .then_some(())
                .ok_or_else(|| "Codex TUI proxy switched to a different thread".to_string());
        }
        self.reducer = Some(FailureReducer::new(thread_id));
        Ok(())
    }
}

fn track_thread_start(pending: &mut BTreeSet<String>, value: &Value) {
    let Some(key) = value.get("id").and_then(json_id_key) else {
        return;
    };
    if pending.len() >= MAX_REDUCER_PENDING_TURNS
        && !pending.contains(&key)
        && let Some(oldest) = pending.iter().next().cloned()
    {
        pending.remove(&oldest);
    }
    pending.insert(key);
}

fn completed_thread_start<'a>(pending: &mut BTreeSet<String>, value: &'a Value) -> Option<&'a str> {
    let key = value.get("id").and_then(json_id_key)?;
    if !pending.remove(&key) {
        return None;
    }
    value
        .pointer("/result/thread/id")
        .and_then(Value::as_str)
        .filter(|id| protocol_id_is_valid(id))
}

fn json_id_key(value: &Value) -> Option<String> {
    match value {
        Value::String(id) if id.len() <= MAX_PROTOCOL_ID_BYTES => Some(value.to_string()),
        Value::Number(_) => Some(value.to_string()),
        _ => None,
    }
}

fn attention_request_id_key(value: &Value) -> Option<String> {
    match value {
        Value::String(id) if !id.is_empty() && id.len() <= MAX_PROTOCOL_ID_BYTES => {
            Some(format!("string:{id}"))
        }
        Value::Number(number) => number.as_i64().map(|id| format!("int64:{id}")),
        _ => None,
    }
}

fn message_value(message: &Message) -> Result<Option<Value>, String> {
    match message {
        Message::Text(text) => serde_json::from_str(text)
            .map(Some)
            .map_err(|_| "proxy observed malformed JSON text".to_string()),
        Message::Binary(bytes) => serde_json::from_slice(bytes)
            .map(Some)
            .map_err(|_| "proxy observed malformed JSON binary data".to_string()),
        _ => Ok(None),
    }
}

enum ProxyObservation {
    Client(Value),
    Server {
        value: Value,
        persisted_thread: Option<String>,
        binding_ack: Option<oneshot::Sender<Result<(), String>>>,
    },
}

enum ServerProjection {
    Irrelevant,
    Repeatable(Value),
    Unique(Value),
    RejectedUnique,
}

type SharedFailCloseTask = Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>;

fn start_fail_close_task(task: &SharedFailCloseTask, context: &CliContext, record: &SessionRecord) {
    let mut task = task.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if task.is_none() {
        let context = context.clone();
        let record = record.clone();
        *task = Some(tokio::spawn(async move {
            fail_closed_projection(&context, &record).await;
        }));
    }
}

struct ProxyProjection {
    sender: Option<mpsc::Sender<ProxyObservation>>,
    task: Option<tokio::task::JoinHandle<()>>,
    fail_close_task: SharedFailCloseTask,
    pending_thread_starts: BTreeSet<String>,
    requires_thread_binding: bool,
    context: CliContext,
    record: SessionRecord,
}

impl ProxyProjection {
    fn new(context: CliContext, record: SessionRecord) -> Self {
        let (sender, mut receiver) = mpsc::channel(MAX_PROXY_OBSERVATIONS);
        let worker_context = context.clone();
        let worker_record = record.clone();
        let fail_close_task = Arc::new(Mutex::new(None));
        let worker_fail_close_task = fail_close_task.clone();
        let task = tokio::spawn(async move {
            let mut observer = ProxyObserver::new();
            while let Some(observation) = receiver.recv().await {
                let (result, binding_ack) = match observation {
                    ProxyObservation::Client(value) => {
                        (observer.observe_client(&worker_record, &value), None)
                    }
                    ProxyObservation::Server {
                        value,
                        persisted_thread,
                        binding_ack,
                    } => (
                        observer
                            .observe_server(
                                &worker_context,
                                &worker_record,
                                &value,
                                persisted_thread.as_deref(),
                            )
                            .await,
                        binding_ack,
                    ),
                };
                if let Some(binding_ack) = binding_ack {
                    let _ = binding_ack.send(result.clone());
                }
                if let Err(error) = result {
                    eprintln!("warning: Codex projection disabled: {error}");
                    start_fail_close_task(&worker_fail_close_task, &worker_context, &worker_record);
                    break;
                }
            }
        });
        Self {
            sender: Some(sender),
            task: Some(task),
            fail_close_task,
            pending_thread_starts: BTreeSet::new(),
            requires_thread_binding: thread_attached_path(&record)
                .is_some_and(|path| !path.is_file()),
            context,
            record,
        }
    }

    fn observe_client(&mut self, value: &Value) {
        if !self.is_active() {
            return;
        }
        if let Some(value) = client_observation(value) {
            if value.get("method").and_then(Value::as_str) == Some("thread/start") {
                track_thread_start(&mut self.pending_thread_starts, &value);
            }
            self.enqueue(ProxyObservation::Client(value));
        }
    }

    #[cfg(test)]
    fn observe_server(&mut self, value: &Value) {
        match server_observation(value) {
            ServerProjection::Irrelevant => {}
            ServerProjection::Repeatable(value) | ServerProjection::Unique(value) => {
                self.enqueue(ProxyObservation::Server {
                    value,
                    persisted_thread: None,
                    binding_ack: None,
                });
            }
            ServerProjection::RejectedUnique => {
                eprintln!("warning: Codex projection disabled: unique observation was invalid");
                self.disable();
            }
        }
    }

    async fn observe_server_before_forward(&mut self, value: &Value) -> Result<(), String> {
        if !self.is_active() {
            if self.requires_thread_binding {
                self.disable();
                return Err("Codex projection binding queue unavailable".to_string());
            }
            return Ok(());
        }
        match server_observation(value) {
            ServerProjection::Irrelevant => {}
            ServerProjection::Repeatable(value) => {
                self.enqueue(ProxyObservation::Server {
                    value,
                    persisted_thread: None,
                    binding_ack: None,
                });
            }
            ServerProjection::Unique(value) => {
                // A fresh TUI may submit its first turn as soon as it receives
                // the thread/start response. First require the projection
                // worker to accept the bound identity, then publish the marker
                // before forwarding the response. No new observation can make
                // the acknowledged worker fail while this proxy branch waits.
                let persisted_thread =
                    completed_thread_start(&mut self.pending_thread_starts, &value)
                        .map(str::to_string);
                let Some(persisted_thread) = persisted_thread else {
                    self.enqueue(ProxyObservation::Server {
                        value,
                        persisted_thread: None,
                        binding_ack: None,
                    });
                    return Ok(());
                };
                if !self.requires_thread_binding {
                    self.enqueue(ProxyObservation::Server {
                        value,
                        persisted_thread: None,
                        binding_ack: None,
                    });
                    return Ok(());
                }
                let (binding_ack, receive_ack) = oneshot::channel();
                let queued = self
                    .enqueue_binding(ProxyObservation::Server {
                        value,
                        persisted_thread: Some(persisted_thread.clone()),
                        binding_ack: Some(binding_ack),
                    })
                    .await;
                if let Err(error) = queued {
                    eprintln!("warning: Codex projection disabled: {error}");
                    self.disable();
                    return Err(error);
                }
                let acknowledged = tokio::time::timeout(CONTROL_RESPONSE_TIMEOUT, receive_ack)
                    .await
                    .map_err(|_| "Codex projection binding acknowledgement timed out".to_string())
                    .and_then(|result| {
                        result.map_err(|_| {
                            "Codex projection binding acknowledgement was unavailable".to_string()
                        })
                    })
                    .and_then(|result| result);
                if let Err(error) = acknowledged {
                    eprintln!("warning: Codex projection disabled: {error}");
                    self.disable();
                    return Err(error);
                }
                let record = self.record.clone();
                let worker_thread = persisted_thread;
                let result =
                    tokio::task::spawn_blocking(move || bind_thread(&record, &worker_thread))
                        .await
                        .map_err(|error| format!("Codex thread binding worker failed: {error}"))
                        .and_then(|result| result);
                if let Err(error) = result {
                    eprintln!("warning: Codex projection disabled: {error}");
                    self.disable();
                    return Err(error);
                }
                self.requires_thread_binding = false;
            }
            ServerProjection::RejectedUnique => {
                eprintln!("warning: Codex projection disabled: unique observation was invalid");
                let required_binding_rejected = self.requires_thread_binding;
                self.disable();
                if required_binding_rejected {
                    return Err("Codex projection required thread binding was invalid".to_string());
                }
            }
        }
        Ok(())
    }

    fn is_active(&mut self) -> bool {
        match self.sender.as_ref() {
            Some(sender) if !sender.is_closed() => true,
            Some(_) => {
                self.disable();
                false
            }
            None => false,
        }
    }

    fn enqueue(&mut self, observation: ProxyObservation) -> bool {
        let Some(sender) = self.sender.as_ref() else {
            return false;
        };
        match sender.try_send(observation) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(ProxyObservation::Server {
                value,
                persisted_thread: _,
                binding_ack: None,
            })) if value.get("method").and_then(Value::as_str)
                == Some("account/rateLimits/updated") =>
            {
                // Rate-limit updates are advisory and repeatable. The
                // scheduler rechecks scheduled claims, so a saturated queue
                // may coalesce this update without losing a unique event.
                true
            }
            Err(_) => {
                eprintln!("warning: Codex projection disabled: observation queue unavailable");
                self.disable();
                false
            }
        }
    }

    async fn enqueue_binding(&mut self, observation: ProxyObservation) -> Result<(), String> {
        let sender = self
            .sender
            .as_ref()
            .cloned()
            .ok_or_else(|| "Codex projection binding queue unavailable".to_string())?;
        tokio::time::timeout(CONTROL_RESPONSE_TIMEOUT, sender.send(observation))
            .await
            .map_err(|_| "Codex projection binding queue timed out".to_string())?
            .map_err(|_| "Codex projection binding queue unavailable".to_string())
    }

    fn disable(&mut self) {
        self.sender = None;
        self.pending_thread_starts.clear();
        if let Some(task) = self.task.take() {
            task.abort();
        }
        start_fail_close_task(&self.fail_close_task, &self.context, &self.record);
    }

    fn has_fail_close_task(&self) -> bool {
        self.fail_close_task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    fn take_fail_close_task(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.fail_close_task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    async fn finish_fail_close(&mut self) {
        self.disable();
        let mut retry = false;
        if let Some(mut task) = self.take_fail_close_task() {
            match tokio::time::timeout(CONTROL_RESPONSE_TIMEOUT, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => retry = true,
                Err(_) => {
                    // Dropping a JoinHandle detaches the retry. The durable
                    // unhealthy marker already makes activity and auto-resume
                    // fail closed while a contended session lock converges.
                }
            }
        }
        if retry {
            let context = self.context.clone();
            let record = self.record.clone();
            let mut task = tokio::spawn(async move {
                fail_closed_projection(&context, &record).await;
            });
            let _ = tokio::time::timeout(CONTROL_RESPONSE_TIMEOUT, &mut task).await;
        }
    }

    async fn finish(&mut self) {
        if self.has_fail_close_task() {
            self.finish_fail_close().await;
            return;
        }
        self.sender = None;
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(CONTROL_RESPONSE_TIMEOUT, &mut task)
                .await
                .is_err()
        {
            task.abort();
        }
        if self.has_fail_close_task() {
            self.finish_fail_close().await;
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

fn server_observation(value: &Value) -> ServerProjection {
    if let (Some(id), Some(thread_id)) = (value.get("id"), value.pointer("/result/thread/id")) {
        let Some(thread_id) = thread_id.as_str() else {
            return ServerProjection::RejectedUnique;
        };
        if json_id_key(id).is_none() || !protocol_id_is_valid(thread_id) {
            return ServerProjection::RejectedUnique;
        }
        return bounded_observation(json!({
            "id": id,
            "result": { "thread": { "id": thread_id } }
        }))
        .map(ServerProjection::Unique)
        .unwrap_or(ServerProjection::RejectedUnique);
    }
    let Some(method) = value.get("method").and_then(Value::as_str) else {
        return ServerProjection::Irrelevant;
    };
    if let Some(kind) = match method {
        "item/commandExecution/requestApproval"
        | "item/fileChange/requestApproval"
        | "item/permissions/requestApproval" => Some("approval"),
        "item/tool/requestUserInput" => Some("clarification"),
        "mcpServer/elicitation/request" => {
            match value.pointer("/params/mode").and_then(Value::as_str) {
                Some("form" | "openai/form") => Some("clarification"),
                Some("url") => Some("authentication"),
                _ => return ServerProjection::RejectedUnique,
            }
        }
        _ => None,
    } {
        let (Some(request_id), Some(thread_id)) = (
            value.get("id"),
            value.pointer("/params/threadId").and_then(Value::as_str),
        ) else {
            return ServerProjection::RejectedUnique;
        };
        let turn_id = value.pointer("/params/turnId");
        let turn_required = method != "mcpServer/elicitation/request";
        if attention_request_id_key(request_id).is_none()
            || !protocol_id_is_valid(thread_id)
            || (turn_required
                && !turn_id
                    .and_then(Value::as_str)
                    .is_some_and(protocol_id_is_valid))
            || (!turn_required
                && turn_id.is_some_and(|turn_id| {
                    !turn_id.is_null() && !turn_id.as_str().is_some_and(protocol_id_is_valid)
                }))
        {
            return ServerProjection::RejectedUnique;
        }
        return bounded_observation(json!({
            "method": "agent-session/attention/requested",
            "params": {
                "requestId": request_id,
                "threadId": thread_id,
                "turnId": turn_id,
                "kind": kind
            }
        }))
        .map(ServerProjection::Unique)
        .unwrap_or(ServerProjection::RejectedUnique);
    }
    if method == "serverRequest/resolved" {
        let (Some(request_id), Some(thread_id)) = (
            value.pointer("/params/requestId"),
            value.pointer("/params/threadId").and_then(Value::as_str),
        ) else {
            return ServerProjection::RejectedUnique;
        };
        if attention_request_id_key(request_id).is_none() || !protocol_id_is_valid(thread_id) {
            return ServerProjection::RejectedUnique;
        }
        return bounded_observation(json!({
            "method": "agent-session/attention/resolved",
            "params": {"requestId": request_id, "threadId": thread_id}
        }))
        .map(ServerProjection::Unique)
        .unwrap_or(ServerProjection::RejectedUnique);
    }
    let observation = match method {
        "error" => {
            let (Some(thread_id), Some(turn_id)) = (
                value.pointer("/params/threadId").and_then(Value::as_str),
                value.pointer("/params/turnId").and_then(Value::as_str),
            ) else {
                return ServerProjection::RejectedUnique;
            };
            if !protocol_id_is_valid(thread_id) || !protocol_id_is_valid(turn_id) {
                return ServerProjection::RejectedUnique;
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
            let (Some(thread_id), Some(turn_id)) = (
                value.pointer("/params/threadId").and_then(Value::as_str),
                value.pointer("/params/turn/id").and_then(Value::as_str),
            ) else {
                return ServerProjection::RejectedUnique;
            };
            if !protocol_id_is_valid(thread_id) || !protocol_id_is_valid(turn_id) {
                return ServerProjection::RejectedUnique;
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
        _ => return ServerProjection::Irrelevant,
    };
    match bounded_observation(observation) {
        Some(observation) if method == "account/rateLimits/updated" => {
            ServerProjection::Repeatable(observation)
        }
        Some(observation) => ServerProjection::Unique(observation),
        None if method == "account/rateLimits/updated" => ServerProjection::Irrelevant,
        None => ServerProjection::RejectedUnique,
    }
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
            crate::activity::mark_runtime_unhealthy(
                &context,
                &id,
                &launch_id,
                "codex_projection_unavailable",
            )?;
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
        let auto_resume_idle = auto_resume_is_healthy_idle(context, record);
        if record.provider_resume.is_none()
            && thread_attached_path(record).is_some_and(|path| !path.is_file())
            && starting
            && auto_resume_idle
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
        if !auto_resume_is_healthy_idle(context, record) {
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
    _manual_input_gate: Option<ManualInputGate>,
}

fn auto_resume_is_healthy_idle(context: &CliContext, record: &SessionRecord) -> bool {
    let view = crate::auto_resume::view_for_record(context, record);
    let idle_state_matches_enablement = match view.state.as_str() {
        "disabled" => !view.enabled,
        "enabled" => view.enabled,
        _ => false,
    };
    view.supported
        && idle_state_matches_enablement
        && view.scheduled_at.is_none()
        && view.failure_reason.is_none()
}

async fn cancel_before_tui_mutation(
    context: &CliContext,
    record: &SessionRecord,
    bootstrap: &mut FreshBootstrap,
    value: &Value,
) -> Option<MutationAuthorization> {
    let method = value.get("method").and_then(Value::as_str);
    if (crate::codex_account::binding_is_present(record)
        || crate::codex_account::view_for_record(record).supported)
        && matches!(
            method,
            Some("account/login/start" | "account/login/cancel" | "account/logout")
        )
    {
        bootstrap.close();
        return None;
    }
    if !matches!(method, Some("thread/start" | "turn/start")) {
        return Some(MutationAuthorization {
            _bootstrap_gate: None,
            _manual_input_gate: None,
        });
    }
    if method == Some("turn/start") {
        let account_context = context.clone();
        let account_id = record.id.clone();
        let allowed = tokio::task::spawn_blocking(move || {
            let current = crate::load_session_record(&account_context, &account_id)?;
            crate::codex_account::ensure_input_allowed(&current)
        })
        .await
        .ok()
        .and_then(Result::ok)
        .is_some();
        if !allowed {
            bootstrap.close();
            return None;
        }
    }
    // A fresh Codex TUI emits `thread/start`, then the initial prompt emits
    // `turn/start`, while the parent create path still owns the lifecycle lock.
    // The create-owned marker and exact healthy idle auto-resume state are
    // checked live for both requests. A per-device default may opt in before
    // the TUI creates its thread, but armed or failed continuation state must
    // still fail closed. The first turn is authorized only by a successful
    // matching thread/start response and must target the returned thread id.
    #[cfg(test)]
    {
        let mut attempts = normal_cancellation_attempts()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *attempts.entry(record.id.clone()).or_default() += 1;
    }
    let bootstrap_gate = if matches!(bootstrap, FreshBootstrap::Closed) {
        None
    } else {
        let gate_record = record.clone();
        tokio::task::spawn_blocking(move || acquire_create_bootstrap_gate(&gate_record))
            .await
            .ok()
            .flatten()
    };
    let manual_input_gate = if method == Some("turn/start") {
        let gate_context = context.clone();
        let gate_record = record.clone();
        let gate_value = value.clone();
        tokio::task::spawn_blocking(move || {
            acquire_manual_input_gate(&gate_context, &gate_record, &gate_value)
        })
        .await
        .ok()
        .flatten()
    } else {
        None
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
                _manual_input_gate: manual_input_gate,
            })
        }
        Some(crate::auto_resume::ManualInputCancelOutcome::Busy)
            if bootstrap_gate.is_some()
                && bootstrap.bypasses_create_lock(context, record, value) =>
        {
            Some(MutationAuthorization {
                _bootstrap_gate: bootstrap_gate,
                _manual_input_gate: manual_input_gate,
            })
        }
        Some(crate::auto_resume::ManualInputCancelOutcome::Busy) if manual_input_gate.is_some() => {
            bootstrap.close();
            Some(MutationAuthorization {
                _bootstrap_gate: bootstrap_gate,
                _manual_input_gate: manual_input_gate,
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

async fn send_proxy_upstream<S>(
    upstream: &mut S,
    message: Message,
    timeout: Duration,
) -> Result<(), String>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    tokio::time::timeout(timeout, upstream.send(message))
        .await
        .map_err(|_| "upstream app-server write timed out".to_string())?
        .map_err(|err| format!("upstream app-server write failed: {err}"))
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
    let _capability = begin_proxy_capability(&context, &record)
        .map_err(|err| format!("failed to advertise proxy capability: {}", err.code()))?;
    let _ = crate::write_private_file(
        &crate::session_dir(&context, &record.id).join(crate::STARTUP_STAGE_FILE),
        b"initial_connection\n",
    );
    let _ = fs::remove_file(
        crate::session_dir(&context, &record.id).join(crate::STARTUP_DIAGNOSTIC_FILE),
    );
    let mut projection = ProxyProjection::new(context.clone(), record.clone());
    let mut bootstrap = FreshBootstrap::for_runtime(&context, &record);
    let result = async {
        loop {
            tokio::select! {
            message = tui.next() => {
                let message = message
                    .ok_or_else(|| "remote TUI closed the proxy".to_string())?
                    .map_err(|err| format!("remote TUI read failed: {err}"))?;
                let authorization = if let Some(value) = message_value(&message)? {
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
                    MutationAuthorization {
                        _bootstrap_gate: None,
                        _manual_input_gate: None,
                    }
                };
                let closed = matches!(message, Message::Close(_));
                send_proxy_upstream(&mut upstream, message, CONTROL_RESPONSE_TIMEOUT).await?;
                drop(authorization);
                if closed {
                    return Ok(());
                }
            }
            message = upstream.next() => {
                let message = message
                    .ok_or_else(|| "upstream app-server closed the proxy".to_string())?
                    .map_err(|err| format!("upstream app-server read failed: {err}"))?;
                let observed = message.clone();
                let closed = matches!(message, Message::Close(_));
                if let Some(value) = message_value(&observed)? {
                    bootstrap.observe_server(&value);
                    projection.observe_server_before_forward(&value).await?;
                }
                tui.send(message).await
                    .map_err(|err| format!("remote TUI write failed: {err}"))?;
                if closed {
                    return Ok(());
                }
            }
        }
        }
    }
    .await;
    drop(listener);
    drop(_guard);
    drop(upstream);
    drop(tui);
    if result.is_err() {
        projection.finish_fail_close().await;
    } else {
        projection.finish().await;
    }
    result
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
    external_auth: Option<(&CliContext, &SessionRecord, &str)>,
    timeout: Duration,
) -> Result<Value, String>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    tokio::time::timeout(
        timeout,
        receive_response(websocket, id, live, external_auth),
    )
    .await
    .map_err(|_| "Codex app-server request timed out".to_string())?
}

async fn receive_response<S>(
    websocket: &mut S,
    id: u64,
    mut live: Option<(&CliContext, &SessionRecord, &mut FailureReducer)>,
    external_auth: Option<(&CliContext, &SessionRecord, &str)>,
) -> Result<Value, String>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    loop {
        let value = decode_message(websocket.next().await).await?;
        if respond_to_external_auth_refresh(websocket, &value, external_auth).await? {
            continue;
        }
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
            process_live_message(context, record, reducer, None, &value).await?;
        }
    }
}

async fn respond_to_external_auth_refresh<S>(
    websocket: &mut S,
    value: &Value,
    external_auth: Option<(&CliContext, &SessionRecord, &str)>,
) -> Result<bool, String>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    if value.get("method").and_then(Value::as_str) != Some("account/chatgptAuthTokens/refresh") {
        return Ok(false);
    }
    if value.pointer("/params/reason").and_then(Value::as_str) != Some("unauthorized") {
        return Err("Codex requested an unsupported external-auth refresh".to_string());
    }
    let id = value
        .get("id")
        .filter(|id| json_id_key(id).is_some())
        .cloned()
        .ok_or_else(|| "Codex external-auth refresh request omitted a valid id".to_string())?;
    let (context, record, account) = external_auth.ok_or_else(|| {
        "Codex requested an external-auth refresh without a bound account".to_string()
    })?;
    let launch_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
        .ok_or_else(|| "Codex runtime identity is missing".to_string())?;
    let begin_context = context.clone();
    let begin_id = record.id.clone();
    let begin_launch_id = launch_id.clone();
    let begin_account = account.to_string();
    let revision = tokio::task::spawn_blocking(move || {
        crate::codex_account::begin_binding(
            &begin_context,
            &begin_id,
            &begin_launch_id,
            &begin_account,
        )
    })
    .await
    .map_err(|_| "Codex account refresh worker failed".to_string())?
    .map_err(|err| format!("Codex account refresh rejected: {}", err.code()))?;
    let refresh_account = account.to_string();
    let credentials_result = tokio::task::spawn_blocking(move || {
        crate::codex_account::resolve_account(&refresh_account, true)
    })
    .await
    .map_err(|_| "Codex account refresh worker failed".to_string());
    let credentials = match credentials_result {
        Ok(Ok(credentials)) => credentials,
        Ok(Err(err)) => {
            let _ = finish_account_binding(
                context,
                record,
                &launch_id,
                account,
                revision,
                Err("refresh_failed"),
            )
            .await;
            return Err(format!("Codex account refresh failed: {}", err.code()));
        }
        Err(error) => {
            let _ = finish_account_binding(
                context,
                record,
                &launch_id,
                account,
                revision,
                Err("refresh_failed"),
            )
            .await;
            return Err(error);
        }
    };
    let fence_context = context.clone();
    let fence_id = record.id.clone();
    let fence_launch_id = launch_id.clone();
    let fence_account = account.to_string();
    let refresh_fence = tokio::task::spawn_blocking(move || {
        let lock = crate::acquire_session_record_lock(&fence_context, &fence_id)?;
        let current = crate::load_session_record(&fence_context, &fence_id)?;
        if current
            .runtime
            .as_ref()
            .is_none_or(|runtime| runtime.launch_id != fence_launch_id)
        {
            return Err(CliError::runtime(
                "codex-account-refresh-superseded",
                "Codex runtime changed during credential refresh",
                Some(json!({ "id": current.id })),
            ));
        }
        let view = crate::codex_account::view_for_record(&current);
        if view.state != "pending"
            || view.selected_account.as_deref() != Some(fence_account.as_str())
            || view.revision != revision
        {
            return Err(CliError::runtime(
                "codex-account-refresh-superseded",
                "Codex account binding changed during credential refresh",
                Some(json!({ "id": current.id })),
            ));
        }
        Ok::<_, CliError>(lock)
    })
    .await
    .map_err(|_| "Codex account refresh fence worker failed".to_string())?
    .map_err(|err| format!("Codex account refresh rejected: {}", err.code()))?;
    let send_result = send_json(
        websocket,
        external_auth_refresh_response(
            id,
            &credentials.access_token,
            &credentials.chatgpt_account_id,
            credentials.chatgpt_plan_type.as_deref(),
        ),
    )
    .await;
    drop(refresh_fence);
    if let Err(error) = send_result {
        let _ = finish_account_binding(
            context,
            record,
            &launch_id,
            account,
            revision,
            Err("refresh_failed"),
        )
        .await;
        return Err(error);
    }
    finish_account_binding(context, record, &launch_id, account, revision, Ok(())).await?;
    Ok(true)
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
    pending_attention_requests: Option<&mut BTreeMap<String, String>>,
    value: &Value,
) -> Result<(), String> {
    if matches!(
        value.get("method").and_then(Value::as_str),
        Some("agent-session/attention/requested" | "agent-session/attention/resolved")
    ) {
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .expect("matched attention method");
        let thread_id = value
            .pointer("/params/threadId")
            .and_then(Value::as_str)
            .ok_or_else(|| "Codex attention projection omitted thread scope".to_string())?;
        if thread_id != reducer.thread_id {
            return Err("Codex attention projection changed runtime thread scope".to_string());
        }
        let typed_request_id = value
            .pointer("/params/requestId")
            .and_then(attention_request_id_key)
            .ok_or_else(|| "Codex attention projection omitted typed request id".to_string())?;
        let requested_kind = (method == "agent-session/attention/requested")
            .then(|| {
                value
                    .pointer("/params/kind")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Codex attention projection omitted kind".to_string())
            })
            .transpose()?;
        let pending_attention_requests = pending_attention_requests.ok_or_else(|| {
            "Codex attention projection reached a non-authoritative channel".to_string()
        })?;
        let correlation_token = if requested_kind.is_some() {
            if pending_attention_requests.contains_key(&typed_request_id) {
                return Ok(());
            }
            if pending_attention_requests.len() >= MAX_PENDING_ATTENTION_REQUESTS {
                return Err(
                    "Codex attention request cardinality exceeded the bounded projection"
                        .to_string(),
                );
            }
            let token = uuid::Uuid::new_v4().to_string();
            pending_attention_requests.insert(typed_request_id, token.clone());
            token
        } else {
            let Some(token) = pending_attention_requests.remove(&typed_request_id) else {
                return Ok(());
            };
            token
        };
        let turn_id = value.pointer("/params/turnId").and_then(Value::as_str);
        let context = context.clone();
        let id = record.id.clone();
        let runtime_id = record
            .runtime
            .as_ref()
            .map(|runtime| runtime.launch_id.clone())
            .ok_or_else(|| "Codex runtime identity is missing".to_string())?;
        let thread_id = thread_id.to_string();
        let turn_id = turn_id.map(str::to_string);
        let requested_kind = requested_kind.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            crate::activity::ingest_codex_app_server_attention(
                &context,
                &id,
                &runtime_id,
                &thread_id,
                turn_id.as_deref(),
                &correlation_token,
                requested_kind.as_deref(),
            )
        })
        .await
        .map_err(|_| "Codex attention ingestion worker failed".to_string())?
        .map_err(|err| format!("Codex attention ingestion failed: {}", err.code()))?;
        return Ok(());
    }
    if value.get("method").and_then(Value::as_str) == Some("account/rateLimits/updated") {
        let snapshot = value
            .get("params")
            .map(usage_snapshot)
            .unwrap_or(UsageSnapshot {
                authoritative: false,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
                soonest_reset_epoch: None,
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
    let wake = tokio::task::spawn_blocking(move || {
        crate::auto_resume::wake_scheduled_if_usage_open_for_runtime(
            &context,
            &id,
            &runtime_id,
            Timestamp::now().as_second(),
        )
    })
    .await
    .map_err(|_| "Codex usage wake worker failed".to_string())?;
    wake.map(|_| ())
        .map_err(|err| format!("Codex usage wake failed: {}", err.code()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

    fn capability_probe_test_guard() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn external_auth_login_request_matches_codex_0_144_1_contract() {
        assert_eq!(
            external_auth_login_request(
                7,
                "fixture-access-token",
                "workspace-fixture",
                Some("pro"),
            ),
            json!({
                "id": 7,
                "method": "account/login/start",
                "params": {
                    "type": "chatgptAuthTokens",
                    "accessToken": "fixture-access-token",
                    "chatgptAccountId": "workspace-fixture",
                    "chatgptPlanType": "pro"
                }
            })
        );
    }

    #[test]
    fn external_auth_refresh_response_preserves_json_rpc_id() {
        assert_eq!(
            external_auth_refresh_response(
                json!("refresh-request"),
                "refreshed-fixture-token",
                "workspace-refreshed",
                None,
            ),
            json!({
                "id": "refresh-request",
                "result": {
                    "accessToken": "refreshed-fixture-token",
                    "chatgptAccountId": "workspace-refreshed",
                    "chatgptPlanType": null
                }
            })
        );
    }

    struct PendingMessageSink;

    impl futures_util::Sink<Message> for PendingMessageSink {
        type Error = tokio_tungstenite::tungstenite::Error;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }

        fn start_send(self: std::pin::Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            unreachable!("pending sink never becomes ready")
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

    impl futures_util::Stream for PendingMessageSink {
        type Item = Result<Message, tokio_tungstenite::tungstenite::Error>;

        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Pending
        }
    }

    #[derive(Default)]
    struct RecordingMessageSink {
        messages: Vec<Message>,
    }

    impl futures_util::Sink<Message> for RecordingMessageSink {
        type Error = tokio_tungstenite::tungstenite::Error;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn start_send(
            mut self: std::pin::Pin<&mut Self>,
            item: Message,
        ) -> Result<(), Self::Error> {
            self.messages.push(item);
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

    fn record_with_runtime(id: &str, socket: &Path) -> SessionRecord {
        SessionRecord {
            schema_version: crate::SESSION_DOCUMENT_VERSION.to_string(),
            id: id.to_string(),
            agent: "codex".to_string(),
            mode: "interactive".to_string(),
            coordination_mode: crate::cli::CoordinationMode::Advisory,
            title: None,
            title_state: None,
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
                    (
                        ATTENTION_AUTHORITY_KEY.to_string(),
                        json!(ATTENTION_AUTHORITY_PROTOCOL),
                    ),
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

    async fn wait_for_activity(
        context: &CliContext,
        id: &str,
        predicate: impl Fn(&crate::activity::TurnState) -> bool,
    ) -> crate::activity::TurnState {
        for _ in 0..100 {
            let state = crate::activity::activity_status(context, id)
                .unwrap()
                .turn_state;
            if predicate(&state) {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("activity did not converge before the bounded deadline")
    }

    fn write_create_bootstrap_marker(record: &SessionRecord) {
        let runtime = record.runtime.as_ref().unwrap();
        write_private_file(
            thread_handoff_path(record).unwrap(),
            runtime.launch_id.as_bytes(),
        )
        .unwrap();
    }

    fn write_manual_input_marker(
        context: &CliContext,
        record: &SessionRecord,
        launch_id: &str,
        expires_at_epoch_ms: u64,
    ) -> ManualInputMarker {
        let path = manual_input_section_path(context, record);
        let token = uuid::Uuid::new_v4().to_string();
        write_private_file(
            &path,
            &serde_json::to_vec(&RuntimeProcessMarker {
                schema_version: MANUAL_INPUT_SECTION_VERSION.to_string(),
                launch_id: launch_id.to_string(),
                token: token.clone(),
                owner_pid: std::process::id(),
                expires_at_epoch_ms,
            })
            .unwrap(),
        )
        .unwrap();
        let file = fs::File::open(&path).unwrap();
        // SAFETY: test owns this valid descriptor.
        assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) }, 0);
        open_manual_input_gate_file(&manual_input_gate_path(context, record)).unwrap();
        let ack_path = manual_input_ack_path(record).unwrap();
        let _ = fs::remove_file(&ack_path);
        let ack_socket = UnixDatagram::bind(&ack_path).unwrap();
        ack_socket
            .set_read_timeout(Some(MANUAL_INPUT_ACK_TIMEOUT))
            .unwrap();
        ManualInputMarker {
            path,
            token,
            _owner_file: file,
            gate_path: Some(manual_input_gate_path(context, record)),
            ack_path: Some(ack_path),
            ack_socket: Some(ack_socket),
            cleanup_on_drop: true,
        }
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
        let _probe_guard = capability_probe_test_guard();
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let runtime_dir = tmp.path().join("run");
        fs::create_dir(&runtime_dir).unwrap();
        fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700)).unwrap();
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
    fn selected_account_requires_capability_even_with_auto_preference() {
        let _probe_guard = capability_probe_test_guard();
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let runtime_dir = tmp.path().join("run");
        fs::create_dir(&runtime_dir).unwrap();
        let _runtime_dir = EnvGuard::set(&lock, "XDG_RUNTIME_DIR", runtime_dir.to_str().unwrap());
        let _preference = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_RUNTIME", "auto");
        let _broker = EnvGuard::set(
            &lock,
            "AGENT_SESSION_CODEX_ACCOUNT_BROKER",
            r#"["/configured/broker"]"#,
        );
        let agent = tmp.path().join("codex");
        fs::write(&agent, "#!/bin/sh\nprintf '%s\\n' 'codex-cli 0.145.0'\n").unwrap();
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut record = record_with_runtime("selected-auto", &runtime_dir.join("placeholder"));
        record.runtime.as_mut().unwrap().kind = "tmux".to_string();
        record.runtime.as_mut().unwrap().extra.clear();
        crate::codex_account::set_initial_binding(&mut record, Some("gamania")).unwrap();

        let err = configure_runtime(&context, &agent, &mut record, true).unwrap_err();
        assert_eq!(err.code(), "codex-app-server-capability-unavailable");
        assert_eq!(record.runtime.unwrap().kind, "tmux");
    }

    #[test]
    fn capability_probe_accepts_newer_versions_with_unix_transport() {
        let _probe_guard = capability_probe_test_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        for (name, version, help, expected) in [
            (
                "supported",
                "codex-cli 0.144.1",
                "  --listen <URL>  Supported values: stdio://, unix://PATH",
                true,
            ),
            (
                "supported-0.144.3",
                "codex-cli 0.144.3",
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
                "newer-patch",
                "codex-cli 0.144.5",
                "  --listen <URL>  Supported values: stdio://, unix://PATH",
                true,
            ),
            (
                "newer-minor",
                "codex-cli 0.145.0",
                "  --listen <URL>  Supported values: stdio://, unix://PATH",
                true,
            ),
            (
                "extra-component",
                "codex-cli 0.144.1.1",
                "  --listen <URL>  Supported values: stdio://, unix://PATH",
                false,
            ),
            (
                "prerelease",
                "codex-cli 0.144.1-beta",
                "  --listen <URL>  Supported values: stdio://, unix://PATH",
                false,
            ),
            (
                "build-metadata",
                "codex-cli 0.144.1+custom",
                "  --listen <URL>  Supported values: stdio://, unix://PATH",
                false,
            ),
            (
                "unrelated-token",
                "wrapper release 0.144.1",
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
            assert_eq!(app_server_capabilities(&path).transport, expected, "{name}");
        }
    }

    #[test]
    fn account_binding_readiness_reports_bounded_safe_reasons() {
        let _probe_guard = capability_probe_test_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let old = tmp.path().join("old");
        fs::write(&old, "#!/bin/sh\nprintf '%s\\n' 'codex-cli 0.143.9'\n").unwrap();
        fs::set_permissions(&old, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            account_binding_readiness(&old),
            CodexAccountReadiness {
                schema_version: CODEX_ACCOUNT_READINESS_SCHEMA_VERSION,
                supported: false,
                provider_version: Some("0.143.9".to_string()),
                reason_code: Some("codex-version-too-old"),
            }
        );

        let missing = tmp.path().join("missing");
        assert_eq!(
            account_binding_readiness(&missing),
            CodexAccountReadiness {
                schema_version: CODEX_ACCOUNT_READINESS_SCHEMA_VERSION,
                supported: false,
                provider_version: None,
                reason_code: Some("codex-unavailable"),
            }
        );
    }

    #[test]
    fn configured_app_server_runtime_selects_protocol_attention_authority() {
        let _probe_guard = capability_probe_test_guard();
        let lock = GlobalStateLock::new();
        let tmp = tempfile::Builder::new()
            .prefix("agent-session-authority-")
            .tempdir_in("/tmp")
            .unwrap();
        let runtime_dir = tmp.path().join("run");
        let home = tmp.path().join("home");
        fs::create_dir(&runtime_dir).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let _runtime_dir = EnvGuard::set(&lock, "XDG_RUNTIME_DIR", runtime_dir.to_str().unwrap());
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().unwrap());
        let _preference = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_RUNTIME", "app-server");
        fs::write(
            home.join(".codex/hooks.json"),
            serde_json::to_vec_pretty(&json!({
                "hooks": {
                    "PermissionRequest": [{
                        "hooks": [{
                            "type": "command",
                            "command": "sh -c 'if [ \"${AGENT_SESSION_ATTENTION_AUTHORITY:-hook}\" = protocol ]; then exit 0; fi; exec agent-session activity hook --agent codex'",
                            "timeout": 5
                        }]
                    }]
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let agent = tmp.path().join("codex");
        fs::write(
            &agent,
            "#!/bin/sh\nif [ \"$1\" = --version ]; then printf '%s\\n' 'codex-cli 0.144.3'; exit 0; fi\nif [ \"$1\" = app-server ] && [ \"$2\" = --help ]; then printf '%s\\n' '  --listen <URL>  Supported values: stdio://, unix://PATH'; exit 0; fi\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut record = record_with_runtime("authority", &runtime_dir.join("placeholder"));
        record.runtime.as_mut().unwrap().kind = "tmux".to_string();
        record.runtime.as_mut().unwrap().extra = BTreeMap::from([(
            ATTENTION_AUTHORITY_KEY.to_string(),
            json!(ATTENTION_AUTHORITY_HOOK),
        )]);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();

        configure_runtime(&context, &agent, &mut record, true).unwrap();
        assert_eq!(record.runtime.as_ref().unwrap().kind, RUNTIME_KIND);
        assert_eq!(attention_authority(&record), ATTENTION_AUTHORITY_PROTOCOL);
        assert_eq!(
            record
                .runtime
                .as_ref()
                .unwrap()
                .extra
                .get(ATTENTION_AUTHORITY_KEY),
            Some(&json!(ATTENTION_AUTHORITY_PROTOCOL))
        );
    }

    #[test]
    fn unguarded_permission_hook_prevents_protocol_attention_authority() {
        let _probe_guard = capability_probe_test_guard();
        let lock = GlobalStateLock::new();
        let tmp = tempfile::Builder::new()
            .prefix("agent-session-unguarded-")
            .tempdir_in("/tmp")
            .unwrap();
        let runtime_dir = tmp.path().join("run");
        let home = tmp.path().join("home");
        fs::create_dir(&runtime_dir).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let _runtime_dir = EnvGuard::set(&lock, "XDG_RUNTIME_DIR", runtime_dir.to_str().unwrap());
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().unwrap());
        let _preference = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_RUNTIME", "app-server");
        fs::write(
            home.join(".codex/hooks.json"),
            serde_json::to_vec_pretty(&json!({
                "hooks": {
                    "PermissionRequest": [{
                        "hooks": [
                            {
                                "type": "command",
                                "command": "sh -c 'if [ \"${AGENT_SESSION_ATTENTION_AUTHORITY:-hook}\" = protocol ]; then exit 0; fi; exec agent-session activity hook --agent codex'",
                                "timeout": 5
                            },
                            {
                                "type": "command",
                                "command": "agent-session activity hook --agent codex",
                                "timeout": 5
                            }
                        ]
                    }]
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let agent = tmp.path().join("codex");
        fs::write(
            &agent,
            "#!/bin/sh\nif [ \"$1\" = --version ]; then printf '%s\\n' 'codex-cli 0.144.3'; exit 0; fi\nif [ \"$1\" = app-server ] && [ \"$2\" = --help ]; then printf '%s\\n' '  --listen <URL>  Supported values: stdio://, unix://PATH'; exit 0; fi\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut record = record_with_runtime("unguarded-hook", &runtime_dir.join("placeholder"));
        record.runtime.as_mut().unwrap().kind = "tmux".to_string();
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();

        configure_runtime(&context, &agent, &mut record, true).unwrap();
        assert_eq!(record.runtime.as_ref().unwrap().kind, RUNTIME_KIND);
        assert_eq!(attention_authority(&record), ATTENTION_AUTHORITY_HOOK);
    }

    #[test]
    fn transport_only_app_server_runtime_keeps_hook_attention_authority() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::Builder::new()
            .prefix("agent-session-transport-")
            .tempdir_in("/tmp")
            .unwrap();
        let runtime_dir = tmp.path().join("run");
        fs::create_dir(&runtime_dir).unwrap();
        fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let _runtime_dir = EnvGuard::set(&lock, "XDG_RUNTIME_DIR", runtime_dir.to_str().unwrap());
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut record = record_with_runtime("transport-only", &runtime_dir.join("placeholder"));
        record.runtime.as_mut().unwrap().kind = "tmux".to_string();
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();

        configure_runtime_with_capabilities(
            &context,
            &mut record,
            true,
            AppServerCapabilities {
                transport: true,
                exact_attention: false,
                source_guard: true,
            },
        )
        .unwrap();

        assert_eq!(record.runtime.as_ref().unwrap().kind, RUNTIME_KIND);
        assert_eq!(attention_authority(&record), ATTENTION_AUTHORITY_HOOK);
    }

    #[test]
    fn capability_probe_tolerates_cold_start_latency() {
        let _probe_guard = capability_probe_test_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("cold-codex");
        fs::write(
            &path,
            r#"#!/bin/sh
sleep 0.35
if [ "$1" = --version ]; then
  printf '%s\n' 'codex-cli 0.144.1'
  exit 0
fi
if [ "$1" = app-server ] && [ "$2" = --help ]; then
  printf '%s\n' '  --listen <URL>  Supported values: stdio://, unix://PATH'
  exit 0
fi
exit 1
"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();

        assert!(app_server_capabilities(&path).transport);
    }

    #[test]
    fn capability_probe_timeout_kills_and_reaps_the_process_group() {
        let _probe_guard = capability_probe_test_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("hung-codex");
        let pid_file = tmp.path().join("descendant.pid");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nsleep 60 &\nprintf '%s' \"$!\" > {}\nwait\n",
                shell_words::quote(&pid_file.to_string_lossy())
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();

        let started = Instant::now();
        assert!(
            bounded_command_output_with_timeout(&path, &["--version"], Duration::from_millis(500))
                .is_none()
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "injected probe timeout must remain bounded"
        );

        let pid = fs::read_to_string(&pid_file)
            .unwrap()
            .parse::<libc::pid_t>()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn capability_probe_timeout_kills_descendant_after_leader_exits() {
        let _probe_guard = capability_probe_test_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("exited-codex");
        let pid_file = tmp.path().join("descendant.pid");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nsleep 60 &\nprintf '%s' \"$!\" > {}\nexit 0\n",
                shell_words::quote(&pid_file.to_string_lossy())
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();

        let started = Instant::now();
        assert!(
            bounded_command_output_with_timeout(&path, &["--version"], Duration::from_millis(500))
                .is_none()
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "output collection must remain under the injected probe timeout"
        );

        let pid = fs::read_to_string(&pid_file)
            .unwrap()
            .parse::<libc::pid_t>()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn launch_routes_the_visible_tui_through_the_private_proxy() {
        let script = launch_script();
        assert!(script.contains("codex-app-server-proxy"));
        assert!(script.contains("--remote \"unix://$proxy\""));
        assert!(!script.contains("--remote \"unix://$socket\""));
        assert!(!script.contains("thread/shellCommand"));
        assert!(script.contains(".startup-stage"));
        assert!(script.contains(".startup-failure"));
        assert!(script.contains(".startup-diagnostic.log"));
        assert!(script.contains(".runtime-exit-status"));
        assert!(script.contains("write_startup_marker \"$runtime_exit_status\" \"$status\""));
        let hold = script
            .find("exec 9>\"$startup_diagnostic_pipe\"")
            .expect("diagnostic pipe hold");
        let marker = script
            .find("write_startup_marker \"$runtime_exit_status\" \"$status\"")
            .expect("runtime exit marker");
        let provider_child_close = script
            .find("\"$@\" 9>&- 2>\"$provider_stderr_pipe\"")
            .expect("provider child closes diagnostic hold descriptor");
        let release = script[marker..]
            .find("exec 9>&-")
            .map(|offset| marker + offset)
            .expect("diagnostic pipe release");
        assert!(hold < provider_child_close && provider_child_close < marker && marker < release);
        assert!(script.contains("runtime-helper-unavailable"));
        assert!(script.contains("provider-client-exited"));
        assert!(script.contains("!= initial_connection"));
        assert!(script.contains("collect_startup_diagnostic"));
        assert!(script.contains("tail -c 16384"));
        assert!(script.contains("mkfifo \"$startup_diagnostic_pipe\""));
        assert!(script.contains("tee \"$startup_diagnostic_pipe\""));
        assert!(script.contains(
            "(umask 077; exec \"$proxy_bin\" --state-dir \"$state_dir\" codex-app-server-proxy"
        ));
        assert!(script.contains("kill -9 \"$provider_stderr_pid\""));
        let final_tee_kill = script
            .rfind("kill -9 \"$provider_stderr_pid\"")
            .expect("final tee escalation");
        let claim_before_wait = &script[final_tee_kill..];
        let clear = claim_before_wait
            .find("provider_stderr_pid=")
            .expect("tee pid ownership clear");
        let wait = claim_before_wait
            .find("wait \"$owned_pid\"")
            .expect("tee wait through claimed pid");
        assert!(clear < wait);
        assert!(!script.contains(">>\"$startup_diagnostic\""));
        let cleanup_lines = script
            .lines()
            .filter(|line| line.trim_start().starts_with("rm -f --") && line.contains("$socket"))
            .collect::<Vec<_>>();
        assert!(!cleanup_lines[0].contains("$handoff"));
        assert!(cleanup_lines[1].contains("$handoff"));
    }

    #[test]
    fn launch_script_retains_failed_provider_diagnostics_without_leaking_the_hold_descriptor() {
        let tmp = tempfile::TempDir::new().unwrap();
        let helper = tmp.path().join("fake-codex-runtime");
        fs::write(
            &helper,
            r#"#!/bin/sh
if [ "$1" = app-server ]; then
  : > "$FAKE_APP_SERVER_REQUEST"
  exec sleep 60
fi
case " $* " in
  *" codex-app-server-proxy "*)
    : > "$FAKE_PROXY_REQUEST"
    exec sleep 60
    ;;
esac
i=0
while [ "$(cat "$FAKE_PROVIDER_STAGE" 2>/dev/null)" != initial_connection ]; do
  i=$((i + 1))
  [ "$i" -lt 200 ] || exit 99
  sleep 0.01
done
printf '%s' "$FAKE_PROVIDER_STDERR" >&2
if [ -n "$FAKE_LAUNCHER_PID_FILE" ]; then
  printf '%s' "$PPID" > "$FAKE_LAUNCHER_PID_FILE"
fi
exit "$FAKE_PROVIDER_EXIT"
"#,
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let tee = tmp.path().join("tee");
        fs::write(&tee, "#!/bin/sh\ntrap '' TERM\nexec /usr/bin/tee \"$@\"\n").unwrap();
        fs::set_permissions(&tee, fs::Permissions::from_mode(0o700)).unwrap();
        let mut test_paths = vec![tmp.path().to_path_buf()];
        test_paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
        let test_path = env::join_paths(test_paths).unwrap();
        let state_dir = tmp.path().join("state");
        let cwd = tmp.path().join("cwd");
        fs::create_dir(&cwd).unwrap();

        let run_case = |id: &str, status: i32, stderr: &str, descendant: bool, interrupt: bool| {
            let session_dir = state_dir.join("sessions").join(id);
            fs::create_dir_all(&session_dir).unwrap();
            let socket = tmp.path().join(format!("{id}-app.sock"));
            let proxy = tmp.path().join(format!("{id}-proxy.sock"));
            let handoff = tmp.path().join(format!("{id}-thread"));
            let attached = tmp.path().join(format!("{id}-attached"));
            let stage = session_dir.join(".startup-stage");
            let runtime_exit_status = session_dir.join(".runtime-exit-status");
            let provider_stderr_pipe = session_dir.join(".provider-stderr.pipe");
            let launcher_pid_file = session_dir.join("launcher.pid");
            let app_server_request = session_dir.join("app-server.request");
            let proxy_request = session_dir.join("proxy.request");
            let stop = Arc::new(AtomicBool::new(false));
            let descriptor_holder_ready = descendant.then(|| Arc::new(AtomicBool::new(false)));
            let bind_socket = |request: PathBuf,
                               socket: PathBuf,
                               stage: Option<PathBuf>,
                               stage_barrier: Option<Arc<AtomicBool>>,
                               stop: Arc<AtomicBool>| {
                thread::spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(3);
                    while !request.exists()
                        && !stop.load(Ordering::Relaxed)
                        && Instant::now() < deadline
                    {
                        thread::sleep(Duration::from_millis(5));
                    }
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
                    if let Some(stage) = stage {
                        while fs::read_to_string(&stage)
                            .ok()
                            .is_none_or(|value| value.trim() != "provider_client")
                            && !stop.load(Ordering::Relaxed)
                            && Instant::now() < deadline
                        {
                            thread::sleep(Duration::from_millis(5));
                        }
                        if !stop.load(Ordering::Relaxed) {
                            if let Some(stage_barrier) = stage_barrier {
                                while !stage_barrier.load(Ordering::Acquire)
                                    && !stop.load(Ordering::Relaxed)
                                    && Instant::now() < deadline
                                {
                                    thread::sleep(Duration::from_millis(5));
                                }
                                assert!(
                                    stage_barrier.load(Ordering::Acquire),
                                    "provider stderr holder must be ready before stage advance"
                                );
                            }
                            fs::write(stage, "initial_connection\n").unwrap();
                        }
                    }
                    while !stop.load(Ordering::Relaxed) {
                        thread::sleep(Duration::from_millis(5));
                    }
                })
            };
            let app_server_thread = bind_socket(
                app_server_request.clone(),
                socket.clone(),
                None,
                None,
                Arc::clone(&stop),
            );
            let proxy_thread = bind_socket(
                proxy_request.clone(),
                proxy.clone(),
                Some(stage.clone()),
                descriptor_holder_ready.as_ref().map(Arc::clone),
                Arc::clone(&stop),
            );
            let descriptor_holder_thread = descriptor_holder_ready.map(|ready| {
                thread::spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(10);
                    let stderr = loop {
                        match OpenOptions::new()
                            .write(true)
                            .custom_flags(libc::O_NONBLOCK)
                            .open(&provider_stderr_pipe)
                        {
                            Ok(stderr) => break stderr,
                            Err(err)
                                if Instant::now() < deadline
                                    && matches!(
                                        err.raw_os_error(),
                                        Some(libc::ENOENT) | Some(libc::ENXIO)
                                    ) =>
                            {
                                thread::sleep(Duration::from_millis(5));
                            }
                            Err(err) => panic!("provider stderr holder must open the pipe: {err}"),
                        }
                    };
                    let child = Command::new("sleep")
                        .arg("300")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::from(stderr))
                        .spawn()
                        .expect("provider stderr holder must start");
                    ready.store(true, Ordering::Release);
                    child
                })
            });
            let interrupt_thread = interrupt.then(|| {
                let runtime_exit_status = runtime_exit_status.clone();
                let launcher_pid_file = launcher_pid_file.clone();
                thread::spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while (!runtime_exit_status.exists() || !launcher_pid_file.exists())
                        && Instant::now() < deadline
                    {
                        thread::sleep(Duration::from_millis(1));
                    }
                    let pid = fs::read_to_string(&launcher_pid_file)
                        .expect("provider must persist the launcher pid before exiting")
                        .parse::<libc::pid_t>()
                        .expect("launcher pid must be valid");
                    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
                })
            });
            let mut command = Command::new("/bin/sh");
            command
                .arg("-c")
                .arg(launch_script())
                .arg("agent-session-launch-test")
                .arg(&socket)
                .arg(&proxy)
                .arg(&handoff)
                .arg(&attached)
                .arg(&helper)
                .arg(&state_dir)
                .arg(id)
                .arg(&helper)
                .arg(&cwd)
                .env("FAKE_PROVIDER_STAGE", &stage)
                .env("FAKE_APP_SERVER_REQUEST", &app_server_request)
                .env("FAKE_PROXY_REQUEST", &proxy_request)
                .env("FAKE_PROVIDER_EXIT", status.to_string())
                .env("FAKE_PROVIDER_STDERR", stderr)
                .env(
                    "FAKE_LAUNCHER_PID_FILE",
                    if interrupt {
                        launcher_pid_file.to_string_lossy().into_owned()
                    } else {
                        String::new()
                    },
                )
                .env("PATH", &test_path);
            let output = crate::run_output_with_timeout(command, Duration::from_secs(10));
            stop.store(true, Ordering::Relaxed);
            app_server_thread.join().unwrap();
            proxy_thread.join().unwrap();
            if let Some(thread) = interrupt_thread {
                thread.join().expect("launcher interrupt must not panic");
            }
            let mut descriptor_holder = descriptor_holder_thread.map(|thread| {
                thread
                    .join()
                    .expect("provider stderr holder setup must not panic")
            });
            let holder_was_alive = descriptor_holder
                .as_mut()
                .map(|child| {
                    let alive = child
                        .try_wait()
                        .expect("provider stderr holder status must be readable")
                        .is_none();
                    let _ = child.kill();
                    let _ = child.wait();
                    alive
                })
                .unwrap_or(true);
            let output = output
                .expect("generated launcher must terminate without waiting for stderr holders");
            if descendant {
                assert!(
                    holder_was_alive,
                    "generated launcher must return while the provider stderr holder is alive"
                );
            }
            assert_eq!(
                output.status.code(),
                Some(if interrupt { 143 } else { status })
            );
            session_dir
        };

        let failed = run_case("failed-stderr", 17, "post-ready-failure\n", true, false);
        assert_eq!(
            fs::read(failed.join(".startup-diagnostic.log")).unwrap(),
            b"post-ready-failure\n"
        );
        assert_eq!(
            fs::read_to_string(failed.join(".runtime-exit-status")).unwrap(),
            "17\n"
        );

        let interrupted = run_case("interrupted-drain", 29, "interrupted\n", true, true);
        assert_eq!(
            fs::read(interrupted.join(".startup-diagnostic.log")).unwrap(),
            b"interrupted\n"
        );
        assert_eq!(
            fs::read_to_string(interrupted.join(".runtime-exit-status")).unwrap(),
            "29\n"
        );

        let status_only = run_case("failed-status-only", 23, "", false, false);
        assert!(!status_only.join(".startup-diagnostic.log").exists());
        assert_eq!(
            fs::read_to_string(status_only.join(".runtime-exit-status")).unwrap(),
            "23\n"
        );

        let clean = run_case("clean", 0, "ordinary ready stderr\n", false, false);
        assert!(!clean.join(".startup-diagnostic.log").exists());
        assert!(!clean.join(".runtime-exit-status").exists());
    }

    #[test]
    fn managed_codex_client_launch_disables_startup_update_check_without_owning_base_arguments() {
        let script = launch_script();
        assert!(script.contains(
            "\"$agent\" -c check_for_update_on_startup=false --remote \"unix://$proxy\" \"$@\" 9>&-"
        ));
        assert!(!script.contains("--cd \"$cwd\""));
        assert!(!script.contains("--no-alt-screen \"$@\""));
    }

    #[test]
    fn startup_diagnostic_collector_caps_private_failure_output_discards_clean_ready_output_and_retains_abnormal_ready_output()
     {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let tmp = tempfile::TempDir::new().unwrap();
        let stage = tmp.path().join("stage");
        let diagnostic = tmp.path().join("diagnostic.log");
        let exit_status = tmp.path().join("runtime-exit-status");
        fs::write(&stage, "provider_client\n").unwrap();
        let buffer = tmp.path().join("diagnostic.buffer");
        let command = format!(
            "startup_stage={}; startup_diagnostic={}; startup_diagnostic_buffer={}; runtime_exit_status={}; {}; collect_startup_diagnostic",
            shell_words::quote(&stage.to_string_lossy()),
            shell_words::quote(&diagnostic.to_string_lossy()),
            shell_words::quote(&buffer.to_string_lossy()),
            shell_words::quote(&exit_status.to_string_lossy()),
            STARTUP_DIAGNOSTIC_COLLECTOR_SCRIPT,
        );
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = vec![b'x'; 2 * 1024 * 1024];
        input.extend_from_slice(b"failure-sentinel");
        child.stdin.take().unwrap().write_all(&input).unwrap();
        assert!(child.wait().unwrap().success());

        let retained = fs::read(&diagnostic).unwrap();
        assert!(retained.len() <= 16 * 1024);
        assert!(retained.ends_with(b"failure-sentinel"));
        assert_eq!(
            fs::metadata(&diagnostic).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::remove_file(&diagnostic).unwrap();
        fs::write(&stage, "initial_connection\n").unwrap();
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&vec![b'y'; 2 * 1024 * 1024])
            .unwrap();
        assert!(child.wait().unwrap().success());
        assert!(!diagnostic.exists());
        assert!(!buffer.exists());

        fs::write(&exit_status, "1\n").unwrap();
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"post-ready-failure\n")
            .unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(fs::read(&diagnostic).unwrap(), b"post-ready-failure\n");

        fs::remove_file(&diagnostic).unwrap();
        let mut split_utf8 = "🙂".repeat(4096).into_bytes();
        split_utf8.push(b'x');
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&split_utf8).unwrap();
        assert!(child.wait().unwrap().success());
        let retained = fs::read(&diagnostic).unwrap();
        assert_eq!(retained.len(), 16 * 1024);
        assert!(
            std::str::from_utf8(&retained).is_err(),
            "the byte cap fixture must split a multibyte code point"
        );
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
        let mut stream = PendingMessageSink;
        let err =
            receive_response_with_timeout(&mut stream, 1, None, None, Duration::from_millis(5))
                .await
                .unwrap_err();
        assert_eq!(err, "Codex app-server request timed out");
    }

    #[tokio::test]
    async fn external_auth_refresh_rebinds_durable_state_without_serializing_token() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let broker = tmp.path().join("broker");
        let calls = tmp.path().join("calls");
        fs::write(
            &broker,
            r#"#!/bin/sh
calls=$1
shift
printf '%s\n' "$*" >> "$calls"
printf '%s\n' '{"schema_version":"agent-session.codex-auth-broker.v1","account":"gamania","access_token":"refreshed-fixture-token","chatgpt_account_id":"workspace-refreshed","plan":"team"}'
"#,
        )
        .unwrap();
        fs::set_permissions(&broker, fs::Permissions::from_mode(0o700)).unwrap();
        let argv = serde_json::to_string(&vec![
            broker.to_string_lossy().into_owned(),
            calls.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let _broker = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_ACCOUNT_BROKER", &argv);
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut record = record_with_runtime("refresh-success", &tmp.path().join("server.sock"));
        crate::codex_account::set_initial_binding(&mut record, Some("gamania")).unwrap();
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::codex_account::finish_binding(
            &context,
            &record.id,
            "runtime-refresh-success",
            "gamania",
            1,
            Ok(()),
        )
        .unwrap();

        let mut sink = RecordingMessageSink::default();
        assert!(
            respond_to_external_auth_refresh(
                &mut sink,
                &json!({
                    "id": "refresh-1",
                    "method": "account/chatgptAuthTokens/refresh",
                    "params": {
                        "reason": "unauthorized",
                        "previousAccountId": "workspace-old"
                    }
                }),
                Some((&context, &record, "gamania")),
            )
            .await
            .unwrap()
        );
        let response: Value = match &sink.messages[0] {
            Message::Text(text) => serde_json::from_str(text).unwrap(),
            message => panic!("unexpected refresh response: {message:?}"),
        };
        assert_eq!(response["id"], "refresh-1");
        assert_eq!(response["result"]["accessToken"], "refreshed-fixture-token");
        let persisted = crate::load_session_record(&context, &record.id).unwrap();
        let view = crate::codex_account::view_for_record(&persisted);
        assert_eq!(view.state, "bound");
        assert_eq!(
            view.applied_runtime_id.as_deref(),
            Some("runtime-refresh-success")
        );
        let session_json =
            fs::read_to_string(crate::session_dir(&context, &record.id).join("session.json"))
                .unwrap();
        assert!(!session_json.contains("refreshed-fixture-token"));
        assert_eq!(
            fs::read_to_string(calls).unwrap().trim(),
            "resolve --account gamania --force-refresh --format json"
        );
    }

    #[tokio::test]
    async fn external_auth_refresh_failure_marks_binding_failed() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let broker = tmp.path().join("broker");
        fs::write(&broker, "#!/bin/sh\nexit 9\n").unwrap();
        fs::set_permissions(&broker, fs::Permissions::from_mode(0o700)).unwrap();
        let argv = serde_json::to_string(&vec![broker.to_string_lossy().into_owned()]).unwrap();
        let _broker = EnvGuard::set(&lock, "AGENT_SESSION_CODEX_ACCOUNT_BROKER", &argv);
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut record = record_with_runtime("refresh-failure", &tmp.path().join("server.sock"));
        crate::codex_account::set_initial_binding(&mut record, Some("gamania")).unwrap();
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::codex_account::finish_binding(
            &context,
            &record.id,
            "runtime-refresh-failure",
            "gamania",
            1,
            Ok(()),
        )
        .unwrap();

        let error = respond_to_external_auth_refresh(
            &mut RecordingMessageSink::default(),
            &json!({
                "id": 19,
                "method": "account/chatgptAuthTokens/refresh",
                "params": { "reason": "unauthorized" }
            }),
            Some((&context, &record, "gamania")),
        )
        .await
        .unwrap_err();
        assert!(error.starts_with("Codex account refresh failed:"));
        let persisted = crate::load_session_record(&context, &record.id).unwrap();
        let view = crate::codex_account::view_for_record(&persisted);
        assert_eq!(view.state, "failed");
        assert_eq!(view.failure_reason.as_deref(), Some("refresh_failed"));
        assert_eq!(
            crate::codex_account::ensure_input_allowed(&persisted)
                .unwrap_err()
                .code(),
            "codex-account-not-bound"
        );
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
        assert!(matches!(
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
            })),
            ServerProjection::Irrelevant
        ));
    }

    #[test]
    fn exact_attention_projection_preserves_typed_ids_and_discards_content() {
        let cases = [
            (
                json!({
                    "id": 1,
                    "method": "item/commandExecution/requestApproval",
                    "params": {
                        "threadId": "thread-a",
                        "turnId": "turn-a",
                        "command": "must-not-leave-the-adapter",
                        "reason": "must-not-leave-the-adapter"
                    }
                }),
                json!(1),
                "approval",
                json!("turn-a"),
            ),
            (
                json!({
                    "id": "1",
                    "method": "item/tool/requestUserInput",
                    "params": {
                        "threadId": "thread-a",
                        "turnId": "turn-a",
                        "questions": [{"question": "must-not-leave-the-adapter"}]
                    }
                }),
                json!("1"),
                "clarification",
                json!("turn-a"),
            ),
            (
                json!({
                    "id": 2,
                    "method": "item/fileChange/requestApproval",
                    "params": {"threadId": "thread-a", "turnId": "turn-a", "changes": "discarded"}
                }),
                json!(2),
                "approval",
                json!("turn-a"),
            ),
            (
                json!({
                    "id": 3,
                    "method": "item/permissions/requestApproval",
                    "params": {"threadId": "thread-a", "turnId": "turn-a", "permissions": "discarded"}
                }),
                json!(3),
                "approval",
                json!("turn-a"),
            ),
            (
                json!({
                    "id": 4,
                    "method": "mcpServer/elicitation/request",
                    "params": {"threadId": "thread-a", "turnId": null, "mode": "form", "serverName": "discarded"}
                }),
                json!(4),
                "clarification",
                Value::Null,
            ),
        ];

        let mut projected = Vec::new();
        for (raw, request_id, kind, turn_id) in cases {
            let ServerProjection::Unique(value) = server_observation(&raw) else {
                panic!("recognized blocking request must be a unique projection");
            };
            assert_eq!(
                value,
                json!({
                    "method": "agent-session/attention/requested",
                    "params": {
                        "requestId": request_id,
                        "threadId": "thread-a",
                        "turnId": turn_id,
                        "kind": kind
                    }
                })
            );
            let wire = serde_json::to_string(&value).unwrap();
            assert!(!wire.contains("must-not-leave-the-adapter"));
            assert!(!wire.contains("discarded"));
            projected.push(value);
        }
        assert_ne!(
            projected[0].pointer("/params/requestId"),
            projected[1].pointer("/params/requestId"),
            "JSON integer 1 and string \"1\" must remain distinct"
        );

        let ServerProjection::Unique(resolved) = server_observation(&json!({
            "method": "serverRequest/resolved",
            "params": {"threadId": "thread-a", "requestId": 1}
        })) else {
            panic!("typed resolution must be a unique projection");
        };
        assert_eq!(
            resolved,
            json!({
                "method": "agent-session/attention/resolved",
                "params": {"threadId": "thread-a", "requestId": 1}
            })
        );
    }

    #[test]
    fn mcp_elicitation_mode_maps_exactly_and_rejects_unknown_shapes() {
        for (mode, expected) in [
            ("form", "clarification"),
            ("openai/form", "clarification"),
            ("url", "authentication"),
        ] {
            let ServerProjection::Unique(projected) = server_observation(&json!({
                "id": format!("request-{mode}"),
                "method": "mcpServer/elicitation/request",
                "params": {"threadId": "thread-a", "turnId": null, "mode": mode}
            })) else {
                panic!("audited MCP elicitation mode must project");
            };
            assert_eq!(projected["params"]["kind"], expected);
        }
        for mode in [Value::Null, json!("future-mode"), json!({"bad": true})] {
            assert!(matches!(
                server_observation(&json!({
                    "id": "request-invalid",
                    "method": "mcpServer/elicitation/request",
                    "params": {"threadId": "thread-a", "turnId": null, "mode": mode}
                })),
                ServerProjection::RejectedUnique
            ));
        }
    }

    #[test]
    fn exact_attention_projection_rejects_invalid_scope_and_request_ids() {
        for value in [
            json!({
                "id": 1.5,
                "method": "item/fileChange/requestApproval",
                "params": {"threadId": "thread-a", "turnId": "turn-a"}
            }),
            json!({
                "id": 1,
                "method": "item/permissions/requestApproval",
                "params": {"threadId": "", "turnId": "turn-a"}
            }),
            json!({
                "method": "serverRequest/resolved",
                "params": {"threadId": "thread-a", "requestId": {"bad": true}}
            }),
        ] {
            assert!(matches!(
                server_observation(&value),
                ServerProjection::RejectedUnique
            ));
        }
    }

    #[tokio::test]
    async fn exact_attention_requests_clear_independently_before_turn_completion() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("exact-attention", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        let mut projection = ProxyProjection::new(context.clone(), record.clone());
        projection.observe_client(&json!({
            "method": "turn/start",
            "params": {"threadId": "thread-a"}
        }));
        projection.observe_server(&json!({
            "id": 1,
            "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "thread-a", "turnId": "turn-a", "command": "secret"}
        }));
        projection.observe_server(&json!({
            "id": "1",
            "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "thread-a", "turnId": "turn-a", "command": "secret"}
        }));
        let state = wait_for_activity(&context, &record.id, |state| {
            state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .is_some_and(|attention| attention.pending_count == 2)
        })
        .await;
        assert_eq!(state.phase, crate::activity::TurnPhase::NeedsInput);
        assert_eq!(
            state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .map(|attention| attention.pending_count),
            Some(2)
        );
        assert!(!serde_json::to_string(&state).unwrap().contains("secret"));

        projection.observe_server(&json!({
            "method": "serverRequest/resolved",
            "params": {"threadId": "thread-a", "requestId": 1}
        }));
        let one = wait_for_activity(&context, &record.id, |state| {
            state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .is_some_and(|attention| attention.pending_count == 1)
        })
        .await;
        assert_eq!(one.phase, crate::activity::TurnPhase::NeedsInput);
        assert_eq!(
            one.current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .map(|attention| attention.pending_count),
            Some(1)
        );

        projection.observe_server(&json!({
            "method": "serverRequest/resolved",
            "params": {"threadId": "thread-a", "requestId": "1"}
        }));
        let cleared = wait_for_activity(&context, &record.id, |state| {
            state.phase == crate::activity::TurnPhase::Working
                && state
                    .current_turn
                    .as_ref()
                    .and_then(|turn| turn.attention.as_ref())
                    .is_none()
        })
        .await;
        assert_eq!(cleared.phase, crate::activity::TurnPhase::Working);
        assert!(
            cleared
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .is_none()
        );

        let revision = cleared.revision;
        for request_id in [json!("1"), json!("unmatched")] {
            projection.observe_server(&json!({
                "method": "serverRequest/resolved",
                "params": {"threadId": "thread-a", "requestId": request_id}
            }));
        }
        projection.finish().await;
        assert_eq!(
            crate::activity::activity_status(&context, &record.id)
                .unwrap()
                .turn_state
                .revision,
            revision,
            "repeated and unmatched resolutions must be idempotent no-ops"
        );
    }

    #[tokio::test]
    async fn exact_attention_allows_sequential_id_reuse_and_keeps_raw_id_private() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("attention-reuse", &tmp.path().join("server.sock"));
        let dir = crate::session_dir(&context, &record.id);
        fs::create_dir_all(&dir).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        let mut projection = ProxyProjection::new(context.clone(), record.clone());
        projection.observe_client(&json!({
            "method": "turn/start",
            "params": {"threadId": "thread-a"}
        }));
        let raw_id = format!("private-{}", "x".repeat(MAX_PROTOCOL_ID_BYTES - 8));
        assert_eq!(raw_id.len(), MAX_PROTOCOL_ID_BYTES);

        for occurrence in 1..=2 {
            projection.observe_server(&json!({
                "id": raw_id.clone(),
                "method": "item/commandExecution/requestApproval",
                "params": {
                    "threadId": "thread-a",
                    "turnId": "turn-a",
                    "command": "private-command-must-not-persist"
                }
            }));
            let requested = wait_for_activity(&context, &record.id, |state| {
                state.phase == crate::activity::TurnPhase::NeedsInput
            })
            .await;
            assert!(requested.revision >= occurrence * 2 - 1);

            let activity_files = [
                "activity.json",
                "activity.journal.jsonl",
                "activity.replay.bin",
            ];
            for _ in 0..100 {
                if activity_files.iter().all(|file| dir.join(file).is_file()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(activity_files.iter().all(|file| dir.join(file).is_file()));
            for file in activity_files {
                let bytes = fs::read(dir.join(file)).unwrap();
                assert!(
                    !bytes
                        .windows(raw_id.len())
                        .any(|window| window == raw_id.as_bytes())
                );
                assert!(
                    !String::from_utf8_lossy(&bytes).contains("private-command-must-not-persist")
                );
            }
            let public = serde_json::to_string(
                &crate::activity::activity_status(&context, &record.id)
                    .unwrap()
                    .turn_state,
            )
            .unwrap();
            assert!(!public.contains(&raw_id));
            assert!(!public.contains("private-command-must-not-persist"));

            projection.observe_server(&json!({
                "method": "serverRequest/resolved",
                "params": {"threadId": "thread-a", "requestId": raw_id.clone()}
            }));
            wait_for_activity(&context, &record.id, |state| {
                state.phase == crate::activity::TurnPhase::Working
                    && state
                        .current_turn
                        .as_ref()
                        .and_then(|turn| turn.attention.as_ref())
                        .is_none()
            })
            .await;
        }
        projection.finish().await;

        assert!(matches!(
            server_observation(&json!({
                "id": "x".repeat(MAX_PROTOCOL_ID_BYTES + 1),
                "method": "item/commandExecution/requestApproval",
                "params": {"threadId": "thread-a", "turnId": "turn-a"}
            })),
            ServerProjection::RejectedUnique
        ));
    }

    #[tokio::test]
    async fn exact_attention_rejects_wrong_turn_and_allows_nullable_mcp_turn() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("attention-turn-scope", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        let mut projection = ProxyProjection::new(context.clone(), record.clone());
        projection.observe_client(&json!({
            "method": "turn/start",
            "params": {"threadId": "thread-a"}
        }));

        projection.observe_server(&json!({
            "id": "matching-request",
            "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "thread-a", "turnId": "turn-a"}
        }));
        wait_for_activity(&context, &record.id, |state| {
            state.phase == crate::activity::TurnPhase::NeedsInput
        })
        .await;
        projection.observe_server(&json!({
            "method": "serverRequest/resolved",
            "params": {"threadId": "thread-a", "requestId": "matching-request"}
        }));
        wait_for_activity(&context, &record.id, |state| {
            state.phase == crate::activity::TurnPhase::Working
        })
        .await;

        projection.observe_server(&json!({
            "id": "mcp-request",
            "method": "mcpServer/elicitation/request",
            "params": {"threadId": "thread-a", "turnId": null, "mode": "form"}
        }));
        wait_for_activity(&context, &record.id, |state| {
            state.phase == crate::activity::TurnPhase::NeedsInput
        })
        .await;
        projection.observe_server(&json!({
            "method": "serverRequest/resolved",
            "params": {"threadId": "thread-a", "requestId": "mcp-request"}
        }));
        wait_for_activity(&context, &record.id, |state| {
            state.phase == crate::activity::TurnPhase::Working
        })
        .await;

        projection.observe_server(&json!({
            "id": "wrong-turn-request",
            "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "thread-a", "turnId": "turn-b"}
        }));
        let unknown = wait_for_activity(&context, &record.id, |state| {
            state.phase == crate::activity::TurnPhase::Unknown
        })
        .await;
        assert_eq!(unknown.phase, crate::activity::TurnPhase::Unknown);
        projection.finish().await;
    }

    #[tokio::test]
    async fn persisted_thread_observation_skips_duplicate_marker_io() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("persisted-binding", &tmp.path().join("server.sock"));
        let mut observer = ProxyObserver::new();
        observer
            .observe_client(
                &record,
                &json!({ "id": 1, "method": "thread/start", "params": {} }),
            )
            .unwrap();

        observer
            .observe_server(
                &context,
                &record,
                &json!({ "id": 1, "result": { "thread": { "id": "fresh-thread" } } }),
                Some("fresh-thread"),
            )
            .await
            .expect("the projection worker should trust the pre-forward persisted binding");

        assert_eq!(
            observer
                .reducer
                .as_ref()
                .map(|reducer| reducer.thread_id.as_str()),
            Some("fresh-thread")
        );
        assert!(
            !thread_attached_path(&record).unwrap().exists(),
            "the projection worker must not repeat marker persistence or validation"
        );
    }

    #[tokio::test]
    async fn fresh_thread_binding_precedes_first_turn_authorization() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("binding-barrier", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        let mut projection = ProxyProjection::new(context, record.clone());

        projection.observe_client(&json!({
            "id": 1,
            "method": "thread/start",
            "params": { "cwd": "/repo" }
        }));
        projection
            .observe_server_before_forward(&json!({
                "id": 1,
                "result": { "thread": { "id": "fresh-thread" } }
            }))
            .await
            .unwrap();

        let attached = thread_attached_path(&record).unwrap();
        assert_eq!(
            fs::read_to_string(attached).unwrap(),
            projected_thread_binding("fresh-thread"),
            "the proxy must publish the bound thread before forwarding the response that enables the first turn"
        );

        let _capability = begin_proxy_capability(&projection.context, &record).unwrap();
        let lifecycle_lock =
            crate::acquire_session_record_lock(&projection.context, &record.id).unwrap();
        let marker = begin_manual_input_section(&projection.context, &record)
            .unwrap()
            .unwrap();
        let first_turn = json!({
            "id": 2,
            "method": "turn/start",
            "params": { "threadId": "fresh-thread", "input": [] }
        });
        let gate = acquire_manual_input_gate(&projection.context, &record, &first_turn)
            .expect("the serialized first turn must recognize the synchronously bound thread");
        drop(gate);
        marker.finish(|| drop(lifecycle_lock));
        projection.finish().await;
    }

    #[tokio::test]
    async fn saturated_projection_preserves_fresh_thread_binding_barrier() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("binding-overflow", &tmp.path().join("server.sock"));
        let mut projection = ProxyProjection::new(context, record.clone());

        projection.task.take().unwrap().abort();
        let (sender, mut blocked_receiver) = mpsc::channel(MAX_PROXY_OBSERVATIONS);
        projection.sender = Some(sender);
        projection.observe_client(&json!({ "id": 1, "method": "thread/start" }));
        for _ in 1..MAX_PROXY_OBSERVATIONS {
            projection.observe_server(&json!({
                "method": "account/rateLimits/updated",
                "params": {
                    "rateLimits": {
                        "primary": { "usedPercent": 42.0, "resetsAt": 1_900_000_000_i64 }
                    }
                }
            }));
        }

        let (release_worker, wait_for_release) = oneshot::channel();
        let worker = tokio::spawn(async move {
            wait_for_release.await.unwrap();
            while let Some(observation) = blocked_receiver.recv().await {
                if let ProxyObservation::Server {
                    persisted_thread: Some(thread_id),
                    binding_ack: Some(binding_ack),
                    ..
                } = observation
                {
                    assert_eq!(thread_id, "fresh-thread");
                    binding_ack.send(Ok(())).unwrap();
                    return;
                }
            }
            panic!("the saturated queue never admitted the critical binding observation");
        });
        let response_value = json!({
            "id": 1,
            "result": { "thread": { "id": "fresh-thread" } }
        });
        let response = projection.observe_server_before_forward(&response_value);
        tokio::pin!(response);
        assert!(
            futures_util::poll!(&mut response).is_pending(),
            "the critical binding send must wait while every queue slot remains occupied"
        );
        release_worker.send(()).unwrap();
        response.await.unwrap();
        worker.await.unwrap();

        assert_eq!(
            fs::read_to_string(thread_attached_path(&record).unwrap()).unwrap(),
            projected_thread_binding("fresh-thread"),
            "a saturated projection queue must not bypass the pre-forward binding barrier"
        );
    }

    #[tokio::test]
    async fn disabled_projection_never_persists_thread_binding() {
        let tmp = tempfile::TempDir::new().unwrap();
        for disable_before_request in [true, false] {
            let context = CliContext {
                state_dir: tmp.path().join(format!("state-{disable_before_request}")),
                host: None,
            };
            let record = record_with_runtime(
                &format!("disabled-{disable_before_request}"),
                &tmp.path()
                    .join(format!("server-{disable_before_request}.sock")),
            );
            let mut projection = ProxyProjection::new(context, record.clone());
            if disable_before_request {
                projection.disable();
            }
            projection.observe_client(&json!({ "id": 1, "method": "thread/start" }));
            if !disable_before_request {
                projection.disable();
            }
            let result = projection
                .observe_server_before_forward(&json!({
                    "id": 1,
                    "result": { "thread": { "id": "fresh-thread" } }
                }))
                .await;
            assert!(result.is_err());
            assert!(!thread_attached_path(&record).unwrap().exists());
        }

        let context = CliContext {
            state_dir: tmp.path().join("state-worker-close"),
            host: None,
        };
        let record = record_with_runtime("worker-close", &tmp.path().join("worker-close.sock"));
        let mut projection = ProxyProjection::new(context, record.clone());
        projection.observe_client(&json!({ "id": 1, "method": "thread/start" }));
        projection.task.take().unwrap().abort();
        let result = projection
            .observe_server_before_forward(&json!({
                "id": 1,
                "result": { "thread": { "id": "fresh-thread" } }
            }))
            .await;
        assert!(result.is_err());
        assert!(
            !thread_attached_path(&record).unwrap().exists(),
            "a worker closing after the active check must prevent marker publication"
        );
    }

    #[tokio::test]
    async fn rejected_fresh_thread_binding_is_critical() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("rejected-binding", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();

        let mut projection = ProxyProjection::new(context, record);
        projection.observe_client(&json!({ "id": 1, "method": "thread/start" }));
        let result = projection
            .observe_server_before_forward(&json!({
                "id": 1,
                "result": { "thread": { "id": "" } }
            }))
            .await;

        assert!(
            result.is_err(),
            "an invalid response for the required fresh binding must stop forwarding"
        );
        assert!(
            projection.has_fail_close_task(),
            "the critical rejection must retain durable fail-close"
        );
        projection.finish_fail_close().await;
    }

    #[test]
    fn critical_binding_failure_completes_fail_close_before_runtime_shutdown() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("binding-fail-close", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();

        let runtime_context = context.clone();
        let runtime_record = record.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let mut projection = ProxyProjection::new(runtime_context, runtime_record.clone());
                projection.observe_client(&json!({ "id": 1, "method": "thread/start" }));
                let (closed_sender, closed_receiver) = mpsc::channel(1);
                drop(closed_receiver);
                projection.sender = Some(closed_sender);

                let result = projection
                    .observe_server_before_forward(&json!({
                        "id": 1,
                        "result": { "thread": { "id": "fresh-thread" } }
                    }))
                    .await;
                assert!(result.is_err());
                projection.finish_fail_close().await;
            });
        })
        .join()
        .unwrap();

        let view = crate::auto_resume::view_for_record(&context, &record);
        assert!(!view.enabled);
        assert_eq!(view.state, "terminal_failure");
        assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));
    }

    #[test]
    fn critical_binding_failure_returns_before_fail_close_lock_retry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("binding-retry", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();

        let runtime_context = context.clone();
        let runtime_record = record.clone();
        std::thread::spawn(move || {
            let record_lock =
                crate::acquire_session_record_lock(&runtime_context, &runtime_record.id).unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let mut projection = ProxyProjection::new(runtime_context, runtime_record.clone());
                projection.observe_client(&json!({ "id": 1, "method": "thread/start" }));
                let (closed_sender, closed_receiver) = mpsc::channel(1);
                drop(closed_receiver);
                projection.sender = Some(closed_sender);

                let observed = tokio::time::timeout(
                    Duration::from_millis(100),
                    projection.observe_server_before_forward(&json!({
                        "id": 1,
                        "result": { "thread": { "id": "fresh-thread" } }
                    })),
                )
                .await;
                assert!(
                    observed.is_ok(),
                    "critical binding failure must return promptly"
                );
                assert!(observed.unwrap().is_err());
                let fail_close = projection.finish_fail_close();
                tokio::pin!(fail_close);
                assert!(
                    tokio::time::timeout(Duration::from_millis(50), &mut fail_close)
                        .await
                        .is_err(),
                    "durable fail-close should remain pending while the record lock is held"
                );
                drop(record_lock);
                fail_close.await;
            });
        })
        .join()
        .unwrap();

        let view = crate::auto_resume::view_for_record(&context, &record);
        assert!(!view.enabled);
        assert_eq!(view.state, "terminal_failure");
        assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));
    }

    #[test]
    fn disabled_fresh_projection_finish_waits_for_durable_fail_close() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("disabled-finish", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();

        let runtime_context = context.clone();
        let runtime_record = record.clone();
        std::thread::spawn(move || {
            let record_lock =
                crate::acquire_session_record_lock(&runtime_context, &runtime_record.id).unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let mut projection = ProxyProjection::new(runtime_context, runtime_record);
                projection.disable();
                let finish = projection.finish();
                tokio::pin!(finish);
                assert!(
                    tokio::time::timeout(Duration::from_millis(50), &mut finish)
                        .await
                        .is_err(),
                    "fresh disabled projection finalization must retain the fail-close retry"
                );
                drop(record_lock);
                finish.await;
            });
        })
        .join()
        .unwrap();

        let view = crate::auto_resume::view_for_record(&context, &record);
        assert!(!view.enabled);
        assert_eq!(view.state, "terminal_failure");
        assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));
    }

    #[test]
    fn disabled_bound_projection_finish_waits_for_durable_fail_close() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("disabled-bound-finish", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        bind_thread(&record, "bound-thread").unwrap();

        let runtime_context = context.clone();
        let runtime_record = record.clone();
        std::thread::spawn(move || {
            let record_lock =
                crate::acquire_session_record_lock(&runtime_context, &runtime_record.id).unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let mut projection = ProxyProjection::new(runtime_context, runtime_record);
                projection.disable();
                let finish = projection.finish();
                tokio::pin!(finish);
                assert!(
                    tokio::time::timeout(Duration::from_millis(50), &mut finish)
                        .await
                        .is_err(),
                    "bound disabled projection finalization must retain the fail-close retry"
                );
                drop(record_lock);
                finish.await;
            });
        })
        .join()
        .unwrap();

        let view = crate::auto_resume::view_for_record(&context, &record);
        assert!(!view.enabled);
        assert_eq!(view.state, "terminal_failure");
        assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));
    }

    #[tokio::test]
    async fn worker_failure_finish_waits_for_durable_fail_close() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("worker-fail-close", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        bind_thread(&record, "thread-a").unwrap();
        let record_lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();

        let mut projection = ProxyProjection::new(context.clone(), record.clone());
        projection.observe_client(&json!({
            "method": "turn/start",
            "params": { "threadId": "thread-a" }
        }));
        projection.observe_client(&json!({
            "method": "turn/start",
            "params": { "threadId": "thread-b" }
        }));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let release_lock = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            drop(record_lock);
        });

        projection.finish().await;
        let view = crate::auto_resume::view_for_record(&context, &record);
        assert!(!view.enabled);
        assert_eq!(view.state, "terminal_failure");
        assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));
        release_lock.await.unwrap();
    }

    #[tokio::test]
    async fn critical_binding_failure_closes_listener_before_fail_close_retry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let upstream = tmp.path().join("critical-listener.sock");
        let upstream_listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("critical-listener", &upstream);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        write_create_bootstrap_marker(&record);

        let listen = upstream.with_extension("proxy");
        let proxy_args = crate::cli::CodexAppServerProxyArgs {
            id: record.id.clone(),
            upstream: upstream.clone(),
            listen: listen.clone(),
        };
        let proxy_context = context.clone();
        let mut proxy =
            tokio::spawn(async move { run_proxy_session(proxy_context, proxy_args).await });
        let server_record = record.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = upstream_listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request = receive_json(&mut socket).await;
            assert_eq!(request["method"], "thread/start");
            bind_thread(&server_record, "conflicting-thread").unwrap();
            respond(
                &mut socket,
                &request,
                json!({ "thread": { "id": "fresh-thread" } }),
            )
            .await;
            let closed = tokio::time::timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("the upstream connection must close promptly");
            assert!(
                closed.is_none() || closed.is_some_and(|message| message.is_err()),
                "critical binding failure must close the upstream connection"
            );
        });
        let proxy_stream = connect_socket(&listen).await.unwrap();
        let (mut tui, _) = tokio_tungstenite::client_async("ws://localhost", proxy_stream)
            .await
            .unwrap();
        let record_lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
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
        let closed = tokio::time::timeout(Duration::from_secs(1), tui.next())
            .await
            .expect("the TUI connection must close promptly");
        assert!(
            closed.is_none() || closed.is_some_and(|message| message.is_err()),
            "critical binding failure must not forward the successful response"
        );

        let reconnect = tokio::time::timeout(
            Duration::from_millis(100),
            tokio::net::UnixStream::connect(&listen),
        )
        .await
        .expect("a retrying client must fail promptly");
        assert!(
            reconnect.is_err(),
            "the proxy listener must stop accepting while durable fail-close retries"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut proxy)
                .await
                .is_err(),
            "durable fail-close must remain pending while the record lock is held"
        );

        drop(record_lock);
        assert!(
            tokio::time::timeout(Duration::from_secs(3), &mut proxy)
                .await
                .expect("the proxy must finish after fail-close acquires the record lock")
                .unwrap()
                .is_err()
        );
        server.await.unwrap();
        let view = crate::auto_resume::view_for_record(&context, &record);
        assert!(!view.enabled);
        assert_eq!(view.state, "terminal_failure");
        assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));
    }

    #[tokio::test]
    async fn oversized_server_message_projects_only_bounded_fields() {
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
        let value = json!({
            "padding": "x".repeat(MAX_PROXY_OBSERVATION_BYTES * 5),
            "method": "turn/completed",
            "params": {
                "threadId": "thread-a",
                "turn": { "id": "turn-a", "status": "completed" }
            }
        });

        projection.observe_server(&value);
        tokio::time::sleep(Duration::from_millis(100)).await;
        projection.finish().await;

        let view = crate::auto_resume::view_for_record(&context, &record);
        assert!(view.enabled);
        assert_eq!(view.state, "enabled");
        assert_eq!(view.failure_reason, None);
    }

    #[tokio::test]
    async fn oversized_selected_unique_observation_fails_closed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("oversized-unique", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        let mut projection = ProxyProjection::new(context.clone(), record.clone());

        projection.observe_server(&json!({
            "method": "turn/completed",
            "params": {
                "threadId": "thread-a",
                "turn": {
                    "id": "turn-a",
                    "status": "failed",
                    "error": {
                        "codexErrorInfo": "x".repeat(MAX_PROXY_OBSERVATION_BYTES)
                    }
                }
            }
        }));
        for _ in 0..20 {
            let view = crate::auto_resume::view_for_record(&context, &record);
            if view.state == "terminal_failure" {
                assert!(!view.enabled);
                assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("oversized selected unique observation did not fail closed");
    }

    #[tokio::test]
    async fn saturated_projection_coalesces_repeatable_usage_updates() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("usage-coalesce", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        let mut projection = ProxyProjection::new(context.clone(), record.clone());

        for _ in 0..(MAX_PROXY_OBSERVATIONS + 4) {
            projection.observe_server(&json!({
                "method": "account/rateLimits/updated",
                "params": {
                    "rateLimits": {
                        "primary": { "usedPercent": 42.0, "resetsAt": 1_900_000_000_i64 }
                    }
                }
            }));
        }
        projection.finish().await;

        let view = crate::auto_resume::view_for_record(&context, &record);
        assert!(view.enabled);
        assert_eq!(view.state, "enabled");
        assert_eq!(view.failure_reason, None);
    }

    #[tokio::test]
    async fn saturated_projection_still_fails_closed_for_unique_events() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("unique-overflow", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        let mut projection = ProxyProjection::new(context.clone(), record.clone());

        for index in 0..(MAX_PROXY_OBSERVATIONS + 4) {
            projection.observe_server(&json!({
                "method": "turn/completed",
                "params": {
                    "threadId": "thread-a",
                    "turn": { "id": format!("turn-{index}"), "status": "completed" }
                }
            }));
        }
        for _ in 0..20 {
            let view = crate::auto_resume::view_for_record(&context, &record);
            if view.state == "terminal_failure" {
                assert!(!view.enabled);
                assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("unique projection overflow did not fail closed");
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
    async fn projection_fail_close_marks_unknown_while_activity_lock_is_held() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record =
            record_with_runtime("projection-activity-lock", &tmp.path().join("server.sock"));
        let dir = crate::session_dir(&context, &record.id);
        fs::create_dir_all(&dir).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join(".activity.lock"))
            .unwrap();
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);

        tokio::time::timeout(
            Duration::from_secs(2),
            fail_closed_projection(&context, &record),
        )
        .await
        .expect("activity-lock fail-close must not hang");
        assert_eq!(
            crate::activity::activity_status(&context, &record.id)
                .unwrap()
                .turn_state
                .phase,
            crate::activity::TurnPhase::Unknown
        );
        let view = crate::auto_resume::view_for_record(&context, &record);
        assert!(!view.enabled);
        assert_eq!(view.state, "terminal_failure");
        assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));

        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    }

    #[tokio::test]
    async fn open_usage_lock_contention_is_retryable_without_disabling_projection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("usage-wake-busy", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        let lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();

        wake_from_open_usage(
            &context,
            &record,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
                soonest_reset_epoch: None,
            },
        )
        .await
        .expect("advisory open-usage wake should defer while the lifecycle lock is busy");
        drop(lock);

        let view = crate::auto_resume::view_for_record(&context, &record);
        assert!(view.enabled);
        assert_eq!(view.state, "enabled");
        assert_eq!(view.failure_reason, None);
    }

    #[tokio::test]
    async fn open_usage_burst_during_lifecycle_lock_keeps_projection_enabled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("usage-wake-burst", &tmp.path().join("server.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        let mut projection = ProxyProjection::new(context.clone(), record.clone());
        projection.observe_client(&json!({
            "method": "turn/start",
            "params": { "threadId": "thread-a" }
        }));
        tokio::time::sleep(Duration::from_millis(25)).await;
        let lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        projection.observe_server(&json!({
            "method": "account/rateLimits/updated",
            "params": {
                "rateLimits": {
                    "primary": {
                        "usedPercent": 42.0,
                        "resetsAt": 1_900_000_000_i64
                    }
                }
            }
        }));
        tokio::time::sleep(Duration::from_millis(25)).await;

        for index in 0..(MAX_PROXY_OBSERVATIONS + 4) {
            projection.observe_server(&json!({
                "method": "turn/completed",
                "params": {
                    "threadId": "thread-a",
                    "turn": {
                        "id": format!("turn-{index}"),
                        "status": "completed"
                    }
                }
            }));
            tokio::task::yield_now().await;
        }
        drop(lock);
        tokio::time::sleep(Duration::from_millis(100)).await;
        projection.finish().await;

        let view = crate::auto_resume::view_for_record(&context, &record);
        assert!(view.enabled);
        assert_eq!(view.state, "enabled");
        assert_eq!(view.failure_reason, None);
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
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
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
                json!({
                    "data": ["raw-thread-decoy", "raw-thread-a"],
                    "nextCursor": null
                }),
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
    async fn control_reconnect_applies_external_auth_before_thread_discovery() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let broker = tmp.path().join("broker");
        fs::write(
            &broker,
            "#!/bin/sh\nprintf '%s\\n' '{\"schema_version\":\"agent-session.codex-auth-broker.v1\",\"account\":\"gamania\",\"access_token\":\"token-gamania\",\"chatgpt_account_id\":\"workspace-gamania\",\"plan\":\"team\"}'\n",
        )
        .unwrap();
        fs::set_permissions(&broker, fs::Permissions::from_mode(0o700)).unwrap();
        let _broker = EnvGuard::set(
            &lock,
            "AGENT_SESSION_CODEX_ACCOUNT_BROKER",
            &serde_json::to_string(&vec![broker.to_string_lossy().into_owned()]).unwrap(),
        );
        let socket_path = tmp.path().join("auth-reconnect.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut record = record_with_runtime("auth-reconnect", &socket_path);
        crate::codex_account::set_initial_binding(&mut record, Some("gamania")).unwrap();
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        bind_thread(&record, "raw-thread-auth").unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let initialize = receive_json(&mut socket).await;
            respond(&mut socket, &initialize, json!({})).await;
            assert_eq!(receive_json(&mut socket).await["method"], "initialized");
            let login = receive_json(&mut socket).await;
            assert_eq!(login["method"], "account/login/start");
            assert_eq!(login["params"]["accessToken"], "token-gamania");
            assert_eq!(login["params"]["chatgptAccountId"], "workspace-gamania");
            respond(&mut socket, &login, json!({ "type": "chatgptAuthTokens" })).await;
            let loaded = receive_json(&mut socket).await;
            assert_eq!(loaded["method"], "thread/loaded/list");
            respond(
                &mut socket,
                &loaded,
                json!({ "data": ["raw-thread-auth"], "nextCursor": null }),
            )
            .await;
            let resume = receive_json(&mut socket).await;
            respond(&mut socket, &resume, json!({})).await;
            for _ in 0..2 {
                let usage = receive_json(&mut socket).await;
                assert_eq!(usage["method"], "account/rateLimits/read");
                respond(
                    &mut socket,
                    &usage,
                    json!({ "rateLimits": { "primary": { "usedPercent": 1 } } }),
                )
                .await;
            }
            let prompt = receive_json(&mut socket).await;
            assert_eq!(prompt["method"], "turn/start");
            assert_eq!(
                prompt["params"]["input"][0]["text"],
                "next selected-account prompt"
            );
            respond(
                &mut socket,
                &prompt,
                json!({ "turn": { "id": "selected-account-turn" } }),
            )
            .await;
        });
        let (handle, commands) = control_channel();
        let control_context = context.clone();
        let control_record = record.clone();
        let control = tokio::spawn(run_control(control_context, control_record, commands));
        let usage = handle.usage().await;
        if let Err(error) = usage.as_ref() {
            drop(handle);
            let server_result = server.await;
            let control_result = control.await;
            panic!("usage failed: {error}; server={server_result:?}; control={control_result:?}");
        }
        assert!(usage.unwrap().authoritative);
        assert_eq!(
            handle
                .submit_prompt("next selected-account prompt")
                .await
                .unwrap(),
            "selected-account-turn"
        );
        server.await.unwrap();
        let persisted = crate::load_session_record(&context, &record.id).unwrap();
        let view = crate::codex_account::view_for_record(&persisted);
        assert_eq!(view.state, "bound");
        assert_eq!(view.selected_account.as_deref(), Some("gamania"));
        drop(handle);
        control.abort();
        let _ = control.await;
    }

    #[tokio::test]
    async fn apply_next_drains_a_queued_account_at_the_idle_boundary() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let broker = tmp.path().join("broker");
        fs::write(
            &broker,
            r#"#!/bin/sh
account=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --account)
      account="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
case "$account" in
  gamania|sym) ;;
  *) exit 2 ;;
esac
printf '%s\n' "{\"schema_version\":\"agent-session.codex-auth-broker.v1\",\"account\":\"$account\",\"access_token\":\"token-$account\",\"chatgpt_account_id\":\"workspace-$account\",\"plan\":\"team\"}"
"#,
        )
        .unwrap();
        fs::set_permissions(&broker, fs::Permissions::from_mode(0o700)).unwrap();
        let _broker = EnvGuard::set(
            &lock,
            "AGENT_SESSION_CODEX_ACCOUNT_BROKER",
            &serde_json::to_string(&vec![broker.to_string_lossy().into_owned()]).unwrap(),
        );
        let socket_path = tmp.path().join("apply-next.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut record = record_with_runtime("apply-next", &socket_path);
        crate::codex_account::set_initial_binding(&mut record, Some("gamania")).unwrap();
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        let turn_started = serde_json::from_value(json!({
            "schema_version": crate::activity::TURN_EVENT_VERSION,
            "event_id": "apply-next-turn-start",
            "runtime_id": "runtime-apply-next",
            "provider": "codex",
            "provider_turn_id": "turn-apply-next",
            "kind": "turn_started",
            "confidence": "authoritative"
        }))
        .unwrap();
        crate::activity::ingest_event(&context, &record.id, turn_started).unwrap();
        bind_thread(&record, "raw-thread-apply-next").unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let initialize = receive_json(&mut socket).await;
            respond(&mut socket, &initialize, json!({})).await;
            assert_eq!(receive_json(&mut socket).await["method"], "initialized");
            let first_login = receive_json(&mut socket).await;
            assert_eq!(first_login["method"], "account/login/start");
            assert_eq!(first_login["params"]["accessToken"], "token-gamania");
            respond(
                &mut socket,
                &first_login,
                json!({ "type": "chatgptAuthTokens" }),
            )
            .await;
            let loaded = receive_json(&mut socket).await;
            assert_eq!(loaded["method"], "thread/loaded/list");
            respond(
                &mut socket,
                &loaded,
                json!({ "data": ["raw-thread-apply-next"], "nextCursor": null }),
            )
            .await;
            let resume = receive_json(&mut socket).await;
            assert_eq!(resume["method"], "thread/resume");
            respond(&mut socket, &resume, json!({})).await;
            for _ in 0..2 {
                let usage = receive_json(&mut socket).await;
                assert_eq!(usage["method"], "account/rateLimits/read");
                respond(
                    &mut socket,
                    &usage,
                    json!({ "rateLimits": { "primary": { "usedPercent": 1 } } }),
                )
                .await;
            }
            let next_login = receive_json(&mut socket).await;
            assert_eq!(next_login["method"], "account/login/start");
            assert_eq!(next_login["params"]["accessToken"], "token-sym");
            assert_eq!(next_login["params"]["chatgptAccountId"], "workspace-sym");
            respond(
                &mut socket,
                &next_login,
                json!({ "type": "chatgptAuthTokens" }),
            )
            .await;
        });

        let (handle, commands) = control_channel();
        let control = tokio::spawn(run_control(context.clone(), record.clone(), commands));
        assert!(handle.usage().await.unwrap().authoritative);
        crate::codex_account::queue_next_account(&context, &record.id, "runtime-apply-next", "sym")
            .unwrap();
        let turn_completed = serde_json::from_value(json!({
            "schema_version": crate::activity::TURN_EVENT_VERSION,
            "event_id": "apply-next-turn-completed",
            "runtime_id": "runtime-apply-next",
            "provider": "codex",
            "provider_turn_id": "turn-apply-next",
            "kind": "turn_completed",
            "confidence": "authoritative"
        }))
        .unwrap();
        crate::activity::ingest_event(&context, &record.id, turn_completed).unwrap();

        handle.apply_next().await.unwrap();
        server.await.unwrap();
        let persisted = crate::load_session_record(&context, &record.id).unwrap();
        let view = crate::codex_account::view_for_record(&persisted);
        assert_eq!(view.selected_account.as_deref(), Some("sym"));
        assert!(view.next.is_none());
        drop(handle);
        control.abort();
        let _ = control.await;
    }

    #[tokio::test]
    async fn submit_prompt_rejects_response_without_acknowledged_turn_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket_path = tmp.path().join("missing-turn-id.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("missing-turn-id", &socket_path);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        bind_thread(&record, "thread-missing-turn-id").unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let initialize = receive_json(&mut socket).await;
            respond(&mut socket, &initialize, json!({})).await;
            assert_eq!(receive_json(&mut socket).await["method"], "initialized");
            let loaded = receive_json(&mut socket).await;
            assert_eq!(loaded["method"], "thread/loaded/list");
            respond(
                &mut socket,
                &loaded,
                json!({ "data": ["thread-missing-turn-id"], "nextCursor": null }),
            )
            .await;
            let resume = receive_json(&mut socket).await;
            assert_eq!(resume["method"], "thread/resume");
            respond(&mut socket, &resume, json!({})).await;
            let usage = receive_json(&mut socket).await;
            assert_eq!(usage["method"], "account/rateLimits/read");
            respond(
                &mut socket,
                &usage,
                json!({ "rateLimits": { "primary": { "usedPercent": 1 } } }),
            )
            .await;
            let prompt = receive_json(&mut socket).await;
            assert_eq!(prompt["method"], "turn/start");
            respond(
                &mut socket,
                &prompt,
                json!({ "turn": { "status": "inProgress" } }),
            )
            .await;
        });
        let (handle, commands) = control_channel();
        let control = tokio::spawn(run_control(context, record, commands));

        let error = handle
            .submit_prompt("must require an acknowledged turn id")
            .await
            .unwrap_err();
        assert_eq!(
            error,
            "Codex turn/start response omitted the acknowledged turn id"
        );
        server.await.unwrap();
        drop(handle);
        control.abort();
        let _ = control.await;
    }

    #[tokio::test]
    async fn failed_reconnect_waits_for_explicit_rebind_before_thread_discovery() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let broker = tmp.path().join("broker");
        fs::write(
            &broker,
            "#!/bin/sh\nprintf '%s\\n' '{\"schema_version\":\"agent-session.codex-auth-broker.v1\",\"account\":\"gamania\",\"access_token\":\"token-gamania\",\"chatgpt_account_id\":\"workspace-gamania\",\"plan\":\"team\"}'\n",
        )
        .unwrap();
        fs::set_permissions(&broker, fs::Permissions::from_mode(0o700)).unwrap();
        let _broker = EnvGuard::set(
            &lock,
            "AGENT_SESSION_CODEX_ACCOUNT_BROKER",
            &serde_json::to_string(&vec![broker.to_string_lossy().into_owned()]).unwrap(),
        );
        let socket_path = tmp.path().join("failed-reconnect.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut record = record_with_runtime("failed-reconnect", &socket_path);
        crate::codex_account::set_initial_binding(&mut record, Some("gamania")).unwrap();
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::codex_account::finish_binding(
            &context,
            &record.id,
            "runtime-failed-reconnect",
            "gamania",
            1,
            Err("apply_failed"),
        )
        .unwrap();
        record = crate::load_session_record(&context, &record.id).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::activity::ingest_codex_app_server_failure(
            &context,
            &record.id,
            "runtime-failed-reconnect",
            "thread-failed",
            "turn-failed",
        )
        .unwrap();
        bind_thread(&record, "raw-thread-failed").unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let initialize = receive_json(&mut socket).await;
            respond(&mut socket, &initialize, json!({})).await;
            assert_eq!(receive_json(&mut socket).await["method"], "initialized");
            let login = receive_json(&mut socket).await;
            assert_eq!(login["method"], "account/login/start");
            respond(&mut socket, &login, json!({ "type": "chatgptAuthTokens" })).await;
            let loaded = receive_json(&mut socket).await;
            assert_eq!(loaded["method"], "thread/loaded/list");
            respond(
                &mut socket,
                &loaded,
                json!({ "data": ["raw-thread-failed"], "nextCursor": null }),
            )
            .await;
            let resume = receive_json(&mut socket).await;
            assert_eq!(resume["method"], "thread/resume");
            respond(&mut socket, &resume, json!({})).await;
            let usage = receive_json(&mut socket).await;
            assert_eq!(usage["method"], "account/rateLimits/read");
            respond(
                &mut socket,
                &usage,
                json!({ "rateLimits": { "primary": { "usedPercent": 1 } } }),
            )
            .await;
        });
        let (handle, commands) = control_channel();
        let control = tokio::spawn(run_control(context.clone(), record.clone(), commands));

        assert!(handle.usage().await.is_err());
        let revision = crate::codex_account::begin_switch_binding(
            &context,
            &record.id,
            "runtime-failed-reconnect",
            "gamania",
        )
        .unwrap();
        assert_eq!(revision, 2);
        let view = handle.bind_account("gamania", revision).await.unwrap();
        assert_eq!(view.state, "bound");
        assert_eq!(
            view.applied_runtime_id.as_deref(),
            Some("runtime-failed-reconnect")
        );
        server.await.unwrap();
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
        let session_dir = crate::session_dir(&context, id);
        fs::create_dir_all(&session_dir).unwrap();
        crate::write_private_file(
            &session_dir.join(crate::STARTUP_DIAGNOSTIC_FILE),
            b"local-only startup detail\n",
        )
        .unwrap();
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

        assert_eq!(
            fs::read_to_string(session_dir.join(crate::STARTUP_STAGE_FILE)).unwrap(),
            "initial_connection\n"
        );
        assert!(!session_dir.join(crate::STARTUP_DIAGNOSTIC_FILE).exists());
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
                    soonest_reset_epoch: None,
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
    async fn manual_input_section_bypasses_reentrant_lock_until_sender_cleanup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let upstream = tmp.path().join("owned-input.sock");
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("owned-input", &upstream);
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
            assert_eq!(request["method"], "turn/start");
            respond(
                &mut socket,
                &request,
                json!({ "turn": { "id": "owned-turn", "status": "inProgress" } }),
            )
            .await;
            let request = receive_json(&mut socket).await;
            assert_eq!(request["id"], 3);
            assert_eq!(request["method"], "thread/read");
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
        for _ in 0..100 {
            if live_proxy_capability(&context, &record) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let marker = begin_manual_input_section(&context, &record)
            .unwrap()
            .unwrap();
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
        let response = receive_json(&mut tui).await;
        assert_eq!(response["result"]["turn"]["id"], "owned-turn");
        assert!(
            manual_input_section_path(&context, &record).exists(),
            "manual input section remains live until the sender releases its lock"
        );
        marker.finish(|| drop(lock));
        let unrelated_lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        tui.send(Message::Text(
            json!({
                "id": 2,
                "method": "turn/start",
                "params": { "threadId": "thread-a", "input": [] }
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
        let rejected = receive_json(&mut tui).await;
        assert_eq!(rejected["id"], 2);
        assert_eq!(rejected["error"]["code"], -32001);
        drop(unrelated_lock);
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

    #[test]
    fn local_input_ownership_rejects_expiry_and_runtime_replacement() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("owned-negative", &tmp.path().join("negative.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        bind_thread(&record, "thread-a").unwrap();
        let turn = json!({
            "id": 1,
            "method": "turn/start",
            "params": { "threadId": "thread-a", "input": [] }
        });

        let expired = write_manual_input_marker(
            &context,
            &record,
            &record.runtime.as_ref().unwrap().launch_id,
            epoch_millis().unwrap().saturating_sub(1),
        );
        assert!(acquire_manual_input_gate(&context, &record, &turn).is_none());
        drop(expired);

        let replacement = write_manual_input_marker(
            &context,
            &record,
            "replacement-runtime",
            epoch_millis()
                .unwrap()
                .saturating_add(u64::try_from(MANUAL_INPUT_SECTION_TTL.as_millis()).unwrap()),
        );
        assert!(acquire_manual_input_gate(&context, &record, &turn).is_none());
        drop(replacement);

        let valid = write_manual_input_marker(
            &context,
            &record,
            &record.runtime.as_ref().unwrap().launch_id,
            epoch_millis()
                .unwrap()
                .saturating_add(u64::try_from(MANUAL_INPUT_SECTION_TTL.as_millis()).unwrap()),
        );
        let wrong_thread = json!({
            "id": 2,
            "method": "turn/start",
            "params": { "threadId": "thread-b", "input": [] }
        });
        assert!(acquire_manual_input_gate(&context, &record, &wrong_thread).is_none());
        assert!(manual_input_section_path(&context, &record).exists());
        let malformed = json!({
            "id": 3,
            "method": "turn/start",
            "params": { "threadId": "thread-a" }
        });
        assert!(acquire_manual_input_gate(&context, &record, &malformed).is_none());
        assert!(manual_input_section_path(&context, &record).exists());
        drop(acquire_manual_input_gate(&context, &record, &turn).unwrap());
        drop(valid);
    }

    #[test]
    fn manual_input_submission_detection_excludes_ordinary_text_and_keys() {
        for key in [
            crate::cli::SpecialKey::Escape,
            crate::cli::SpecialKey::Backspace,
            crate::cli::SpecialKey::CtrlC,
            crate::cli::SpecialKey::Up,
            crate::cli::SpecialKey::Down,
            crate::cli::SpecialKey::Left,
            crate::cli::SpecialKey::Right,
            crate::cli::SpecialKey::Tab,
        ] {
            assert!(
                !input_contains_submission(None, &[key]),
                "non-submitting key must not open a manual section: {key:?}"
            );
        }
        assert!(input_contains_submission(
            None,
            &[crate::cli::SpecialKey::Enter]
        ));
        for text in ["\r", "\n", "\r\n"] {
            assert!(
                input_contains_submission(Some(text), &[]),
                "terminal submit must open a manual section: {text:?}"
            );
        }
        for text in [
            "",
            "hi",
            "line one\nline two",
            "\u{1b}[200~pasted\ntext\u{1b}[201~",
        ] {
            assert!(
                !input_contains_submission(Some(text), &[]),
                "ordinary text and multiline paste must not open a manual section: {text:?}"
            );
        }
    }

    #[test]
    fn manual_input_section_requires_live_proxy_capability_and_cleans_up() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("owned-capability", &tmp.path().join("capability.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();

        let error = begin_manual_input_section(&context, &record).unwrap_err();
        assert_eq!(error.code(), "codex-input-section-unavailable");

        let capability = begin_proxy_capability(&context, &record).unwrap();
        let marker = begin_manual_input_section(&context, &record)
            .unwrap()
            .unwrap();
        assert!(manual_input_section_path(&context, &record).exists());
        drop(marker);
        assert!(!manual_input_section_path(&context, &record).exists());
        drop(capability);
        assert!(proxy_capability_path(&context, &record).exists());
        assert!(!live_proxy_capability(&context, &record));
    }

    #[test]
    fn manual_input_gate_finishes_before_lifecycle_unlock_and_marker_cleanup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("owned-gate", &tmp.path().join("gate.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        bind_thread(&record, "thread-a").unwrap();
        let _capability = begin_proxy_capability(&context, &record).unwrap();
        let lifecycle_lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        let marker = begin_manual_input_section(&context, &record)
            .unwrap()
            .unwrap();
        let turn = json!({
            "id": 1,
            "method": "turn/start",
            "params": { "threadId": "thread-a", "input": [] }
        });
        let gate = acquire_manual_input_gate(&context, &record, &turn).unwrap();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let teardown = std::thread::spawn(move || {
            marker.finish(|| drop(lifecycle_lock));
            finished_tx.send(()).unwrap();
        });

        assert!(finished_rx.recv_timeout(Duration::from_millis(10)).is_err());
        assert!(
            crate::try_acquire_session_record_lock(&context, &record.id)
                .unwrap()
                .is_none(),
            "lifecycle lock stays held while the proxy forwards through the gate"
        );
        drop(gate);
        finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        teardown.join().unwrap();
        assert!(!manual_input_section_path(&context, &record).exists());
        assert!(
            crate::try_acquire_session_record_lock(&context, &record.id)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn manual_input_gate_timeout_releases_lifecycle_and_invalidates_marker() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("owned-timeout", &tmp.path().join("timeout.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        let _capability = begin_proxy_capability(&context, &record).unwrap();
        let lifecycle_lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        let mut marker = begin_manual_input_section(&context, &record)
            .unwrap()
            .unwrap();
        let held_gate =
            open_manual_input_gate_file(&manual_input_gate_path(&context, &record)).unwrap();
        assert!(lock_file_timed(&held_gate, Duration::from_millis(10)));
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let teardown = std::thread::spawn(move || {
            marker.finish_with_timeout(|| drop(lifecycle_lock), Duration::from_millis(20));
            finished_tx.send(()).unwrap();
        });

        finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        teardown.join().unwrap();
        assert!(!manual_input_section_path(&context, &record).exists());
        assert!(
            crate::try_acquire_session_record_lock(&context, &record.id)
                .unwrap()
                .is_some()
        );
        unlock_bootstrap_file(&held_gate);
    }

    #[test]
    fn delayed_proxy_claim_acknowledges_before_sender_retires_section() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("owned-delayed", &tmp.path().join("delayed.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        bind_thread(&record, "thread-a").unwrap();
        let _capability = begin_proxy_capability(&context, &record).unwrap();
        let lifecycle_lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        let marker = begin_manual_input_section(&context, &record)
            .unwrap()
            .unwrap();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let teardown = std::thread::spawn(move || {
            marker.finish(|| drop(lifecycle_lock));
            finished_tx.send(()).unwrap();
        });
        std::thread::sleep(Duration::from_millis(25));
        let turn = json!({
            "id": 1,
            "method": "turn/start",
            "params": { "threadId": "thread-a", "input": [] }
        });
        let gate = acquire_manual_input_gate(&context, &record, &turn).unwrap();
        assert!(finished_rx.recv_timeout(Duration::from_millis(10)).is_err());
        assert!(
            crate::try_acquire_session_record_lock(&context, &record.id)
                .unwrap()
                .is_none()
        );
        drop(gate);
        finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        teardown.join().unwrap();
    }

    #[test]
    fn stale_manual_marker_without_owner_lease_cannot_authorize_busy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("owned-stale", &tmp.path().join("stale.sock"));
        let session_dir = crate::session_dir(&context, &record.id);
        fs::create_dir_all(&session_dir).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        bind_thread(&record, "thread-a").unwrap();
        let _capability = begin_proxy_capability(&context, &record).unwrap();
        let lifecycle_lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        let marker = begin_manual_input_section(&context, &record)
            .unwrap()
            .unwrap();
        fs::set_permissions(&session_dir, fs::Permissions::from_mode(0o500)).unwrap();
        marker.finish(|| drop(lifecycle_lock));
        fs::set_permissions(&session_dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(manual_input_section_path(&context, &record).exists());
        let unrelated_lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        let turn = json!({
            "id": 1,
            "method": "turn/start",
            "params": { "threadId": "thread-a", "input": [] }
        });
        assert!(acquire_manual_input_gate(&context, &record, &turn).is_none());
        drop(unrelated_lock);
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
    fn fresh_bootstrap_allows_pre_enabled_auto_resume() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("fresh-enabled", &tmp.path().join("enabled.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        write_create_bootstrap_marker(&record);
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();

        let mut bootstrap = FreshBootstrap::for_runtime(&context, &record);
        assert!(bootstrap.bypasses_create_lock(
            &context,
            &record,
            &json!({ "id": 1, "method": "thread/start", "params": {} }),
        ));
        bootstrap
            .observe_server(&json!({ "id": 1, "result": { "thread": { "id": "fresh-thread" } } }));
        assert!(bootstrap.bypasses_create_lock(
            &context,
            &record,
            &json!({
                "id": 2,
                "method": "turn/start",
                "params": { "threadId": "fresh-thread", "input": [] }
            }),
        ));
    }

    #[test]
    fn fresh_bootstrap_accepts_only_healthy_idle_auto_resume_states() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("fresh-matrix", &tmp.path().join("matrix.sock"));
        let session_dir = crate::session_dir(&context, &record.id);
        fs::create_dir_all(&session_dir).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        write_create_bootstrap_marker(&record);

        for (name, enabled, state, scheduled_at, failure_reason, accepted) in [
            ("healthy-disabled", false, "disabled", None, None, true),
            ("healthy-enabled", true, "enabled", None, None, true),
            (
                "disabled-flag-mismatch",
                true,
                "disabled",
                None,
                None,
                false,
            ),
            ("enabled-flag-mismatch", false, "enabled", None, None, false),
            ("armed", true, "armed", None, None, false),
            (
                "scheduled",
                true,
                "scheduled",
                Some("2030-01-01T01:00:00Z"),
                None,
                false,
            ),
            ("checking", true, "checking", None, None, false),
            (
                "transient-failure",
                true,
                "transient_failure",
                None,
                Some("usage_unavailable"),
                false,
            ),
            (
                "terminal-failure",
                false,
                "terminal_failure",
                None,
                Some("state_unavailable"),
                false,
            ),
            (
                "enabled-with-schedule",
                true,
                "enabled",
                Some("2030-01-01T01:00:00Z"),
                None,
                false,
            ),
            (
                "enabled-with-failure",
                true,
                "enabled",
                None,
                Some("usage_unavailable"),
                false,
            ),
            (
                "disabled-with-schedule",
                false,
                "disabled",
                Some("2030-01-01T01:00:00Z"),
                None,
                false,
            ),
            (
                "disabled-with-failure",
                false,
                "disabled",
                None,
                Some("state_unavailable"),
                false,
            ),
        ] {
            fs::write(
                session_dir.join("auto-resume.json"),
                serde_json::to_vec(&json!({
                    "schema_version": "agent-session.auto-resume.v1",
                    "enabled": enabled,
                    "state": state,
                    "updated_at": "2030-01-01T00:00:00Z",
                    "scheduled_at": scheduled_at,
                    "failure_reason": failure_reason,
                    "attempt": 0,
                    "ever_scheduled": false,
                }))
                .unwrap(),
            )
            .unwrap();

            assert_eq!(
                FreshBootstrap::for_runtime(&context, &record) == FreshBootstrap::ThreadStart,
                accepted,
                "unexpected bootstrap decision for {name}",
            );
        }
    }

    #[test]
    fn fresh_bootstrap_rejects_pre_armed_auto_resume() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("fresh-armed", &tmp.path().join("armed.sock"));
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        write_create_bootstrap_marker(&record);
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();
        assert!(
            crate::auto_resume::arm_usage_exhaustion(
                &context,
                &record.id,
                "blocked-turn".to_string(),
                1,
                "2030-01-01T00:00:01Z",
            )
            .unwrap()
        );

        assert_eq!(
            FreshBootstrap::for_runtime(&context, &record),
            FreshBootstrap::Closed
        );
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
        normal_cancellation_attempts()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&record.id);
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
            normal_cancellation_attempts()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&record.id)
                .copied()
                .unwrap_or_default(),
            1
        );
        assert_eq!(bootstrap, FreshBootstrap::Closed);
    }

    #[tokio::test]
    async fn broker_bound_tui_rejects_account_auth_mutations() {
        let lock = GlobalStateLock::new();
        let tmp = tempfile::TempDir::new().unwrap();
        let _broker = EnvGuard::set(
            &lock,
            "AGENT_SESSION_CODEX_ACCOUNT_BROKER",
            r#"["/configured/broker"]"#,
        );
        let socket = tmp.path().join("broker-bound-auth.sock");
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let mut record = record_with_runtime("broker-bound-auth", &socket);
        crate::codex_account::set_initial_binding(&mut record, Some("gamania")).unwrap();

        for method in [
            "account/login/start",
            "account/login/cancel",
            "account/logout",
        ] {
            let mut bootstrap = FreshBootstrap::Closed;
            assert!(
                cancel_before_tui_mutation(
                    &context,
                    &record,
                    &mut bootstrap,
                    &json!({ "id": 1, "method": method, "params": {} }),
                )
                .await
                .is_none(),
                "broker-bound TUI mutation {method} must be rejected"
            );
        }
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
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if BOOTSTRAP_GATE_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the blocking bootstrap gate attempt should start within the test deadline");
        assert!(
            BOOTSTRAP_GATE_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed) >= 1,
            "the test-owned gate attempt must be observable even when parallel tests also use the global probe"
        );
        fs::remove_file(thread_handoff_path(&record).unwrap()).unwrap();
        unlock_bootstrap_file(&bootstrap_guard.file);

        let (authorized, _) = task.await.unwrap();
        assert!(!authorized);
        drop(replacement_lock);
        drop(bootstrap_guard);
    }

    #[tokio::test]
    async fn stalled_upstream_write_times_out_and_releases_bootstrap_gate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let record = record_with_runtime("stalled-write", &tmp.path().join("stalled.sock"));
        fs::create_dir_all(crate::session_dir(
            &CliContext {
                state_dir: tmp.path().join("state"),
                host: None,
            },
            &record.id,
        ))
        .unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        crate::write_session_record(&context, &record).unwrap();
        let lifecycle_lock = crate::acquire_session_record_lock(&context, &record.id).unwrap();
        let bootstrap_guard = begin_create_bootstrap(&record).unwrap().unwrap();
        let authorization = MutationAuthorization {
            _bootstrap_gate: acquire_create_bootstrap_gate(&record),
            _manual_input_gate: None,
        };
        assert!(authorization._bootstrap_gate.is_some());
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let teardown = std::thread::spawn(move || {
            bootstrap_guard.finish(|| drop(lifecycle_lock));
            finished_tx.send(()).unwrap();
        });
        assert!(
            finished_rx.recv_timeout(Duration::from_millis(10)).is_err(),
            "teardown must wait while the proxy owns the gate"
        );
        let mut upstream = PendingMessageSink;
        let error = send_proxy_upstream(
            &mut upstream,
            Message::Text("stalled".into()),
            Duration::from_millis(10),
        )
        .await
        .unwrap_err();
        assert_eq!(error, "upstream app-server write timed out");
        drop(authorization);
        finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        teardown.join().unwrap();
        assert!(
            crate::try_acquire_session_record_lock(&context, &record.id)
                .unwrap()
                .is_some()
        );
        assert!(!thread_handoff_path(&record).unwrap().exists());
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
                soonest_reset_epoch: None,
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
        assert_eq!(view.state, "terminal_failure");
        assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));
    }

    #[tokio::test]
    async fn proxy_transport_loss_durably_fails_closed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let upstream = tmp.path().join("transport-loss.sock");
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let record = record_with_runtime("transport-loss", &upstream);
        fs::create_dir_all(crate::session_dir(&context, &record.id)).unwrap();
        crate::write_session_record(&context, &record).unwrap();
        crate::activity::activate_runtime(&context, &record).unwrap();
        crate::auto_resume::set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .unwrap();

        let proxy_args = crate::cli::CodexAppServerProxyArgs {
            id: record.id.clone(),
            upstream: upstream.clone(),
            listen: upstream.with_extension("proxy"),
        };
        let proxy_context = context.clone();
        let proxy = tokio::spawn(async move { run_proxy_session(proxy_context, proxy_args).await });
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            drop(socket);
        });
        let proxy_stream = connect_socket(&upstream.with_extension("proxy"))
            .await
            .unwrap();
        let (tui, _) = tokio_tungstenite::client_async("ws://localhost", proxy_stream)
            .await
            .unwrap();
        server.await.unwrap();
        let result = proxy.await.unwrap();
        assert!(
            result.is_err(),
            "unexpected upstream EOF must fail the proxy"
        );
        drop(tui);

        let state = crate::activity::activity_status(&context, &record.id)
            .unwrap()
            .turn_state;
        assert_eq!(state.phase, crate::activity::TurnPhase::Unknown);
        let view = crate::auto_resume::view_for_record(&context, &record);
        assert!(!view.enabled);
        assert_eq!(view.state, "terminal_failure");
        assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));
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
            coordination_mode: crate::cli::CoordinationMode::Advisory,
            title: None,
            title_state: None,
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

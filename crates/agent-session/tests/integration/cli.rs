use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn run(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    let options = CmdOptions::new().with_cwd(dir).with_envs(envs);
    run_resolved("agent-session", args, &options)
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write executable");
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod executable");
}

fn fake_tmux(tmp: &Path) -> (PathBuf, PathBuf) {
    let bin = tmp.join("tmux");
    let log = tmp.join("tmux.log");
    write_executable(
        &bin,
        r#"#!/usr/bin/env sh
: "${AGENT_SESSION_FAKE_TMUX_LOG:?}"
for arg in "$@"; do
  printf '%s\000' "$arg" >> "$AGENT_SESSION_FAKE_TMUX_LOG"
done
printf '\036' >> "$AGENT_SESSION_FAKE_TMUX_LOG"

operation="$1"
if [ "$operation" = "if-shell" ]; then
  operation="kill-session"
fi
if [ "${AGENT_SESSION_FAKE_TMUX_FAIL:-}" = "$operation" ]; then
  echo "fake tmux failed at $operation" >&2
  exit 42
fi

if [ "${AGENT_SESSION_FAKE_TMUX_ABSENT:-0}" = "1" ] && { [ "$1" = "display-message" ] || [ "$1" = "has-session" ]; }; then
  printf "%s\n" "can't find session: ${target:-unknown}" >&2
  exit 1
fi

if [ "${AGENT_SESSION_FAKE_TMUX_ABSENT_BEFORE_LAUNCH:-0}" = "1" ] && { [ "$1" = "display-message" ] || [ "$1" = "has-session" ]; } && [ ! -f "$AGENT_SESSION_FAKE_TMUX_LOG.launched" ]; then
  printf "%s\n" "can't find session: ${target:-unknown}" >&2
  exit 1
fi

if [ "${AGENT_SESSION_FAKE_TMUX_BLANK_DISPLAY:-0}" = "1" ] && [ "$1" = "display-message" ]; then
  exit 0
fi

target=""
previous=""
for arg in "$@"; do
  if [ "$previous" = "-t" ]; then
    target="$arg"
    break
  fi
  previous="$arg"
done

if [ "$1" = "kill-session" ]; then
  printf '%s\n' "$target" >> "$AGENT_SESSION_FAKE_TMUX_LOG.killed"
  if [ -n "${AGENT_SESSION_FAKE_TMUX_PROCESS_GROUP_ID:-}" ] && [ "${AGENT_SESSION_FAKE_TMUX_KEEP_PROCESS_GROUP:-0}" != "1" ]; then
    kill -TERM "-${AGENT_SESSION_FAKE_TMUX_PROCESS_GROUP_ID}" 2>/dev/null || true
  fi
  exit 0
fi

if [ "$1" = "if-shell" ]; then
  kill_target=""
  for arg in "$@"; do
    case "$arg" in
      'kill-session -t '*) kill_target="${arg#kill-session -t }" ;;
    esac
  done
  [ -n "$kill_target" ] || exit 42
  printf '%s\n' "$kill_target" >> "$AGENT_SESSION_FAKE_TMUX_LOG.killed"
  if [ -n "${AGENT_SESSION_FAKE_TMUX_PROCESS_GROUP_ID:-}" ] && [ "${AGENT_SESSION_FAKE_TMUX_KEEP_PROCESS_GROUP:-0}" != "1" ]; then
    kill -TERM "-${AGENT_SESSION_FAKE_TMUX_PROCESS_GROUP_ID}" 2>/dev/null || true
  fi
  exit 0
fi

if [ "$1" = "has-session" ]; then
  if [ "${AGENT_SESSION_FAKE_TMUX_HAS_SESSION:-1}" = "0" ]; then
    printf "%s\n" "can't find session: $target" >&2
    exit 1
  fi
  if [ -f "$AGENT_SESSION_FAKE_TMUX_LOG.killed" ]; then
    while IFS= read -r killed_target; do
      if [ "$killed_target" = "$target" ]; then
        printf "%s\n" "can't find session: $target" >&2
        exit 1
      fi
    done < "$AGENT_SESSION_FAKE_TMUX_LOG.killed"
  fi
  exit 0
fi

if [ "$1" = "display-message" ] && [ -n "${AGENT_SESSION_FAKE_TMUX_PANE_PID:-}" ]; then
  last_arg=""
  for arg in "$@"; do
    last_arg="$arg"
  done
  case "$last_arg" in
    *'#{pane_pid}')
      session_identity="${AGENT_SESSION_FAKE_TMUX_SESSION_ID:-}"
      [ -n "$session_identity" ] || session_identity='$77'
      pane_identity="${AGENT_SESSION_FAKE_TMUX_PANE_ID:-}"
      [ -n "$pane_identity" ] || pane_identity='%77'
      printf '%s\t%s\t%s\n' "$session_identity" "$pane_identity" "$AGENT_SESSION_FAKE_TMUX_PANE_PID"
      exit 0
      ;;
  esac
fi

if [ "$1" = "show-environment" ]; then
  case "$4" in
    AGENT_SESSION_ID) printf 'AGENT_SESSION_ID=%s\n' "$AGENT_SESSION_FAKE_TMUX_AGENT_SESSION_ID" ;;
    AGENT_SESSION_STATE_DIR) printf 'AGENT_SESSION_STATE_DIR=%s\n' "$AGENT_SESSION_FAKE_TMUX_STATE_DIR" ;;
    AGENT_SESSION_RUNTIME_ID)
      runtime_id="${AGENT_SESSION_FAKE_TMUX_RUNTIME_ID:-}"
      if [ -z "$runtime_id" ] && [ -n "${AGENT_SESSION_FAKE_TMUX_RUNTIME_RECORD:-}" ]; then
        runtime_id="$(sed -n 's/.*"launch_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$AGENT_SESSION_FAKE_TMUX_RUNTIME_RECORD" | head -n 1)"
      fi
      printf 'AGENT_SESSION_RUNTIME_ID=%s\n' "$runtime_id" ;;
    *) exit 1 ;;
  esac
  exit 0
fi

if [ "$1" = "capture-pane" ]; then
  if [ "${AGENT_SESSION_FAKE_TMUX_CAPTURE+x}" = "x" ]; then
    printf '%s' "$AGENT_SESSION_FAKE_TMUX_CAPTURE"
    exit 0
  fi
  printf 'pane one\npane two\n'
  exit 0
fi

if [ "$1" = "list-windows" ]; then
  if [ "${AGENT_SESSION_FAKE_TMUX_LIST_WINDOWS+x}" = "x" ]; then
    printf '%s\n' "$AGENT_SESSION_FAKE_TMUX_LIST_WINDOWS"
    exit 0
  fi
  exit 1
fi

if [ "$1" = "display-message" ] && [ "${AGENT_SESSION_FAKE_TMUX_WINDOW_ACTIVITY+x}" = "x" ]; then
  printf '%s\n' "$AGENT_SESSION_FAKE_TMUX_WINDOW_ACTIVITY"
  exit 0
fi

if [ "$1" = "new-session" ] && [ "${AGENT_SESSION_FAKE_CODEX_SESSION_FILE+x}" = "x" ]; then
  cwd="${AGENT_SESSION_FAKE_CODEX_CWD:-}"
  if [ -z "$cwd" ]; then
    previous=""
    for arg in "$@"; do
      if [ "$previous" = "-c" ]; then
        cwd="$arg"
        break
      fi
      previous="$arg"
    done
  fi
  session_id="${AGENT_SESSION_FAKE_CODEX_SESSION_ID:-fake-codex-session}"
  timestamp="${AGENT_SESSION_FAKE_CODEX_SESSION_TIMESTAMP:-2099-01-01T00:00:00Z}"
  old_ifs="$IFS"
  IFS=":"
  index=1
  for file in $AGENT_SESSION_FAKE_CODEX_SESSION_FILE; do
    IFS="$old_ifs"
    current_id="$session_id"
    if [ $index -gt 1 ]; then
      current_id="${session_id}-${index}"
    fi
    mkdir -p "$(dirname "$file")"
    if [ "${AGENT_SESSION_FAKE_CODEX_APPEND:-0}" = "1" ]; then
      printf '{"type":"event","timestamp":"%s"}\n' "$timestamp" >> "$file"
    else
      printf '{"timestamp":"%s","type":"session_meta","payload":{"id":"%s","session_id":"%s","cwd":"%s","source":"cli","timestamp":"%s"}}\n' "$timestamp" "$current_id" "$current_id" "$cwd" "$timestamp" > "$file"
    fi
    IFS=":"
    index=$((index + 1))
  done
  IFS="$old_ifs"
fi

if [ "$1" = "new-session" ] && [ "${AGENT_SESSION_FAKE_RECORD_AT_NEW_SESSION+x}" = "x" ]; then
  if ! grep -Eq '"generation"[[:space:]]*:[[:space:]]*2' "$AGENT_SESSION_FAKE_RECORD_AT_NEW_SESSION"; then
    printf 'runtime generation was not persisted before tmux launch\n' >&2
    exit 43
  fi
fi

if [ "$1" = "new-session" ] && [ "${AGENT_SESSION_FAKE_CHMOD_AFTER_NEW_SESSION+x}" = "x" ]; then
  chmod "${AGENT_SESSION_FAKE_CHMOD_MODE:-0500}" "$AGENT_SESSION_FAKE_CHMOD_AFTER_NEW_SESSION"
fi

if [ "$1" = "new-session" ] && [ "${AGENT_SESSION_FAKE_MKDIR_AFTER_NEW_SESSION+x}" = "x" ]; then
  mkdir -p "$AGENT_SESSION_FAKE_MKDIR_AFTER_NEW_SESSION"
fi

if [ "$1" = "new-session" ]; then
  : > "$AGENT_SESSION_FAKE_TMUX_LOG.launched"
  if [ "${AGENT_SESSION_FAKE_TMUX_MALFORMED_LAUNCH_IDENTITY:-0}" = "1" ]; then
    printf 'contaminated launch output\n'
    exit 0
  fi
  session_identity="${AGENT_SESSION_FAKE_TMUX_SESSION_ID:-}"
  [ -n "$session_identity" ] || session_identity='$77'
  pane_identity="${AGENT_SESSION_FAKE_TMUX_PANE_ID:-}"
  [ -n "$pane_identity" ] || pane_identity='%77'
  pane_pid="${AGENT_SESSION_FAKE_TMUX_PANE_PID:-$$}"
  printf '%s\t%s\t%s\n' "$session_identity" "$pane_identity" "$pane_pid"
fi

exit 0
"#,
    );
    (bin, log)
}

fn fake_agent(tmp: &Path, name: &str) -> PathBuf {
    let bin = tmp.join(name);
    write_executable(
        &bin,
        r#"#!/usr/bin/env sh
printf 'fake agent started\n'
sleep 60
"#,
    );
    bin
}

fn unused_loopback_addr() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener.local_addr().expect("local addr")
}

fn http_get(addr: std::net::SocketAddr, path: &str) -> std::io::Result<Value> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or(response.as_str());
    serde_json::from_str(body).map_err(std::io::Error::other)
}

fn wait_for_http_json(addr: std::net::SocketAddr, path: &str, timeout: Duration) -> Value {
    let start = Instant::now();
    let mut last_err = None;
    while start.elapsed() < timeout {
        match http_get(addr, path) {
            Ok(value) => return value,
            Err(err) => {
                last_err = Some(err);
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
    panic!(
        "timed out waiting for {path}: {}",
        last_err
            .map(|err| err.to_string())
            .unwrap_or_else(|| "no attempt".to_string())
    );
}

fn stop_child(child: &mut Child) {
    if child.try_wait().expect("try_wait").is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

struct TestProcessGroup {
    pid: u32,
    child: Arc<Mutex<Child>>,
    stop_reaper: Arc<AtomicBool>,
    reaper: Option<thread::JoinHandle<()>>,
}

impl TestProcessGroup {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn is_running(&self) -> bool {
        self.child
            .lock()
            .expect("test process lock")
            .try_wait()
            .expect("poll test process")
            .is_none()
    }

    fn stop(&mut self) {
        self.stop_reaper.store(true, Ordering::Release);
        let mut child = self.child.lock().expect("test process lock");
        if child.try_wait().expect("poll test process").is_none() {
            let pgid = -(self.pid as libc::pid_t);
            // SAFETY: the still-owned child leads this dedicated test process group.
            unsafe {
                libc::kill(pgid, libc::SIGKILL);
            }
            let _ = child.wait();
        }
        drop(child);
        if let Some(reaper) = self.reaper.take() {
            let _ = reaper.join();
        }
    }
}

impl Drop for TestProcessGroup {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_test_process_group() -> TestProcessGroup {
    let mut command = Command::new("sleep");
    command.arg("30");
    // SAFETY: this test-only child must own a dedicated process session.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let child = command.spawn().expect("spawn test process session");
    let pid = child.id();
    let child = Arc::new(Mutex::new(child));
    let stop_reaper = Arc::new(AtomicBool::new(false));
    let reaper_child = Arc::clone(&child);
    let reaper_stop = Arc::clone(&stop_reaper);
    let reaper = thread::spawn(move || {
        while !reaper_stop.load(Ordering::Acquire) {
            let stopped = reaper_child
                .lock()
                .expect("test process lock")
                .try_wait()
                .expect("poll test process")
                .is_some();
            if stopped {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
    });
    TestProcessGroup {
        pid,
        child,
        stop_reaper,
        reaper: Some(reaper),
    }
}

fn tmux_calls(log: &Path) -> Vec<Vec<String>> {
    let text = fs::read_to_string(log).unwrap_or_default();
    text.split('\u{001e}')
        .filter(|call| !call.is_empty())
        .map(|call| {
            call.split('\0')
                .filter(|arg| !arg.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .collect()
}

#[test]
fn serve_usage_returns_partial_provider_results_from_helpers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).expect("fake bin");
    write_executable(
        &fake_bin.join("codex-cli"),
        r#"#!/usr/bin/env sh
cat <<'JSON'
{"schema_version":"codex-cli.diag.rate-limits.v1","command":"diag rate-limits","mode":"all","ok":true,"results":[{"provider":"codex","name":"auth","target_file":"auth.json","status":"ok","ok":true,"source":"network","windows":[{"label":"Weekly","used_percent":25,"remaining_percent":75,"reset_at_epoch":1780600000}]}]}
JSON
"#,
    );
    write_executable(
        &fake_bin.join("claude-cli"),
        r#"#!/usr/bin/env sh
if [ "${CLAUDE_PROMPT_SEGMENT_CLAUDE_TIMEOUT_SECONDS:-}" != "40" ]; then
  cat <<'JSON'
{"schema_version":"claude-cli.usage.v1","command":"usage","ok":false,"error":{"code":"timeout-not-propagated","message":"missing inner timeout"}}
JSON
  exit 1
fi
cat <<'JSON'
{"schema_version":"claude-cli.usage.v1","command":"usage","ok":false,"error":{"code":"auth-unavailable","message":"missing auth at /Users/terry/.claude/token for user@example.com"}}
JSON
exit 1
"#,
    );

    let (tmux, _tmux_log) = fake_tmux(tmp.path());
    let addr = unused_loopback_addr();
    let mut paths = vec![fake_bin];
    if let Some(current_path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&current_path));
    }
    let path = std::env::join_paths(paths).expect("join PATH");
    let mut child = Command::new(nils_test_support::bin::resolve("agent-session"))
        .arg("serve")
        .arg("--bind")
        .arg(addr.to_string())
        .env("AGENT_SESSION_TMUX_BIN", tmux)
        .env("AGENT_SESSION_USAGE_TIMEOUT_MS", "45000")
        .env_remove("CLAUDE_PROMPT_SEGMENT_CLAUDE_TIMEOUT_SECONDS")
        .env("PATH", path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let payload = wait_for_http_json(addr, "/usage", Duration::from_secs(5));
    stop_child(&mut child);

    assert_eq!(payload["ok"], true);
    let usage = &payload["data"]["usage"];
    assert_eq!(usage["schema_version"], "agent-session.usage.v1");
    assert_eq!(usage["ok"], false);
    let providers = usage["providers"].as_array().expect("providers");
    let codex = providers
        .iter()
        .find(|provider| provider["id"] == "codex")
        .expect("codex provider");
    assert_eq!(codex["ok"], true);
    assert_eq!(codex["source"], "codex-cli");
    assert_eq!(codex["windows"].as_array().expect("windows").len(), 1);
    assert_eq!(codex["windows"][0]["label"], "Weekly");
    assert_eq!(codex["windows"][0]["remaining_percent"], 75);

    let claude = providers
        .iter()
        .find(|provider| provider["id"] == "claude")
        .expect("claude provider");
    assert_eq!(claude["ok"], false);
    assert_eq!(claude["error"]["code"], "auth-unavailable");
    let message = claude["error"]["message"].as_str().expect("message");
    assert!(!message.contains("/Users/terry"));
    assert!(!message.contains("user@example.com"));
}

#[test]
fn serve_usage_preserves_claude_reset_aliases_from_helper() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fake_bin = tmp.path().join("bin");
    fs::create_dir_all(&fake_bin).expect("fake bin");
    write_executable(
        &fake_bin.join("codex-cli"),
        r#"#!/usr/bin/env sh
cat <<'JSON'
{"schema_version":"codex-cli.diag.rate-limits.v1","command":"diag rate-limits","mode":"all","ok":false,"error":{"code":"auth-unavailable","message":"missing codex auth"}}
JSON
exit 1
"#,
    );
    write_executable(
        &fake_bin.join("claude-cli"),
        r#"#!/usr/bin/env sh
if [ "${CLAUDE_PROMPT_SEGMENT_CLAUDE_TIMEOUT_SECONDS:-}" != "9" ]; then
  cat <<'JSON'
{"schema_version":"claude-cli.usage.v1","command":"usage","ok":false,"error":{"code":"timeout-override-lost","message":"explicit inner timeout was overwritten"}}
JSON
  exit 1
fi
cat <<'JSON'
{"schema_version":"claude-cli.usage.v1","command":"usage","ok":true,"result":{"windows":[{"label":"5h","used_percent":3,"remaining_percent":97,"resets_at":"2030-01-01T00:00:00Z"},{"label":"Weekly","used_percent":0,"remaining_percent":100,"resetsAtEpoch":1805000000}]}}
JSON
"#,
    );

    let (tmux, _tmux_log) = fake_tmux(tmp.path());
    let addr = unused_loopback_addr();
    let mut paths = vec![fake_bin];
    if let Some(current_path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&current_path));
    }
    let path = std::env::join_paths(paths).expect("join PATH");
    let mut child = Command::new(nils_test_support::bin::resolve("agent-session"))
        .arg("serve")
        .arg("--bind")
        .arg(addr.to_string())
        .env("AGENT_SESSION_TMUX_BIN", tmux)
        .env("AGENT_SESSION_USAGE_TIMEOUT_MS", "45000")
        .env("CLAUDE_PROMPT_SEGMENT_CLAUDE_TIMEOUT_SECONDS", "9")
        .env("PATH", path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let payload = wait_for_http_json(addr, "/usage", Duration::from_secs(5));
    stop_child(&mut child);

    let usage = &payload["data"]["usage"];
    let providers = usage["providers"].as_array().expect("providers");
    let claude = providers
        .iter()
        .find(|provider| provider["id"] == "claude")
        .expect("claude provider");
    assert_eq!(claude["ok"], true);
    assert_eq!(claude["source"], "claude-cli");
    assert_eq!(claude["windows"][0]["label"], "5h");
    assert_eq!(claude["windows"][0]["reset_at"], "2030-01-01T00:00:00Z");
    assert_eq!(claude["windows"][0]["reset_at_epoch"], 1_893_456_000);
    assert_eq!(claude["windows"][1]["label"], "Weekly");
    assert_eq!(claude["windows"][1]["reset_at_epoch"], 1_805_000_000);
}

fn data(value: &Value) -> &Value {
    assert_eq!(value["ok"], true);
    assert!(
        value.get("command").is_none(),
        "shared envelope must not include top-level command: {value}"
    );
    assert!(
        value.get("result").is_none() && value.get("results").is_none(),
        "shared envelope must use data instead of result/results: {value}"
    );
    &value["data"]
}

fn assert_no_secret(output: &CmdOutput, secret: &str) {
    let combined = format!("{}{}", output.stdout_text(), output.stderr_text());
    assert!(
        !combined.contains(secret),
        "secret leaked into command output: {combined}"
    );
}

#[test]
fn help_includes_version_flag_and_examples() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["--help"], &[]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("-V, --version"),
        "missing version flag: {stdout}"
    );
    assert!(stdout.contains("EXAMPLES:"), "missing examples: {stdout}");
}

#[test]
fn serve_help_describes_activity_stream_authentication() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["serve", "--help"], &[]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("activity streaming, write, and attach endpoints"),
        "serve help omitted the authenticated activity stream: {stdout}"
    );
    assert!(
        stdout.contains("activity streaming, writes, and attach are disabled"),
        "serve help omitted token-unset degradation: {stdout}"
    );
}

#[test]
fn activity_setup_repair_preview_is_codex_only_in_matrix_help_and_completion() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).expect("home dir");
    let home_arg = home.to_string_lossy().to_string();

    for provider in ["claude", "hermes"] {
        let preview = run(
            tmp.path(),
            &[
                "activity",
                "setup",
                "--agent",
                provider,
                "--repair",
                "--dry-run",
                "--format",
                "json",
            ],
            &[("HOME", home_arg.as_str())],
        );
        assert_ne!(preview.code, 0, "provider={provider}");
        assert_eq!(
            preview.stdout_json()["error"]["code"],
            "provider-repair-preview-unsupported",
            "provider={provider}"
        );
        assert!(
            !preview.stdout_text().contains("digest unavailable"),
            "provider={provider}"
        );
    }

    let help = run(tmp.path(), &["activity", "setup", "--help"], &[]);
    assert_eq!(help.code, 0, "stderr={}", help.stderr_text());
    assert!(
        help.stdout_text()
            .contains("Preview a Codex-only repair plan"),
        "help={}",
        help.stdout_text()
    );

    for shell in ["bash", "zsh"] {
        let completion = run(tmp.path(), &["completion", shell], &[]);
        assert_eq!(
            completion.code,
            0,
            "shell={shell} stderr={}",
            completion.stderr_text()
        );
        let script = completion.stdout_text();
        assert!(
            script.contains("--expected-preview-digest"),
            "shell={shell}"
        );
        if shell == "zsh" {
            assert!(
                script.contains("Preview a Codex-only repair plan"),
                "shell={shell}"
            );
        }
    }
}

#[test]
fn standalone_start_keeps_codex_raw_without_a_serve_owned_control_plane() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let runtime_dir = tmp.path().join("run");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(runtime_dir.join("agent-session")).expect("shared runtime dir");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = tmp.path().join("codex-app-server");
    write_executable(
        &codex_bin,
        "#!/usr/bin/env sh\nif [ \"$1\" = --version ]; then printf '%s\\n' 'codex-cli 0.144.1'; exit 0; fi\nif [ \"$1\" = app-server ] && [ \"$2\" = --help ]; then printf '%s\\n' '  --listen <URL>  unix://'; exit 0; fi\nexit 1\n",
    );

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let runtime_arg = runtime_dir.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_CODEX_CAPTURE_TIMEOUT_MS", "10"),
            ("XDG_RUNTIME_DIR", &runtime_arg),
            ("CODEX_HOME", &codex_home_arg),
        ],
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let id = data(&output.stdout_json())["id"]
        .as_str()
        .expect("session id")
        .to_string();
    let record: Value = serde_json::from_str(
        &fs::read_to_string(state_dir.join("sessions").join(id).join("session.json"))
            .expect("session record"),
    )
    .expect("record json");
    assert_eq!(record["runtime"]["kind"], "tmux");
    assert!(record["runtime"].get("codex_app_server_protocol").is_none());
    let launch = tmux_calls(&tmux_log)
        .into_iter()
        .find(|call| call.first().is_some_and(|arg| arg == "new-session"))
        .expect("new-session call");
    assert!(launch.iter().any(|arg| arg == &codex_arg));
    assert!(!launch.iter().any(|arg| arg.contains("app-server --listen")));
}

#[test]
fn activity_events_are_runtime_bound_private_and_deterministic() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let envs = [("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str())];
    let start = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &envs,
    );
    assert_eq!(start.code, 0, "stderr={}", start.stderr_text());
    let start_json = start.stdout_json();
    let start_data = data(&start_json);
    let id = start_data["id"].as_str().expect("session id");
    assert_eq!(
        start_data["turn_state"]["schema_version"],
        "agent-session.turn-state.v1"
    );
    assert_eq!(start_data["turn_state"]["phase"], "starting");
    assert!(start_data["runtime_started_at"].is_string());

    let session_dir = state_dir.join("sessions").join(id);
    let record: Value = serde_json::from_str(
        &fs::read_to_string(session_dir.join("session.json")).expect("session record"),
    )
    .expect("session json");
    let runtime_id = record["runtime"]["launch_id"]
        .as_str()
        .expect("runtime launch id");
    let new_session = tmux_calls(&tmux_log)
        .into_iter()
        .find(|call| call.first().is_some_and(|arg| arg == "new-session"))
        .expect("new-session call");
    assert!(
        new_session
            .windows(2)
            .any(|pair| pair == ["-e", &format!("AGENT_SESSION_ID={id}")])
    );
    assert!(new_session.windows(2).any(|pair| {
        pair == [
            "-e",
            &format!("AGENT_SESSION_STATE_DIR={}", state_dir.display()),
        ]
    }));
    assert!(
        new_session
            .windows(2)
            .any(|pair| { pair == ["-e", &format!("AGENT_SESSION_RUNTIME_ID={runtime_id}")] })
    );

    let event = |event_id: &str,
                 kind: &str,
                 turn_id: Option<&str>,
                 attention_id: Option<&str>,
                 runtime: &str| {
        let mut value = json!({
            "schema_version": "agent-session.turn-event.v1",
            "event_id": event_id,
            "runtime_id": runtime,
            "provider": "codex",
            "provider_session_id": "provider-session",
            "kind": kind,
            "confidence": "observed"
        });
        if let Some(turn_id) = turn_id {
            value["provider_turn_id"] = json!(turn_id);
        }
        if let Some(attention_id) = attention_id {
            value["attention_id"] = json!(attention_id);
            value["attention_kind"] = json!("approval");
        }
        value.to_string()
    };
    let submit = |payload: &str| {
        run_with_stdin(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "activity",
                "event",
                id,
                "--stdin",
                "--format",
                "json",
            ],
            &[],
            payload,
        )
    };

    let started = submit(&event(
        "evt-start",
        "turn_started",
        Some("turn-1"),
        None,
        runtime_id,
    ));
    assert_eq!(started.code, 0, "stderr={}", started.stderr_text());
    assert_eq!(
        data(&started.stdout_json())["turn_state"]["phase"],
        "working"
    );

    let attention = submit(&event(
        "evt-attention",
        "attention_requested",
        Some("turn-1"),
        Some("approval-1"),
        runtime_id,
    ));
    assert_eq!(attention.code, 0, "stderr={}", attention.stderr_text());
    let attention_json = attention.stdout_json();
    let attention_data = data(&attention_json);
    assert_eq!(attention_data["turn_state"]["phase"], "needs_input");
    assert_eq!(
        attention_data["turn_state"]["current_turn"]["attention"]["pending_count"],
        1
    );
    let attention_revision = attention_data["turn_state"]["revision"]
        .as_u64()
        .expect("attention revision");

    let unrelated = submit(&event(
        "evt-progress",
        "progress",
        Some("turn-1"),
        None,
        runtime_id,
    ));
    assert_eq!(unrelated.code, 0, "stderr={}", unrelated.stderr_text());
    assert_eq!(
        data(&unrelated.stdout_json())["turn_state"]["phase"],
        "needs_input",
        "uncorrelated progress must not clear attention"
    );

    let stale = submit(&event(
        "evt-stale",
        "turn_completed",
        Some("turn-1"),
        None,
        "prior-runtime",
    ));
    assert_ne!(stale.code, 0);
    assert_eq!(stale.stdout_json()["error"]["code"], "runtime-id-mismatch");

    let completed_payload = event(
        "evt-complete",
        "turn_completed",
        Some("turn-1"),
        None,
        runtime_id,
    );
    let completed = submit(&completed_payload);
    assert_eq!(completed.code, 0, "stderr={}", completed.stderr_text());
    let completed_json = completed.stdout_json();
    let completed_data = data(&completed_json);
    assert_eq!(completed_data["turn_state"]["phase"], "waiting");
    assert_eq!(
        completed_data["turn_state"]["last_turn"]["outcome"],
        "completed"
    );
    assert!(
        completed_data["turn_state"]["revision"]
            .as_u64()
            .expect("completed revision")
            > attention_revision
    );
    let completed_revision = completed_data["turn_state"]["revision"].clone();

    let duplicate = submit(&completed_payload);
    assert_eq!(duplicate.code, 0, "stderr={}", duplicate.stderr_text());
    assert_eq!(
        data(&duplicate.stdout_json())["turn_state"]["revision"],
        completed_revision,
        "duplicate events must be idempotent"
    );

    for file in ["activity.json", "activity.journal.jsonl"] {
        let path = session_dir.join(file);
        assert_eq!(
            fs::metadata(&path)
                .expect("activity metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let contents = fs::read_to_string(path).expect("activity file");
        assert!(!contents.contains("prompt"));
        assert!(!contents.contains("tool_input"));
        assert!(!contents.contains("transcript"));
    }
    let replay_path = session_dir.join("activity.replay.bin");
    assert_eq!(
        fs::metadata(&replay_path)
            .expect("replay metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(replay_path).expect("replay size").len(),
        64 + 4096 * 2 * 32
    );
}

#[test]
fn hermes_identical_approval_hooks_preserve_persisted_multiplicity_until_completion() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let hermes_bin = fake_agent(tmp.path(), "hermes");
    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let hermes_arg = hermes_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let start = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "hermes",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &hermes_arg,
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str())],
    );
    assert_eq!(start.code, 0, "stderr={}", start.stderr_text());
    let id = data(&start.stdout_json())["id"]
        .as_str()
        .expect("session id")
        .to_string();
    let session_dir = state_dir.join("sessions").join(&id);
    let record: Value = serde_json::from_str(
        &fs::read_to_string(session_dir.join("session.json")).expect("session record"),
    )
    .expect("session json");
    let runtime_id = record["runtime"]["launch_id"]
        .as_str()
        .expect("runtime id")
        .to_string();
    let hook_env = [
        ("AGENT_SESSION_ID", id.as_str()),
        ("AGENT_SESSION_RUNTIME_ID", runtime_id.as_str()),
        ("AGENT_SESSION_STATE_DIR", state_arg.as_str()),
    ];
    let hook = |payload: &Value| {
        run_with_stdin(
            tmp.path(),
            &["activity", "hook", "--agent", "hermes"],
            &hook_env,
            &payload.to_string(),
        )
    };
    let status = || {
        run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "activity",
                "status",
                &id,
                "--format",
                "json",
            ],
            &[],
        )
    };

    let malformed = run_with_stdin(
        tmp.path(),
        &["activity", "hook", "--agent", "hermes"],
        &hook_env,
        "{invalid-hook",
    );
    assert_eq!(malformed.code, 0, "hook telemetry is fail-open");
    let diagnostic_path = session_dir.join("activity.diagnostic.json");
    let diagnostic = fs::read_to_string(&diagnostic_path).expect("hook diagnostic");
    assert!(diagnostic.contains("provider-hook-invalid"));

    let raw_values = [
        "raw-command-never-persist",
        "raw-description-never-persist",
        "raw-pattern-never-persist",
        "raw-secondary-pattern-never-persist",
        "raw-session-never-persist",
        "raw-surface-never-persist",
    ];
    let request = json!({
        "event": "pre_approval_request",
        "command": raw_values[0],
        "description": raw_values[1],
        "pattern_key": raw_values[2],
        "pattern_keys": [raw_values[3], raw_values[2]],
        "session_key": raw_values[4],
        "surface": raw_values[5]
    });
    let first = hook(&request);
    assert_eq!(first.code, 0, "stderr={}", first.stderr_text());
    assert!(first.stdout_text().is_empty());
    assert!(first.stderr_text().is_empty());
    assert!(
        !diagnostic_path.exists(),
        "successful ingestion clears diagnostic"
    );
    let first_status = status();
    assert_eq!(
        first_status.code,
        0,
        "stderr={}",
        first_status.stderr_text()
    );
    let first_status_json = first_status.stdout_json();
    let first_state = &data(&first_status_json)["turn_state"];
    assert_eq!(first_state["phase"], "needs_input");
    assert_eq!(first_state["current_turn"]["attention"]["pending_count"], 1);
    let first_revision = first_state["revision"].as_u64().expect("first revision");

    let second = hook(&request);
    assert_eq!(second.code, 0, "stderr={}", second.stderr_text());
    let second_status = status();
    let second_status_json = second_status.stdout_json();
    let second_state = &data(&second_status_json)["turn_state"];
    assert_eq!(second_state["phase"], "needs_input");
    assert_eq!(
        second_state["current_turn"]["attention"]["pending_count"],
        2
    );
    let second_revision = second_state["revision"].as_u64().expect("second revision");
    assert!(second_revision > first_revision);

    let mut response = request.clone();
    response["event"] = json!("post_approval_response");
    response["choice"] = json!("once");
    let post = hook(&response);
    assert_eq!(post.code, 0, "stderr={}", post.stderr_text());
    let post_status = status();
    let post_status_json = post_status.stdout_json();
    let post_state = &data(&post_status_json)["turn_state"];
    assert_eq!(post_state["phase"], "needs_input");
    assert_eq!(post_state["current_turn"]["attention"]["pending_count"], 1);
    let post_revision = post_state["revision"].as_u64().expect("post revision");
    assert!(post_revision > second_revision);

    let completion = hook(&json!({
        "event": "post_llm_call",
        "session_id": raw_values[4],
        "platform": raw_values[5]
    }));
    assert_eq!(completion.code, 0, "stderr={}", completion.stderr_text());
    let completed_status = status();
    let completed_status_json = completed_status.stdout_json();
    let completed_state = &data(&completed_status_json)["turn_state"];
    assert_eq!(completed_state["phase"], "waiting");
    assert!(completed_state["current_turn"].is_null());
    assert_eq!(completed_state["last_turn"]["outcome"], "completed");
    assert!(
        completed_state["revision"]
            .as_u64()
            .expect("completion revision")
            > post_revision
    );

    let journal =
        fs::read_to_string(session_dir.join("activity.journal.jsonl")).expect("activity journal");
    assert_eq!(journal.matches("attention_requested").count(), 2);
    assert_eq!(journal.matches("attention_cleared").count(), 1);
    assert_eq!(journal.matches("turn_completed").count(), 1);
    let snapshot =
        fs::read_to_string(session_dir.join("activity.json")).expect("activity snapshot");
    for persisted in [snapshot.as_str(), journal.as_str()] {
        for field in [
            "\"command\"",
            "\"description\"",
            "\"pattern_key\"",
            "\"pattern_keys\"",
            "\"session_key\"",
            "\"surface\"",
        ] {
            assert!(
                !persisted.contains(field),
                "raw tuple field persisted: {field}"
            );
        }
        for raw in raw_values {
            assert!(!persisted.contains(raw), "raw tuple value persisted: {raw}");
        }
    }
    assert!(!diagnostic_path.exists());
    assert!(session_dir.join("activity.replay.bin").is_file());
}

#[test]
fn hermes_shell_wire_approvals_use_exact_ids_and_compatibility_fallback() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let hermes_bin = fake_agent(tmp.path(), "hermes");
    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let hermes_arg = hermes_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let start = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "hermes",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &hermes_arg,
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str())],
    );
    assert_eq!(start.code, 0, "stderr={}", start.stderr_text());
    let id = data(&start.stdout_json())["id"]
        .as_str()
        .expect("session id")
        .to_string();
    let session_dir = state_dir.join("sessions").join(&id);
    let record: Value = serde_json::from_str(
        &fs::read_to_string(session_dir.join("session.json")).expect("session record"),
    )
    .expect("session json");
    let runtime_id = record["runtime"]["launch_id"]
        .as_str()
        .expect("runtime id")
        .to_string();
    let hook_env = [
        ("AGENT_SESSION_ID", id.as_str()),
        ("AGENT_SESSION_RUNTIME_ID", runtime_id.as_str()),
        ("AGENT_SESSION_STATE_DIR", state_arg.as_str()),
    ];
    let hook = |payload: &Value| {
        run_with_stdin(
            tmp.path(),
            &["activity", "hook", "--agent", "hermes"],
            &hook_env,
            &payload.to_string(),
        )
    };
    let state = || {
        let status = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "activity",
                "status",
                &id,
                "--format",
                "json",
            ],
            &[],
        );
        assert_eq!(status.code, 0, "stderr={}", status.stderr_text());
        data(&status.stdout_json())["turn_state"].clone()
    };
    let fixture = include_str!("../fixtures/activity/hermes-shell-approval-events.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("Hermes shell-wire fixture"))
        .collect::<Vec<_>>();
    assert_eq!(fixture.len(), 7);

    let malformed = run_with_stdin(
        tmp.path(),
        &["activity", "hook", "--agent", "hermes"],
        &hook_env,
        "{invalid-hook",
    );
    assert_eq!(malformed.code, 0, "hook telemetry is fail-open");
    let diagnostic_path = session_dir.join("activity.diagnostic.json");
    let diagnostic = fs::read_to_string(&diagnostic_path).expect("hook diagnostic");
    assert!(diagnostic.contains("provider-hook-invalid"));

    let first = hook(&fixture[0]);
    assert_eq!(first.code, 0, "stderr={}", first.stderr_text());
    assert!(first.stdout_text().is_empty());
    assert!(first.stderr_text().is_empty());
    assert!(!diagnostic_path.exists());
    let first_state = state();
    assert_eq!(first_state["phase"], "needs_input");
    assert_eq!(first_state["current_turn"]["attention"]["pending_count"], 1);
    let first_revision = first_state["revision"].as_u64().expect("first revision");

    let first_replay = hook(&fixture[0]);
    assert_eq!(first_replay.code, 0);
    let replay_state = state();
    assert_eq!(replay_state["revision"], first_revision);
    assert_eq!(
        replay_state["current_turn"]["attention"]["pending_count"],
        1
    );

    let second = hook(&fixture[1]);
    assert_eq!(second.code, 0, "stderr={}", second.stderr_text());
    let second_state = state();
    assert_eq!(second_state["phase"], "needs_input");
    assert_eq!(
        second_state["current_turn"]["attention"]["pending_count"],
        2
    );
    let second_revision = second_state["revision"].as_u64().expect("second revision");
    assert!(second_revision > first_revision);

    let journal_path = session_dir.join("activity.journal.jsonl");
    let before_interleaved_replay =
        fs::read_to_string(&journal_path).expect("journal before interleaved replay");
    let interleaved_replay = hook(&fixture[0]);
    assert_eq!(interleaved_replay.code, 0);
    let interleaved_replay_state = state();
    assert_eq!(interleaved_replay_state["revision"], second_revision);
    assert_eq!(
        interleaved_replay_state["current_turn"]["attention"]["pending_count"],
        2
    );
    assert_eq!(
        fs::read_to_string(&journal_path).expect("journal after interleaved replay"),
        before_interleaved_replay
    );

    thread::sleep(Duration::from_millis(1_100));
    let delayed_replay = hook(&fixture[0]);
    assert_eq!(delayed_replay.code, 0);
    let delayed_replay_state = state();
    assert_eq!(delayed_replay_state["revision"], second_revision);
    assert_eq!(
        delayed_replay_state["current_turn"]["attention"]["pending_count"],
        2
    );
    assert_eq!(
        fs::read_to_string(&journal_path).expect("journal after delayed replay"),
        before_interleaved_replay
    );

    let post_b = hook(&fixture[2]);
    assert_eq!(post_b.code, 0, "stderr={}", post_b.stderr_text());
    let post_b_state = state();
    assert_eq!(post_b_state["phase"], "needs_input");
    assert_eq!(
        post_b_state["current_turn"]["attention"]["pending_count"],
        1
    );
    let post_b_revision = post_b_state["revision"].as_u64().expect("post B revision");
    assert!(post_b_revision > second_revision);

    let post_b_replay = hook(&fixture[2]);
    assert_eq!(post_b_replay.code, 0);
    let post_b_replay_state = state();
    assert_eq!(post_b_replay_state["revision"], post_b_revision);
    assert_eq!(
        post_b_replay_state["current_turn"]["attention"]["pending_count"],
        1
    );

    let fallback_pre = hook(&fixture[4]);
    assert_eq!(
        fallback_pre.code,
        0,
        "stderr={}",
        fallback_pre.stderr_text()
    );
    let fallback_pre_state = state();
    assert_eq!(fallback_pre_state["phase"], "needs_input");
    assert_eq!(
        fallback_pre_state["current_turn"]["attention"]["pending_count"],
        2
    );

    let fallback_post = hook(&fixture[5]);
    assert_eq!(
        fallback_post.code,
        0,
        "stderr={}",
        fallback_post.stderr_text()
    );
    let fallback_post_state = state();
    assert_eq!(fallback_post_state["phase"], "needs_input");
    assert_eq!(
        fallback_post_state["current_turn"]["attention"]["pending_count"],
        1
    );

    let post_a = hook(&fixture[3]);
    assert_eq!(post_a.code, 0, "stderr={}", post_a.stderr_text());
    let post_a_state = state();
    assert_eq!(post_a_state["phase"], "working");
    assert!(post_a_state["current_turn"]["attention"].is_null());
    let cleared_revision = post_a_state["revision"].as_u64().expect("cleared revision");

    let before_cleared_pre_replay =
        fs::read_to_string(&journal_path).expect("journal before cleared pre replay");
    let cleared_pre_replay = hook(&fixture[0]);
    assert_eq!(cleared_pre_replay.code, 0);
    let cleared_pre_replay_state = state();
    assert_eq!(cleared_pre_replay_state["phase"], "working");
    assert!(cleared_pre_replay_state["current_turn"]["attention"].is_null());
    assert_eq!(cleared_pre_replay_state["revision"], cleared_revision);
    assert_eq!(
        fs::read_to_string(&journal_path).expect("journal after cleared pre replay"),
        before_cleared_pre_replay
    );

    let fallback_pending = hook(&fixture[4]);
    assert_eq!(fallback_pending.code, 0);
    let pending_state = state();
    assert_eq!(pending_state["phase"], "needs_input");
    assert_eq!(
        pending_state["current_turn"]["attention"]["pending_count"],
        1
    );
    let pending_revision = pending_state["revision"]
        .as_u64()
        .expect("pending revision");
    assert!(pending_revision > cleared_revision);

    let stale_env = [
        ("AGENT_SESSION_ID", id.as_str()),
        ("AGENT_SESSION_RUNTIME_ID", "prior-runtime"),
        ("AGENT_SESSION_STATE_DIR", state_arg.as_str()),
    ];
    let stale = run_with_stdin(
        tmp.path(),
        &["activity", "hook", "--agent", "hermes"],
        &stale_env,
        &fixture[1].to_string(),
    );
    assert_eq!(stale.code, 0, "stale hook telemetry is fail-open");
    let after_stale_state = state();
    assert_eq!(after_stale_state["revision"], pending_revision);
    assert_eq!(
        after_stale_state["current_turn"]["attention"]["pending_count"],
        1
    );

    let mut invalid_tool_call = fixture[0].clone();
    invalid_tool_call["extra"]["tool_call_id"] = json!({"raw": "must-not-persist"});
    let invalid = hook(&invalid_tool_call);
    assert_eq!(invalid.code, 0, "invalid hook telemetry is fail-open");
    let diagnostic = fs::read_to_string(&diagnostic_path).expect("invalid-id diagnostic");
    assert!(diagnostic.contains("provider-hook-correlation-invalid"));
    for forbidden in [
        "must-not-persist",
        "tool_call_id",
        "raw-command-never-persist",
    ] {
        assert!(!diagnostic.contains(forbidden));
    }
    assert_eq!(state()["revision"], pending_revision);

    let completion = hook(&fixture[6]);
    assert_eq!(completion.code, 0, "stderr={}", completion.stderr_text());
    assert!(!diagnostic_path.exists());
    let completed_state = state();
    assert_eq!(completed_state["phase"], "waiting");
    assert!(completed_state["current_turn"].is_null());
    assert_eq!(completed_state["last_turn"]["outcome"], "completed");
    assert!(
        completed_state["revision"]
            .as_u64()
            .expect("completion revision")
            > pending_revision
    );

    let journal =
        fs::read_to_string(session_dir.join("activity.journal.jsonl")).expect("activity journal");
    assert_eq!(journal.matches("attention_requested").count(), 4);
    assert_eq!(journal.matches("attention_cleared").count(), 3);
    assert_eq!(journal.matches("turn_completed").count(), 1);
    let journal_entries = journal
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("journal entry"))
        .collect::<Vec<_>>();
    let requested_ids = journal_entries
        .iter()
        .filter(|entry| entry["event"]["kind"] == "attention_requested")
        .map(|entry| {
            entry["event"]["attention_id"]
                .as_str()
                .expect("attention id")
        })
        .collect::<Vec<_>>();
    assert_eq!(requested_ids.len(), 4);
    assert_ne!(requested_ids[0], requested_ids[1]);
    assert_ne!(requested_ids[0], requested_ids[2]);
    assert_eq!(requested_ids[2], requested_ids[3]);

    let snapshot =
        fs::read_to_string(session_dir.join("activity.json")).expect("activity snapshot");
    let raw_values = [
        "raw-command-never-persist",
        "raw-description-never-persist",
        "raw-pattern-never-persist",
        "raw-secondary-pattern-never-persist",
        "raw-session-never-persist",
        "raw-surface-never-persist",
        "raw-turn-never-persist",
        "raw-tool-call-a-never-persist",
        "raw-tool-call-b-never-persist",
        "raw-fallback-command-never-persist",
        "raw-fallback-description-never-persist",
        "raw-fallback-pattern-never-persist",
        "raw-fallback-secondary-never-persist",
        "/raw/cwd-never-persist",
    ];
    for persisted in [snapshot.as_str(), journal.as_str()] {
        for field in [
            "\"extra\"",
            "\"command\"",
            "\"description\"",
            "\"pattern_key\"",
            "\"pattern_keys\"",
            "\"session_key\"",
            "\"surface\"",
            "\"tool_call_id\"",
            "\"cwd\"",
        ] {
            assert!(
                !persisted.contains(field),
                "raw shell field persisted: {field}"
            );
        }
        for raw in raw_values {
            assert!(!persisted.contains(raw), "raw shell value persisted: {raw}");
        }
    }
    assert!(session_dir.join("activity.replay.bin").is_file());
}

#[test]
fn activity_setup_is_dry_run_first_additive_idempotent_and_reversible() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex dir");
    fs::create_dir_all(home.join(".claude")).expect("claude dir");
    fs::create_dir_all(home.join(".hermes")).expect("hermes dir");
    let codex_path = home.join(".codex/hooks.json");
    let codex_notify_path = home.join(".codex/config.toml");
    let claude_path = home.join(".claude/settings.json");
    let hermes_path = home.join(".hermes/config.yaml");
    fs::write(
        &codex_path,
        r#"{"keep":"codex","hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep-codex-hook"}]}]}}"#,
    )
    .expect("codex config");
    fs::write(
        &codex_notify_path,
        "model = \"gpt-test\"\n\n[features]\nkeep = true\n",
    )
    .expect("Codex notify config");
    fs::write(
        &claude_path,
        r#"{"keep":"claude","hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep-claude-hook"}]}]}}"#,
    )
    .expect("claude config");
    fs::write(
        &hermes_path,
        "keep: hermes\nhooks:\n  post_llm_call:\n    - command: keep-hermes-hook\n",
    )
    .expect("hermes config");
    let home_arg = home.to_string_lossy().to_string();
    let envs = [("HOME", home_arg.as_str())];

    for (agent, path, keep) in [
        ("codex", codex_path.as_path(), "keep-codex-hook"),
        ("claude", claude_path.as_path(), "keep-claude-hook"),
        ("hermes", hermes_path.as_path(), "keep-hermes-hook"),
    ] {
        let before = fs::read(path).expect("before config");
        let notify_before = (agent == "codex")
            .then(|| fs::read(&codex_notify_path).expect("before Codex notify config"));
        let dry_run = run(
            tmp.path(),
            &[
                "activity",
                "setup",
                "--agent",
                agent,
                "--dry-run",
                "--format",
                "json",
            ],
            &envs,
        );
        assert_eq!(
            dry_run.code,
            0,
            "agent={agent} stderr={}",
            dry_run.stderr_text()
        );
        let dry_run_json = dry_run.stdout_json();
        assert_eq!(data(&dry_run_json)["changed"], false);
        assert_eq!(data(&dry_run_json)["would_change"], true);
        assert_eq!(data(&dry_run_json)["configured"], false);
        assert_eq!(data(&dry_run_json)["would_configure"], true);
        assert_eq!(fs::read(path).expect("dry-run config"), before);
        if let Some(notify_before) = notify_before.as_ref() {
            assert_eq!(
                fs::read(&codex_notify_path).expect("dry-run Codex notify config"),
                *notify_before
            );
        }

        let dry_run_text = run(
            tmp.path(),
            &["activity", "setup", "--agent", agent, "--dry-run"],
            &envs,
        );
        assert_eq!(dry_run_text.code, 0);
        assert!(dry_run_text.stdout_text().contains("configured now: no"));
        assert!(dry_run_text.stdout_text().contains("would configure: yes"));

        let apply = run(
            tmp.path(),
            &[
                "activity", "setup", "--agent", agent, "--apply", "--format", "json",
            ],
            &envs,
        );
        assert_eq!(
            apply.code,
            0,
            "agent={agent} stderr={}",
            apply.stderr_text()
        );
        let apply_json = apply.stdout_json();
        assert_eq!(data(&apply_json)["changed"], true);
        assert_eq!(data(&apply_json)["would_change"], true);
        assert_eq!(data(&apply_json)["configured"], true);
        assert_eq!(data(&apply_json)["would_configure"], true);
        let applied = fs::read(path).expect("applied config");
        assert!(String::from_utf8_lossy(&applied).contains(keep));
        let applied_notify = (agent == "codex").then(|| {
            let contents =
                fs::read_to_string(&codex_notify_path).expect("applied Codex notify config");
            assert!(contents.contains(
                "notify = [\"agent-session\", \"activity\", \"notify\", \"--agent\", \"codex\"]"
            ));
            assert!(contents.contains("model = \"gpt-test\""));
            assert!(contents.contains("[features]"));
            assert!(contents.contains("keep = true"));
            contents
        });

        let second = run(
            tmp.path(),
            &[
                "activity", "setup", "--agent", agent, "--apply", "--format", "json",
            ],
            &envs,
        );
        assert_eq!(
            second.code,
            0,
            "agent={agent} stderr={}",
            second.stderr_text()
        );
        assert_eq!(data(&second.stdout_json())["changed"], false);
        assert_eq!(data(&second.stdout_json())["would_change"], false);
        assert_eq!(data(&second.stdout_json())["configured"], true);
        assert_eq!(data(&second.stdout_json())["would_configure"], true);
        assert_eq!(fs::read(path).expect("second config"), applied);
        if let Some(applied_notify) = applied_notify.as_ref() {
            assert_eq!(
                fs::read_to_string(&codex_notify_path).expect("second Codex notify config"),
                *applied_notify
            );
        }

        let remove = run(
            tmp.path(),
            &[
                "activity", "setup", "--agent", agent, "--remove", "--format", "json",
            ],
            &envs,
        );
        assert_eq!(
            remove.code,
            0,
            "agent={agent} stderr={}",
            remove.stderr_text()
        );
        let remove_json = remove.stdout_json();
        assert_eq!(data(&remove_json)["changed"], true);
        assert_eq!(data(&remove_json)["would_change"], true);
        assert_eq!(data(&remove_json)["configured"], false);
        assert_eq!(data(&remove_json)["would_configure"], false);
        let removed = fs::read_to_string(path).expect("removed config");
        assert!(removed.contains(keep));
        assert!(!removed.contains("agent-session activity hook"));
        if agent == "codex" {
            let removed_notify_bytes =
                fs::read(&codex_notify_path).expect("removed Codex notify config");
            assert_eq!(
                removed_notify_bytes,
                *notify_before
                    .as_ref()
                    .expect("original Codex notify config"),
                "owned notify apply/remove must preserve the original config bytes"
            );
            let removed_notify = String::from_utf8(removed_notify_bytes).expect("UTF-8 config");
            assert!(!removed_notify.contains("notify ="));
            assert!(removed_notify.contains("model = \"gpt-test\""));
            assert!(removed_notify.contains("[features]"));
            assert!(removed_notify.contains("keep = true"));
        }
    }
}

#[test]
fn codex_activity_setup_composes_and_restores_a_user_owned_notify() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex dir");
    let hooks_path = home.join(".codex/hooks.json");
    let notify_path = home.join(".codex/config.toml");
    fs::write(&hooks_path, r#"{"keep":"hooks"}"#).expect("hooks config");
    fs::write(
        &notify_path,
        "model = \"gpt-test\"\nnotify = [\"user-notifier\", \"--keep\"]\n",
    )
    .expect("notify config");
    let hooks_before = fs::read(&hooks_path).expect("hooks before");
    let notify_before = fs::read(&notify_path).expect("notify before");
    let home_arg = home.to_string_lossy().to_string();
    let result = run(
        tmp.path(),
        &[
            "activity", "setup", "--agent", "codex", "--apply", "--format", "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_eq!(result.code, 0, "stderr={}", result.stderr_text());
    assert_ne!(fs::read(&hooks_path).expect("hooks after"), hooks_before);
    let composed = fs::read_to_string(&notify_path).expect("composed notify");
    assert!(composed.contains("agent-session"));
    assert!(composed.contains("--forward-notify-argv-json"));
    assert!(composed.contains("user-notifier"));
    assert!(composed.contains("--keep"));

    let second = run(
        tmp.path(),
        &[
            "activity", "setup", "--agent", "codex", "--apply", "--format", "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_eq!(second.code, 0, "stderr={}", second.stderr_text());
    assert_eq!(data(&second.stdout_json())["changed"], false);
    let doctor = run(
        tmp.path(),
        &["activity", "doctor", "--agent", "codex", "--format", "json"],
        &[("HOME", home_arg.as_str())],
    );
    assert_eq!(doctor.code, 0, "stderr={}", doctor.stderr_text());
    let doctor_json = doctor.stdout_json();
    let provider = &data(&doctor_json)["providers"][0];
    assert_eq!(provider["configured"], true);
    assert_eq!(provider["notification_mode"], "composed");

    let removed = run(
        tmp.path(),
        &[
            "activity", "setup", "--agent", "codex", "--remove", "--format", "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_eq!(removed.code, 0, "stderr={}", removed.stderr_text());
    assert_eq!(
        fs::read(&notify_path).expect("restored notify"),
        notify_before
    );
}

#[test]
fn codex_activity_setup_refuses_composition_that_cannot_restore_exact_bytes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex dir");
    let hooks_path = home.join(".codex/hooks.json");
    let notify_path = home.join(".codex/config.toml");
    fs::write(&hooks_path, r#"{"keep":"hooks"}"#).expect("hooks config");
    fs::write(
        &notify_path,
        "# decorated user setting\nnotify = [\n  \"user-notifier\", # retain this comment\n  \"--keep\",\n]\n",
    )
    .expect("notify config");
    let hooks_before = fs::read(&hooks_path).expect("hooks before");
    let notify_before = fs::read(&notify_path).expect("notify before");
    let home_arg = home.to_string_lossy().to_string();
    let result = run(
        tmp.path(),
        &[
            "activity", "setup", "--agent", "codex", "--apply", "--format", "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_ne!(result.code, 0);
    assert_eq!(
        result.stdout_json()["error"]["code"],
        "provider-notification-config-conflict"
    );
    assert_eq!(fs::read(&hooks_path).expect("hooks after"), hooks_before);
    assert_eq!(fs::read(&notify_path).expect("notify after"), notify_before);
}

#[test]
fn codex_activity_repair_preview_reports_safe_nonreversible_notifier_by_hash() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex dir");
    let hooks_path = home.join(".codex/hooks.json");
    let notify_path = home.join(".codex/config.toml");
    fs::write(&hooks_path, r#"{"keep":"hooks"}"#).expect("hooks config");
    fs::write(
        &notify_path,
        "# desktop rewrite\nnotify = [\n  \"user-notifier\",\n  \"--title\",\n  \"sensitive-title-must-not-persist\",\n  \"--message\",\n]\n",
    )
    .expect("notify config");
    let hooks_before = fs::read(&hooks_path).expect("hooks before");
    let notify_before = fs::read(&notify_path).expect("notify before");
    let home_arg = home.to_string_lossy().to_string();

    let preview = run(
        tmp.path(),
        &[
            "activity",
            "setup",
            "--agent",
            "codex",
            "--repair",
            "--dry-run",
            "--format",
            "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    let preview = preview.stdout_json();
    let result = data(&preview);
    assert_eq!(result["action"], "repair-preview");
    assert_eq!(result["changed"], false);
    assert_eq!(result["configured"], false);
    assert_eq!(result["apply_allowed"], false);
    assert_eq!(result["notification_preview"]["current_mode"], "conflict");
    assert_eq!(result["notification_preview"]["candidate_mode"], "composed");
    assert_eq!(result["notification_preview"]["forwarded_argc"], 4);
    assert_eq!(result["notification_preview"]["reversible"], false);
    assert_eq!(
        result["notification_preview"]["blocker_code"],
        "provider-notification-config-nonreversible"
    );
    assert_eq!(
        result["notification_preview"]["forwarded_argv_sha256"],
        "sha256:1396340af9627c539dea8d0fff93faa8e06f84c38cadeebad406d4100ef6ad55",
        "preview hash must match compact JSON argv plus one LF"
    );
    let serialized = serde_json::to_string(&preview).expect("preview json");
    for forbidden in [
        "user-notifier",
        "--title",
        "sensitive-title-must-not-persist",
        "--message",
    ] {
        assert!(!serialized.contains(forbidden), "forbidden {forbidden}");
    }
    assert_eq!(fs::read(&hooks_path).expect("hooks after"), hooks_before);
    assert_eq!(fs::read(&notify_path).expect("notify after"), notify_before);
    let preview_digest = result["preview_digest"]
        .as_str()
        .expect("preview digest")
        .to_string();

    let repair = run(
        tmp.path(),
        &[
            "activity",
            "setup",
            "--agent",
            "codex",
            "--repair",
            "--expected-preview-digest",
            &preview_digest,
            "--format",
            "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_ne!(repair.code, 0);
    assert_eq!(
        repair.stdout_json()["error"]["code"],
        "provider-notification-config-conflict"
    );
    assert_eq!(fs::read(&hooks_path).expect("hooks after"), hooks_before);
    assert_eq!(fs::read(&notify_path).expect("notify after"), notify_before);
}

#[test]
fn codex_activity_repair_requires_a_preview_digest() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex dir");
    let hooks_path = home.join(".codex/hooks.json");
    let notify_path = home.join(".codex/config.toml");
    fs::write(&hooks_path, r#"{"keep":"reviewed-marker-A"}"#).expect("hooks config");
    fs::write(&notify_path, "notify = [\"user-notifier\", \"--keep\"]\n").expect("notify config");
    let hooks_before = fs::read(&hooks_path).expect("hooks before");
    let notify_before = fs::read(&notify_path).expect("notify before");
    let home_arg = home.to_string_lossy().to_string();

    let repair = run(
        tmp.path(),
        &[
            "activity", "setup", "--agent", "codex", "--repair", "--format", "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_ne!(repair.code, 0);
    assert_eq!(
        repair.stdout_json()["error"]["code"],
        "provider-config-preview-digest-required"
    );
    assert_eq!(fs::read(&hooks_path).expect("hooks after"), hooks_before);
    assert_eq!(fs::read(&notify_path).expect("notify after"), notify_before);
}

#[test]
fn codex_activity_repair_rejects_stale_preview_digest_without_writes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex dir");
    let hooks_path = home.join(".codex/hooks.json");
    let notify_path = home.join(".codex/config.toml");
    fs::write(&hooks_path, r#"{"keep":"reviewed-marker-A"}"#).expect("hooks config");
    fs::write(&notify_path, "notify = [\"user-notifier\", \"--keep\"]\n").expect("notify config");
    let home_arg = home.to_string_lossy().to_string();

    let preview = run(
        tmp.path(),
        &[
            "activity",
            "setup",
            "--agent",
            "codex",
            "--repair",
            "--dry-run",
            "--format",
            "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    let reviewed_digest = data(&preview.stdout_json())["preview_digest"]
        .as_str()
        .expect("preview digest")
        .to_string();

    fs::write(&hooks_path, r#"{"keep":"unreviewed-marker-B"}"#).expect("changed hooks");
    let hooks_changed = fs::read(&hooks_path).expect("changed hooks bytes");
    let notify_unchanged = fs::read(&notify_path).expect("unchanged notify bytes");
    let repair = run(
        tmp.path(),
        &[
            "activity",
            "setup",
            "--agent",
            "codex",
            "--repair",
            "--expected-preview-digest",
            &reviewed_digest,
            "--format",
            "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_ne!(repair.code, 0);
    assert_eq!(
        repair.stdout_json()["error"]["code"],
        "provider-config-preview-digest-mismatch"
    );
    assert_eq!(fs::read(&hooks_path).expect("hooks after"), hooks_changed);
    assert_eq!(
        fs::read(&notify_path).expect("notify after"),
        notify_unchanged
    );
    for forbidden in ["reviewed-marker-A", "unreviewed-marker-B", "user-notifier"] {
        assert!(!repair.stdout_text().contains(forbidden));
        assert!(!repair.stderr_text().contains(forbidden));
    }

    let hooks_reviewed_preview = run(
        tmp.path(),
        &[
            "activity",
            "setup",
            "--agent",
            "codex",
            "--repair",
            "--dry-run",
            "--format",
            "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    let hooks_reviewed_digest = data(&hooks_reviewed_preview.stdout_json())["preview_digest"]
        .as_str()
        .expect("hooks-reviewed preview digest")
        .to_string();
    assert_ne!(hooks_reviewed_digest, reviewed_digest);

    fs::write(
        &notify_path,
        "notify = [\"replacement-notifier\", \"--other\"]\n",
    )
    .expect("changed notification config");
    let hooks_unchanged = fs::read(&hooks_path).expect("unchanged hooks bytes");
    let notify_changed = fs::read(&notify_path).expect("changed notification bytes");
    let stale_notification_repair = run(
        tmp.path(),
        &[
            "activity",
            "setup",
            "--agent",
            "codex",
            "--repair",
            "--expected-preview-digest",
            &hooks_reviewed_digest,
            "--format",
            "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_ne!(stale_notification_repair.code, 0);
    assert_eq!(
        stale_notification_repair.stdout_json()["error"]["code"],
        "provider-config-preview-digest-mismatch"
    );
    assert_eq!(
        fs::read(&hooks_path).expect("hooks after notification race"),
        hooks_unchanged
    );
    assert_eq!(
        fs::read(&notify_path).expect("notification after race"),
        notify_changed
    );
    for forbidden in ["unreviewed-marker-B", "replacement-notifier", "--other"] {
        assert!(!stale_notification_repair.stdout_text().contains(forbidden));
        assert!(!stale_notification_repair.stderr_text().contains(forbidden));
    }

    let current_preview = run(
        tmp.path(),
        &[
            "activity",
            "setup",
            "--agent",
            "codex",
            "--repair",
            "--dry-run",
            "--format",
            "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    let current_digest = data(&current_preview.stdout_json())["preview_digest"]
        .as_str()
        .expect("current preview digest")
        .to_string();
    assert_ne!(current_digest, hooks_reviewed_digest);
    let applied = run(
        tmp.path(),
        &[
            "activity",
            "setup",
            "--agent",
            "codex",
            "--repair",
            "--expected-preview-digest",
            &current_digest,
            "--format",
            "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());
    assert_eq!(data(&applied.stdout_json())["configured"], true);
}

#[test]
fn codex_activity_setup_refuses_a_recursive_notify_without_mutating_configs() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex dir");
    let hooks_path = home.join(".codex/hooks.json");
    let notify_path = home.join(".codex/config.toml");
    fs::write(&hooks_path, r#"{"keep":"hooks"}"#).expect("hooks config");
    fs::write(
        &notify_path,
        "notify = [\"agent-session\", \"activity\", \"notify\", \"--agent\", \"codex\", \"--unexpected\"]\n",
    )
    .expect("notify config");
    let hooks_before = fs::read(&hooks_path).expect("hooks before");
    let notify_before = fs::read(&notify_path).expect("notify before");
    let home_arg = home.to_string_lossy().to_string();
    let result = run(
        tmp.path(),
        &[
            "activity", "setup", "--agent", "codex", "--apply", "--format", "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_ne!(result.code, 0);
    assert_eq!(
        result.stdout_json()["error"]["code"],
        "provider-notification-config-conflict"
    );
    assert_eq!(fs::read(&hooks_path).expect("hooks after"), hooks_before);
    assert_eq!(fs::read(&notify_path).expect("notify after"), notify_before);
}

#[test]
fn codex_activity_setup_refuses_argv_that_expands_past_the_composition_bound() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex dir");
    let hooks_path = home.join(".codex/hooks.json");
    let notify_path = home.join(".codex/config.toml");
    fs::write(&hooks_path, r#"{"keep":"hooks"}"#).expect("hooks config");
    let expanding_arg = "\\".repeat(9_000);
    fs::write(
        &notify_path,
        format!(
            "notify = [\"user-notifier\", {}]\n",
            serde_json::to_string(&expanding_arg).expect("encoded expanding arg")
        ),
    )
    .expect("notify config");
    let hooks_before = fs::read(&hooks_path).expect("hooks before");
    let notify_before = fs::read(&notify_path).expect("notify before");
    let home_arg = home.to_string_lossy().to_string();
    let result = run(
        tmp.path(),
        &[
            "activity", "setup", "--agent", "codex", "--apply", "--format", "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_ne!(result.code, 0);
    assert_eq!(
        result.stdout_json()["error"]["code"],
        "provider-notification-config-conflict"
    );
    assert_eq!(fs::read(&hooks_path).expect("hooks after"), hooks_before);
    assert_eq!(fs::read(&notify_path).expect("notify after"), notify_before);
}

#[test]
fn codex_composed_notify_executes_user_argv_directly_and_fail_open() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex dir");
    let notifier = tmp.path().join("user notifier");
    let log = tmp.path().join("forward.log");
    let injected = tmp.path().join("must-not-exist");
    write_executable(
        &notifier,
        r#"#!/usr/bin/env sh
: "${FORWARD_LOG:?}"
for arg in "$@"; do
  printf '%s\000' "$arg" >> "$FORWARD_LOG"
done
exit 23
"#,
    );
    let notify_path = home.join(".codex/config.toml");
    fs::write(
        &notify_path,
        format!(
            "notify = [{}]\n",
            [
                serde_json::to_string(&notifier.to_string_lossy()).expect("notifier string"),
                serde_json::to_string(&format!("$(touch {})", injected.display()))
                    .expect("literal string"),
            ]
            .join(", ")
        ),
    )
    .expect("notify config");
    let home_arg = home.to_string_lossy().to_string();
    let setup = run(
        tmp.path(),
        &[
            "activity", "setup", "--agent", "codex", "--apply", "--format", "json",
        ],
        &[("HOME", home_arg.as_str())],
    );
    assert_eq!(setup.code, 0, "stderr={}", setup.stderr_text());

    let document = fs::read_to_string(&notify_path)
        .expect("composed config")
        .parse::<toml_edit::DocumentMut>()
        .expect("composed TOML");
    let argv = document["notify"]
        .as_array()
        .expect("notify array")
        .iter()
        .map(|value| value.as_str().expect("string argv").to_string())
        .collect::<Vec<_>>();
    assert_eq!(argv[0], "agent-session");
    let payload = r#"{"type":"agent-turn-complete","thread-id":"thread","turn-id":"turn"}"#;
    let mut args = argv.iter().skip(1).map(String::as_str).collect::<Vec<_>>();
    args.push(payload);
    let log_arg = log.to_string_lossy().to_string();
    let notify = run(
        tmp.path(),
        &args,
        &[
            ("HOME", home_arg.as_str()),
            ("FORWARD_LOG", log_arg.as_str()),
        ],
    );
    assert_eq!(notify.code, 0, "stderr={}", notify.stderr_text());
    assert_eq!(
        fs::read(&log).expect("forward log"),
        format!("$(touch {})\0{payload}\0", injected.display()).as_bytes()
    );
    assert!(
        !injected.exists(),
        "forwarded argv must never run through a shell"
    );
}

#[test]
fn codex_composed_notify_bounds_a_hung_user_notifier() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let notifier = tmp.path().join("hung-notifier");
    let orphan_marker = tmp.path().join("orphan-marker");
    write_executable(
        &notifier,
        &format!(
            "#!/usr/bin/env sh\n(sleep 3; touch '{}') &\nwait\n",
            orphan_marker.display()
        ),
    );
    let forwarded = serde_json::to_string(&vec![notifier.to_string_lossy().to_string()])
        .expect("forwarded argv");
    let started = Instant::now();
    let notify = run(
        tmp.path(),
        &[
            "activity",
            "notify",
            "--agent",
            "codex",
            "--forward-notify-argv-json",
            &forwarded,
            r#"{"type":"agent-turn-complete"}"#,
        ],
        &[],
    );
    assert_eq!(notify.code, 0, "stderr={}", notify.stderr_text());
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "hung notifier must be killed at the bounded fail-open deadline"
    );
    thread::sleep(Duration::from_millis(1_250));
    assert!(
        !orphan_marker.exists(),
        "the timeout must kill the notifier process group, not only its parent"
    );
}

#[test]
fn codex_composed_notify_suppresses_nested_fanout() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let marker = tmp.path().join("nested-marker");
    let wrapper_marker = tmp.path().join("wrapper-marker");
    let notifier = tmp.path().join("nested-notifier");
    write_executable(
        &notifier,
        &format!("#!/usr/bin/env sh\ntouch '{}'\n", marker.display()),
    );
    let nested =
        serde_json::to_string(&vec![notifier.to_string_lossy().to_string()]).expect("nested argv");
    let wrapper = tmp.path().join("fanout-wrapper");
    write_executable(
        &wrapper,
        r#"#!/usr/bin/env sh
touch "$WRAPPER_MARKER"
exec "$WRAPPER_BINARY" activity notify --agent codex --forward-notify-argv-json "$NESTED_ARGV" "$1"
"#,
    );
    let outer =
        serde_json::to_string(&vec![wrapper.to_string_lossy().to_string()]).expect("outer argv");
    let binary = nils_test_support::bin::resolve("agent-session");
    let binary_arg = binary.to_string_lossy().to_string();
    let wrapper_marker_arg = wrapper_marker.to_string_lossy().to_string();
    let notify = run(
        tmp.path(),
        &[
            "activity",
            "notify",
            "--agent",
            "codex",
            "--forward-notify-argv-json",
            &outer,
            r#"{"type":"agent-turn-complete"}"#,
        ],
        &[
            ("WRAPPER_BINARY", binary_arg.as_str()),
            ("WRAPPER_MARKER", wrapper_marker_arg.as_str()),
            ("NESTED_ARGV", nested.as_str()),
        ],
    );
    assert_eq!(notify.code, 0, "stderr={}", notify.stderr_text());
    thread::sleep(Duration::from_millis(100));
    assert!(
        wrapper_marker.exists(),
        "the outer safe wrapper must execute"
    );
    assert!(
        !marker.exists(),
        "a composed notifier must not recursively fan out through agent-session"
    );
}

#[test]
fn codex_composed_notify_fans_out_while_activity_lock_is_contended() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let start = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str())],
    );
    assert_eq!(start.code, 0, "stderr={}", start.stderr_text());
    let id = data(&start.stdout_json())["id"]
        .as_str()
        .expect("id")
        .to_string();
    let record: Value = serde_json::from_str(
        &fs::read_to_string(state_dir.join("sessions").join(&id).join("session.json"))
            .expect("record"),
    )
    .expect("record json");
    let runtime_id = record["runtime"]["launch_id"]
        .as_str()
        .expect("runtime id")
        .to_string();
    let hook_env = [
        ("AGENT_SESSION_ID", id.as_str()),
        ("AGENT_SESSION_RUNTIME_ID", runtime_id.as_str()),
        ("AGENT_SESSION_STATE_DIR", state_arg.as_str()),
    ];
    let provider_thread = "provider-thread";
    let provider_turn = "provider-turn";
    let prompt = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": provider_thread,
        "turn_id": provider_turn,
        "prompt": "content-free marker"
    })
    .to_string();
    let hook = run_with_stdin(
        tmp.path(),
        &["activity", "hook", "--agent", "codex"],
        &hook_env,
        &prompt,
    );
    assert_eq!(hook.code, 0);

    let malformed = run(
        tmp.path(),
        &[
            "activity",
            "notify",
            "--agent",
            "codex",
            "{invalid-notification",
        ],
        &hook_env,
    );
    assert_eq!(malformed.code, 0);
    let diagnostic_path = state_dir
        .join("sessions")
        .join(&id)
        .join("activity.diagnostic.json");
    assert!(diagnostic_path.is_file());

    let lock_path = state_dir.join("sessions").join(&id).join(".activity.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("activity lock");
    // SAFETY: the test owns the descriptor and unlocks it before dropping it.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);

    let marker = tmp.path().join("forwarded-under-lock");
    let notifier = tmp.path().join("lock-notifier");
    write_executable(
        &notifier,
        &format!("#!/usr/bin/env sh\ntouch '{}'\n", marker.display()),
    );
    let forwarded = serde_json::to_string(&vec![notifier.to_string_lossy().to_string()])
        .expect("forwarded argv");
    let payload = json!({
        "type": "agent-turn-complete",
        "thread-id": provider_thread,
        "turn-id": provider_turn
    })
    .to_string();
    let binary = nils_test_support::bin::resolve("agent-session");
    let child = Command::new(binary)
        .current_dir(tmp.path())
        .args([
            "activity",
            "notify",
            "--agent",
            "codex",
            "--forward-notify-argv-json",
            &forwarded,
            &payload,
        ])
        .envs(hook_env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn notify helper");
    let deadline = Instant::now() + Duration::from_millis(750);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let forwarded_before_unlock = marker.exists();
    let diagnostic_preserved_until_retry_finishes = diagnostic_path.is_file();
    // SAFETY: the test owns the locked descriptor.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    let output = child.wait_with_output().expect("notify output");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        forwarded_before_unlock,
        "activity lock contention must not delay the preserved user notifier"
    );
    assert!(
        diagnostic_preserved_until_retry_finishes,
        "the parent must not clear diagnostics before deferred ingestion finishes"
    );
    let completion_deadline = Instant::now() + Duration::from_secs(2);
    let final_phase = loop {
        let status = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "activity",
                "status",
                &id,
                "--format",
                "json",
            ],
            &[],
        );
        let phase = data(&status.stdout_json())["turn_state"]["phase"]
            .as_str()
            .expect("phase")
            .to_string();
        if (phase == "waiting" && !diagnostic_path.exists())
            || Instant::now() >= completion_deadline
        {
            break phase;
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        final_phase, "waiting",
        "the single-shot authoritative completion must be retried after contention"
    );
    assert!(
        !diagnostic_path.exists(),
        "durable retry success must clear the prior diagnostic"
    );

    let invalid_retry = json!({
        "schema_version": "agent-session.turn-event.v1",
        "event_id": "retry-worker-failure",
        "runtime_id": "prior-runtime",
        "provider": "codex",
        "kind": "turn_completed",
        "confidence": "authoritative"
    })
    .to_string();
    let retry_env = [
        ("AGENT_SESSION_ID", id.as_str()),
        ("AGENT_SESSION_RUNTIME_ID", runtime_id.as_str()),
        ("AGENT_SESSION_STATE_DIR", state_arg.as_str()),
        ("AGENT_SESSION_ACTIVITY_RETRY_PROVIDER", "codex"),
    ];
    let failed_retry = run_with_stdin(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "activity",
            "event",
            &id,
            "--stdin",
            "--format",
            "json",
        ],
        &retry_env,
        &invalid_retry,
    );
    assert_ne!(failed_retry.code, 0);
    let diagnostic = fs::read_to_string(&diagnostic_path).expect("retry failure diagnostic");
    assert!(diagnostic.contains("runtime-id-mismatch"));
    assert!(!diagnostic.contains("retry-worker-failure"));
}

#[test]
fn codex_activity_remove_validates_both_configs_before_mutating_either() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex dir");
    let hooks_path = home.join(".codex/hooks.json");
    let notify_path = home.join(".codex/config.toml");
    let home_arg = home.to_string_lossy().to_string();
    let envs = [("HOME", home_arg.as_str())];

    let applied = run(
        tmp.path(),
        &[
            "activity", "setup", "--agent", "codex", "--apply", "--format", "json",
        ],
        &envs,
    );
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());

    let mut hooks: Value =
        serde_json::from_slice(&fs::read(&hooks_path).expect("hooks")).expect("hooks json");
    hooks["hooks"]
        .as_object_mut()
        .expect("hooks object")
        .remove("UserPromptSubmit");
    fs::write(
        &hooks_path,
        serde_json::to_vec_pretty(&hooks).expect("hooks bytes"),
    )
    .expect("partial hooks");
    fs::write(&notify_path, b"notify = [\"unterminated\"").expect("malformed notify config");
    let hooks_before = fs::read(&hooks_path).expect("hooks before");
    let notify_before = fs::read(&notify_path).expect("notify before");

    let removed = run(
        tmp.path(),
        &[
            "activity", "setup", "--agent", "codex", "--remove", "--format", "json",
        ],
        &envs,
    );
    assert_ne!(removed.code, 0);
    assert_eq!(
        removed.stdout_json()["error"]["code"],
        "provider-config-invalid"
    );
    assert_eq!(fs::read(&hooks_path).expect("hooks after"), hooks_before);
    assert_eq!(fs::read(&notify_path).expect("notify after"), notify_before);
}

#[test]
fn codex_activity_doctor_surfaces_notification_config_errors() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".codex")).expect("codex dir");
    fs::write(home.join(".codex/hooks.json"), r#"{"hooks":{}}"#).expect("hooks config");
    fs::write(
        home.join(".codex/config.toml"),
        b"notify = [\"unterminated\"",
    )
    .expect("malformed notify config");
    let home_arg = home.to_string_lossy().to_string();
    let doctor = run(
        tmp.path(),
        &["activity", "doctor", "--agent", "codex", "--format", "json"],
        &[("HOME", home_arg.as_str())],
    );
    assert_eq!(doctor.code, 0, "stderr={}", doctor.stderr_text());
    let doctor_json = doctor.stdout_json();
    let provider = &data(&doctor_json)["providers"][0];
    assert_eq!(provider["configured"], false);
    assert_eq!(provider["configuration_error"], "provider-config-invalid");
    assert_eq!(provider["notification_mode"], "invalid");
    assert!(
        provider["guidance"]
            .as_str()
            .expect("guidance")
            .contains("fix the provider configuration before running repair")
    );
}

#[test]
fn codex_adapter_uses_authoritative_notify_after_conservative_raw_stop() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let start = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str())],
    );
    assert_eq!(start.code, 0, "stderr={}", start.stderr_text());
    let start_json = start.stdout_json();
    let id = data(&start_json)["id"].as_str().expect("id").to_string();
    let record: Value = serde_json::from_str(
        &fs::read_to_string(state_dir.join("sessions").join(&id).join("session.json"))
            .expect("record"),
    )
    .expect("record json");
    let runtime_id = record["runtime"]["launch_id"]
        .as_str()
        .expect("runtime id")
        .to_string();
    let hook_env = [
        ("AGENT_SESSION_ID", id.as_str()),
        ("AGENT_SESSION_RUNTIME_ID", runtime_id.as_str()),
        ("AGENT_SESSION_STATE_DIR", state_arg.as_str()),
    ];
    let secret = "sk-proj-content-must-not-persist";
    let provider_session_secret = "sk-proj-provider-session-must-not-persist";
    let provider_turn_secret = "/secret/provider/turn/path";
    let prompt = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": provider_session_secret,
        "turn_id": provider_turn_secret,
        "prompt": secret,
        "transcript_path": "/secret/transcript.jsonl"
    })
    .to_string();
    let hook = run_with_stdin(
        tmp.path(),
        &["activity", "hook", "--agent", "codex"],
        &hook_env,
        &prompt,
    );
    assert_eq!(hook.code, 0);
    assert!(hook.stdout_text().is_empty());
    assert!(hook.stderr_text().is_empty());
    let status = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "activity",
            "status",
            &id,
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(
        data(&status.stdout_json())["turn_state"]["phase"],
        "working"
    );

    let stop = json!({
        "hook_event_name": "Stop",
        "session_id": provider_session_secret,
        "turn_id": provider_turn_secret,
        "last_assistant_message": secret,
        "stop_hook_active": false
    })
    .to_string();
    let hook = run_with_stdin(
        tmp.path(),
        &["activity", "hook", "--agent", "codex"],
        &hook_env,
        &stop,
    );
    assert_eq!(hook.code, 0);
    let status = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "activity",
            "status",
            &id,
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(
        data(&status.stdout_json())["turn_state"]["phase"],
        "working",
        "raw Stop must not claim Waiting"
    );
    for file in ["activity.json", "activity.journal.jsonl"] {
        let contents = fs::read_to_string(state_dir.join("sessions").join(&id).join(file))
            .expect("activity file");
        assert!(!contents.contains(secret));
        assert!(!contents.contains("transcript_path"));
        assert!(!contents.contains("last_assistant_message"));
        assert!(!contents.contains(provider_session_secret));
        assert!(!contents.contains(provider_turn_secret));
    }

    let notify = |payload: Value, envs: &[(&str, &str)]| {
        run(
            tmp.path(),
            &[
                "activity",
                "notify",
                "--agent",
                "codex",
                &payload.to_string(),
            ],
            envs,
        )
    };
    let missing_turn = notify(
        json!({
            "type": "agent-turn-complete",
            "thread-id": provider_session_secret,
            "last-assistant-message": secret
        }),
        &hook_env,
    );
    assert_eq!(missing_turn.code, 0);
    let diagnostic_path = state_dir
        .join("sessions")
        .join(&id)
        .join("activity.diagnostic.json");
    let diagnostic = fs::read_to_string(&diagnostic_path).expect("missing turn diagnostic");
    assert!(diagnostic.contains("provider-notification-turn-id-missing"));
    assert!(!diagnostic.contains(secret));

    let stale_env = [
        ("AGENT_SESSION_ID", id.as_str()),
        ("AGENT_SESSION_RUNTIME_ID", "prior-runtime"),
        ("AGENT_SESSION_STATE_DIR", state_arg.as_str()),
    ];
    let stale = notify(
        json!({
            "type": "agent-turn-complete",
            "thread-id": provider_session_secret,
            "turn-id": provider_turn_secret
        }),
        &stale_env,
    );
    assert_eq!(stale.code, 0);
    let malformed_notify = run(
        tmp.path(),
        &[
            "activity",
            "notify",
            "--agent",
            "codex",
            "{invalid-notification",
        ],
        &hook_env,
    );
    assert_eq!(malformed_notify.code, 0);
    let diagnostic = fs::read_to_string(&diagnostic_path).expect("invalid notify diagnostic");
    assert!(diagnostic.contains("provider-notification-invalid"));

    let oversized_secret = "sensitive-notification-content";
    let oversized = notify(
        json!({
            "type": "agent-turn-complete",
            "thread-id": provider_session_secret,
            "turn-id": provider_turn_secret,
            "last-assistant-message": oversized_secret.repeat(3_000)
        }),
        &hook_env,
    );
    assert_eq!(oversized.code, 0);
    let diagnostic = fs::read_to_string(&diagnostic_path).expect("oversized notify diagnostic");
    assert!(diagnostic.contains("provider-notification-too-large"));
    assert!(!diagnostic.contains(oversized_secret));
    let wrong_thread = notify(
        json!({
            "type": "agent-turn-complete",
            "thread-id": "different-thread",
            "turn-id": provider_turn_secret
        }),
        &hook_env,
    );
    assert_eq!(wrong_thread.code, 0);
    let status = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "activity",
            "status",
            &id,
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(
        data(&status.stdout_json())["turn_state"]["phase"],
        "working"
    );

    let wrong_turn = notify(
        json!({
            "type": "agent-turn-complete",
            "thread-id": provider_session_secret,
            "turn-id": "different-turn"
        }),
        &hook_env,
    );
    assert_eq!(wrong_turn.code, 0);
    let status = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "activity",
            "status",
            &id,
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(
        data(&status.stdout_json())["turn_state"]["phase"],
        "working"
    );

    let notification = json!({
        "type": "agent-turn-complete",
        "thread-id": provider_session_secret,
        "turn-id": provider_turn_secret,
        "cwd": "/secret/cwd",
        "input-messages": [secret],
        "last-assistant-message": secret
    });
    let completed_notify = notify(notification.clone(), &hook_env);
    assert_eq!(
        completed_notify.code,
        0,
        "official completion notification must be fail-open: stderr={}",
        completed_notify.stderr_text()
    );
    assert!(completed_notify.stdout_text().is_empty());
    assert!(completed_notify.stderr_text().is_empty());
    let status = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "activity",
            "status",
            &id,
            "--format",
            "json",
        ],
        &[],
    );
    let status_json = status.stdout_json();
    assert_eq!(data(&status_json)["turn_state"]["phase"], "waiting");
    assert_eq!(
        data(&status_json)["turn_state"]["last_turn"]["outcome"],
        "completed"
    );
    let completed_revision = data(&status_json)["turn_state"]["revision"].clone();
    let duplicate = notify(notification, &hook_env);
    assert_eq!(duplicate.code, 0);
    let duplicate_status = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "activity",
            "status",
            &id,
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(
        data(&duplicate_status.stdout_json())["turn_state"]["revision"],
        completed_revision
    );
    for file in ["activity.json", "activity.journal.jsonl"] {
        let contents = fs::read_to_string(state_dir.join("sessions").join(&id).join(file))
            .expect("activity file after notify");
        for forbidden in [
            secret,
            provider_session_secret,
            provider_turn_secret,
            "/secret/cwd",
            "input-messages",
            "last-assistant-message",
        ] {
            assert!(!contents.contains(forbidden), "forbidden {forbidden}");
        }
    }

    let malformed = format!(r#"{{"prompt":"{secret}""#);
    let hook = run_with_stdin(
        tmp.path(),
        &["activity", "hook", "--agent", "codex"],
        &hook_env,
        &malformed,
    );
    assert_eq!(hook.code, 0, "provider hooks must remain fail-open");
    let diagnostic = fs::read_to_string(&diagnostic_path).expect("safe diagnostic");
    assert!(diagnostic.contains("provider-hook-invalid"));
    assert!(!diagnostic.contains(secret));
    assert_eq!(
        fs::metadata(&diagnostic_path)
            .expect("diagnostic metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let doctor = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "activity",
            "doctor",
            "--agent",
            "codex",
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(
        data(&doctor.stdout_json())["providers"][0]["last_error"],
        "provider-hook-invalid"
    );

    let ignored = json!({"hook_event_name": "FutureIgnoredEvent"}).to_string();
    let hook = run_with_stdin(
        tmp.path(),
        &["activity", "hook", "--agent", "codex"],
        &hook_env,
        &ignored,
    );
    assert_eq!(hook.code, 0);
    assert!(
        diagnostic_path.exists(),
        "ignored events do not clear errors"
    );

    let stale_env = [
        ("AGENT_SESSION_ID", id.as_str()),
        ("AGENT_SESSION_RUNTIME_ID", "prior-runtime"),
        ("AGENT_SESSION_STATE_DIR", state_arg.as_str()),
    ];
    let hook = run_with_stdin(
        tmp.path(),
        &["activity", "hook", "--agent", "codex"],
        &stale_env,
        "{invalid-stale-json",
    );
    assert_eq!(hook.code, 0);
    let diagnostic = fs::read_to_string(&diagnostic_path).expect("current diagnostic retained");
    assert!(diagnostic.contains("provider-hook-invalid"));

    let mismatched = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "different-provider-session"
    })
    .to_string();
    let hook = run_with_stdin(
        tmp.path(),
        &["activity", "hook", "--agent", "codex"],
        &hook_env,
        &mismatched,
    );
    assert_eq!(hook.code, 0);
    let diagnostic = fs::read_to_string(&diagnostic_path).expect("session mismatch diagnostic");
    assert!(diagnostic.contains("provider-session-id-mismatch"));

    let missing_identity = json!({"hook_event_name": "PostToolUse"}).to_string();
    let hook = run_with_stdin(
        tmp.path(),
        &["activity", "hook", "--agent", "codex"],
        &hook_env,
        &missing_identity,
    );
    assert_eq!(hook.code, 0);
    let diagnostic = fs::read_to_string(&diagnostic_path).expect("missing identity diagnostic");
    assert!(diagnostic.contains("provider-session-id-missing"));

    let progress = json!({
        "hook_event_name": "PostToolUse",
        "session_id": provider_session_secret
    })
    .to_string();
    let hook = run_with_stdin(
        tmp.path(),
        &["activity", "hook", "--agent", "codex"],
        &hook_env,
        &progress,
    );
    assert_eq!(hook.code, 0);
    assert!(!diagnostic_path.exists());
}

#[test]
fn claude_ask_user_question_clears_exactly_and_keeps_generic_attention() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let claude_bin = fake_agent(tmp.path(), "claude");
    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let claude_arg = claude_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let start = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "claude",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &claude_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str())],
    );
    assert_eq!(start.code, 0, "stderr={}", start.stderr_text());
    let start_json = start.stdout_json();
    let id = data(&start_json)["id"].as_str().expect("id").to_string();
    let record: Value = serde_json::from_str(
        &fs::read_to_string(state_dir.join("sessions").join(&id).join("session.json"))
            .expect("record"),
    )
    .expect("record json");
    let runtime_id = record["runtime"]["launch_id"]
        .as_str()
        .expect("runtime id")
        .to_string();
    let provider_session_id = record["provider_resume"]["session_id"]
        .as_str()
        .unwrap_or("claude-session")
        .to_string();
    let hook_env = [
        ("AGENT_SESSION_ID", id.as_str()),
        ("AGENT_SESSION_RUNTIME_ID", runtime_id.as_str()),
        ("AGENT_SESSION_STATE_DIR", state_arg.as_str()),
    ];
    let hook = |payload: Value| {
        run_with_stdin(
            tmp.path(),
            &["activity", "hook", "--agent", "claude"],
            &hook_env,
            &payload.to_string(),
        )
    };
    let status = || {
        run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "activity",
                "status",
                &id,
                "--format",
                "json",
            ],
            &[],
        )
        .stdout_json()
    };

    let prompt_hook = hook(json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": provider_session_id
    }));
    assert_eq!(prompt_hook.code, 0);
    let diagnostic_path = state_dir
        .join("sessions")
        .join(&id)
        .join("activity.diagnostic.json");
    assert!(
        !diagnostic_path.exists(),
        "diagnostic={}",
        fs::read_to_string(&diagnostic_path).unwrap_or_default()
    );
    let working = status();
    assert_eq!(
        data(&working)["turn_state"]["phase"],
        "working",
        "status={working}"
    );
    let started_at = data(&working)["turn_state"]["current_turn"]["started_at"]
        .as_str()
        .expect("started at")
        .to_string();

    let drift_secret = "missing-correlation-content-must-not-persist";
    let missing_correlation = hook(json!({
        "hook_event_name": "PreToolUse",
        "session_id": provider_session_id,
        "tool_name": "AskUserQuestion",
        "tool_input": {"questions": [{"question": drift_secret}]}
    }));
    assert_eq!(
        missing_correlation.code, 0,
        "provider hooks must remain fail-open"
    );
    assert!(missing_correlation.stdout_text().is_empty());
    assert!(missing_correlation.stderr_text().is_empty());
    let diagnostic = fs::read_to_string(&diagnostic_path).expect("schema drift diagnostic");
    assert!(diagnostic.contains("provider-hook-correlation-missing"));
    assert!(!diagnostic.contains(drift_secret));
    assert!(!diagnostic.contains("questions"));

    let raw_tool_id = "tool-use-must-not-persist";
    assert_eq!(
        hook(json!({
            "hook_event_name": "PreToolUse",
            "session_id": provider_session_id,
            "tool_name": "AskUserQuestion",
            "tool_use_id": raw_tool_id,
            "tool_input": {"questions": [{"question": "discarded"}]}
        }))
        .code,
        0
    );
    assert!(
        !diagnostic_path.exists(),
        "a later valid event should clear the drift diagnostic"
    );
    assert_eq!(
        hook(json!({
            "hook_event_name": "PermissionRequest",
            "session_id": provider_session_id,
            "tool_name": "Bash",
            "tool_input": {"command": "discarded"}
        }))
        .code,
        0
    );
    let pending = status();
    assert_eq!(data(&pending)["turn_state"]["phase"], "needs_input");
    assert_eq!(
        data(&pending)["turn_state"]["current_turn"]["attention"]["pending_count"],
        2
    );

    assert_eq!(
        hook(json!({
            "hook_event_name": "PostToolUse",
            "session_id": provider_session_id,
            "tool_name": "AskUserQuestion",
            "tool_use_id": raw_tool_id,
            "tool_response": {"answers": "discarded"}
        }))
        .code,
        0
    );
    let cleared = status();
    let turn = &data(&cleared)["turn_state"]["current_turn"];
    assert_eq!(data(&cleared)["turn_state"]["phase"], "needs_input");
    assert_eq!(turn["attention"]["pending_count"], 1);
    assert_eq!(turn["started_at"], started_at);
    assert!(turn["last_progress_at"].is_string());

    assert_eq!(
        hook(json!({
            "hook_event_name": "PostToolUse",
            "session_id": provider_session_id,
            "tool_name": "Bash",
            "tool_use_id": "unrelated-tool"
        }))
        .code,
        0
    );
    assert_eq!(data(&status())["turn_state"]["phase"], "needs_input");

    for file in ["activity.json", "activity.journal.jsonl"] {
        let contents = fs::read_to_string(state_dir.join("sessions").join(&id).join(file))
            .expect("activity file");
        for forbidden in [
            raw_tool_id,
            provider_session_id.as_str(),
            "questions",
            "answers",
            "discarded",
            "command",
        ] {
            assert!(!contents.contains(forbidden), "forbidden {forbidden}");
        }
    }
}

#[test]
fn start_creates_session_state_without_printing_prompt() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo with spaces");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let prompt = "sent from telegram with sk-proj-secret";
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "--host",
            "sympoies",
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--title",
            "Fix API",
            "--prompt",
            prompt,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--agent-arg",
            "dangerous value; $(nope)",
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[(
            "AGENT_SESSION_FAKE_TMUX_LOG",
            tmux_log.to_string_lossy().as_ref(),
        )],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_no_secret(&output, "sk-proj-secret");
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.start.v1");
    let result = data(&value);
    assert_eq!(result["agent"], "codex");
    assert_eq!(result["cwd"], cwd_arg);
    assert!(
        result["attach_command"]
            .as_str()
            .unwrap()
            .starts_with("tmux attach -t hs-codex-")
    );
    assert!(
        result["ssh_attach_command"]
            .as_str()
            .unwrap()
            .starts_with("ssh -t sympoies ")
    );

    let id = result["id"].as_str().expect("id");
    let prompt_file = state_dir.join("sessions").join(id).join("prompt.md");
    assert_eq!(
        fs::read_to_string(prompt_file).expect("prompt file"),
        prompt
    );
    let record: Value = serde_json::from_str(
        &fs::read_to_string(state_dir.join("sessions").join(id).join("session.json"))
            .expect("session record"),
    )
    .expect("session json");
    let runtime_id = record["runtime"]["launch_id"].as_str().expect("runtime id");

    let calls = tmux_calls(&tmux_log);
    let new_session = calls
        .iter()
        .find(|call| call.first().is_some_and(|arg| arg == "new-session"))
        .expect("new-session call");
    assert_eq!(
        new_session,
        &vec![
            "new-session".to_string(),
            "-d".to_string(),
            "-P".to_string(),
            "-F".to_string(),
            "#{session_id}\t#{pane_id}\t#{pane_pid}".to_string(),
            "-s".to_string(),
            result["tmux_session"].as_str().unwrap().to_string(),
            "-c".to_string(),
            cwd_arg.clone(),
            "-e".to_string(),
            format!("AGENT_SESSION_ID={id}"),
            "-e".to_string(),
            format!("AGENT_SESSION_STATE_DIR={}", state_dir.display()),
            "-e".to_string(),
            format!("AGENT_SESSION_RUNTIME_ID={runtime_id}"),
            "--".to_string(),
            codex_arg.clone(),
            "--cd".to_string(),
            cwd_arg.clone(),
            "--no-alt-screen".to_string(),
            "dangerous value; $(nope)".to_string(),
        ]
    );
    assert!(
        calls
            .iter()
            .any(|call| call.first().is_some_and(|arg| arg == "load-buffer")),
        "missing load-buffer call: {calls:?}"
    );
    assert!(
        calls.iter().any(|call| call
            == &vec![
                "paste-buffer".to_string(),
                "-b".to_string(),
                format!("{id}-prompt"),
                "-d".to_string(),
                "-t".to_string(),
                format!("{}:0.0", result["tmux_session"].as_str().unwrap()),
            ]),
        "missing paste-buffer -d call: {calls:?}"
    );
}

#[test]
fn list_command_and_delete_manage_existing_session() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let claude_bin = fake_agent(tmp.path(), "claude");
    let mut pane = spawn_test_process_group();
    let pane_pid = pane.pid().to_string();

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let claude_arg = claude_bin.to_string_lossy().to_string();
    let envs = [
        (
            "AGENT_SESSION_FAKE_TMUX_LOG",
            tmux_log.to_string_lossy().to_string(),
        ),
        ("AGENT_SESSION_TMUX_BIN", tmux_arg.clone()),
        (
            "AGENT_SESSION_FAKE_TMUX_WINDOW_ACTIVITY",
            "1000000000".to_string(),
        ),
    ];
    let env_refs = [
        (envs[0].0, envs[0].1.as_str()),
        (envs[1].0, envs[1].1.as_str()),
        (envs[2].0, envs[2].1.as_str()),
    ];
    let start_env_refs = [
        (envs[0].0, envs[0].1.as_str()),
        (envs[1].0, envs[1].1.as_str()),
        (envs[2].0, envs[2].1.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_PANE_PID", pane_pid.as_str()),
    ];

    let start = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "claude",
            "--cwd",
            &cwd_arg,
            "--title",
            "Review",
            "--prompt",
            "review this repo",
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &claude_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &start_env_refs,
    );
    assert_eq!(start.code, 0, "stderr={}", start.stderr_text());
    let start_json = start.stdout_json();
    let start_data = data(&start_json);
    let id = start_data["id"].as_str().expect("id").to_string();
    assert_eq!(
        start_data["last_terminal_activity_at"],
        "2001-09-09T01:46:40Z"
    );
    let record_path = state_dir.join("sessions").join(&id).join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    let tmux_session = record["tmux_session"]
        .as_str()
        .expect("tmux session")
        .to_string();
    let provider_resume = &record["provider_resume"];
    let claude_session_id = provider_resume["session_id"]
        .as_str()
        .expect("claude session id");
    assert_eq!(provider_resume["provider"], "claude");
    assert_eq!(
        provider_resume["resume_args"],
        serde_json::json!(["--resume", claude_session_id])
    );
    assert_eq!(record["runtime"]["generation"], 1);
    assert!(record.get("agent_args").is_none());
    let calls = tmux_calls(&tmux_log);
    let new_session = calls
        .iter()
        .find(|call| call.first().is_some_and(|arg| arg == "new-session"))
        .expect("new-session call");
    let session_flag = new_session
        .iter()
        .position(|arg| arg == "--session-id")
        .expect("claude --session-id");
    assert_eq!(
        new_session.get(session_flag + 1).map(String::as_str),
        Some(claude_session_id)
    );
    assert!(
        new_session
            .windows(2)
            .any(|pair| pair[0] == "--name" && pair[1] == "Review"),
        "claude launch should keep title name: {new_session:?}"
    );

    let display_messages_before_list = calls
        .iter()
        .filter(|call| call.first().is_some_and(|arg| arg == "display-message"))
        .count();
    let list_windows = format!("{tmux_session}\t1000000000");
    let list_env_refs = [
        (envs[0].0, envs[0].1.as_str()),
        (envs[1].0, envs[1].1.as_str()),
        (envs[2].0, envs[2].1.as_str()),
        (
            "AGENT_SESSION_FAKE_TMUX_LIST_WINDOWS",
            list_windows.as_str(),
        ),
    ];
    let list = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &list_env_refs,
    );
    assert_eq!(list.code, 0, "stderr={}", list.stderr_text());
    let list_json = list.stdout_json();
    assert_eq!(list_json["schema_version"], "cli.agent-session.list.v1");
    let list_data = data(&list_json).as_array().expect("list data");
    assert_eq!(list_data.len(), 1);
    assert_eq!(list_data[0]["id"], id);
    assert_eq!(list_data[0]["status"], "running");
    assert_eq!(
        list_data[0]["last_terminal_activity_at"],
        "2001-09-09T01:46:40Z"
    );
    let calls_after_list = tmux_calls(&tmux_log);
    assert_eq!(
        calls_after_list
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "list-windows"))
            .count(),
        1,
        "list should batch tmux activity lookup: {calls_after_list:?}"
    );
    assert_eq!(
        calls_after_list
            .iter()
            .filter(|call| call.first().is_some_and(|arg| arg == "display-message"))
            .count(),
        display_messages_before_list,
        "list should not add per-session display-message calls: {calls_after_list:?}"
    );

    let invalid_activity_windows = format!("{tmux_session}\tnot-a-number");
    let invalid_activity_env_refs = [
        (envs[0].0, envs[0].1.as_str()),
        (envs[1].0, envs[1].1.as_str()),
        (envs[2].0, envs[2].1.as_str()),
        (
            "AGENT_SESSION_FAKE_TMUX_LIST_WINDOWS",
            invalid_activity_windows.as_str(),
        ),
    ];
    let list_without_activity = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &invalid_activity_env_refs,
    );
    assert_eq!(
        list_without_activity.code,
        0,
        "stderr={}",
        list_without_activity.stderr_text()
    );
    let list_without_activity_json = list_without_activity.stdout_json();
    let list_without_activity_data = data(&list_without_activity_json)
        .as_array()
        .expect("list data");
    assert_eq!(list_without_activity_data[0]["status"], "running");
    assert!(
        list_without_activity_data[0]
            .get("last_terminal_activity_at")
            .is_none()
    );

    let command = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "--host",
            "sympoies",
            "command",
            &id,
            "--format",
            "json",
        ],
        &env_refs,
    );
    assert_eq!(command.code, 0, "stderr={}", command.stderr_text());
    let command_json = command.stdout_json();
    let command_data = data(&command_json);
    assert_eq!(
        command_data["last_terminal_activity_at"],
        "2001-09-09T01:46:40Z"
    );
    assert!(
        command_data["ssh_attach_command"]
            .as_str()
            .unwrap()
            .starts_with("ssh -t sympoies"),
        "missing ssh attach command: {command_data}"
    );
    let command_without_activity_envs = [
        (envs[0].0, envs[0].1.as_str()),
        (envs[1].0, envs[1].1.as_str()),
        (envs[2].0, envs[2].1.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_FAIL", "display-message"),
    ];
    let command_without_activity = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "--host",
            "sympoies",
            "command",
            &id,
            "--format",
            "json",
        ],
        &command_without_activity_envs,
    );
    assert_eq!(
        command_without_activity.code,
        0,
        "stderr={}",
        command_without_activity.stderr_text()
    );
    let command_without_activity_json = command_without_activity.stdout_json();
    let command_without_activity_data = data(&command_without_activity_json);
    assert_eq!(command_without_activity_data["status"], "running");
    assert!(
        command_without_activity_data
            .get("last_terminal_activity_at")
            .is_none()
    );

    pane.stop();
    let launch_id = seed_delete_tmux_identity(&record_path, "$77", "%77", pane.pid());
    let mut stopped_record: Value =
        serde_json::from_slice(&fs::read(&record_path).expect("stopped session record")).unwrap();
    stopped_record["delete_tmux_termination_state"] = json!({
        "launch_id": launch_id,
        "state": "kill-confirmed",
    });
    fs::write(
        &record_path,
        serde_json::to_vec_pretty(&stopped_record).unwrap(),
    )
    .unwrap();
    let delete_env = [
        (envs[0].0, envs[0].1.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
    ];
    let delete = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "delete",
            &id,
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &delete_env,
    );
    assert_eq!(delete.code, 0, "stderr={}", delete.stderr_text());
    let delete_json = delete.stdout_json();
    assert_eq!(delete_json["schema_version"], "cli.agent-session.delete.v1");
    assert_eq!(data(&delete_json)["deleted"], true);
    let delete_calls = tmux_calls(&tmux_log);
    assert!(
        delete_calls
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "if-shell")),
        "confirmed stopped cleanup must not repeat the tmux mutation: {delete_calls:?}"
    );
    assert!(
        delete_calls.iter().any(|call| call
            == &vec![
                "has-session".to_string(),
                "-t".to_string(),
                "$77".to_string(),
            ]),
        "delete must verify the recorded tmux session stopped: {delete_calls:?}"
    );

    let list_again = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &env_refs,
    );
    assert_eq!(list_again.code, 0, "stderr={}", list_again.stderr_text());
    assert_eq!(data(&list_again.stdout_json()).as_array().unwrap().len(), 0);
}

#[test]
fn delete_kill_failure_retains_codex_and_claude_runtime_state() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    for agent in ["codex", "claude"] {
        let id = format!("failed-delete-{agent}");
        let tmux_session = format!("hs-{agent}-failed-delete");
        let session_dir = write_session_record(&state_dir, &id, agent, &tmux_session);
        let resume_path = session_dir.join("resume.json");
        fs::write(&resume_path, format!("resume metadata for {agent}")).expect("resume metadata");

        let record_path = session_dir.join("session.json");
        let runtime_files = attach_provider_runtime(
            tmp.path(),
            &state_dir,
            &session_dir,
            &id,
            agent,
            &tmux_session,
        );
        let record: Value =
            serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
        let runtime_id = record["runtime"]["launch_id"].as_str().unwrap();
        let pane = spawn_test_process_group();
        let pane_pid = pane.pid().to_string();

        let output = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "delete",
                &id,
                "--tmux-bin",
                &tmux_arg,
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ("AGENT_SESSION_FAKE_TMUX_FAIL", "kill-session"),
                ("AGENT_SESSION_FAKE_TMUX_PANE_PID", pane_pid.as_str()),
                (
                    "AGENT_SESSION_FAKE_TMUX_PROCESS_GROUP_ID",
                    pane_pid.as_str(),
                ),
                ("AGENT_SESSION_FAKE_TMUX_SESSION_ID", "$77"),
                ("AGENT_SESSION_FAKE_TMUX_AGENT_SESSION_ID", id.as_str()),
                ("AGENT_SESSION_FAKE_TMUX_STATE_DIR", state_arg.as_str()),
                ("AGENT_SESSION_FAKE_TMUX_RUNTIME_ID", runtime_id),
            ],
        );

        assert_eq!(
            output.code,
            1,
            "stdout={} stderr={}",
            output.stdout_text(),
            output.stderr_text()
        );
        let failure = output.stdout_json();
        assert_eq!(failure["schema_version"], "cli.agent-session.delete.v1");
        assert_eq!(failure["ok"], false);
        assert_eq!(failure["error"]["code"], "session-termination-failed");
        assert_eq!(failure["error"]["details"]["id"], id);
        assert_eq!(failure["error"]["details"]["tmux_session"], tmux_session);
        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                failure["error"]["details"]["reason"],
                "runtime-identity-unavailable"
            );
            assert_eq!(
                failure["error"]["details"]["action"],
                "manual-runtime-verification-required"
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert_eq!(failure["error"]["details"]["reason"], "kill-failed");
            assert_eq!(failure["error"]["details"]["action"], "retry-delete");
        }
        assert!(session_dir.exists(), "{agent} state must remain retryable");
        assert!(record_path.exists(), "{agent} session metadata must remain");
        let retained: Value =
            serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
        assert_eq!(retained["delete_tmux_identity"]["session_id"], "$77");
        assert_eq!(retained["delete_tmux_identity"]["pane_id"], "%77");
        assert_eq!(
            retained["delete_tmux_identity"]["process_group_id"],
            pane.pid()
        );
        assert_eq!(
            fs::read_to_string(&resume_path).unwrap(),
            format!("resume metadata for {agent}")
        );
        for runtime_file in runtime_files {
            assert_eq!(
                fs::read_to_string(runtime_file).unwrap(),
                format!("runtime metadata for {agent}")
            );
        }
        let calls = tmux_calls(&tmux_log);
        assert!(
            calls.iter().any(|call| call
                == &vec![
                    "display-message".to_string(),
                    "-p".to_string(),
                    "-t".to_string(),
                    format!("={tmux_session}:0.0"),
                    "#{session_id} #{pane_id} #{pane_pid}".to_string(),
                ]),
            "delete must inspect the managed 0.0 pane, not the active pane: {calls:?}"
        );
        #[cfg(target_os = "linux")]
        assert!(
            calls
                .iter()
                .all(|call| call.first().is_none_or(|arg| arg != "if-shell")),
            "Linux deletion must fail before mutating tmux without a dedicated cgroup: {calls:?}"
        );
    }
}

#[test]
fn delete_uses_launch_identity_after_runtime_stops_before_first_delete() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    for agent in ["codex", "claude"] {
        let agent_bin = fake_agent(tmp.path(), agent);
        let agent_arg = agent_bin.to_string_lossy().to_string();
        let id = format!("stopped-before-delete-{agent}");
        let start = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "start",
                "--agent",
                agent,
                "--id",
                &id,
                "--cwd",
                &cwd_arg,
                "--tmux-bin",
                &tmux_arg,
                "--agent-bin",
                &agent_arg,
                "--paste-delay-ms",
                "0",
                "--format",
                "json",
            ],
            &[("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg)],
        );
        assert_eq!(start.code, 0, "stdout={}", start.stdout_text());
        let session_dir = state_dir.join("sessions").join(&id);
        let record: Value = serde_json::from_slice(
            &fs::read(session_dir.join("session.json")).expect("launched session record"),
        )
        .expect("session json");
        assert_eq!(record["delete_tmux_identity"]["session_id"], "$77");
        assert_eq!(record["delete_tmux_identity"]["pane_id"], "%77");
        assert_eq!(
            record["delete_tmux_identity"]["launch_id"],
            record["runtime"]["launch_id"]
        );

        let delete = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "delete",
                &id,
                "--tmux-bin",
                &tmux_arg,
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
            ],
        );
        assert_eq!(delete.code, 0, "stdout={}", delete.stdout_text());
        assert_eq!(data(&delete.stdout_json())["deleted"], true);
        assert!(
            !session_dir.exists(),
            "{agent} stopped state must be removed"
        );
    }
}

#[test]
fn resume_refuses_to_replace_a_surviving_prior_launch_identity() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let session = write_resumable_session_record_with_agent_bin(
        &state_dir,
        "resume-survivor",
        "codex",
        "hs-codex-resume-survivor",
        &cwd,
        &[
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().unwrap(),
            "--no-alt-screen",
        ],
        Some(&codex_bin),
    );
    let record_path = session.join("session.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    record["runtime"]["launch_id"] = json!("prior-launch");
    let mut prior_process = spawn_test_process_group();
    record["delete_tmux_identity"] = json!({
        "launch_id": "prior-launch",
        "session_id": "$66",
        "pane_id": "%66",
        "pane_pid": 99999999,
        "process_group_id": 99999999,
    });
    record["delete_tmux_prior_identities"] = json!([{
        "launch_id": "prior-launch",
        "session_id": "$66",
        "pane_id": "%66",
        "pane_pid": prior_process.pid(),
        "process_group_id": prior_process.pid(),
    }]);
    fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let resume_args = [
        "--state-dir",
        &state_arg,
        "resume",
        "resume-survivor",
        "--tmux-bin",
        &tmux_arg,
        "--format",
        "json",
    ];
    let envs = [
        ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
    ];

    let blocked = run(tmp.path(), &resume_args, &envs);
    assert_eq!(blocked.code, 1, "stdout={}", blocked.stdout_text());
    let blocked_json = blocked.stdout_json();
    assert_eq!(
        blocked_json["error"]["details"]["reason"],
        "process-still-running"
    );
    assert_eq!(blocked_json["error"]["details"]["retryable"], true);
    assert_eq!(blocked_json["error"]["details"]["action"], "retry-resume");
    let retained: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(retained["runtime"]["launch_id"], "prior-launch");
    assert_eq!(retained["delete_tmux_identity"]["session_id"], "$66");
    assert_eq!(
        retained["delete_tmux_prior_identities"][0]["process_group_id"],
        prior_process.pid()
    );
    assert!(
        tmux_calls(&tmux_log)
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "new-session")),
        "resume must not replace a runtime that still has surviving processes"
    );

    prior_process.stop();
    let resumed = run(tmp.path(), &resume_args, &envs);
    assert_eq!(resumed.code, 0, "stdout={}", resumed.stdout_text());
    let replaced: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    assert_ne!(replaced["runtime"]["launch_id"], "prior-launch");
    assert_eq!(
        replaced["delete_tmux_identity"]["launch_id"],
        replaced["runtime"]["launch_id"]
    );
    assert_eq!(replaced["delete_tmux_identity"]["session_id"], "$77");
    assert!(replaced.get("delete_tmux_prior_identities").is_none());
}

#[test]
fn resume_refuses_unprovable_pre_upgrade_codex_and_claude_runtimes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    for agent in ["codex", "claude"] {
        for runtime_shape in ["identity-free", "omitted"] {
            let id = format!("unprovable-resume-{agent}-{runtime_shape}");
            let tmux_session = format!("hs-{agent}-unprovable-resume-{runtime_shape}");
            let resume_args = match agent {
                "codex" => vec![
                    "resume",
                    "resume-session-id",
                    "--cd",
                    cwd.to_str().unwrap(),
                    "--no-alt-screen",
                ],
                "claude" => vec!["--resume", "resume-session-id"],
                _ => unreachable!(),
            };
            let session_dir = write_resumable_session_record(
                &state_dir,
                &id,
                agent,
                &tmux_session,
                &cwd,
                &resume_args,
            );
            let record_path = session_dir.join("session.json");
            let mut record: Value =
                serde_json::from_slice(&fs::read(&record_path).expect("session record")).unwrap();
            let record = record.as_object_mut().unwrap();
            record.remove("startup");
            record.remove("tmux_runtime_never_launched");
            if runtime_shape == "omitted" {
                record.remove("runtime");
            }
            fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
            let before = fs::read(&record_path).expect("session record");
            let output = run(
                tmp.path(),
                &[
                    "--state-dir",
                    &state_arg,
                    "resume",
                    &id,
                    "--tmux-bin",
                    &tmux_arg,
                    "--format",
                    "json",
                ],
                &[
                    ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                    ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
                ],
            );

            assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
            let error = output.stdout_json();
            assert_eq!(error["error"]["code"], "session-termination-failed");
            assert_eq!(
                error["error"]["details"]["reason"],
                "runtime-identity-unavailable"
            );
            assert_eq!(error["error"]["details"]["retryable"], false);
            assert_eq!(
                error["error"]["details"]["action"],
                "manual-runtime-verification-required"
            );
            assert_eq!(
                fs::read(&record_path).expect("retained session record"),
                before,
                "resume must preserve the unprovable {agent} {runtime_shape} record"
            );
        }
    }

    assert!(
        tmux_calls(&tmux_log)
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "new-session")),
        "unprovable pre-upgrade runtimes must never be replaced"
    );
}

#[test]
fn pre_upgrade_delete_handles_live_identity_and_explains_unprovable_stopped_state() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    for agent in ["codex", "claude"] {
        let live_id = format!("pre-upgrade-live-{agent}");
        let live_session = format!("hs-{agent}-pre-upgrade-live");
        let live_dir = write_session_record(&state_dir, &live_id, agent, &live_session);
        let live_process = spawn_test_process_group();
        let live_pid = live_process.pid().to_string();
        let live_delete = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "delete",
                &live_id,
                "--tmux-bin",
                &tmux_arg,
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ("AGENT_SESSION_FAKE_TMUX_PANE_PID", &live_pid),
                ("AGENT_SESSION_FAKE_TMUX_PROCESS_GROUP_ID", &live_pid),
                ("AGENT_SESSION_FAKE_TMUX_AGENT_SESSION_ID", &live_id),
                ("AGENT_SESSION_FAKE_TMUX_STATE_DIR", &state_arg),
            ],
        );
        #[cfg(target_os = "linux")]
        {
            assert_eq!(live_delete.code, 1, "stdout={}", live_delete.stdout_text());
            let error = live_delete.stdout_json();
            assert_eq!(
                error["error"]["details"]["reason"],
                "runtime-identity-unavailable"
            );
            assert_eq!(error["error"]["details"]["retryable"], false);
            assert!(
                live_dir.exists(),
                "live pre-upgrade {agent} state without a dedicated cgroup must remain"
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert_eq!(live_delete.code, 0, "stdout={}", live_delete.stdout_text());
            assert!(
                !live_dir.exists(),
                "live pre-upgrade {agent} state must delete"
            );
        }

        let stopped_id = format!("pre-upgrade-stopped-{agent}");
        let stopped_session = format!("hs-{agent}-pre-upgrade-stopped");
        let stopped_dir = write_session_record(&state_dir, &stopped_id, agent, &stopped_session);
        let stopped_delete = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "delete",
                &stopped_id,
                "--tmux-bin",
                &tmux_arg,
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
            ],
        );
        assert_eq!(
            stopped_delete.code,
            1,
            "stdout={}",
            stopped_delete.stdout_text()
        );
        let error = stopped_delete.stdout_json();
        assert_eq!(
            error["error"]["details"]["reason"],
            "runtime-identity-unavailable"
        );
        assert_eq!(error["error"]["details"]["retryable"], false);
        assert_eq!(
            error["error"]["details"]["action"],
            "manual-runtime-verification-required"
        );
        assert!(stopped_dir.exists(), "unprovable {agent} state must remain");
    }
}

#[test]
fn delete_accepts_current_generation_never_launched_proof_for_codex_and_claude() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    for agent in ["codex", "claude"] {
        let id = format!("never-launched-delete-{agent}");
        let tmux_session = format!("hs-{agent}-never-launched-delete");
        let session_dir = write_session_record(&state_dir, &id, agent, &tmux_session);
        let runtime_files = attach_provider_runtime(
            tmp.path(),
            &state_dir,
            &session_dir,
            &id,
            agent,
            &tmux_session,
        );
        let record_path = session_dir.join("session.json");
        let mut record: Value =
            serde_json::from_slice(&fs::read(&record_path).expect("session record")).unwrap();
        record["tmux_runtime_never_launched"] = record["runtime"]["launch_id"].clone();
        record
            .as_object_mut()
            .expect("session object")
            .remove("delete_tmux_identity");
        fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

        let output = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "delete",
                &id,
                "--tmux-bin",
                &tmux_arg,
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
            ],
        );

        assert_eq!(output.code, 0, "stdout={}", output.stdout_text());
        assert_eq!(data(&output.stdout_json())["deleted"], true);
        assert!(!session_dir.exists(), "{agent} metadata must be removed");
        assert!(
            runtime_files.iter().all(|path| !path.exists()),
            "{agent} runtime metadata must be removed"
        );
    }

    let calls = tmux_calls(&tmux_log);
    assert!(
        calls
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "kill-session")),
        "a definitively unlaunched runtime must not invoke kill-session: {calls:?}"
    );
}

#[test]
fn delete_accepts_blank_identity_output_only_after_exact_absence_confirmation() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    for (id, has_session, persisted_identity, expected_code) in [
        ("blank-absent-never-launched", "0", false, 0),
        ("blank-live-never-launched", "1", false, 1),
        ("blank-absent-stopped-runtime", "0", true, 0),
        ("blank-live-persisted-runtime", "1", true, 1),
    ] {
        let tmux_session = format!("hs-codex-{id}");
        let session_dir = write_session_record(&state_dir, id, "codex", &tmux_session);
        let runtime_files = attach_provider_runtime(
            tmp.path(),
            &state_dir,
            &session_dir,
            id,
            "codex",
            &tmux_session,
        );
        let record_path = session_dir.join("session.json");
        let mut record: Value =
            serde_json::from_slice(&fs::read(&record_path).expect("session record")).unwrap();
        if persisted_identity {
            let launch_id = record["runtime"]["launch_id"].clone();
            record["delete_tmux_identity"] = json!({
                "launch_id": launch_id,
                "session_id": "$77",
                "pane_id": "%77",
                "pane_pid": 99_999_977,
                "process_group_id": 99_999_977,
            });
            record
                .as_object_mut()
                .expect("session object")
                .remove("tmux_runtime_never_launched");
        } else {
            record["tmux_runtime_never_launched"] = record["runtime"]["launch_id"].clone();
            record
                .as_object_mut()
                .expect("session object")
                .remove("delete_tmux_identity");
        }
        fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        let record_before = fs::read(&record_path).expect("session record before delete");
        let calls_before = tmux_calls(&tmux_log).len();

        let output = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "delete",
                id,
                "--tmux-bin",
                &tmux_arg,
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ("AGENT_SESSION_FAKE_TMUX_BLANK_DISPLAY", "1"),
                ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", has_session),
            ],
        );

        assert_eq!(
            output.code,
            expected_code,
            "stdout={}",
            output.stdout_text()
        );
        let calls = tmux_calls(&tmux_log);
        let exact_probe = vec![
            "has-session".to_string(),
            "-t".to_string(),
            format!("={tmux_session}"),
        ];
        assert!(
            calls[calls_before..]
                .iter()
                .any(|call| call == &exact_probe),
            "delete must confirm the exact session target: {:?}",
            &calls[calls_before..]
        );

        if expected_code == 0 {
            assert!(!session_dir.exists());
            assert!(
                runtime_files.iter().all(|path| !path.exists()),
                "successful cleanup must remove provider runtime files"
            );
        } else {
            assert!(session_dir.exists());
            assert_eq!(
                output.stdout_json()["error"]["details"]["reason"],
                "runtime-identity-unavailable"
            );
            assert_eq!(
                fs::read(&record_path).expect("retained session record"),
                record_before
            );
            for runtime_file in runtime_files {
                assert_eq!(
                    fs::read_to_string(runtime_file).expect("retained runtime file"),
                    "runtime metadata for codex"
                );
            }
        }
    }

    let calls = tmux_calls(&tmux_log);
    assert!(
        calls
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "kill-session"))
    );
}

#[test]
fn blank_identity_probe_keeps_state_when_the_persisted_process_group_is_live() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let session_dir = write_session_record(
        &state_dir,
        "blank-probe-live-process",
        "codex",
        "hs-codex-blank-probe-live-process",
    );
    attach_provider_runtime(
        tmp.path(),
        &state_dir,
        &session_dir,
        "blank-probe-live-process",
        "codex",
        "hs-codex-blank-probe-live-process",
    );
    let mut process = spawn_test_process_group();
    seed_delete_tmux_identity(
        &session_dir.join("session.json"),
        "$77",
        "%77",
        process.pid(),
    );

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "delete",
            "blank-probe-live-process",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_BLANK_DISPLAY", "1"),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    assert_eq!(
        output.stdout_json()["error"]["details"]["reason"],
        "process-still-running"
    );
    assert!(
        session_dir.exists(),
        "live process evidence must retain state"
    );
    process.stop();
}

#[test]
fn successful_delete_removes_codex_and_claude_runtime_state() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    for agent in ["codex", "claude"] {
        let id = format!("successful-delete-{agent}");
        let tmux_session = format!("hs-{agent}-successful-delete");
        let session_dir = write_session_record(&state_dir, &id, agent, &tmux_session);
        let runtime_files = attach_provider_runtime(
            tmp.path(),
            &state_dir,
            &session_dir,
            &id,
            agent,
            &tmux_session,
        );
        let record_path = session_dir.join("session.json");
        let launch_id = seed_delete_tmux_identity(&record_path, "$77", "%77", 99_999_977);
        let mut record: Value =
            serde_json::from_slice(&fs::read(&record_path).expect("session record")).unwrap();
        record["delete_tmux_termination_state"] = json!({
            "launch_id": launch_id,
            "state": "kill-confirmed",
        });
        fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

        let output = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "delete",
                &id,
                "--tmux-bin",
                &tmux_arg,
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
            ],
        );

        assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
        let result = output.stdout_json();
        assert_eq!(result["ok"], true);
        assert_eq!(data(&result)["killed"], true);
        assert_eq!(data(&result)["deleted"], true);
        assert!(!session_dir.exists(), "{agent} metadata must be removed");
        for runtime_file in runtime_files {
            assert!(
                !runtime_file.exists(),
                "{} must be removed",
                runtime_file.display()
            );
        }
    }

    let calls = tmux_calls(&tmux_log);
    assert!(
        calls
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "if-shell")),
        "confirmed stopped cleanup must not repeat the tmux mutation: {calls:?}"
    );
}

#[test]
fn delete_operational_tmux_probe_error_retains_codex_and_claude_state() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let tmux_bin = tmp.path().join("tmux-probe-error");
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let state_arg = state_dir.to_string_lossy().to_string();
    write_executable(
        &tmux_bin,
        "#!/usr/bin/env sh\necho 'error connecting to /tmp/tmux-test/default (No such file or directory)' >&2\nexit 1\n",
    );

    for agent in ["codex", "claude"] {
        let id = format!("probe-error-{agent}");
        let tmux_session = format!("hs-{agent}-probe-error");
        let session_dir = write_session_record(&state_dir, &id, agent, &tmux_session);
        let runtime_files = attach_provider_runtime(
            tmp.path(),
            &state_dir,
            &session_dir,
            &id,
            agent,
            &tmux_session,
        );
        let output = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "delete",
                &id,
                "--tmux-bin",
                &tmux_arg,
                "--format",
                "json",
            ],
            &[],
        );

        assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
        let error = output.stdout_json();
        assert_eq!(error["error"]["code"], "session-termination-failed");
        assert_eq!(error["error"]["details"]["reason"], "verification-failed");
        assert!(session_dir.exists(), "{agent} metadata must remain");
        assert!(runtime_files.iter().all(|path| path.exists()));
    }
}

#[test]
fn delete_runtime_identity_mismatch_never_kills_the_exact_name_reuse() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let id = "identity-mismatch";
    let tmux_session = "hs-codex-identity-mismatch";
    let session_dir = write_session_record(&state_dir, id, "codex", tmux_session);
    attach_provider_runtime(
        tmp.path(),
        &state_dir,
        &session_dir,
        id,
        "codex",
        tmux_session,
    );
    let tmux_bin = tmp.path().join("tmux-identity-mismatch");
    let killed = tmp.path().join("wrong-session-killed");
    write_executable(
        &tmux_bin,
        &format!(
            "#!/usr/bin/env sh\ncase \"$1\" in\n  display-message) printf '$91\\t%%91\\t99999999\\n'; exit 0 ;;
  show-environment)\n    case \"$4\" in\n      AGENT_SESSION_ID) printf 'AGENT_SESSION_ID=unrelated-session\\n' ;;
      AGENT_SESSION_STATE_DIR) printf 'AGENT_SESSION_STATE_DIR={}\\n' ;;
      AGENT_SESSION_RUNTIME_ID) printf 'AGENT_SESSION_RUNTIME_ID=unrelated-runtime\\n' ;;
    esac\n    exit 0 ;;
  has-session) [ -f {} ] && exit 1; exit 0 ;;
  if-shell) : > {}; exit 0 ;;
esac\nexit 42\n",
            state_dir.display(),
            killed.display(),
            killed.display(),
        ),
    );
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_dir.to_string_lossy(),
            "delete",
            id,
            "--tmux-bin",
            &tmux_bin.to_string_lossy(),
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    assert_eq!(
        output.stdout_json()["error"]["details"]["reason"],
        "runtime-identity-mismatch"
    );
    assert!(!killed.exists(), "the reused exact name must not be killed");
    assert!(session_dir.exists());
}

#[test]
fn delete_retains_original_process_identity_when_tmux_pane_respawns() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let tmux_bin = tmp.path().join("tmux-respawned-pane");
    let tmux_log = tmp.path().join("tmux-respawned-pane.log");
    let tmux_stopped = tmp.path().join("tmux-respawned-pane.stopped");
    let id = "respawned-pane";
    let tmux_session = "hs-codex-respawned-pane";
    let session_dir = write_session_record(&state_dir, id, "codex", tmux_session);
    attach_provider_runtime(
        tmp.path(),
        &state_dir,
        &session_dir,
        id,
        "codex",
        tmux_session,
    );
    let record_path = session_dir.join("session.json");
    let mut old_pane = spawn_test_process_group();
    let mut live_pane = spawn_test_process_group();
    let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    let launch_id = record["runtime"]["launch_id"].as_str().unwrap().to_string();
    record["delete_tmux_identity"] = json!({
        "launch_id": launch_id,
        "session_id": "$77",
        "pane_id": "%77",
        "pane_pid": old_pane.pid(),
        "process_group_id": old_pane.pid(),
    });
    fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

    write_executable(
        &tmux_bin,
        &format!(
            r#"#!/usr/bin/env sh
printf '%s\n' "$*" >> {tmux_log}
case "$1" in
  display-message)
    if [ -f {tmux_stopped} ]; then
      printf "%s\n" "can't find session: {tmux_session}" >&2
      exit 1
    fi
    printf '$77\t%%77\t{live_pid}\n'
    ;;
  show-environment)
    case "$4" in
      AGENT_SESSION_ID) printf 'AGENT_SESSION_ID=%s\n' {id} ;;
      AGENT_SESSION_STATE_DIR) printf 'AGENT_SESSION_STATE_DIR=%s\n' {state_dir} ;;
      AGENT_SESSION_RUNTIME_ID) printf 'AGENT_SESSION_RUNTIME_ID=%s\n' {launch_id} ;;
    esac
    ;;
  kill-session)
    : > {tmux_stopped}
    ;;
  has-session)
    if [ -f {tmux_stopped} ]; then
      printf "%s\n" "can't find session: $77" >&2
      exit 1
    fi
    ;;
  *) exit 42 ;;
esac
exit 0
"#,
            tmux_log = shell_words::quote(&tmux_log.to_string_lossy()),
            tmux_stopped = shell_words::quote(&tmux_stopped.to_string_lossy()),
            live_pid = live_pane.pid(),
            id = shell_words::quote(id),
            state_dir = shell_words::quote(&state_dir.to_string_lossy()),
            launch_id = shell_words::quote(&launch_id),
        ),
    );

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "delete",
            id,
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    let error = output.stdout_json();
    assert_eq!(error["error"]["code"], "session-termination-failed");
    assert!(
        matches!(
            error["error"]["details"]["reason"].as_str(),
            Some("process-still-running" | "verification-failed")
        ),
        "unexpected bounded verification result: {error}"
    );
    assert_eq!(error["error"]["details"]["retryable"], true);
    assert_eq!(error["error"]["details"]["action"], "retry-delete");
    assert!(
        session_dir.exists(),
        "original process evidence must remain"
    );
    assert!(
        old_pane.is_running(),
        "the original pane group must still be live"
    );
    let retained: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(
        retained["delete_tmux_identity"]["pane_pid"],
        live_pane.pid()
    );
    assert_eq!(
        retained["delete_tmux_identity"]["process_group_id"],
        live_pane.pid()
    );
    assert_eq!(
        retained["delete_tmux_prior_identities"][0]["pane_pid"],
        old_pane.pid()
    );
    assert_eq!(
        retained["delete_tmux_prior_identities"][0]["process_group_id"],
        old_pane.pid()
    );

    fs::write(&tmux_stopped, b"").unwrap();
    old_pane.stop();
    let retry = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "delete",
            id,
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(retry.code, 1, "stdout={}", retry.stdout_text());
    let retry_error = retry.stdout_json();
    assert_eq!(
        retry_error["error"]["details"]["reason"],
        "process-still-running"
    );
    assert!(
        session_dir.exists(),
        "replacement process evidence must remain"
    );
    assert!(
        live_pane.is_running(),
        "the replacement pane group must still be live"
    );
    let retained: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(
        retained["delete_tmux_identity"]["pane_pid"],
        live_pane.pid()
    );
    assert_eq!(
        retained["delete_tmux_identity"]["process_group_id"],
        live_pane.pid()
    );
    let calls = fs::read_to_string(&tmux_log).unwrap();
    assert_eq!(calls.matches("kill-session -t $77").count(), 0, "{calls}");

    live_pane.stop();
    let final_retry = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "delete",
            id,
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(final_retry.code, 0, "stdout={}", final_retry.stdout_text());
    assert!(!session_dir.exists(), "retry must remove stopped state");
}

#[test]
fn delete_recaptures_a_bounded_replacement_and_fails_closed_without_a_control_group() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let tmux_bin = tmp.path().join("tmux-kill-time-respawn");
    let tmux_log = tmp.path().join("tmux-kill-time-respawn.log");
    let tmux_switched = tmp.path().join("tmux-kill-time-respawn.switched");
    let tmux_stopped = tmp.path().join("tmux-kill-time-respawn.stopped");
    let id = "kill-time-respawn";
    let tmux_session = "hs-codex-kill-time-respawn";
    let session_dir = write_session_record(&state_dir, id, "codex", tmux_session);
    attach_provider_runtime(
        tmp.path(),
        &state_dir,
        &session_dir,
        id,
        "codex",
        tmux_session,
    );
    let record_path = session_dir.join("session.json");
    let mut captured_pane = spawn_test_process_group();
    let mut replacement_pane = spawn_test_process_group();
    let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    let launch_id = record["runtime"]["launch_id"].as_str().unwrap().to_string();
    record["delete_tmux_identity"] = json!({
        "launch_id": launch_id,
        "session_id": "$78",
        "pane_id": "%78",
        "pane_pid": captured_pane.pid(),
        "process_group_id": captured_pane.pid(),
    });
    fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

    write_executable(
        &tmux_bin,
        &format!(
            r#"#!/usr/bin/env sh
printf '%s\n' "$*" >> {tmux_log}
case "$1" in
  display-message)
    if [ -f {tmux_stopped} ]; then
      printf "%s\n" "can't find session: {tmux_session}" >&2
      exit 1
    fi
    if [ -f {tmux_switched} ]; then
      printf '$78\t%%78\t{replacement_pid}\n'
    else
      printf '$78\t%%78\t{captured_pid}\n'
    fi
    ;;
  show-environment)
    case "$4" in
      AGENT_SESSION_ID) printf 'AGENT_SESSION_ID=%s\n' {id} ;;
      AGENT_SESSION_STATE_DIR) printf 'AGENT_SESSION_STATE_DIR=%s\n' {state_dir} ;;
      AGENT_SESSION_RUNTIME_ID) printf 'AGENT_SESSION_RUNTIME_ID=%s\n' {launch_id} ;;
    esac
    ;;
  kill-session)
    /bin/kill -TERM -- -{captured_pid} 2>/dev/null || true
    : > {tmux_switched}
    : > {tmux_stopped}
    ;;
  if-shell)
    if [ ! -f {tmux_switched} ]; then
      /bin/kill -TERM -- -{captured_pid} 2>/dev/null || true
      : > {tmux_switched}
      printf 'agent-session-runtime-identity-changed\n'
    else
      : > {tmux_stopped}
    fi
    ;;
  has-session)
    if [ -f {tmux_stopped} ]; then
      printf "%s\n" "can't find session: $78" >&2
      exit 1
    fi
    ;;
  *) exit 42 ;;
esac
exit 0
"#,
            tmux_log = shell_words::quote(&tmux_log.to_string_lossy()),
            tmux_switched = shell_words::quote(&tmux_switched.to_string_lossy()),
            tmux_stopped = shell_words::quote(&tmux_stopped.to_string_lossy()),
            captured_pid = captured_pane.pid(),
            replacement_pid = replacement_pane.pid(),
            id = shell_words::quote(id),
            state_dir = shell_words::quote(&state_dir.to_string_lossy()),
            launch_id = shell_words::quote(&launch_id),
        ),
    );

    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_dir.to_string_lossy(),
            "delete",
            id,
            "--tmux-bin",
            &tmux_bin.to_string_lossy(),
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    #[cfg(target_os = "linux")]
    assert_eq!(
        output.stdout_json()["error"]["details"]["reason"],
        "runtime-identity-unavailable"
    );
    #[cfg(not(target_os = "linux"))]
    assert_eq!(
        output.stdout_json()["error"]["details"]["reason"],
        "process-still-running"
    );
    assert!(
        session_dir.exists(),
        "failed-closed delete must retain metadata"
    );
    #[cfg(target_os = "linux")]
    assert!(
        captured_pane.is_running(),
        "Linux must not mutate tmux before pinning a dedicated cgroup"
    );
    #[cfg(not(target_os = "linux"))]
    assert!(!captured_pane.is_running());
    assert!(replacement_pane.is_running());
    let calls = fs::read_to_string(&tmux_log).unwrap();
    #[cfg(target_os = "linux")]
    assert!(!calls.contains("if-shell -F -t %78"), "{calls}");
    #[cfg(not(target_os = "linux"))]
    assert!(calls.contains("if-shell -F -t %78"), "{calls}");
    assert!(
        tmux_calls(&tmux_log)
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "kill-session")),
        "{calls}"
    );

    captured_pane.stop();
    replacement_pane.stop();
}

#[test]
fn delete_transition_markers_converge_after_verified_shutdown() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let tmux_bin = tmp.path().join("tmux-stopped-transition");
    write_executable(
        &tmux_bin,
        r#"#!/usr/bin/env sh
case "$1" in
  display-message|has-session)
    printf "%s\n" "can't find session: $88" >&2
    exit 1 ;;
  *) exit 42 ;;
esac
"#,
    );

    for state in ["pending", "kill-confirmed"] {
        let id = format!("delete-transition-{state}");
        let tmux_session = format!("hs-codex-delete-transition-{state}");
        let session_dir = write_session_record(&state_dir, &id, "codex", &tmux_session);
        let runtime_files = attach_provider_runtime(
            tmp.path(),
            &state_dir,
            &session_dir,
            &id,
            "codex",
            &tmux_session,
        );
        let record_path = session_dir.join("session.json");
        let launch_id = seed_delete_tmux_identity(&record_path, "$88", "%88", 99_999_988);
        let mut record: Value =
            serde_json::from_slice(&fs::read(&record_path).expect("session record")).unwrap();
        record["delete_tmux_termination_state"] = json!({
            "launch_id": launch_id,
            "state": state,
        });
        record["provider_resume"] = json!({
            "provider": "codex",
            "session_id": "resume-session-id",
            "captured_at": "2000-01-01T00:00:00Z",
            "capture_method": "fixture",
            "resume_args": [
                "resume",
                "resume-session-id",
                "--cd",
                "/tmp",
                "--no-alt-screen"
            ],
        });
        record["agent_args"] = json!([]);
        fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

        let delete = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_dir.to_string_lossy(),
                "delete",
                &id,
                "--tmux-bin",
                &tmux_bin.to_string_lossy(),
                "--format",
                "json",
            ],
            &[],
        );

        assert_eq!(delete.code, 0, "stdout={}", delete.stdout_text());
        assert!(
            !session_dir.exists(),
            "a fully verified stopped runtime must converge from {state}"
        );
        assert_eq!(
            runtime_files.iter().filter(|path| path.exists()).count(),
            0,
            "verified stopped runtime files must be removed"
        );
    }
}

#[test]
fn delete_rejects_malformed_or_mismatched_transition_markers() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let tmux_bin = tmp.path().join("tmux-stopped-invalid-transition");
    write_executable(
        &tmux_bin,
        r#"#!/usr/bin/env sh
case "$1" in
  display-message|has-session)
    printf "%s\n" "can't find session: $89" >&2
    exit 1 ;;
  *) exit 42 ;;
esac
"#,
    );

    for case_name in ["malformed", "mismatched"] {
        let id = format!("delete-transition-{case_name}");
        let tmux_session = format!("hs-claude-delete-transition-{case_name}");
        let session_dir = write_session_record(&state_dir, &id, "claude", &tmux_session);
        let runtime_files = attach_provider_runtime(
            tmp.path(),
            &state_dir,
            &session_dir,
            &id,
            "claude",
            &tmux_session,
        );
        let record_path = session_dir.join("session.json");
        let launch_id = seed_delete_tmux_identity(&record_path, "$89", "%89", 99_999_989);
        let mut record: Value =
            serde_json::from_slice(&fs::read(&record_path).expect("session record")).unwrap();
        record["delete_tmux_termination_state"] = match case_name {
            "malformed" => json!({"launch_id": launch_id, "state": "unknown"}),
            "mismatched" => {
                json!({"launch_id": "other-launch", "state": "kill-confirmed"})
            }
            _ => unreachable!(),
        };
        fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        let before = fs::read(&record_path).unwrap();

        let delete = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_dir.to_string_lossy(),
                "delete",
                &id,
                "--tmux-bin",
                &tmux_bin.to_string_lossy(),
                "--format",
                "json",
            ],
            &[],
        );

        assert_eq!(delete.code, 1, "stdout={}", delete.stdout_text());
        let error = delete.stdout_json();
        assert_eq!(
            error["error"]["details"]["reason"],
            "runtime-identity-unavailable"
        );
        assert_eq!(error["error"]["details"]["retryable"], false);
        assert_eq!(fs::read(&record_path).unwrap(), before);
        assert!(session_dir.exists());
        assert!(runtime_files.iter().all(|path| path.exists()));
    }
}

#[test]
fn delete_exhausts_bounded_identity_churn_without_losing_current_runtime() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let tmux_bin = tmp.path().join("tmux-repeated-identity-churn");
    let tmux_log = tmp.path().join("tmux-repeated-identity-churn.log");
    let tmux_generation = tmp.path().join("tmux-repeated-identity-churn.generation");
    let id = "repeated-identity-churn";
    let tmux_session = "hs-codex-repeated-identity-churn";
    let session_dir = write_session_record(&state_dir, id, "codex", tmux_session);
    attach_provider_runtime(
        tmp.path(),
        &state_dir,
        &session_dir,
        id,
        "codex",
        tmux_session,
    );
    let record_path = session_dir.join("session.json");
    let panes = [
        spawn_test_process_group(),
        spawn_test_process_group(),
        spawn_test_process_group(),
        spawn_test_process_group(),
    ];
    let launch_id = seed_delete_tmux_identity(&record_path, "$90", "%90", panes[0].pid());
    fs::write(&tmux_generation, b"0").unwrap();
    write_executable(
        &tmux_bin,
        &format!(
            r#"#!/usr/bin/env sh
printf '%s\n' "$*" >> {tmux_log}
generation=$(cat {tmux_generation})
case "$generation" in
  0) pane_pid={pane_0} ;;
  1) pane_pid={pane_1} ;;
  2) pane_pid={pane_2} ;;
  *) pane_pid={pane_3} ;;
esac
case "$1" in
  display-message) printf '$90\t%%90\t%s\n' "$pane_pid" ;;
  show-environment)
    case "$4" in
      AGENT_SESSION_ID) printf 'AGENT_SESSION_ID=%s\n' {id} ;;
      AGENT_SESSION_STATE_DIR) printf 'AGENT_SESSION_STATE_DIR=%s\n' {state_dir} ;;
      AGENT_SESSION_RUNTIME_ID) printf 'AGENT_SESSION_RUNTIME_ID=%s\n' {launch_id} ;;
    esac ;;
  if-shell)
    /bin/kill -TERM -- "-$pane_pid" 2>/dev/null || true
    printf '%s\n' "$((generation + 1))" > {tmux_generation}
    printf 'agent-session-runtime-identity-changed\n' ;;
  has-session) exit 0 ;;
  *) exit 42 ;;
esac
exit 0
"#,
            tmux_log = shell_words::quote(&tmux_log.to_string_lossy()),
            tmux_generation = shell_words::quote(&tmux_generation.to_string_lossy()),
            pane_0 = panes[0].pid(),
            pane_1 = panes[1].pid(),
            pane_2 = panes[2].pid(),
            pane_3 = panes[3].pid(),
            id = shell_words::quote(id),
            state_dir = shell_words::quote(&state_dir.to_string_lossy()),
            launch_id = shell_words::quote(&launch_id),
        ),
    );

    let delete = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_dir.to_string_lossy(),
            "delete",
            id,
            "--tmux-bin",
            &tmux_bin.to_string_lossy(),
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(delete.code, 1, "stdout={}", delete.stdout_text());
    let error = delete.stdout_json();
    #[cfg(target_os = "linux")]
    {
        assert_eq!(
            error["error"]["details"]["reason"],
            "runtime-identity-unavailable"
        );
        assert_eq!(error["error"]["details"]["retryable"], false);
        assert_eq!(
            error["error"]["details"]["action"],
            "manual-runtime-verification-required"
        );
    }
    #[cfg(not(target_os = "linux"))]
    {
        assert_eq!(
            error["error"]["details"]["reason"],
            "runtime-identity-changed"
        );
        assert_eq!(error["error"]["details"]["retryable"], true);
        assert_eq!(error["error"]["details"]["action"], "retry-delete");
    }
    assert!(session_dir.exists());
    #[cfg(target_os = "linux")]
    assert!(
        panes[0].is_running(),
        "Linux must not enter identity churn without a dedicated cgroup"
    );
    assert!(
        panes[3].is_running(),
        "latest observed group must remain live"
    );
    let retained: Value =
        serde_json::from_slice(&fs::read(&record_path).expect("retained record")).unwrap();
    #[cfg(target_os = "linux")]
    assert_eq!(retained["delete_tmux_identity"]["pane_pid"], panes[0].pid());
    #[cfg(not(target_os = "linux"))]
    assert_eq!(retained["delete_tmux_identity"]["pane_pid"], panes[3].pid());
    assert!(retained.get("delete_tmux_termination_state").is_none());
    let calls = fs::read_to_string(&tmux_log).unwrap();
    #[cfg(target_os = "linux")]
    assert_eq!(calls.matches("if-shell -F -t %90").count(), 0, "{calls}");
    #[cfg(not(target_os = "linux"))]
    assert_eq!(calls.matches("if-shell -F -t %90").count(), 3, "{calls}");
}

#[test]
fn delete_fails_closed_when_a_pane_survives_without_a_control_group() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let pane = spawn_test_process_group();
    let pane_pid = pane.pid();
    let killed = tmp.path().join("tmux-killed");
    let tmux_bin = tmp.path().join("tmux-pane-survives");
    let id = "pane-survives";
    let tmux_session = "hs-claude-pane-survives";
    let session_dir = write_session_record(&state_dir, id, "claude", tmux_session);
    attach_provider_runtime(
        tmp.path(),
        &state_dir,
        &session_dir,
        id,
        "claude",
        tmux_session,
    );
    let record: Value = serde_json::from_str(
        &fs::read_to_string(session_dir.join("session.json")).expect("session record"),
    )
    .expect("session json");
    let runtime_id = record["runtime"]["launch_id"].as_str().unwrap();
    write_executable(
        &tmux_bin,
        &format!(
            r#"#!/usr/bin/env sh
case "$1" in
  display-message) printf '$92\t%%92\t{pane_pid}\n'; exit 0 ;;
  show-environment)
    case "$4" in
      AGENT_SESSION_ID) printf 'AGENT_SESSION_ID={id}\n' ;;
      AGENT_SESSION_STATE_DIR) printf 'AGENT_SESSION_STATE_DIR={}\n' ;;
      AGENT_SESSION_RUNTIME_ID) printf 'AGENT_SESSION_RUNTIME_ID={runtime_id}\n' ;;
    esac
    exit 0 ;;
  has-session) if [ -f {} ]; then printf "%s\n" "can't find session: \$92" >&2; exit 1; fi; exit 0 ;;
  if-shell) : > {}; exit 0 ;;
esac
exit 42
"#,
            state_dir.display(),
            killed.display(),
            killed.display(),
        ),
    );
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_dir.to_string_lossy(),
            "delete",
            id,
            "--tmux-bin",
            &tmux_bin.to_string_lossy(),
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    #[cfg(target_os = "linux")]
    assert_eq!(
        output.stdout_json()["error"]["details"]["reason"],
        "runtime-identity-unavailable"
    );
    #[cfg(not(target_os = "linux"))]
    assert_eq!(
        output.stdout_json()["error"]["details"]["reason"],
        "process-still-running"
    );
    assert!(session_dir.exists());
    assert!(pane.is_running());
    #[cfg(target_os = "linux")]
    assert!(
        !killed.is_file(),
        "Linux must not mutate tmux without a dedicated cgroup"
    );
}

#[test]
fn start_rejects_claude_resume_identity_agent_args() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let claude_bin = fake_agent(tmp.path(), "claude");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let claude_arg = claude_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "claude",
            "--cwd",
            &cwd_arg,
            "--id",
            "bad-claude-arg",
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &claude_arg,
            "--agent-arg=--session-id",
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg)],
    );

    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_json()["error"]["code"], "reserved-agent-arg");
    assert!(
        !state_dir.join("sessions/bad-claude-arg").exists(),
        "invalid managed identity args must be rejected before state creation"
    );
    assert!(
        tmux_calls(&tmux_log).is_empty(),
        "invalid managed identity args must not start tmux"
    );
}

#[test]
fn start_rejects_claude_resume_identity_agent_arg_aliases() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let claude_bin = fake_agent(tmp.path(), "claude");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let claude_arg = claude_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    for (index, reserved_arg) in ["-r", "-r=other-session", "-c"].iter().enumerate() {
        let session_id = format!("bad-claude-alias-{index}");
        let output = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "start",
                "--agent",
                "claude",
                "--cwd",
                &cwd_arg,
                "--id",
                &session_id,
                "--tmux-bin",
                &tmux_arg,
                "--agent-bin",
                &claude_arg,
                &format!("--agent-arg={reserved_arg}"),
                "--format",
                "json",
            ],
            &[("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg)],
        );

        assert_eq!(
            output.code,
            64,
            "reserved_arg={reserved_arg}, stderr={}",
            output.stderr_text()
        );
        assert_eq!(output.stdout_json()["error"]["code"], "reserved-agent-arg");
        assert!(
            !state_dir.join("sessions").join(&session_id).exists(),
            "invalid managed identity alias must be rejected before state creation"
        );
    }
    assert!(
        tmux_calls(&tmux_log).is_empty(),
        "invalid managed identity aliases must not start tmux"
    );
}

#[test]
fn run_and_logs_cover_json_contract_and_file_fallback() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let secret = "secret-run-prompt";
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let env_refs = [("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str())];

    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "run",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--prompt",
            secret,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--format",
            "json",
        ],
        &env_refs,
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_no_secret(&output, secret);
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.run.v1");
    let result = data(&value);
    assert_eq!(result["mode"], "run");
    let id = result["id"].as_str().expect("id");
    let log_file = PathBuf::from(result["log_file"].as_str().expect("log_file"));

    let calls = tmux_calls(&tmux_log);
    let new_session = calls
        .iter()
        .find(|call| call.first().is_some_and(|arg| arg == "new-session"))
        .expect("new-session call");
    let script = new_session.last().expect("script");
    assert!(
        script.contains("$(cat "),
        "script should read prompt file: {script}"
    );
    assert!(
        script.contains(&log_file.to_string_lossy().to_string()),
        "script should redirect to log file: {script}"
    );
    assert!(
        !script.contains(secret),
        "run script must not inline prompt text: {script}"
    );

    fs::write(&log_file, "alpha\nbeta\ngamma\n").expect("write log file");
    let logs_from_file = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "logs",
            id,
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &env_refs,
    );
    assert_eq!(
        logs_from_file.code,
        0,
        "stderr={}",
        logs_from_file.stderr_text()
    );
    let file_logs_json = logs_from_file.stdout_json();
    let file_logs = data(&file_logs_json);
    assert_eq!(file_logs["source"], "file");
    assert_eq!(file_logs["text"], "alpha\nbeta\ngamma\n");

    let envs = [
        (
            "AGENT_SESSION_FAKE_TMUX_LOG",
            tmux_log.to_string_lossy().to_string(),
        ),
        ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0".to_string()),
    ];
    let env_refs = [
        (envs[0].0, envs[0].1.as_str()),
        (envs[1].0, envs[1].1.as_str()),
    ];
    let logs_from_file = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "logs",
            id,
            "--tail",
            "2",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &env_refs,
    );
    assert_eq!(
        logs_from_file.code,
        0,
        "stderr={}",
        logs_from_file.stderr_text()
    );
    let file_logs_json = logs_from_file.stdout_json();
    let file_logs = data(&file_logs_json);
    assert_eq!(file_logs["source"], "file");
    assert_eq!(file_logs["text"], "beta\ngamma\n");
}

#[test]
fn logs_fall_back_to_a_retained_startup_diagnostic_for_a_stopped_session() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let session_dir = write_session_record(
        &state_dir,
        "failed-startup-diagnostic",
        "codex",
        "hs-codex-failed-startup-diagnostic",
    );
    fs::write(
        session_dir.join(".startup-diagnostic.log"),
        "provider failed\nretry after sign-in\n",
    )
    .expect("startup diagnostic");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "logs",
            "failed-startup-diagnostic",
            "--tail",
            "1",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );

    assert_eq!(output.code, 0, "stdout={}", output.stdout_text());
    let logs = output.stdout_json();
    assert_eq!(data(&logs)["source"], "diagnostic");
    assert_eq!(data(&logs)["text"], "retry after sign-in\n");

    fs::write(
        session_dir.join(".startup-diagnostic.log"),
        [vec![0x80], b"capped diagnostic\n".to_vec()].concat(),
    )
    .expect("split UTF-8 diagnostic tail");
    let split_utf8 = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "logs",
            "failed-startup-diagnostic",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(split_utf8.code, 0, "stdout={}", split_utf8.stdout_text());
    let logs = split_utf8.stdout_json();
    assert_eq!(data(&logs)["source"], "diagnostic");
    assert_eq!(data(&logs)["text"], "�capped diagnostic\n");

    fs::remove_file(session_dir.join(".startup-diagnostic.log"))
        .expect("remove startup diagnostic");
    fs::write(session_dir.join(".runtime-exit-status"), "17\n").expect("runtime exit status");
    let status_only = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "logs",
            "failed-startup-diagnostic",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(status_only.code, 0, "stdout={}", status_only.stdout_text());
    let logs = status_only.stdout_json();
    assert_eq!(data(&logs)["source"], "diagnostic");
    assert_eq!(
        data(&logs)["text"],
        "provider client exited with status 17\n"
    );
}

#[test]
fn failure_paths_return_json_without_leaking_prompt_and_classify_durable_startup() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let secret = "secret-start-prompt";
    let envs = [
        (
            "AGENT_SESSION_FAKE_TMUX_LOG",
            tmux_log.to_string_lossy().to_string(),
        ),
        ("AGENT_SESSION_FAKE_TMUX_FAIL", "new-session".to_string()),
        ("AGENT_SESSION_TMUX_BIN", tmux_arg.clone()),
    ];
    let env_refs = [
        (envs[0].0, envs[0].1.as_str()),
        (envs[1].0, envs[1].1.as_str()),
        (envs[2].0, envs[2].1.as_str()),
    ];

    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--id",
            "fail-start",
            "--prompt",
            secret,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--format",
            "json",
        ],
        &env_refs,
    );
    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    assert_no_secret(&output, secret);
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.start.v1");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "command-failed");
    assert!(
        state_dir.join("sessions").join("fail-start").exists(),
        "post-record tmux failure should remain a durable stopped session"
    );
    let list_env_refs = [
        env_refs[0],
        env_refs[1],
        env_refs[2],
        ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
    ];
    let list = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &list_env_refs,
    );
    assert_eq!(list.code, 0, "stderr={}", list.stderr_text());
    let list_value = list.stdout_json();
    let listed = &data(&list_value)[0];
    assert_eq!(listed["status"], "stopped");
    assert_eq!(listed["startup"]["state"], "failed");
    assert_eq!(listed["startup"]["stage"], "tmux");
    assert_eq!(
        listed["startup"]["failure_code"],
        "terminal-runtime-create-failed"
    );

    let envs = [
        (
            "AGENT_SESSION_FAKE_TMUX_LOG",
            tmux_log.to_string_lossy().to_string(),
        ),
        ("AGENT_SESSION_FAKE_TMUX_FAIL", "paste-buffer".to_string()),
    ];
    let env_refs = [
        (envs[0].0, envs[0].1.as_str()),
        (envs[1].0, envs[1].1.as_str()),
    ];
    let paste_fail = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--id",
            "paste-fail",
            "--prompt",
            secret,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &env_refs,
    );
    assert_eq!(paste_fail.code, 1, "stderr={}", paste_fail.stderr_text());
    assert_no_secret(&paste_fail, secret);
    assert!(
        !state_dir.join("sessions").join("paste-fail").exists(),
        "failed prompt paste should remove session state"
    );
    let calls = tmux_calls(&tmux_log);
    assert!(
        calls.iter().any(|call| {
            call.first().is_some_and(|arg| arg == "if-shell")
                && call.get(3).is_some_and(|arg| arg == "%77")
                && call.get(5).is_some_and(|arg| arg == "kill-session -t $77")
        }),
        "failed prompt paste should kill the orphaned tmux session: {calls:?}"
    );

    let missing_prompt = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "run",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--format",
            "json",
        ],
        &env_refs,
    );
    assert_eq!(missing_prompt.code, 64);
    let value = missing_prompt.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.run.v1");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "missing-prompt");
}

#[test]
fn parse_context_data_and_session_reference_errors_follow_contract() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let state_arg = state_dir.to_string_lossy().to_string();

    let parse = run(tmp.path(), &["list", "--format", "json", "--bad-flag"], &[]);
    assert_eq!(parse.code, 64);
    let parse_json = parse.stdout_json();
    assert_eq!(parse_json["schema_version"], "cli.agent-session.error.v1");
    assert_eq!(parse_json["ok"], false);
    assert_eq!(parse_json["error"]["code"], "parse-error");

    let unknown = run(tmp.path(), &["nope", "--format", "json"], &[]);
    assert_eq!(unknown.code, 64);
    let unknown_json = unknown.stdout_json();
    assert_eq!(unknown_json["schema_version"], "cli.agent-session.error.v1");
    assert_eq!(unknown_json["error"]["code"], "unknown-subcommand");

    let invalid_host = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "--host=-oProxyCommand=bad",
            "list",
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(invalid_host.code, 64);
    let host_json = invalid_host.stdout_json();
    assert_eq!(host_json["schema_version"], "cli.agent-session.error.v1");
    assert_eq!(host_json["error"]["code"], "invalid-host");

    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).expect("outside dir");
    for args in [
        vec![
            "--state-dir",
            &state_arg,
            "command",
            "../outside",
            "--format",
            "json",
        ],
        vec![
            "--state-dir",
            &state_arg,
            "logs",
            "../outside",
            "--format",
            "json",
        ],
        vec![
            "--state-dir",
            &state_arg,
            "delete",
            "../outside",
            "--format",
            "json",
        ],
    ] {
        let output = run(tmp.path(), &args, &[]);
        assert_eq!(output.code, 64, "args={args:?}");
        let value = output.stdout_json();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "invalid-session-id");
    }
    let attach = run(
        tmp.path(),
        &["--state-dir", &state_arg, "attach", "../outside"],
        &[],
    );
    assert_eq!(attach.code, 64);
    assert!(attach.stderr_text().contains("session id may contain only"));
    assert!(
        outside.exists(),
        "invalid delete id must not remove outside dir"
    );

    let bad_session = state_dir.join("sessions").join("bad");
    fs::create_dir_all(&bad_session).expect("bad session dir");
    fs::write(bad_session.join("session.json"), "{not json").expect("bad json");
    let data_error = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "command",
            "bad",
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(data_error.code, 65);
    let value = data_error.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.command.v1");
    assert_eq!(value["error"]["code"], "session-json-invalid");

    let sessions_root = state_dir.join("sessions");
    let victim_dir = sessions_root.join("victim");
    let alias_dir = sessions_root.join("alias");
    fs::create_dir_all(&victim_dir).expect("victim session dir");
    fs::create_dir_all(&alias_dir).expect("alias session dir");
    fs::write(
        victim_dir.join("session.json"),
        r#"{
  "schema_version": "agent-session.session.v1",
  "id": "victim",
  "agent": "codex",
  "mode": "interactive",
  "title": null,
  "cwd": "/tmp",
  "tmux_session": "hs-codex-victim",
  "prompt_file": null,
  "log_file": null,
  "created_at": "2026-07-02T00:00:00Z",
  "updated_at": "2026-07-02T00:00:00Z"
}"#,
    )
    .expect("victim session record");
    symlink(
        victim_dir.join("session.json"),
        alias_dir.join("session.json"),
    )
    .expect("alias session symlink");
    let alias = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "command",
            "alias",
            "--format",
            "json",
        ],
        &[],
    );
    assert_eq!(alias.code, 64);
    let value = alias.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.command.v1");
    assert_eq!(value["error"]["code"], "session-path-escaped");
}

fn write_session_record(dir: &Path, id: &str, agent: &str, tmux_session: &str) -> PathBuf {
    write_session_record_with_cwd(dir, id, agent, tmux_session, Path::new("/tmp"))
}

fn attach_provider_runtime(
    tmp: &Path,
    state_dir: &Path,
    session_dir: &Path,
    id: &str,
    agent: &str,
    tmux_session: &str,
) -> Vec<PathBuf> {
    let record_path = session_dir.join("session.json");
    let mut record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
    let launch_id = format!("launch-{id}");
    let runtime_files = if agent == "codex" {
        let mut digest = Sha256::new();
        digest.update(state_dir.as_os_str().as_encoded_bytes());
        digest.update([0]);
        digest.update(id.as_bytes());
        digest.update([0]);
        digest.update(launch_id.as_bytes());
        let namespace: String = digest
            .finalize()
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let runtime_dir = tmp.join("runtime/agent-session");
        fs::create_dir_all(&runtime_dir).expect("Codex runtime dir");
        let socket = runtime_dir.join(format!("cx-{namespace}.sock"));
        let files = vec![
            socket.clone(),
            socket.with_extension("proxy"),
            socket.with_extension("thread"),
            socket.with_extension("attached"),
        ];
        record["runtime"] = json!({
            "kind": "codex_app_server",
            "tmux_session": tmux_session,
            "generation": 1,
            "started_at": "2000-01-01T00:00:00Z",
            "launch_id": launch_id,
            "codex_app_server_protocol": "v2",
            "codex_app_server_socket": files[0],
            "codex_app_server_proxy": files[1],
            "codex_app_server_thread_handoff": files[2],
            "codex_app_server_thread_attached": files[3],
        });
        files
    } else {
        let runtime_file = session_dir.join("provider-runtime.json");
        record["runtime"] = json!({
            "kind": "tmux",
            "tmux_session": tmux_session,
            "generation": 1,
            "started_at": "2000-01-01T00:00:00Z",
            "launch_id": launch_id,
            "provider_runtime_file": runtime_file,
        });
        vec![runtime_file]
    };
    for runtime_file in &runtime_files {
        fs::write(runtime_file, format!("runtime metadata for {agent}")).expect("runtime metadata");
    }
    fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).expect("session record");
    runtime_files
}

fn seed_delete_tmux_identity(
    record_path: &Path,
    session_id: &str,
    pane_id: &str,
    pane_pid: u32,
) -> String {
    let mut record: Value =
        serde_json::from_slice(&fs::read(record_path).expect("session record")).unwrap();
    let launch_id = record["runtime"]["launch_id"]
        .as_str()
        .expect("runtime launch id")
        .to_string();
    record["delete_tmux_identity"] = json!({
        "launch_id": launch_id,
        "session_id": session_id,
        "pane_id": pane_id,
        "pane_pid": pane_pid,
        "process_group_id": pane_pid,
    });
    fs::write(record_path, serde_json::to_vec_pretty(&record).unwrap())
        .expect("seed delete tmux identity");
    launch_id
}

fn write_session_record_with_cwd(
    dir: &Path,
    id: &str,
    agent: &str,
    tmux_session: &str,
    cwd: &Path,
) -> PathBuf {
    let session = dir.join("sessions").join(id);
    fs::create_dir_all(&session).expect("session dir");
    let cwd = cwd.to_string_lossy();
    let record = format!(
        r#"{{
  "schema_version": "agent-session.session.v1",
  "id": "{id}",
  "agent": "{agent}",
  "mode": "interactive",
  "title": null,
  "cwd": "{cwd}",
  "tmux_session": "{tmux_session}",
  "prompt_file": null,
  "log_file": null,
  "created_at": "2000-01-01T00:00:00Z",
  "updated_at": "2000-01-01T00:00:00Z"
}}"#
    );
    fs::write(session.join("session.json"), record).expect("session record");
    session
}

fn write_codex_session_meta(path: &Path, session_id: &str, cwd: &Path, timestamp: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("codex sessions");
    let line = json!({
        "timestamp": timestamp,
        "type": "session_meta",
        "payload": {
            "id": session_id,
            "session_id": session_id,
            "cwd": cwd.to_string_lossy().to_string(),
            "source": "cli",
            "timestamp": timestamp,
        },
    });
    fs::write(path, format!("{line}\n")).expect("codex session metadata");
}

fn write_resumable_session_record(
    dir: &Path,
    id: &str,
    agent: &str,
    tmux_session: &str,
    cwd: &Path,
    resume_args: &[&str],
) -> PathBuf {
    write_resumable_session_record_with_agent_bin(
        dir,
        id,
        agent,
        tmux_session,
        cwd,
        resume_args,
        None,
    )
}

fn write_resumable_session_record_with_agent_bin(
    dir: &Path,
    id: &str,
    agent: &str,
    tmux_session: &str,
    cwd: &Path,
    resume_args: &[&str],
    agent_bin: Option<&Path>,
) -> PathBuf {
    let session = dir.join("sessions").join(id);
    fs::create_dir_all(&session).expect("session dir");
    let resume_args = serde_json::to_string(resume_args).expect("resume args json");
    let cwd = cwd.to_string_lossy();
    let agent_bin_json = agent_bin
        .map(|path| {
            format!(
                r#",
  "agent_bin": "{}""#,
                path.to_string_lossy()
            )
        })
        .unwrap_or_default();
    let record = format!(
        r#"{{
  "schema_version": "agent-session.session.v1",
  "id": "{id}",
  "agent": "{agent}",
  "mode": "interactive",
  "title": "Recover me",
  "cwd": "{cwd}",
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
    "started_at": "2000-01-01T00:00:00Z",
    "launch_id": "never-launched-fixture"
  }},
  "tmux_runtime_never_launched": "never-launched-fixture",
  "startup": {{
    "schema_version": "agent-session.startup.v1",
    "state": "failed",
    "stage": "tmux",
    "started_at": "2000-01-01T00:00:00Z",
    "failure_code": "terminal-runtime-create-failed",
    "message": "The terminal runtime could not be created.",
    "occurred_at": "2000-01-01T00:00:01Z",
    "retry_safe": true
  }},
  "agent_args": ["--model", "fixture-model"]
  {agent_bin_json}
}}"#
    );
    fs::write(session.join("session.json"), record).expect("session record");
    session
}

fn add_provider_resume_extra(session: &Path) {
    let record_path = session.join("session.json");
    let mut record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    record["provider_resume"]["storage_only"] = json!({ "keep": true });
    fs::write(&record_path, serde_json::to_string_pretty(&record).unwrap())
        .expect("rewrite session record");
}

fn write_resume_sidecar(
    session: &Path,
    agent: &str,
    tmux_session: &str,
    agent_bin: &Path,
    resume_args: &[&str],
) {
    let resume_args = serde_json::to_string(resume_args).expect("resume args json");
    fs::write(
        session.join("resume.json"),
        format!(
            r#"{{
  "schema_version": "agent-session.resume.v1",
  "provider_resume": {{
    "provider": "{agent}",
    "session_id": "resume-session-id",
    "captured_at": "2000-01-01T00:00:00Z",
    "capture_method": "fixture-sidecar",
    "resume_args": {resume_args}
  }},
  "runtime": {{
    "kind": "tmux",
    "tmux_session": "{tmux_session}",
    "generation": 3,
    "started_at": "2000-01-01T00:00:00Z"
  }},
  "agent_args": ["--model", "sidecar-model"],
  "agent_bin": "{}"
}}"#,
            agent_bin.to_string_lossy()
        ),
    )
    .expect("resume sidecar");
}

#[test]
fn start_captures_codex_resume_metadata_from_unique_post_launch_session_meta() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let codex_session = codex_home.join("sessions/2026/07/05/session.jsonl");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let codex_session_arg = codex_session.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--title",
            "Capture Codex",
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_FILE", &codex_session_arg),
            (
                "AGENT_SESSION_FAKE_CODEX_SESSION_ID",
                "codex-post-launch-id",
            ),
            ("AGENT_SESSION_FAKE_CODEX_CWD", &cwd_arg),
            ("AGENT_SESSION_CODEX_CAPTURE_TIMEOUT_MS", "250"),
            ("AGENT_SESSION_CODEX_CAPTURE_POLL_MS", "10"),
            ("AGENT_SESSION_CODEX_AMBIGUITY_WINDOW_MS", "40"),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["resumable"], true);
    let id = result["id"].as_str().expect("session id");
    let record_path = state_dir.join("sessions").join(id).join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert_eq!(record["provider_resume"]["provider"], "codex");
    assert_eq!(
        record["provider_resume"]["resume_args"],
        serde_json::json!([
            "resume",
            "codex-post-launch-id",
            "--cd",
            cwd_arg,
            "--no-alt-screen"
        ])
    );
    assert_eq!(record["agent_bin"], codex_arg);
    assert!(
        record_path.with_file_name("resume.json").is_file(),
        "durable resume sidecar should be written"
    );
}

#[test]
fn start_does_not_capture_when_codex_session_metadata_is_absent() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    fs::create_dir_all(codex_home.join("sessions")).expect("codex sessions");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_CODEX_CAPTURE_TIMEOUT_MS", "25"),
            ("AGENT_SESSION_CODEX_CAPTURE_POLL_MS", "5"),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["resumable"], false);
    let id = result["id"].as_str().expect("session id");
    let record_path = state_dir.join("sessions").join(id).join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert!(record.get("provider_resume").is_none());
    assert!(!record_path.with_file_name("resume.json").exists());
}

#[test]
fn start_does_not_capture_prelaunch_codex_session_meta() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    let codex_session = codex_home.join("sessions/2026/07/05/prelaunch.jsonl");
    fs::create_dir_all(codex_session.parent().expect("parent")).expect("codex sessions");
    fs::create_dir_all(&cwd).expect("repo dir");
    fs::write(
        &codex_session,
        format!(
            r#"{{"timestamp":"2000-01-01T00:00:00Z","type":"session_meta","payload":{{"id":"stale-codex-id","cwd":"{}","source":"cli","timestamp":"2000-01-01T00:00:00Z"}}}}"#,
            cwd.to_string_lossy()
        ),
    )
    .expect("stale codex metadata");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_CODEX_CAPTURE_TIMEOUT_MS", "25"),
            ("AGENT_SESSION_CODEX_CAPTURE_POLL_MS", "5"),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["resumable"], false);
    let id = result["id"].as_str().expect("session id");
    let record_path = state_dir.join("sessions").join(id).join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert!(record.get("provider_resume").is_none());
    assert!(
        !record_path.with_file_name("resume.json").exists(),
        "stale pre-launch metadata must not create a resume sidecar"
    );
}

#[test]
fn start_does_not_capture_ambiguous_post_launch_codex_session_meta() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let codex_one = codex_home.join("sessions/2026/07/05/one.jsonl");
    let codex_two = codex_home.join("sessions/2026/07/05/two.jsonl");
    let codex_files = format!("{}:{}", codex_one.display(), codex_two.display());

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_FILE", &codex_files),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_ID", "codex-ambiguous-id"),
            ("AGENT_SESSION_FAKE_CODEX_CWD", &cwd_arg),
            ("AGENT_SESSION_CODEX_CAPTURE_TIMEOUT_MS", "25"),
            ("AGENT_SESSION_CODEX_CAPTURE_POLL_MS", "5"),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["resumable"], false);
    let id = result["id"].as_str().expect("session id");
    let record_path = state_dir.join("sessions").join(id).join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert!(record.get("provider_resume").is_none());
    assert!(!record_path.with_file_name("resume.json").exists());
}

#[test]
fn start_does_not_capture_transient_singleton_codex_session_meta() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let other_session = codex_home.join("sessions/2026/07/05/other.jsonl");
    let own_session = codex_home.join("sessions/2026/07/05/own.jsonl");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let other_session_arg = other_session.to_string_lossy().to_string();
    let delayed_cwd = cwd_arg.clone();
    let delayed_writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::create_dir_all(own_session.parent().expect("parent")).expect("codex sessions");
        fs::write(
            &own_session,
            format!(
                r#"{{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{{"id":"own-codex-id","session_id":"own-codex-id","cwd":"{}","source":"cli","timestamp":"2099-01-01T00:00:00Z"}}}}"#,
                delayed_cwd
            ),
        )
        .expect("delayed codex metadata");
    });
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_FILE", &other_session_arg),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_ID", "other-codex-id"),
            ("AGENT_SESSION_FAKE_CODEX_CWD", &cwd_arg),
            ("AGENT_SESSION_CODEX_CAPTURE_TIMEOUT_MS", "1000"),
            ("AGENT_SESSION_CODEX_CAPTURE_POLL_MS", "10"),
            ("AGENT_SESSION_CODEX_AMBIGUITY_WINDOW_MS", "800"),
        ],
    );
    delayed_writer.join().expect("delayed writer");

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["resumable"], false);
    let id = result["id"].as_str().expect("session id");
    let record_path = state_dir.join("sessions").join(id).join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert!(record.get("provider_resume").is_none());
    assert!(!record_path.with_file_name("resume.json").exists());
}

#[test]
fn start_captures_stable_codex_session_meta_before_full_timeout() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let codex_session = codex_home.join("sessions/2026/07/05/stable.jsonl");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let codex_session_arg = codex_session.to_string_lossy().to_string();
    let started = std::time::Instant::now();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_FILE", &codex_session_arg),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_ID", "stable-codex-id"),
            ("AGENT_SESSION_FAKE_CODEX_CWD", &cwd_arg),
            ("AGENT_SESSION_CODEX_CAPTURE_TIMEOUT_MS", "1000"),
            ("AGENT_SESSION_CODEX_CAPTURE_POLL_MS", "10"),
            ("AGENT_SESSION_CODEX_AMBIGUITY_WINDOW_MS", "40"),
        ],
    );
    let elapsed = started.elapsed();

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(
        elapsed < std::time::Duration::from_millis(750),
        "stable capture should not wait for full timeout; elapsed={elapsed:?}"
    );
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["resumable"], true);
}

#[test]
fn start_does_not_capture_codex_singleton_before_ambiguity_window() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let codex_session = codex_home.join("sessions/2026/07/05/short-timeout.jsonl");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let codex_session_arg = codex_session.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_FILE", &codex_session_arg),
            (
                "AGENT_SESSION_FAKE_CODEX_SESSION_ID",
                "short-timeout-codex-id",
            ),
            ("AGENT_SESSION_FAKE_CODEX_CWD", &cwd_arg),
            ("AGENT_SESSION_CODEX_CAPTURE_TIMEOUT_MS", "40"),
            ("AGENT_SESSION_CODEX_CAPTURE_POLL_MS", "10"),
            ("AGENT_SESSION_CODEX_AMBIGUITY_WINDOW_MS", "200"),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["resumable"], false);
    let id = result["id"].as_str().expect("session id");
    let record_path = state_dir.join("sessions").join(id).join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert!(record.get("provider_resume").is_none());
    assert!(!record_path.with_file_name("resume.json").exists());
}

#[test]
fn start_does_not_capture_old_codex_session_meta_appended_after_launch() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    let codex_session = codex_home.join("sessions/2026/07/05/old.jsonl");
    fs::create_dir_all(codex_session.parent().expect("parent")).expect("codex sessions");
    fs::create_dir_all(&cwd).expect("repo dir");
    fs::write(
        &codex_session,
        format!(
            r#"{{"timestamp":"2000-01-01T00:00:00Z","type":"session_meta","payload":{{"id":"old-codex-id","cwd":"{}","source":"cli","timestamp":"2000-01-01T00:00:00Z"}}}}
"#,
            cwd.to_string_lossy()
        ),
    )
    .expect("old codex metadata");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let codex_session_arg = codex_session.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_FILE", &codex_session_arg),
            ("AGENT_SESSION_FAKE_CODEX_APPEND", "1"),
            (
                "AGENT_SESSION_FAKE_CODEX_SESSION_TIMESTAMP",
                "2099-01-01T00:00:00Z",
            ),
            ("AGENT_SESSION_CODEX_CAPTURE_TIMEOUT_MS", "25"),
            ("AGENT_SESSION_CODEX_CAPTURE_POLL_MS", "5"),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["resumable"], false);
    let id = result["id"].as_str().expect("session id");
    let record_path = state_dir.join("sessions").join(id).join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert!(record.get("provider_resume").is_none());
    assert!(!record_path.with_file_name("resume.json").exists());
}

#[test]
fn start_does_not_capture_when_codex_scan_budget_is_truncated() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let codex_session = codex_home.join("sessions/2026/07/05/session.jsonl");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let codex_session_arg = codex_session.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_FILE", &codex_session_arg),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_ID", "codex-budget-id"),
            ("AGENT_SESSION_FAKE_CODEX_CWD", &cwd_arg),
            ("AGENT_SESSION_CODEX_CAPTURE_TIMEOUT_MS", "25"),
            ("AGENT_SESSION_CODEX_SCAN_SLICE_MS", "0"),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["resumable"], false);
    let id = result["id"].as_str().expect("session id");
    let record_path = state_dir.join("sessions").join(id).join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert!(record.get("provider_resume").is_none());
    assert!(!record_path.with_file_name("resume.json").exists());
}

#[test]
fn start_ignores_oversized_codex_session_meta_line() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let session_file = codex_home.join("sessions/2026/07/05/oversized.jsonl");
    fs::create_dir_all(session_file.parent().expect("parent")).expect("codex sessions");
    let huge = "x".repeat(2 * 1024 * 1024);
    fs::write(
        &session_file,
        format!(
            r#"{{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{{"id":"oversized-codex-id","session_id":"oversized-codex-id","cwd":"{}","source":"cli","timestamp":"2099-01-01T00:00:00Z","pad":"{}"}}}}"#,
            cwd.display(),
            huge
        ),
    )
    .expect("oversized session metadata");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["resumable"], false);
    let id = result["id"].as_str().expect("session id");
    let record_path = state_dir.join("sessions").join(id).join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert!(record.get("provider_resume").is_none());
}

#[test]
fn run_retains_state_when_malformed_launch_identity_cannot_be_stopped() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let mut pane_process = spawn_test_process_group();
    let pane_pid = pane_process.pid().to_string();
    let session_dir = state_dir.join("sessions/malformed-run");
    let record_path = session_dir.join("session.json");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let record_arg = record_path.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "run",
            "--agent",
            "codex",
            "--id",
            "malformed-run",
            "--cwd",
            &cwd_arg,
            "--prompt",
            "fixture prompt",
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_MALFORMED_LAUNCH_IDENTITY", "1"),
            ("AGENT_SESSION_FAKE_TMUX_FAIL", "kill-session"),
            ("AGENT_SESSION_FAKE_TMUX_PANE_PID", &pane_pid),
            ("AGENT_SESSION_FAKE_TMUX_AGENT_SESSION_ID", "malformed-run"),
            ("AGENT_SESSION_FAKE_TMUX_STATE_DIR", &state_arg),
            ("AGENT_SESSION_FAKE_TMUX_RUNTIME_RECORD", &record_arg),
        ],
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    let error = output.stdout_json();
    assert_eq!(error["error"]["code"], "session-termination-failed");
    assert_eq!(error["error"]["details"]["reason"], "kill-failed");
    assert_eq!(error["error"]["details"]["action"], "retry-delete");
    assert!(
        session_dir.exists(),
        "uncertain live runtime must stay discoverable"
    );
    let record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(record["delete_tmux_identity"]["session_id"], "$77");
    assert_eq!(record["delete_tmux_identity"]["pane_id"], "%77");
    assert_eq!(
        record["delete_tmux_identity"]["process_group_id"],
        pane_process.pid()
    );
    pane_process.stop();
}

#[test]
fn start_fails_closed_when_launch_identity_cannot_be_persisted() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let codex_session = codex_home.join("sessions/2026/07/05/session.jsonl");
    let chmod_dir = state_dir.join("sessions/write-fail");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let codex_session_arg = codex_session.to_string_lossy().to_string();
    let chmod_dir_arg = chmod_dir.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--id",
            "write-fail",
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_FILE", &codex_session_arg),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_ID", "codex-write-fail-id"),
            ("AGENT_SESSION_FAKE_CODEX_CWD", &cwd_arg),
            ("AGENT_SESSION_FAKE_CHMOD_AFTER_NEW_SESSION", &chmod_dir_arg),
        ],
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    let value = output.stdout_json();
    assert_eq!(value["error"]["code"], "file-write-failed");
    let _ = fs::set_permissions(&chmod_dir, fs::Permissions::from_mode(0o700));
    let record_path = chmod_dir.join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert!(record.get("provider_resume").is_none());
    assert!(record.get("delete_tmux_identity").is_none());
    assert!(!record_path.with_file_name("resume.json").exists());
    let calls = tmux_calls(&tmux_log);
    assert!(
        calls
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "kill-session")),
        "a launch must remain queryable until its recovery identity is durable: {calls:?}"
    );
}

#[test]
fn start_retains_state_when_process_group_survives_post_launch_cleanup() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let claude_bin = fake_agent(tmp.path(), "claude");
    let mut pane_process = spawn_test_process_group();
    let pane_pid = pane_process.pid().to_string();
    let session_dir = state_dir.join("sessions/process-survivor");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let claude_arg = claude_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "claude",
            "--id",
            "process-survivor",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &claude_arg,
            "--prompt",
            "fixture prompt",
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_PANE_PID", &pane_pid),
            ("AGENT_SESSION_FAKE_TMUX_PROCESS_GROUP_ID", &pane_pid),
            ("AGENT_SESSION_FAKE_TMUX_KEEP_PROCESS_GROUP", "1"),
            ("AGENT_SESSION_FAKE_TMUX_FAIL", "paste-buffer"),
        ],
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    let error = output.stdout_json();
    assert_eq!(error["error"]["code"], "session-termination-failed");
    assert_eq!(error["error"]["details"]["reason"], "process-still-running");
    assert_eq!(error["error"]["details"]["action"], "retry-delete");
    assert!(
        session_dir.exists(),
        "surviving process state must remain discoverable"
    );
    assert!(
        pane_process.is_running(),
        "the fixture process group must prove kill success alone is insufficient"
    );
    let record: Value = serde_json::from_slice(
        &fs::read(session_dir.join("session.json")).expect("retained session record"),
    )
    .unwrap();
    assert_eq!(
        record["delete_tmux_identity"]["process_group_id"],
        pane_process.pid()
    );
    pane_process.stop();

    let retry = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "delete",
            "process-survivor",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
        ],
    );
    assert_eq!(retry.code, 0, "stdout={}", retry.stdout_text());
    assert!(
        !session_dir.exists(),
        "retry must use the durable process identity"
    );
}

#[test]
fn start_reports_non_resumable_when_resume_sidecar_write_fails() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    let session_dir = state_dir.join("sessions/sidecar-conflict");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let codex_session = codex_home.join("sessions/2026/07/05/session.jsonl");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let codex_session_arg = codex_session.to_string_lossy().to_string();
    let sidecar_conflict_arg = session_dir
        .join("resume.json")
        .to_string_lossy()
        .to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "codex",
            "--cwd",
            &cwd_arg,
            "--id",
            "sidecar-conflict",
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &codex_arg,
            "--paste-delay-ms",
            "0",
            "--format",
            "json",
        ],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_CODEX_SESSION_FILE", &codex_session_arg),
            (
                "AGENT_SESSION_FAKE_CODEX_SESSION_ID",
                "codex-sidecar-conflict-id",
            ),
            ("AGENT_SESSION_FAKE_CODEX_CWD", &cwd_arg),
            (
                "AGENT_SESSION_FAKE_MKDIR_AFTER_NEW_SESSION",
                &sidecar_conflict_arg,
            ),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["id"], "sidecar-conflict");
    assert_eq!(result["status"], "running");
    assert_eq!(result["resumable"], false);
    let record_path = session_dir.join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert!(record.get("provider_resume").is_none());
    assert!(record_path.with_file_name("resume.json").is_dir());
}

#[test]
fn list_backfills_codex_resume_metadata_from_late_session_meta() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let session =
        write_session_record_with_cwd(&state_dir, "late-codex", "codex", "hs-codex-late", &cwd);
    write_codex_session_meta(
        &codex_home.join("sessions/2026/07/05/late.jsonl"),
        "late-codex-id",
        &cwd,
        "2000-01-01T00:00:30Z",
    );

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.list.v1");
    let sessions = data(&value).as_array().expect("list data");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], "late-codex");
    assert_eq!(sessions[0]["status"], "stopped");
    assert_eq!(sessions[0]["resumable"], true);
    assert_eq!(sessions[0]["provider_resume"]["provider"], "codex");
    assert_eq!(
        sessions[0]["provider_resume"]["session_id"],
        "late-codex-id"
    );
    assert_eq!(
        sessions[0]["provider_resume"]["resume_args"],
        json!([
            "resume",
            "late-codex-id",
            "--cd",
            cwd_arg,
            "--no-alt-screen"
        ])
    );

    let record: Value =
        serde_json::from_str(&fs::read_to_string(session.join("session.json")).expect("record"))
            .expect("record json");
    assert_eq!(record["provider_resume"]["session_id"], "late-codex-id");
    assert!(
        session.join("resume.json").is_file(),
        "lazy Codex metadata backfill should write the durable resume sidecar"
    );
}

#[test]
fn list_does_not_backfill_codex_resume_metadata_from_later_same_cwd_session() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let session =
        write_session_record_with_cwd(&state_dir, "stale-codex", "codex", "hs-codex-stale", &cwd);
    write_codex_session_meta(
        &codex_home.join("sessions/2026/07/05/later.jsonl"),
        "later-codex-id",
        &cwd,
        "2000-01-01T00:20:00Z",
    );

    let state_arg = state_dir.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let sessions = data(&value).as_array().expect("list data");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], "stale-codex");
    assert_eq!(sessions[0]["resumable"], false);
    assert!(sessions[0].get("provider_resume").is_none());

    let record: Value =
        serde_json::from_str(&fs::read_to_string(session.join("session.json")).expect("record"))
            .expect("record json");
    assert!(record.get("provider_resume").is_none());
    assert!(
        !session.join("resume.json").exists(),
        "later same-cwd metadata must not create a resume sidecar"
    );
}

#[test]
fn list_marks_missing_tmux_with_resume_identity_as_resumable() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let session = write_resumable_session_record(
        &state_dir,
        "recoverable",
        "codex",
        "hs-codex-recoverable",
        &cwd,
        &[
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().unwrap(),
            "--no-alt-screen",
        ],
    );
    add_provider_resume_extra(&session);

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &[
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.list.v1");
    let sessions = data(&value).as_array().expect("list data");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], "recoverable");
    assert_eq!(sessions[0]["status"], "stopped");
    assert_eq!(sessions[0]["resumable"], true);
    assert_eq!(sessions[0]["repo_name"], "repo");
    assert_eq!(sessions[0]["provider_resume"]["provider"], "codex");
    assert_eq!(
        sessions[0]["provider_resume"]["session_id"],
        "resume-session-id"
    );
    assert_eq!(
        sessions[0]["provider_resume"]["resume_args"],
        json!([
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().unwrap(),
            "--no-alt-screen"
        ])
    );
    assert!(sessions[0]["provider_resume"].get("storage_only").is_none());
}

#[test]
fn resume_recreates_tmux_runtime_from_exact_provider_identity() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let session = write_resumable_session_record_with_agent_bin(
        &state_dir,
        "recoverable",
        "codex",
        "hs-codex-recoverable",
        &cwd,
        &[
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().unwrap(),
            "--no-alt-screen",
        ],
        Some(&codex_bin),
    );
    let record_path = session.join("session.json");
    let mut failed_record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
    failed_record["startup"] = json!({
        "schema_version": "agent-session.startup.v1",
        "state": "failed",
        "stage": "tmux",
        "started_at": "2000-01-01T00:00:00Z",
        "failure_code": "terminal-runtime-create-failed",
        "message": "The terminal runtime could not be created.",
        "occurred_at": "2000-01-01T00:00:01Z",
        "retry_safe": true
    });
    fs::write(
        &record_path,
        serde_json::to_string_pretty(&failed_record).unwrap(),
    )
    .unwrap();
    fs::write(session.join(".startup-stage"), "tmux\n").unwrap();
    fs::write(
        session.join(".startup-failure"),
        "terminal-runtime-create-failed\n",
    )
    .unwrap();
    fs::write(
        session.join(".startup-diagnostic.log"),
        "private prior failure\n",
    )
    .unwrap();
    fs::write(session.join(".runtime-exit-status"), "17\n").unwrap();

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "resume",
            "recoverable",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.resume.v1");
    let result = data(&value);
    assert_eq!(result["id"], "recoverable");
    assert_eq!(result["status"], "running");
    assert_eq!(result["resumable"], true);
    assert_eq!(result["startup"]["state"], "ready");

    let calls = tmux_calls(&tmux_log);
    let new_session = calls
        .iter()
        .find(|call| call.first().is_some_and(|arg| arg == "new-session"))
        .expect("new-session call");
    let record: Value = serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
    let runtime_id = record["runtime"]["launch_id"].as_str().expect("runtime id");
    assert_eq!(
        new_session,
        &vec![
            "new-session".to_string(),
            "-d".to_string(),
            "-P".to_string(),
            "-F".to_string(),
            "#{session_id}\t#{pane_id}\t#{pane_pid}".to_string(),
            "-s".to_string(),
            "hs-codex-recoverable".to_string(),
            "-c".to_string(),
            cwd.to_string_lossy().to_string(),
            "-e".to_string(),
            "AGENT_SESSION_ID=recoverable".to_string(),
            "-e".to_string(),
            format!("AGENT_SESSION_STATE_DIR={}", state_dir.display()),
            "-e".to_string(),
            format!("AGENT_SESSION_RUNTIME_ID={runtime_id}"),
            "--".to_string(),
            codex_arg.clone(),
            "resume".to_string(),
            "resume-session-id".to_string(),
            "--cd".to_string(),
            cwd.to_string_lossy().to_string(),
            "--no-alt-screen".to_string(),
            "--model".to_string(),
            "fixture-model".to_string(),
        ]
    );

    assert_eq!(record["id"], "recoverable");
    assert_eq!(record["runtime"]["generation"], 2);
    assert_ne!(record["updated_at"], "2000-01-01T00:00:00Z");
    assert_eq!(record["agent_bin"], codex_arg);
    assert_eq!(record["startup"]["state"], "ready");
    assert!(!session.join(".startup-failure").exists());
    assert!(!session.join(".startup-diagnostic.log").exists());
    assert!(!session.join(".runtime-exit-status").exists());
    assert!(
        record_path.with_file_name("resume.json").is_file(),
        "resume should refresh the durable sidecar"
    );

    let list = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &[
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(list.code, 0, "stderr={}", list.stderr_text());
    let list_value = list.stdout_json();
    let listed = &data(&list_value)[0];
    assert_eq!(listed["status"], "stopped");
    assert_eq!(listed["startup"]["state"], "ready");
}

#[test]
fn resume_persists_runtime_generation_before_provider_launch() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let session = write_resumable_session_record_with_agent_bin(
        &state_dir,
        "resume-write-fail",
        "codex",
        "hs-codex-resume-write-fail",
        &cwd,
        &[
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().unwrap(),
            "--no-alt-screen",
        ],
        Some(&codex_bin),
    );

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let record_at_launch = session.join("session.json").to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "resume",
            "resume-write-fail",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
            (
                "AGENT_SESSION_FAKE_RECORD_AT_NEW_SESSION",
                &record_at_launch,
            ),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["status"], "running");
    assert_eq!(result["resumable"], true);
    assert_ne!(result["updated_at"], "2000-01-01T00:00:00Z");
    assert_eq!(result["turn_state"]["phase"], "starting");
    let record_path = session.join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert_eq!(record["runtime"]["generation"], 2);
    assert!(record["runtime"]["launch_id"].is_string());
    assert_ne!(record["updated_at"], "2000-01-01T00:00:00Z");
}

#[test]
fn resume_launch_failure_restores_the_prior_runtime_and_activity() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let session = write_resumable_session_record_with_agent_bin(
        &state_dir,
        "resume-launch-fail",
        "codex",
        "hs-codex-resume-launch-fail",
        &cwd,
        &[
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().unwrap(),
            "--no-alt-screen",
        ],
        Some(&codex_bin),
    );
    let record_path = session.join("session.json");
    let mut seeded: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
    seeded["startup"] = json!({
        "schema_version": "agent-session.startup.v1",
        "state": "failed",
        "stage": "tmux",
        "started_at": "2000-01-01T00:00:00Z",
        "failure_code": "terminal-runtime-create-failed",
        "message": "The terminal runtime could not be created.",
        "occurred_at": "2000-01-01T00:00:01Z",
        "retry_safe": true
    });
    fs::write(&record_path, serde_json::to_string_pretty(&seeded).unwrap()).unwrap();
    fs::write(session.join(".startup-stage"), "tmux\n").unwrap();
    fs::write(
        session.join(".startup-failure"),
        "terminal-runtime-create-failed\n",
    )
    .unwrap();
    fs::write(
        session.join(".startup-diagnostic.log"),
        "private prior failure\n",
    )
    .unwrap();
    fs::write(session.join(".runtime-exit-status"), "17\n").unwrap();
    let before: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("prior record"))
            .expect("prior record json");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "resume",
            "resume-launch-fail",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
            ("AGENT_SESSION_FAKE_TMUX_FAIL", "new-session"),
        ],
    );

    assert_ne!(output.code, 0);
    assert_eq!(output.stdout_json()["error"]["code"], "command-failed");
    let after: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("restored record"))
            .expect("restored record json");
    assert_eq!(after["runtime"], before["runtime"]);
    assert_eq!(after["updated_at"], before["updated_at"]);
    assert_eq!(after["startup"], before["startup"]);
    assert_eq!(
        fs::read_to_string(session.join(".startup-failure")).unwrap(),
        "terminal-runtime-create-failed\n"
    );
    assert_eq!(
        fs::read_to_string(session.join(".startup-diagnostic.log")).unwrap(),
        "private prior failure\n"
    );
    assert_eq!(
        fs::read_to_string(session.join(".runtime-exit-status")).unwrap(),
        "17\n"
    );
    assert!(
        !session.join("activity.json").exists(),
        "a failed launch must not retain a phantom Starting runtime"
    );
    assert!(
        !session.join("activity.replay.bin").exists(),
        "a failed launch must restore the prior replay-index boundary"
    );
}

#[test]
fn resume_retains_new_generation_when_identity_persistence_and_shutdown_fail() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let session = write_resumable_session_record_with_agent_bin(
        &state_dir,
        "resume-persist-fail",
        "codex",
        "hs-codex-resume-persist-fail",
        &cwd,
        &[
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().unwrap(),
            "--no-alt-screen",
        ],
        Some(&codex_bin),
    );
    let record_path = session.join("session.json");
    let before: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let session_arg = session.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "resume",
            "resume-persist-fail",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_ABSENT_BEFORE_LAUNCH", "1"),
            ("AGENT_SESSION_FAKE_CHMOD_AFTER_NEW_SESSION", &session_arg),
        ],
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    let error = output.stdout_json();
    assert_eq!(error["error"]["code"], "file-write-failed");
    let _ = fs::set_permissions(&session, fs::Permissions::from_mode(0o700));
    let retained: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(retained["runtime"]["generation"], 2);
    assert_ne!(
        retained["runtime"]["launch_id"], before["runtime"]["launch_id"],
        "the live replacement generation must not be rolled back to the prior identity"
    );
    assert!(
        session.exists(),
        "the current generation must remain discoverable"
    );
    assert!(
        tmux_calls(&tmux_log)
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "kill-session")),
        "resume must keep the replacement queryable until its identity is durable"
    );
}

#[test]
fn resume_fails_closed_without_deleting_interrupted_startup_backups() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    write_codex_session_meta(
        &codex_home.join("sessions/2026/07/05/backfill.jsonl"),
        "resume-session-id",
        &cwd,
        "2000-01-01T00:00:30Z",
    );
    let state_arg = state_dir.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    for (suffix, artifact) in [
        ("stage", ".startup-stage"),
        ("failure", ".startup-failure"),
        ("diagnostic", ".startup-diagnostic.log"),
        ("exit-status", ".runtime-exit-status"),
    ] {
        let id = format!("resume-interrupted-{suffix}");
        let session = write_session_record_with_cwd(
            &state_dir,
            &id,
            "codex",
            &format!("hs-codex-resume-interrupted-{suffix}"),
            &cwd,
        );
        let record_path = session.join("session.json");
        let sidecar_path = session.join("resume.json");
        let before = fs::read_to_string(&record_path).expect("prior record");
        let staged = session.join(format!("{artifact}.resume-backup"));
        fs::write(&staged, "prior private artifact\n").unwrap();
        let output = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "resume",
                &id,
                "--tmux-bin",
                &tmux_arg,
                "--format",
                "json",
            ],
            &[
                ("CODEX_HOME", &codex_home_arg),
                ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
                ("AGENT_SESSION_FAKE_TMUX_FAIL", "new-session"),
            ],
        );

        assert_ne!(output.code, 0);
        assert_eq!(
            output.stdout_json()["error"]["code"],
            "startup-artifact-backup-interrupted"
        );
        assert_eq!(fs::read_to_string(&record_path).unwrap(), before);
        assert!(!sidecar_path.exists());
        assert_eq!(
            fs::read_to_string(&staged).unwrap(),
            "prior private artifact\n"
        );
    }
    assert!(
        tmux_calls(&tmux_log)
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "new-session")),
        "interrupted transaction must fail before launching a new runtime"
    );
}

#[test]
fn resume_treats_a_dangling_startup_backup_symlink_as_interrupted_state() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let codex_home = tmp.path().join("codex-home");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let session = write_session_record_with_cwd(
        &state_dir,
        "resume-dangling-backup",
        "codex",
        "hs-codex-resume-dangling-backup",
        &cwd,
    );
    write_codex_session_meta(
        &codex_home.join("sessions/2026/07/05/backfill.jsonl"),
        "resume-session-id",
        &cwd,
        "2000-01-01T00:00:30Z",
    );
    let record_path = session.join("session.json");
    let sidecar_path = session.join("resume.json");
    let current_failure = session.join(".startup-failure");
    let staged_failure = session.join(".startup-failure.resume-backup");
    let missing_target = session.join("missing-prior-artifact");
    let before = fs::read_to_string(&record_path).expect("prior record");
    fs::write(&current_failure, "current failure\n").unwrap();
    symlink(&missing_target, &staged_failure).unwrap();

    let state_arg = state_dir.to_string_lossy().to_string();
    let codex_home_arg = codex_home.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "resume",
            "resume-dangling-backup",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("CODEX_HOME", &codex_home_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
            ("AGENT_SESSION_FAKE_TMUX_FAIL", "new-session"),
        ],
    );

    assert_ne!(output.code, 0);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "startup-artifact-backup-interrupted"
    );
    assert_eq!(fs::read_to_string(&record_path).unwrap(), before);
    assert!(!sidecar_path.exists());
    assert_eq!(
        fs::read_to_string(&current_failure).unwrap(),
        "current failure\n"
    );
    assert_eq!(fs::read_link(&staged_failure).unwrap(), missing_target);
    assert!(
        tmux_calls(&tmux_log)
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "new-session")),
        "dangling backup entry must block before launching a new runtime"
    );
}

#[test]
fn resume_recovers_provider_identity_from_durable_sidecar() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex-custom");
    let session =
        write_session_record(&state_dir, "sidecar-only", "codex", "hs-codex-sidecar-only");
    let mut record: Value =
        serde_json::from_str(&fs::read_to_string(session.join("session.json")).unwrap()).unwrap();
    record["cwd"] = Value::String(cwd.to_string_lossy().to_string());
    record["startup"] = json!({
        "schema_version": "agent-session.startup.v1",
        "state": "failed",
        "stage": "tmux",
        "started_at": "2000-01-01T00:00:00Z",
        "failure_code": "terminal-runtime-create-failed",
        "message": "The terminal runtime could not be created.",
        "occurred_at": "2000-01-01T00:00:01Z",
        "retry_safe": true
    });
    fs::write(
        session.join("session.json"),
        serde_json::to_string_pretty(&record).unwrap(),
    )
    .expect("rewrite fixture record");
    write_resume_sidecar(
        &session,
        "codex",
        "hs-codex-sidecar-only",
        &codex_bin,
        &[
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().unwrap(),
            "--no-alt-screen",
        ],
    );
    let sidecar_path = session.join("resume.json");
    let mut sidecar: Value =
        serde_json::from_slice(&fs::read(&sidecar_path).expect("resume sidecar")).unwrap();
    sidecar["runtime"]["launch_id"] = json!("never-launched-sidecar");
    fs::write(&sidecar_path, serde_json::to_vec_pretty(&sidecar).unwrap()).unwrap();
    let mut record: Value =
        serde_json::from_slice(&fs::read(session.join("session.json")).unwrap()).unwrap();
    record["tmux_runtime_never_launched"] = json!("never-launched-sidecar");
    fs::write(
        session.join("session.json"),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let codex_arg = codex_bin.to_string_lossy().to_string();
    let list = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &[
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(list.code, 0, "stderr={}", list.stderr_text());
    let list_json = list.stdout_json();
    let sessions = data(&list_json).as_array().expect("list data");
    assert_eq!(sessions[0]["status"], "stopped");
    assert_eq!(sessions[0]["provider_resume"]["provider"], "codex");
    assert_eq!(
        sessions[0]["provider_resume"]["session_id"],
        "resume-session-id"
    );
    assert_eq!(
        sessions[0]["provider_resume"]["capture_method"],
        "fixture-sidecar"
    );
    assert_eq!(
        sessions[0]["provider_resume"]["resume_args"],
        json!([
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().unwrap(),
            "--no-alt-screen"
        ])
    );

    let resume = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "resume",
            "sidecar-only",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(resume.code, 0, "stderr={}", resume.stderr_text());
    let calls = tmux_calls(&tmux_log);
    let new_session = calls
        .iter()
        .find(|call| call.first().is_some_and(|arg| arg == "new-session"))
        .expect("new-session call");
    assert!(
        new_session.contains(&codex_arg),
        "resume should use sidecar agent_bin: {new_session:?}"
    );
    assert!(
        new_session.contains(&"sidecar-model".to_string()),
        "resume should use sidecar agent args: {new_session:?}"
    );
}

#[test]
fn resume_preserves_nested_future_fields_in_session_and_sidecar() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    let session = write_resumable_session_record_with_agent_bin(
        &state_dir,
        "future-fields",
        "codex",
        "hs-codex-future-fields",
        &cwd,
        &[
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().unwrap(),
            "--no-alt-screen",
        ],
        Some(&codex_bin),
    );
    write_resume_sidecar(
        &session,
        "codex",
        "hs-codex-future-fields",
        &codex_bin,
        &[
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().unwrap(),
            "--no-alt-screen",
        ],
    );
    let record_path = session.join("session.json");
    let mut record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
    record["provider_resume"]["future_provider"] = json!({"keep": "session"});
    record["runtime"]["future_runtime"] = json!({"keep": "session"});
    fs::write(&record_path, serde_json::to_string_pretty(&record).unwrap())
        .expect("session record with future fields");
    let sidecar_path = session.join("resume.json");
    let mut sidecar: Value =
        serde_json::from_str(&fs::read_to_string(&sidecar_path).unwrap()).unwrap();
    sidecar["future_sidecar"] = json!({"keep": "sidecar"});
    sidecar["provider_resume"]["future_provider_sidecar"] = json!({"keep": "sidecar"});
    sidecar["runtime"]["future_runtime_sidecar"] = json!({"keep": "sidecar"});
    fs::write(
        &sidecar_path,
        serde_json::to_string_pretty(&sidecar).unwrap(),
    )
    .expect("resume sidecar with future fields");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "resume",
            "future-fields",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let record_after: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
    assert_eq!(
        record_after["provider_resume"]["future_provider"],
        json!({"keep": "session"})
    );
    assert_eq!(
        record_after["provider_resume"]["future_provider_sidecar"],
        json!({"keep": "sidecar"})
    );
    assert_eq!(
        record_after["runtime"]["future_runtime"],
        json!({"keep": "session"})
    );
    assert_eq!(
        record_after["runtime"]["future_runtime_sidecar"],
        json!({"keep": "sidecar"})
    );
    let sidecar_after: Value =
        serde_json::from_str(&fs::read_to_string(&sidecar_path).unwrap()).unwrap();
    assert_eq!(sidecar_after["future_sidecar"], json!({"keep": "sidecar"}));
    assert_eq!(
        sidecar_after["provider_resume"]["future_provider"],
        json!({"keep": "session"})
    );
    assert_eq!(
        sidecar_after["provider_resume"]["future_provider_sidecar"],
        json!({"keep": "sidecar"})
    );
    assert_eq!(
        sidecar_after["runtime"]["future_runtime"],
        json!({"keep": "session"})
    );
    assert_eq!(
        sidecar_after["runtime"]["future_runtime_sidecar"],
        json!({"keep": "sidecar"})
    );
}

#[test]
fn list_and_delete_ignore_unsupported_or_malformed_resume_sidecars() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let future = write_session_record(
        &state_dir,
        "future-sidecar",
        "codex",
        "hs-codex-future-sidecar",
    );
    fs::write(
        future.join("resume.json"),
        r#"{"schema_version":"agent-session.resume.v2","provider_resume":{"provider":"codex","session_id":"future","captured_at":"2000-01-01T00:00:00Z","capture_method":"fixture","resume_args":["resume","future"]}}"#,
    )
    .expect("future resume sidecar");
    attach_provider_runtime(
        tmp.path(),
        &state_dir,
        &future,
        "future-sidecar",
        "codex",
        "hs-codex-future-sidecar",
    );
    let malformed = write_session_record(
        &state_dir,
        "malformed-sidecar",
        "codex",
        "hs-codex-malformed-sidecar",
    );
    fs::write(malformed.join("resume.json"), "{not-json").expect("malformed resume sidecar");
    attach_provider_runtime(
        tmp.path(),
        &state_dir,
        &malformed,
        "malformed-sidecar",
        "codex",
        "hs-codex-malformed-sidecar",
    );

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let list = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &[
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(list.code, 0, "stderr={}", list.stderr_text());
    let list_json = list.stdout_json();
    let sessions = data(&list_json).as_array().expect("list data");
    assert_eq!(sessions.len(), 2);
    for id in ["future-sidecar", "malformed-sidecar"] {
        let session = sessions
            .iter()
            .find(|session| session["id"] == id)
            .expect("listed session");
        assert_eq!(session["status"], "stopped");
        assert_eq!(session["resumable"], false);
    }

    for id in ["future-sidecar", "malformed-sidecar"] {
        let record_path = state_dir.join("sessions").join(id).join("session.json");
        let mut record: Value =
            serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
        record["tmux_runtime_never_launched"] = record["runtime"]["launch_id"].clone();
        fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        let delete = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "delete",
                id,
                "--tmux-bin",
                &tmux_arg,
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ("AGENT_SESSION_FAKE_TMUX_ABSENT", "1"),
            ],
        );
        assert_eq!(delete.code, 0, "stderr={}", delete.stderr_text());
        assert_eq!(data(&delete.stdout_json())["deleted"], true);
    }
}

#[test]
fn send_preserves_unsupported_resume_sidecar() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let session = write_session_record(
        &state_dir,
        "future-sidecar-write",
        "codex",
        "hs-codex-future-sidecar-write",
    );
    let sidecar_path = session.join("resume.json");
    let future_sidecar = r#"{"schema_version":"agent-session.resume.v2","provider_resume":{"provider":"codex","session_id":"future","captured_at":"2000-01-01T00:00:00Z","capture_method":"fixture","resume_args":["resume","future"]}}"#;
    fs::write(&sidecar_path, future_sidecar).expect("future sidecar");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let send = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "send",
            "future-sidecar-write",
            "--key",
            "enter",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg)],
    );

    assert_eq!(send.code, 0, "stderr={}", send.stderr_text());
    assert_eq!(
        fs::read_to_string(&sidecar_path).expect("future sidecar after send"),
        future_sidecar
    );
}

#[test]
fn send_preserves_unsupported_resume_sidecar_with_inline_provider_resume() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let session = write_resumable_session_record(
        &state_dir,
        "future-sidecar-inline-write",
        "codex",
        "hs-codex-future-sidecar-inline-write",
        &cwd,
        &["resume", "resume-session-id"],
    );
    let sidecar_path = session.join("resume.json");
    let future_sidecar = r#"{"schema_version":"agent-session.resume.v2","provider_resume":{"provider":"codex","session_id":"future","captured_at":"2000-01-01T00:00:00Z","capture_method":"fixture","resume_args":["resume","future"]}}"#;
    fs::write(&sidecar_path, future_sidecar).expect("future sidecar");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let send = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "send",
            "future-sidecar-inline-write",
            "--key",
            "enter",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg)],
    );

    assert_eq!(send.code, 0, "stderr={}", send.stderr_text());
    assert_eq!(
        fs::read_to_string(&sidecar_path).expect("future sidecar after send"),
        future_sidecar
    );
}

#[test]
fn resume_refuses_non_resumable_or_invalid_identity_without_starting_tmux() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    write_session_record(&state_dir, "plain", "codex", "hs-codex-plain");
    let mismatch_session = write_resumable_session_record(
        &state_dir,
        "mismatch",
        "codex",
        "hs-codex-mismatch",
        &cwd,
        &[
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().unwrap(),
            "--no-alt-screen",
        ],
    );
    let mismatch_path = mismatch_session.join("session.json");
    let mut mismatch: Value =
        serde_json::from_str(&fs::read_to_string(&mismatch_path).unwrap()).unwrap();
    mismatch["provider_resume"]["provider"] = Value::String("claude".to_string());
    fs::write(
        &mismatch_path,
        serde_json::to_string_pretty(&mismatch).unwrap(),
    )
    .expect("mismatch fixture");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let env_refs = [
        ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
        ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
    ];

    let plain = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "resume",
            "plain",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &env_refs,
    );
    assert_eq!(plain.code, 65, "stderr={}", plain.stderr_text());
    assert_eq!(
        plain.stdout_json()["error"]["code"],
        "session-not-resumable"
    );

    let list = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &[
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(list.code, 0, "stderr={}", list.stderr_text());
    let list_json = list.stdout_json();
    let sessions = data(&list_json).as_array().expect("list data");
    let mismatch = sessions
        .iter()
        .find(|session| session["id"] == "mismatch")
        .expect("mismatch session");
    assert_eq!(mismatch["status"], "stopped");
    assert_eq!(mismatch["resumable"], false);

    let mismatch = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "resume",
            "mismatch",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &env_refs,
    );
    assert_eq!(mismatch.code, 65, "stderr={}", mismatch.stderr_text());
    assert_eq!(
        mismatch.stdout_json()["error"]["code"],
        "session-provider-mismatch"
    );

    let calls = tmux_calls(&tmux_log);
    assert!(
        calls
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "new-session")),
        "resume refusals should not create tmux sessions: {calls:?}"
    );
}

#[test]
fn resume_refuses_provider_resume_args_that_do_not_match_session_id() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let session = write_resumable_session_record(
        &state_dir,
        "mismatched-resume-args",
        "codex",
        "hs-codex-mismatched-resume-args",
        &cwd,
        &[
            "resume",
            "different-session-id",
            "--cd",
            cwd.to_str().expect("cwd"),
            "--no-alt-screen",
        ],
    );
    assert!(session.join("session.json").exists());

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let list = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &[
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(list.code, 0, "stderr={}", list.stderr_text());
    let list_json = list.stdout_json();
    let sessions = data(&list_json).as_array().expect("list data");
    assert_eq!(sessions[0]["resumable"], false);

    let resume = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "resume",
            "mismatched-resume-args",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );

    assert_eq!(resume.code, 65, "stderr={}", resume.stderr_text());
    assert_eq!(
        resume.stdout_json()["error"]["code"],
        "session-not-resumable"
    );
    assert!(
        !tmux_calls(&tmux_log)
            .iter()
            .any(|call| call.first().is_some_and(|arg| arg == "new-session")),
        "invalid resume args must not start tmux"
    );
}

#[test]
fn resume_refuses_stored_claude_resume_identity_agent_args() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let claude_bin = fake_agent(tmp.path(), "claude");

    let session_record = write_resumable_session_record(
        &state_dir,
        "claude-record-conflict",
        "claude",
        "hs-claude-record-conflict",
        &cwd,
        &["--resume", "resume-session-id"],
    );
    let record_path = session_record.join("session.json");
    let mut record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    record["agent_args"] = json!(["-rother-session"]);
    fs::write(
        &record_path,
        serde_json::to_string_pretty(&record).expect("record json"),
    )
    .expect("session record");

    let sidecar_record = write_session_record(
        &state_dir,
        "claude-sidecar-conflict",
        "claude",
        "hs-claude-sidecar-conflict",
    );
    write_resume_sidecar(
        &sidecar_record,
        "claude",
        "hs-claude-sidecar-conflict",
        &claude_bin,
        &["--resume", "resume-session-id"],
    );
    let sidecar_path = sidecar_record.join("resume.json");
    let mut sidecar: Value =
        serde_json::from_str(&fs::read_to_string(&sidecar_path).expect("resume sidecar"))
            .expect("sidecar json");
    sidecar["agent_args"] = json!(["--continue"]);
    fs::write(
        &sidecar_path,
        serde_json::to_string_pretty(&sidecar).expect("sidecar json"),
    )
    .expect("resume sidecar");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let list = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &[
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(list.code, 0, "stderr={}", list.stderr_text());
    let list_json = list.stdout_json();
    let sessions = data(&list_json).as_array().expect("list data");
    for id in ["claude-record-conflict", "claude-sidecar-conflict"] {
        let session = sessions
            .iter()
            .find(|session| session["id"] == id)
            .expect("listed session");
        assert_eq!(session["status"], "stopped");
        assert_eq!(session["resumable"], false);
    }

    for id in ["claude-record-conflict", "claude-sidecar-conflict"] {
        let resume = run(
            tmp.path(),
            &[
                "--state-dir",
                &state_arg,
                "resume",
                id,
                "--tmux-bin",
                &tmux_arg,
                "--format",
                "json",
            ],
            &[
                ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
                ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
            ],
        );
        assert_eq!(resume.code, 65, "id={id}, stderr={}", resume.stderr_text());
        assert_eq!(
            resume.stdout_json()["error"]["code"],
            "session-not-resumable"
        );
    }
    assert!(
        !tmux_calls(&tmux_log)
            .iter()
            .any(|call| call.first().is_some_and(|arg| arg == "new-session")),
        "invalid stored agent args must not start tmux"
    );
}

#[test]
fn resume_refuses_stored_codex_cwd_agent_args() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());

    let session_record = write_resumable_session_record(
        &state_dir,
        "codex-record-conflict",
        "codex",
        "hs-codex-record-conflict",
        &cwd,
        &[
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().expect("cwd"),
            "--no-alt-screen",
        ],
    );
    let record_path = session_record.join("session.json");
    let mut record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    record["agent_args"] = json!(["-C/tmp/other"]);
    fs::write(
        &record_path,
        serde_json::to_string_pretty(&record).expect("record json"),
    )
    .expect("session record");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let list = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &[
            ("AGENT_SESSION_TMUX_BIN", &tmux_arg),
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(list.code, 0, "stderr={}", list.stderr_text());
    let list_json = list.stdout_json();
    let sessions = data(&list_json).as_array().expect("list data");
    let session = sessions
        .iter()
        .find(|session| session["id"] == "codex-record-conflict")
        .expect("listed session");
    assert_eq!(session["status"], "stopped");
    assert_eq!(session["resumable"], false);

    let resume = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "resume",
            "codex-record-conflict",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(resume.code, 65, "stderr={}", resume.stderr_text());
    assert_eq!(
        resume.stdout_json()["error"]["code"],
        "session-not-resumable"
    );
    assert!(
        !tmux_calls(&tmux_log)
            .iter()
            .any(|call| call.first().is_some_and(|arg| arg == "new-session")),
        "invalid stored agent args must not start tmux"
    );
}

#[test]
fn resume_refuses_when_tmux_status_is_unknown() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let codex_bin = fake_agent(tmp.path(), "codex");
    write_resumable_session_record_with_agent_bin(
        &state_dir,
        "unknown-status",
        "codex",
        "hs-codex-unknown-status",
        &cwd,
        &[
            "resume",
            "resume-session-id",
            "--cd",
            cwd.to_str().unwrap(),
            "--no-alt-screen",
        ],
        Some(&codex_bin),
    );

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "resume",
            "unknown-status",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg),
            ("AGENT_SESSION_FAKE_TMUX_FAIL", "has-session"),
        ],
    );

    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "session-status-unknown"
    );
    let calls = tmux_calls(&tmux_log);
    assert!(
        calls
            .iter()
            .all(|call| call.first().is_none_or(|arg| arg != "new-session")),
        "unknown status should not create tmux sessions: {calls:?}"
    );
}

#[test]
fn send_delivers_text_and_keys_without_leaking_and_bumps_updated_at() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let session = write_session_record(&state_dir, "steer", "codex", "hs-codex-steer");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let env_refs = [("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str())];
    let secret = "approve-secret-payload";

    // With neither text nor keys, send is a usage error before touching tmux.
    let empty = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "send",
            "steer",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &env_refs,
    );
    assert_eq!(empty.code, 64, "stderr={}", empty.stderr_text());
    assert_eq!(empty.stdout_json()["error"]["code"], "empty-send");

    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "send",
            "steer",
            "--text",
            secret,
            "--key",
            "enter",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &env_refs,
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_no_secret(&output, secret);
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.send.v1");
    let result = data(&value);
    assert_eq!(result["id"], "steer");
    assert_eq!(result["sent_text"], true);
    let keys = result["keys"].as_array().expect("keys array");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0], "enter");

    let calls = tmux_calls(&tmux_log);
    assert!(
        calls
            .iter()
            .any(|call| call.first().is_some_and(|arg| arg == "load-buffer")),
        "missing load-buffer call: {calls:?}"
    );
    assert!(
        calls.iter().any(|call| {
            call.first().is_some_and(|arg| arg == "paste-buffer")
                && call.get(1).is_some_and(|arg| arg == "-b")
                && call
                    .get(2)
                    .is_some_and(|arg| arg.starts_with("steer-send-"))
                && call.get(3).is_some_and(|arg| arg == "-d")
                && call.get(4).is_some_and(|arg| arg == "-t")
                && call.get(5).is_some_and(|arg| arg == "hs-codex-steer:0.0")
        }),
        "missing unique paste-buffer -d call: {calls:?}"
    );
    assert!(
        calls.iter().any(|call| call
            == &vec![
                "send-keys".to_string(),
                "-t".to_string(),
                "hs-codex-steer:0.0".to_string(),
                "Enter".to_string(),
            ]),
        "missing send-keys Enter call: {calls:?}"
    );
    // Text is applied BEFORE keys: the paste must precede the Enter, or an empty
    // prompt would be submitted before the text arrives.
    let paste_idx = calls
        .iter()
        .position(|call| call.first().is_some_and(|arg| arg == "paste-buffer"))
        .expect("paste-buffer call");
    let enter_idx = calls
        .iter()
        .position(|call| {
            call.first().is_some_and(|arg| arg == "send-keys")
                && call.last().is_some_and(|arg| arg == "Enter")
        })
        .expect("send-keys Enter call");
    assert!(
        paste_idx < enter_idx,
        "text paste must precede the Enter key: {calls:?}"
    );
    // The secret text travels through a private buffer file, never argv.
    for call in &calls {
        for arg in call {
            assert!(
                !arg.contains(secret),
                "secret text leaked into tmux argv: {call:?}"
            );
        }
    }
    assert!(
        fs::read_dir(&session).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("send-input-")),
        "unique send-input temp files should be cleaned up"
    );

    // send bumps updated_at away from the sentinel so list can sort by activity.
    let after: Value = serde_json::from_str(
        &fs::read_to_string(session.join("session.json")).expect("re-read record"),
    )
    .expect("parse record");
    assert_ne!(
        after["updated_at"], "2000-01-01T00:00:00Z",
        "updated_at should be bumped after send"
    );
    assert!(
        after["updated_at"].as_str().unwrap() > "2000-01-01T00:00:00Z",
        "updated_at should advance forward: {}",
        after["updated_at"]
    );
}

#[test]
fn glance_returns_pane_tail_and_status_contract() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    write_session_record(&state_dir, "look", "claude", "hs-claude-look");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "glance",
            "look",
            "--tail",
            "10",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_WINDOW_ACTIVITY", "1000000000"),
        ],
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.glance.v1");
    let result = data(&value);
    assert_eq!(result["id"], "look");
    assert_eq!(result["agent"], "claude");
    assert_eq!(result["status"], "running");
    assert_eq!(result["last_terminal_activity_at"], "2001-09-09T01:46:40Z");
    assert!(result.get("provider_resume").is_none());
    let tail = result["tail"].as_str().expect("tail");
    assert!(
        tail.contains("pane one") && tail.contains("pane two"),
        "unexpected tail: {tail}"
    );

    // A stopped session yields status=stopped with an empty tail, no error.
    let stopped = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "glance",
            "look",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_WINDOW_ACTIVITY", "1000000000"),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(stopped.code, 0, "stderr={}", stopped.stderr_text());
    let stopped_result = data(&stopped.stdout_json()).clone();
    assert_eq!(stopped_result["status"], "stopped");
    assert_eq!(stopped_result["tail"], "");
    assert!(stopped_result.get("last_terminal_activity_at").is_none());
    assert!(stopped_result.get("provider_resume").is_none());

    let recover_cwd = tmp.path().join("recoverable-repo");
    fs::create_dir_all(&recover_cwd).expect("recoverable repo dir");
    let recover_session = write_resumable_session_record(
        &state_dir,
        "recoverable",
        "codex",
        "hs-codex-recoverable",
        &recover_cwd,
        &[
            "resume",
            "resume-session-id",
            "--cd",
            recover_cwd.to_str().unwrap(),
            "--no-alt-screen",
        ],
    );
    add_provider_resume_extra(&recover_session);

    let resumable = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "glance",
            "recoverable",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(resumable.code, 0, "stderr={}", resumable.stderr_text());
    let resumable_result = data(&resumable.stdout_json()).clone();
    assert_eq!(resumable_result["status"], "stopped");
    assert_eq!(resumable_result["resumable"], true);
    assert_eq!(resumable_result["provider_resume"]["provider"], "codex");
    assert_eq!(
        resumable_result["provider_resume"]["session_id"],
        "resume-session-id"
    );
    assert_eq!(
        resumable_result["provider_resume"]["capture_method"],
        "fixture"
    );
    assert_eq!(
        resumable_result["provider_resume"]["resume_args"],
        json!([
            "resume",
            "resume-session-id",
            "--cd",
            recover_cwd.to_str().unwrap(),
            "--no-alt-screen"
        ])
    );
    assert!(
        resumable_result["provider_resume"]
            .get("storage_only")
            .is_none()
    );
}

#[test]
fn start_hermes_launches_interactive_chat_session() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let hermes_bin = fake_agent(tmp.path(), "hermes");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let hermes_arg = hermes_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "start",
            "--agent",
            "hermes",
            "--cwd",
            &cwd_arg,
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &hermes_arg,
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str())],
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.start.v1");
    let result = data(&value);
    assert_eq!(result["agent"], "hermes");
    assert!(
        result["tmux_session"]
            .as_str()
            .unwrap()
            .starts_with("hs-hermes-"),
        "tmux_session={}",
        result["tmux_session"]
    );

    let calls = tmux_calls(&tmux_log);
    let new_session = calls
        .iter()
        .find(|call| call.first().is_some_and(|arg| arg == "new-session"))
        .expect("new-session call");
    let bin_idx = new_session
        .iter()
        .position(|arg| arg == &hermes_arg)
        .expect("hermes bin in new-session call");
    assert_eq!(
        new_session.get(bin_idx + 1).map(String::as_str),
        Some("chat"),
        "hermes must launch the `chat` subcommand: {new_session:?}"
    );
}

#[test]
fn run_rejects_hermes_agent_without_orphaning_state() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let cwd = tmp.path().join("repo");
    fs::create_dir_all(&cwd).expect("repo dir");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let hermes_bin = fake_agent(tmp.path(), "hermes");

    let state_arg = state_dir.to_string_lossy().to_string();
    let cwd_arg = cwd.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let hermes_arg = hermes_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "run",
            "--agent",
            "hermes",
            "--cwd",
            &cwd_arg,
            "--prompt",
            "do a thing",
            "--tmux-bin",
            &tmux_arg,
            "--agent-bin",
            &hermes_arg,
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str())],
    );
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.run.v1");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "unsupported-run-agent");
    let orphans = fs::read_dir(state_dir.join("sessions"))
        .map(|dir| dir.count())
        .unwrap_or(0);
    assert_eq!(
        orphans, 0,
        "rejected hermes run must not leave session state"
    );
}

fn run_with_stdin(dir: &Path, args: &[&str], envs: &[(&str, &str)], stdin: &str) -> CmdOutput {
    let options = CmdOptions::new()
        .with_cwd(dir)
        .with_envs(envs)
        .with_stdin_str(stdin);
    run_resolved("agent-session", args, &options)
}

#[test]
fn send_keys_only_skips_buffer_and_maps_special_keys() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    write_session_record(&state_dir, "steer", "codex", "hs-codex-steer");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "send",
            "steer",
            "--key",
            "c-c",
            "--key",
            "escape",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str())],
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.send.v1");
    let result = data(&value);
    assert_eq!(result["sent_text"], false);
    let keys = result["keys"].as_array().expect("keys array");
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], "c-c");
    assert_eq!(keys[1], "escape");

    let calls = tmux_calls(&tmux_log);
    // Keys-only: no buffer is loaded or pasted.
    assert!(
        !calls.iter().any(|call| call
            .first()
            .is_some_and(|arg| arg == "load-buffer" || arg == "paste-buffer")),
        "keys-only send must not touch buffers: {calls:?}"
    );
    // Special keys map to their tmux names and are sent in order.
    let ctrl_c_idx = calls
        .iter()
        .position(|call| {
            call.first().is_some_and(|arg| arg == "send-keys")
                && call.last().is_some_and(|arg| arg == "C-c")
        })
        .expect("send-keys C-c call");
    let escape_idx = calls
        .iter()
        .position(|call| {
            call.first().is_some_and(|arg| arg == "send-keys")
                && call.last().is_some_and(|arg| arg == "Escape")
        })
        .expect("send-keys Escape call");
    assert!(
        ctrl_c_idx < escape_idx,
        "keys must send in order: {calls:?}"
    );
}

#[test]
fn send_rejects_stopped_session_without_delivering() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    write_session_record(&state_dir, "steer", "codex", "hs-codex-steer");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "send",
            "steer",
            "--text",
            "x",
            "--key",
            "enter",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_HAS_SESSION", "0"),
        ],
    );
    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "session-not-running");
    // Nothing is delivered to a dead pane.
    let calls = tmux_calls(&tmux_log);
    assert!(
        !calls
            .iter()
            .any(|call| call.first().is_some_and(|arg| arg == "load-buffer"
                || arg == "paste-buffer"
                || arg == "send-keys")),
        "stopped session must not receive input: {calls:?}"
    );
}

#[test]
fn send_reads_stdin_and_rejects_empty_or_dual_text() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    write_session_record(&state_dir, "steer", "codex", "hs-codex-steer");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();
    let env_refs = [("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str())];
    let secret = "stdin-secret-payload";

    // --text-stdin delivers without leaking the secret into output or argv.
    let stdin_out = run_with_stdin(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "send",
            "steer",
            "--text-stdin",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &env_refs,
        secret,
    );
    assert_eq!(stdin_out.code, 0, "stderr={}", stdin_out.stderr_text());
    assert_no_secret(&stdin_out, secret);
    assert_eq!(data(&stdin_out.stdout_json())["sent_text"], true);
    let stdin_calls = tmux_calls(&tmux_log);
    assert!(
        stdin_calls
            .iter()
            .any(|call| call.first().is_some_and(|arg| arg == "paste-buffer")),
        "stdin text should be pasted: {stdin_calls:?}"
    );
    for call in &stdin_calls {
        for arg in call {
            assert!(
                !arg.contains(secret),
                "secret leaked into tmux argv: {call:?}"
            );
        }
    }

    // --text + --text-stdin together is a usage error.
    let dual = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "send",
            "steer",
            "--text",
            "x",
            "--text-stdin",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &env_refs,
    );
    assert_eq!(dual.code, 64, "stderr={}", dual.stderr_text());
    assert_eq!(dual.stdout_json()["error"]["code"], "multiple-text-sources");

    // An empty --text is a no-op, not a false success: caught by empty-send.
    let empty_text = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "send",
            "steer",
            "--text",
            "",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &env_refs,
    );
    assert_eq!(empty_text.code, 64, "stderr={}", empty_text.stderr_text());
    assert_eq!(empty_text.stdout_json()["error"]["code"], "empty-send");
}

#[test]
fn glance_truncates_to_tail_and_leaves_updated_at() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    let session = write_session_record(&state_dir, "look", "claude", "hs-claude-look");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "glance",
            "look",
            "--tail",
            "2",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            ("AGENT_SESSION_FAKE_TMUX_CAPTURE", "l1\nl2\nl3\nl4\nl5\n"),
        ],
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let result = data(&output.stdout_json()).clone();
    // Client-side truncation keeps only the last N lines.
    assert_eq!(result["tail"], "l4\nl5\n");
    // The capture is requested with the right tail window and target.
    let calls = tmux_calls(&tmux_log);
    let capture = calls
        .iter()
        .find(|call| call.first().is_some_and(|arg| arg == "capture-pane"))
        .expect("capture-pane call");
    assert!(
        capture.contains(&"-S".to_string()) && capture.contains(&"-2".to_string()),
        "capture must request the tail window: {capture:?}"
    );
    assert!(
        capture.contains(&"hs-claude-look".to_string()),
        "capture must target the session pane: {capture:?}"
    );

    // glance is a passive poll: it must not bump updated_at.
    let after: Value = serde_json::from_str(
        &fs::read_to_string(session.join("session.json")).expect("re-read record"),
    )
    .expect("parse record");
    assert_eq!(
        after["updated_at"], "2000-01-01T00:00:00Z",
        "glance must not bump updated_at"
    );
}

#[test]
fn glance_strips_trailing_blank_pane_padding() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let (tmux_bin, tmux_log) = fake_tmux(tmp.path());
    write_session_record(&state_dir, "look", "codex", "hs-codex-look");

    let state_arg = state_dir.to_string_lossy().to_string();
    let tmux_arg = tmux_bin.to_string_lossy().to_string();
    let tmux_log_arg = tmux_log.to_string_lossy().to_string();

    // capture-pane pads a short, top-anchored pane to the full height with blank
    // lines; glance must show the real content, not the empty bottom rows.
    let output = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "glance",
            "look",
            "--tail",
            "10",
            "--tmux-bin",
            &tmux_arg,
            "--format",
            "json",
        ],
        &[
            ("AGENT_SESSION_FAKE_TMUX_LOG", tmux_log_arg.as_str()),
            (
                "AGENT_SESSION_FAKE_TMUX_CAPTURE",
                "top-line\nsecond-line\n\n\n\n\n\n",
            ),
        ],
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let tail = data(&output.stdout_json())["tail"]
        .as_str()
        .expect("tail")
        .to_string();
    assert_eq!(tail, "top-line\nsecond-line\n", "tail={tail:?}");
}

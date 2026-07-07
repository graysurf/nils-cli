use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

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

if [ "${AGENT_SESSION_FAKE_TMUX_FAIL:-}" = "$1" ]; then
  echo "fake tmux failed at $1" >&2
  exit 42
fi

if [ "$1" = "has-session" ] && [ "${AGENT_SESSION_FAKE_TMUX_HAS_SESSION:-1}" = "0" ]; then
  exit 1
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

if [ "$1" = "new-session" ] && [ "${AGENT_SESSION_FAKE_CHMOD_AFTER_NEW_SESSION+x}" = "x" ]; then
  chmod "${AGENT_SESSION_FAKE_CHMOD_MODE:-0500}" "$AGENT_SESSION_FAKE_CHMOD_AFTER_NEW_SESSION"
fi

if [ "$1" = "new-session" ] && [ "${AGENT_SESSION_FAKE_MKDIR_AFTER_NEW_SESSION+x}" = "x" ]; then
  mkdir -p "$AGENT_SESSION_FAKE_MKDIR_AFTER_NEW_SESSION"
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
            "-s".to_string(),
            result["tmux_session"].as_str().unwrap().to_string(),
            "-c".to_string(),
            cwd_arg.clone(),
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
        &env_refs,
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
        &env_refs,
    );
    assert_eq!(delete.code, 0, "stderr={}", delete.stderr_text());
    let delete_json = delete.stdout_json();
    assert_eq!(delete_json["schema_version"], "cli.agent-session.delete.v1");
    assert_eq!(data(&delete_json)["deleted"], true);

    let list_again = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &env_refs,
    );
    assert_eq!(list_again.code, 0, "stderr={}", list_again.stderr_text());
    assert_eq!(data(&list_again.stdout_json()).as_array().unwrap().len(), 0);
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
fn failure_paths_return_json_without_leaking_prompt_or_orphaning_state() {
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
    ];
    let env_refs = [
        (envs[0].0, envs[0].1.as_str()),
        (envs[1].0, envs[1].1.as_str()),
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
        !state_dir.join("sessions").join("fail-start").exists(),
        "failed tmux startup should not leave session state"
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
        calls.iter().any(|call| call
            == &vec![
                "kill-session".to_string(),
                "-t".to_string(),
                "hs-codex-paste-fail".to_string(),
            ]),
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
    "started_at": "2000-01-01T00:00:00Z"
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
fn start_reports_persisted_state_when_post_launch_resume_write_fails() {
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

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["id"], "write-fail");
    assert_eq!(result["status"], "running");
    assert_eq!(result["resumable"], false);
    let _ = fs::set_permissions(&chmod_dir, fs::Permissions::from_mode(0o700));
    let record_path = chmod_dir.join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert!(record.get("provider_resume").is_none());
    assert!(!record_path.with_file_name("resume.json").exists());
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
    write_resumable_session_record_with_agent_bin(
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
            "-s".to_string(),
            "hs-codex-recoverable".to_string(),
            "-c".to_string(),
            cwd.to_string_lossy().to_string(),
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

    let record_path = state_dir.join("sessions/recoverable/session.json");
    let record: Value = serde_json::from_str(&fs::read_to_string(&record_path).unwrap()).unwrap();
    assert_eq!(record["id"], "recoverable");
    assert_eq!(record["runtime"]["generation"], 2);
    assert_ne!(record["updated_at"], "2000-01-01T00:00:00Z");
    assert_eq!(record["agent_bin"], codex_arg);
    assert!(
        record_path.with_file_name("resume.json").is_file(),
        "resume should refresh the durable sidecar"
    );
}

#[test]
fn resume_reports_persisted_state_when_runtime_refresh_write_fails() {
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
    let chmod_dir_arg = session.to_string_lossy().to_string();
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
            ("AGENT_SESSION_FAKE_CHMOD_AFTER_NEW_SESSION", &chmod_dir_arg),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    let result = data(&value);
    assert_eq!(result["status"], "running");
    assert_eq!(result["resumable"], true);
    assert_eq!(result["updated_at"], "2000-01-01T00:00:00Z");
    let _ = fs::set_permissions(&session, fs::Permissions::from_mode(0o700));
    let record_path = session.join("session.json");
    let record: Value =
        serde_json::from_str(&fs::read_to_string(&record_path).expect("session record"))
            .expect("record json");
    assert_eq!(record["runtime"]["generation"], 1);
    assert_eq!(record["updated_at"], "2000-01-01T00:00:00Z");
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
    let malformed = write_session_record(
        &state_dir,
        "malformed-sidecar",
        "codex",
        "hs-codex-malformed-sidecar",
    );
    fs::write(malformed.join("resume.json"), "{not-json").expect("malformed resume sidecar");

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
            &[("AGENT_SESSION_FAKE_TMUX_LOG", &tmux_log_arg)],
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
        calls.iter().any(|call| call
            == &vec![
                "paste-buffer".to_string(),
                "-b".to_string(),
                "steer-send".to_string(),
                "-d".to_string(),
                "-t".to_string(),
                "hs-codex-steer:0.0".to_string(),
            ]),
        "missing paste-buffer -d call: {calls:?}"
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
        !session.join("send-input").exists(),
        "send-input temp file should be cleaned up"
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

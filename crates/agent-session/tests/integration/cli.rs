use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;
use serde_json::Value;

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
    ];
    let env_refs = [
        (envs[0].0, envs[0].1.as_str()),
        (envs[1].0, envs[1].1.as_str()),
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
    let id = data(&start.stdout_json())["id"]
        .as_str()
        .expect("id")
        .to_string();

    let list = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &env_refs,
    );
    assert_eq!(list.code, 0, "stderr={}", list.stderr_text());
    let list_json = list.stdout_json();
    assert_eq!(list_json["schema_version"], "cli.agent-session.list.v1");
    let list_data = data(&list_json).as_array().expect("list data");
    assert_eq!(list_data.len(), 1);
    assert_eq!(list_data[0]["id"], id);
    assert_eq!(list_data[0]["status"], "running");

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
    assert!(
        command_data["ssh_attach_command"]
            .as_str()
            .unwrap()
            .starts_with("ssh -t sympoies"),
        "missing ssh attach command: {command_data}"
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

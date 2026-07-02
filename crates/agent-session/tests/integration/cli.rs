use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;

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
printf '%s\n' "$*" >> "$AGENT_SESSION_FAKE_TMUX_LOG"
case "$1" in
  has-session|new-session|load-buffer|paste-buffer|send-keys|kill-session|capture-pane|attach-session)
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
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
    let cwd = tmp.path().join("repo");
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
    let combined = format!("{}{}", output.stdout_text(), output.stderr_text());
    assert!(
        !combined.contains("sk-proj-secret"),
        "prompt content leaked into output: {combined}"
    );
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-session.start.v1");
    assert_eq!(value["command"], "agent-session start");
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["agent"], "codex");
    assert_eq!(value["result"]["cwd"], cwd_arg);
    assert!(
        value["result"]["attach_command"]
            .as_str()
            .unwrap()
            .starts_with("tmux attach -t hs-codex-")
    );
    assert!(
        value["result"]["ssh_attach_command"]
            .as_str()
            .unwrap()
            .starts_with("ssh -t sympoies ")
    );

    let id = value["result"]["id"].as_str().expect("id");
    let prompt_file = state_dir.join("sessions").join(id).join("prompt.md");
    assert_eq!(
        fs::read_to_string(prompt_file).expect("prompt file"),
        prompt
    );

    let tmux_log = fs::read_to_string(tmux_log).expect("tmux log");
    assert!(
        tmux_log.contains("new-session"),
        "missing new-session call: {tmux_log}"
    );
    assert!(
        tmux_log.contains("load-buffer"),
        "missing load-buffer call: {tmux_log}"
    );
    assert!(
        tmux_log.contains("paste-buffer"),
        "missing paste-buffer call: {tmux_log}"
    );
    assert!(
        tmux_log.contains("send-keys"),
        "missing send-keys call: {tmux_log}"
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
    let id = start.stdout_json()["result"]["id"]
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
    assert_eq!(list_json["command"], "agent-session list");
    assert_eq!(list_json["ok"], true);
    assert_eq!(list_json["results"].as_array().unwrap().len(), 1);
    assert_eq!(list_json["results"][0]["id"], id);
    assert_eq!(list_json["results"][0]["status"], "running");

    let command = run(
        tmp.path(),
        &[
            "--state-dir",
            &state_arg,
            "--host",
            "sympoies",
            "command",
            &id,
        ],
        &env_refs,
    );
    assert_eq!(command.code, 0, "stderr={}", command.stderr_text());
    assert!(
        command.stdout_text().contains("ssh -t sympoies"),
        "missing ssh attach command: {}",
        command.stdout_text()
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
    assert_eq!(delete_json["ok"], true);
    assert_eq!(delete_json["result"]["deleted"], true);

    let list_again = run(
        tmp.path(),
        &["--state-dir", &state_arg, "list", "--format", "json"],
        &env_refs,
    );
    assert_eq!(list_again.code, 0, "stderr={}", list_again.stderr_text());
    assert_eq!(
        list_again.stdout_json()["results"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

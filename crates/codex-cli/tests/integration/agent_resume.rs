use std::fs;
use std::path::{Path, PathBuf};

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions};
use pretty_assertions::assert_eq;

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn write_codex_session_meta(sessions: &Path, id: &str, cwd: &Path) {
    fs::create_dir_all(sessions).unwrap();
    let line = format!(
        r#"{{"timestamp":"2099-01-01T00:00:00Z","type":"session_meta","payload":{{"id":"{id}","session_id":"{id}","cwd":"{}","source":"cli","timestamp":"2099-01-01T00:00:00Z"}}}}"#,
        cwd.display()
    );
    fs::write(sessions.join("rollout.jsonl"), line).unwrap();
}

#[test]
fn resume_control_char_id_is_usage_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let options = CmdOptions::default().with_cwd(tmp.path());
    let output = cmd::run_with(&codex_cli_bin(), &["agent", "resume", "bad\tid"], &options);
    assert_eq!(output.code, 64, "stderr: {}", output.stderr_text());
}

#[test]
fn resume_unknown_id_returns_data_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let codex_home = tmp.path().join("codex");
    fs::create_dir_all(codex_home.join("sessions")).unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();

    let options = CmdOptions::default()
        .with_cwd(&elsewhere)
        .with_env("CODEX_HOME", codex_home.to_str().unwrap());
    let output = cmd::run_with(
        &codex_cli_bin(),
        &["agent", "resume", "absent-id"],
        &options,
    );

    assert_eq!(output.code, 65, "stderr: {}", output.stderr_text());
    assert!(output.stderr_text().contains("no Codex session history"));
}

#[test]
fn resume_launches_codex_with_recorded_cwd_from_unrelated_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    // A recorded cwd with a space exercises exact argv passing.
    let repo = tmp.path().join("recorded repo");
    fs::create_dir_all(&repo).unwrap();
    let codex_home = tmp.path().join("codex");
    write_codex_session_meta(&codex_home.join("sessions"), "sess-x", &repo);
    let elsewhere = tmp.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();

    let argv_log = tmp.path().join("codex-argv.txt");
    let stub_dir = tmp.path().join("stub");
    fs::create_dir_all(&stub_dir).unwrap();
    nils_test_support::write_exe(
        &stub_dir,
        "codex",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 7\n",
            argv_log.display()
        ),
    );

    let options = CmdOptions::default()
        .with_cwd(&elsewhere)
        .with_env("CODEX_HOME", codex_home.to_str().unwrap())
        .with_path_prepend(&stub_dir);
    let output = cmd::run_with(&codex_cli_bin(), &["agent", "resume", "sess-x"], &options);

    assert_eq!(output.code, 7, "stderr: {}", output.stderr_text());
    let argv = fs::read_to_string(&argv_log).unwrap();
    let lines: Vec<&str> = argv.lines().collect();
    assert_eq!(
        lines,
        vec![
            "resume",
            "sess-x",
            "--cd",
            repo.to_str().unwrap(),
            "--no-alt-screen",
        ]
    );
}

#[test]
fn resume_cd_override_to_missing_directory_is_runtime_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let missing = tmp.path().join("does-not-exist");
    let options = CmdOptions::default().with_cwd(tmp.path());
    let output = cmd::run_with(
        &codex_cli_bin(),
        &[
            "agent",
            "resume",
            "any-id",
            "--cd",
            missing.to_str().unwrap(),
        ],
        &options,
    );

    assert_eq!(output.code, 1, "stderr: {}", output.stderr_text());
    assert!(output.stderr_text().contains("not an existing directory"));
}

#[test]
fn resume_truncated_scan_returns_runtime_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let codex_home = tmp.path().join("codex");
    let sessions = codex_home.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    // Two matching entries plus a one-entry budget forces the bounded scan to
    // truncate before it can decide, without ever launching codex.
    for index in 0..2 {
        write_codex_session_meta(&sessions, "trunc-id", &repo);
        fs::rename(
            sessions.join("rollout.jsonl"),
            sessions.join(format!("rollout-{index}.jsonl")),
        )
        .unwrap();
    }

    let options = CmdOptions::default()
        .with_cwd(tmp.path())
        .with_env("CODEX_HOME", codex_home.to_str().unwrap())
        .with_env("AGENT_SESSION_CODEX_RESUME_SCAN_MAX_ENTRIES", "1");
    let output = cmd::run_with(&codex_cli_bin(), &["agent", "resume", "trunc-id"], &options);

    assert_eq!(output.code, 1, "stderr: {}", output.stderr_text());
    assert!(output.stderr_text().contains("truncated"));
}

#[test]
fn resume_cd_override_bypasses_resolution() {
    let tmp = tempfile::TempDir::new().unwrap();
    let override_dir = tmp.path().join("override target");
    fs::create_dir_all(&override_dir).unwrap();
    // Empty history root: automatic resolution would fail with NotFound, so a
    // successful launch proves `--cd` bypassed resolution entirely.
    let codex_home = tmp.path().join("codex");
    fs::create_dir_all(codex_home.join("sessions")).unwrap();

    let argv_log = tmp.path().join("codex-argv.txt");
    let stub_dir = tmp.path().join("stub");
    fs::create_dir_all(&stub_dir).unwrap();
    nils_test_support::write_exe(
        &stub_dir,
        "codex",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 7\n",
            argv_log.display()
        ),
    );

    let options = CmdOptions::default()
        .with_cwd(tmp.path())
        .with_env("CODEX_HOME", codex_home.to_str().unwrap())
        .with_path_prepend(&stub_dir);
    let output = cmd::run_with(
        &codex_cli_bin(),
        &[
            "agent",
            "resume",
            "unresolved-id",
            "--cd",
            override_dir.to_str().unwrap(),
        ],
        &options,
    );

    assert_eq!(output.code, 7, "stderr: {}", output.stderr_text());
    let argv = fs::read_to_string(&argv_log).unwrap();
    let lines: Vec<&str> = argv.lines().collect();
    assert_eq!(
        lines,
        vec![
            "resume",
            "unresolved-id",
            "--cd",
            override_dir.to_str().unwrap(),
            "--no-alt-screen",
        ]
    );
}

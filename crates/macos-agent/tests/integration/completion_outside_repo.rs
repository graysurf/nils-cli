use tempfile::TempDir;

use crate::common;
#[test]
fn completion_export_succeeds_outside_git_repo() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["completion", "zsh"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let stdout = out.stdout_text();
    assert!(stdout.contains("#compdef macos-agent"), "stdout: {stdout}");
}

#[test]
fn completion_rejects_unknown_shell_outside_git_repo() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["completion", "fish"]);
    assert_ne!(out.code, 0);
    let stderr = out.stderr_text();
    assert!(stderr.contains("invalid value"), "stderr: {stderr}");
    assert!(stderr.contains("fish"), "stderr: {stderr}");
}

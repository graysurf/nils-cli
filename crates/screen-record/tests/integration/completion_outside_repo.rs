use crate::common;
use tempfile::TempDir;

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let harness = common::ScreenRecordHarness::new();
    let dir = TempDir::new().expect("tempdir");
    let out = harness.run(dir.path(), &["completion", "zsh"]);

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("#compdef screen-record"));
}

#[test]
fn completion_rejects_unknown_shell_outside_git_repo() {
    let harness = common::ScreenRecordHarness::new();
    let dir = TempDir::new().expect("tempdir");
    let out = harness.run(dir.path(), &["completion", "fish"]);

    assert_ne!(out.code, 0);
    assert!(out.stderr_text().contains("unsupported completion shell"));
    assert!(out.stderr_text().contains("fish"));
}

#[test]
fn completion_bash_export_is_normalized() {
    let harness = common::ScreenRecordHarness::new();
    let dir = TempDir::new().expect("tempdir");
    let out = harness.run(dir.path(), &["completion", "bash"]);

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let stdout = out.stdout_text();
    assert!(
        stdout.contains("_screen__record()"),
        "missing bash completion entry point: {stdout}"
    );
    // clap_complete emits `__subcmd__` separators in its generated command
    // ids; the shipped `completions/bash/screen-record` asset must not carry
    // them, so the normalizer rewrites them before the script reaches stdout.
    assert!(
        !stdout.contains("__subcmd__"),
        "bash completion leaked an un-normalized subcommand separator"
    );
}

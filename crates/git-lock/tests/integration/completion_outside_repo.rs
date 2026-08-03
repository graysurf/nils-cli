use crate::common;
use std::process::{Command, Stdio};

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = Command::new(common::git_lock_bin())
        .args(["completion", "zsh"])
        .current_dir(temp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run git-lock completion zsh");

    assert!(
        output.status.success(),
        "expected exit code 0, got: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("#compdef git-lock"),
        "missing zsh completion header: {stdout}"
    );
    assert!(
        !stdout.contains("Not a Git repository"),
        "unexpected repo warning: {stdout}"
    );
}

#[test]
fn completion_rejects_unknown_shell_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = Command::new(common::git_lock_bin())
        .args(["completion", "fish"])
        .current_dir(temp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run git-lock completion fish");

    assert!(
        !output.status.success(),
        "expected non-zero exit code for unknown shell, got: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Not a Git repository"),
        "unexpected repo warning: {stderr}"
    );
}

#[test]
fn completion_bash_export_is_normalized() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = Command::new(common::git_lock_bin())
        .args(["completion", "bash"])
        .current_dir(temp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run git-lock completion bash");

    assert!(
        output.status.success(),
        "expected exit code 0, got: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("_git-lock()"),
        "missing bash completion entry point: {stdout}"
    );
    // clap_complete emits `__subcmd__` separators in its generated command
    // ids; the shipped `completions/bash/git-lock` asset must not carry them,
    // so the normalizer rewrites them before the script reaches stdout.
    assert!(
        !stdout.contains("__subcmd__"),
        "bash completion leaked an un-normalized subcommand separator"
    );
    assert!(
        !stdout.contains("Not a Git repository"),
        "unexpected repo warning: {stdout}"
    );
}

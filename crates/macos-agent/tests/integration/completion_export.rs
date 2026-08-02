//! Shell-completion export contract. `macos-agent` ships completion assets for
//! both shells, and rendering them must not depend on macOS automation
//! permissions or on being run inside a git repository.

use std::process::{Command, Stdio};

use nils_test_support::bin::resolve;

fn run_completion(shell: &str) -> std::process::Output {
    let temp = tempfile::TempDir::new().unwrap();
    Command::new(resolve("macos-agent"))
        .args(["completion", shell])
        .current_dir(temp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap_or_else(|error| panic!("run macos-agent completion {shell}: {error}"))
}

#[test]
fn completion_zsh_export_succeeds_outside_git_repo() {
    let output = run_completion("zsh");

    assert!(
        output.status.success(),
        "expected exit code 0, got: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("#compdef macos-agent"),
        "missing zsh completion header: {stdout}"
    );
}

#[test]
fn completion_bash_export_is_normalized() {
    let output = run_completion("bash");

    assert!(
        output.status.success(),
        "expected exit code 0, got: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("_macos-agent()"),
        "missing bash completion entry point: {stdout}"
    );
    // clap_complete emits `__subcmd__` separators in its generated command
    // ids; the shipped `completions/bash/macos-agent` asset must not carry
    // them, so the normalizer rewrites them before stdout.
    assert!(
        !stdout.contains("__subcmd__"),
        "bash completion leaked an un-normalized subcommand separator"
    );
}

#[test]
fn completion_rejects_an_unsupported_shell() {
    let output = run_completion("fish");

    assert!(
        !output.status.success(),
        "expected non-zero exit code for unknown shell, got: {output:?}"
    );
}

//! Shell-completion export contract. Both shipped shells must render from a
//! directory that is not a git repository, and the bash script must already be
//! normalized when it reaches stdout.

use std::process::{Command, Stdio};

use nils_test_support::bin::resolve;

fn run_completion(shell: &str) -> std::process::Output {
    let temp = tempfile::TempDir::new().unwrap();
    Command::new(resolve("git-scope"))
        .args(["completion", shell])
        .current_dir(temp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap_or_else(|error| panic!("run git-scope completion {shell}: {error}"))
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
        stdout.contains("#compdef git-scope"),
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
        stdout.contains("_git__scope()"),
        "missing bash completion entry point: {stdout}"
    );
    // clap_complete emits `__subcmd__` separators in its generated command
    // ids; the shipped `completions/bash/git-scope` asset must not carry them,
    // so the normalizer rewrites them before the script reaches stdout.
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

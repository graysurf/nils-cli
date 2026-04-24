use std::path::Path;
use std::process::Output;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{options_in_dir_with_envs, run_with};

fn git_scope_bin() -> std::path::PathBuf {
    resolve("git-scope")
}

fn run_git_scope_outside_repo(dir: &Path, args: &[&str]) -> Output {
    let options = options_in_dir_with_envs(dir, &[]);
    run_with(&git_scope_bin(), args, &options).into_output()
}

#[test]
fn help_flag_succeeds_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = run_git_scope_outside_repo(temp.path(), &["--help"]);

    assert!(
        output.status.success(),
        "expected exit code 0, got: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage: git-scope"),
        "missing Usage: {stdout}"
    );
    assert!(
        !stdout.contains("Not a Git repository"),
        "unexpected repo warning: {stdout}"
    );
}

#[test]
fn help_subcommand_succeeds_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = run_git_scope_outside_repo(temp.path(), &["help"]);

    assert!(
        output.status.success(),
        "expected exit code 0, got: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage: git-scope"),
        "missing Usage: {stdout}"
    );
    assert!(
        !stdout.contains("Not a Git repository"),
        "unexpected repo warning: {stdout}"
    );
}

#[test]
fn version_flag_succeeds_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = run_git_scope_outside_repo(temp.path(), &["--version"]);

    assert!(
        output.status.success(),
        "expected exit code 0, got: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("git-scope"),
        "missing binary name: {stdout}"
    );
    assert!(
        !stdout.contains("Not a Git repository"),
        "unexpected repo warning: {stdout}"
    );
}

#[test]
fn root_help_does_not_advertise_subcommand_print_flag() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = run_git_scope_outside_repo(temp.path(), &["--help"]);

    assert!(
        output.status.success(),
        "expected exit code 0, got: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("-p, --print"),
        "root help should not show subcommand-only flags: {stdout}"
    );
}

#[test]
fn subcommand_help_uses_subcommand_scope() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = run_git_scope_outside_repo(temp.path(), &["all", "--help"]);

    assert!(
        output.status.success(),
        "expected exit code 0, got: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage: all [OPTIONS]"),
        "expected subcommand usage in help output: {stdout}"
    );
    assert!(
        stdout.contains("-p, --print"),
        "expected subcommand print option in help output: {stdout}"
    );
}

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = run_git_scope_outside_repo(temp.path(), &["completion", "zsh"]);

    assert!(
        output.status.success(),
        "expected exit code 0, got: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("#compdef git-scope"),
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
    let output = run_git_scope_outside_repo(temp.path(), &["completion", "fish"]);

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

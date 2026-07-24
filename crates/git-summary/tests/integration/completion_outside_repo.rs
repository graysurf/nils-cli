use crate::common;
use std::process::{Command, Stdio};

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = Command::new(common::git_summary_bin())
        .args(["completion", "zsh"])
        .current_dir(temp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run git-summary completion zsh");

    assert!(
        output.status.success(),
        "expected exit code 0, got: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("#compdef git-summary"),
        "missing zsh completion header: {stdout}"
    );
    assert!(
        !stdout.contains("Not a Git repository"),
        "unexpected repo warning: {stdout}"
    );
}

#[test]
fn completion_help_flags_render_subcommand_help() {
    let temp = tempfile::TempDir::new().unwrap();

    for help_flag in ["--help", "-h"] {
        let output = Command::new(common::git_summary_bin())
            .args(["completion", help_flag])
            .current_dir(temp.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run git-summary completion help flag");

        assert!(
            output.status.success(),
            "expected exit code 0 for `completion {help_flag}`, got: {output:?}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Usage: git-summary completion"),
            "missing completion usage line for `{help_flag}`: {stdout}"
        );
        assert!(
            stdout.contains("<shell>"),
            "missing shell argument for `{help_flag}`: {stdout}"
        );
        // The flag-parity audit compares this help against the generated
        // completion, so the propagated global flags must be listed here.
        for flag in ["--format", "--no-mailmap"] {
            assert!(
                stdout.contains(flag),
                "missing global flag `{flag}` for `{help_flag}`: {stdout}"
            );
        }
    }
}

#[test]
fn completion_rejects_help_flag_with_extra_arguments() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = Command::new(common::git_summary_bin())
        .args(["completion", "--help", "bash"])
        .current_dir(temp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run git-summary completion --help bash");

    assert!(
        !output.status.success(),
        "expected non-zero exit code for extra arguments, got: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("expected `git-summary completion <bash|zsh>`"),
        "missing usage error: {stderr}"
    );
}

#[test]
fn completion_rejects_unknown_shell_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = Command::new(common::git_summary_bin())
        .args(["completion", "fish"])
        .current_dir(temp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run git-summary completion fish");

    assert!(
        !output.status.success(),
        "expected non-zero exit code for unknown shell, got: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported completion shell"),
        "missing unsupported shell error: {stderr}"
    );
    assert!(
        !stderr.contains("Not a Git repository"),
        "unexpected repo warning: {stderr}"
    );
}

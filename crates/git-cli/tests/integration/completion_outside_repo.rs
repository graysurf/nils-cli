use crate::common;
use common::GitCliHarness;

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["completion", "zsh"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    let stdout = output.stdout_text();
    assert!(stdout.contains("#compdef git-cli"));
    assert!(!stdout.contains("Not a git repository"));
}

#[test]
fn completion_rejects_unknown_shell_outside_git_repo() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["completion", "fish"]);

    assert_eq!(output.code, 1);
    assert!(
        output
            .stderr_text()
            .contains("unsupported completion shell")
    );
    assert!(!output.stderr_text().contains("Not a git repository"));
}

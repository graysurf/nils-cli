use crate::common;
use tempfile::TempDir;

fn as_str(output: &[u8]) -> String {
    String::from_utf8_lossy(output).to_string()
}

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let dir = TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(dir.path(), &["completion", "zsh"], &[], None);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={}",
        as_str(&output.stderr)
    );
    assert!(as_str(&output.stdout).contains("#compdef semantic-commit"));
}

#[test]
fn completion_rejects_unknown_shell_outside_git_repo() {
    let dir = TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(dir.path(), &["completion", "fish"], &[], None);

    assert_ne!(output.status.code(), Some(0));
    assert!(as_str(&output.stderr).contains("unsupported completion shell"));
    assert!(as_str(&output.stderr).contains("fish"));
}

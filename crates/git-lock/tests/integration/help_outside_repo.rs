use crate::common;
use common::run_git_lock_output;

#[test]
fn help_flag_outside_repo_exits_zero() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = run_git_lock_output(
        dir.path(),
        &["--help"],
        &[("ZSH_CACHE_DIR", dir.path().to_str().unwrap())],
        None,
    );

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: git-lock"));
    assert!(!stdout.contains("Not a Git repository"));
}

#[test]
fn help_subcommand_outside_repo_exits_zero() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = run_git_lock_output(
        dir.path(),
        &["help"],
        &[("ZSH_CACHE_DIR", dir.path().to_str().unwrap())],
        None,
    );

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: git-lock"));
    assert!(!stdout.contains("Not a Git repository"));
}

#[test]
fn version_flag_outside_repo_exits_zero() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = run_git_lock_output(
        dir.path(),
        &["--version"],
        &[("ZSH_CACHE_DIR", dir.path().to_str().unwrap())],
        None,
    );

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("git-lock"));
    assert!(!stdout.contains("Not a Git repository"));
}

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
fn subcommand_help_flag_outside_repo_exits_zero_for_each_subcommand() {
    for subcmd in [
        "lock",
        "unlock",
        "list",
        "copy",
        "delete",
        "diff",
        "tag",
        "completion",
    ] {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let output = run_git_lock_output(
            dir.path(),
            &[subcmd, "--help"],
            &[("ZSH_CACHE_DIR", dir.path().to_str().unwrap())],
            None,
        );
        assert!(
            output.status.success(),
            "expected `git-lock {subcmd} --help` to exit 0"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.starts_with(&format!("Usage: git-lock {subcmd}")),
            "expected stdout to begin with `Usage: git-lock {subcmd}`; got: {stdout}"
        );
        assert!(
            !stdout.contains("Not a Git repository"),
            "expected `--help` to short-circuit before the repo check"
        );
    }
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

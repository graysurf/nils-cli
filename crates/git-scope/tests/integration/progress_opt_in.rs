use crate::common;
use std::fs;

#[test]
fn progress_opt_in_preserves_stdout_and_is_silent_in_non_tty() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("a.txt"), "BASE").unwrap();
    fs::write(root.join("b.txt"), "BASE").unwrap();
    common::git(root, &["add", "."]);
    common::git(root, &["commit", "-m", "init"]);

    fs::write(root.join("a.txt"), "STAGED").unwrap();
    common::git(root, &["add", "a.txt"]);
    fs::write(root.join("b.txt"), "UNSTAGED").unwrap();

    {
        let baseline = common::run_git_scope(root, &["staged", "-p"], &[("NO_COLOR", "1")]);
        let output = common::run_git_scope_output(
            root,
            &["staged", "-p"],
            &[("NO_COLOR", "1"), ("GIT_SCOPE_PROGRESS", "1")],
        );
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert_eq!(
            stdout, baseline,
            "stdout changed under GIT_SCOPE_PROGRESS=1"
        );
        assert!(
            stderr.trim().is_empty(),
            "expected no stderr output in non-TTY tests: {stderr}"
        );
    }

    {
        let baseline = common::run_git_scope(root, &["all", "-p"], &[("NO_COLOR", "1")]);
        let output = common::run_git_scope_output(
            root,
            &["all", "-p"],
            &[("NO_COLOR", "1"), ("GIT_SCOPE_PROGRESS", "1")],
        );
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert_eq!(
            stdout, baseline,
            "stdout changed under GIT_SCOPE_PROGRESS=1"
        );
        assert!(
            stderr.trim().is_empty(),
            "expected no stderr output in non-TTY tests: {stderr}"
        );
    }

    {
        let baseline = common::run_git_scope(root, &["commit", "HEAD", "-p"], &[("NO_COLOR", "1")]);
        let output = common::run_git_scope_output(
            root,
            &["commit", "HEAD", "-p"],
            &[("NO_COLOR", "1"), ("GIT_SCOPE_PROGRESS", "1")],
        );
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        assert_eq!(
            stdout, baseline,
            "stdout changed under GIT_SCOPE_PROGRESS=1"
        );
        assert!(
            stderr.trim().is_empty(),
            "expected no stderr output in non-TTY tests: {stderr}"
        );
    }
}

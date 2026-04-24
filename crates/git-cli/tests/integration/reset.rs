use crate::common;
use common::{GitCliHarness, init_bare_remote, init_repo};
use nils_test_support::cmd::{CmdOutput, run_with};
use nils_test_support::git::{commit_file, git};
use std::fs;
use std::path::Path;

fn run_with_stdin(harness: &GitCliHarness, cwd: &Path, args: &[&str], stdin: &str) -> CmdOutput {
    let options = harness.cmd_options(cwd).with_stdin_str(stdin);
    run_with(&harness.git_cli_bin(), args, &options)
}

fn setup_repo_with_two_commits() -> tempfile::TempDir {
    let dir = init_repo();
    commit_file(dir.path(), "second.txt", "two\n", "second");
    dir
}

fn setup_repo_with_three_commits() -> tempfile::TempDir {
    let dir = init_repo();
    commit_file(dir.path(), "second.txt", "two\n", "second");
    commit_file(dir.path(), "third.txt", "three\n", "third");
    dir
}

fn git_path(dir: &Path, path: &str) -> std::path::PathBuf {
    let raw = git(dir, &["rev-parse", "--git-path", path]);
    std::path::PathBuf::from(raw.trim_end_matches(['\n', '\r']))
}

#[test]
fn reset_soft_abort_prints_prompt_and_aborts() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_two_commits();

    let output = run_with_stdin(&harness, dir.path(), &["reset", "soft", "1"], "n\n");

    assert_eq!(output.code, 1);
    assert!(output.stdout_text().contains("🧾 Commits to be rewound:"));
    assert!(output.stdout_text().contains("🚫 Aborted"));
    assert_eq!(output.stderr_text(), "");
}

#[test]
fn reset_mixed_invalid_count_errors() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["reset", "mixed", "0"]);

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("❌ Invalid commit count: 0 (must be a positive integer).")
    );
}

#[test]
fn reset_soft_insufficient_commits_errors() {
    let harness = GitCliHarness::new();
    let dir = init_repo();

    let output = harness.run(dir.path(), &["reset", "soft", "2"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert!(
        output
            .stderr_text()
            .contains("❌ Cannot resolve HEAD~2 (not enough commits?).")
    );
}

#[test]
fn reset_hard_confirm_succeeds() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_two_commits();

    let output = run_with_stdin(&harness, dir.path(), &["reset", "hard", "1"], "y\n");

    assert_eq!(output.code, 0);
    assert!(
        output
            .stdout_text()
            .contains("❓ Are you absolutely sure? [y/N] ")
    );
    assert!(
        output
            .stdout_text()
            .contains("✅ Hard reset completed. HEAD moved back to HEAD~1.")
    );
}

#[test]
fn reset_undo_dirty_tree_default_aborts() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_three_commits();
    git(dir.path(), &["reset", "--soft", "HEAD~1"]);

    let output = run_with_stdin(&harness, dir.path(), &["reset", "undo"], "\n");

    assert_eq!(output.code, 1);
    assert!(output.stdout_text().contains("Choose how to proceed:"));
    assert!(output.stdout_text().contains("🚫 Aborted"));
}

#[test]
fn reset_undo_no_reflog_entry_errors() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    git(dir.path(), &["init", "-q"]);

    let output = harness.run(dir.path(), &["reset", "undo"]);

    assert_eq!(output.code, 1);
    assert!(
        output
            .stdout_text()
            .contains("❌ Cannot resolve HEAD@{1} (no previous HEAD position in reflog).")
    );
}

#[test]
fn reset_undo_clean_tree_fast_path() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_two_commits();
    git(dir.path(), &["reset", "--hard", "HEAD~1"]);

    let output = harness.run(dir.path(), &["reset", "undo"]);

    assert_eq!(output.code, 0);
    assert!(
        output
            .stdout_text()
            .contains("✅ Working tree clean. Proceeding with: git reset --hard ")
    );
    assert!(
        output
            .stdout_text()
            .contains("✅ Repository reset back to previous HEAD: ")
    );
}

#[test]
fn reset_undo_dirty_choice_soft_succeeds() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_three_commits();
    git(dir.path(), &["reset", "--soft", "HEAD~1"]);

    let output = run_with_stdin(&harness, dir.path(), &["reset", "undo"], "1\n");

    assert_eq!(output.code, 0);
    assert!(
        output
            .stdout_text()
            .contains("🧷 Preserving INDEX (staged) and working tree. Running: git reset --soft ")
    );
    assert!(
        output
            .stdout_text()
            .contains("✅ HEAD moved back while preserving index + working tree: ")
    );
}

#[test]
fn reset_undo_dirty_choice_mixed_succeeds() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_three_commits();
    git(dir.path(), &["reset", "--soft", "HEAD~1"]);

    let output = run_with_stdin(&harness, dir.path(), &["reset", "undo"], "2\n");

    assert_eq!(output.code, 0);
    assert!(output.stdout_text().contains(
        "🧷 Preserving working tree but clearing INDEX (unstage all). Running: git reset --mixed "
    ));
    assert!(
        output
            .stdout_text()
            .contains("✅ HEAD moved back; working tree preserved; index reset: ")
    );
}

#[test]
fn reset_undo_dirty_choice_hard_decline_aborts() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_three_commits();
    git(dir.path(), &["reset", "--soft", "HEAD~1"]);

    let output = run_with_stdin(&harness, dir.path(), &["reset", "undo"], "3\nn\n");

    assert_eq!(output.code, 1);
    assert!(
        output
            .stdout_text()
            .contains("❓ Are you absolutely sure? [y/N] ")
    );
    assert!(output.stdout_text().contains("🚫 Aborted"));
}

#[test]
fn reset_back_head_abort() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_two_commits();

    let output = run_with_stdin(&harness, dir.path(), &["reset", "back-head"], "n\n");

    assert_eq!(output.code, 1);
    assert!(
        output
            .stdout_text()
            .contains("❓ Proceed with 'git checkout HEAD@{1}'? [y/N] ")
    );
    assert!(output.stdout_text().contains("🚫 Aborted"));
}

#[test]
fn reset_back_head_checkout_failure_prints_error() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_two_commits();
    fs::write(dir.path().join("second.txt"), "conflict\n").expect("write dirty change");

    let output = run_with_stdin(&harness, dir.path(), &["reset", "back-head"], "y\n");

    assert_eq!(output.code, 1);
    assert!(
        output
            .stdout_text()
            .contains("❌ Checkout failed (likely due to local changes or invalid reflog state).")
    );
}

#[test]
fn reset_back_checkout_detached_head_refuses() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_two_commits();
    git(dir.path(), &["checkout", "HEAD~1"]);

    let output = harness.run(dir.path(), &["reset", "back-checkout"]);

    assert_eq!(output.code, 1);
    assert!(output.stdout_text().contains(
        "❌ You are in a detached HEAD state. This function targets branch-to-branch checkouts."
    ));
}

#[test]
fn reset_back_checkout_missing_reflog_entry_errors() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_two_commits();
    let logs_head = git_path(dir.path(), "logs/HEAD");
    if logs_head.exists() {
        fs::remove_file(&logs_head).expect("remove reflog");
    }

    let output = harness.run(dir.path(), &["reset", "back-checkout"]);

    assert_eq!(output.code, 1);
    assert!(
        output
            .stdout_text()
            .contains("❌ Could not find a previous checkout that switched to main.")
    );
}

#[test]
fn reset_back_checkout_sha_like_from_refuses() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_two_commits();
    git(dir.path(), &["checkout", "HEAD~1"]);
    git(dir.path(), &["checkout", "main"]);

    let output = harness.run(dir.path(), &["reset", "back-checkout"]);

    assert_eq!(output.code, 1);
    assert!(
        output
            .stdout_text()
            .contains("❌ Previous 'from' looks like a commit SHA")
    );
}

#[test]
fn reset_remote_yes_mode_resets_and_cleans() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let remote = init_bare_remote();
    let remote_path = remote.path().to_string_lossy().to_string();

    git(dir.path(), &["remote", "add", "origin", &remote_path]);
    git(dir.path(), &["push", "-u", "origin", "main"]);

    commit_file(dir.path(), "local.txt", "local\n", "local commit");
    let untracked = dir.path().join("untracked.txt");
    fs::write(&untracked, "temp\n").expect("write untracked file");

    let output = harness.run(
        dir.path(),
        &[
            "reset",
            "remote",
            "--ref",
            "origin/main",
            "-r",
            "origin",
            "-b",
            "main",
            "--no-fetch",
            "--prune",
            "--clean",
            "--set-upstream",
            "-y",
        ],
    );

    assert_eq!(output.code, 0);
    assert!(
        output
            .stdout_text()
            .contains("✅ Done. 'main' now matches 'origin/main'.")
    );
    assert!(!untracked.exists(), "expected untracked file to be removed");
}

#[test]
fn reset_remote_help_prints_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["reset", "remote", "--help"]);

    assert_eq!(output.code, 0);
    assert!(output.stdout_text().contains(
        "git-reset-remote: overwrite current local branch with a remote-tracking branch"
    ));
    assert!(output.stdout_text().contains("Options:"));
}

#[test]
fn reset_remote_detached_head_refuses() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_two_commits();
    git(dir.path(), &["checkout", "HEAD~1"]);

    let output = harness.run(dir.path(), &["reset", "remote"]);

    assert_eq!(output.code, 1);
    assert!(
        output
            .stderr_text()
            .contains("❌ Detached HEAD. Switch to a branch first.")
    );
}

#[test]
fn reset_remote_missing_tracking_ref_errors() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let remote = init_bare_remote();
    let remote_path = remote.path().to_string_lossy().to_string();
    git(dir.path(), &["remote", "add", "origin", &remote_path]);

    let output = harness.run(
        dir.path(),
        &["reset", "remote", "--ref", "origin/main", "--no-fetch"],
    );

    assert_eq!(output.code, 1);
    assert!(
        output
            .stderr_text()
            .contains("❌ Remote-tracking branch not found: origin/main")
    );
}

#[test]
fn reset_remote_clean_prompt_skip_leaves_untracked() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    let remote = init_bare_remote();
    let remote_path = remote.path().to_string_lossy().to_string();
    git(dir.path(), &["remote", "add", "origin", &remote_path]);
    git(dir.path(), &["push", "-u", "origin", "main"]);

    let untracked = dir.path().join("untracked.txt");
    fs::write(&untracked, "temp\n").expect("write untracked file");

    let output = run_with_stdin(
        &harness,
        dir.path(),
        &[
            "reset",
            "remote",
            "--ref",
            "origin/main",
            "--no-fetch",
            "--clean",
        ],
        "y\nn\n",
    );

    assert_eq!(output.code, 0);
    assert!(output.stdout_text().contains("ℹ️  Skipped git clean -fd"));
    assert!(untracked.exists(), "expected untracked file to remain");
}

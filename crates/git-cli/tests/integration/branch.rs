use crate::common;
use common::{GitCliHarness, init_repo};
use nils_test_support::cmd::{CmdOutput, run_with};
use nils_test_support::git::{commit_file, git};
use std::path::Path;

fn run_with_stdin(harness: &GitCliHarness, cwd: &Path, args: &[&str], stdin: &str) -> CmdOutput {
    let options = harness.cmd_options(cwd).with_stdin_str(stdin);
    run_with(&harness.git_cli_bin(), args, &options)
}

fn setup_repo_with_branches() -> tempfile::TempDir {
    let dir = init_repo();
    commit_file(dir.path(), "file.txt", "base\n", "base");

    git(dir.path(), &["checkout", "-b", "feature-merged"]);
    commit_file(dir.path(), "feature.txt", "merged\n", "feature");
    git(dir.path(), &["checkout", "main"]);
    git(
        dir.path(),
        &["merge", "--no-ff", "feature-merged", "-m", "merge feature"],
    );

    git(dir.path(), &["checkout", "-b", "feature-squash"]);
    commit_file(dir.path(), "squash.txt", "squash\n", "squash work");
    let squash_sha = git(dir.path(), &["rev-parse", "HEAD"]);
    git(dir.path(), &["checkout", "main"]);
    git(dir.path(), &["cherry-pick", "-n", squash_sha.trim()]);
    git(dir.path(), &["commit", "-m", "squash commit"]);

    git(dir.path(), &["checkout", "-b", "develop"]);
    commit_file(dir.path(), "dev.txt", "dev\n", "dev work");
    git(dir.path(), &["checkout", "main"]);

    dir
}

fn setup_repo_with_real_squash() -> tempfile::TempDir {
    let dir = init_repo();
    commit_file(dir.path(), "file.txt", "base\n", "base");

    // A feature branch with multiple commits.
    git(dir.path(), &["checkout", "-b", "feature-multi-squash"]);
    commit_file(dir.path(), "a.txt", "alpha\n", "add a");
    commit_file(dir.path(), "b.txt", "beta\n", "add b");
    git(dir.path(), &["checkout", "main"]);

    // Simulate a provider squash-merge: collapse the whole branch into a single
    // commit on main. None of the branch's per-commit patch ids match this
    // squashed commit, so `git cherry main feature-multi-squash` cannot see it.
    git(dir.path(), &["merge", "--squash", "feature-multi-squash"]);
    git(
        dir.path(),
        &["commit", "-m", "squash: feature-multi-squash (#1)"],
    );

    dir
}

fn setup_repo_with_orphan_and_squash() -> tempfile::TempDir {
    let dir = init_repo();
    commit_file(dir.path(), "file.txt", "base\n", "base");

    // A real multi-commit squash-merge: the genuine cleanup candidate.
    git(dir.path(), &["checkout", "-b", "feature-multi-squash"]);
    commit_file(dir.path(), "a.txt", "alpha\n", "add a");
    commit_file(dir.path(), "b.txt", "beta\n", "add b");
    git(dir.path(), &["checkout", "main"]);
    git(dir.path(), &["merge", "--squash", "feature-multi-squash"]);
    git(
        dir.path(),
        &["commit", "-m", "squash: feature-multi-squash (#1)"],
    );

    // An orphan branch with unrelated history: its own root commit, so it has
    // no merge-base with main. The squash sweep must skip it, not abort.
    git(dir.path(), &["checkout", "--orphan", "orphan-fixture"]);
    commit_file(dir.path(), "orphan.txt", "orphan\n", "orphan root");
    git(dir.path(), &["checkout", "main"]);

    dir
}

#[test]
fn branch_cleanup_help() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["branch", "cleanup", "--help"]);

    assert_eq!(output.code, 0);
    assert!(
        output
            .stdout_text()
            .contains(
                "Usage: git-delete-merged-branches [-b|--base <ref>] [-s|--squash] [-w|--remove-worktrees]\n"
            )
    );
    assert_eq!(output.stderr_text(), "");
}

#[test]
fn branch_cleanup_missing_base_arg() {
    let harness = GitCliHarness::new();
    let dir = init_repo();

    let output = harness.run(dir.path(), &["branch", "cleanup", "--base"]);

    assert_eq!(output.code, 2);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(output.stderr_text(), "");
}

#[test]
fn branch_cleanup_not_in_repo() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["branch", "cleanup"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(output.stderr_text(), "❌ Not in a git repository\n");
}

#[test]
fn branch_cleanup_invalid_base_ref() {
    let harness = GitCliHarness::new();
    let dir = init_repo();

    let output = harness.run(dir.path(), &["branch", "cleanup", "--base", "nope"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(output.stderr_text(), "❌ Invalid base ref: nope\n");
}

#[test]
fn branch_cleanup_merged_lists_candidates_and_aborts() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_branches();

    let output = run_with_stdin(&harness, dir.path(), &["branch", "cleanup"], "n\n");

    assert_eq!(output.code, 1);
    assert!(
        output
            .stdout_text()
            .contains("🧹 Merged branches to delete (base: HEAD):")
    );
    assert!(output.stdout_text().contains("  - feature-merged"));
    assert!(!output.stdout_text().contains("feature-squash"));
    assert!(!output.stdout_text().contains("develop"));
    assert!(output.stdout_text().contains("🚫 Aborted"));
}

#[test]
fn branch_cleanup_squash_lists_squash_candidates_and_aborts() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_branches();

    let output = run_with_stdin(
        &harness,
        dir.path(),
        &["branch", "cleanup", "--squash"],
        "n\n",
    );

    assert_eq!(output.code, 1);
    assert!(
        output
            .stdout_text()
            .contains("🧹 Branches to delete (base: HEAD, mode: squash):")
    );
    assert!(output.stdout_text().contains("  - feature-merged"));
    assert!(output.stdout_text().contains("  - feature-squash"));
    assert!(!output.stdout_text().contains("develop"));
    assert!(output.stdout_text().contains("🚫 Aborted"));
}

#[test]
fn branch_cleanup_protects_base_and_main() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_branches();

    let output = run_with_stdin(
        &harness,
        dir.path(),
        &["branch", "cleanup", "--base", "main"],
        "n\n",
    );

    assert_eq!(output.code, 1);
    assert!(
        output
            .stdout_text()
            .contains("🧹 Merged branches to delete (base: main):")
    );
    assert!(output.stdout_text().contains("  - feature-merged"));
    assert!(!output.stdout_text().contains("  - main"));
    assert!(output.stdout_text().contains("🚫 Aborted"));
}

#[test]
fn branch_cleanup_no_candidates_message() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    commit_file(dir.path(), "file.txt", "base\n", "base");

    let output = harness.run(dir.path(), &["branch", "cleanup"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stdout_text(), "✅ No deletable merged branches.\n");
}

#[test]
fn branch_cleanup_squash_no_candidates_message() {
    let harness = GitCliHarness::new();
    let dir = init_repo();
    commit_file(dir.path(), "file.txt", "base\n", "base");

    let output = harness.run(dir.path(), &["branch", "cleanup", "--squash"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stdout_text(), "✅ No deletable branches found.\n");
}

#[test]
fn branch_cleanup_reports_failed_deletion_for_linked_worktree_branch() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_branches();

    let linked_worktree = dir.path().join("linked-worktree");
    let linked_worktree_path = linked_worktree.to_str().expect("utf8 linked worktree path");
    git(
        dir.path(),
        &["worktree", "add", linked_worktree_path, "feature-merged"],
    );

    let output = run_with_stdin(&harness, dir.path(), &["branch", "cleanup"], "y\n");

    assert_eq!(output.code, 1);
    assert!(
        output
            .stdout_text()
            .contains("🧹 Merged branches to delete (base: HEAD):")
    );
    assert!(output.stdout_text().contains("  - feature-merged"));
    assert!(
        output
            .stderr_text()
            .contains("⚠️  Failed to delete 1 branch(es):")
    );
    assert!(output.stderr_text().contains("feature-merged"));
    assert!(git(dir.path(), &["branch", "--list", "feature-merged"]).contains("feature-merged"));
}

#[test]
fn branch_cleanup_remove_worktrees_flag_deletes_linked_worktree_branch() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_branches();

    let linked_worktree = dir.path().join("linked-worktree");
    let linked_worktree_path = linked_worktree.to_str().expect("utf8 linked worktree path");
    git(
        dir.path(),
        &["worktree", "add", linked_worktree_path, "feature-merged"],
    );

    let output = run_with_stdin(
        &harness,
        dir.path(),
        &["branch", "cleanup", "--remove-worktrees"],
        "y\n",
    );

    assert_eq!(output.code, 0);
    assert!(
        output
            .stdout_text()
            .contains("⚠️  Linked worktrees to remove (--remove-worktrees):")
    );
    assert!(output.stdout_text().contains("feature-merged"));
    assert!(
        output
            .stdout_text()
            .contains("✅ Removed 1 linked worktree(s).")
    );
    assert!(output.stdout_text().contains("✅ Deleted 1 branch(es)."));
    assert!(!linked_worktree.exists());
    assert_eq!(git(dir.path(), &["branch", "--list", "feature-merged"]), "");
}

#[test]
fn branch_cleanup_squash_detects_real_multi_commit_squash() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_real_squash();

    let output = run_with_stdin(
        &harness,
        dir.path(),
        &["branch", "cleanup", "--squash"],
        "n\n",
    );

    assert_eq!(output.code, 1);
    assert!(
        output
            .stdout_text()
            .contains("🧹 Branches to delete (base: HEAD, mode: squash):")
    );
    assert!(output.stdout_text().contains("  - feature-multi-squash"));
    assert!(output.stdout_text().contains("🚫 Aborted"));
}

#[test]
fn branch_cleanup_squash_remove_worktrees_deletes_squashed_branch_with_worktree() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_real_squash();

    let linked_worktree = dir.path().join("linked-worktree");
    let linked_worktree_path = linked_worktree.to_str().expect("utf8 linked worktree path");
    git(
        dir.path(),
        &[
            "worktree",
            "add",
            linked_worktree_path,
            "feature-multi-squash",
        ],
    );

    let output = run_with_stdin(
        &harness,
        dir.path(),
        &["branch", "cleanup", "--squash", "--remove-worktrees"],
        "y\n",
    );

    assert_eq!(output.code, 0);
    assert!(output.stdout_text().contains("  - feature-multi-squash"));
    assert!(
        output
            .stdout_text()
            .contains("⚠️  Linked worktrees to remove (--remove-worktrees):")
    );
    assert!(
        output
            .stdout_text()
            .contains("✅ Removed 1 linked worktree(s).")
    );
    assert!(output.stdout_text().contains("✅ Deleted 1 branch(es)."));
    assert!(!linked_worktree.exists());
    assert_eq!(
        git(dir.path(), &["branch", "--list", "feature-multi-squash"]),
        ""
    );
}

#[test]
fn branch_cleanup_squash_skips_unrelated_history_branch() {
    let harness = GitCliHarness::new();
    let dir = setup_repo_with_orphan_and_squash();

    let output = run_with_stdin(
        &harness,
        dir.path(),
        &["branch", "cleanup", "--squash"],
        "n\n",
    );

    // The orphan branch has no merge-base with main; the sweep must skip it and
    // still list the genuine squash-merge, rather than abort the whole run with
    // a merge-base error.
    assert!(!output.stderr_text().contains("Failed to find merge-base"));
    assert!(
        output
            .stdout_text()
            .contains("🧹 Branches to delete (base: HEAD, mode: squash):")
    );
    assert!(output.stdout_text().contains("  - feature-multi-squash"));
    assert!(!output.stdout_text().contains("orphan-fixture"));
    assert!(output.stdout_text().contains("🚫 Aborted"));
}

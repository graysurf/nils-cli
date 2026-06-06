use crate::common;
use std::fs;
use std::os::unix::fs::symlink;

#[test]
fn commit_invalid_parent_warning_is_stable() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("base.txt"), "base").unwrap();
    common::git(root, &["add", "base.txt"]);
    common::git(root, &["commit", "-m", "base"]);

    common::git(root, &["checkout", "-b", "feature"]);
    fs::write(root.join("feature.txt"), "feature").unwrap();
    common::git(root, &["add", "feature.txt"]);
    common::git(root, &["commit", "-m", "feature"]);

    common::git(root, &["checkout", "main"]);
    fs::write(root.join("main.txt"), "main").unwrap();
    common::git(root, &["add", "main.txt"]);
    common::git(root, &["commit", "-m", "main"]);

    common::git(
        root,
        &["merge", "--no-ff", "feature", "-m", "merge feature"],
    );
    let merge_hash = common::git(root, &["rev-parse", "HEAD"]).trim().to_string();

    let output = common::run_git_scope(
        root,
        &["commit", &merge_hash, "--parent", "nope"],
        &[("NO_COLOR", "1")],
    );

    assert!(
        output.contains("  ⚠️  Invalid --parent value 'nope' — falling back to parent #1"),
        "invalid parent warning missing: {output}"
    );
}

#[test]
fn built_in_tree_output_is_stable_without_tree_on_path() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("file.txt"), "base").unwrap();
    common::git(root, &["add", "file.txt"]);
    common::git(root, &["commit", "-m", "base"]);

    fs::write(root.join("file.txt"), "change").unwrap();
    common::git(root, &["add", "file.txt"]);

    let stub = tempfile::TempDir::new().unwrap();
    let git_path = common::resolve_path_command("git");
    symlink(&git_path, stub.path().join("git")).unwrap();

    let path_env = stub.path().to_string_lossy().to_string();
    let output = common::run_git_scope(
        root,
        &["staged"],
        &[("NO_COLOR", "1"), ("PATH", path_env.as_str())],
    );

    assert!(
        output.contains("📂 Directory tree:\n.\n└── file.txt\n\n1 directory, 1 file"),
        "built-in tree output not found: {output}"
    );
    assert!(
        !output.contains("tree is not installed"),
        "external tree warning should not be emitted: {output}"
    );
}

#[test]
fn missing_file_warning_is_stable() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("vanish.txt"), "gone").unwrap();
    common::git(root, &["add", "vanish.txt"]);
    common::git(root, &["commit", "-m", "add vanish"]);
    let old_commit = common::git(root, &["rev-parse", "HEAD"]).trim().to_string();

    common::git(root, &["rm", "vanish.txt"]);
    common::git(root, &["commit", "-m", "remove vanish"]);

    let output = common::run_git_scope(root, &["commit", &old_commit, "-p"], &[("NO_COLOR", "1")]);

    assert!(
        output.contains(&format!("📄 vanish.txt (from {old_commit})")),
        "commit snapshot header missing: {output}"
    );
}

#[test]
fn missing_worktree_file_falls_back_to_head() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("worktree.txt"), "worktree").unwrap();
    common::git(root, &["add", "worktree.txt"]);
    common::git(root, &["commit", "-m", "add worktree"]);

    fs::remove_file(root.join("worktree.txt")).unwrap();

    let output = common::run_git_scope(root, &["unstaged", "-p"], &[("NO_COLOR", "1")]);

    assert!(
        output.contains("📄 worktree.txt (from HEAD)"),
        "missing worktree fallback not found: {output}"
    );
}

#[test]
fn missing_index_file_falls_back_to_head() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("index.txt"), "index").unwrap();
    common::git(root, &["add", "index.txt"]);
    common::git(root, &["commit", "-m", "add index"]);

    common::git(root, &["rm", "index.txt"]);

    let output = common::run_git_scope(root, &["staged", "-p"], &[("NO_COLOR", "1")]);

    assert!(
        output.contains("📄 index.txt (deleted in index; from HEAD)"),
        "missing index fallback not found: {output}"
    );
}

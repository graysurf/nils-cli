use crate::common;
use std::fs;

#[test]
fn untracked_lists_files_with_u_status() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("untracked.txt"), "hello").unwrap();

    let output = common::run_git_scope(root, &["untracked"], &[("NO_COLOR", "1")]);
    assert!(
        output.contains("➔ [U] untracked.txt"),
        "untracked output missing: {output}"
    );
}

#[test]
fn staged_binary_prints_placeholder() {
    let repo = common::init_repo();
    let root = repo.path();

    let bytes = [0u8, 159u8, 146u8, 150u8];
    fs::write(root.join("bin.dat"), bytes).unwrap();
    common::git(root, &["add", "bin.dat"]);

    let output = common::run_git_scope(root, &["staged", "-p"], &[("NO_COLOR", "1")]);
    assert!(
        output.contains("📄 bin.dat (binary file in index)"),
        "binary index label missing: {output}"
    );
    assert!(
        output.contains("[Binary file content omitted]"),
        "binary placeholder missing: {output}"
    );
}

#[test]
fn rename_shows_arrow() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("old.txt"), "data").unwrap();
    common::git(root, &["add", "old.txt"]);
    common::git(root, &["commit", "-m", "old"]);

    common::git(root, &["mv", "old.txt", "new.txt"]);

    let output = common::run_git_scope(root, &["staged"], &[("NO_COLOR", "1")]);
    assert!(
        output.contains("->"),
        "rename arrow missing in output: {output}"
    );
    assert!(
        output.contains("new.txt"),
        "rename target missing: {output}"
    );
}

#[test]
fn staged_deletion_is_listed() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("gone.txt"), "gone").unwrap();
    common::git(root, &["add", "gone.txt"]);
    common::git(root, &["commit", "-m", "add gone"]);

    common::git(root, &["rm", "gone.txt"]);

    let output = common::run_git_scope(root, &["staged"], &[("NO_COLOR", "1")]);
    assert!(
        output.contains("➔ [D] gone.txt"),
        "staged deletion missing: {output}"
    );
}

#[test]
fn outside_repo_prints_warning() {
    let temp = tempfile::TempDir::new().unwrap();
    let (code, output) =
        common::run_git_scope_allow_fail(temp.path(), &["staged"], &[("NO_COLOR", "1")]);
    assert!(code != 0, "expected non-zero exit code");
    assert!(
        output.contains("Not a Git repository"),
        "missing repo warning: {output}"
    );
}

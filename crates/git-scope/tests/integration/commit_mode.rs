use crate::common;
use std::fs;

#[test]
fn commit_help_exits_zero_and_prints_subcommand_help() {
    let repo = common::init_repo();
    let (code, stdout) = common::run_git_scope_allow_fail(repo.path(), &["commit", "--help"], &[]);
    assert_eq!(
        code, 0,
        "expected `commit --help` to exit 0; stdout: {stdout}"
    );
    assert!(
        stdout.contains("Show commit details"),
        "expected commit subcommand help; got: {stdout}"
    );
    assert!(
        stdout.contains("[COMMIT]"),
        "expected [COMMIT] argument in help; got: {stdout}"
    );
}

#[test]
fn commit_merge_parent_selection() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("base.txt"), "base").unwrap();
    common::git(root, &["add", "."]);
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
        &["commit", &merge_hash, "--parent", "2"],
        &[("NO_COLOR", "1")],
    );

    assert!(
        output.contains("Merge commit with 2 parents — showing diff against parent #2"),
        "merge parent selection missing: {output}"
    );

    let output_invalid = common::run_git_scope(
        root,
        &["commit", &merge_hash, "--parent", "9"],
        &[("NO_COLOR", "1")],
    );

    assert!(
        output_invalid.contains("Parent index 9 out of range (1-2) — falling back to parent #1"),
        "invalid parent warning missing: {output_invalid}"
    );
}

#[test]
fn commit_print_outputs_file_contents() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("hello.txt"), "hello").unwrap();
    common::git(root, &["add", "hello.txt"]);
    common::git(root, &["commit", "-m", "hello"]);

    let output = common::run_git_scope(root, &["commit", "HEAD", "-p"], &[("NO_COLOR", "1")]);
    assert!(output.contains("📦 Printing file contents:"));
    assert!(
        output.contains("📄 hello.txt (working tree)")
            || output.contains("📄 hello.txt (from HEAD)"),
        "expected file content header missing: {output}"
    );
}

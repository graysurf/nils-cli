use crate::common;
use std::fs;
use std::os::unix::fs::symlink;

#[test]
fn tracked_renders_tree_when_tree_missing() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("tracked.txt"), "hello").unwrap();
    common::git(root, &["add", "tracked.txt"]);
    common::git(root, &["commit", "-m", "tracked"]);

    let stub = tempfile::TempDir::new().unwrap();
    let git_path = common::resolve_path_command("git");
    let link_path = stub.path().join("git");
    symlink(&git_path, &link_path).unwrap();

    let path_env = stub.path().to_string_lossy().to_string();
    let output = common::run_git_scope(
        root,
        &["tracked"],
        &[("NO_COLOR", "1"), ("PATH", path_env.as_str())],
    );

    assert!(output.contains("📂 Directory tree:"));
    assert!(output.contains("└── tracked.txt"));
    assert!(output.contains("1 directory, 1 file"));
    assert!(!output.contains("tree is not installed"));
}

#[test]
fn tracked_print_works_when_file_missing() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("tracked.txt"), "HELLO_FROM_TEXT").unwrap();
    let bytes = [0u8, 159u8, 146u8, 150u8];
    fs::write(root.join("bin.dat"), bytes).unwrap();
    common::git(root, &["add", "."]);
    common::git(root, &["commit", "-m", "add files"]);

    let stub = tempfile::TempDir::new().unwrap();
    let git_path = common::resolve_path_command("git");
    symlink(&git_path, stub.path().join("git")).unwrap();

    let path_env = stub.path().to_string_lossy().to_string();
    let output = common::run_git_scope(
        root,
        &["tracked", "-p"],
        &[("NO_COLOR", "1"), ("PATH", path_env.as_str())],
    );

    assert!(
        output.contains("📄 tracked.txt (working tree)"),
        "text label missing: {output}"
    );
    assert!(
        output.contains("HELLO_FROM_TEXT"),
        "text content missing: {output}"
    );
    assert!(
        output.contains("📄 bin.dat (binary file in working tree)"),
        "binary label missing: {output}"
    );
    assert!(
        output.contains("[Binary file content omitted]"),
        "binary placeholder missing: {output}"
    );
}

#[test]
fn staged_print_works_without_mktemp_or_file() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("tracked.txt"), "base line\n").unwrap();
    common::git(root, &["add", "tracked.txt"]);
    common::git(root, &["commit", "-m", "base"]);

    fs::write(root.join("tracked.txt"), "STAGED_LINE\n").unwrap();
    common::git(root, &["add", "tracked.txt"]);

    let stub = tempfile::TempDir::new().unwrap();
    let git_path = common::resolve_path_command("git");
    symlink(&git_path, stub.path().join("git")).unwrap();

    let path_env = stub.path().to_string_lossy().to_string();
    let output = common::run_git_scope(
        root,
        &["staged", "-p"],
        &[("NO_COLOR", "1"), ("PATH", path_env.as_str())],
    );

    assert!(
        output.contains("📄 tracked.txt (index)"),
        "index label missing: {output}"
    );
    assert!(
        output.contains("STAGED_LINE"),
        "staged content missing: {output}"
    );
}

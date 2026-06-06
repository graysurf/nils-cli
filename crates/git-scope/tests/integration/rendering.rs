use crate::common;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

#[test]
fn no_color_outputs_no_ansi() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("file.txt"), "base").unwrap();
    common::git(root, &["add", "file.txt"]);
    common::git(root, &["commit", "-m", "base"]);

    fs::write(root.join("file.txt"), "change").unwrap();
    common::git(root, &["add", "file.txt"]);

    let output = common::run_git_scope(root, &["staged"], &[("NO_COLOR", "1")]);
    assert!(!output.contains("\x1b["), "unexpected ANSI codes: {output}");
}

#[test]
fn tree_renders_without_tree_on_path() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("file.txt"), "base").unwrap();
    common::git(root, &["add", "file.txt"]);
    common::git(root, &["commit", "-m", "base"]);

    fs::write(root.join("file.txt"), "change").unwrap();
    common::git(root, &["add", "file.txt"]);

    let temp_path = tempfile::TempDir::new().unwrap();
    let git_path = common::resolve_path_command("git");
    let link_path = temp_path.path().join("git");
    symlink(&git_path, &link_path).unwrap();

    let output = common::run_git_scope(
        root,
        &["staged"],
        &[
            ("NO_COLOR", "1"),
            ("PATH", temp_path.path().to_str().unwrap()),
        ],
    );

    assert!(
        output.contains("📂 Directory tree:"),
        "tree section not found: {output}"
    );
    assert!(
        output.contains("└── file.txt"),
        "built-in tree entry not found: {output}"
    );
    assert!(
        output.contains("1 directory, 1 file"),
        "tree summary not found: {output}"
    );
    assert!(
        !output.contains("tree is not installed"),
        "external tree warning should not be emitted: {output}"
    );
}

#[test]
fn failing_tree_binary_on_path_is_not_invoked() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("file.txt"), "base").unwrap();
    common::git(root, &["add", "file.txt"]);
    common::git(root, &["commit", "-m", "base"]);

    fs::write(root.join("file.txt"), "change").unwrap();
    common::git(root, &["add", "file.txt"]);

    let temp_path = tempfile::TempDir::new().unwrap();
    let git_path = common::resolve_path_command("git");
    let link_path = temp_path.path().join("git");
    symlink(&git_path, &link_path).unwrap();

    let tree_path = temp_path.path().join("tree");
    fs::write(
        &tree_path,
        "#!/usr/bin/env bash\necho 'external tree should not run' >&2\nexit 1\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&tree_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&tree_path, perms).unwrap();

    let output = common::run_git_scope(
        root,
        &["staged"],
        &[
            ("NO_COLOR", "1"),
            ("PATH", temp_path.path().to_str().unwrap()),
        ],
    );

    assert!(
        output.contains("└── file.txt"),
        "built-in tree entry not found: {output}"
    );
    assert!(
        !output.contains("tree does not support --fromfile"),
        "external tree warning should not be emitted: {output}"
    );
}

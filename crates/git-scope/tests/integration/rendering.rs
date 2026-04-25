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
fn tree_missing_emits_warning() {
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
        &[("PATH", temp_path.path().to_str().unwrap())],
    );

    assert!(
        output.contains("tree is not installed"),
        "tree missing warning not found: {output}"
    );
}

#[test]
fn tree_fromfile_unsupported_emits_warning() {
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
        "#!/usr/bin/env bash\nif [[ \"$1\" == \"--version\" ]]; then\n  exit 0\nfi\nif [[ \"$1\" == \"--fromfile\" ]]; then\n  exit 1\nfi\nexit 0\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&tree_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&tree_path, perms).unwrap();

    let output = common::run_git_scope(
        root,
        &["staged"],
        &[("PATH", temp_path.path().to_str().unwrap())],
    );

    assert!(
        output.contains("tree does not support --fromfile"),
        "tree unsupported warning not found: {output}"
    );
}

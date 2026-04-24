use crate::common;
use std::fs;

#[test]
fn tracked_prefix_includes_expected_file() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::create_dir_all(root.join("scripts/git")).unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();

    fs::write(root.join("scripts/git/git-scope.zsh"), "echo scope").unwrap();
    fs::write(root.join("docs/readme.md"), "docs").unwrap();

    common::git(root, &["add", "."]);
    common::git(root, &["commit", "-m", "init"]);

    let output = common::run_git_scope(root, &["tracked", "./scripts"], &[("NO_COLOR", "1")]);
    assert!(
        output.contains("➔ [-] scripts/git/git-scope.zsh"),
        "tracked prefix output missing expected path: {output}"
    );
}

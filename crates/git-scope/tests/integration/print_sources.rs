use crate::common;
use std::fs;

#[test]
fn print_sources_match_index_and_worktree() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("only_staged.txt"), "BASE").unwrap();
    fs::write(root.join("only_unstaged.txt"), "BASE").unwrap();
    fs::write(root.join("both.txt"), "BASE").unwrap();

    common::git(root, &["add", "."]);
    common::git(root, &["commit", "-m", "init"]);

    fs::write(root.join("only_staged.txt"), "STAGED").unwrap();
    common::git(root, &["add", "only_staged.txt"]);

    fs::write(root.join("only_unstaged.txt"), "UNSTAGED").unwrap();

    fs::write(root.join("both.txt"), "INDEX").unwrap();
    common::git(root, &["add", "both.txt"]);
    fs::write(root.join("both.txt"), "WORKTREE").unwrap();

    let staged_output = common::run_git_scope(root, &["staged", "-p"], &[("NO_COLOR", "1")]);
    assert!(staged_output.contains("📄 both.txt (index)"));
    assert!(staged_output.contains("INDEX"));
    assert!(!staged_output.contains("WORKTREE"));

    let all_output = common::run_git_scope(root, &["all", "-p"], &[("NO_COLOR", "1")]);
    assert!(all_output.contains("📄 only_staged.txt (index)"));
    assert!(!all_output.contains("📄 only_staged.txt (working tree)"));

    assert!(all_output.contains("📄 only_unstaged.txt (working tree)"));
    assert!(!all_output.contains("📄 only_unstaged.txt (index)"));

    assert!(all_output.contains("📄 both.txt (index)"));
    assert!(all_output.contains("📄 both.txt (working tree)"));
    assert!(all_output.contains("INDEX"));
    assert!(all_output.contains("WORKTREE"));
}

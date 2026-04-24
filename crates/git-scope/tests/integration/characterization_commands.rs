use crate::common;
use std::fs;
use std::path::Path;

fn run_git_scope_no_color(dir: &Path, args: &[&str]) -> String {
    common::run_git_scope(dir, args, &[("NO_COLOR", "1")])
}

fn assert_section_order(output: &str, sections: &[&str]) {
    let mut cursor = 0usize;
    for section in sections {
        let Some(pos) = output[cursor..].find(section) else {
            panic!("missing section header '{section}' in output: {output}");
        };
        cursor += pos + section.len();
    }
}

fn assert_change_sections(output: &str) {
    assert_section_order(
        output,
        &[
            "📄 Changed files:",
            "📂 Directory tree:",
            "📦 Printing file contents:",
        ],
    );
}

fn assert_commit_sections(output: &str) {
    assert_section_order(
        output,
        &[
            "📝 Commit Message:",
            "📄 Changed files:",
            "📂 Directory tree:",
            "📦 Printing file contents:",
        ],
    );
}

fn init_repo_with_changes() -> tempfile::TempDir {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("tracked.txt"), "base").unwrap();
    common::git(root, &["add", "tracked.txt"]);
    common::git(root, &["commit", "-m", "base"]);

    fs::write(root.join("staged.txt"), "staged").unwrap();
    common::git(root, &["add", "staged.txt"]);

    fs::write(root.join("tracked.txt"), "unstaged").unwrap();
    fs::write(root.join("untracked.txt"), "untracked").unwrap();

    repo
}

#[test]
fn tracked_section_headers_in_order() {
    let repo = init_repo_with_changes();
    let output = run_git_scope_no_color(repo.path(), &["tracked", "-p"]);
    assert_change_sections(&output);
}

#[test]
fn staged_section_headers_in_order() {
    let repo = init_repo_with_changes();
    let output = run_git_scope_no_color(repo.path(), &["staged", "-p"]);
    assert_change_sections(&output);
}

#[test]
fn unstaged_section_headers_in_order() {
    let repo = init_repo_with_changes();
    let output = run_git_scope_no_color(repo.path(), &["unstaged", "-p"]);
    assert_change_sections(&output);
}

#[test]
fn all_section_headers_in_order() {
    let repo = init_repo_with_changes();
    let output = run_git_scope_no_color(repo.path(), &["all", "-p"]);
    assert_change_sections(&output);
}

#[test]
fn untracked_section_headers_in_order() {
    let repo = init_repo_with_changes();
    let output = run_git_scope_no_color(repo.path(), &["untracked", "-p"]);
    assert_change_sections(&output);
}

#[test]
fn commit_section_headers_in_order() {
    let repo = common::init_repo();
    let root = repo.path();

    fs::write(root.join("hello.txt"), "hello").unwrap();
    common::git(root, &["add", "hello.txt"]);
    common::git(root, &["commit", "-m", "hello"]);

    let output = run_git_scope_no_color(root, &["commit", "HEAD", "-p"]);
    assert_commit_sections(&output);
}

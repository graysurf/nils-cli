use crate::common;
use chrono::Local;
use common::{git, git_with_env, init_repo, run_git_summary, run_git_summary_allow_fail};
use std::fs;

const SEPARATOR: &str = "----------------------------------------------------------------------------------------------------------------------------------------";

fn commit_with_author(
    dir: &std::path::Path,
    name: &str,
    email: &str,
    date: &str,
    file: &str,
    contents: &[u8],
) {
    let path = dir.join(file);
    fs::write(&path, contents).expect("write file");
    git(dir, &["add", file]);

    let tz = Local::now().format("%z").to_string();
    let datetime = format!("{date} 12:00:00 {tz}");
    let envs = [
        ("GIT_AUTHOR_NAME", name),
        ("GIT_AUTHOR_EMAIL", email),
        ("GIT_COMMITTER_NAME", name),
        ("GIT_COMMITTER_EMAIL", email),
        ("GIT_AUTHOR_DATE", datetime.as_str()),
        ("GIT_COMMITTER_DATE", datetime.as_str()),
    ];

    git_with_env(dir, &["commit", "-m", "commit"], &envs);
}

#[test]
fn invalid_date_format() {
    let repo = init_repo();
    let root = repo.path();

    let (code, output) = run_git_summary_allow_fail(root, &["2024/01/01", "2024-01-31"], &[]);
    assert!(code != 0, "expected non-zero exit code");
    assert!(
        output.contains("❌ Invalid date format: 2024/01/01 (expected YYYY-MM-DD)."),
        "missing format error: {output}"
    );
}

#[test]
fn invalid_date_value() {
    let repo = init_repo();
    let root = repo.path();

    let (code, output) = run_git_summary_allow_fail(root, &["2024-02-30", "2024-03-01"], &[]);
    assert!(code != 0, "expected non-zero exit code");
    assert!(
        output.contains("❌ Invalid date value: 2024-02-30."),
        "missing value error: {output}"
    );
}

#[test]
fn start_after_end() {
    let repo = init_repo();
    let root = repo.path();

    let (code, output) = run_git_summary_allow_fail(root, &["2024-02-01", "2024-01-31"], &[]);
    assert!(code != 0, "expected non-zero exit code");
    assert!(
        output.contains("❌ Start date must be on or before end date."),
        "missing range error: {output}"
    );
}

#[test]
fn missing_args_invalid_usage() {
    let repo = init_repo();
    let root = repo.path();

    let (code, output) = run_git_summary_allow_fail(root, &["2024-01-01"], &[]);
    assert!(code != 0, "expected non-zero exit code");
    assert!(
        output.contains("❌ Invalid usage. Try: git-summary help"),
        "missing usage error: {output}"
    );
}

#[test]
fn outside_repo_prints_warning() {
    let temp = tempfile::TempDir::new().unwrap();
    let (code, output) = run_git_summary_allow_fail(temp.path(), &["all"], &[]);
    assert!(code != 0, "expected non-zero exit code");
    assert!(
        output.contains("Not a Git repository"),
        "missing repo warning: {output}"
    );
}

#[test]
fn no_commits_in_range_still_prints_header() {
    let repo = init_repo();
    let root = repo.path();

    commit_with_author(
        root,
        "Alice",
        "alice@example.com",
        "2023-01-01",
        "a.txt",
        b"one\n",
    );

    let output = run_git_summary(root, &["2024-01-01", "2024-01-02"], &[]);
    assert!(output.contains(SEPARATOR), "missing separator: {output}");
    assert!(
        !output.contains("Alice"),
        "expected no author rows: {output}"
    );
}

#[test]
fn binary_only_author_is_hidden() {
    let repo = init_repo();
    let root = repo.path();

    // Binary numstat is counted as zero lines, so an author who only touches a
    // binary blob has no code changes and is dropped from the report.
    commit_with_author(
        root,
        "Binary",
        "bin@example.com",
        "2024-01-10",
        "bin.dat",
        &[0u8, 159u8, 146u8, 150u8],
    );
    // A real-code author keeps the table non-empty and proves the filter is
    // selective rather than blanket-hiding everyone.
    commit_with_author(
        root,
        "Coder",
        "coder@example.com",
        "2024-01-11",
        "a.txt",
        b"one\ntwo\n",
    );

    let output = run_git_summary(root, &["2024-01-01", "2024-01-31"], &[]);
    let coder_line = format!(
        "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
        "Coder", "coder@example.com", 2, 0, 2, 1, "2024-01-11", "2024-01-11"
    );
    assert!(output.contains(&coder_line), "missing Coder row: {output}");
    assert!(
        !output.contains("Binary"),
        "binary-only author has no code changes and must be hidden: {output}"
    );
}

#[test]
fn filenames_with_spaces_are_counted() {
    let repo = init_repo();
    let root = repo.path();

    commit_with_author(
        root,
        "Space",
        "space@example.com",
        "2024-01-12",
        "file with spaces.txt",
        b"one\ntwo\n",
    );

    let output = run_git_summary(root, &["2024-01-01", "2024-01-31"], &[]);
    let line = format!(
        "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
        "Space", "space@example.com", 2, 0, 2, 1, "2024-01-12", "2024-01-12"
    );
    assert!(
        output.contains(&line),
        "missing spaced filename row: {output}"
    );
}

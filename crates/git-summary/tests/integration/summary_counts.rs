use crate::common;
use chrono::Local;
use common::{git, git_with_env, init_repo, run_git_summary};
use std::fs;

const SEPARATOR: &str = "----------------------------------------------------------------------------------------------------------------------------------------";

fn commit_with_author(
    dir: &std::path::Path,
    name: &str,
    email: &str,
    date: &str,
    file: &str,
    contents: &str,
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
fn summary_counts_and_sorting() {
    let repo = init_repo();
    let root = repo.path();

    commit_with_author(
        root,
        "Alice",
        "alice@example.com",
        "2024-01-05",
        "a.txt",
        "one\ntwo\nthree\n",
    );
    commit_with_author(
        root,
        "Alice",
        "alice@example.com",
        "2024-01-06",
        "yarn.lock",
        "lockline1\nlockline2\n",
    );
    commit_with_author(
        root,
        "Bob",
        "bob@example.com",
        "2024-01-07",
        "b.txt",
        "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\n",
    );

    let output = run_git_summary(root, &["2024-01-01", "2024-01-31"], &[]);

    let header = format!(
        "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
        "Name", "Email", "Added", "Deleted", "Net", "Commits", "First", "Last"
    );
    assert!(output.contains(&header), "missing header: {output}");
    assert!(output.contains(SEPARATOR), "missing separator: {output}");

    let bob_line = format!(
        "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
        "Bob", "bob@example.com", 6, 0, 6, 1, "2024-01-07", "2024-01-07"
    );
    let alice_line = format!(
        "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
        "Alice", "alice@example.com", 3, 0, 3, 2, "2024-01-05", "2024-01-06"
    );

    assert!(output.contains(&bob_line), "missing Bob row: {output}");
    assert!(output.contains(&alice_line), "missing Alice row: {output}");

    let bob_pos = output.find(&bob_line).expect("bob row pos");
    let alice_pos = output.find(&alice_line).expect("alice row pos");
    assert!(bob_pos < alice_pos, "expected Bob before Alice: {output}");
}

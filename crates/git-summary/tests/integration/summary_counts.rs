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

#[test]
fn summary_aggregates_mailmap_aliases_into_one_canonical_author() {
    let repo = init_repo();
    let root = repo.path();

    commit_with_author(
        root,
        "graysurf",
        "graysurf-github@example.test",
        "2024-01-05",
        "github.txt",
        "one\ntwo\nthree\n",
    );
    commit_with_author(
        root,
        "graysurf",
        "graysurf-codeberg@example.test",
        "2024-01-06",
        "codeberg.txt",
        "alpha\nbeta\n",
    );
    fs::write(
        root.join(".mailmap"),
        "\
graysurf <commits@id.graysurf.dev> <graysurf-github@example.test>
graysurf <commits@id.graysurf.dev> <graysurf-codeberg@example.test>
",
    )
    .expect("write mailmap");

    let output = run_git_summary(root, &["2024-01-01", "2024-01-31"], &[]);

    let canonical_line = format!(
        "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
        "graysurf", "commits@id.graysurf.dev", 5, 0, 5, 2, "2024-01-05", "2024-01-06"
    );
    assert!(
        output.contains(&canonical_line),
        "missing canonical aggregate: {output}"
    );
    assert!(
        !output.contains("graysurf-github@example.test"),
        "raw GitHub alias should be hidden: {output}"
    );
    assert!(
        !output.contains("graysurf-codeberg@example.test"),
        "raw Codeberg alias should be hidden: {output}"
    );
}

#[test]
fn no_mailmap_preserves_raw_author_rows() {
    let repo = init_repo();
    let root = repo.path();

    commit_with_author(
        root,
        "graysurf",
        "github@example.com",
        "2024-01-05",
        "github.txt",
        "one\n",
    );
    commit_with_author(
        root,
        "graysurf",
        "codeberg@example.com",
        "2024-01-06",
        "codeberg.txt",
        "two\n",
    );
    fs::write(
        root.join(".mailmap"),
        "\
graysurf <canonical@example.com> <github@example.com>
graysurf <canonical@example.com> <codeberg@example.com>
",
    )
    .expect("write mailmap");

    let output = run_git_summary(root, &["--no-mailmap", "2024-01-01", "2024-01-31"], &[]);

    assert!(
        output.contains("github@example.com"),
        "missing GitHub row: {output}"
    );
    assert!(
        output.contains("codeberg@example.com"),
        "missing Codeberg row: {output}"
    );
    assert!(
        !output.contains("canonical@example.com"),
        "canonical identity should be disabled: {output}"
    );
}

#[test]
fn json_output_uses_the_versioned_summary_envelope() {
    let repo = init_repo();
    let root = repo.path();

    commit_with_author(
        root,
        "Alice",
        "alice@example.com",
        "2024-01-05",
        "a.txt",
        "one\ntwo\n",
    );

    let output = run_git_summary(root, &["--format", "json", "2024-01-01", "2024-01-31"], &[]);

    assert!(
        output.contains("\"schema_version\":\"cli.git-summary.summary.v1\""),
        "missing schema version: {output}"
    );
    assert!(
        output.contains("\"ok\":true"),
        "missing success state: {output}"
    );
    assert!(
        output.contains("\"mailmap\":true"),
        "missing mailmap mode: {output}"
    );
    assert!(
        output.contains("\"authors\":[{\"name\":\"Alice\",\"email\":\"alice@example.com\",\"added\":2,\"deleted\":0,\"net\":2,\"commits\":1,\"first\":\"2024-01-05\",\"last\":\"2024-01-05\"}]"),
        "unexpected authors payload: {output}"
    );
}

#[test]
fn summary_hides_authors_without_code_changes() {
    let repo = init_repo();
    let root = repo.path();

    // Alice touches real code and should be listed.
    commit_with_author(
        root,
        "Alice",
        "alice@example.com",
        "2024-01-05",
        "a.txt",
        "one\ntwo\nthree\n",
    );
    // Lockbot only ever bumps a lockfile, so it has no counted code changes
    // (added == 0 && deleted == 0) and must not appear in the summary.
    commit_with_author(
        root,
        "Lockbot",
        "lockbot@example.com",
        "2024-01-06",
        "yarn.lock",
        "lockline1\nlockline2\n",
    );

    let output = run_git_summary(root, &["2024-01-01", "2024-01-31"], &[]);

    let alice_line = format!(
        "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
        "Alice", "alice@example.com", 3, 0, 3, 1, "2024-01-05", "2024-01-05"
    );
    assert!(output.contains(&alice_line), "missing Alice row: {output}");
    assert!(
        !output.contains("Lockbot"),
        "Lockbot has no code changes and must be hidden: {output}"
    );
}

#[test]
fn summary_shows_deletion_only_author() {
    let repo = init_repo();
    let root = repo.path();

    // Seed three lines, then have a distinct author delete two of them. The
    // deleter has added == 0 but deleted > 0, so the `added == 0 && deleted
    // == 0` guard must NOT hide them (this would regress under a `net == 0`
    // or `net <= 0` filter).
    commit_with_author(
        root,
        "Seed",
        "seed@example.com",
        "2024-01-05",
        "shrink.txt",
        "one\ntwo\nthree\n",
    );
    commit_with_author(
        root,
        "Deleter",
        "deleter@example.com",
        "2024-01-06",
        "shrink.txt",
        "one\n",
    );

    let output = run_git_summary(root, &["2024-01-01", "2024-01-31"], &[]);

    let deleter_line = format!(
        "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
        "Deleter", "deleter@example.com", 0, 2, -2, 1, "2024-01-06", "2024-01-06"
    );
    assert!(
        output.contains(&deleter_line),
        "deletion-only author must still appear: {output}"
    );
}

#[test]
fn summary_shows_net_zero_churn_author() {
    let repo = init_repo();
    let root = repo.path();

    // Add two lines, then remove them. Totals are added == 2, deleted == 2,
    // net == 0. The author still has counted code changes, so they must
    // appear (this would regress under a `net == 0` filter).
    commit_with_author(
        root,
        "Churn",
        "churn@example.com",
        "2024-01-05",
        "churn.txt",
        "a\nb\n",
    );
    commit_with_author(
        root,
        "Churn",
        "churn@example.com",
        "2024-01-06",
        "churn.txt",
        "",
    );

    let output = run_git_summary(root, &["2024-01-01", "2024-01-31"], &[]);

    let churn_line = format!(
        "{:<25} {:<40} {:>8} {:>8} {:>8} {:>8} {:>12} {:>12}",
        "Churn", "churn@example.com", 2, 2, 0, 2, "2024-01-05", "2024-01-06"
    );
    assert!(
        output.contains(&churn_line),
        "net-zero churn author must still appear: {output}"
    );
}

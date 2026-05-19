use crate::common;
use pretty_assertions::assert_eq;

#[test]
fn unknown_subcommand_exits_one() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(dir.path(), &["bogus"], &[], None);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn staged_context_outside_repo_exits_one() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(dir.path(), &["staged-context"], &[], None);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn staged_context_no_staged_changes_exits_two() {
    let repo = common::init_repo();
    let output = common::run_semantic_commit_output(repo.path(), &["staged-context"], &[], None);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn commit_missing_message_exits_three() {
    let repo = common::init_repo();
    common::write_file(repo.path(), "a.txt", "hello\n");
    common::git(repo.path(), &["add", "a.txt"]);
    let output =
        common::run_semantic_commit_output(repo.path(), &["commit", "--automation"], &[], None);
    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn commit_invalid_message_exits_four() {
    let repo = common::init_repo();
    common::write_file(repo.path(), "a.txt", "hello\n");
    common::git(repo.path(), &["add", "a.txt"]);
    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "not a semantic commit",
            "--validate-only",
        ],
        &[],
        None,
    );
    assert_eq!(output.status.code(), Some(4));
}

#[test]
fn staged_context_emits_v2_schema_version_with_camelcase_alias() {
    let repo = common::init_repo();
    common::write_file(repo.path(), "a.txt", "hello\n");
    common::git(repo.path(), &["add", "a.txt"]);

    let output = common::run_semantic_commit_output(
        repo.path(),
        &["staged-context", "--format", "json"],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let context: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("parse staged-context json");
    assert_eq!(
        context["schema_version"].as_str(),
        Some("cli.semantic-commit.staged-context.v2")
    );
    assert_eq!(context["schemaVersion"].as_i64(), Some(1));
    assert!(context["generated_at"].is_string());
    assert!(context["generatedAt"].is_string());
    assert_eq!(context["staged"]["summary"]["file_count"].as_i64(), Some(1));
    assert_eq!(context["staged"]["summary"]["fileCount"].as_i64(), Some(1));
}

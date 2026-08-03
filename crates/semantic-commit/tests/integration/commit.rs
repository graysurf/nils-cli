use crate::common;
use serde_json::Value;
use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;

fn as_str(output: &[u8]) -> String {
    String::from_utf8_lossy(output).to_string()
}

fn stage_file(repo: &Path, name: &str, contents: &str) {
    common::write_file(repo, name, contents);
    common::git(repo, &["add", name]);
}

fn git_trim(repo: &Path, args: &[&str]) -> String {
    common::git(repo, args).trim().to_string()
}

fn deterministic_env(path: &str) -> Vec<(&'static str, String)> {
    vec![
        ("PATH", path.to_string()),
        (
            "GIT_AUTHOR_DATE",
            "Thu, 01 Jan 1970 00:00:00 +0000".to_string(),
        ),
        (
            "GIT_COMMITTER_DATE",
            "Thu, 01 Jan 1970 00:00:00 +0000".to_string(),
        ),
    ]
}

fn deterministic_env_with_pager(path: &str, pager: &str) -> Vec<(&'static str, String)> {
    let mut env = deterministic_env(path);
    env.push(("GIT_PAGER", pager.to_string()));
    env
}

fn env_refs<'a>(envs: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    envs.iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect()
}

#[test]
fn commit_outside_git_repo_errors() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &["commit", "--message", "chore: test"],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("error: must run inside a git work tree"));
}

#[test]
fn commit_missing_git_dependency_exits_5() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let tool_dir = tempfile::TempDir::new().expect("tempdir");
    let path_env = tool_dir.path().to_str().expect("tool dir path");
    let envs_owned = deterministic_env(path_env);
    let envs = env_refs(&envs_owned);

    let output = common::run_semantic_commit_output(
        dir.path(),
        &["commit", "--message", "chore: test"],
        &envs,
        None,
    );

    assert_eq!(output.status.code(), Some(5));
    assert!(as_str(&output.stderr).contains("error: git is required"));
}

#[test]
fn commit_help_flag_prints_usage() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(dir.path(), &["commit", "--help"], &[], None);

    assert_eq!(output.status.code(), Some(0));
    assert!(
        as_str(&output.stdout)
            .contains("semantic-commit commit [--message <text>|--message-file <path>] [options]")
    );
    assert!(as_str(&output.stdout).contains("--amend"));
    assert!(as_str(&output.stdout).contains("--message-only"));
    assert!(as_str(&output.stdout).contains("--format <mode>"));
    assert!(as_str(&output.stdout).contains("--body-bullet <text>"));
}

#[test]
fn commit_unknown_argument_errors_before_git_checks() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(dir.path(), &["commit", "--bogus"], &[], None);

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("error: unknown argument: --bogus"));
}

#[test]
fn commit_message_flag_requires_value() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output =
        common::run_semantic_commit_output(dir.path(), &["commit", "--message"], &[], None);

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("error: --message requires a value"));
}

#[test]
fn commit_short_message_flag_requires_value() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(dir.path(), &["commit", "-m"], &[], None);

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("error: -m requires a value"));
}

#[test]
fn commit_message_file_flag_requires_path() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output =
        common::run_semantic_commit_output(dir.path(), &["commit", "--message-file"], &[], None);

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("error: --message-file requires a path"));
}

#[test]
fn commit_no_staged_changes_exits_2() {
    let repo = common::init_repo();
    let output = common::run_semantic_commit_output(
        repo.path(),
        &["commit", "--message", "chore: test"],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(2));
    assert!(
        as_str(&output.stderr)
            .contains("error: no staged changes (stage files with git add first)")
    );
}

#[test]
fn commit_invalid_header_format_is_hard_fail() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &["commit", "--message", "Feat: bad"],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
    assert!(as_str(&output.stderr).contains("error: invalid header format"));

    let head = common::git_output(repo.path(), &["rev-parse", "--verify", "HEAD"]);
    assert!(
        !head.status.success(),
        "expected no commit to have been created"
    );
}

#[test]
fn commit_body_requires_blank_line() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let message = "feat: test\n- Bad body without blank line\n";
    let output = common::run_semantic_commit_output(repo.path(), &["commit"], &[], Some(message));

    assert_eq!(output.status.code(), Some(4));
    assert!(
        as_str(&output.stderr)
            .contains("error: commit body must be separated from header by a blank line")
    );
}

#[test]
fn commit_body_line_requires_capitalized_bullet() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let message = "feat: test\n\n- bad\n";
    let output = common::run_semantic_commit_output(repo.path(), &["commit"], &[], Some(message));

    assert_eq!(output.status.code(), Some(4));
    assert!(
        as_str(&output.stderr).contains(
            "error: commit body line 3 must start with '- ' followed by uppercase letter"
        ),
        "stderr was: {}",
        as_str(&output.stderr)
    );
}

#[test]
fn commit_body_accepts_two_space_continuation_lines() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let message = "feat: test\n\n- First bullet wraps onto the next line because it is long.\n  Continuation text describes the why in more detail.\n- Second bullet stands on its own.\n";
    let output = common::run_semantic_commit_output(repo.path(), &["commit"], &[], Some(message));

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
}

#[test]
fn commit_body_rejects_continuation_without_preceding_bullet() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let message = "feat: test\n\n  Orphan continuation line without a leading bullet.\n";
    let output = common::run_semantic_commit_output(repo.path(), &["commit"], &[], Some(message));

    assert_eq!(output.status.code(), Some(4));
    assert!(
        as_str(&output.stderr).contains("commit body line 3 must start with '- '"),
        "stderr was: {}",
        as_str(&output.stderr)
    );
}

#[test]
fn commit_message_and_message_file_are_mutually_exclusive() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--message",
            "chore: test",
            "--message-file",
            "message.txt",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("error: use only one of --message or --message-file"));
}

#[test]
fn commit_message_file_missing_errors() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &["commit", "--message-file", "missing.txt"],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("error: message file not found: missing.txt"));
}

#[test]
fn commit_empty_stdin_message_errors_with_exit_3() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output = common::run_semantic_commit_output(repo.path(), &["commit"], &[], Some(""));

    assert_eq!(output.status.code(), Some(3));
    assert!(as_str(&output.stderr).contains("error: commit message is empty"));
}

#[test]
fn commit_whitespace_stdin_message_errors_with_exit_3() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output = common::run_semantic_commit_output(repo.path(), &["commit"], &[], Some(" \n\t\n"));

    assert_eq!(output.status.code(), Some(3));
    assert!(as_str(&output.stderr).contains("error: commit message is empty"));
}

#[test]
fn commit_automation_requires_message_flag_or_file() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output =
        common::run_semantic_commit_output(repo.path(), &["commit", "--automation"], &[], Some(""));

    assert_eq!(output.status.code(), Some(3));
    assert!(
        as_str(&output.stderr).contains("error: no commit message provided in automation mode")
    );
}

#[test]
fn commit_validate_only_allows_outside_repo() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--message",
            "feat(core): add thing",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn commit_validate_only_invalid_message_returns_4() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--message",
            "Feat(core): add thing",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
}

#[test]
fn commit_validate_only_rejects_claude_coauthor_trailer_in_message() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--message",
            "feat(core): add thing\n\nCo-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
    let stderr = as_str(&output.stderr);
    assert!(
        stderr.contains("blocked by rule `claude-coauthor-trailer`"),
        "stderr was: {stderr}"
    );
    assert!(
        stderr.contains("matched: Co-Authored-By: Claude"),
        "stderr was: {stderr}"
    );
}

#[test]
fn commit_validate_only_rejects_claude_coauthor_trailer_flag() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--message",
            "feat(core): add thing",
            "--trailer",
            "Co-authored-by: Claude Sonnet 4.6 <noreply@anthropic.com>",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
    let stderr = as_str(&output.stderr);
    assert!(
        stderr.contains("source: --trailer #1"),
        "stderr was: {stderr}"
    );
    assert!(
        stderr.contains("blocked by rule `claude-coauthor-trailer`"),
        "stderr was: {stderr}"
    );
}

#[test]
fn commit_validate_only_rejects_claude_coauthor_equals_trailer_flag() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--message",
            "feat(core): add thing",
            "--trailer",
            "Co-Authored-By=Claude Haiku <noreply@anthropic.com>",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
    let stderr = as_str(&output.stderr);
    assert!(
        stderr.contains("matched: Co-Authored-By: Claude ..."),
        "stderr was: {stderr}"
    );
}

#[test]
fn commit_validate_only_allows_non_claude_coauthor_trailers() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--message",
            "feat(core): add thing\n\nCo-Authored-By: Jane Dev <jane@example.com>",
            "--trailer",
            "Co-Authored-By: Build Bot <bot@example.com>",
        ],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
}

#[test]
fn commit_validate_only_allows_claude_as_part_of_longer_word() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--message",
            "feat(core): add thing\n\nCo-Authored-By: Claudette Dev <dev@example.com>",
        ],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
}

#[test]
fn commit_validate_only_rejects_vendor_noreply_coauthor_trailer() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--message",
            "feat(core): add thing",
            "--trailer",
            "Co-Authored-By: Some Agent <noreply@anthropic.com>",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
    let stderr = as_str(&output.stderr);
    assert!(
        stderr.contains("blocked by rule `claude-coauthor-trailer`"),
        "stderr was: {stderr}"
    );
}

#[test]
fn commit_validate_only_rejects_generator_marker_in_message() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--message",
            "feat(core): add thing\n\n- Ship the thing\n\nX-Origin: https://claude.com/claude-code",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
    let stderr = as_str(&output.stderr);
    assert!(
        stderr.contains("blocked by rule `claude-generated-marker`"),
        "stderr was: {stderr}"
    );
    assert!(
        stderr.contains("source: message line 5"),
        "stderr was: {stderr}"
    );
}

#[test]
fn commit_validate_only_rejects_generator_marker_trailer_flag() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--message",
            "feat(core): add thing",
            "--trailer",
            "X-Origin: https://claude.ai/code",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
    let stderr = as_str(&output.stderr);
    assert!(
        stderr.contains("blocked by rule `claude-generated-marker`"),
        "stderr was: {stderr}"
    );
    assert!(
        stderr.contains("source: --trailer #1"),
        "stderr was: {stderr}"
    );
}

#[test]
fn commit_validate_only_allows_unrelated_agent_prose() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--message",
            "feat(core): add thing\n\n- Reject agent attribution markers on both egress paths",
        ],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
}

fn header_with_total_len(total_len: usize) -> String {
    let prefix = "feat: ";
    assert!(total_len > prefix.len());
    format!("{prefix}{}", "a".repeat(total_len - prefix.len()))
}

#[test]
fn commit_max_header_width_flag_rejects_header_over_active_limit() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let message = header_with_total_len(73);
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--max-header-width",
            "72",
            "--message",
            &message,
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
    assert!(
        as_str(&output.stderr).contains("commit header exceeds 72 characters (max 72)"),
        "stderr was: {}",
        as_str(&output.stderr)
    );
}

#[test]
fn commit_max_header_width_env_sets_default_when_flag_absent() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let message = header_with_total_len(81);
    let output = common::run_semantic_commit_output(
        dir.path(),
        &["commit", "--validate-only", "--message", &message],
        &[("SEMANTIC_COMMIT_HEADER_WIDTH", "80")],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
    assert!(
        as_str(&output.stderr).contains("commit header exceeds 80 characters (max 80)"),
        "stderr was: {}",
        as_str(&output.stderr)
    );
}

#[test]
fn commit_max_header_width_flag_wins_over_env_default() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let message = header_with_total_len(79);
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--max-header-width",
            "80",
            "--message",
            &message,
        ],
        &[("SEMANTIC_COMMIT_HEADER_WIDTH", "72")],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
}

#[test]
fn commit_max_header_width_env_rejects_non_positive_values() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--validate-only",
            "--message",
            "feat(core): add thing",
        ],
        &[("SEMANTIC_COMMIT_HEADER_WIDTH", "0")],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(
        as_str(&output.stderr).contains("SEMANTIC_COMMIT_HEADER_WIDTH must be a positive integer"),
        "stderr was: {}",
        as_str(&output.stderr)
    );
}

#[test]
fn commit_dry_run_validates_and_checks_staged_without_committing() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &["commit", "--dry-run", "--message", "feat(core): add thing"],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(0));

    let head = common::git_output(repo.path(), &["rev-parse", "--verify", "HEAD"]);
    assert!(!head.status.success(), "expected no commit during dry-run");
}

#[test]
fn commit_json_outputs_result_metadata() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &["commit", "--json", "--message", "feat(core): add thing"],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).expect("commit json");
    assert_eq!(json["schema_version"], "cli.semantic-commit.commit.v1");
    assert_eq!(json["operation"], "commit");
    assert_eq!(json["dry_run"], false);
    assert_eq!(json["staged"]["file_count"], 1);
    assert_eq!(json["staged"]["files"][0]["path"], "a.txt");
    assert_eq!(
        json["commit"]["sha"].as_str().expect("sha"),
        git_trim(repo.path(), &["rev-parse", "HEAD"])
    );
    assert!(as_str(&output.stderr).trim().is_empty());
}

#[test]
fn commit_amend_no_edit_reuses_head_message() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");
    let first = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "feat(core): add thing",
            "--no-summary",
        ],
        &[],
        None,
    );
    assert_eq!(first.status.code(), Some(0));

    stage_file(repo.path(), "b.txt", "world\n");
    let amend = common::run_semantic_commit_output(
        repo.path(),
        &["commit", "--amend", "--no-edit", "--no-summary"],
        &[],
        None,
    );

    assert_eq!(
        amend.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&amend.stderr)
    );
    assert_eq!(git_trim(repo.path(), &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(
        git_trim(repo.path(), &["show", "-s", "--format=%s", "HEAD"]),
        "feat(core): add thing"
    );
    assert_eq!(git_trim(repo.path(), &["show", "HEAD:b.txt"]), "world");
}

#[test]
fn commit_message_only_amend_updates_message_without_staged_changes() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");
    let first = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "feat(core): add thing",
            "--no-summary",
        ],
        &[],
        None,
    );
    assert_eq!(first.status.code(), Some(0));

    let amend = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--amend",
            "--message-only",
            "--message",
            "fix(core): improve subject",
            "--no-summary",
        ],
        &[],
        None,
    );

    assert_eq!(
        amend.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&amend.stderr)
    );
    assert_eq!(git_trim(repo.path(), &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(
        git_trim(repo.path(), &["show", "-s", "--format=%s", "HEAD"]),
        "fix(core): improve subject"
    );
}

#[test]
fn commit_message_only_rejects_staged_changes() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");
    let first = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "feat(core): add thing",
            "--no-summary",
        ],
        &[],
        None,
    );
    assert_eq!(first.status.code(), Some(0));

    stage_file(repo.path(), "b.txt", "world\n");
    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--amend",
            "--message-only",
            "--message",
            "fix(core): improve subject",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("--message-only requires no staged changes"));
}

#[test]
fn commit_require_clean_rejects_unstaged_changes() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");
    common::write_file(repo.path(), "b.txt", "unstaged\n");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--dry-run",
            "--require-clean",
            "--message",
            "feat(core): add thing",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("unstaged or untracked changes present"));
}

#[test]
fn commit_expect_head_rejects_mismatch() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");
    let first = common::run_semantic_commit_output(
        repo.path(),
        &["commit", "--message", "feat(core): first", "--no-summary"],
        &[],
        None,
    );
    assert_eq!(first.status.code(), Some(0));
    let first_sha = git_trim(repo.path(), &["rev-parse", "HEAD"]);

    stage_file(repo.path(), "b.txt", "world\n");
    let second = common::run_semantic_commit_output(
        repo.path(),
        &["commit", "--message", "feat(core): second", "--no-summary"],
        &[],
        None,
    );
    assert_eq!(second.status.code(), Some(0));

    stage_file(repo.path(), "c.txt", "again\n");
    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--dry-run",
            "--expect-head",
            &first_sha,
            "--message",
            "feat(core): third",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("HEAD mismatch"));
}

#[test]
fn commit_structured_message_builds_valid_commit_message() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--type",
            "Feat",
            "--scope",
            "Core",
            "--subject",
            "add thing",
            "--body-bullet",
            "add supporting behavior",
            "--no-summary",
        ],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
    assert_eq!(
        git_trim(repo.path(), &["show", "-s", "--format=%s", "HEAD"]),
        "feat(core): add thing"
    );
    let body = common::git(repo.path(), &["show", "-s", "--format=%B", "HEAD"]);
    assert!(body.contains("- Add supporting behavior"));
}

#[test]
fn commit_trailer_appends_git_trailer() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "feat(core): add thing",
            "--trailer",
            "Refs: #573",
            "--no-summary",
        ],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
    let body = common::git(repo.path(), &["show", "-s", "--format=%B", "HEAD"]);
    assert!(body.contains("Refs: #573"));
}

#[test]
fn commit_signoff_appends_committer_signoff() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "feat(core): add thing",
            "--signoff",
            "--no-summary",
        ],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
    let body = common::git(repo.path(), &["show", "-s", "--format=%B", "HEAD"]);
    assert!(body.contains("Signed-off-by: Test User <test@example.com>"));
}

#[test]
fn commit_default_summary_falls_back_to_git_show_when_git_scope_missing() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let envs_owned = deterministic_env("/usr/bin:/bin:/usr/sbin:/sbin");
    let envs = env_refs(&envs_owned);
    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "feat(core): add thing",
            "--no-progress",
        ],
        &envs,
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(as_str(&output.stderr).contains("warning: git-scope summary unavailable"));
    assert!(as_str(&output.stdout).contains("feat(core): add thing"));
}

#[test]
fn commit_no_summary_suppresses_summary_output() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let envs_owned = deterministic_env("/usr/bin:/bin:/usr/sbin:/sbin");
    let envs = env_refs(&envs_owned);
    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "-m",
            "feat(core): add thing",
            "--no-summary",
            "--no-progress",
        ],
        &envs,
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(as_str(&output.stdout).trim().is_empty());
}

#[test]
fn commit_quiet_suppresses_progress_and_summary_output() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let envs_owned = deterministic_env("/usr/bin:/bin:/usr/sbin:/sbin");
    let envs = env_refs(&envs_owned);
    let output = common::run_semantic_commit_output(
        repo.path(),
        &["commit", "--message", "feat(core): add thing", "--quiet"],
        &envs,
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(as_str(&output.stdout).trim().is_empty());
    assert!(!as_str(&output.stderr).contains("semantic-commit"));
    assert!(!as_str(&output.stderr).contains("git-scope summary unavailable"));
}

#[test]
fn commit_git_show_summary_overrides_git_pager() {
    let env_without_pager = deterministic_env("/usr/bin:/bin:/usr/sbin:/sbin");
    let env_without_pager = env_refs(&env_without_pager);
    let without_pager =
        run_git_show_summary_commit(&env_without_pager, "without-pager.txt", "hello\n");

    let env_with_pager = deterministic_env_with_pager("/usr/bin:/bin:/usr/sbin:/sbin", "less");
    let env_with_pager = env_refs(&env_with_pager);
    let with_pager = run_git_show_summary_commit(&env_with_pager, "without-pager.txt", "hello\n");

    assert_eq!(without_pager.status.code(), Some(0));
    assert_eq!(with_pager.status.code(), Some(0));
    assert_eq!(as_str(&with_pager.stdout), as_str(&without_pager.stdout));
}

#[test]
fn commit_git_show_summary_mode_works_without_git_scope() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let envs_owned = deterministic_env("/usr/bin:/bin:/usr/sbin:/sbin");
    let envs = env_refs(&envs_owned);
    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "feat(core): add thing",
            "--summary",
            "git-show",
            "--no-progress",
        ],
        &envs,
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(as_str(&output.stdout).contains("feat(core): add thing"));
    assert!(!as_str(&output.stderr).contains("warning: git-scope"));
}

fn run_git_show_summary_commit(
    envs: &[(&'static str, &str)],
    file_name: &str,
    contents: &str,
) -> std::process::Output {
    let repo = common::init_repo();
    stage_file(repo.path(), file_name, contents);

    common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "feat(core): add thing",
            "--summary",
            "git-show",
            "--no-progress",
        ],
        envs,
        None,
    )
}

#[test]
fn commit_message_out_writes_recovery_message() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");
    let out_path = repo.path().join("commit-message.txt");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "Feat(core): bad",
            "--message-out",
            "commit-message.txt",
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
    let saved = fs::read_to_string(out_path).expect("read message-out file");
    assert_eq!(saved, "Feat(core): bad");
}

#[test]
fn commit_message_file_successfully_commits_with_git_scope() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");
    common::write_file(
        repo.path(),
        "message.txt",
        "feat(core): add thing\n\n- Add thing\n",
    );

    let tool_dir = tempfile::TempDir::new().expect("tempdir");
    common::write_executable(
        tool_dir.path(),
        "git-scope",
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1-}" != "commit" || "${2-}" != "HEAD" || "${3-}" != "--no-color" ]]; then
  echo "unexpected args: $*" >&2
  exit 2
fi
echo "GIT_SCOPE_OK"
"#,
    );

    let tool_dir = tool_dir.path().to_str().expect("tool dir str");
    let path_env = format!("{tool_dir}:/usr/bin:/bin:/usr/sbin:/sbin");
    let envs_owned = deterministic_env(&path_env);
    let envs = env_refs(&envs_owned);
    let output = common::run_semantic_commit_output(
        repo.path(),
        &["commit", "-F", "message.txt", "--no-progress"],
        &envs,
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(as_str(&output.stdout).contains("GIT_SCOPE_OK"));
}

#[cfg(unix)]
#[test]
fn commit_falls_back_when_git_scope_is_not_executable() {
    use std::os::unix::fs::PermissionsExt;

    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let tool_dir = tempfile::TempDir::new().expect("tempdir");
    let tool_path = tool_dir.path().join("git-scope");
    fs::write(&tool_path, "#!/usr/bin/env bash\nexit 0\n").expect("write git-scope");
    let mut perms = fs::metadata(&tool_path).expect("metadata").permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&tool_path, perms).expect("set permissions");

    let tool_dir = tool_dir.path().to_str().unwrap();
    let path_env = format!("{tool_dir}:/usr/bin:/bin:/usr/sbin:/sbin");
    let envs_owned = deterministic_env(&path_env);
    let envs = env_refs(&envs_owned);
    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "feat(core): add thing",
            "--no-progress",
        ],
        &envs,
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(as_str(&output.stderr).contains("warning: git-scope summary unavailable"));
    assert!(as_str(&output.stdout).contains("feat(core): add thing"));
}

#[test]
fn commit_auto_fix_wraps_overlength_body_line() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let long_word_run = "word ".repeat(30);
    let long_bullet = long_word_run.trim_end();
    let message = format!("feat: test\n\n- {long_bullet}\n");
    let output = common::run_semantic_commit_output(
        repo.path(),
        &["commit", "--auto-fix", "--validate-only"],
        &[],
        Some(&message),
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
}

#[test]
fn commit_auto_fix_lowercases_uppercase_type_and_bullet() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--auto-fix",
            "--validate-only",
            "--message",
            "Feat(Core): add thing",
        ],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
}

#[test]
fn commit_auto_fix_does_not_truncate_overlength_header() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let header = format!("feat: {}", "a".repeat(120));
    let output = common::run_semantic_commit_output(
        dir.path(),
        &[
            "commit",
            "--auto-fix",
            "--validate-only",
            "--message",
            &header,
        ],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
    assert!(
        as_str(&output.stderr).contains("commit header exceeds 100 characters"),
        "stderr was: {}",
        as_str(&output.stderr)
    );
}

#[test]
fn commit_auto_fix_message_out_captures_normalized_message() {
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");
    let out_path = repo.path().join("commit-message.txt");

    let output = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--auto-fix",
            "--dry-run",
            "--message",
            "Feat(Core): add thing",
            "--message-out",
            "commit-message.txt",
        ],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
    let saved = fs::read_to_string(out_path).expect("read message-out file");
    assert_eq!(saved, "feat(core): add thing");
}

#[test]
fn commit_repo_flag_commits_from_external_cwd() {
    let outer = tempfile::TempDir::new().expect("tempdir");
    let repo = common::init_repo();
    stage_file(repo.path(), "a.txt", "hello\n");

    let envs_owned = deterministic_env("/usr/bin:/bin:/usr/sbin:/sbin");
    let envs = env_refs(&envs_owned);
    let repo_path = repo.path().to_str().expect("repo path");
    let output = common::run_semantic_commit_output(
        outer.path(),
        &[
            "commit",
            "--repo",
            repo_path,
            "--message",
            "feat(core): add thing",
            "--summary",
            "none",
            "--no-progress",
        ],
        &envs,
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    let head = common::git_output(repo.path(), &["rev-parse", "--verify", "HEAD"]);
    assert!(
        head.status.success(),
        "expected commit to be created in --repo target"
    );
}

#[test]
fn fixup_creates_fixup_commit_for_target() {
    let repo = common::init_repo();
    stage_file(repo.path(), "base.txt", "base\n");
    let base = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "feat(core): base change",
            "--no-summary",
        ],
        &[],
        None,
    );
    assert_eq!(base.status.code(), Some(0));

    stage_file(repo.path(), "fix.txt", "fix\n");
    let output = common::run_semantic_commit_output(
        repo.path(),
        &["fixup", "--target", "HEAD", "--no-summary"],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
    assert_eq!(
        git_trim(repo.path(), &["show", "-s", "--format=%s", "HEAD"]),
        "fixup! feat(core): base change"
    );
}

#[test]
fn squash_json_dry_run_reports_target_without_committing() {
    let repo = common::init_repo();
    stage_file(repo.path(), "base.txt", "base\n");
    let base = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "feat(core): base change",
            "--no-summary",
        ],
        &[],
        None,
    );
    assert_eq!(base.status.code(), Some(0));
    let base_sha = git_trim(repo.path(), &["rev-parse", "HEAD"]);

    stage_file(repo.path(), "squash.txt", "squash\n");
    let output = common::run_semantic_commit_output(
        repo.path(),
        &["squash", "--target", "HEAD", "--json", "--dry-run"],
        &[],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).expect("squash json");
    assert_eq!(json["operation"], "squash");
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["target"]["sha"], base_sha);
    assert_eq!(json["generated_subject"], "squash! feat(core): base change");
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), base_sha);
}

#[test]
fn squash_creates_squash_commit_without_opening_editor() {
    let repo = common::init_repo();
    stage_file(repo.path(), "base.txt", "base\n");
    let base = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "feat(core): base change",
            "--no-summary",
        ],
        &[],
        None,
    );
    assert_eq!(base.status.code(), Some(0));

    common::write_executable(repo.path(), "fail-editor.sh", "#!/bin/sh\nexit 42\n");
    let editor = repo.path().join("fail-editor.sh");
    stage_file(repo.path(), "squash.txt", "squash\n");
    let output = common::run_semantic_commit_output(
        repo.path(),
        &["squash", "--target", "HEAD", "--json", "--no-summary"],
        &[("GIT_EDITOR", editor.to_str().expect("editor path"))],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was: {}",
        as_str(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).expect("squash json");
    assert_eq!(json["operation"], "squash");
    assert_eq!(
        git_trim(repo.path(), &["show", "-s", "--format=%s", "HEAD"]),
        "squash! feat(core): base change"
    );
}

#[test]
fn fixup_invalid_target_fails_without_commit() {
    let repo = common::init_repo();
    stage_file(repo.path(), "base.txt", "base\n");
    let base = common::run_semantic_commit_output(
        repo.path(),
        &[
            "commit",
            "--message",
            "feat(core): base change",
            "--no-summary",
        ],
        &[],
        None,
    );
    assert_eq!(base.status.code(), Some(0));
    let base_sha = git_trim(repo.path(), &["rev-parse", "HEAD"]);

    stage_file(repo.path(), "fix.txt", "fix\n");
    let output = common::run_semantic_commit_output(
        repo.path(),
        &["fixup", "--target", "missing-target"],
        &[],
        None,
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(as_str(&output.stderr).contains("target revision not found"));
    assert_eq!(git_trim(repo.path(), &["rev-parse", "HEAD"]), base_sha);
}

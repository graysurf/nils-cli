use crate::common;
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

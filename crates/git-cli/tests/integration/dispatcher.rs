use crate::common;
use pretty_assertions::assert_eq;

use common::GitCliHarness;

fn top_level_usage() -> &'static str {
    r#"Usage:
  git-cli <group> <command> [args]

Groups:
  utils    zip | copy-staged | root | commit-hash
  reset    soft | mixed | hard | undo | back-head | back-checkout | remote
  commit   context | context-json | to-stash
  branch   cleanup
  ci       pick
  open     repo | branch | default-branch | commit | compare | pr | pulls | issues | actions | releases | tags | commits | file | blame
  completion  bash | zsh

Help:
  git-cli help
  git-cli <group> help

Examples:
  git-cli utils zip
  git-cli reset hard 3
"#
}

#[test]
fn no_args_prints_top_level_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &[]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(output.stdout_text(), top_level_usage());
}

#[test]
fn help_prints_top_level_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["help"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(output.stdout_text(), top_level_usage());
}

#[test]
fn unknown_group_prints_error_and_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["nope"]);

    assert_eq!(output.code, 2);
    assert_eq!(output.stderr_text(), "Unknown group: nope\n");
    assert_eq!(output.stdout_text(), top_level_usage());
}

#[test]
fn group_usage_prints_help_for_group() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["utils"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(
        output.stdout_text(),
        "Usage: git-cli utils <command> [args]\n  zip | copy-staged | root | commit-hash\n"
    );
}

#[test]
fn group_help_token_prints_group_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["ci", "--help"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(
        output.stdout_text(),
        "Usage: git-cli ci <command> [args]\n  pick\n"
    );
}

#[test]
fn open_group_usage_prints_help_for_group() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["open"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_eq!(
        output.stdout_text(),
        "Usage: git-cli open <command> [args]\n  repo | branch | default-branch | commit | compare | pr | pulls | issues | actions | releases | tags | commits | file | blame\n"
    );
}

#[test]
fn unknown_command_prints_error_and_group_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["utils", "nope"]);

    assert_eq!(output.code, 2);
    assert_eq!(output.stderr_text(), "Unknown utils command: nope\n");
    assert_eq!(
        output.stdout_text(),
        "Usage: git-cli utils <command> [args]\n  zip | copy-staged | root | commit-hash\n"
    );
}

#[test]
fn commit_unknown_command_prints_error_and_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["commit", "nope"]);

    assert_eq!(output.code, 2);
    assert_eq!(output.stderr_text(), "Unknown commit command: nope\n");
    assert_eq!(
        output.stdout_text(),
        "Usage: git-cli commit <command> [args]\n  context | context-json | to-stash\n"
    );
}

#[test]
fn unknown_group_help_prints_error_and_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["nope", "help"]);

    assert_eq!(output.code, 2);
    assert_eq!(output.stderr_text(), "Unknown group: nope\n");
    assert_eq!(output.stdout_text(), top_level_usage());
}

#[test]
fn commit_context_outside_repo_fails() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["commit", "context"]);

    assert_eq!(output.code, 1);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(output.stderr_text(), "❌ Not a git repository.\n");
}

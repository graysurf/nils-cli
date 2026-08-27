use crate::common;
use pretty_assertions::assert_eq;

use common::GitCliHarness;

fn assert_contains(text: &str, needle: &str) {
    assert!(
        text.contains(needle),
        "expected output to contain {needle:?}\n\n{text}"
    );
}

/// Assert one `Commands:` row, ignoring clap's column padding. The padding
/// reflows whenever a longer command name is added, which says nothing about
/// whether the row is present.
fn assert_command_row(stdout: &str, name: &str, description: &str) {
    let expected = format!("{name} {description}");
    let found = stdout
        .lines()
        .any(|line| line.split_whitespace().collect::<Vec<_>>().join(" ") == expected);
    assert!(
        found,
        "expected a command row for {name:?} described as {description:?}\n\n{stdout}"
    );
}

fn assert_top_level_help(stdout: &str) {
    assert_contains(stdout, "Git helper CLI");
    assert_contains(stdout, "Usage: git-cli <group> <command> [args]");
    assert_contains(stdout, "Commands:");
    assert_command_row(stdout, "utils", "Utility helpers");
    assert_command_row(stdout, "summary", "Summarize repository history");
    assert_command_row(stdout, "completion", "Export shell completion script");
    assert_command_row(
        stdout,
        "push",
        "Publish the checked-out branch to its own remote branch",
    );
    assert_command_row(
        stdout,
        "sync-default",
        "Fast-forward the local default branch to its remote-tracking ref",
    );
    assert_command_row(
        stdout,
        "sync-branch",
        "Fast-forward the checked-out non-default branch to its remote-tracking ref",
    );
    assert_contains(stdout, "-V, --version  Print version");
}

#[test]
fn no_args_prints_top_level_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &[]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_top_level_help(&output.stdout_text());
}

#[test]
fn help_prints_top_level_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["help"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    assert_top_level_help(&output.stdout_text());
}

#[test]
fn unknown_group_prints_error_and_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["nope"]);

    assert_eq!(output.code, 64);
    assert_eq!(output.stdout_text(), "");
    let stderr = output.stderr_text();
    assert_contains(&stderr, "error: unrecognized subcommand 'nope'");
    assert_contains(&stderr, "Usage: git-cli <group> <command> [args]");
}

#[test]
fn group_usage_prints_help_for_group() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["utils"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    let stdout = output.stdout_text();
    assert_contains(&stdout, "Utility helpers");
    assert_contains(&stdout, "Usage: utils [COMMAND]");
    assert_contains(&stdout, "zip          Create zip archive from HEAD");
    assert_contains(&stdout, "copy-staged  Copy staged diff to clipboard");
}

#[test]
fn group_help_token_prints_group_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["ci", "--help"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    let stdout = output.stdout_text();
    assert_contains(&stdout, "CI helpers");
    assert_contains(&stdout, "Usage: git-cli ci [COMMAND]");
    assert_contains(&stdout, "pick  Cherry-pick into CI branch");
}

#[test]
fn open_group_usage_prints_help_for_group() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["open"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    let stdout = output.stdout_text();
    assert_contains(&stdout, "Open remote pages");
    assert_contains(&stdout, "Usage: open [COMMAND]");
    assert_contains(&stdout, "default-branch  Open default branch tree page");
    assert_contains(&stdout, "pulls           Open pull or merge request list");
}

#[test]
fn worktree_group_help_exposes_governed_dirty_commands() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["worktree", "--help"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    let stdout = output.stdout_text();
    assert_contains(&stdout, "dirty-snapshot");
    assert_contains(&stdout, "adopt-dirty");
    assert_contains(&stdout, "revoke-dirty");
}

#[test]
fn summary_group_delegates_to_the_standalone_summary_cli() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["summary", "--help"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    let stdout = output.stdout_text();
    assert_contains(&stdout, "Git history summary CLI");
    assert_contains(&stdout, "this-month");
    assert_contains(&stdout, "--format <FORMAT>");
    assert_contains(&stdout, "--no-mailmap");
}

#[test]
fn unknown_command_prints_error_and_group_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["utils", "nope"]);

    assert_eq!(output.code, 64);
    assert_eq!(output.stdout_text(), "");
    let stderr = output.stderr_text();
    assert_contains(&stderr, "error: unrecognized subcommand 'nope'");
    assert_contains(&stderr, "Usage: git-cli utils [COMMAND]");
}

#[test]
fn commit_unknown_command_prints_error_and_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["commit", "nope"]);

    assert_eq!(output.code, 64);
    assert_eq!(output.stdout_text(), "");
    let stderr = output.stderr_text();
    assert_contains(&stderr, "error: unrecognized subcommand 'nope'");
    assert_contains(&stderr, "Usage: git-cli commit [COMMAND]");
}

#[test]
fn unknown_group_help_prints_error_and_usage() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["nope", "help"]);

    assert_eq!(output.code, 64);
    assert_eq!(output.stdout_text(), "");
    let stderr = output.stderr_text();
    assert_contains(&stderr, "error: unrecognized subcommand 'nope'");
    assert_contains(&stderr, "Usage: git-cli <group> <command> [args]");
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

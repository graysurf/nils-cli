use crate::common;
use common::{GitCliHarness, git, init_repo};
use nils_test_support::cmd::{CmdOutput, run_with};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::path::Path;

fn run_with_agent_home(
    harness: &GitCliHarness,
    cwd: &Path,
    agent_home: &Path,
    args: &[&str],
) -> CmdOutput {
    let agent_home = agent_home.to_string_lossy().to_string();
    let options = harness.cmd_options(cwd).with_env("AGENT_HOME", &agent_home);
    run_with(&harness.git_cli_bin(), args, &options)
}

fn parse_json(output: &CmdOutput) -> Value {
    serde_json::from_str(output.stdout_text().trim()).expect("valid json output")
}

#[test]
fn worktree_add_creates_deterministic_agent_home_path_and_lists_json() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let agent_home = tempfile::TempDir::new().expect("agent home");

    let add = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &[
            "worktree",
            "add",
            "topic-one",
            "--from",
            "main",
            "--format",
            "json",
        ],
    );

    assert_eq!(add.code, 0, "stderr: {}", add.stderr_text());
    assert_eq!(add.stderr_text(), "");

    let add_json = parse_json(&add);
    assert_eq!(add_json["schema_version"], "cli.git-cli.worktree.add.v1");
    assert_eq!(add_json["ok"], true);
    assert_eq!(add_json["data"]["slug"], "topic-one");
    assert_eq!(add_json["data"]["branch"], "feat/topic-one");

    let repo_key = add_json["data"]["repo_key"].as_str().expect("repo key");
    let path = add_json["data"]["path"].as_str().expect("path");
    let canonical_agent_home = agent_home
        .path()
        .canonicalize()
        .expect("canonical agent home");
    let canonical_agent_home_text = canonical_agent_home.to_string_lossy().to_string();
    let expected_path = canonical_agent_home
        .join("worktrees")
        .join(repo_key)
        .join("topic-one");
    assert_eq!(
        add_json["data"]["agent_home"].as_str(),
        Some(canonical_agent_home_text.as_str())
    );
    assert_eq!(path, expected_path.to_string_lossy());
    assert!(expected_path.exists(), "worktree path should exist");

    let porcelain = git(repo.path(), &["worktree", "list", "--porcelain"]);
    assert!(porcelain.contains("branch refs/heads/feat/topic-one"));
    assert!(porcelain.contains(path));

    let list = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &["worktree", "list", "--format", "json"],
    );
    assert_eq!(list.code, 0, "stderr: {}", list.stderr_text());
    let list_json = parse_json(&list);
    assert_eq!(list_json["schema_version"], "cli.git-cli.worktree.list.v1");

    let entries = list_json["data"]["entries"]
        .as_array()
        .expect("entries array");
    let managed = entries
        .iter()
        .find(|entry| entry["path"].as_str() == Some(path))
        .expect("managed worktree listed");
    assert_eq!(managed["branch"], "feat/topic-one");
    assert_eq!(managed["managed"], true);
}

#[test]
fn worktree_add_existing_slug_fails_with_json_error() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let agent_home = tempfile::TempDir::new().expect("agent home");

    let first = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &["worktree", "add", "topic-one", "--from", "main"],
    );
    assert_eq!(first.code, 0, "stderr: {}", first.stderr_text());

    let second = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &[
            "worktree",
            "add",
            "topic-one",
            "--from",
            "main",
            "--format",
            "json",
        ],
    );

    assert_ne!(second.code, 0);
    assert_eq!(second.stderr_text(), "");
    let json = parse_json(&second);
    assert_eq!(json["schema_version"], "cli.git-cli.worktree.add.v1");
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "branch-exists");
    assert!(
        json["error"]["message"]
            .as_str()
            .expect("message")
            .contains("feat/topic-one")
    );
}

#[test]
fn worktree_remove_parse_error_respects_json_format() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let agent_home = tempfile::TempDir::new().expect("agent home");

    let output = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &["worktree", "remove", "--format", "json"],
    );

    assert_eq!(output.code, 64);
    assert_eq!(output.stderr_text(), "");
    let json = parse_json(&output);
    assert_eq!(json["schema_version"], "cli.git-cli.worktree.remove.v1");
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "invalid-target-count");
}

#[test]
fn worktree_remove_refuses_primary_and_removes_managed_slug() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let agent_home = tempfile::TempDir::new().expect("agent home");

    let add = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &[
            "worktree",
            "add",
            "topic-two",
            "--from",
            "main",
            "--format",
            "json",
        ],
    );
    assert_eq!(add.code, 0, "stderr: {}", add.stderr_text());
    let add_json = parse_json(&add);
    let path = add_json["data"]["path"].as_str().expect("path");
    assert!(Path::new(path).exists());
    fs::create_dir(repo.path().join("topic-two")).expect("local shadow dir");

    let primary = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &[
            "worktree",
            "remove",
            repo.path().to_str().expect("utf8 repo path"),
            "--format",
            "json",
        ],
    );
    assert_ne!(primary.code, 0);
    let primary_json = parse_json(&primary);
    assert_eq!(primary_json["error"]["code"], "refuse-primary-worktree");

    let remove = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &["worktree", "remove", "topic-two", "--format", "json"],
    );
    assert_eq!(remove.code, 0, "stderr: {}", remove.stderr_text());
    assert_eq!(remove.stderr_text(), "");

    let remove_json = parse_json(&remove);
    assert_eq!(
        remove_json["schema_version"],
        "cli.git-cli.worktree.remove.v1"
    );
    assert_eq!(remove_json["ok"], true);
    assert_eq!(remove_json["data"]["removed_path"], path);
    assert!(!Path::new(path).exists(), "worktree path should be removed");
}

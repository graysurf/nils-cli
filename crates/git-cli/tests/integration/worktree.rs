use crate::common;
use common::{GitCliHarness, git, init_bare_remote, init_repo};
use nils_test_support::cmd::{CmdOutput, run_with};
use nils_test_support::git::git_output;
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

/// Read one config key, distinguishing "unset" from "set to the empty string".
fn git_config_optional(repo: &Path, key: &str) -> Option<String> {
    let output = git_output(repo, &["config", "--get", key]);
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
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
    assert_eq!(
        add_json["data"]["kind"], "feature",
        "default kind is feature"
    );
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
fn worktree_add_kind_bug_uses_fix_branch_prefix() {
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
            "topic-bug",
            "--from",
            "main",
            "--kind",
            "bug",
            "--format",
            "json",
        ],
    );

    assert_eq!(add.code, 0, "stderr: {}", add.stderr_text());
    assert_eq!(add.stderr_text(), "");

    let add_json = parse_json(&add);
    assert_eq!(add_json["ok"], true);
    assert_eq!(add_json["data"]["slug"], "topic-bug");
    assert_eq!(add_json["data"]["kind"], "bug");
    assert_eq!(
        add_json["data"]["branch"], "fix/topic-bug",
        "kind=bug derives the fix/ prefix forge-cli's --kind bug expects"
    );

    let path = add_json["data"]["path"].as_str().expect("path");
    let porcelain = git(repo.path(), &["worktree", "list", "--porcelain"]);
    assert!(porcelain.contains("branch refs/heads/fix/topic-bug"));
    assert!(porcelain.contains(path));
}

#[test]
fn worktree_add_rejects_unknown_kind() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let agent_home = tempfile::TempDir::new().expect("agent home");

    let add = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &[
            "worktree", "add", "topic-x", "--kind", "nope", "--from", "main", "--format", "json",
        ],
    );

    assert_ne!(add.code, 0, "unknown --kind must fail");
    let add_json = parse_json(&add);
    assert_eq!(add_json["ok"], false);
    assert_eq!(add_json["error"]["code"], "invalid-kind");
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
fn worktree_remove_with_branch_name_hints_slug_in_text_and_json() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let agent_home = tempfile::TempDir::new().expect("agent home");

    // A docs-kind worktree: branch `docs/topic-docs`, slug `topic-docs`.
    let add = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &[
            "worktree",
            "add",
            "topic-docs",
            "--from",
            "main",
            "--kind",
            "docs",
            "--format",
            "json",
        ],
    );
    assert_eq!(add.code, 0, "stderr: {}", add.stderr_text());
    let add_json = parse_json(&add);
    assert_eq!(add_json["data"]["branch"], "docs/topic-docs");

    // Passing the branch name (text mode) fails but points at the slug + path.
    let text = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &["worktree", "remove", "docs/topic-docs"],
    );
    assert_ne!(text.code, 0);
    let stderr = text.stderr_text();
    assert!(stderr.contains("hint:"), "stderr: {stderr}");
    assert!(stderr.contains("branch name"), "stderr: {stderr}");
    assert!(stderr.contains("slug 'topic-docs'"), "stderr: {stderr}");

    // The same mistake in JSON mode carries the hint on the error envelope.
    let json = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &["worktree", "remove", "docs/topic-docs", "--format", "json"],
    );
    assert_ne!(json.code, 0);
    let json = parse_json(&json);
    assert_eq!(json["error"]["code"], "worktree-not-found");
    let hint = json["error"]["hint"].as_str().expect("hint");
    assert!(hint.contains("slug 'topic-docs'"), "hint: {hint}");
}

#[test]
fn worktree_list_from_linked_worktree_resolves_primary_repo_root() {
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
    let add_json = parse_json(&add);
    let linked_path = add_json["data"]["path"].as_str().expect("path").to_string();
    let expected_repo_root = repo
        .path()
        .canonicalize()
        .expect("canonical repo")
        .to_string_lossy()
        .to_string();

    // Run `worktree list` from INSIDE the linked worktree. The managed layout
    // (repo_root / repo_key / managed flag) must reflect the PRIMARY worktree,
    // not the linked one we happen to stand in.
    let list = run_with_agent_home(
        &harness,
        Path::new(&linked_path),
        agent_home.path(),
        &["worktree", "list", "--format", "json"],
    );
    assert_eq!(list.code, 0, "stderr: {}", list.stderr_text());
    let list_json = parse_json(&list);
    assert_eq!(
        list_json["data"]["repo_root"].as_str(),
        Some(expected_repo_root.as_str()),
        "repo_root should resolve to the primary worktree even from inside a linked worktree"
    );

    let entries = list_json["data"]["entries"]
        .as_array()
        .expect("entries array");
    let managed = entries
        .iter()
        .find(|entry| entry["path"].as_str() == Some(linked_path.as_str()))
        .expect("managed worktree listed");
    assert_eq!(
        managed["managed"], true,
        "managed worktree must stay classified managed from inside a linked worktree"
    );
}

#[test]
fn worktree_go_resolves_slug_and_emits_path_shell_and_json() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let agent_home = tempfile::TempDir::new().expect("agent home");

    let add = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &[
            "worktree", "add", "topic-go", "--from", "main", "--format", "json",
        ],
    );
    assert_eq!(add.code, 0, "stderr: {}", add.stderr_text());
    let path = parse_json(&add)["data"]["path"]
        .as_str()
        .expect("path")
        .to_string();

    // Default text mode prints the bare resolved path (composable with `cd`).
    let go = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &["worktree", "go", "topic-go"],
    );
    assert_eq!(go.code, 0, "stderr: {}", go.stderr_text());
    assert_eq!(go.stdout_text().trim(), path);

    // Shell mode prints an evaluable `cd -- <path>` command.
    let go_shell = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &["worktree", "go", "topic-go", "--shell"],
    );
    assert_eq!(go_shell.code, 0, "stderr: {}", go_shell.stderr_text());
    let shell_out = go_shell.stdout_text();
    assert!(
        shell_out.trim_start().starts_with("cd -- "),
        "stdout: {shell_out}"
    );
    assert!(shell_out.contains(&path), "stdout: {shell_out}");

    // JSON mode carries the resolved metadata under a versioned envelope.
    let go_json = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &["worktree", "go", "topic-go", "--format", "json"],
    );
    assert_eq!(go_json.code, 0, "stderr: {}", go_json.stderr_text());
    let json = parse_json(&go_json);
    assert_eq!(json["schema_version"], "cli.git-cli.worktree.go.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["path"].as_str(), Some(path.as_str()));
    assert_eq!(json["data"]["branch"], "feat/topic-go");
    assert_eq!(json["data"]["managed"], true);
}

#[test]
fn worktree_go_resolves_branch_name_from_a_linked_worktree() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let agent_home = tempfile::TempDir::new().expect("agent home");

    let alpha = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &[
            "worktree", "add", "alpha", "--from", "main", "--format", "json",
        ],
    );
    assert_eq!(alpha.code, 0, "stderr: {}", alpha.stderr_text());
    let alpha_path = parse_json(&alpha)["data"]["path"]
        .as_str()
        .expect("alpha path")
        .to_string();

    let beta = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &[
            "worktree", "add", "beta", "--from", "main", "--format", "json",
        ],
    );
    assert_eq!(beta.code, 0, "stderr: {}", beta.stderr_text());
    let beta_path = parse_json(&beta)["data"]["path"]
        .as_str()
        .expect("beta path")
        .to_string();

    // From inside alpha, jump to beta by its full branch name.
    let go = run_with_agent_home(
        &harness,
        Path::new(&alpha_path),
        agent_home.path(),
        &["worktree", "go", "feat/beta", "--format", "json"],
    );
    assert_eq!(go.code, 0, "stderr: {}", go.stderr_text());
    let json = parse_json(&go);
    assert_eq!(json["data"]["path"].as_str(), Some(beta_path.as_str()));
}

#[test]
fn worktree_go_unknown_target_errors_in_json() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let agent_home = tempfile::TempDir::new().expect("agent home");

    let go = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &["worktree", "go", "does-not-exist", "--format", "json"],
    );
    assert_ne!(go.code, 0);
    assert_eq!(go.stderr_text(), "");
    let json = parse_json(&go);
    assert_eq!(json["schema_version"], "cli.git-cli.worktree.go.v1");
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "worktree-not-found");
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

#[test]
fn worktree_add_does_not_adopt_the_base_ref_as_upstream() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let remote = init_bare_remote();
    let agent_home = tempfile::TempDir::new().expect("agent home");

    let remote_path = remote.path().to_string_lossy().to_string();
    git(repo.path(), &["remote", "add", "origin", &remote_path]);
    git(repo.path(), &["push", "-u", "origin", "main"]);
    git(repo.path(), &["remote", "set-head", "origin", "main"]);

    let add = run_with_agent_home(
        &harness,
        repo.path(),
        agent_home.path(),
        &["worktree", "add", "topic-upstream", "--format", "json"],
    );
    assert_eq!(add.code, 0, "stderr: {}", add.stderr_text());

    let add_json = parse_json(&add);
    assert_eq!(
        add_json["data"]["base_ref"], "origin/main",
        "the default base ref stays the cached remote default branch"
    );
    assert_eq!(add_json["data"]["branch"], "feat/topic-upstream");

    // Branching from `origin/main` must not make the default branch this
    // branch's upstream. A managed worktree branch is unpublished, so any
    // consumer that reads `@{upstream}` to find the branch head — `forge-cli pr
    // deliver` among them — would resolve the default branch instead and report
    // the head as unpushed.
    assert_eq!(
        git_config_optional(repo.path(), "branch.feat/topic-upstream.merge"),
        None,
        "a new managed branch must not inherit an upstream ref"
    );
    assert_eq!(
        git_config_optional(repo.path(), "branch.feat/topic-upstream.remote"),
        None,
        "a new managed branch must not inherit an upstream remote"
    );

    let worktree_path = add_json["data"]["path"].as_str().expect("path");
    let upstream = git_output(
        Path::new(worktree_path),
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    );
    assert!(
        !upstream.status.success(),
        "an unpublished managed branch has no upstream, got {}",
        String::from_utf8_lossy(&upstream.stdout).trim()
    );
}

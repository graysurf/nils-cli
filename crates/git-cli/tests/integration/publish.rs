use crate::common;
use common::{GitCliHarness, git, init_bare_remote, init_repo};
use nils_test_support::cmd::CmdOutput;
use nils_test_support::git::{commit_file, git_output};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

fn parse_json(output: &CmdOutput) -> Value {
    serde_json::from_str(output.stdout_text().trim()).expect("valid json output")
}

/// A repository with `origin` wired, `main` published, and `origin/HEAD` cached,
/// which is the shape every real agent checkout has after a clone.
fn repo_with_published_main() -> (TempDir, TempDir) {
    let repo = init_repo();
    let remote = init_bare_remote();
    let remote_path = remote.path().to_string_lossy().to_string();
    git(repo.path(), &["remote", "add", "origin", &remote_path]);
    git(repo.path(), &["push", "-u", "origin", "main"]);
    git(repo.path(), &["remote", "set-head", "origin", "main"]);
    (repo, remote)
}

fn git_config_optional(repo: &Path, key: &str) -> Option<String> {
    let output = git_output(repo, &["config", "--get", key]);
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn rev_parse(repo: &Path, rev: &str) -> String {
    git(repo, &["rev-parse", rev]).trim().to_string()
}

// ---------------------------------------------------------------- push

#[test]
fn push_publishes_the_current_branch_and_sets_its_own_upstream() {
    let harness = GitCliHarness::new();
    let (repo, remote) = repo_with_published_main();

    git(
        repo.path(),
        &["checkout", "-q", "--no-track", "-b", "feat/topic"],
    );
    commit_file(repo.path(), "topic.txt", "one\n", "add topic");
    let head = rev_parse(repo.path(), "HEAD");

    let output = harness.run(repo.path(), &["push", "--format", "json"]);
    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());

    let json = parse_json(&output);
    assert_eq!(json["schema_version"], "cli.git-cli.push.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["branch"], "feat/topic");
    assert_eq!(json["data"]["remote"], "origin");
    assert_eq!(json["data"]["remote_branch"], "feat/topic");
    assert_eq!(json["data"]["head"], head);
    assert_eq!(json["data"]["pushed"], true);
    assert_eq!(json["data"]["created_remote_branch"], true);
    assert_eq!(json["data"]["default_branch"], "main");

    assert_eq!(
        rev_parse(remote.path(), "refs/heads/feat/topic"),
        head,
        "the remote branch holds the pushed head"
    );

    // The upstream is established at publish time and points at this branch's
    // own ref, which is what `worktree add --no-track` deliberately leaves unset.
    assert_eq!(
        git_config_optional(repo.path(), "branch.feat/topic.merge").as_deref(),
        Some("refs/heads/feat/topic")
    );
    assert_eq!(
        git_config_optional(repo.path(), "branch.feat/topic.remote").as_deref(),
        Some("origin")
    );
}

#[test]
fn push_repairs_an_upstream_that_points_at_the_default_branch() {
    let harness = GitCliHarness::new();
    let (repo, _remote) = repo_with_published_main();

    // Exactly what `worktree add` produced before it passed `--no-track`: the
    // branch exists, and its upstream is the default branch. "Has an upstream"
    // is true here, so publishing must look at *which* ref it names.
    git(repo.path(), &["checkout", "-q", "-b", "feat/inherited"]);
    git(
        repo.path(),
        &["config", "branch.feat/inherited.remote", "origin"],
    );
    git(
        repo.path(),
        &["config", "branch.feat/inherited.merge", "refs/heads/main"],
    );
    commit_file(repo.path(), "inherited.txt", "one\n", "add inherited");

    let output = harness.run(repo.path(), &["push", "--format", "json"]);
    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());

    assert_eq!(
        git_config_optional(repo.path(), "branch.feat/inherited.merge").as_deref(),
        Some("refs/heads/feat/inherited"),
        "publishing repairs an upstream inherited from the default branch"
    );
}

#[test]
fn push_refuses_the_default_branch() {
    let harness = GitCliHarness::new();
    let (repo, remote) = repo_with_published_main();
    let published = rev_parse(repo.path(), "HEAD");
    commit_file(repo.path(), "local.txt", "local\n", "local only");

    let output = harness.run(repo.path(), &["push", "--format", "json"]);
    assert_ne!(output.code, 0);

    let json = parse_json(&output);
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "refuse-default-branch");
    let hint = json["error"]["hint"].as_str().unwrap_or_default();
    assert!(
        hint.contains("forge-cli repo push-default"),
        "the refusal names the governed default-branch surface, got: {hint}"
    );

    assert_eq!(
        rev_parse(remote.path(), "refs/heads/main"),
        published,
        "the remote default branch is untouched"
    );
}

#[test]
fn push_dry_run_reports_the_plan_without_publishing() {
    let harness = GitCliHarness::new();
    let (repo, remote) = repo_with_published_main();

    git(
        repo.path(),
        &["checkout", "-q", "--no-track", "-b", "feat/dry"],
    );
    commit_file(repo.path(), "dry.txt", "one\n", "add dry");

    let output = harness.run(repo.path(), &["push", "--dry-run", "--format", "json"]);
    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());

    let json = parse_json(&output);
    assert_eq!(json["data"]["pushed"], false);
    assert_eq!(json["data"]["dry_run"], true);
    assert_eq!(json["data"]["branch"], "feat/dry");

    assert!(
        !git_output(
            remote.path(),
            &["rev-parse", "--verify", "refs/heads/feat/dry"]
        )
        .status
        .success(),
        "a dry run publishes nothing"
    );
}

#[test]
fn push_refuses_a_detached_head() {
    let harness = GitCliHarness::new();
    let (repo, _remote) = repo_with_published_main();
    git(repo.path(), &["checkout", "-q", "--detach"]);

    let output = harness.run(repo.path(), &["push", "--format", "json"]);
    assert_ne!(output.code, 0);
    assert_eq!(parse_json(&output)["error"]["code"], "detached-head");
}

#[test]
fn push_refuses_when_the_remote_default_branch_is_unknown() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let remote = init_bare_remote();
    let remote_path = remote.path().to_string_lossy().to_string();
    git(repo.path(), &["remote", "add", "origin", &remote_path]);
    git(repo.path(), &["push", "origin", "main"]);
    git(
        repo.path(),
        &["checkout", "-q", "--no-track", "-b", "feat/unknown"],
    );
    commit_file(repo.path(), "x.txt", "x\n", "add x");

    let output = harness.run(repo.path(), &["push", "--format", "json"]);
    assert_ne!(output.code, 0, "an unprovable default branch fails closed");

    let json = parse_json(&output);
    assert_eq!(json["error"]["code"], "default-branch-unresolved");
    let hint = json["error"]["hint"].as_str().unwrap_or_default();
    assert!(
        hint.contains("--expect-default"),
        "the refusal names the offline escape hatch, got: {hint}"
    );

    // The escape hatch names what the default *is*, so it can admit this push
    // without ever admitting a push of the default branch itself.
    let admitted = harness.run(
        repo.path(),
        &["push", "--expect-default", "main", "--format", "json"],
    );
    assert_eq!(admitted.code, 0, "stderr: {}", admitted.stderr_text());
    assert_eq!(parse_json(&admitted)["data"]["pushed"], true);
}

#[test]
fn push_expect_default_cannot_widen_admission() {
    // `--expect-default` exists so an offline caller can name the default branch
    // when the remote head is not cached. It must never be able to *admit* a
    // push that would otherwise be refused, or it is a bypass rather than an
    // escape hatch: asserting some other branch while standing on the real
    // default would publish the default branch.
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let remote = init_bare_remote();
    let remote_path = remote.path().to_string_lossy().to_string();
    git(repo.path(), &["remote", "add", "origin", &remote_path]);
    git(repo.path(), &["push", "origin", "main"]);
    let published = rev_parse(repo.path(), "HEAD");
    commit_file(repo.path(), "local.txt", "local\n", "local only");

    // No cached `origin/HEAD`, standing on `main`, asserting a different branch.
    let output = harness.run(
        repo.path(),
        &["push", "--expect-default", "develop", "--format", "json"],
    );
    assert_ne!(
        output.code, 0,
        "a wrong --expect-default must not admit a push of a default-looking branch"
    );
    assert_eq!(
        rev_parse(remote.path(), "refs/heads/main"),
        published,
        "the remote default branch is untouched"
    );

    // With the real default cached, a disagreeing assertion is a mismatch, not a
    // second opinion that wins.
    git(repo.path(), &["remote", "set-head", "origin", "main"]);
    let mismatch = harness.run(
        repo.path(),
        &["push", "--expect-default", "develop", "--format", "json"],
    );
    assert_ne!(mismatch.code, 0);
    assert_eq!(
        parse_json(&mismatch)["error"]["code"],
        "expect-default-mismatch"
    );
}

#[test]
fn push_expect_default_still_refuses_the_default_branch() {
    let harness = GitCliHarness::new();
    let repo = init_repo();
    let remote = init_bare_remote();
    let remote_path = remote.path().to_string_lossy().to_string();
    git(repo.path(), &["remote", "add", "origin", &remote_path]);
    git(repo.path(), &["push", "origin", "main"]);
    commit_file(repo.path(), "local.txt", "local\n", "local only");

    // Uncached remote head: refused before the assertion is even consulted,
    // because a conventional default-branch name cannot be cleared by an
    // unverifiable claim.
    let unverifiable = harness.run(
        repo.path(),
        &["push", "--expect-default", "main", "--format", "json"],
    );
    assert_ne!(unverifiable.code, 0);
    assert_eq!(
        parse_json(&unverifiable)["error"]["code"],
        "default-branch-unverifiable"
    );

    // Cached remote head: the assertion agrees, and the push is refused for the
    // plain reason that this *is* the default branch.
    git(repo.path(), &["remote", "set-head", "origin", "main"]);
    let refused = harness.run(
        repo.path(),
        &["push", "--expect-default", "main", "--format", "json"],
    );
    assert_ne!(refused.code, 0);
    assert_eq!(
        parse_json(&refused)["error"]["code"],
        "refuse-default-branch"
    );
}

// -------------------------------------------------------- sync-default

/// Advance the remote's default branch by one commit, from a scratch clone, so
/// the repository under test genuinely lags behind its remote.
fn advance_remote_main(remote: &TempDir) -> String {
    let scratch = TempDir::new().expect("scratch clone");
    let remote_path = remote.path().to_string_lossy().to_string();
    let scratch_path = scratch.path().to_string_lossy().to_string();
    // `git init --bare` leaves the remote HEAD on `master`, so the clone has to
    // name the branch it wants rather than inherit an unborn one.
    git(
        remote.path(),
        &[
            "clone",
            "-q",
            "--branch",
            "main",
            &remote_path,
            &scratch_path,
        ],
    );
    git(
        scratch.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(scratch.path(), &["config", "user.name", "Test"]);
    commit_file(scratch.path(), "remote.txt", "remote\n", "advance remote");
    git(scratch.path(), &["push", "-q", "origin", "main"]);
    rev_parse(scratch.path(), "HEAD")
}

#[test]
fn sync_default_fast_forwards_the_checked_out_default_branch() {
    let harness = GitCliHarness::new();
    let (repo, remote) = repo_with_published_main();
    let previous = rev_parse(repo.path(), "HEAD");
    let advanced = advance_remote_main(&remote);

    let output = harness.run(repo.path(), &["sync-default", "--format", "json"]);
    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());

    let json = parse_json(&output);
    assert_eq!(json["schema_version"], "cli.git-cli.sync-default.v1");
    assert_eq!(json["data"]["default_branch"], "main");
    assert_eq!(json["data"]["strategy"], "merge-ff-only");
    assert_eq!(json["data"]["previous_head"], previous);
    assert_eq!(json["data"]["new_head"], advanced);
    assert_eq!(json["data"]["already_current"], false);
    assert_eq!(json["data"]["fetched"], true);

    assert_eq!(rev_parse(repo.path(), "refs/heads/main"), advanced);
}

#[test]
fn sync_default_updates_the_ref_when_the_default_branch_is_not_checked_out() {
    let harness = GitCliHarness::new();
    let (repo, remote) = repo_with_published_main();
    let previous = rev_parse(repo.path(), "HEAD");
    let advanced = advance_remote_main(&remote);

    // The everyday agent shape: working on a topic branch while local `main`
    // lags. No working tree holds `main`, so its ref can move on its own.
    git(
        repo.path(),
        &["checkout", "-q", "--no-track", "-b", "feat/side"],
    );
    commit_file(repo.path(), "side.txt", "side\n", "add side");
    let side = rev_parse(repo.path(), "HEAD");

    let output = harness.run(repo.path(), &["sync-default", "--format", "json"]);
    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());

    let json = parse_json(&output);
    assert_eq!(json["data"]["strategy"], "update-ref");
    assert_eq!(json["data"]["previous_head"], previous);
    assert_eq!(json["data"]["new_head"], advanced);

    assert_eq!(rev_parse(repo.path(), "refs/heads/main"), advanced);
    assert_eq!(
        rev_parse(repo.path(), "HEAD"),
        side,
        "the checked-out topic branch is untouched"
    );
}

#[test]
fn sync_default_is_a_noop_when_already_current() {
    let harness = GitCliHarness::new();
    let (repo, _remote) = repo_with_published_main();
    let head = rev_parse(repo.path(), "HEAD");

    let output = harness.run(repo.path(), &["sync-default", "--format", "json"]);
    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());

    let json = parse_json(&output);
    assert_eq!(json["data"]["already_current"], true);
    assert_eq!(json["data"]["strategy"], "noop");
    assert_eq!(json["data"]["previous_head"], head);
    assert_eq!(json["data"]["new_head"], head);
}

#[test]
fn sync_default_refuses_a_non_fast_forward() {
    let harness = GitCliHarness::new();
    let (repo, remote) = repo_with_published_main();
    advance_remote_main(&remote);

    // Diverge locally: local `main` gains a commit the remote does not have, so
    // adopting the remote head would discard local work.
    commit_file(repo.path(), "local.txt", "local\n", "local only");
    let diverged = rev_parse(repo.path(), "HEAD");

    let output = harness.run(repo.path(), &["sync-default", "--format", "json"]);
    assert_ne!(output.code, 0);

    let json = parse_json(&output);
    assert_eq!(json["error"]["code"], "not-fast-forward");
    assert_eq!(
        rev_parse(repo.path(), "refs/heads/main"),
        diverged,
        "a refused sync never moves the default branch"
    );
}

#[test]
fn sync_default_refuses_a_dirty_checked_out_default_branch() {
    let harness = GitCliHarness::new();
    let (repo, remote) = repo_with_published_main();
    let previous = rev_parse(repo.path(), "HEAD");
    advance_remote_main(&remote);

    std::fs::write(repo.path().join("dirty.txt"), "dirty\n").expect("write dirty file");
    git(repo.path(), &["add", "dirty.txt"]);

    let output = harness.run(repo.path(), &["sync-default", "--format", "json"]);
    assert_ne!(output.code, 0);
    assert_eq!(parse_json(&output)["error"]["code"], "dirty-checkout");
    assert_eq!(rev_parse(repo.path(), "refs/heads/main"), previous);
}

#[test]
fn sync_default_dry_run_reports_the_plan_without_moving_the_ref() {
    let harness = GitCliHarness::new();
    let (repo, remote) = repo_with_published_main();
    let previous = rev_parse(repo.path(), "HEAD");
    let advanced = advance_remote_main(&remote);

    let output = harness.run(
        repo.path(),
        &["sync-default", "--dry-run", "--format", "json"],
    );
    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());

    let json = parse_json(&output);
    assert_eq!(json["data"]["dry_run"], true);
    assert_eq!(json["data"]["strategy"], "merge-ff-only");
    assert_eq!(json["data"]["new_head"], advanced);
    assert_eq!(
        rev_parse(repo.path(), "refs/heads/main"),
        previous,
        "a dry run moves nothing"
    );
}

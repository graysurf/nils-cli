//! Full-chain integration tests for the `pr deliver` macro. These exercise
//! the entire execute_sequence — auth.status / repo.view / pr.create /
//! pr.wait-checks / pr.ready / pr.merge — against a comprehensive gh stub
//! that branches on argv. The dry-run path is covered by the sibling
//! `pr_deliver.rs` module; this one pins the lock-step ordering and
//! short-circuit behaviour against real subprocess wiring.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::support::{CmdOutput, StubEnv, parse_envelope, run_forge_cli_in};

const FIXTURE_CREATE_STDOUT: &str = include_str!("../fixtures/github/pr_create/create_stdout.txt");
const FIXTURE_CHECKS_JSON: &str = include_str!("../fixtures/github/pr_checks/all_success.json");

/// Full pr.view JSON used by every step that re-fetches the PR. The fixture
/// at `tests/fixtures/github/pr_create/view_response.json` was designed for
/// pr.create's bespoke parser and omits `state`/`mergeable`; the macro's
/// chain reuses `pr_view::parse_view_output` (via pr.ready / pr.merge) which
/// requires the full Sprint 2 shape, so we inline a complete payload here.
const FULL_PR_VIEW_JSON: &str = r#"{
  "number": 123,
  "url": "https://github.com/sympoies/nils-cli/pull/123",
  "state": "OPEN",
  "isDraft": false,
  "title": "feat: sample feature",
  "headRefName": "feat/sample",
  "baseRefName": "main",
  "mergeable": "MERGEABLE",
  "mergedAt": null,
  "labels": []
}"#;

/// Post-merge `pr view` payload — same as [`FULL_PR_VIEW_JSON`] except the
/// state has flipped to `MERGED`. Used by stubs that need to surface a
/// merged PR after the merge step ran (and possibly exited non-zero).
const MERGED_PR_VIEW_JSON: &str = r#"{
  "number": 123,
  "url": "https://github.com/sympoies/nils-cli/pull/123",
  "state": "MERGED",
  "isDraft": false,
  "title": "feat: sample feature",
  "headRefName": "feat/sample",
  "baseRefName": "main",
  "mergeable": "MERGEABLE",
  "mergedAt": "2026-05-23T12:00:00Z",
  "labels": []
}"#;

/// Build the canonical Sprint 2 git tempdir: clean worktree on `feat/sample`
/// tracking `origin/feat/sample` at the same SHA, with the remote URL set to
/// `https://<host>/<repo_slug>.git` so provider detection lands on GitHub.
fn make_git_repo() -> TempDir {
    let tempdir = TempDir::new().expect("tempdir");
    let repo = tempdir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");

    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .expect("git spawn");
        if !out.status.success() {
            panic!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        out
    };

    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Tester"]);
    git(&["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "init\n").expect("readme");
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "initial"]);
    git(&["checkout", "-q", "-b", "feat/sample"]);
    git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/sympoies/nils-cli.git",
    ]);
    let head_sha = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();
    let upstream_ref = repo.join(".git/refs/remotes/origin/feat/sample");
    fs::create_dir_all(upstream_ref.parent().unwrap()).unwrap();
    fs::write(&upstream_ref, format!("{head_sha}\n")).unwrap();
    git(&[
        "branch",
        "-q",
        "--set-upstream-to=origin/feat/sample",
        "feat/sample",
    ]);
    tempdir
}

fn write_full_chain_stub(stub: &StubEnv) -> PathBuf {
    write_chain_stub(stub, FULL_PR_VIEW_JSON, MERGED_PR_VIEW_JSON, false)
}

/// Chain stub with a controllable `pr merge` failure mode used by the
/// idempotency-on-non-zero-exit regression test. When `merge_exits_one` is
/// true, the merge subcommand touches a sentinel file in `stub.tempdir`
/// and exits 1 with a non-fatal stderr message. Subsequent `pr view`
/// calls observe the sentinel and switch to the post-merge JSON so the
/// macro can verify the PR actually landed.
fn write_chain_stub_with_merge_exit(
    stub: &StubEnv,
    pre_view: &str,
    post_view: &str,
    merge_exits_one: bool,
) -> PathBuf {
    write_chain_stub(stub, pre_view, post_view, merge_exits_one)
}

fn write_chain_stub(
    stub: &StubEnv,
    pre_view: &str,
    post_view: &str,
    merge_exits_one: bool,
) -> PathBuf {
    let sentinel = stub.tempdir.path().join("merge-called");
    let merge_branch = if merge_exits_one {
        format!(
            "    touch {sentinel}\n    echo 'X stderr warning after merge' >&2\n    exit 1\n",
            sentinel = sentinel.display(),
        )
    } else {
        format!(
            "    touch {sentinel}\n    :\n",
            sentinel = sentinel.display(),
        )
    };
    let body = format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "auth status")
    cat <<'EOF' 1>&2
github.com
  ✓ Logged in to github.com account testuser-gh (keyring)
  - Token scopes: 'repo', 'read:org'
EOF
    ;;
  "repo view")
    cat <<'EOF'
{{
  "name": "nils-cli",
  "owner": {{ "login": "sympoies" }},
  "url": "https://github.com/sympoies/nils-cli",
  "defaultBranchRef": {{ "name": "main" }},
  "mergeCommitAllowed": false,
  "squashMergeAllowed": true,
  "rebaseMergeAllowed": false
}}
EOF
    ;;
  "pr create")
    cat <<'EOF'
{create}
EOF
    ;;
  "pr checks")
    cat <<'EOF'
{checks}
EOF
    ;;
  "pr ready")
    :
    ;;
  "api graphql")
    # Merge lock-down rule 12 — review-thread sweep; all resolved.
    cat <<'EOF'
{{ "data": {{ "repository": {{ "pullRequest": {{ "reviewThreads": {{ "nodes": [] }} }} }} }} }}
EOF
    ;;
  "pr merge")
{merge_branch}
    ;;
  "pr view")
    # Distinguish the post-merge merge_sha view (--json mergeCommit) from
    # regular view; for the latter, switch to the merged-state JSON once
    # the merge step has been observed via the sentinel file.
    case "$*" in
      *"--json mergeCommit"*)
        cat <<'EOF'
{{ "mergeCommit": {{ "oid": "abc123def456" }} }}
EOF
        ;;
      *)
        if [ -e {sentinel} ]; then
          cat <<'EOF'
{post_view}
EOF
        else
          cat <<'EOF'
{pre_view}
EOF
        fi
        ;;
    esac
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
        create = FIXTURE_CREATE_STDOUT,
        pre_view = pre_view,
        post_view = post_view,
        checks = FIXTURE_CHECKS_JSON,
        merge_branch = merge_branch,
        sentinel = sentinel.display(),
    );
    let path = stub.tempdir.path().join("gh");
    fs::write(&path, body).expect("write gh stub");
    let mut perm = fs::metadata(&path).expect("metadata").permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).expect("chmod");
    path
}

fn run_in_repo(stub: &StubEnv, repo: &Path, args: &[&str]) -> CmdOutput {
    run_forge_cli_in(stub, args, Some(repo))
}

#[test]
fn pr_deliver_full_chain_no_merge_emits_four_steps_and_returns_success() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let gh_path = write_full_chain_stub(&stub);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nLand the new feature.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
            "--no-merge",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.pr.deliver.v1");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["kind"], "feature");
    assert_eq!(envelope["data"]["provider"], "github");
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        steps,
        vec!["auth_status", "repo_view", "create", "wait_checks"],
        "no-merge must stop after wait_checks"
    );
    assert_eq!(envelope["data"]["pr"]["merged"], false);
    assert!(envelope["data"]["pr"]["merge_sha"].is_null());
    assert_eq!(envelope["data"]["pr"]["number"], 123);
}

#[test]
fn pr_deliver_full_chain_emits_all_six_steps_with_merge_sha() {
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let gh_path = write_full_chain_stub(&stub);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nLand the new feature.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        steps,
        vec![
            "auth_status",
            "repo_view",
            "create",
            "wait_checks",
            "ready",
            "merge",
        ],
        "full chain must include all six steps in order"
    );
    assert_eq!(envelope["data"]["pr"]["merged"], true);
    assert_eq!(envelope["data"]["pr"]["merge_sha"], "abc123def456");
    // Every step carries its atom's schema literal.
    let schemas: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["schema_version"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        schemas,
        vec![
            "cli.forge-cli.auth.status.v1",
            "cli.forge-cli.repo.view.v1",
            "cli.forge-cli.pr.create.v1",
            "cli.forge-cli.pr.checks.v1",
            "cli.forge-cli.pr.ready.v1",
            "cli.forge-cli.pr.merge.v1",
        ]
    );
}

#[test]
fn pr_deliver_short_circuits_when_pr_create_validation_fails_with_data_65() {
    // Title over 70 chars trips pr.create's title_length validation. The
    // macro must surface DATA 65 (not a remapped runtime code) and omit
    // later steps (wait_checks / ready / merge) from data.steps[].
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let gh_path = write_full_chain_stub(&stub);
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let long_title = "a".repeat(71);
    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            &long_title,
            "--body",
            "## Summary\n\nx\n\n## Test plan\n\ny\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
        ],
    );
    assert_eq!(
        out.code, 65,
        "expected DATA 65 on title_too_long, stderr={}",
        out.stderr
    );
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "title_too_long");
    // Only auth_status + repo_view ran before the validation failed.
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(steps, vec!["auth_status", "repo_view"]);
}

#[test]
fn pr_deliver_treats_gh_exit_1_after_successful_merge_as_success() {
    // Regression: `gh pr merge` can return exit 1 even after the API merge
    // call succeeds (typically a branch-cleanup race or post-merge stderr
    // warning treated as failure). The pr.merge atom should re-check the
    // PR state and treat the chain as successful when GitHub reports the
    // PR as merged, so `forge-cli pr deliver` does not surface a phantom
    // `backend_error: gh exited with status 1` after the PR is actually on
    // main.
    let tempdir = make_git_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let gh_path = write_chain_stub_with_merge_exit(
        &stub,
        FULL_PR_VIEW_JSON,
        MERGED_PR_VIEW_JSON,
        /*merge_exits_one=*/ true,
    );
    let stub = stub.env("FORGE_CLI_GH_BIN", gh_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "deliver",
            "--kind",
            "feature",
            "--title",
            "feat: sample feature",
            "--body",
            "## Summary\n\nLand the new feature.\n\n## Test plan\n\nVerified.\n",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--timeout",
            "5s",
        ],
    );
    assert_eq!(
        out.code, 0,
        "expected success when gh exits 1 but PR is merged; stdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], true);
    let steps: Vec<&str> = envelope["data"]["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .map(|s| s["step"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        steps,
        vec![
            "auth_status",
            "repo_view",
            "create",
            "wait_checks",
            "ready",
            "merge",
        ],
        "merge step must still be recorded as ok=true after recovery"
    );
    assert_eq!(envelope["data"]["pr"]["merged"], true);
    assert_eq!(envelope["data"]["pr"]["merge_sha"], "abc123def456");
}

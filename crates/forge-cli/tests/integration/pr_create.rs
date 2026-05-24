//! End-to-end `pr create` integration tests.
//!
//! These tests drive the real `forge-cli` binary against a temporary git
//! work tree so the validation chain (branch / kind / body / worktree /
//! head_pushed) runs against actual git output, plus a dispatching backend
//! stub that branches on argv to mimic `gh` / `glab` for the create + view
//! call chain. Token-shaped strings never appear in fixtures — see
//! `tests/fixtures/{github,gitlab}/pr_create/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::support::{CmdOutput, StubEnv, parse_envelope, run_forge_cli_in};

const FIXTURE_GH_CREATE_STDOUT: &str =
    include_str!("../fixtures/github/pr_create/create_stdout.txt");
const FIXTURE_GH_VIEW_JSON: &str = include_str!("../fixtures/github/pr_create/view_response.json");
const FIXTURE_GLAB_CREATE_STDOUT: &str =
    include_str!("../fixtures/gitlab/pr_create/create_stdout.txt");
const FIXTURE_GLAB_VIEW_JSON: &str =
    include_str!("../fixtures/gitlab/pr_create/view_response.json");

/// Set up a temp git repo at `tempdir/repo`. Creates a single commit on
/// `main`, then a `feat/sample` branch tracking a synthetic
/// `refs/remotes/origin/feat/sample` ref pointing at the same SHA, and a
/// matching `origin` remote URL. The repo is clean.
fn make_git_repo(provider_host: &str, repo_slug: &str) -> TempDir {
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
        &format!("https://{provider_host}/{repo_slug}.git"),
    ]);
    // Synthesize the upstream ref by direct write — avoids needing a real
    // remote.
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

fn well_formed_body() -> &'static str {
    "## Summary\n\nLand the new feature.\n\n## Test plan\n\nVerified via cargo test.\n"
}

fn write_label_catalog() -> (TempDir, String) {
    let tempdir = TempDir::new().expect("label catalog tempdir");
    let path = tempdir.path().join("forge-labels.yaml");
    fs::write(
        &path,
        r#"schema: forge-label-catalog.v1
groups:
  - name: type
    prefix: "type::"
    exclusive: true
labels:
  - name: "type::feature"
    group: type
    color: a2eeef
    description: Feature work.
    applies_to: [pr, mr]
"#,
    )
    .expect("write catalog");
    (tempdir, path.to_string_lossy().into_owned())
}

#[test]
fn pr_create_dry_run_renders_plan_envelope() {
    let tempdir = make_git_repo("github.com", "sympoies/nils-cli");
    let repo_path = tempdir.path().join("repo");
    // No backend call needed when --base is provided (skips default-branch
    // resolution).
    let stub = StubEnv::new();
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "create",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--title",
            "feat: dry-run demo",
            "--kind",
            "feature",
            "--body",
            well_formed_body(),
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.pr.create.v1");
    assert_eq!(envelope["data"]["provider"], "github");
    let plan = envelope["data"]["plan"].as_array().expect("plan array");
    let plan_strings: Vec<String> = plan
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(plan_strings.contains(&"pr".to_string()), "{plan_strings:?}");
    assert!(
        plan_strings.contains(&"create".to_string()),
        "{plan_strings:?}"
    );
    assert!(
        plan_strings.contains(&"--draft".to_string()),
        "{plan_strings:?}"
    );
}

#[test]
fn pr_create_strict_labels_rejects_unknown_catalog_label() {
    let tempdir = make_git_repo("github.com", "sympoies/nils-cli");
    let repo_path = tempdir.path().join("repo");
    let (_catalog_tempdir, catalog) = write_label_catalog();

    let stub = StubEnv::new();
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "create",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--title",
            "feat: strict labels",
            "--kind",
            "feature",
            "--body",
            well_formed_body(),
            "--label",
            "priority::high",
            "--label-catalog",
            &catalog,
            "--strict-labels",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65, "expected DATA 65, stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "label_unknown");
}

#[test]
fn pr_create_rejects_dirty_worktree_with_data_exit() {
    let tempdir = make_git_repo("github.com", "sympoies/nils-cli");
    let repo_path = tempdir.path().join("repo");
    // Create an untracked file to dirty the worktree.
    fs::write(repo_path.join("dirty.txt"), "x\n").unwrap();

    let stub = StubEnv::new();
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "create",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--title",
            "feat: dirty",
            "--kind",
            "feature",
            "--body",
            well_formed_body(),
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65, "expected DATA 65, stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "dirty_worktree");
}

#[test]
fn pr_create_rejects_branch_kind_mismatch_with_data_exit() {
    let tempdir = make_git_repo("github.com", "sympoies/nils-cli");
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "create",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--title",
            "feat: wrong-kind",
            "--kind",
            "bug",
            "--body",
            well_formed_body(),
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65, "expected DATA 65, stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "branch_kind_mismatch");
}

#[test]
fn pr_create_rejects_body_without_summary_with_data_exit() {
    let tempdir = make_git_repo("github.com", "sympoies/nils-cli");
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "create",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--title",
            "feat: missing summary",
            "--kind",
            "feature",
            "--body",
            "## Test plan\n\nfine.\n",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "body_missing_summary");
}

#[test]
fn pr_create_rejects_title_over_70_chars_with_data_exit() {
    let tempdir = make_git_repo("github.com", "sympoies/nils-cli");
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let too_long = "a".repeat(71);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "create",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--title",
            &too_long,
            "--kind",
            "feature",
            "--body",
            well_formed_body(),
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["error"]["code"], "title_too_long");
}

#[test]
fn pr_create_rejects_body_and_body_file_conflict_with_usage_exit() {
    let tempdir = make_git_repo("github.com", "sympoies/nils-cli");
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "create",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--title",
            "feat: conflict",
            "--kind",
            "feature",
            "--body",
            "inline",
            "--body-file",
            "-",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 64, "expected USAGE 64, stderr={}", out.stderr);
}

/// Dispatching gh stub: branches on the first arg-2 pair (`repo view`,
/// `pr create`, `pr view`).
fn write_gh_dispatch_stub(stub: &StubEnv) -> PathBuf {
    let body = format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
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
  "pr view")
    cat <<'EOF'
{view}
EOF
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
        create = FIXTURE_GH_CREATE_STDOUT,
        view = FIXTURE_GH_VIEW_JSON,
    );
    stub.write_stub("gh", &body)
}

fn write_glab_dispatch_stub(stub: &StubEnv) -> PathBuf {
    let body = format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "repo view")
    cat <<'EOF'
{{
  "path": "nils-cli",
  "namespace": {{ "full_path": "sympoies" }},
  "web_url": "https://gitlab.com/sympoies/nils-cli",
  "default_branch": "main",
  "merge_method": "merge"
}}
EOF
    ;;
  "mr create")
    cat <<'EOF'
{create}
EOF
    ;;
  "mr view")
    cat <<'EOF'
{view}
EOF
    ;;
  *)
    echo "stub: unexpected glab args: $*" >&2
    exit 99
    ;;
esac
"#,
        create = FIXTURE_GLAB_CREATE_STDOUT,
        view = FIXTURE_GLAB_VIEW_JSON,
    );
    stub.write_stub("glab", &body)
}

fn run_in_repo(stub: &StubEnv, repo: &Path, args: &[&str]) -> CmdOutput {
    run_forge_cli_in(stub, args, Some(repo))
}

#[test]
fn pr_create_github_full_chain_emits_canonical_envelope() {
    let tempdir = make_git_repo("github.com", "sympoies/nils-cli");
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let gh_path = write_gh_dispatch_stub(&stub);
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
            "create",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--title",
            "feat: sample feature",
            "--kind",
            "feature",
            "--body",
            well_formed_body(),
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.pr.create.v1");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["provider"], "github");
    assert_eq!(envelope["data"]["number"], 123);
    assert_eq!(envelope["data"]["head"], "feat/sample");
    assert_eq!(envelope["data"]["base"], "main");
    assert_eq!(envelope["data"]["draft"], true);
    assert_eq!(envelope["data"]["kind"], "feature");
    assert_eq!(
        envelope["data"]["url"],
        "https://github.com/sympoies/nils-cli/pull/123"
    );
}

#[test]
fn pr_create_gitlab_full_chain_emits_canonical_envelope() {
    let tempdir = make_git_repo("gitlab.com", "sympoies/nils-cli");
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let glab_path = write_glab_dispatch_stub(&stub);
    let stub = stub.env("FORGE_CLI_GLAB_BIN", glab_path.to_string_lossy());

    let out = run_in_repo(
        &stub,
        &repo_path,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "pr",
            "create",
            "--head",
            "feat/sample",
            "--base",
            "main",
            "--title",
            "feat: sample feature",
            "--kind",
            "feature",
            "--body",
            well_formed_body(),
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.pr.create.v1");
    assert_eq!(envelope["data"]["provider"], "gitlab");
    assert_eq!(envelope["data"]["number"], 77);
    assert_eq!(envelope["data"]["base"], "main");
    assert_eq!(envelope["data"]["draft"], true);
    assert_eq!(
        envelope["data"]["url"],
        "https://gitlab.com/sympoies/nils-cli/-/merge_requests/77"
    );
}

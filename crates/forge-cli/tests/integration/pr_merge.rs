//! `pr merge` integration tests covering the dry-run plan envelope and the
//! configuration-driven `keep_branch_conflict` rule. The wider lock-down chain
//! (rules 4 / 6 / 7 / 8 / 9) is unit-tested in
//! `crates/forge-cli/src/ops/pr_merge.rs` and the dedicated gate suite at
//! `tests/integration/required_check_gate.rs`; this module pins the CLI surface
//! and the per-repo `.forge-cli.toml` precedence end-to-end.

use std::fs;
use std::process::Command;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::support::{StubEnv, parse_envelope, run_forge_cli_in};

const FORBIDDEN_STUB: &str = "#!/bin/sh\necho 'should not run during dry-run' >&2\nexit 99\n";

fn make_gitlab_repo() -> TempDir {
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
    git(&[
        "remote",
        "add",
        "origin",
        "https://gitlab.example.com/group/project.git",
    ]);
    tempdir
}

fn make_github_repo(config: Option<&str>) -> TempDir {
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
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Tester"]);
    git(&["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "init\n").expect("readme");
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "initial"]);
    git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/widgets.git",
    ]);
    if let Some(config) = config {
        fs::write(repo.join(".forge-cli.toml"), config).expect("review config");
        git(&["add", ".forge-cli.toml"]);
        git(&["commit", "-q", "-m", "config"]);
    }
    tempdir
}

fn github_merge_stub(
    stub: &StubEnv,
    review_nodes: &str,
    thread_nodes: &str,
    reject_reviews: bool,
) -> String {
    let merged = stub.tempdir.path().join("github-merged");
    let review_calls = stub.tempdir.path().join("review-calls");
    let second_review_response = stub.tempdir.path().join("second-review-response.json");
    let review_query = if reject_reviews {
        "echo 'native review query must be disabled' >&2; exit 98".to_string()
    } else {
        format!(
            r#"count=0
if [ -f {review_calls} ]; then count=$(cat {review_calls}); fi
count=$((count + 1))
printf '%s\n' "$count" > {review_calls}
if [ "$count" -ge 2 ] && [ -f {second_review_response} ]; then
  cat {second_review_response}
else
  cat <<'JSON'
{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head123","reviews":{{"nodes":[{review_nodes}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}
JSON
fi
"#,
            review_calls = review_calls.display(),
            second_review_response = second_review_response.display(),
        )
    };
    format!(
        r#"#!/bin/sh
set -eu
case "$1 $2" in
  "pr view")
    case "$*" in
      *"--json mergeCommit "*) printf '%s\n' '{{"mergeCommit":{{"oid":"merge456"}}}}' ;;
      *) printf '%s\n' '{{"number":7,"url":"https://github.com/acme/widgets/pull/7","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feat/reviews","headRefOid":"head123","title":"feat: reviews","body":""}}' ;;
    esac
    ;;
  "repo view")
    printf '%s\n' '{{"name":"widgets","owner":{{"login":"acme"}},"url":"https://github.com/acme/widgets","defaultBranchRef":{{"name":"main"}},"mergeCommitAllowed":true,"squashMergeAllowed":true,"rebaseMergeAllowed":true}}'
    ;;
  "pr checks") printf '%s\n' '[]' ;;
  "api graphql")
    case "$*" in
      *"reviews(first:"*) {review_query} ;;
      *"reviewThreads(first: 100)"*) printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"reviewThreads":{{"nodes":[{thread_nodes}]}}}}}}}}}}' ;;
      *) echo "unexpected graphql args: $*" >&2; exit 99 ;;
    esac
    ;;
  "pr merge") touch {merged} ;;
  *) echo "unexpected gh args: $*" >&2; exit 99 ;;
esac
"#,
        review_query = review_query,
        thread_nodes = thread_nodes,
        merged = merged.display(),
    )
}

fn gitlab_merge_api_stub(stub: &StubEnv) -> String {
    gitlab_merge_api_stub_full(stub, "[]", "null")
}

/// Same stub, with a caller-provided MR discussions response so tests can
/// exercise merge lock-down rule 13 (unresolved review threads).
fn gitlab_merge_api_stub_with_discussions(stub: &StubEnv, discussions: &str) -> String {
    gitlab_merge_api_stub_full(stub, discussions, "null")
}

/// Base stub: caller provides the MR discussions response (rule 13) and the
/// MR `description` as a raw JSON value (rule 14 — task-list gate).
fn gitlab_merge_api_stub_full(stub: &StubEnv, discussions: &str, description: &str) -> String {
    let sentinel = stub.tempdir.path().join("merged");
    let args_log = stub.tempdir.path().join("merge-args");
    format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "repo view")
    cat <<'EOF'
{{
  "namespace": {{ "full_path": "group" }},
  "path": "project",
  "web_url": "https://gitlab.example.com/group/project",
  "default_branch": "main",
  "merge_method": "merge",
  "squash_option": "default_on"
}}
EOF
    ;;
  "mr view")
    if [ -e {sentinel} ]; then
      cat <<'EOF'
{{
  "iid": 7,
  "web_url": "https://gitlab.example.com/group/project/-/merge_requests/7",
  "description": {description},
  "state": "merged",
  "draft": false,
  "title": "feat: sample",
  "source_branch": "feat/sample",
  "target_branch": "main",
  "merge_status": "can_be_merged",
  "sha": "abc123",
  "merge_commit_sha": "def456",
  "labels": [],
  "head_pipeline": {{ "id": 99, "status": "success", "web_url": "https://gitlab.example.com/group/project/-/pipelines/99" }}
}}
EOF
    else
      cat <<'EOF'
{{
  "iid": 7,
  "web_url": "https://gitlab.example.com/group/project/-/merge_requests/7",
  "description": {description},
  "state": "opened",
  "draft": false,
  "title": "feat: sample",
  "source_branch": "feat/sample",
  "target_branch": "main",
  "merge_status": "can_be_merged",
  "sha": "abc123",
  "merge_commit_sha": null,
  "labels": [],
  "head_pipeline": {{ "id": 99, "status": "success", "web_url": "https://gitlab.example.com/group/project/-/pipelines/99" }}
}}
EOF
    fi
    ;;
  "api --hostname")
    case "$*" in
      *"projects/group%2Fproject/pipelines/99/jobs?per_page=100"*)
        cat <<'EOF'
[
  {{
    "name": "build",
    "stage": "test",
    "status": "success",
    "allow_failure": false,
    "web_url": "https://gitlab.example.com/group/project/-/jobs/1"
  }}
]
EOF
        ;;
      *)
        echo "stub: unexpected api args: $*" >&2
        exit 99
        ;;
    esac
    ;;
  "api --paginate")
    case "$*" in
      *"projects/group%2Fproject/merge_requests/7/discussions?per_page=100"*)
        # Merge lock-down rule 13 — review-thread sweep.
        cat <<'EOF'
{discussions}
EOF
        ;;
      *)
        echo "stub: unexpected paginate api args: $*" >&2
        exit 99
        ;;
    esac
    ;;
  "api --method")
    printf '%s\n' "$*" > {args_log}
    case "$*" in
      *"PUT"*"--hostname gitlab.example.com"*"projects/group%2Fproject/merge_requests/7/merge"*"squash=true"*"should_remove_source_branch=true"*"sha=abc123"*)
        touch {sentinel}
        cat <<'EOF'
{{ "state": "merged", "merge_commit_sha": "def456" }}
EOF
        ;;
      *)
        echo "stub: unexpected merge api args: $*" >&2
        exit 99
        ;;
    esac
    ;;
  *)
    echo "stub: unexpected glab args: $*" >&2
    exit 99
    ;;
esac
"#,
        sentinel = sentinel.display(),
        args_log = args_log.display(),
        discussions = discussions,
        description = description,
    )
}

#[test]
fn pr_merge_dry_run_renders_squash_plan_with_delete_branch() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "merge",
            "42",
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.merge.v1");
    let plan: Vec<String> = env["data"]["plan"]
        .as_array()
        .expect("plan array")
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(plan.iter().any(|s| s == "merge"), "{plan:?}");
    assert!(plan.iter().any(|s| s == "42"), "{plan:?}");
    assert!(plan.iter().any(|s| s == "--squash"), "{plan:?}");
    assert!(plan.iter().any(|s| s == "--delete-branch"), "{plan:?}");
}

#[test]
fn pr_merge_expected_head_rejects_provider_drift_before_mutation() {
    let tempdir = make_github_repo(None);
    let repo_path = tempdir.path().join("repo");
    let stub = StubEnv::new();
    let merged = stub.tempdir.path().join("github-merged");
    let body = github_merge_stub(&stub, "", "", false);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--expected-head",
            "head-reviewed",
        ],
        Some(&repo_path),
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(
        env["error"]["code"],
        "test_first_evidence_provider_head_mismatch"
    );
    assert!(
        !merged.exists(),
        "merge mutation must not run after head drift"
    );
}

#[test]
fn pr_merge_expected_head_allows_the_matching_provider_head() {
    let tempdir = make_github_repo(None);
    let repo_path = tempdir.path().join("repo");
    let stub = StubEnv::new();
    let merged = stub.tempdir.path().join("github-merged");
    let body = github_merge_stub(&stub, "", "", true);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--expected-head",
            "head123",
        ],
        Some(&repo_path),
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["merge_sha"], "merge456");
    assert!(merged.exists(), "matching reviewed head must reach merge");
}

#[test]
fn pr_merge_github_dry_run_exposes_enabled_review_convergence() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "merge",
            "42",
            "--review-convergence",
        ],
        None,
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["review_convergence"]["require"], true);
}

#[test]
fn pr_merge_gitlab_dry_run_rejects_enabled_review_convergence() {
    let tempdir = make_gitlab_repo();
    let repo_path = tempdir.path().join("repo");
    let stub = StubEnv::new().glab_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--review-convergence",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 64, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "provider_unsupported");
}

#[test]
fn pr_merge_dry_run_method_override_uses_merge_flag() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "merge",
            "1",
            "--method",
            "merge",
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plan: Vec<String> = env["data"]["plan"]
        .as_array()
        .expect("plan array")
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(plan.iter().any(|s| s == "--merge"), "{plan:?}");
    assert!(!plan.iter().any(|s| s == "--squash"), "{plan:?}");
}

#[test]
fn pr_merge_keep_branch_drops_delete_branch_in_dry_run_plan() {
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "merge",
            "1",
            "--keep-branch",
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    let plan: Vec<String> = env["data"]["plan"]
        .as_array()
        .expect("plan array")
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(!plan.iter().any(|s| s == "--delete-branch"), "{plan:?}");
}

#[test]
fn pr_merge_keep_branch_conflicts_with_config_delete_branch_true() {
    // The lock-down rule (rule 10) requires an *explicit* config opting into
    // branch deletion before --keep-branch becomes a hard error. We mock that
    // by writing a .forge-cli.toml inside a tempdir and invoking the binary
    // from there so the loader picks it up.
    let repo = TempDir::new().expect("tempdir");
    fs::write(
        repo.path().join(".forge-cli.toml"),
        "[merge]\ndelete_branch = true\n",
    )
    .expect("write config");
    fs::create_dir_all(repo.path().join(".git")).expect("fake .git");

    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);
    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "merge",
            "1",
            "--keep-branch",
        ],
        Some(repo.path()),
    );
    assert_eq!(
        out.code, 65,
        "expected DATA 65 on keep_branch_conflict, stderr={}",
        out.stderr
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "keep_branch_conflict");
}

#[test]
fn pr_merge_gitlab_uses_api_merge_after_required_checks_pass() {
    let tempdir = make_gitlab_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let args_log = stub.tempdir.path().join("merge-args");
    let body = gitlab_merge_api_stub(&stub);
    let stub = stub.glab_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.merge.v1");
    assert_eq!(env["data"]["provider"], "gitlab");
    assert_eq!(env["data"]["merge_sha"], "def456");

    let args = fs::read_to_string(args_log).expect("merge args log");
    assert!(args.contains("--method PUT"), "{args}");
    assert!(args.contains("--hostname gitlab.example.com"), "{args}");
    assert!(
        args.contains("projects/group%2Fproject/merge_requests/7/merge"),
        "{args}"
    );
    assert!(args.contains("sha=abc123"), "{args}");
}

#[test]
fn pr_merge_observed_policy_allows_absent_bot_and_returns_snapshot() {
    let config = r#"[review_convergence]
require = true
quiet_period = "0s"
timeout = "20m"

[[review_convergence.bots]]
login = "example-review-bot"
mode = "observed"
"#;
    let tempdir = make_github_repo(Some(config));
    let repo_path = tempdir.path().join("repo");
    let stub = StubEnv::new();
    let body = github_merge_stub(&stub, "", "", false);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["review_convergence"]["required"], true);
    assert_eq!(env["data"]["review_convergence"]["head_sha"], "head123");
    assert_eq!(
        env["data"]["review_convergence"]["observed_reviews"],
        serde_json::json!([])
    );
    assert_eq!(
        env["data"]["review_convergence"]["missing_reviewers"],
        serde_json::json!([])
    );
    assert_eq!(env["data"]["review_convergence"]["unresolved_threads"], 0);
}

#[test]
fn pr_merge_enabled_review_convergence_requires_an_initial_provider_head() {
    let config = r#"[review_convergence]
require = true
quiet_period = "0s"
timeout = "20m"
"#;
    let tempdir = make_github_repo(Some(config));
    let repo_path = tempdir.path().join("repo");
    let stub = StubEnv::new();
    let body = github_merge_stub(&stub, "", "", false)
        .replace("\"headRefOid\":\"head123\",\"title\"", "\"title\"");
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "review_convergence_head_missing");
    assert!(
        !stub.tempdir.path().join("review-calls").exists(),
        "convergence must bind the initial provider head before reading reviews"
    );
    assert!(
        !stub.tempdir.path().join("github-merged").exists(),
        "missing merge CAS head must fail closed"
    );
}

#[test]
fn pr_merge_review_convergence_false_overrides_enabled_repo_config() {
    let config = r#"[review_convergence]
require = true
quiet_period = "0s"
timeout = "20m"
"#;
    let tempdir = make_github_repo(Some(config));
    let repo_path = tempdir.path().join("repo");
    let stub = StubEnv::new();
    let body = github_merge_stub(&stub, "", "", true);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--review-convergence=false",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert!(env["data"].get("review_convergence").is_none());
}

#[test]
fn pr_merge_blocks_current_head_native_changes_requested() {
    let tempdir = make_github_repo(None);
    let repo_path = tempdir.path().join("repo");
    let review = r#"{"id":"PRR_1","databaseId":1,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-1","author":{"login":"reviewer"},"state":"CHANGES_REQUESTED","commit":{"oid":"head123"},"submittedAt":"2026-07-14T04:00:00Z","body":"free-form prose is not parsed"}"#;
    let stub = StubEnv::new();
    let body = github_merge_stub(&stub, review, "", false);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--review-convergence",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "review_changes_requested");
    assert!(
        env["error"]["details"]["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("reviewer")
    );
}

#[test]
fn pr_merge_rejects_a_native_review_without_commit_oid() {
    let tempdir = make_github_repo(None);
    let repo_path = tempdir.path().join("repo");
    let review = r#"{"id":"PRR_missing_commit","databaseId":8,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-8","author":{"login":"reviewer"},"state":"CHANGES_REQUESTED","commit":null,"submittedAt":"2026-07-14T04:00:00Z","body":"must fail closed"}"#;
    let stub = StubEnv::new();
    let body = github_merge_stub(&stub, review, "", false);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--review-convergence",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "review_snapshot_incomplete");
    assert!(
        env["error"]["details"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("review.commit.oid"))
    );
    assert!(
        !stub.tempdir.path().join("github-merged").exists(),
        "malformed review data must prevent provider merge"
    );
}

#[test]
fn pr_merge_global_config_enables_review_convergence() {
    let tempdir = make_github_repo(None);
    let repo_path = tempdir.path().join("repo");
    let stub = StubEnv::new();
    let xdg_config = stub.tempdir.path().join("global-xdg");
    fs::create_dir_all(xdg_config.join("forge-cli")).expect("global config directory");
    fs::write(
        xdg_config.join("forge-cli/config.toml"),
        r#"[review_convergence]
require = true
quiet_period = "0s"
timeout = "20m"
"#,
    )
    .expect("global review config");
    let body = github_merge_stub(&stub, "", "", false);
    let stub = stub
        .env("XDG_CONFIG_HOME", xdg_config.to_string_lossy())
        .gh_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["review_convergence"]["required"], true);
}

#[test]
fn pr_merge_enabled_review_convergence_rejects_invalid_duration_config() {
    let config = r#"[review_convergence]
require = true
quiet_period = "3601s"
timeout = "20m"
"#;
    let tempdir = make_github_repo(Some(config));
    let repo_path = tempdir.path().join("repo");
    let stub = StubEnv::new();

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "invalid_review_convergence_config");
    assert!(
        env["error"]["details"]["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("quiet_period")
    );
}

#[test]
fn pr_merge_explicit_review_convergence_rejects_non_table_section() {
    let tempdir = make_github_repo(Some("review_convergence = \"not-a-table\"\n"));
    let repo_path = tempdir.path().join("repo");
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--review-convergence",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "invalid_review_convergence_config");
    assert!(
        env["error"]["details"]["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("review_convergence:not_a_table")
    );
}

#[test]
fn pr_merge_explicit_review_convergence_rejects_malformed_config_file() {
    let tempdir = make_github_repo(Some("[review_convergence\nrequire = true\n"));
    let repo_path = tempdir.path().join("repo");
    let stub = StubEnv::new().gh_stub(FORBIDDEN_STUB);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--review-convergence",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "invalid_review_convergence_config");
    assert!(
        env["error"]["details"]["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("parse_error")
    );
}

#[test]
fn pr_merge_stale_changes_requested_is_informational() {
    let config = r#"[review_convergence]
require = true
quiet_period = "0s"
timeout = "20m"

[[review_convergence.bots]]
login = "example-review-bot"
mode = "observed"
"#;
    let tempdir = make_github_repo(Some(config));
    let repo_path = tempdir.path().join("repo");
    let review = r#"{"id":"PRR_stale","databaseId":2,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-2","author":{"login":"example-review-bot[bot]"},"state":"CHANGES_REQUESTED","commit":{"oid":"old-head"},"submittedAt":"2026-07-14T03:00:00Z","body":"stale finding"}"#;
    let stub = StubEnv::new();
    let body = github_merge_stub(&stub, review, "", false);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(
        env["data"]["review_convergence"]["stale_reviews"][0]["id"],
        "PRR_stale"
    );
    assert_eq!(
        env["data"]["review_convergence"]["changes_requested_by"],
        serde_json::json!([])
    );
}

#[test]
fn pr_merge_commented_prose_is_not_a_machine_verdict() {
    let config = r#"[review_convergence]
require = true
quiet_period = "0s"
timeout = "20m"

[[review_convergence.bots]]
login = "example-review-bot"
mode = "observed"
"#;
    let tempdir = make_github_repo(Some(config));
    let repo_path = tempdir.path().join("repo");
    let review = r#"{"id":"PRR_comment","databaseId":3,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-3","author":{"login":"example-review-bot[bot]"},"state":"COMMENTED","commit":{"oid":"head123"},"submittedAt":"2026-07-14T04:00:00Z","body":"REQUEST CHANGES: this prose must not be parsed"}"#;
    let stub = StubEnv::new();
    let body = github_merge_stub(&stub, review, "", false);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(
        env["data"]["review_convergence"]["observed_reviews"][0]["state"],
        "COMMENTED"
    );
    assert_eq!(
        env["data"]["review_convergence"]["changes_requested_by"],
        serde_json::json!([])
    );
}

#[test]
fn pr_merge_rechecks_native_review_activity_immediately_before_merge() {
    let config = r#"[review_convergence]
require = true
quiet_period = "0s"
timeout = "20m"

[[review_convergence.bots]]
login = "example-review-bot"
mode = "observed"
"#;
    let tempdir = make_github_repo(Some(config));
    let repo_path = tempdir.path().join("repo");
    let initial = r#"{"id":"PRR_comment","databaseId":3,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-3","author":{"login":"example-review-bot[bot]"},"state":"COMMENTED","commit":{"oid":"head123"},"submittedAt":"2026-07-14T04:00:00Z","body":"initial"}"#;
    let stub = StubEnv::new();
    fs::write(
        stub.tempdir.path().join("second-review-response.json"),
        r#"{"data":{"repository":{"pullRequest":{"headRefOid":"head123","reviews":{"nodes":[{"id":"PRR_comment","databaseId":3,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-3","author":{"login":"example-review-bot[bot]"},"state":"COMMENTED","commit":{"oid":"head123"},"submittedAt":"2026-07-14T04:00:00Z","body":"initial"},{"id":"PRR_blocking","databaseId":4,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-4","author":{"login":"reviewer"},"state":"CHANGES_REQUESTED","commit":{"oid":"head123"},"submittedAt":"2026-07-14T04:01:00Z","body":"late request"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}"#,
    )
    .expect("second review response");
    let body = github_merge_stub(&stub, initial, "", false);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "review_changes_requested");
    assert!(
        !stub.tempdir.path().join("github-merged").exists(),
        "late review must prevent provider merge"
    );
}

#[test]
fn pr_merge_restarts_after_late_observed_bot_activity_before_merge() {
    let config = r#"[review_convergence]
require = true
quiet_period = "0s"
timeout = "20m"

[[review_convergence.bots]]
login = "example-review-bot"
mode = "observed"
"#;
    let tempdir = make_github_repo(Some(config));
    let repo_path = tempdir.path().join("repo");
    let initial = r#"{"id":"PRR_comment","databaseId":3,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-3","author":{"login":"example-review-bot[bot]"},"state":"COMMENTED","commit":{"oid":"head123"},"submittedAt":"2026-07-14T04:00:00Z","body":"initial"}"#;
    let stub = StubEnv::new();
    fs::write(
        stub.tempdir.path().join("second-review-response.json"),
        r#"{"data":{"repository":{"pullRequest":{"headRefOid":"head123","reviews":{"nodes":[{"id":"PRR_comment","databaseId":3,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-3","author":{"login":"example-review-bot[bot]"},"state":"COMMENTED","commit":{"oid":"head123"},"submittedAt":"2026-07-14T04:00:00Z","body":"initial"},{"id":"PRR_followup","databaseId":4,"url":"https://github.com/acme/widgets/pull/7#pullrequestreview-4","author":{"login":"example-review-bot[bot]"},"state":"COMMENTED","commit":{"oid":"head123"},"submittedAt":"2026-07-14T04:01:00Z","body":"late follow-up"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}"#,
    )
    .expect("second review response");
    let body = github_merge_stub(&stub, initial, "", false);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "review_convergence_activity_changed");
    assert!(
        !stub.tempdir.path().join("github-merged").exists(),
        "late observed activity must restart convergence before provider merge"
    );
}

#[test]
fn pr_merge_review_convergence_keeps_unresolved_thread_gate_authoritative() {
    let tempdir = make_github_repo(None);
    let repo_path = tempdir.path().join("repo");
    let thread = r#"{"id":"PRRT_1","isResolved":false,"isOutdated":false,"path":"src/lib.rs","comments":{"nodes":[{"author":{"login":"reviewer"},"body":"please address","createdAt":"2026-07-14T04:00:00Z","url":"https://github.com/acme/widgets/pull/7#discussion_r1"}]}}"#;
    let stub = StubEnv::new();
    let body = github_merge_stub(&stub, "", thread, false);
    let stub = stub.gh_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--review-convergence",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "unresolved_review_threads");
}

const UNRESOLVED_DISCUSSIONS_JSON: &str = r#"[
  {
    "id": "d1",
    "notes": [
      {
        "id": 21,
        "resolvable": true,
        "resolved": false,
        "body": "please address this finding",
        "author": { "username": "quality-bot" },
        "created_at": "2026-06-11T04:49:36Z",
        "position": { "new_path": "src/lib.rs" }
      }
    ]
  }
]"#;

#[test]
fn pr_merge_gitlab_blocks_on_unresolved_review_threads() {
    let tempdir = make_gitlab_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let args_log = stub.tempdir.path().join("merge-args");
    let body = gitlab_merge_api_stub_with_discussions(&stub, UNRESOLVED_DISCUSSIONS_JSON);
    let stub = stub.glab_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
        ],
        Some(&repo_path),
    );
    assert_eq!(
        out.code, 65,
        "expected DATA 65 on unresolved threads, stdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "unresolved_review_threads");
    let message = env["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("--allow-unresolved-threads"),
        "message must name the bypass flag: {message}"
    );
    // The gate fired before the backend merge call.
    assert!(
        !args_log.exists(),
        "merge API must not run when the thread gate blocks"
    );
}

#[test]
fn pr_merge_gitlab_allow_unresolved_threads_bypasses_gate() {
    let tempdir = make_gitlab_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let args_log = stub.tempdir.path().join("merge-args");
    let body = gitlab_merge_api_stub_with_discussions(&stub, UNRESOLVED_DISCUSSIONS_JSON);
    let stub = stub.glab_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--allow-unresolved-threads",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["merge_sha"], "def456");
    assert!(args_log.exists(), "bypassed merge must reach the backend");
}

/// MR description with one unchecked task-list item (raw JSON string value).
const UNCHECKED_TASKS_DESCRIPTION_JSON: &str =
    r###""## Test plan\n\n- [x] unit tests\n- [ ] run e2e suite""###;

#[test]
fn pr_merge_gitlab_blocks_on_unchecked_task_items() {
    let tempdir = make_gitlab_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let args_log = stub.tempdir.path().join("merge-args");
    let body = gitlab_merge_api_stub_full(&stub, "[]", UNCHECKED_TASKS_DESCRIPTION_JSON);
    let stub = stub.glab_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
        ],
        Some(&repo_path),
    );
    assert_eq!(
        out.code, 65,
        "expected DATA 65 on unchecked task items, stdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "unchecked_task_items");
    let message = env["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("--allow-unchecked-tasks"),
        "message must name the bypass flag: {message}"
    );
    let detail = env["error"]["details"]["detail"]
        .as_str()
        .unwrap_or_default();
    assert!(
        detail.contains("run e2e suite"),
        "detail must list the unchecked item: {detail}"
    );
    // The gate fired before the backend merge call.
    assert!(
        !args_log.exists(),
        "merge API must not run when the task-list gate blocks"
    );
}

#[test]
fn pr_merge_gitlab_allow_unchecked_tasks_bypasses_gate_and_records_reason() {
    let tempdir = make_gitlab_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let args_log = stub.tempdir.path().join("merge-args");
    let body = gitlab_merge_api_stub_full(&stub, "[]", UNCHECKED_TASKS_DESCRIPTION_JSON);
    let stub = stub.glab_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--allow-unchecked-tasks",
            "--allow-unchecked-tasks-reason",
            "e2e deferred to follow-up #814",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["merge_sha"], "def456");
    assert_eq!(
        env["data"]["unchecked_tasks_override_reason"],
        "e2e deferred to follow-up #814"
    );
    assert!(args_log.exists(), "bypassed merge must reach the backend");
}

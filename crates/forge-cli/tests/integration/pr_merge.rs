//! `pr merge` integration tests covering the dry-run plan envelope and the
//! configuration-driven `keep_branch_conflict` rule. The wider lock-down chain
//! (rules 4 / 6 / 7 / 8 / 9) is unit-tested in
//! `crates/forge-cli/src/ops/pr_merge.rs` and the dedicated gate suite at
//! `tests/integration/required_check_gate.rs`; this module pins the CLI surface
//! and the per-repo `.forge-cli.toml` precedence end-to-end.

use std::collections::BTreeMap;
use std::fs;
use std::process::Command;

use forge_cli::ops::review_state::{
    ReviewFindingStatus, ReviewLoopBudget, ReviewLoopFinding, ReviewLoopState, ReviewStatePayload,
    ReviewStateRecord,
};
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
    github_merge_stub_with_ledger(
        stub,
        review_nodes,
        thread_nodes,
        reject_reviews,
        Some(review_loop_marker(false)),
    )
}

fn review_loop_marker(with_open_finding: bool) -> String {
    let mut findings = BTreeMap::new();
    if with_open_finding {
        findings.insert(
            "correctness:review-loop:merge-gate".to_string(),
            ReviewLoopFinding {
                root_cause_fingerprint: None,
                status: ReviewFindingStatus::Open,
                blocking: true,
                first_seen_head: "head123".to_string(),
                last_seen_head: "head123".to_string(),
                seen_count: 1,
                reopen_count: 0,
                threads: Vec::new(),
            },
        );
    }
    ReviewStateRecord::new(
        "acme/widgets",
        7,
        "head123",
        0,
        None,
        ReviewStatePayload::ReviewLoop {
            state: ReviewLoopState {
                head_sha: "head123".to_string(),
                round: 0,
                no_progress_rounds: 0,
                budget: ReviewLoopBudget::default(),
                findings,
                extensions: Vec::new(),
                hard_stop: None,
            },
        },
    )
    .expect("review-loop record")
    .marker()
    .expect("review-loop marker")
}

/// One passing required check: enough for rule 8 to pass on its own merits, so
/// tests aimed at the other gates are not silently riding on an empty snapshot.
const ONE_REQUIRED_PASS_CHECKS: &str =
    r#"[{"name":"ci","bucket":"pass","state":"COMPLETED","isRequired":true}]"#;

/// What the provider reports for a head with no checks at all.
const NO_CHECKS: &str = "[]";

fn github_merge_stub_with_ledger(
    stub: &StubEnv,
    review_nodes: &str,
    thread_nodes: &str,
    reject_reviews: bool,
    ledger_marker: Option<String>,
) -> String {
    github_merge_stub_with_checks(
        stub,
        review_nodes,
        thread_nodes,
        reject_reviews,
        ledger_marker,
        ONE_REQUIRED_PASS_CHECKS,
    )
}

fn github_merge_stub_with_checks(
    stub: &StubEnv,
    review_nodes: &str,
    thread_nodes: &str,
    reject_reviews: bool,
    ledger_marker: Option<String>,
    checks_json: &str,
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
    let state_nodes = ledger_marker.map_or_else(String::new, |body| {
        serde_json::json!({
            "author": {"login": "maintainer"},
            "authorAssociation": "MEMBER",
            "body": body,
            "createdAt": "2026-07-20T12:00:00Z"
        })
        .to_string()
    });
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
  "pr checks") printf '%s\n' '{checks_json}' ;;
  "api graphql")
    case "$*" in
      *"authorAssociation body createdAt"*) printf '%s\n' '{{"data":{{"viewer":{{"login":"maintainer"}},"repository":{{"pullRequest":{{"comments":{{"nodes":[{state_nodes}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}' ;;
      *"reviews(first:"*) {review_query} ;;
      *"reviewThreads(first: 100"*) printf '%s\n' '{{"data":{{"repository":{{"pullRequest":{{"headRefOid":"head123","reviewThreads":{{"nodes":[{thread_nodes}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}}}}}' ;;
      *) echo "unexpected graphql args: $*" >&2; exit 99 ;;
    esac
    ;;
  "pr merge") touch {merged} ;;
  *) echo "unexpected gh args: $*" >&2; exit 99 ;;
esac
"#,
        review_query = review_query,
        thread_nodes = thread_nodes,
        state_nodes = state_nodes,
        merged = merged.display(),
        checks_json = checks_json,
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
            "--host",
            "gitlab.example.com",
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
            "--host",
            "gitlab.example.com",
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
fn pr_merge_enforced_review_requires_explicit_genesis_ledger() {
    let config = r#"[review_convergence]
require = true
quiet_period = "0s"
timeout = "20m"
"#;
    let tempdir = make_github_repo(Some(config));
    let repo_path = tempdir.path().join("repo");
    let stub = StubEnv::new();
    let body = github_merge_stub_with_ledger(&stub, "", "", false, None);
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
    assert_eq!(env["error"]["code"], "review_state_conflict");
    assert!(!stub.tempdir.path().join("github-merged").exists());
}

#[test]
fn pr_merge_convergence_override_cannot_bypass_an_existing_open_ledger() {
    let config = r#"[review_convergence]
require = true
quiet_period = "0s"
timeout = "20m"
"#;
    let tempdir = make_github_repo(Some(config));
    let repo_path = tempdir.path().join("repo");
    let stub = StubEnv::new();
    let body = github_merge_stub_with_ledger(&stub, "", "", true, Some(review_loop_marker(true)));
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

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "review_findings_open");
    assert!(!stub.tempdir.path().join("github-merged").exists());
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
fn pr_merge_restarts_when_semantic_activity_disappears_before_merge() {
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
        r#"{"data":{"repository":{"pullRequest":{"headRefOid":"head123","reviews":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}"#,
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
    assert!(!stub.tempdir.path().join("github-merged").exists());
}

#[test]
fn pr_merge_review_convergence_keeps_unresolved_thread_gate_authoritative() {
    let tempdir = make_github_repo(None);
    let repo_path = tempdir.path().join("repo");
    let thread = r#"{"id":"PRRT_1","isResolved":false,"isOutdated":false,"path":"src/lib.rs","diffSide":"RIGHT","line":10,"originalLine":10,"originalStartLine":null,"startDiffSide":null,"startLine":null,"subjectType":"LINE","comments":{"nodes":[{"id":"PRRC_1","author":{"login":"reviewer"},"body":"please address","createdAt":"2026-07-14T04:00:00Z","url":"https://github.com/acme/widgets/pull/7#discussion_r1"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}"#;
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

#[test]
fn pr_merge_github_outdated_thread_is_dispositioned_stale_and_merges() {
    // An unresolved thread whose anchored diff hunk is outdated must be
    // mechanically dispositioned `stale` (recorded, not silently dropped) and
    // must not block the merge. This breaks the "outdated threads accumulate
    // and block forever" loop (nils-cli#1272).
    let tempdir = make_github_repo(None);
    let repo_path = tempdir.path().join("repo");
    let thread = r#"{"id":"PRRT_stale","isResolved":false,"isOutdated":true,"path":"src/lib.rs","diffSide":"RIGHT","line":10,"originalLine":10,"originalStartLine":null,"startDiffSide":null,"startLine":null,"subjectType":"LINE","comments":{"nodes":[{"id":"PRRC_9","author":{"login":"quality-bot"},"body":"nit: rename this local","createdAt":"2026-07-14T04:00:00Z","url":"https://github.com/acme/widgets/pull/7#discussion_r9"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}"#;
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
        ],
        Some(&repo_path),
    );
    assert_eq!(
        out.code, 0,
        "outdated-only threads must not block; stdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["merge_sha"], "merge456");
    let dispositions = env["data"]["stale_thread_dispositions"]
        .as_array()
        .expect("stale_thread_dispositions must be recorded");
    assert_eq!(
        dispositions.len(),
        1,
        "the outdated thread must be recorded as dispositioned stale"
    );
    assert_eq!(dispositions[0]["thread_id"], "PRRT_stale");
    assert_eq!(dispositions[0]["disposition"], "stale");
    assert_eq!(dispositions[0]["author"], "quality-bot");
    assert_eq!(dispositions[0]["path"], "src/lib.rs");
    assert_eq!(dispositions[0]["summary"], "nit: rename this local");
    assert_eq!(
        dispositions[0]["rationale"],
        "the anchored diff hunk is outdated; the referenced code changed"
    );
    assert!(
        stub.tempdir.path().join("github-merged").exists(),
        "merge must reach the backend once outdated threads are dispositioned"
    );
}

#[test]
fn pr_merge_github_mixed_outdated_and_live_thread_still_blocks() {
    // A non-outdated unresolved thread must still block even when an outdated
    // thread is present: stale disposition must not weaken the live gate.
    let tempdir = make_github_repo(None);
    let repo_path = tempdir.path().join("repo");
    let outdated = r#"{"id":"PRRT_stale","isResolved":false,"isOutdated":true,"path":"src/lib.rs","diffSide":"RIGHT","line":10,"originalLine":10,"originalStartLine":null,"startDiffSide":null,"startLine":null,"subjectType":"LINE","comments":{"nodes":[{"id":"PRRC_9","author":{"login":"quality-bot"},"body":"nit: moved code","createdAt":"2026-07-14T04:00:00Z","url":"https://github.com/acme/widgets/pull/7#discussion_r9"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}"#;
    let live = r#"{"id":"PRRT_live","isResolved":false,"isOutdated":false,"path":"src/main.rs","diffSide":"RIGHT","line":20,"originalLine":20,"originalStartLine":null,"startDiffSide":null,"startLine":null,"subjectType":"LINE","comments":{"nodes":[{"id":"PRRC_10","author":{"login":"reviewer"},"body":"please address this","createdAt":"2026-07-14T05:00:00Z","url":"https://github.com/acme/widgets/pull/7#discussion_r10"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}"#;
    let thread_nodes = format!("{outdated},{live}");
    let stub = StubEnv::new();
    let body = github_merge_stub(&stub, "", &thread_nodes, false);
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
    assert_eq!(
        out.code, 65,
        "a live unresolved thread must still block; stdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "unresolved_review_threads");
    let detail = env["error"]["details"]["detail"]
        .as_str()
        .unwrap_or_default();
    assert!(
        detail.contains("reviewer") && !detail.contains("quality-bot"),
        "only the live (non-outdated) thread should be listed as blocking: {detail}"
    );
    assert!(
        !stub.tempdir.path().join("github-merged").exists(),
        "the gate must fire before the backend merge call"
    );
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
            "--host",
            "gitlab.example.com",
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
            "--host",
            "gitlab.example.com",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--allow-unresolved-threads",
            "--allow-unresolved-threads-reason",
            "outdated bot threads",
        ],
        Some(&repo_path),
    );
    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["merge_sha"], "def456");
    assert_eq!(
        env["data"]["unresolved_threads_override_reason"], "outdated bot threads",
        "the recorded bypass reason must be present in the merge payload"
    );
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
            "--host",
            "gitlab.example.com",
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
            "--host",
            "gitlab.example.com",
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

/// Rule 8's absence half, end to end: the provider reports no checks for the
/// head, and the merge is refused rather than passing on a vacuous "all
/// required checks are green".
#[test]
fn pr_merge_github_refuses_a_head_with_no_checks_at_all() {
    let tempdir = make_github_repo(None);
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let merged = stub.tempdir.path().join("github-merged");
    let body = github_merge_stub_with_checks(&stub, "", "", true, None, NO_CHECKS);
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
        ],
        Some(&repo_path),
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "checks_not_registered");
    assert!(
        !merged.exists(),
        "an unchecked head must never reach the provider merge"
    );
}

/// The declared opt-out, and the only durable record that a merge happened
/// without CI evidence.
#[test]
fn pr_merge_github_allow_no_checks_bypasses_rule_eight_and_records_the_reason() {
    let tempdir = make_github_repo(None);
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let merged = stub.tempdir.path().join("github-merged");
    let body = github_merge_stub_with_checks(&stub, "", "", true, None, NO_CHECKS);
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
            "--review-convergence=false",
            "--allow-no-checks",
            "--allow-no-checks-reason",
            "this repository configures no CI",
        ],
        Some(&repo_path),
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(
        env["data"]["no_checks_override_reason"],
        "this repository configures no CI"
    );
    assert!(merged.exists(), "the bypassed merge must reach the backend");
}

/// The field is absent — not null, not empty — when the bypass was not used,
/// so its presence is itself the audit signal.
#[test]
fn pr_merge_omits_the_no_checks_reason_when_the_bypass_was_not_used() {
    let tempdir = make_github_repo(None);
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
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
        ],
        Some(&repo_path),
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert!(
        env["data"].get("no_checks_override_reason").is_none(),
        "an ordinary merge must not carry an override record: {}",
        out.stdout
    );
}

/// `--allow-no-checks` cannot be used without stating why, matching the two
/// sibling bypasses.
#[test]
fn pr_merge_allow_no_checks_requires_a_reason() {
    let tempdir = make_github_repo(None);
    let repo_path = tempdir.path().join("repo");
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
            "7",
            "--allow-no-checks",
        ],
        Some(&repo_path),
    );

    assert_eq!(out.code, 64, "stdout={}\nstderr={}", out.stdout, out.stderr);
}

/// Rule 8 is provider-neutral, and on GitLab that is a behaviour change worth
/// pinning: an MR whose head has no pipeline — no `.gitlab-ci.yml`, excluded by
/// `workflow:rules`, or pipelines disabled on a fork — reports the same empty
/// snapshot as an unregistered GitHub head, and is refused for the same reason.
/// The deliver-side visible-row fallback is GitHub-only, so there is no
/// mitigation here; `--allow-no-checks` is the declared way through.
#[test]
fn pr_merge_gitlab_refuses_an_mr_whose_head_has_no_pipeline() {
    let tempdir = make_gitlab_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let sentinel = stub.tempdir.path().join("merged");
    let body = gitlab_merge_api_stub_full(&stub, "[]", "null").replace(
        r#""head_pipeline": { "id": 99, "status": "success", "web_url": "https://gitlab.example.com/group/project/-/pipelines/99" }"#,
        r#""head_pipeline": null"#,
    );
    let stub = stub.glab_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--host",
            "gitlab.example.com",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--review-convergence=false",
        ],
        Some(&repo_path),
    );

    assert_eq!(out.code, 65, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["error"]["code"], "checks_not_registered");
    assert!(!sentinel.exists(), "the merge must not reach the backend");
}

/// …and the opt-out works there too, which is what makes the refusal
/// acceptable for projects that genuinely have no CI.
#[test]
fn pr_merge_gitlab_allow_no_checks_merges_an_mr_with_no_pipeline() {
    let tempdir = make_gitlab_repo();
    let repo_path = tempdir.path().join("repo");

    let stub = StubEnv::new();
    let sentinel = stub.tempdir.path().join("merged");
    let body = gitlab_merge_api_stub_full(&stub, "[]", "null").replace(
        r#""head_pipeline": { "id": 99, "status": "success", "web_url": "https://gitlab.example.com/group/project/-/pipelines/99" }"#,
        r#""head_pipeline": null"#,
    );
    let stub = stub.glab_stub(&body);

    let out = run_forge_cli_in(
        &stub,
        &[
            "--provider",
            "gitlab",
            "--host",
            "gitlab.example.com",
            "--format",
            "json",
            "pr",
            "merge",
            "7",
            "--review-convergence=false",
            "--allow-no-checks",
            "--allow-no-checks-reason",
            "this project has no .gitlab-ci.yml",
        ],
        Some(&repo_path),
    );

    assert_eq!(out.code, 0, "stdout={}\nstderr={}", out.stdout, out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(
        env["data"]["no_checks_override_reason"],
        "this project has no .gitlab-ci.yml"
    );
    assert!(
        sentinel.exists(),
        "the bypassed merge must reach the backend"
    );
}

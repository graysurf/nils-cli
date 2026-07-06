//! End-to-end `pr checks` integration tests against a stubbed `gh`.
//!
//! Each test wires `FORGE_CLI_GH_BIN` at a generated shell stub that echoes
//! one of the canonical JSON fixtures under
//! `tests/fixtures/github/pr_checks/`. The five shapes — all-success,
//! mixed-failure, all-pending, cancelled, empty — pin the aggregate state /
//! required-only behaviour required by spec §"pr checks".

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

const FIXTURE_ALL_SUCCESS: &str = include_str!("../fixtures/github/pr_checks/all_success.json");
const FIXTURE_MIXED_FAILURE: &str = include_str!("../fixtures/github/pr_checks/mixed_failure.json");
const FIXTURE_MIXED_FAILURE_REQUIRED: &str =
    include_str!("../fixtures/github/pr_checks/mixed_failure_required.json");
const FIXTURE_ALL_PENDING: &str = include_str!("../fixtures/github/pr_checks/all_pending.json");
const FIXTURE_CANCELLED: &str = include_str!("../fixtures/github/pr_checks/cancelled.json");
const FIXTURE_EMPTY: &str = include_str!("../fixtures/github/pr_checks/empty.json");

fn gh_dispatch_stub(all_checks_json: &str, required_checks_json: &str) -> String {
    format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "pr checks")
    case " $* " in
      *" --required "*)
        cat <<'EOF'
{required_json}
EOF
        ;;
      *)
        cat <<'EOF'
{all_json}
EOF
        ;;
    esac
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
        all_json = all_checks_json,
        required_json = required_checks_json,
    )
}

fn gh_status_rollup_fallback_stub() -> String {
    r#"#!/bin/sh
set -e
case "$1 $2" in
  "pr checks")
    echo "GraphQL: Resource not accessible by integration (node.statusCheckRollup.nodes.0.commit.statusCheckRollup)" >&2
    exit 1
    ;;
  "pr view")
    cat <<'EOF'
{
  "headRefOid": "abc123",
  "statusCheckRollup": [
    {
      "__typename": "CheckRun",
      "name": "test",
      "status": "COMPLETED",
      "conclusion": "SUCCESS",
      "detailsUrl": "https://github.com/sympoies/nils-cli/actions/runs/1/job/2",
      "workflowName": "CI",
      "startedAt": "2026-07-06T10:00:00Z",
      "completedAt": "2026-07-06T10:05:00Z"
    },
    {
      "__typename": "StatusContext",
      "context": "coverage",
      "state": "SUCCESS",
      "targetUrl": "https://github.com/sympoies/nils-cli/runs/3"
    }
  ]
}
EOF
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    .to_string()
}

fn gh_status_rollup_fallback_stub_with_rollup(rollup_json: &str) -> String {
    format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "pr checks")
    echo "GraphQL: Resource not accessible by integration (node.statusCheckRollup.nodes.0.commit.statusCheckRollup)" >&2
    exit 1
    ;;
  "pr view")
    cat <<'EOF'
{{
  "headRefOid": "abc123",
  "statusCheckRollup": {rollup_json}
}}
EOF
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#
    )
}

fn run_checks(
    all_checks_json: &str,
    required_checks_json: &str,
    extra_args: &[&str],
) -> (i32, serde_json::Value) {
    let stub = StubEnv::new().gh_stub(&gh_dispatch_stub(all_checks_json, required_checks_json));
    let mut argv = vec![
        "--provider",
        "github",
        "--format",
        "json",
        "pr",
        "checks",
        "1",
    ];
    argv.extend_from_slice(extra_args);
    let out = run_forge_cli(&stub, &argv);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    (out.code, env)
}

#[test]
fn pr_checks_falls_back_to_status_rollup_view_on_permission_error() {
    let stub = StubEnv::new().gh_stub(&gh_status_rollup_fallback_stub());
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "sympoies/nils-cli",
            "--format",
            "json",
            "pr",
            "checks",
            "7",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.checks.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["state"], "success");
    assert_eq!(env["data"]["required_count"], 2);
    assert_eq!(env["data"]["success_count"], 2);
    assert_eq!(
        env["data"]["warnings"][0],
        "github_status_rollup_requiredness_unknown_all_rows_gated"
    );
    let checks = env["data"]["checks"].as_array().expect("checks array");
    assert_eq!(checks.len(), 2);
    assert_eq!(checks[0]["name"], "test");
    assert_eq!(
        checks[0]["url"],
        "https://github.com/sympoies/nils-cli/actions/runs/1/job/2"
    );
    assert_eq!(checks[0]["workflow"], "CI");
    assert_eq!(checks[1]["name"], "coverage");
    assert_eq!(
        checks[1]["url"],
        "https://github.com/sympoies/nils-cli/runs/3"
    );
}

#[test]
fn pr_checks_rollup_completed_check_run_without_conclusion_is_pending() {
    let stub = StubEnv::new().gh_stub(&gh_status_rollup_fallback_stub_with_rollup(
        r#"[
    {
      "__typename": "CheckRun",
      "name": "test",
      "status": "COMPLETED",
      "detailsUrl": "https://github.com/sympoies/nils-cli/actions/runs/1/job/2"
    }
  ]"#,
    ));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "sympoies/nils-cli",
            "--format",
            "json",
            "pr",
            "checks",
            "7",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["state"], "pending");
    assert_eq!(env["data"]["required_count"], 1);
    assert_eq!(env["data"]["success_count"], 0);
    assert_eq!(env["data"]["pending"][0]["name"], "test");
    assert_eq!(env["data"]["checks"][0]["state"], "pending");
}

#[test]
fn pr_checks_rollup_non_success_conclusions_fail_or_cancel() {
    let stub = StubEnv::new().gh_stub(&gh_status_rollup_fallback_stub_with_rollup(
        r#"[
    {
      "__typename": "CheckRun",
      "name": "test",
      "status": "COMPLETED",
      "conclusion": "FAILURE"
    },
    {
      "__typename": "CheckRun",
      "name": "timeout",
      "status": "COMPLETED",
      "conclusion": "TIMED_OUT"
    },
    {
      "__typename": "CheckRun",
      "name": "cancelled",
      "status": "COMPLETED",
      "conclusion": "CANCELLED"
    }
  ]"#,
    ));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "sympoies/nils-cli",
            "--format",
            "json",
            "pr",
            "checks",
            "7",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["state"], "failure");
    let failed = env["data"]["failed"].as_array().expect("failed");
    assert_eq!(failed.len(), 3);
    assert_eq!(env["data"]["checks"][0]["state"], "failure");
    assert_eq!(env["data"]["checks"][1]["state"], "timed_out");
    assert_eq!(env["data"]["checks"][2]["state"], "cancelled");
}

#[test]
fn pr_checks_rollup_required_only_false_gates_all_rows_without_warning() {
    let stub = StubEnv::new().gh_stub(&gh_status_rollup_fallback_stub_with_rollup(
        r#"[
    {
      "__typename": "CheckRun",
      "name": "optional-flaky",
      "status": "COMPLETED",
      "conclusion": "FAILURE"
    }
  ]"#,
    ));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--repo",
            "sympoies/nils-cli",
            "--format",
            "json",
            "pr",
            "checks",
            "7",
            "--required-only",
            "false",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["state"], "failure");
    assert_eq!(env["data"]["required_count"], 1);
    assert!(env["data"].get("warnings").is_none());
}

#[test]
fn pr_checks_all_success_emits_success_state() {
    let (_, env) = run_checks(FIXTURE_ALL_SUCCESS, FIXTURE_ALL_SUCCESS, &[]);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.checks.v1");
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["provider"], "github");
    assert_eq!(env["data"]["state"], "success");
    assert_eq!(env["data"]["required_count"], 3);
    assert_eq!(env["data"]["success_count"], 3);
    assert!(env["data"]["failed"].as_array().unwrap().is_empty());
    assert!(env["data"]["pending"].as_array().unwrap().is_empty());
    assert_eq!(env["data"]["checks"].as_array().unwrap().len(), 3);
}

#[test]
fn pr_checks_mixed_failure_marks_required_failed() {
    let (_, env) = run_checks(FIXTURE_MIXED_FAILURE, FIXTURE_MIXED_FAILURE_REQUIRED, &[]);
    assert_eq!(env["data"]["state"], "failure");
    assert_eq!(env["data"]["required_count"], 2);
    assert_eq!(env["data"]["success_count"], 1);
    let failed = env["data"]["failed"].as_array().expect("failed array");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["name"], "test");
    assert!(failed[0]["conclusion"].is_null());
    // Non-required pending check stays in data.checks but not in gating.
    let checks = env["data"]["checks"].as_array().expect("checks array");
    assert_eq!(checks.len(), 3);
    assert!(checks.iter().any(|c| c["name"] == "optional-flaky"));
}

#[test]
fn pr_checks_mixed_failure_required_only_false_includes_optional_in_gating() {
    let (_, env) = run_checks(
        FIXTURE_MIXED_FAILURE,
        FIXTURE_MIXED_FAILURE_REQUIRED,
        &["--required-only", "false"],
    );
    // Required-only=false: the optional pending check is now part of the
    // gating set, so required_count grows to 3 and pending becomes part of
    // the aggregate.
    assert_eq!(env["data"]["required_count"], 3);
    // State remains failure (failure dominates pending).
    assert_eq!(env["data"]["state"], "failure");
    let pending = env["data"]["pending"].as_array().expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["name"], "optional-flaky");
}

#[test]
fn pr_checks_all_pending_reports_pending_state() {
    let (_, env) = run_checks(FIXTURE_ALL_PENDING, FIXTURE_ALL_PENDING, &[]);
    assert_eq!(env["data"]["state"], "pending");
    assert_eq!(env["data"]["required_count"], 2);
    assert_eq!(env["data"]["success_count"], 0);
    assert_eq!(env["data"]["pending"].as_array().unwrap().len(), 2);
    assert!(env["data"]["failed"].as_array().unwrap().is_empty());
}

#[test]
fn pr_checks_cancelled_marks_failure_lane() {
    let (_, env) = run_checks(FIXTURE_CANCELLED, FIXTURE_CANCELLED, &[]);
    // Cancelled is a failure-class terminal state.
    assert_eq!(env["data"]["state"], "cancelled");
    let failed = env["data"]["failed"].as_array().expect("failed");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["name"], "test");
    assert!(failed[0]["conclusion"].is_null());
}

#[test]
fn pr_checks_empty_array_is_success_zero_count() {
    let (_, env) = run_checks(FIXTURE_EMPTY, FIXTURE_EMPTY, &[]);
    assert_eq!(env["data"]["state"], "success");
    assert_eq!(env["data"]["required_count"], 0);
    assert_eq!(env["data"]["success_count"], 0);
    assert!(env["data"]["checks"].as_array().unwrap().is_empty());
}

#[test]
fn pr_checks_no_required_checks_backend_message_is_success_zero_required() {
    let stub = StubEnv::new().gh_stub(&format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "pr checks")
    case " $* " in
      *" --required "*)
        echo "no required checks reported on the 'feat/example' branch" >&2
        exit 1
        ;;
      *)
        cat <<'EOF'
{all_json}
EOF
        ;;
    esac
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
        all_json = FIXTURE_ALL_SUCCESS,
    ));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "checks",
            "1",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["state"], "success");
    assert_eq!(env["data"]["required_count"], 0);
    assert_eq!(env["data"]["success_count"], 0);
    assert_eq!(env["data"]["checks"].as_array().unwrap().len(), 3);
}

#[test]
fn pr_checks_nonzero_pending_stdout_is_still_parsed() {
    let stub = StubEnv::new().gh_stub(&format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "pr checks")
    case " $* " in
      *" --required "*)
        echo "no required checks reported on the 'feat/example' branch" >&2
        exit 1
        ;;
      *)
        cat <<'EOF'
{all_json}
EOF
        exit 8
        ;;
    esac
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
        all_json = FIXTURE_ALL_PENDING,
    ));
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--format",
            "json",
            "pr",
            "checks",
            "1",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["data"]["required_count"], 0);
    assert_eq!(env["data"]["success_count"], 0);
    assert_eq!(env["data"]["checks"].as_array().unwrap().len(), 2);
}

#[test]
fn pr_checks_dry_run_renders_plan_with_json_fields() {
    // Dry-run does not invoke the backend; the stub should never fire.
    let stub = StubEnv::new().gh_stub("#!/bin/sh\necho 'should not run' >&2\nexit 77\n");
    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            "github",
            "--dry-run",
            "--format",
            "json",
            "pr",
            "checks",
            "42",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let env = parse_envelope(&out.stdout);
    assert_eq!(env["schema_version"], "cli.forge-cli.pr.checks.v1");
    let plan: Vec<String> = env["data"]["plan"]
        .as_array()
        .expect("plan array")
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();
    assert!(plan.contains(&"checks".to_string()), "{plan:?}");
    assert!(plan.contains(&"42".to_string()), "{plan:?}");
    assert!(plan.contains(&"--required".to_string()), "{plan:?}");
    let json_idx = plan
        .iter()
        .position(|s| s == "--json")
        .expect("--json present");
    assert!(plan[json_idx + 1].contains("bucket"), "{plan:?}");
    assert!(!plan[json_idx + 1].contains("isRequired"), "{plan:?}");
    assert!(!plan[json_idx + 1].contains("conclusion"), "{plan:?}");
}

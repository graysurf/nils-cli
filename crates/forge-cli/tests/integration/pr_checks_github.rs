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
const FIXTURE_ALL_PENDING: &str = include_str!("../fixtures/github/pr_checks/all_pending.json");
const FIXTURE_CANCELLED: &str = include_str!("../fixtures/github/pr_checks/cancelled.json");
const FIXTURE_EMPTY: &str = include_str!("../fixtures/github/pr_checks/empty.json");

fn gh_dispatch_stub(checks_json: &str) -> String {
    format!(
        r#"#!/bin/sh
set -e
case "$1 $2" in
  "pr checks")
    cat <<'EOF'
{json}
EOF
    ;;
  *)
    echo "stub: unexpected gh args: $*" >&2
    exit 99
    ;;
esac
"#,
        json = checks_json,
    )
}

fn run_checks(checks_json: &str, extra_args: &[&str]) -> (i32, serde_json::Value) {
    let stub = StubEnv::new().gh_stub(&gh_dispatch_stub(checks_json));
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
fn pr_checks_all_success_emits_success_state() {
    let (_, env) = run_checks(FIXTURE_ALL_SUCCESS, &[]);
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
    let (_, env) = run_checks(FIXTURE_MIXED_FAILURE, &[]);
    assert_eq!(env["data"]["state"], "failure");
    assert_eq!(env["data"]["required_count"], 2);
    assert_eq!(env["data"]["success_count"], 1);
    let failed = env["data"]["failed"].as_array().expect("failed array");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["name"], "test");
    assert_eq!(failed[0]["conclusion"], "failure");
    // Non-required pending check stays in data.checks but not in gating.
    let checks = env["data"]["checks"].as_array().expect("checks array");
    assert_eq!(checks.len(), 3);
    assert!(checks.iter().any(|c| c["name"] == "optional-flaky"));
}

#[test]
fn pr_checks_mixed_failure_required_only_false_includes_optional_in_gating() {
    let (_, env) = run_checks(FIXTURE_MIXED_FAILURE, &["--required-only", "false"]);
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
    let (_, env) = run_checks(FIXTURE_ALL_PENDING, &[]);
    assert_eq!(env["data"]["state"], "pending");
    assert_eq!(env["data"]["required_count"], 2);
    assert_eq!(env["data"]["success_count"], 0);
    assert_eq!(env["data"]["pending"].as_array().unwrap().len(), 2);
    assert!(env["data"]["failed"].as_array().unwrap().is_empty());
}

#[test]
fn pr_checks_cancelled_marks_failure_lane() {
    let (_, env) = run_checks(FIXTURE_CANCELLED, &[]);
    // Cancelled is a failure-class terminal state.
    assert_eq!(env["data"]["state"], "cancelled");
    let failed = env["data"]["failed"].as_array().expect("failed");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["name"], "test");
    assert_eq!(failed[0]["conclusion"], "cancelled");
}

#[test]
fn pr_checks_empty_array_is_success_zero_count() {
    let (_, env) = run_checks(FIXTURE_EMPTY, &[]);
    assert_eq!(env["data"]["state"], "success");
    assert_eq!(env["data"]["required_count"], 0);
    assert_eq!(env["data"]["success_count"], 0);
    assert!(env["data"]["checks"].as_array().unwrap().is_empty());
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
    let json_idx = plan
        .iter()
        .position(|s| s == "--json")
        .expect("--json present");
    assert!(plan[json_idx + 1].contains("isRequired"), "{plan:?}");
}

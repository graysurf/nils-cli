//! End-to-end `pr checks` integration tests against a stubbed `glab`.
//!
//! Each test wires `FORGE_CLI_GLAB_BIN` at a dispatching shell stub that
//! mocks `glab --version` (version probe) and `glab ci status -b <branch>`
//! (text status). Numeric ids resolve through `glab mr view <id> -F json`.
//! Out-of-range versions short-circuit with `UNAVAILABLE 69` and
//! `error.kind = "glab_version_unsupported"` per spec.

use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

const VERSION_OK: &str = include_str!("../fixtures/gitlab/pr_checks/version_supported.txt");
const VERSION_TOO_NEW: &str = include_str!("../fixtures/gitlab/pr_checks/version_too_new.txt");
const ALL_SUCCESS: &str = include_str!("../fixtures/gitlab/pr_checks/all_success.txt");
const ONE_FAILURE: &str = include_str!("../fixtures/gitlab/pr_checks/one_failure.txt");
const MIXED_STATES: &str = include_str!("../fixtures/gitlab/pr_checks/mixed_states.txt");
const PENDING_ONLY: &str = include_str!("../fixtures/gitlab/pr_checks/pending_only.txt");
const EMPTY_PIPELINE: &str = include_str!("../fixtures/gitlab/pr_checks/empty_pipeline.txt");
const MANUAL_ONLY: &str = include_str!("../fixtures/gitlab/pr_checks/manual_only.txt");

fn glab_stub(version: &str, ci_status: &str) -> String {
    format!(
        r#"#!/bin/sh
set -e
case "$1" in
  "--version")
    cat <<'EOF'
{version}
EOF
    ;;
  "ci")
    if [ "$2" = "status" ]; then
      cat <<'EOF'
{ci_status}
EOF
    else
      echo "stub: unknown ci subcommand: $*" >&2
      exit 99
    fi
    ;;
  "mr")
    # Numeric id resolution → return a JSON record with source_branch=feat/sample.
    if [ "$2" = "view" ]; then
      cat <<'EOF'
{{
  "iid": 1,
  "source_branch": "feat/sample"
}}
EOF
    else
      echo "stub: unknown mr subcommand: $*" >&2
      exit 99
    fi
    ;;
  *)
    echo "stub: unexpected glab args: $*" >&2
    exit 99
    ;;
esac
"#,
        version = version,
        ci_status = ci_status,
    )
}

fn run_checks(
    version: &str,
    ci_status: &str,
    id: &str,
    extra: &[&str],
) -> (i32, serde_json::Value) {
    let stub = StubEnv::new().glab_stub(&glab_stub(version, ci_status));
    let mut argv = vec![
        "--provider",
        "gitlab",
        "--format",
        "json",
        "pr",
        "checks",
        id,
    ];
    argv.extend_from_slice(extra);
    let out = run_forge_cli(&stub, &argv);
    let env = parse_envelope(&out.stdout);
    (out.code, env)
}

#[test]
fn pr_checks_glab_all_success_emits_success_state() {
    let (code, env) = run_checks(VERSION_OK, ALL_SUCCESS, "feat/sample", &[]);
    assert_eq!(code, 0);
    assert_eq!(env["data"]["provider"], "gitlab");
    assert_eq!(env["data"]["state"], "success");
    assert_eq!(env["data"]["required_count"], 3);
    assert_eq!(env["data"]["success_count"], 3);
}

#[test]
fn pr_checks_glab_one_failure_marks_failure_state() {
    let (code, env) = run_checks(VERSION_OK, ONE_FAILURE, "feat/sample", &[]);
    assert_eq!(code, 0); // pr checks always exits 0 (snapshot).
    assert_eq!(env["data"]["state"], "failure");
    assert_eq!(env["data"]["required_count"], 3);
    let failed = env["data"]["failed"].as_array().expect("failed");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["name"], "test");
}

#[test]
fn pr_checks_glab_mixed_states_promotes_failure() {
    let (_, env) = run_checks(VERSION_OK, MIXED_STATES, "feat/sample", &[]);
    assert_eq!(env["data"]["state"], "failure");
    assert_eq!(env["data"]["required_count"], 4);
    // success_count counts only success-classed entries (skipped/neutral
    // count required but not success).
    assert_eq!(env["data"]["success_count"], 1);
}

#[test]
fn pr_checks_glab_pending_only_reports_pending() {
    let (_, env) = run_checks(VERSION_OK, PENDING_ONLY, "feat/sample", &[]);
    assert_eq!(env["data"]["state"], "pending");
    assert_eq!(env["data"]["pending"].as_array().unwrap().len(), 3);
}

#[test]
fn pr_checks_glab_empty_pipeline_is_success_zero_count() {
    let (_, env) = run_checks(VERSION_OK, EMPTY_PIPELINE, "feat/sample", &[]);
    assert_eq!(env["data"]["state"], "success");
    assert_eq!(env["data"]["required_count"], 0);
    assert!(env["data"]["checks"].as_array().unwrap().is_empty());
}

#[test]
fn pr_checks_glab_manual_only_is_success_neutral() {
    let (_, env) = run_checks(VERSION_OK, MANUAL_ONLY, "feat/sample", &[]);
    // Manual maps to neutral (terminal non-failing); aggregate = success.
    assert_eq!(env["data"]["state"], "success");
    assert_eq!(env["data"]["required_count"], 2);
}

#[test]
fn pr_checks_glab_numeric_id_resolves_through_mr_view() {
    // Pass numeric id; stub returns source_branch=feat/sample so the parser
    // still gets the all_success text fixture.
    let (code, env) = run_checks(VERSION_OK, ALL_SUCCESS, "42", &[]);
    assert_eq!(code, 0);
    assert_eq!(env["data"]["state"], "success");
}

fn glab_stub_no_pipeline(version: &str) -> String {
    // Stub that mimics `glab ci status -b ...` when the repo has no pipeline
    // at all: exits non-zero with the human-readable error on stderr (matches
    // glab 1.99 behaviour).
    format!(
        r#"#!/bin/sh
case "$1" in
  "--version")
    cat <<'EOF'
{version}
EOF
    exit 0
    ;;
  "ci")
    if [ "$2" = "status" ]; then
      printf 'No pipeline found. It might not exist yet.\n' >&2
      exit 1
    fi
    ;;
esac
echo "stub: unexpected glab args: $*" >&2
exit 99
"#,
        version = version,
    )
}

#[test]
fn pr_checks_glab_no_pipeline_is_success_zero_count() {
    let stub = StubEnv::new().glab_stub(&glab_stub_no_pipeline(VERSION_OK));
    let argv = vec![
        "--provider", "gitlab", "--format", "json", "pr", "checks", "feat/sample",
    ];
    let out = run_forge_cli(&stub, &argv);
    let env = parse_envelope(&out.stdout);
    assert_eq!(out.code, 0, "no-pipeline should be success, got {env:?}");
    assert_eq!(env["data"]["state"], "success");
    assert_eq!(env["data"]["required_count"], 0);
    assert!(env["data"]["checks"].as_array().unwrap().is_empty());
}

#[test]
fn pr_checks_glab_version_too_new_fails_unavailable() {
    let (code, env) = run_checks(VERSION_TOO_NEW, ALL_SUCCESS, "feat/sample", &[]);
    assert_eq!(code, 69, "stderr={env:?}");
    assert_eq!(env["error"]["code"], "glab_version_unsupported");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("outside the supported")
    );
}

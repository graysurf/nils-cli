use pretty_assertions::assert_eq;
use serde_json::Value;

use crate::common;
#[test]
fn output_json_contract_success_envelope_contains_version_status_and_payload() {
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "build-task-spec",
        "--plan",
        "crates/plan-issue-cli/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
        "--sprint",
        "2",
        "--pr-grouping",
        "per-sprint",
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.trim().is_empty(),
        "stderr should be empty: {}",
        out.stderr
    );

    let payload: Value = serde_json::from_str(&out.stdout).expect("stdout should be JSON");
    assert!(
        payload["schema_version"]
            .as_str()
            .is_some_and(|value| value.starts_with("plan-issue-cli.")),
        "{}",
        out.stdout
    );
    assert_eq!(payload["command"], "build-task-spec");
    assert_eq!(payload["status"], "ok");
    assert!(payload["payload"].is_object(), "{}", out.stdout);
    assert_eq!(payload["payload"]["execution_mode"], "live");
    assert_eq!(payload["payload"]["arguments"]["sprint"], 2);
}

#[test]
fn output_json_contract_error_envelope_contains_code_and_message() {
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "build-task-spec",
        "--plan",
        "crates/plan-issue-cli/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
        "--sprint",
        "2",
        "--pr-grouping",
        "group",
    ]);

    assert_eq!(out.code, 1);

    let payload: Value = serde_json::from_str(&out.stdout).expect("stdout should be JSON");
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error"]["code"], "invalid-pr-grouping");
    assert!(
        payload["error"]["message"]
            .as_str()
            .is_some_and(|value| value.contains("with --strategy deterministic")),
        "{}",
        out.stdout
    );
}

#[test]
fn output_text_contract_success_output_is_deterministic() {
    let out = common::run_plan_issue(&[
        "build-plan-task-spec",
        "--plan",
        "crates/plan-issue-cli/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
        "--pr-grouping",
        "per-sprint",
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.trim().is_empty(),
        "stderr should be empty: {}",
        out.stderr
    );

    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(lines.len(), 4, "unexpected output: {}", out.stdout);
    assert_eq!(
        lines[0],
        "schema_version: plan-issue-cli.build.plan.task.spec.v1"
    );
    assert_eq!(lines[1], "command: build-plan-task-spec");
    assert_eq!(lines[2], "status: ok");
    assert!(lines[3].starts_with("payload: {"), "{}", lines[3]);
}

#[test]
fn output_text_contract_error_output_is_deterministic() {
    let out = common::run_plan_issue(&[
        "build-plan-task-spec",
        "--plan",
        "crates/plan-issue-cli/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
        "--pr-grouping",
        "group",
    ]);

    assert_eq!(out.code, 1);
    assert!(
        out.stdout.trim().is_empty(),
        "stdout should be empty: {}",
        out.stdout
    );

    let lines: Vec<&str> = out.stderr.lines().collect();
    assert_eq!(lines.len(), 5, "unexpected stderr: {}", out.stderr);
    assert_eq!(
        lines[0],
        "schema_version: plan-issue-cli.build.plan.task.spec.v1"
    );
    assert_eq!(lines[1], "command: build-plan-task-spec");
    assert_eq!(lines[2], "status: error");
    assert_eq!(lines[3], "code: invalid-pr-grouping");
    assert_eq!(
        lines[4],
        "message: --pr-grouping group with --strategy deterministic requires --pr-group mappings"
    );
}

//! `plan-issue tracking status` integration coverage (Task 4.3).
//!
//! Source: `docs/source/plan-issue-redesign/plan-tracking-issue-run-state-controller-v1.md`.

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

use plan_issue::lifecycle_record::PAYLOAD_SCHEMA_V2;

use crate::common;

fn v2_comment(role: &str, profile: &str, data: Value, visible: &str) -> String {
    let envelope = json!({
        "schema": PAYLOAD_SCHEMA_V2,
        "role": role,
        "profile": profile,
        "data": data,
    });
    let payload = serde_json::to_string(&envelope).expect("serialize");
    format!(
        "<!-- plan-issue-record:v2 role={role} profile={profile} -->\n\n{visible}\n\n```plan-issue-record-payload\n{payload}\n```\n",
    )
}

fn write_fixture(
    roles: &[(&str, Value, &str, &str)], // role, payload, visible body, created_at
) -> TempDir {
    let tmp = TempDir::new().expect("tmp");
    fs::write(tmp.path().join("body.md"), "## Current Dashboard\n").expect("body");
    let comments: Vec<Value> = roles
        .iter()
        .enumerate()
        .map(|(idx, (role, data, visible, at))| {
            json!({
                "url": format!("https://example.com/c{idx}"),
                "created_at": at,
                "body": v2_comment(role, "tracking", data.clone(), visible),
            })
        })
        .collect();
    fs::write(
        tmp.path().join("comments.json"),
        json!({"comments": comments}).to_string(),
    )
    .expect("comments");
    tmp
}

#[test]
fn tracking_status_emits_stable_envelope_for_fixture_input() {
    let fixture = write_fixture(&[
        (
            "source",
            json!({"path": "p", "commit": "c"}),
            "## Source Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:00Z",
        ),
        (
            "plan",
            json!({"path": "p", "commit": "c"}),
            "## Plan Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:01Z",
        ),
        (
            "state",
            json!({"status": "in-progress", "target_scope": "x", "tasks": [], "prs": []}),
            "## Execution State\n\n- Profile: tracking\n- Status: in-progress\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | done |",
            "2026-05-26T00:00:02Z",
        ),
    ]);

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "status",
        "--profile",
        "tracking",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let envelope = out.stdout_json();
    assert_eq!(envelope["schema_version"], "plan-issue.tracking.status.v1");
    assert_eq!(envelope["command"], "tracking.status");
    assert_eq!(envelope["status"], "ok");
    let result = &envelope["payload"]["result"];
    assert_eq!(result["operation"], "tracking.status");
    assert_eq!(result["fsm_state"], "RECORD_OPEN_INITIAL");
    let truth = &result["issue_truth"];
    assert_eq!(truth["available"], true);
    let roles: Vec<&str> = truth["latest_roles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(roles.contains(&"source"));
    assert!(roles.contains(&"plan"));
    assert!(roles.contains(&"state"));
}

#[test]
fn tracking_status_blocked_state_recommends_resolve_blocker() {
    let fixture = write_fixture(&[
        (
            "source",
            json!({"path": "p", "commit": "c"}),
            "## Source Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:00Z",
        ),
        (
            "plan",
            json!({"path": "p", "commit": "c"}),
            "## Plan Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:01Z",
        ),
        (
            "state",
            json!({"status": "blocked", "target_scope": "x", "tasks": [], "prs": [], "blockers": ["waiting on review"]}),
            "## Execution State\n\n- Profile: tracking\n- Status: blocked\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | pending |",
            "2026-05-26T00:00:02Z",
        ),
    ]);
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "status",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    assert_eq!(result["fsm_state"], "RECORD_BLOCKED");
    assert_eq!(result["recommended_action"], "resolve_blocker");
    assert!(result["blocked_reason"].is_string());
}

#[test]
fn tracking_status_reports_stale_run_state_when_issue_closed() {
    let fixture = write_fixture(&[(
        "closeout",
        json!({
            "final_status": "complete",
            "approval": {"approver": "x"},
            "linked_prs": [],
            "final_validation_url": null,
        }),
        "## Tracking Issue Closeout\n\n- Profile: tracking\n- Final status: complete\n- Approval: x",
        "2026-05-26T00:00:00Z",
    )]);
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    fs::write(
        &run_state_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-1",
            "repo": "owner/repo",
            "issue": 123,
            "profile": "tracking",
            "phase": "implementing",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T00:00:00Z"
        })
        .to_string(),
    )
    .expect("run-state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "status",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--run-state",
        run_state_path.to_str().expect("run-state"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    assert_eq!(result["fsm_state"], "RECORD_CLOSED");
    let warnings: Vec<&str> = result["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(
        warnings.contains(&"run-state-stale"),
        "warnings: {warnings:?}"
    );
}

#[test]
fn tracking_status_ready_for_close_emits_run_close_ready() {
    let fixture = write_fixture(&[
        (
            "source",
            json!({"path": "p", "commit": "c"}),
            "## Source Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:00Z",
        ),
        (
            "plan",
            json!({"path": "p", "commit": "c"}),
            "## Plan Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:01Z",
        ),
        (
            "state",
            json!({"status": "complete", "target_scope": "x", "tasks": [], "prs": [{"ref": "o/r#1", "url": "u", "status": "merged"}]}),
            "## Execution State\n\n- Profile: tracking\n- Status: complete\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | done |",
            "2026-05-26T00:00:02Z",
        ),
        (
            "session",
            json!({"summary": "done"}),
            "## Execution Session\n\n- Profile: tracking\n- Summary: done",
            "2026-05-26T00:00:03Z",
        ),
        (
            "validation",
            json!({"overall": "pass", "commands": [{"command": "cargo test", "status": "pass"}], "waivers": []}),
            "## Validation Evidence\n\n- Profile: tracking\n- Overall: pass\n\n| Command | Status | Evidence |\n|---|---|---|\n| cargo test | pass | log |",
            "2026-05-26T00:00:04Z",
        ),
        (
            "review",
            json!({"decision": "approve", "findings": [], "lenses": ["testing"]}),
            "## Review Evidence\n\n- Profile: tracking\n- Decision: approve",
            "2026-05-26T00:00:05Z",
        ),
    ]);
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "status",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    assert_eq!(result["fsm_state"], "RECORD_READY_FOR_CLOSE");
    assert_eq!(result["recommended_action"], "run_close_ready");
    assert!(
        result["missing_for_closeout"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn tracking_status_expect_visible_flows_through_to_visible_lint() {
    let fixture = write_fixture(&[(
        "state",
        json!({"status": "in-progress", "target_scope": "x", "tasks": [], "prs": []}),
        "## Execution State\n\n- Profile: tracking\n- Status: in-progress\n",
        "2026-05-26T00:00:00Z",
    )]);
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "status",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--expect-visible",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    assert!(result["visible"].is_object(), "visible missing: {result}");
    assert_eq!(result["visible"]["overall_pass"], false);
    let codes: Vec<&str> = result["visible"]["codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        codes.contains(&"state-missing-task-ledger"),
        "codes={codes:?}"
    );
}

#[test]
fn tracking_status_help_lists_status_subcommand() {
    let out = common::run_plan_issue(&["tracking", "--help"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert!(
        out.stdout_text().contains("status"),
        "missing status: {}",
        out.stdout_text()
    );
}

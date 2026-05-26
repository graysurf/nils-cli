//! `plan-issue tracking close-ready` integration coverage (Task 6.2).

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

use plan_issue_cli::lifecycle_record::PAYLOAD_SCHEMA_V2;

use crate::common;

fn json_stdout(out: &common::CmdOut) -> Value {
    serde_json::from_str(&out.stdout).expect("json stdout")
}

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

fn write_fixture(roles: &[(&str, Value, &str, &str)]) -> TempDir {
    let tmp = TempDir::new().expect("tmp");
    fs::write(tmp.path().join("body.md"), "## Final Dashboard\n").expect("body");
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

fn complete_fixture() -> TempDir {
    write_fixture(&[
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
            json!({
                "status": "complete",
                "target_scope": "x",
                "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
                "prs": [{"ref": "owner/repo#1", "url": "https://example.com/pr/1", "status": "merged"}]
            }),
            "## Execution State\n\n- Profile: tracking\n- Status: complete\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | done |",
            "2026-05-26T00:00:02Z",
        ),
        (
            "session",
            json!({"summary": "completed"}),
            "## Execution Session\n\n- Profile: tracking\n- Summary: completed",
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
    ])
}

#[test]
fn tracking_close_ready_reports_ready_for_complete_fixture() {
    let fixture = complete_fixture();
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--approval",
        "https://example.com/approval",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let result = json_stdout(&out)["payload"]["result"].clone();
    assert_eq!(result["fsm_state"], "RECORD_READY_FOR_CLOSE");
    let blockers: Vec<&str> = result["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert_eq!(
        result["ready"], true,
        "expected ready=true; blockers={blockers:?} result={result}"
    );
}

#[test]
fn tracking_close_ready_blocks_when_missing_validation() {
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
            json!({"status": "complete", "target_scope": "x", "tasks": [], "prs": []}),
            "## Execution State\n\n- Profile: tracking\n- Status: complete\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | done |",
            "2026-05-26T00:00:02Z",
        ),
        (
            "session",
            json!({"summary": "done"}),
            "## Execution Session\n\n- Profile: tracking\n- Summary: done",
            "2026-05-26T00:00:03Z",
        ),
    ]);
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--approval",
        "approver",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let result = json_stdout(&out)["payload"]["result"].clone();
    assert_eq!(result["ready"], false);
    let codes: Vec<&str> = result["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"validation-missing"));
}

#[test]
fn tracking_close_ready_collects_linked_prs_from_state_and_flag() {
    let fixture = complete_fixture();
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--approval",
        "approver",
        "--linked-pr",
        "owner/repo#999",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let result = json_stdout(&out)["payload"]["result"].clone();
    let linked: Vec<&str> = result["linked_prs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(linked.contains(&"owner/repo#1"));
    assert!(linked.contains(&"owner/repo#999"));
}

#[test]
fn tracking_close_ready_is_non_mutating() {
    // The command must not post comments or repair dashboards; the JSON
    // envelope therefore never names a `posted` field.
    let fixture = complete_fixture();
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--approval",
        "approver",
    ]);
    assert_eq!(out.code, 0);
    let result = json_stdout(&out)["payload"]["result"].clone();
    assert!(result.get("posted").is_none());
    assert!(result.get("dashboard_repaired").is_none());
}

#[test]
fn tracking_close_ready_help_lists_required_args() {
    let out = common::run_plan_issue(&["tracking", "close-ready", "--help"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("--linked-pr"));
    assert!(out.stdout.contains("--approval"));
    assert!(out.stdout.contains("--expect-visible"));
}

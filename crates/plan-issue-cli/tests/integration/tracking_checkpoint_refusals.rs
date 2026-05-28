//! `plan-issue tracking checkpoint` refusal coverage (Task 5.3).

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

fn write_run_state(path: &std::path::Path, phase: &str) {
    let body = json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "run-1",
        "repo": "owner/repo",
        "issue": 123,
        "profile": "tracking",
        "phase": phase,
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z"
    });
    fs::write(path, body.to_string()).expect("run-state");
}

#[test]
fn tracking_checkpoint_refusals_reject_source_plan_closeout_post_kinds() {
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "implementing");

    for kind in ["source", "plan", "closeout"] {
        let out = common::run_plan_issue(&[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("rs"),
            "--post",
            kind,
        ]);
        assert_ne!(out.code, 0, "post {kind} should fail");
        let envelope = json_stdout(&out);
        assert_eq!(
            envelope["error"]["code"],
            "tracking-checkpoint-role-not-allowed"
        );
    }
}

#[test]
fn tracking_checkpoint_refusals_block_when_run_state_stale_vs_issue_closed() {
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
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "implementing");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let result = json_stdout(&out)["payload"]["result"].clone();
    let codes: Vec<&str> = result["blocked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(
        codes.contains(&"run-state-stale"),
        "blocked codes: {codes:?}"
    );
    assert!(
        !result["roles_planned"].as_array().unwrap().is_empty()
            || !result["blocked"].as_array().unwrap().is_empty()
    );
}

// NOTE: the "tracking-checkpoint-live-not-implemented refusal" coverage that
// used to live here was retired when `tracking checkpoint --live` started
// actually posting (the C-phase fix for inbox
// `tracking-closeout-review-state-complete-gap`). The stable error code is
// retained in `execute.rs` for forward compatibility, but no live-mode
// invocation emits it any more. Positive coverage of the new live + fixture
// posting hop lives in `tracking_checkpoint_live.rs`.

#[test]
fn tracking_checkpoint_refusals_block_unknown_role() {
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "implementing");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "bogus",
    ]);
    assert_ne!(out.code, 0);
    let envelope = json_stdout(&out);
    assert_eq!(
        envelope["error"]["code"],
        "tracking-checkpoint-unknown-role"
    );
}

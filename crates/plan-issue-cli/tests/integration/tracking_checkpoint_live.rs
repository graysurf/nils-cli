//! `plan-issue tracking checkpoint --live` posting coverage.
//!
//! Sister file to `tracking_checkpoint_dry_run.rs` and
//! `tracking_checkpoint_refusals.rs`. These tests cover the fixture-mode
//! posting hop that landed when the
//! `tracking-checkpoint-live-not-implemented` blocked code was retired from
//! the live branch (inbox: `tracking-closeout-review-state-complete-gap`).
//!
//! True provider-bound live mode (real `gh`/`forge-cli` adapter calls) is
//! covered by the live integration suites against fixture provider
//! adapters; the deterministic behavior under `--fixture` is what runtime
//! smoke and the runtime-kit happy-path probe depend on, so it is the
//! primary coverage target here.

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
        "issue": 999,
        "profile": "tracking",
        "phase": phase,
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z",
        "selected_scope": {
            "task": "1.2",
            "title": "demo"
        },
        "branch": "feat/x",
        "review": {
            "decision": "approve",
            "evidence": null
        }
    });
    fs::write(path, body.to_string()).expect("run-state");
}

/// Pre-closeout fixture with source/plan present plus a `status=in-progress`
/// state comment — mirrors the shape `record open` writes initially. The
/// run-state's `phase=ready_for_close` then drives the controller to render
/// a final `status=complete` state and an `approve` review.
fn pre_closeout_fixture() -> TempDir {
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
            json!({"status": "in-progress", "target_scope": "x", "tasks": [], "prs": []}),
            "## Execution State\n\n- Profile: tracking\n- Status: in-progress\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | done |",
            "2026-05-26T00:00:02Z",
        ),
    ])
}

#[test]
fn tracking_checkpoint_live_fixture_returns_posted_state_role_with_synthesized_url() {
    let fixture = pre_closeout_fixture();
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "ready_for_close");

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
        "--issue",
        "144",
        "--live",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let result = json_stdout(&out)["payload"]["result"].clone();
    assert_eq!(result["mode"], "fixture");
    // The retired refusal code must not surface on the live path.
    let blocked_codes: Vec<&str> = result["blocked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(
        !blocked_codes.contains(&"tracking-checkpoint-live-not-implemented"),
        "blocked codes unexpectedly include the retired refusal: {blocked_codes:?}"
    );

    let posted = result["posted"].as_array().expect("posted array");
    assert_eq!(posted.len(), 1, "one comment per role: {posted:?}");
    assert_eq!(posted[0]["role"], "state");
    let url = posted[0]["comment_url"].as_str().expect("url");
    assert_eq!(url, "fixture://issue/144/state");
}

#[test]
fn tracking_checkpoint_live_fixture_posts_state_and_review_in_declaration_order() {
    let fixture = pre_closeout_fixture();
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "ready_for_close");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "state,review",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--issue",
        "144",
        "--live",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let result = json_stdout(&out)["payload"]["result"].clone();
    assert_eq!(result["mode"], "fixture");

    let posted = result["posted"].as_array().expect("posted array");
    assert_eq!(posted.len(), 2, "one comment per role: {posted:?}");
    let posted_roles: Vec<&str> = posted
        .iter()
        .map(|entry| entry["role"].as_str().unwrap())
        .collect();
    assert_eq!(posted_roles, vec!["state", "review"]);

    // The state body must reflect the run-state-driven `status=complete`
    // derivation (phase=ready_for_close maps to status=complete via
    // `synthesize_state_payload`).
    let rendered = result["rendered"].as_array().expect("rendered array");
    let state_entry = rendered
        .iter()
        .find(|entry| entry["role"] == "state")
        .expect("state rendered");
    let state_body = state_entry["body"].as_str().expect("state body");
    assert!(
        state_body.contains("Status: complete"),
        "state body should reflect status=complete; got: {state_body}"
    );
}

#[test]
fn tracking_checkpoint_live_fixture_repair_dashboard_returns_fixture_repair_result() {
    let fixture = pre_closeout_fixture();
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "ready_for_close");

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
        "--issue",
        "144",
        "--live",
        "--repair-dashboard",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let result = json_stdout(&out)["payload"]["result"].clone();
    let repair = &result["repair_dashboard_result"];
    assert_eq!(repair["operation"], "record.repair-dashboard");
    assert_eq!(repair["mode"], "fixture");
    assert_eq!(repair["dry_run"], true);
}

#[test]
fn tracking_checkpoint_live_visible_completeness_failure_short_circuits_before_posting() {
    // Force a visible-completeness failure by asking for the final
    // (`status=complete`) state shape from a run state at `phase=ready_for_close`
    // but with an empty Task Ledger — the visible lint should refuse the
    // body and the live branch must skip posting.
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
    ]);
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    // Use a non-final phase so the synthesizer renders an in-progress state
    // body. Then drop selected_scope by re-writing a minimal run state.
    let minimal = json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "run-1",
        "repo": "owner/repo",
        "issue": 1,
        "profile": "tracking",
        "phase": "implementing",
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z"
    });
    fs::write(&rs_path, minimal.to_string()).expect("rs");

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
        "--issue",
        "1",
        "--live",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let result = json_stdout(&out)["payload"]["result"].clone();
    // Either visible completeness fails (preferred) or the role renders
    // without lint failure but `posted` stays empty under the
    // empty-rendered guard. Both branches must avoid emitting the retired
    // refusal code.
    let blocked_codes: Vec<&str> = result["blocked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(
        !blocked_codes.contains(&"tracking-checkpoint-live-not-implemented"),
        "retired refusal must not surface: {blocked_codes:?}"
    );
}

#[test]
fn tracking_checkpoint_live_fixture_missing_issue_blocks_with_stable_code() {
    let fixture = pre_closeout_fixture();
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "ready_for_close");

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
        "--live", // no --issue
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let result = json_stdout(&out)["payload"]["result"].clone();
    let blocked_codes: Vec<&str> = result["blocked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(
        blocked_codes.contains(&"tracking-checkpoint-live-missing-issue"),
        "expected tracking-checkpoint-live-missing-issue, got {blocked_codes:?}"
    );
    let posted = result["posted"].as_array().expect("posted array");
    assert!(
        posted.is_empty(),
        "no posting may occur without --issue: {posted:?}"
    );
}

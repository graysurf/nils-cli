//! `plan-issue tracking run init` + `tracking run update` integration
//! coverage (Task 5.1).
//!
//! Source: `docs/source/plan-issue-redesign/plan-tracking-issue-run-state-controller-v1.md`.

use std::fs;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use plan_issue::tracking::run_state::{self, RUN_STATE_SCHEMA};

use crate::common;

#[test]
fn tracking_run_update_init_writes_run_state_and_events() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--task",
        "1.2",
        "--branch",
        "feat/x",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "run-test",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let envelope = out.stdout_json();
    assert_eq!(envelope["command"], "tracking.run.init");
    let result = &envelope["payload"]["result"];
    assert_eq!(result["run_id"], "run-test");
    assert!(run_state_path.exists());
    let events_path = run_state_path
        .parent()
        .map(|p| p.join("events.jsonl"))
        .expect("events");
    assert!(events_path.exists(), "events.jsonl should exist");

    // Verify schema id stayed stable.
    let raw = fs::read_to_string(&run_state_path).expect("read");
    assert!(raw.contains(RUN_STATE_SCHEMA));
}

#[test]
fn tracking_run_init_defaults_now_to_wallclock_when_now_omitted() {
    // Regression (issue #588): omitting `--now` must not write the 1970 epoch
    // placeholder into live run-state. The safe default is the current UTC time,
    // and `run_id` is derived from it rather than the `00000000…` placeholder.
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let run = run_state::read_run_state(&run_state_path).expect("read");
    assert_ne!(
        run.created_at, "1970-01-01T00:00:00Z",
        "init without --now must not record the 1970 placeholder"
    );
    assert_eq!(run.created_at, run.updated_at, "init seeds both timestamps");
    // RFC3339 UTC form with `Z` suffix, matching the workspace convention.
    assert!(
        run.created_at.contains('T') && run.created_at.ends_with('Z'),
        "created_at should be RFC3339 UTC: {}",
        run.created_at
    );

    let envelope = out.stdout_json();
    let run_id = envelope["payload"]["result"]["run_id"]
        .as_str()
        .expect("run_id");
    assert!(
        !run_id.starts_with("00000000000000"),
        "run_id should derive from a real timestamp, not the placeholder: {run_id}"
    );
    assert!(run_id.ends_with("-issue-123"), "run_id: {run_id}");
}

#[test]
fn tracking_run_update_changes_phase_and_appends_event() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");

    // Initialize.
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "run-update",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(out.code, 0, "init stderr: {}", out.stderr_text());

    // Update phase + validation.
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--phase",
        "validating",
        "--validation-overall",
        "pass",
        "--validation-command",
        "cargo test",
        "--validation-status",
        "pass",
        "--note",
        "validated locally",
        "--now",
        "2026-05-26T00:01:00Z",
    ]);
    assert_eq!(out.code, 0, "update stderr: {}", out.stderr_text());
    let envelope = out.stdout_json();
    let result = &envelope["payload"]["result"];
    assert_eq!(result["phase"], "validating");
    let changed: Vec<&str> = result["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(changed.contains(&"phase"));
    assert!(changed.contains(&"validation"));
    assert!(changed.contains(&"note"));

    let run = run_state::read_run_state(&run_state_path).expect("read");
    assert_eq!(run.phase.as_str(), "validating");
    assert_eq!(
        run.validation.as_ref().map(|v| v.overall.clone()),
        Some("pass".to_string())
    );
    assert!(!run.notes.is_empty());
    let events = std::fs::read_to_string(run_state_path.parent().unwrap().join("events.jsonl"))
        .expect("events");
    assert!(events.contains("run_updated"));
}

#[test]
fn tracking_run_update_records_rich_review_evidence() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let findings_path = tmp.path().join("review-findings.json");
    fs::write(
        &findings_path,
        r#"[{"id":"F1","severity":"minor","disposition":"fixed","summary":"Review context renders visibly"}]"#,
    )
    .expect("findings");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "run-review",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(out.code, 0, "init stderr: {}", out.stderr_text());

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--review-decision",
        "approve",
        "--review-lens",
        "testing",
        "--review-lens",
        "maintainability",
        "--review-outcome-comment",
        "https://example.test/review",
        "--review-findings-file",
        findings_path.to_str().expect("findings"),
        "--now",
        "2026-05-26T00:02:00Z",
    ]);
    assert_eq!(out.code, 0, "update stderr: {}", out.stderr_text());

    let run = run_state::read_run_state(&run_state_path).expect("read");
    let review = run.review.expect("review summary");
    assert_eq!(review.decision, "approve");
    assert_eq!(review.lenses, vec!["testing", "maintainability"]);
    assert_eq!(
        review.evidence.as_deref(),
        Some("https://example.test/review")
    );
    assert_eq!(review.findings.len(), 1);
    assert_eq!(review.findings[0].id, "F1");
    assert_eq!(review.findings[0].disposition, "fixed");
}

#[test]
fn tracking_run_update_rejects_invalid_run_state() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    fs::write(&run_state_path, "not json").expect("write");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--phase",
        "validating",
    ]);
    assert_ne!(out.code, 0, "should fail");
    let envelope = out.stdout_json();
    assert_eq!(envelope["error"]["code"], "tracking-run-update-read-failed");
}

#[test]
fn tracking_run_update_help_lists_run_init_and_update() {
    let out = common::run_plan_issue(&["tracking", "run", "--help"]);
    assert_eq!(out.code, 0);
    assert!(out.stdout_text().contains("init"));
    assert!(out.stdout_text().contains("update"));
}

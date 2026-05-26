//! Run-state schema integration coverage (Task 4.1).
//!
//! Source: `docs/source/plan-issue-redesign/plan-tracking-issue-run-state-controller-v1.md`.

use std::path::PathBuf;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use plan_issue_cli::tracking::run_state::{
    self, ExecutionRun, LinkedPr, RUN_STATE_SCHEMA, RunPhase, RunStateError, ValidationCommandRow,
    ValidationSummary,
};

fn fixture_run() -> ExecutionRun {
    let mut run = ExecutionRun::new(
        "20260526-150405-issue-123",
        "owner/repo",
        123,
        "tracking",
        RunPhase::Implementing,
        "2026-05-26T15:04:05Z",
    );
    run.bundle = Some(PathBuf::from("docs/plans/example"));
    run.execution_state_file = Some(PathBuf::from(
        "docs/plans/example/example-execution-state.md",
    ));
    run.branch = Some("feat/example".to_string());
    run.pr = Some(LinkedPr {
        r#ref: "owner/repo#456".to_string(),
        url: Some("https://example.com/pr/456".to_string()),
        status: Some("open".to_string()),
    });
    run.validation = Some(ValidationSummary {
        overall: "pass".to_string(),
        commands: vec![ValidationCommandRow {
            command: "cargo test".to_string(),
            status: "pass".to_string(),
            evidence: Some("log.txt".to_string()),
        }],
        waiver: None,
        evidence_path: None,
    });
    run
}

#[test]
fn tracking_run_state_round_trips_disk_io() {
    let tmp = TempDir::new().expect("tmp");
    let path = tmp.path().join("run-state.json");
    let run = fixture_run();
    run_state::write_run_state(&path, &run).expect("write");
    let loaded = run_state::read_run_state(&path).expect("read");
    assert_eq!(loaded.run_id, run.run_id);
    assert_eq!(loaded.repo, run.repo);
    assert_eq!(loaded.issue, run.issue);
    assert_eq!(loaded.phase, run.phase);
    assert_eq!(loaded.bundle, run.bundle);
    assert_eq!(loaded.execution_state_file, run.execution_state_file);
    assert_eq!(loaded.branch, run.branch);
    assert_eq!(
        loaded.pr.as_ref().map(|p| p.r#ref.clone()),
        Some("owner/repo#456".to_string())
    );
    assert_eq!(
        loaded.validation.as_ref().map(|v| v.overall.clone()),
        Some("pass".to_string())
    );
}

#[test]
fn tracking_run_state_runroot_uses_existing_layout_contract() {
    let runroot = run_state::RunRoot::new("owner__repo", 123, "20260526-run-1").expect("runroot");
    let suffix = "owner__repo/issue-123/runs/20260526-run-1";
    assert!(
        runroot.root().to_string_lossy().ends_with(suffix),
        "runroot drift: {}",
        runroot.root().display()
    );
    assert!(runroot.run_state_path().ends_with("run-state.json"));
    assert!(runroot.events_path().ends_with("events.jsonl"));
}

#[test]
fn tracking_run_state_rejects_wrong_schema_id() {
    let body = serde_json::json!({
        "schema": "plan-issue.unknown.v1",
        "run_id": "x",
        "repo": "o/r",
        "issue": 1,
        "profile": "tracking",
        "phase": "initial",
        "created_at": "x",
        "updated_at": "x"
    })
    .to_string();
    match run_state::parse_run_state(&body) {
        Err(RunStateError::SchemaMismatch { actual }) => {
            assert_eq!(actual, "plan-issue.unknown.v1");
        }
        other => panic!("expected SchemaMismatch, got {other:?}"),
    }
}

#[test]
fn tracking_run_state_schema_identifier_is_stable() {
    assert_eq!(RUN_STATE_SCHEMA, "plan-issue.execution-run.v1");
}

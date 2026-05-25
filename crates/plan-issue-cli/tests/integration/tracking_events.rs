//! Append-only event journal integration coverage (Task 4.1).
//!
//! Source: `docs/source/plan-issue-redesign/plan-tracking-issue-run-state-controller-v1.md`.

use std::io::Write;

use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use plan_issue_cli::tracking::events::{
    self, EVENT_SCHEMA, ExecutionEvent, ExecutionEventKind,
};

#[test]
fn tracking_events_schema_identifier_is_stable() {
    assert_eq!(EVENT_SCHEMA, "plan-issue.execution-event.v1");
}

#[test]
fn tracking_events_appends_run_start_through_checkpoint_in_order() {
    let tmp = TempDir::new().expect("tmp");
    let path = tmp.path().join("nested/events.jsonl");
    let sequence: Vec<ExecutionEvent> = vec![
        ExecutionEvent::new("run-1", ExecutionEventKind::RunStarted, "t1"),
        ExecutionEvent::new("run-1", ExecutionEventKind::TaskSelected, "t2")
            .with_task("1.2"),
        ExecutionEvent::new("run-1", ExecutionEventKind::ValidationRecorded, "t3")
            .with_detail(json!({"status": "pass"})),
        ExecutionEvent::new("run-1", ExecutionEventKind::Reconciled, "t4")
            .with_detail(json!({"fsm_state": "RECORD_OPEN_ACTIVE"})),
        ExecutionEvent::new("run-1", ExecutionEventKind::CheckpointPosted, "t5")
            .with_detail(json!({"roles": ["state", "validation"]})),
        ExecutionEvent::new("run-1", ExecutionEventKind::CheckpointFailed, "t6")
            .with_detail(json!({"code": "transition-not-allowed"})),
    ];
    for event in &sequence {
        events::append_event(&path, event).expect("append");
    }
    let read = events::read_events(&path).expect("read");
    let kinds: Vec<_> = read.iter().map(|e| e.kind.clone()).collect();
    assert_eq!(
        kinds,
        vec![
            ExecutionEventKind::RunStarted,
            ExecutionEventKind::TaskSelected,
            ExecutionEventKind::ValidationRecorded,
            ExecutionEventKind::Reconciled,
            ExecutionEventKind::CheckpointPosted,
            ExecutionEventKind::CheckpointFailed,
        ]
    );
    assert_eq!(read[1].task.as_deref(), Some("1.2"));
    assert_eq!(read[4].detail["roles"][0], "state");
    assert_eq!(read[5].detail["code"], "transition-not-allowed");
}

#[test]
fn tracking_events_jsonl_does_not_rewrite_prior_lines() {
    let tmp = TempDir::new().expect("tmp");
    let path = tmp.path().join("events.jsonl");
    events::append_event(
        &path,
        &ExecutionEvent::new("run-1", ExecutionEventKind::RunStarted, "t1"),
    )
    .expect("append 1");

    // Capture the raw file contents and confirm appending another event
    // simply adds bytes at the tail.
    let raw_before = std::fs::read_to_string(&path).expect("read");
    events::append_event(
        &path,
        &ExecutionEvent::new("run-1", ExecutionEventKind::RunCompleted, "t2"),
    )
    .expect("append 2");
    let raw_after = std::fs::read_to_string(&path).expect("read");
    assert!(
        raw_after.starts_with(&raw_before),
        "second append rewrote prior lines: before={raw_before:?} after={raw_after:?}"
    );
}

#[test]
fn tracking_events_large_detail_can_be_stored_as_path_pointer() {
    // The journal must store path/preview pointers, not inline blobs. This
    // test demonstrates the supported pattern: callers store paths (or
    // redacted previews) in `detail`, then read the artifact lazily.
    let tmp = TempDir::new().expect("tmp");
    let log = tmp.path().join("artifacts/validation/log.txt");
    std::fs::create_dir_all(log.parent().unwrap()).expect("mkdir");
    let mut f = std::fs::File::create(&log).expect("create");
    f.write_all(b"large evidence body").expect("write");
    let path = tmp.path().join("events.jsonl");
    let event = ExecutionEvent::new("run-1", ExecutionEventKind::ValidationRecorded, "t1")
        .with_detail(json!({
            "evidence_path": log.to_string_lossy(),
            "preview": "large evidence...",
        }));
    events::append_event(&path, &event).expect("append");
    let read = events::read_events(&path).expect("read");
    assert!(read[0].detail["evidence_path"]
        .as_str()
        .unwrap_or_default()
        .ends_with("artifacts/validation/log.txt"));
}

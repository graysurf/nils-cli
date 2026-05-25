//! Append-only event journal for tracking runs.
//!
//! Schema identifier: `plan-issue.execution-event.v1`. Records run start,
//! task selection, run updates, validation/review evidence, checkpoint
//! attempts (dry-run + live), reconciliation outcomes, and failures.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime_layout;

pub const EVENT_SCHEMA: &str = "plan-issue.execution-event.v1";

/// Discrete event type written to `events.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEventKind {
    RunStarted,
    RunUpdated,
    TaskSelected,
    PhaseChanged,
    ValidationRecorded,
    ReviewRecorded,
    Reconciled,
    CheckpointPlanned,
    CheckpointPosted,
    CheckpointFailed,
    BlockerAdded,
    BlockerCleared,
    RunCompleted,
}

impl ExecutionEventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RunStarted => "run_started",
            Self::RunUpdated => "run_updated",
            Self::TaskSelected => "task_selected",
            Self::PhaseChanged => "phase_changed",
            Self::ValidationRecorded => "validation_recorded",
            Self::ReviewRecorded => "review_recorded",
            Self::Reconciled => "reconciled",
            Self::CheckpointPlanned => "checkpoint_planned",
            Self::CheckpointPosted => "checkpoint_posted",
            Self::CheckpointFailed => "checkpoint_failed",
            Self::BlockerAdded => "blocker_added",
            Self::BlockerCleared => "blocker_cleared",
            Self::RunCompleted => "run_completed",
        }
    }
}

/// One row in the append-only `events.jsonl` journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEvent {
    pub schema: String,
    pub run_id: String,
    pub at: String,
    #[serde(rename = "type")]
    pub kind: ExecutionEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Free-form structured detail for the event. Stays a JSON object so the
    /// schema can grow without rewriting prior events.
    #[serde(default, skip_serializing_if = "is_null_value")]
    pub detail: Value,
}

fn is_null_value(value: &Value) -> bool {
    value.is_null()
}

impl ExecutionEvent {
    pub fn new(run_id: impl Into<String>, kind: ExecutionEventKind, at: impl Into<String>) -> Self {
        Self {
            schema: EVENT_SCHEMA.to_string(),
            run_id: run_id.into(),
            at: at.into(),
            kind,
            task: None,
            note: None,
            detail: Value::Null,
        }
    }

    pub fn with_task(mut self, task: impl Into<String>) -> Self {
        self.task = Some(task.into());
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn with_detail(mut self, detail: Value) -> Self {
        self.detail = detail;
        self
    }
}

/// Append an event to `events.jsonl`. Creates parents and the file when
/// missing; never rewrites prior lines.
pub fn append_event(path: &Path, event: &ExecutionEvent) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        runtime_layout::ensure_dir(parent)?;
    }
    let serialized = serde_json::to_string(event)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(serialized.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Parse every line of `events.jsonl` into an [`ExecutionEvent`]. Skips
/// empty lines; reports malformed lines via the returned error.
pub fn read_events(path: &Path) -> io::Result<Vec<ExecutionEvent>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for (lineno, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event: ExecutionEvent = serde_json::from_str(trimmed).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("events.jsonl line {lineno}: {err}"),
            )
        })?;
        events.push(event);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn tracking_events_round_trip_single_event() {
        let event = ExecutionEvent::new("run-1", ExecutionEventKind::RunStarted, "2026-05-26T00:00:00Z")
            .with_note("session start");
        let raw = serde_json::to_string(&event).expect("serialize");
        let parsed: ExecutionEvent = serde_json::from_str(&raw).expect("parse");
        assert_eq!(parsed.run_id, "run-1");
        assert_eq!(parsed.kind, ExecutionEventKind::RunStarted);
        assert_eq!(parsed.note.as_deref(), Some("session start"));
    }

    #[test]
    fn tracking_events_appends_without_rewriting_prior_lines() {
        let tmp = TempDir::new().expect("tmp");
        let path = tmp.path().join("events.jsonl");
        let e1 = ExecutionEvent::new("run-1", ExecutionEventKind::RunStarted, "t1");
        let e2 = ExecutionEvent::new("run-1", ExecutionEventKind::Reconciled, "t2")
            .with_detail(json!({"fsm_state": "RECORD_OPEN_ACTIVE"}));
        let e3 = ExecutionEvent::new("run-1", ExecutionEventKind::CheckpointPosted, "t3")
            .with_detail(json!({"roles": ["state", "validation"]}));
        append_event(&path, &e1).expect("append 1");
        append_event(&path, &e2).expect("append 2");
        append_event(&path, &e3).expect("append 3");

        let events = read_events(&path).expect("read");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, ExecutionEventKind::RunStarted);
        assert_eq!(events[1].kind, ExecutionEventKind::Reconciled);
        assert_eq!(events[2].kind, ExecutionEventKind::CheckpointPosted);
        assert_eq!(events[2].detail["roles"][0], "state");
    }

    #[test]
    fn tracking_events_skips_empty_lines_and_reports_malformed() {
        let tmp = TempDir::new().expect("tmp");
        let path = tmp.path().join("events.jsonl");
        let good = ExecutionEvent::new("run-1", ExecutionEventKind::RunStarted, "t1");
        append_event(&path, &good).expect("append good");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open append")
            .write_all(b"\n   \n")
            .expect("blank lines");
        let events = read_events(&path).expect("read");
        assert_eq!(events.len(), 1);

        // Malformed line should error.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open append")
            .write_all(b"not json\n")
            .expect("bad line");
        let err = read_events(&path).expect_err("malformed should error");
        assert!(err.to_string().contains("line"));
    }
}

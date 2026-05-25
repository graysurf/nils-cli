//! Append-only event journal for tracking runs.
//!
//! Schema identifier: `plan-issue.execution-event.v1`. Records run start,
//! task selection, run updates, validation/review evidence, checkpoint
//! attempts (dry-run + live), reconciliation outcomes, and failures.
//!
//! Task 1.1 only introduces the data shapes; Task 4.1 wires the JSONL writer
//! under the issue-scoped runtime root.

use serde::{Deserialize, Serialize};

pub const EVENT_SCHEMA: &str = "plan-issue.execution-event.v1";

/// Discrete event kind written to `events.jsonl`.
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

/// One row in the append-only `events.jsonl` journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEvent {
    pub schema: String,
    pub run_id: String,
    pub at: String,
    pub kind: ExecutionEventKind,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub detail: serde_json::Value,
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
            detail: serde_json::Value::Null,
        }
    }
}

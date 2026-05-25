//! Typed local run-state for `plan-issue tracking`.
//!
//! Schema identifier: `plan-issue.execution-run.v1`. File layout under the
//! existing state-dir contract is documented in
//! `docs/source/plan-issue-redesign/plan-tracking-issue-run-state-controller-v1.md`.
//!
//! Task 1.1 only introduces the data shapes; Task 4.1 wires the actual JSON
//! reader/writer, schema validation, and disk layout.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Stable schema identifier embedded in every `run-state.json` document.
pub const RUN_STATE_SCHEMA: &str = "plan-issue.execution-run.v1";

/// High-level execution phase. Mirrors the FSM states without dragging in
/// reconciliation details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    Initial,
    Active,
    Blocked,
    Validating,
    Reviewed,
    ReadyForClose,
    Closed,
}

impl RunPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Active => "active",
            Self::Blocked => "blocked",
            Self::Validating => "validating",
            Self::Reviewed => "reviewed",
            Self::ReadyForClose => "ready_for_close",
            Self::Closed => "closed",
        }
    }
}

/// Typed run state document persisted as `run-state.json` under the
/// issue-scoped runtime root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRun {
    pub schema: String,
    pub run_id: String,
    pub repo: String,
    pub issue: u64,
    pub plan_bundle: PathBuf,
    #[serde(default)]
    pub selected_task: Option<String>,
    #[serde(default)]
    pub selected_sprint: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub worktree: Option<PathBuf>,
    #[serde(default)]
    pub linked_pr: Option<String>,
    pub phase: RunPhase,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub validation: Option<ValidationSummary>,
    #[serde(default)]
    pub review: Option<ReviewSummary>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    pub blockers: Vec<BlockerRef>,
    pub started_at: String,
    pub updated_at: String,
}

/// Compact validation summary captured in run state. Detailed evidence lives
/// in lifecycle comments and validation artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationSummary {
    pub overall: String,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub waiver: Option<String>,
    #[serde(default)]
    pub evidence_path: Option<PathBuf>,
}

/// Compact review summary captured in run state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewSummary {
    pub decision: String,
    #[serde(default)]
    pub findings_disposition: Vec<String>,
    #[serde(default)]
    pub evidence_path: Option<PathBuf>,
}

/// Reference to a large retained artifact (validation log, review report, …).
/// Large blobs live on disk; the run state only carries a path or redacted
/// preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub kind: String,
    pub path: PathBuf,
    #[serde(default)]
    pub preview: Option<String>,
}

/// Reference to a current execution blocker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockerRef {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub owner: Option<String>,
}

//! Tracking checkpoint behavior (dry-run and live).
//!
//! Task 1.1 declares the result shapes; Tasks 5.2, 5.3, and 6.1 implement
//! the dry-run renderer, refusal cases, and live adapter over the existing
//! lifecycle primitives.

use serde::{Deserialize, Serialize};

use crate::lifecycle_record::PayloadRole;

/// Selected role with a fully rendered Markdown body ready to post.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointSlice {
    pub role: PayloadRole,
    pub body: String,
    #[serde(default)]
    pub planned_comment_url: Option<String>,
}

/// Outcome of one checkpoint pass (dry-run or live).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheckpointPlan {
    pub roles_planned: Vec<PayloadRole>,
    pub roles_skipped: Vec<SkippedRole>,
    pub rendered: Vec<CheckpointSlice>,
    pub repair_dashboard: bool,
    pub blocked: Vec<BlockedCheckpoint>,
}

impl CheckpointPlan {
    pub fn is_blocked(&self) -> bool {
        !self.blocked.is_empty()
    }
}

/// Skipped role with the reason recorded for the JSON envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedRole {
    pub role: PayloadRole,
    pub reason: String,
}

/// Stable blocked code returned when a checkpoint is refused. Codes follow
/// `<role>-<rule>` (`state-stale-run-state`, `validation-missing-overall`,
/// `closeout-not-allowed-from-checkpoint`, …) — the full catalog is finalized
/// in Task 5.3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedCheckpoint {
    pub code: String,
    pub role: Option<PayloadRole>,
    pub message: String,
    pub suggested_unblock: String,
}

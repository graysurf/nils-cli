//! Non-mutating close-readiness probe.
//!
//! Task 1.1 declares the result shapes; Task 6.2 implements the probe
//! against the same strict gates as `record close`.

use serde::{Deserialize, Serialize};

/// Stable blocked code emitted by the probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloseReadyBlocker {
    pub code: String,
    pub message: String,
    pub suggested_unblock: String,
}

/// Outcome of the close-ready probe.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CloseReadyReport {
    pub ready: bool,
    pub blockers: Vec<CloseReadyBlocker>,
    pub linked_prs: Vec<String>,
    #[serde(default)]
    pub visible_completeness: Option<VisibleSummary>,
}

/// Compact visible-completeness summary attached to the probe result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisibleSummary {
    pub checked: bool,
    pub pass: bool,
    pub findings: Vec<String>,
}

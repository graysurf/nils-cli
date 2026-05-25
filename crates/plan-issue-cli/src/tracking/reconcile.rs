//! Provider-evidence reconciliation for tracking runs.
//!
//! Task 1.1 declares the inputs/outputs; Task 4.2 wires the actual
//! reconciliation logic (provider issue comments + dashboard + plan bundle +
//! run state + event journal → FSM input).

use serde::{Deserialize, Serialize};

use crate::tracking::fsm::{RecommendedAction, RecordState};

/// Reconciled view used by [`super::fsm`] and the `tracking status` command.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Reconciled {
    pub state: Option<RecordState>,
    pub recommended_action: Option<RecommendedAction>,
    pub warnings: Vec<ReconciliationWarning>,
    pub safe_transitions: Vec<RecordState>,
}

impl Reconciled {
    pub fn is_stale(&self) -> bool {
        self.warnings
            .iter()
            .any(|w| matches!(w.kind, ReconciliationWarningKind::LocalRunStateStale))
    }
}

/// Specific reconciliation warning emitted to the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationWarning {
    pub kind: ReconciliationWarningKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationWarningKind {
    /// Provider issue evidence is ahead of local run state.
    LocalRunStateStale,
    /// Plan bundle on disk is dirty relative to the snapshot.
    PlanBundleDirty,
    /// Dashboard body is out of sync with current lifecycle evidence.
    DashboardOutOfSync,
    /// Lifecycle role expected by the FSM is missing.
    LifecycleRoleMissing,
    /// Run state cannot be parsed; controller falls back to provider truth.
    RunStateUnparseable,
}

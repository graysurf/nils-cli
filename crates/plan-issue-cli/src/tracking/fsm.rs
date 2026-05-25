//! Deterministic finite state machine for the plan-tracking record.
//!
//! States and transitions are defined in
//! `docs/source/plan-issue-redesign/plan-tracking-issue-workflow-v1.md`.
//!
//! Task 1.1 declares the state set; Task 4.2 implements the transition
//! function and reconciles provider evidence against local run state.

use serde::{Deserialize, Serialize};

/// Canonical FSM state for the tracking record. Determined from provider
/// issue evidence (the durable truth), reconciled against local run state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RecordState {
    RecordUnopened,
    RecordOpenInitial,
    RecordOpenActive,
    RecordBlocked,
    RecordValidating,
    RecordReviewed,
    RecordReadyForClose,
    RecordClosed,
}

impl RecordState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecordUnopened => "RECORD_UNOPENED",
            Self::RecordOpenInitial => "RECORD_OPEN_INITIAL",
            Self::RecordOpenActive => "RECORD_OPEN_ACTIVE",
            Self::RecordBlocked => "RECORD_BLOCKED",
            Self::RecordValidating => "RECORD_VALIDATING",
            Self::RecordReviewed => "RECORD_REVIEWED",
            Self::RecordReadyForClose => "RECORD_READY_FOR_CLOSE",
            Self::RecordClosed => "RECORD_CLOSED",
        }
    }
}

/// Recommended next action emitted alongside the FSM evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendedAction {
    OpenRecord,
    AttachInitialState,
    CheckpointProgress,
    RecordValidation,
    RecordReview,
    ResolveBlocker,
    RunCloseReady,
    CallRecordClose,
    NoOp,
}

impl RecommendedAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenRecord => "open_record",
            Self::AttachInitialState => "attach_initial_state",
            Self::CheckpointProgress => "checkpoint_progress",
            Self::RecordValidation => "record_validation",
            Self::RecordReview => "record_review",
            Self::ResolveBlocker => "resolve_blocker",
            Self::RunCloseReady => "run_close_ready",
            Self::CallRecordClose => "call_record_close",
            Self::NoOp => "noop",
        }
    }
}

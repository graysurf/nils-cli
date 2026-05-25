//! Deterministic finite state machine for the plan-tracking record.
//!
//! States and transitions are defined in
//! `docs/source/plan-issue-redesign/plan-tracking-issue-workflow-v1.md`.

use serde::{Deserialize, Serialize};

use crate::lifecycle_record::{PayloadRole, RecordAudit};
use crate::tracking::run_state::ExecutionRun;

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

/// FSM evaluation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsmEvaluation {
    pub state: RecordState,
    pub recommended: RecommendedAction,
    pub missing_for_closeout: Vec<String>,
    pub safe_transitions: Vec<String>,
    pub blocked_reason: Option<String>,
}

/// Compute the FSM state from a record audit. `None` audit means no provider
/// issue exists yet.
pub fn evaluate_audit(audit: Option<&RecordAudit>) -> FsmEvaluation {
    let Some(audit) = audit else {
        return FsmEvaluation {
            state: RecordState::RecordUnopened,
            recommended: RecommendedAction::OpenRecord,
            missing_for_closeout: vec![
                "source".into(),
                "plan".into(),
                "state".into(),
                "session".into(),
                "validation".into(),
                "review".into(),
            ],
            safe_transitions: vec!["open_record".into()],
            blocked_reason: None,
        };
    };

    let has_source = audit.evidence.contains_key("source");
    let has_plan = audit.evidence.contains_key("plan");
    let has_state = audit.evidence.contains_key("state");
    let has_session = audit.evidence.contains_key("session");
    let has_validation = audit.evidence.contains_key("validation");
    let has_review = audit.evidence.contains_key("review");
    let has_closeout = audit.evidence.contains_key("closeout");

    if has_closeout {
        return FsmEvaluation {
            state: RecordState::RecordClosed,
            recommended: RecommendedAction::NoOp,
            missing_for_closeout: Vec::new(),
            safe_transitions: Vec::new(),
            blocked_reason: None,
        };
    }

    // Required source/plan/state must exist before we can talk about progress.
    if !(has_source && has_plan && has_state) {
        let recommended = RecommendedAction::AttachInitialState;
        let mut missing: Vec<String> = Vec::new();
        if !has_source {
            missing.push("source".into());
        }
        if !has_plan {
            missing.push("plan".into());
        }
        if !has_state {
            missing.push("state".into());
        }
        if !has_session {
            missing.push("session".into());
        }
        if !has_validation {
            missing.push("validation".into());
        }
        if !has_review {
            missing.push("review".into());
        }
        return FsmEvaluation {
            state: RecordState::RecordOpenInitial,
            recommended,
            missing_for_closeout: missing,
            safe_transitions: vec!["attach_initial_state".into(), "checkpoint_progress".into()],
            blocked_reason: None,
        };
    }

    // Detect blocked status from the latest state payload.
    let blocked = state_status_is_blocked(audit);
    if blocked {
        return FsmEvaluation {
            state: RecordState::RecordBlocked,
            recommended: RecommendedAction::ResolveBlocker,
            missing_for_closeout: closeout_missing(audit),
            safe_transitions: vec!["resolve_blocker".into(), "checkpoint_progress".into()],
            blocked_reason: Some("latest state payload status=blocked".to_string()),
        };
    }

    // Closeout readiness path: complete state + session + validation + review.
    let state_complete = state_status_is(audit, "complete");
    if state_complete && has_session && has_validation && has_review {
        return FsmEvaluation {
            state: RecordState::RecordReadyForClose,
            recommended: RecommendedAction::RunCloseReady,
            missing_for_closeout: Vec::new(),
            safe_transitions: vec!["run_close_ready".into(), "call_record_close".into()],
            blocked_reason: None,
        };
    }

    if has_review {
        let missing = closeout_missing(audit);
        return FsmEvaluation {
            state: RecordState::RecordReviewed,
            recommended: if missing.is_empty() {
                RecommendedAction::RunCloseReady
            } else {
                RecommendedAction::CheckpointProgress
            },
            missing_for_closeout: missing,
            safe_transitions: vec!["checkpoint_progress".into(), "run_close_ready".into()],
            blocked_reason: None,
        };
    }
    if has_validation {
        return FsmEvaluation {
            state: RecordState::RecordValidating,
            recommended: RecommendedAction::RecordReview,
            missing_for_closeout: closeout_missing(audit),
            safe_transitions: vec!["record_review".into(), "checkpoint_progress".into()],
            blocked_reason: None,
        };
    }
    if has_session {
        return FsmEvaluation {
            state: RecordState::RecordOpenActive,
            recommended: RecommendedAction::RecordValidation,
            missing_for_closeout: closeout_missing(audit),
            safe_transitions: vec![
                "record_validation".into(),
                "checkpoint_progress".into(),
                "record_review".into(),
            ],
            blocked_reason: None,
        };
    }
    // Source/plan/state present but no session yet — initial open with no
    // active work captured.
    FsmEvaluation {
        state: RecordState::RecordOpenInitial,
        recommended: RecommendedAction::CheckpointProgress,
        missing_for_closeout: closeout_missing(audit),
        safe_transitions: vec!["checkpoint_progress".into(), "record_validation".into()],
        blocked_reason: None,
    }
}

fn closeout_missing(audit: &RecordAudit) -> Vec<String> {
    let mut missing: Vec<String> = Vec::new();
    if !audit.evidence.contains_key("session") {
        missing.push("session".into());
    }
    if !audit.evidence.contains_key("validation") {
        missing.push("validation".into());
    }
    if !audit.evidence.contains_key("review") {
        missing.push("review".into());
    }
    // State must report `complete` for closeout. If we have state but it is
    // not complete, surface the gap.
    if let Some(state_evidence) = audit.evidence.get("state") {
        let complete = state_evidence
            .status
            .as_deref()
            .is_some_and(|status| status.eq_ignore_ascii_case("complete"));
        if !complete {
            missing.push("state_complete".into());
        }
    } else {
        missing.push("state".into());
    }
    missing
}

fn state_status_is(audit: &RecordAudit, target: &str) -> bool {
    audit
        .evidence
        .get("state")
        .and_then(|ev| ev.status.as_deref())
        .is_some_and(|status| status.eq_ignore_ascii_case(target))
}

fn state_status_is_blocked(audit: &RecordAudit) -> bool {
    state_status_is(audit, "blocked")
}

/// Cross-check the FSM evaluation against local run state. Returns the
/// staleness verdict the controller uses to refuse live mutation.
pub fn run_state_is_stale(audit: Option<&RecordAudit>, run: &ExecutionRun) -> bool {
    let Some(audit) = audit else {
        return false;
    };
    // If the run state still says implementing but the issue already has a
    // closeout comment, run state is stale.
    if audit.evidence.contains_key("closeout") && !matches!(run.phase, crate::tracking::run_state::RunPhase::Closed) {
        return true;
    }
    false
}

/// Helper used by the `tracking` command surface to project the FSM state
/// onto the `PayloadRole`s missing for closeout.
pub fn missing_payload_roles(audit: Option<&RecordAudit>) -> Vec<PayloadRole> {
    let mut missing = Vec::new();
    let Some(audit) = audit else {
        return vec![
            PayloadRole::Source,
            PayloadRole::Plan,
            PayloadRole::State,
            PayloadRole::Session,
            PayloadRole::Validation,
            PayloadRole::Review,
        ];
    };
    for role in [
        PayloadRole::Source,
        PayloadRole::Plan,
        PayloadRole::State,
        PayloadRole::Session,
        PayloadRole::Validation,
        PayloadRole::Review,
    ] {
        if !audit.evidence.contains_key(role.as_str()) {
            missing.push(role);
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle_record::{
        BodySections, LifecycleEvidence, PayloadProfile, RecordAudit,
    };
    use std::collections::BTreeMap;

    fn evidence_for(role: PayloadRole, status: Option<&str>) -> LifecycleEvidence {
        LifecycleEvidence {
            role,
            profile: PayloadProfile::Tracking,
            url: Some(format!("https://example.com/{}", role.as_str())),
            created_at: Some("2026-05-26T00:00:00Z".to_string()),
            status: status.map(|s| s.to_string()),
            payload: None,
        }
    }

    fn audit_with(roles: &[(PayloadRole, Option<&str>)]) -> RecordAudit {
        let mut evidence: BTreeMap<String, LifecycleEvidence> = BTreeMap::new();
        for (role, status) in roles {
            evidence.insert(role.as_str().to_string(), evidence_for(*role, *status));
        }
        RecordAudit {
            profile_filter: Some("tracking".to_string()),
            body_sections: BodySections {
                current_dashboard: false,
                final_dashboard: false,
                durable_record: false,
                closeout_checks: false,
                task_decomposition: false,
            },
            evidence,
            missing_required: Vec::new(),
            unsupported_markers: Vec::new(),
            recognized_count: roles.len(),
            evidence_text: String::new(),
        }
    }

    #[test]
    fn tracking_fsm_unopened_when_audit_missing() {
        let result = evaluate_audit(None);
        assert_eq!(result.state, RecordState::RecordUnopened);
        assert_eq!(result.recommended, RecommendedAction::OpenRecord);
    }

    #[test]
    fn tracking_fsm_open_initial_when_source_plan_state_present() {
        let audit = audit_with(&[
            (PayloadRole::Source, None),
            (PayloadRole::Plan, None),
            (PayloadRole::State, Some("in-progress")),
        ]);
        let result = evaluate_audit(Some(&audit));
        assert_eq!(result.state, RecordState::RecordOpenInitial);
        assert_eq!(result.recommended, RecommendedAction::CheckpointProgress);
        assert!(result.missing_for_closeout.iter().any(|s| s == "session"));
    }

    #[test]
    fn tracking_fsm_open_active_when_session_present() {
        let audit = audit_with(&[
            (PayloadRole::Source, None),
            (PayloadRole::Plan, None),
            (PayloadRole::State, Some("in-progress")),
            (PayloadRole::Session, None),
        ]);
        let result = evaluate_audit(Some(&audit));
        assert_eq!(result.state, RecordState::RecordOpenActive);
        assert_eq!(result.recommended, RecommendedAction::RecordValidation);
    }

    #[test]
    fn tracking_fsm_blocked_when_state_status_blocked() {
        let audit = audit_with(&[
            (PayloadRole::Source, None),
            (PayloadRole::Plan, None),
            (PayloadRole::State, Some("blocked")),
        ]);
        let result = evaluate_audit(Some(&audit));
        assert_eq!(result.state, RecordState::RecordBlocked);
        assert_eq!(result.recommended, RecommendedAction::ResolveBlocker);
        assert!(result.blocked_reason.is_some());
    }

    #[test]
    fn tracking_fsm_validating_when_validation_present() {
        let audit = audit_with(&[
            (PayloadRole::Source, None),
            (PayloadRole::Plan, None),
            (PayloadRole::State, Some("in-progress")),
            (PayloadRole::Session, None),
            (PayloadRole::Validation, Some("pass")),
        ]);
        let result = evaluate_audit(Some(&audit));
        assert_eq!(result.state, RecordState::RecordValidating);
        assert_eq!(result.recommended, RecommendedAction::RecordReview);
    }

    #[test]
    fn tracking_fsm_reviewed_when_review_but_not_complete() {
        let audit = audit_with(&[
            (PayloadRole::Source, None),
            (PayloadRole::Plan, None),
            (PayloadRole::State, Some("in-progress")),
            (PayloadRole::Session, None),
            (PayloadRole::Validation, Some("pass")),
            (PayloadRole::Review, Some("approve")),
        ]);
        let result = evaluate_audit(Some(&audit));
        assert_eq!(result.state, RecordState::RecordReviewed);
    }

    #[test]
    fn tracking_fsm_ready_for_close_when_all_present_and_complete() {
        let audit = audit_with(&[
            (PayloadRole::Source, None),
            (PayloadRole::Plan, None),
            (PayloadRole::State, Some("complete")),
            (PayloadRole::Session, None),
            (PayloadRole::Validation, Some("pass")),
            (PayloadRole::Review, Some("approve")),
        ]);
        let result = evaluate_audit(Some(&audit));
        assert_eq!(result.state, RecordState::RecordReadyForClose);
        assert_eq!(result.recommended, RecommendedAction::RunCloseReady);
        assert!(result.missing_for_closeout.is_empty());
    }

    #[test]
    fn tracking_fsm_closed_when_closeout_present() {
        let audit = audit_with(&[(PayloadRole::Closeout, Some("complete"))]);
        let result = evaluate_audit(Some(&audit));
        assert_eq!(result.state, RecordState::RecordClosed);
        assert_eq!(result.recommended, RecommendedAction::NoOp);
    }
}

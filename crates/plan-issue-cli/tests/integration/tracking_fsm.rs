//! FSM evaluation integration coverage (Task 4.2).

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;

use plan_issue_cli::lifecycle_record::{
    BodySections, LifecycleEvidence, PayloadProfile, PayloadRole, RecordAudit,
};
use plan_issue_cli::tracking::fsm::{
    self, RecommendedAction, RecordState,
};

fn evidence_for(role: PayloadRole, status: Option<&str>) -> LifecycleEvidence {
    LifecycleEvidence {
        role,
        profile: PayloadProfile::Tracking,
        url: Some("https://example.com".to_string()),
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
fn tracking_fsm_handles_full_lifecycle_progression() {
    // RECORD_UNOPENED → open
    let unopened = fsm::evaluate_audit(None);
    assert_eq!(unopened.state, RecordState::RecordUnopened);
    assert_eq!(unopened.recommended, RecommendedAction::OpenRecord);

    // RECORD_OPEN_INITIAL → checkpoint
    let initial = fsm::evaluate_audit(Some(&audit_with(&[
        (PayloadRole::Source, None),
        (PayloadRole::Plan, None),
        (PayloadRole::State, Some("in-progress")),
    ])));
    assert_eq!(initial.state, RecordState::RecordOpenInitial);

    // RECORD_OPEN_ACTIVE → record_validation
    let active = fsm::evaluate_audit(Some(&audit_with(&[
        (PayloadRole::Source, None),
        (PayloadRole::Plan, None),
        (PayloadRole::State, Some("in-progress")),
        (PayloadRole::Session, None),
    ])));
    assert_eq!(active.state, RecordState::RecordOpenActive);
    assert_eq!(active.recommended, RecommendedAction::RecordValidation);

    // RECORD_VALIDATING → record_review
    let validating = fsm::evaluate_audit(Some(&audit_with(&[
        (PayloadRole::Source, None),
        (PayloadRole::Plan, None),
        (PayloadRole::State, Some("in-progress")),
        (PayloadRole::Session, None),
        (PayloadRole::Validation, Some("pass")),
    ])));
    assert_eq!(validating.state, RecordState::RecordValidating);
    assert_eq!(validating.recommended, RecommendedAction::RecordReview);

    // RECORD_REVIEWED (state still in-progress) → checkpoint_progress
    let reviewed = fsm::evaluate_audit(Some(&audit_with(&[
        (PayloadRole::Source, None),
        (PayloadRole::Plan, None),
        (PayloadRole::State, Some("in-progress")),
        (PayloadRole::Session, None),
        (PayloadRole::Validation, Some("pass")),
        (PayloadRole::Review, Some("approve")),
    ])));
    assert_eq!(reviewed.state, RecordState::RecordReviewed);

    // RECORD_READY_FOR_CLOSE → run_close_ready
    let ready = fsm::evaluate_audit(Some(&audit_with(&[
        (PayloadRole::Source, None),
        (PayloadRole::Plan, None),
        (PayloadRole::State, Some("complete")),
        (PayloadRole::Session, None),
        (PayloadRole::Validation, Some("pass")),
        (PayloadRole::Review, Some("approve")),
    ])));
    assert_eq!(ready.state, RecordState::RecordReadyForClose);
    assert_eq!(ready.recommended, RecommendedAction::RunCloseReady);

    // RECORD_CLOSED → no_op
    let closed = fsm::evaluate_audit(Some(&audit_with(&[(
        PayloadRole::Closeout,
        Some("complete"),
    )])));
    assert_eq!(closed.state, RecordState::RecordClosed);
    assert_eq!(closed.recommended, RecommendedAction::NoOp);
}

#[test]
fn tracking_fsm_blocked_state_maps_to_state_payload_status() {
    let audit = audit_with(&[
        (PayloadRole::Source, None),
        (PayloadRole::Plan, None),
        (PayloadRole::State, Some("blocked")),
    ]);
    let result = fsm::evaluate_audit(Some(&audit));
    assert_eq!(result.state, RecordState::RecordBlocked);
    assert_eq!(result.recommended, RecommendedAction::ResolveBlocker);
    assert!(result.blocked_reason.is_some());
}

#[test]
fn tracking_fsm_missing_roles_produce_precise_next_actions() {
    // Only source/plan → recommend attach_initial_state
    let no_state = fsm::evaluate_audit(Some(&audit_with(&[
        (PayloadRole::Source, None),
        (PayloadRole::Plan, None),
    ])));
    assert_eq!(no_state.recommended, RecommendedAction::AttachInitialState);
    assert!(no_state.missing_for_closeout.iter().any(|s| s == "state"));

    // No issue → recommend open_record
    let unopened = fsm::evaluate_audit(None);
    assert_eq!(unopened.recommended, RecommendedAction::OpenRecord);
}

#[test]
fn tracking_fsm_safe_transitions_are_stable_per_state() {
    let active = fsm::evaluate_audit(Some(&audit_with(&[
        (PayloadRole::Source, None),
        (PayloadRole::Plan, None),
        (PayloadRole::State, Some("in-progress")),
        (PayloadRole::Session, None),
    ])));
    assert!(active.safe_transitions.iter().any(|s| s == "record_validation"));
    assert!(active.safe_transitions.iter().any(|s| s == "checkpoint_progress"));
}

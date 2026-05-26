//! Reconciliation integration coverage (Task 4.2).

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;

use plan_issue_cli::lifecycle_record::{
    BodySections, LifecycleEvidence, PayloadProfile, PayloadRole, RecordAudit,
};
use plan_issue_cli::tracking::fsm::{RecommendedAction, RecordState};
use plan_issue_cli::tracking::reconcile::{self, ReconciliationWarningKind};
use plan_issue_cli::tracking::run_state::{ExecutionRun, RunPhase, ValidationSummary};

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

fn fixture_run(phase: RunPhase) -> ExecutionRun {
    ExecutionRun::new(
        "run-1",
        "owner/repo",
        123,
        "tracking",
        phase,
        "2026-05-26T00:00:00Z",
    )
}

#[test]
fn tracking_reconcile_provider_evidence_wins_when_issue_closed() {
    let audit = audit_with(&[(PayloadRole::Closeout, Some("complete"))]);
    // Local run state still thinks we are implementing.
    let run = fixture_run(RunPhase::Implementing);
    let report = reconcile::reconcile(Some(&audit), Some(&run));
    assert_eq!(report.state, RecordState::RecordClosed);
    assert!(report.is_stale(), "expected stale warning: {report:?}");
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.kind == ReconciliationWarningKind::LocalRunStateStale),
        "warnings: {:?}",
        report.warnings
    );
}

#[test]
fn tracking_reconcile_emits_run_state_ahead_for_unposted_validation() {
    let audit = audit_with(&[
        (PayloadRole::Source, None),
        (PayloadRole::Plan, None),
        (PayloadRole::State, Some("in-progress")),
    ]);
    let mut run = fixture_run(RunPhase::Validating);
    run.validation = Some(ValidationSummary {
        overall: "pass".to_string(),
        commands: Vec::new(),
        waiver: None,
        evidence_path: None,
    });
    let report = reconcile::reconcile(Some(&audit), Some(&run));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.code == "run-state-validation-not-posted")
    );
}

#[test]
fn tracking_reconcile_missing_role_warnings_are_role_specific() {
    let audit = audit_with(&[
        (PayloadRole::Source, None),
        (PayloadRole::Plan, None),
        (PayloadRole::State, Some("in-progress")),
    ]);
    let report = reconcile::reconcile(Some(&audit), None);
    let missing_codes: Vec<String> = report
        .warnings
        .iter()
        .filter(|w| matches!(w.kind, ReconciliationWarningKind::LifecycleRoleMissing))
        .map(|w| w.code.clone())
        .collect();
    assert!(missing_codes.iter().any(|c| c == "session-missing"));
    assert!(missing_codes.iter().any(|c| c == "validation-missing"));
    assert!(missing_codes.iter().any(|c| c == "review-missing"));
}

#[test]
fn tracking_reconcile_safe_transitions_match_fsm() {
    let audit = audit_with(&[
        (PayloadRole::Source, None),
        (PayloadRole::Plan, None),
        (PayloadRole::State, Some("in-progress")),
        (PayloadRole::Session, None),
    ]);
    let report = reconcile::reconcile(Some(&audit), None);
    assert!(
        report
            .safe_transitions
            .iter()
            .any(|s| s == "record_validation")
    );
    assert_eq!(
        report.recommended_action,
        RecommendedAction::RecordValidation
    );
}

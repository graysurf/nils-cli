//! Provider-evidence reconciliation for tracking runs.
//!
//! Combines record audit + plan bundle + run state + events into a
//! [`Reconciled`] view that the [`super::fsm`] evaluation builds on.

use serde::{Deserialize, Serialize};

use crate::lifecycle_record::RecordAudit;
use crate::tracking::fsm::{self, FsmEvaluation, RecommendedAction, RecordState};
use crate::tracking::run_state::ExecutionRun;

/// Reconciled view used by the `tracking status` command and downstream
/// checkpoint/close-ready logic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reconciled {
    pub state: RecordState,
    pub recommended_action: RecommendedAction,
    pub warnings: Vec<ReconciliationWarning>,
    pub safe_transitions: Vec<String>,
    pub missing_for_closeout: Vec<String>,
    pub blocked_reason: Option<String>,
}

impl Reconciled {
    pub fn is_stale(&self) -> bool {
        self.warnings
            .iter()
            .any(|w| matches!(w.kind, ReconciliationWarningKind::LocalRunStateStale))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationWarning {
    pub kind: ReconciliationWarningKind,
    pub code: String,
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
    /// Run state names artifacts the provider has not seen yet.
    RunStateAheadOfIssue,
}

/// Reconcile provider audit, optional run state, and optional plan-bundle
/// status into a [`Reconciled`] view.
pub fn reconcile(audit: Option<&RecordAudit>, run: Option<&ExecutionRun>) -> Reconciled {
    let evaluation: FsmEvaluation = fsm::evaluate_audit(audit);

    let mut warnings = Vec::new();
    if let (Some(audit), Some(run)) = (audit, run) {
        if fsm::run_state_is_stale(Some(audit), run) {
            warnings.push(ReconciliationWarning {
                kind: ReconciliationWarningKind::LocalRunStateStale,
                code: "run-state-stale".to_string(),
                message:
                    "provider issue lifecycle evidence is newer than local run state; refuse live mutation until reconciled".to_string(),
            });
        }
        if run.validation.as_ref().is_some_and(|v| v.overall.eq_ignore_ascii_case("pass"))
            && !audit.evidence.contains_key("validation")
        {
            warnings.push(ReconciliationWarning {
                kind: ReconciliationWarningKind::RunStateAheadOfIssue,
                code: "run-state-validation-not-posted".to_string(),
                message:
                    "run state has passing validation but issue has no validation lifecycle comment"
                        .to_string(),
            });
        }
    }

    // Surface missing lifecycle roles as warnings so callers can act on each.
    for role in fsm::missing_payload_roles(audit) {
        warnings.push(ReconciliationWarning {
            kind: ReconciliationWarningKind::LifecycleRoleMissing,
            code: format!("{}-missing", role.as_str()),
            message: format!("lifecycle role `{}` has no current evidence", role.as_str()),
        });
    }

    Reconciled {
        state: evaluation.state,
        recommended_action: evaluation.recommended,
        warnings,
        safe_transitions: evaluation.safe_transitions.clone(),
        missing_for_closeout: evaluation.missing_for_closeout.clone(),
        blocked_reason: evaluation.blocked_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle_record::{
        BodySections, LifecycleEvidence, PayloadProfile, PayloadRole, RecordAudit,
    };
    use crate::tracking::run_state::{ExecutionRun, RunPhase, ValidationSummary};
    use std::collections::BTreeMap;

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
    fn tracking_reconcile_emits_state_and_recommended_action() {
        let audit = audit_with(&[
            (PayloadRole::Source, None),
            (PayloadRole::Plan, None),
            (PayloadRole::State, Some("in-progress")),
            (PayloadRole::Session, None),
        ]);
        let result = reconcile(Some(&audit), None);
        assert_eq!(result.state, RecordState::RecordOpenActive);
        assert_eq!(result.recommended_action, RecommendedAction::RecordValidation);
    }

    #[test]
    fn tracking_reconcile_reports_stale_when_issue_closed_but_run_active() {
        let audit = audit_with(&[(PayloadRole::Closeout, Some("complete"))]);
        let run = fixture_run(RunPhase::Implementing);
        let result = reconcile(Some(&audit), Some(&run));
        assert!(result.is_stale(), "expected stale warning: {result:?}");
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.code == "run-state-stale")
        );
    }

    #[test]
    fn tracking_reconcile_flags_run_state_ahead_when_validation_passed_but_not_posted() {
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
        let result = reconcile(Some(&audit), Some(&run));
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.code == "run-state-validation-not-posted"),
            "warnings: {:?}",
            result.warnings
        );
    }

    #[test]
    fn tracking_reconcile_surfaces_missing_role_warnings() {
        let audit = audit_with(&[
            (PayloadRole::Source, None),
            (PayloadRole::Plan, None),
            (PayloadRole::State, Some("in-progress")),
        ]);
        let result = reconcile(Some(&audit), None);
        let missing_codes: Vec<_> = result
            .warnings
            .iter()
            .filter(|w| matches!(w.kind, ReconciliationWarningKind::LifecycleRoleMissing))
            .map(|w| w.code.clone())
            .collect();
        assert!(missing_codes.contains(&"session-missing".to_string()));
        assert!(missing_codes.contains(&"validation-missing".to_string()));
        assert!(missing_codes.contains(&"review-missing".to_string()));
    }
}

//! Typed runtime-fault classification and the three bounded interaction lanes.
//!
//! A capability that cannot prove itself must not admit arbitrary mutation. That
//! posture is correct and is preserved here. What it must not do is take the
//! whole session with it: in `sympoies/nils-cli#1409` a coordination failure
//! blocked plain `UserPromptSubmit` text, so the user could not ask what was
//! wrong or request the repair, and `Stop` re-entered the same failing gate until
//! the provider force-terminated the turn.
//!
//! The fix is lane separation rather than a weaker gate:
//!
//! * [`Lane::Conversation`] — prompts, session start, and clarification. A
//!   runtime fault degrades this lane to read-only with one primary diagnosis and
//!   one safe next action. No new authority is acquired.
//! * [`Lane::Mutation`] — tool admission. Unchanged: a fault that cannot be
//!   proven still fails closed.
//! * [`Lane::Terminal`] — the Stop family. A fault, or provider re-entry
//!   metadata, produces one deterministic terminal result. Recovery is named
//!   only when the observed coordination outcome requires it; unrelated Stop
//!   blocks do not manufacture claim, operation, or gating assertions.

use nils_common::coordination_projection::CoordinationMode;
use nils_common::observation::Severity;

use crate::error::HookError;
use crate::model::{
    DECISION_VERSION, DecisionAction, DecisionReason, LoadedPolicy, NormalizedDecision,
    NormalizedRequest,
};
use crate::observe::{
    RECOVERY_BROKER_RECONCILE, RECOVERY_BROKER_STATUS, RECOVERY_DIAGNOSE, RECOVERY_HOOK_DOCTOR,
    Record, disposition_for,
};

/// Synthetic rule id for a decision the degradation lane produced rather than a
/// policy rule. It is not addressable from a policy bundle or user config.
const DEGRADATION_RULE_ID: &str = "runtime.degradation";

/// Stable classification for the conversation lane's read-only degradation.
pub(crate) const CONVERSATION_DEGRADED: &str = "coordination-degraded-read-only";
/// Stable classification for a terminal Stop degraded by a subsystem fault.
pub(crate) const STOP_RECONCILIATION_REQUIRED: &str = "coordination-stop-reconciliation-required";
/// Stable classification for a terminal exit forced by provider Stop re-entry.
pub(crate) const STOP_REENTRY_PENDING: &str = "stop-reentry-reconciliation-pending";
/// Stable classification for a terminal exit whose coordination outcome is clean.
pub(crate) const STOP_REENTRY_TERMINAL: &str = "stop-reentry-terminal-exit";
/// Stable disposition recorded for a retained-lease terminal exit.
const RECONCILIATION_PENDING: &str = "reconciliation-pending";
/// Stable disposition for a re-entry exit that prescribes no recovery mutation.
const TERMINAL_EXIT: &str = "terminal-exit";

/// Typed result of the coordination stage that preceded Stop re-entry.
///
/// Keep this separate from normalized decision reasons: those strings are a
/// presentation contract and cannot distinguish a skipped transaction from a
/// clean one or an unavailable result from a reported pending operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopCoordinationOutcome {
    /// No coordination transaction ran for this evaluation.
    NotRun,
    /// The transaction ran and did not report a pending operation.
    Clean { mode: Option<CoordinationMode> },
    /// The transaction ran and explicitly reported a pending operation.
    Pending { mode: Option<CoordinationMode> },
    /// The transaction was selected but its result could not be observed.
    Unavailable { mode: Option<CoordinationMode> },
}

/// Which interaction lane a provider event belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lane {
    /// Text, status, and clarification. Degradable to read-only.
    Conversation,
    /// State-changing admission. Never degraded.
    Mutation,
    /// The Stop family, where a repeated block becomes a provider loop.
    Terminal,
}

/// Classify a canonical provider event into its interaction lane.
///
/// Events that carry no admission decision at all (notifications, compaction,
/// post-tool reporting) stay in the mutation lane deliberately: they are not the
/// deadlock, and widening the degraded set beyond the proven failure would trade
/// away posture for no liveness gain.
pub(crate) fn lane(event: &str) -> Lane {
    match event {
        "UserPromptSubmit" | "SessionStart" => Lane::Conversation,
        "Stop" | "StopFailure" => Lane::Terminal,
        _ => Lane::Mutation,
    }
}

/// A typed, recoverable control-plane fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeFault {
    /// Stable classification code.
    pub(crate) code: &'static str,
    /// One-sentence primary diagnosis.
    summary: &'static str,
    /// One bounded safe next action.
    pub(crate) recovery: &'static str,
}

/// Classify a dispatch error into a recoverable runtime fault.
///
/// Only faults whose recovery path is known are degradable. An unrecognized
/// error keeps the existing fail-closed handling rather than being degraded on a
/// guess.
pub(crate) fn classify_fault(error: &HookError) -> Option<RuntimeFault> {
    match error.code.as_str() {
        "coordination-unavailable" => Some(RuntimeFault {
            code: "coordination-unavailable",
            summary: "The session coordination store is unavailable.",
            recovery: RECOVERY_BROKER_STATUS,
        }),
        "coordination-untrusted" => Some(RuntimeFault {
            code: "coordination-untrusted",
            summary: "The session coordination store ownership or mode is untrusted.",
            recovery: RECOVERY_DIAGNOSE,
        }),
        "coordination-invalid" => Some(RuntimeFault {
            code: "coordination-invalid",
            summary: "The session coordination projection is invalid.",
            recovery: RECOVERY_BROKER_STATUS,
        }),
        RUNTIME_VERSION_SKEW => Some(RuntimeFault {
            code: RUNTIME_VERSION_SKEW,
            summary: "This runtime and the live coordination broker are different releases.",
            recovery: RECOVERY_BROKER_RECONCILE,
        }),
        "activity-helper-unresolvable" => Some(RuntimeFault {
            code: "activity-helper-unresolvable",
            summary: "The agent-session activity helper could not be resolved or executed.",
            recovery: RECOVERY_HOOK_DOCTOR,
        }),
        _ => None,
    }
}

/// Stable classification for a hook/broker release boundary.
pub(crate) const RUNTIME_VERSION_SKEW: &str = "runtime-version-skew";

/// Build a degraded decision for a dispatch that cannot evaluate its
/// coordination-backed rules, or return `None` to keep the fail-closed path.
///
/// The returned decision carries the fault code first, so diagnostics keep the
/// precise cause, and a single human sentence that states one diagnosis plus one
/// action instead of a comma-separated list of every selected policy.
pub(crate) fn degraded_decision(
    loaded: Option<&LoadedPolicy>,
    request: &NormalizedRequest,
    raw: &[u8],
    error: &HookError,
) -> Option<NormalizedDecision> {
    let fault = classify_fault(error)?;
    let (lane_code, context) = match lane(&request.event) {
        Lane::Mutation => return None,
        Lane::Conversation => (
            CONVERSATION_DEGRADED,
            format!(
                "agent-hook degraded lane: {} Conversation and read-only diagnosis stay available \
                 while every mutation remains blocked. Next: {}.",
                fault.summary, fault.recovery
            ),
        ),
        Lane::Terminal => (
            STOP_RECONCILIATION_REQUIRED,
            format!(
                "agent-hook terminal degradation: {} The turn may end; the claim and any active \
                 operation are retained for external reconciliation. Next: {}.",
                fault.summary, fault.recovery
            ),
        ),
    };
    let decision = NormalizedDecision {
        schema_version: DECISION_VERSION.to_string(),
        request_id: request.request_id.clone(),
        product: request.product,
        event: request.event.clone(),
        action: DecisionAction::Warn,
        reasons: vec![
            DecisionReason {
                rule_id: DEGRADATION_RULE_ID.to_string(),
                code: fault.code.to_string(),
                disposition: "warn".to_string(),
            },
            DecisionReason {
                rule_id: DEGRADATION_RULE_ID.to_string(),
                code: lane_code.to_string(),
                disposition: "warn".to_string(),
            },
        ],
        context: Some(context),
        replacement: None,
        shadow: Vec::new(),
        config_digest: loaded
            .map(|loaded| loaded.config_digest.clone())
            .unwrap_or_default(),
        policy_digest: loaded
            .map(|loaded| loaded.policy_digest.clone())
            .unwrap_or_default(),
        recovery_applied: false,
        provider_output: None,
    };
    Record::new("coordination", lane_code, Severity::Warn)
        .provider(request.product, &request.event)
        .disposition(disposition_for(DecisionAction::Warn))
        .correlate(request.product, raw)
        .recovery(fault.recovery)
        .emit();
    Some(decision)
}

/// Build a terminal exit for a re-entered Stop whose stage failed outright.
///
/// [`degraded_decision`] covers faults with a known recovery. This covers the
/// rest: once the provider reports Stop re-entry, rendering *any* denial repeats a
/// gate that already failed to converge, so the liveness rule has to hold even
/// for a fault this release does not recognize. The exact stage code is retained
/// as the primary reason so the unrecognized failure stays diagnosable, and no
/// claim, lease, or operation is altered.
pub(crate) fn terminal_exit_for_error(
    request: &NormalizedRequest,
    raw: &[u8],
    error: &HookError,
) -> Option<NormalizedDecision> {
    if lane(&request.event) != Lane::Terminal || request.stop_reentry != Some(true) {
        return None;
    }
    let decision = NormalizedDecision {
        schema_version: DECISION_VERSION.to_string(),
        request_id: request.request_id.clone(),
        product: request.product,
        event: request.event.clone(),
        action: DecisionAction::Warn,
        reasons: vec![
            DecisionReason {
                rule_id: DEGRADATION_RULE_ID.to_string(),
                code: error.code.clone(),
                disposition: "warn".to_string(),
            },
            DecisionReason {
                rule_id: DEGRADATION_RULE_ID.to_string(),
                code: STOP_REENTRY_TERMINAL.to_string(),
                disposition: "warn".to_string(),
            },
        ],
        context: Some(format!(
            "agent-hook terminal exit: this Stop hook already re-entered and the hook could not \
             complete ({}), so the turn ends without repeating the block. Coordination state \
             was not observed, so no retained claim, active operation, gate, or recovery \
             mutation is inferred. Next: {}.",
            error.code, RECOVERY_BROKER_STATUS
        )),
        replacement: None,
        shadow: Vec::new(),
        config_digest: String::new(),
        policy_digest: String::new(),
        recovery_applied: false,
        provider_output: None,
    };
    Record::new("stop", STOP_REENTRY_TERMINAL, Severity::Warn)
        .provider(request.product, &request.event)
        .disposition(TERMINAL_EXIT)
        .correlate(request.product, raw)
        .recovery(RECOVERY_BROKER_STATUS)
        .emit();
    Some(decision)
}

/// Turn a blocking terminal decision into a deterministic terminal exit when the
/// provider reports that it is already re-entering its Stop hook.
///
/// Re-entry proves the previous Stop block did not change anything the gate is
/// waiting on, so blocking again cannot converge — it only consumes the
/// provider's consecutive-block budget until the turn is force-terminated.
/// Every original reason is retained and no coordination state is changed. A
/// broker reconciliation is prescribed only when the typed coordination result
/// explicitly reports a pending operation; clean, skipped, and unavailable
/// outcomes do not manufacture a retained-state or mutation-gate claim.
pub(crate) fn apply_stop_reentry(
    request: &NormalizedRequest,
    raw: &[u8],
    mut decision: NormalizedDecision,
    coordination: StopCoordinationOutcome,
) -> NormalizedDecision {
    if decision.action != DecisionAction::Block
        || lane(&request.event) != Lane::Terminal
        || request.stop_reentry != Some(true)
    {
        return decision;
    }
    decision.action = DecisionAction::Warn;
    // A block-shaped provider payload would re-render the block we just resolved.
    decision.provider_output = None;
    let reconciliation_pending = matches!(coordination, StopCoordinationOutcome::Pending { .. });
    let reentry_code = if reconciliation_pending {
        STOP_REENTRY_PENDING
    } else {
        STOP_REENTRY_TERMINAL
    };
    decision.reasons.push(DecisionReason {
        rule_id: DEGRADATION_RULE_ID.to_string(),
        code: reentry_code.to_string(),
        disposition: "warn".to_string(),
    });
    let context = if reconciliation_pending {
        format!(
            "agent-hook terminal exit: this Stop hook already re-entered, so the turn ends with \
             reconciliation pending as reported by the coordination transaction. This exit \
             does not change coordination state. {} Next: {RECOVERY_BROKER_RECONCILE}.",
            coordination_context(coordination)
        )
    } else {
        format!(
            "agent-hook terminal exit: this Stop hook already re-entered, so the turn ends \
             without repeating the block. No recovery mutation is prescribed. {} The original \
             Stop reason remains visible for the next turn.",
            coordination_context(coordination)
        )
    };
    decision.context = Some(match decision.context.take() {
        Some(existing) => format!("{existing}\n{context}"),
        None => context,
    });
    let record = Record::new("stop", reentry_code, Severity::Warn)
        .provider(request.product, &request.event)
        .disposition(if reconciliation_pending {
            RECONCILIATION_PENDING
        } else {
            TERMINAL_EXIT
        })
        .correlate(request.product, raw);
    match coordination {
        StopCoordinationOutcome::Pending { .. } => {
            record.recovery(RECOVERY_BROKER_RECONCILE).emit();
        }
        StopCoordinationOutcome::Unavailable { .. } => {
            record.recovery(RECOVERY_BROKER_STATUS).emit();
        }
        StopCoordinationOutcome::NotRun | StopCoordinationOutcome::Clean { .. } => {
            record.emit();
        }
    }
    decision
}

fn coordination_context(outcome: StopCoordinationOutcome) -> &'static str {
    match outcome {
        StopCoordinationOutcome::NotRun => {
            "No coordination transaction ran, so no coordination-state or gate claim is made."
        }
        StopCoordinationOutcome::Clean {
            mode: Some(CoordinationMode::Advisory),
        } => {
            "The clean transaction ran in advisory mode and reported no pending operation; no mutation gate is claimed."
        }
        StopCoordinationOutcome::Clean {
            mode: Some(CoordinationMode::Off),
        } => {
            "The clean transaction ran with coordination off and reported no pending operation; no mutation gate is claimed."
        }
        StopCoordinationOutcome::Clean {
            mode: Some(CoordinationMode::Enforce),
        } => "The clean enforce-mode transaction reported no pending operation.",
        StopCoordinationOutcome::Clean { mode: None } => {
            "The transaction reported no pending operation, but no effective coordination mode was observed; no gate claim is made."
        }
        StopCoordinationOutcome::Pending {
            mode: Some(CoordinationMode::Enforce),
        } => "Enforce-mode mutation remains gated by the reported pending transaction.",
        StopCoordinationOutcome::Pending {
            mode: Some(CoordinationMode::Advisory),
        } => {
            "The advisory transaction reported a pending operation, but this message does not claim an enforce-mode mutation gate."
        }
        StopCoordinationOutcome::Pending {
            mode: Some(CoordinationMode::Off),
        } => {
            "The transaction reported a pending operation while coordination was off, but this message does not claim a mutation gate."
        }
        StopCoordinationOutcome::Pending { mode: None } => {
            "The transaction reported a pending operation, but no effective coordination mode was observed; no gate claim is made."
        }
        StopCoordinationOutcome::Unavailable { .. } => {
            "The coordination result was unavailable, so no retained-state or gate claim is made; inspect agent-session broker status read-only."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn request(event: &str, stop_reentry: Option<bool>) -> NormalizedRequest {
        NormalizedRequest {
            schema_version: crate::model::REQUEST_VERSION.to_string(),
            request_id: "request:test".to_string(),
            product: crate::model::Product::Claude,
            event: event.to_string(),
            matcher: None,
            target_digest: String::new(),
            command_digest: String::new(),
            snapshot_digest: String::new(),
            worktree_fingerprint: None,
            semantic_conflict: None,
            stop_reentry,
            target_paths: Vec::new(),
            execution_path: None,
            binding_roots: Vec::new(),
            dsh_subject: None,
        }
    }

    fn blocked(event: &str) -> NormalizedDecision {
        NormalizedDecision {
            schema_version: DECISION_VERSION.to_string(),
            request_id: "request:test".to_string(),
            product: crate::model::Product::Claude,
            event: event.to_string(),
            action: DecisionAction::Block,
            reasons: vec![DecisionReason {
                rule_id: "runtime.stop-coordination".to_string(),
                code: "operation-uncertain".to_string(),
                disposition: "block".to_string(),
            }],
            context: None,
            replacement: None,
            shadow: Vec::new(),
            config_digest: "sha256:config".to_string(),
            policy_digest: "sha256:policy".to_string(),
            recovery_applied: false,
            provider_output: Some(serde_json::json!({"decision": "block"})),
        }
    }

    #[test]
    fn lanes_cover_the_deadlocked_events_and_nothing_else() {
        assert_eq!(lane("UserPromptSubmit"), Lane::Conversation);
        assert_eq!(lane("SessionStart"), Lane::Conversation);
        assert_eq!(lane("Stop"), Lane::Terminal);
        assert_eq!(lane("StopFailure"), Lane::Terminal);
        for event in [
            "PreToolUse",
            "PermissionRequest",
            "PostToolUse",
            "PostToolUseFailure",
            "PreCompact",
            "SubagentStop",
        ] {
            assert_eq!(lane(event), Lane::Mutation, "event={event}");
        }
    }

    #[test]
    fn an_unclassified_fault_is_never_degraded() {
        assert_eq!(
            classify_fault(&HookError::runtime("something-else", "unmapped")),
            None
        );
        assert_eq!(
            degraded_decision(
                None,
                &request("UserPromptSubmit", None),
                b"{}",
                &HookError::runtime("something-else", "unmapped")
            ),
            None
        );
    }

    #[test]
    fn the_mutation_lane_is_never_degraded_even_for_a_known_fault() {
        assert_eq!(
            degraded_decision(
                None,
                &request("PreToolUse", None),
                b"{}",
                &HookError::data("coordination-invalid", "invalid")
            ),
            None
        );
    }

    #[test]
    fn stop_reentry_only_resolves_a_block_on_the_terminal_lane() {
        // Not re-entering: the authoritative block stands.
        assert_eq!(
            apply_stop_reentry(
                &request("Stop", Some(false)),
                b"{}",
                blocked("Stop"),
                StopCoordinationOutcome::Pending { mode: None },
            )
            .action,
            DecisionAction::Block
        );
        assert_eq!(
            apply_stop_reentry(
                &request("Stop", None),
                b"{}",
                blocked("Stop"),
                StopCoordinationOutcome::Pending { mode: None },
            )
            .action,
            DecisionAction::Block
        );
        // Re-entry metadata on another lane must not resolve its block.
        assert_eq!(
            apply_stop_reentry(
                &request("PreToolUse", Some(true)),
                b"{}",
                blocked("PreToolUse"),
                StopCoordinationOutcome::Pending { mode: None },
            )
            .action,
            DecisionAction::Block
        );
    }

    #[test]
    fn a_reentered_stop_terminates_while_retaining_its_original_evidence() {
        let resolved = apply_stop_reentry(
            &request("Stop", Some(true)),
            b"{}",
            blocked("Stop"),
            StopCoordinationOutcome::Pending {
                mode: Some(CoordinationMode::Enforce),
            },
        );

        assert_eq!(resolved.action, DecisionAction::Warn);
        assert_eq!(
            resolved
                .reasons
                .iter()
                .map(|reason| reason.code.as_str())
                .collect::<Vec<_>>(),
            vec!["operation-uncertain", STOP_REENTRY_PENDING],
            "the original gating evidence must survive the terminal exit"
        );
        assert_eq!(
            resolved.provider_output, None,
            "a block-shaped provider payload must not re-render the resolved block"
        );
        let context = resolved.context.expect("terminal context");
        assert!(
            context.contains("reported by the coordination transaction"),
            "context={context}"
        );
        assert!(!context.contains("claim and any active operation"));
        assert!(!context.contains("mutation stays gated"));
        assert!(
            context.contains(RECOVERY_BROKER_RECONCILE),
            "context={context}"
        );
    }

    #[test]
    fn a_clean_advisory_reentry_prescribes_no_recovery_or_gate() {
        let mut decision = blocked("Stop");
        decision.reasons[0].rule_id = "runtime.stop-prerequisite".to_string();
        decision.reasons.push(DecisionReason {
            rule_id: "runtime.stop-coordination".to_string(),
            code: "coordination-admitted".to_string(),
            disposition: "allow".to_string(),
        });

        let resolved = apply_stop_reentry(
            &request("Stop", Some(true)),
            b"{}",
            decision,
            StopCoordinationOutcome::Clean {
                mode: Some(CoordinationMode::Advisory),
            },
        );

        assert_eq!(resolved.action, DecisionAction::Warn);
        assert_eq!(
            resolved.reasons.last().map(|reason| reason.code.as_str()),
            Some(STOP_REENTRY_TERMINAL)
        );
        let context = resolved.context.expect("terminal context");
        assert!(context.contains("advisory mode"), "{context}");
        assert!(context.contains("recovery mutation"), "{context}");
        assert!(!context.contains(RECOVERY_BROKER_RECONCILE));
    }

    #[test]
    fn a_degraded_prompt_states_one_diagnosis_and_one_action() {
        let decision = degraded_decision(
            None,
            &request("UserPromptSubmit", None),
            b"{}",
            &HookError::data(RUNTIME_VERSION_SKEW, "skew"),
        )
        .expect("conversation lane degrades");

        assert_eq!(decision.action, DecisionAction::Warn);
        assert_eq!(
            decision
                .reasons
                .iter()
                .map(|reason| reason.code.as_str())
                .collect::<Vec<_>>(),
            vec![RUNTIME_VERSION_SKEW, CONVERSATION_DEGRADED]
        );
        let context = decision.context.expect("degraded context");
        assert!(context.contains("read-only"), "context={context}");
        assert!(
            context.contains(RECOVERY_BROKER_RECONCILE),
            "context={context}"
        );
    }
}

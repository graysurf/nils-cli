use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::contract::{
    effective_mode_for_product, matcher_expression_matches, runtime_handler_filename,
};
use crate::error::HookError;
use crate::liveness;
use crate::model::{
    Capability, DECISION_VERSION, DecisionAction, DecisionReason, FailurePosture, LoadedPolicy,
    NormalizedDecision, NormalizedRequest, OperationEffectClass, Product, RuleMode,
    ShadowObservation, TimeoutPosture,
};

const MAX_AGGREGATE_CONTEXT: usize = 16 * 1024;
const MAX_REASONS: usize = 64;
const MAX_HANDLER_OUTPUT: usize = 256 * 1024;
const HANDLER_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_EXECUTABLE_CAPABILITIES: usize = 17;
const MAX_DISPATCH_CHILD_OUTPUT: usize = 512 * 1024;
const RULE_CHILD_DEADLINE: Duration = HANDLER_TIMEOUT;
const SESSION_COORDINATION_HANDLER: &str = "session-coordination-guard.py";
const TYPED_BOOTSTRAP_AUTHORIZATION_SCHEMA: &str =
    "runtime-kit.session-coordination-bootstrap-authorization.v1";
const TYPED_BOOTSTRAP_AUTHORIZATION_CODE: &str = "typed-main-agent-bootstrap-authorized";
pub(crate) const ACTIVITY_STOP_RECONCILIATION_REQUIRED: &str =
    "activity-stop-reconciliation-required";
const SESSION_COORDINATION_TIMEOUT: Duration = RULE_CHILD_DEADLINE;

#[derive(Debug)]
struct RuleOutcome {
    action: DecisionAction,
    code: String,
    context: Option<String>,
    replacement: Option<Value>,
    provider_output: Option<Value>,
}

#[derive(Debug)]
struct ExecutionBudget {
    children: usize,
    retained_output: usize,
}

#[derive(Debug)]
struct ExecutionBudgets {
    enforced: ExecutionBudget,
    shadow: ExecutionBudget,
}

#[derive(Debug)]
struct PreparedRule<'a> {
    rule: &'a crate::model::PolicyRule,
    mode: RuleMode,
    recovery: bool,
}

#[derive(Debug)]
pub struct PreparedEvaluation<'a> {
    rules: Vec<PreparedRule<'a>>,
    needs_coordination: bool,
    session_coordination: Option<&'a crate::model::PolicyRule>,
}

impl PreparedEvaluation<'_> {
    pub fn needs_coordination(&self) -> bool {
        self.needs_coordination
    }
}

impl ExecutionBudget {
    fn new() -> Self {
        Self {
            children: 0,
            retained_output: 0,
        }
    }

    fn reserve_child(&mut self) -> Result<(Duration, usize), HookError> {
        if self.children >= MAX_EXECUTABLE_CAPABILITIES {
            return Err(HookError::data(
                "dispatch-child-budget-exceeded",
                "dispatch executable capability count exceeds 17",
            ));
        }
        self.children += 1;
        Ok((
            HANDLER_TIMEOUT.min(RULE_CHILD_DEADLINE),
            MAX_DISPATCH_CHILD_OUTPUT.saturating_sub(self.retained_output),
        ))
    }

    fn retain_output(&mut self, bytes: usize) -> Result<(), HookError> {
        self.retained_output = self.retained_output.saturating_add(bytes);
        if self.retained_output > MAX_DISPATCH_CHILD_OUTPUT {
            return Err(HookError::data(
                "dispatch-output-budget-exceeded",
                "dispatch executable capability output exceeds 512 KiB",
            ));
        }
        Ok(())
    }
}

impl ExecutionBudgets {
    fn new() -> Self {
        Self {
            enforced: ExecutionBudget::new(),
            shadow: ExecutionBudget::new(),
        }
    }
}

pub fn prepare<'a>(
    loaded: &'a LoadedPolicy,
    request: &NormalizedRequest,
    all_shadow: bool,
    recovery_rules: &BTreeSet<String>,
) -> Result<PreparedEvaluation<'a>, HookError> {
    let mut selected = loaded
        .bundle
        .rules
        .iter()
        .filter(|rule| {
            rule.products.contains(&request.product)
                && rule.events.iter().any(|event| event == &request.event)
                && rule.matcher.as_deref().is_none_or(|matcher| {
                    request
                        .matcher
                        .as_deref()
                        .is_some_and(|candidate| matcher_expression_matches(matcher, candidate))
                })
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.id.cmp(&right.id))
    });

    let rules = selected
        .into_iter()
        .filter_map(|rule| {
            let mode = if all_shadow {
                if effective_mode_for_product(loaded, request.product, rule) == RuleMode::Disabled {
                    RuleMode::Disabled
                } else {
                    RuleMode::Shadow
                }
            } else {
                effective_mode_for_product(loaded, request.product, rule)
            };
            (mode != RuleMode::Disabled).then(|| PreparedRule {
                rule,
                mode,
                recovery: recovery_rules.contains(&rule.id)
                    && !matches!(rule.capability, Capability::SessionCoordination { .. }),
            })
        })
        .collect::<Vec<_>>();

    let mut session_coordination = rules
        .iter()
        .filter(|prepared| prepared.mode == RuleMode::Enforce)
        .filter(|prepared| {
            matches!(
                prepared.rule.capability,
                Capability::SessionCoordination { .. }
            )
        })
        .map(|prepared| prepared.rule);
    let session_coordination_rule = session_coordination.next();
    if session_coordination.next().is_some() {
        return Err(HookError::data(
            "coordination-capability-ambiguous",
            "one dispatch may select at most one session coordination rule",
        ));
    }

    let executable_count = rules
        .iter()
        .filter(|prepared| !prepared.recovery)
        .filter(|prepared| match prepared.mode {
            RuleMode::Enforce => matches!(
                prepared.rule.capability,
                Capability::SessionActivity { .. }
                    | Capability::ExecutionReadOnly { .. }
                    | Capability::RuntimeKitHandler { .. }
            ),
            RuleMode::Shadow => {
                matches!(
                    prepared.rule.capability,
                    Capability::ExecutionReadOnly { .. }
                )
            }
            RuleMode::Disabled => false,
        })
        .count();
    if executable_count > MAX_EXECUTABLE_CAPABILITIES {
        return Err(HookError::data(
            "dispatch-child-budget-exceeded",
            "dispatch executable capability count exceeds 17",
        ));
    }

    let reason_count = rules
        .iter()
        .filter(|prepared| prepared.mode == RuleMode::Enforce)
        .count();
    if reason_count > MAX_REASONS {
        return Err(HookError::data(
            "decision-reason-limit",
            "aggregate decision exceeds the reason limit",
        ));
    }

    let needs_coordination = rules.iter().any(|prepared| {
        prepared.mode == RuleMode::Enforce
            && !prepared.recovery
            && matches!(
                prepared.rule.capability,
                Capability::SemanticConflict { .. }
                    | Capability::OwnerLiveness { .. }
                    | Capability::SessionCoordination { .. }
            )
    });

    Ok(PreparedEvaluation {
        rules,
        needs_coordination,
        session_coordination: session_coordination_rule,
    })
}

pub fn evaluate(
    loaded: &LoadedPolicy,
    request: &mut NormalizedRequest,
    raw: &[u8],
    prepared: &PreparedEvaluation<'_>,
    coordination: Option<&liveness::Snapshot>,
    coordination_mode_override: Option<nils_common::coordination_projection::CoordinationMode>,
) -> Result<NormalizedDecision, HookError> {
    evaluate_with_io(
        loaded,
        request,
        raw,
        prepared,
        coordination,
        coordination_mode_override,
        liveness::system_io(),
    )
}

fn evaluate_with_io(
    loaded: &LoadedPolicy,
    request: &mut NormalizedRequest,
    raw: &[u8],
    prepared: &PreparedEvaluation<'_>,
    coordination: Option<&liveness::Snapshot>,
    coordination_mode_override: Option<nils_common::coordination_projection::CoordinationMode>,
    liveness_io: &dyn liveness::LivenessIo,
) -> Result<NormalizedDecision, HookError> {
    // Terminal correlation is evidence maintenance, not an admission gate.
    let _ = crate::degraded::complete_terminal(raw, request);
    let liveness = prepared.needs_coordination.then(|| {
        liveness::DispatchProjection::new(coordination, coordination_mode_override, liveness_io)
    });
    request.semantic_conflict = coordination.map(|_| {
        liveness::derive_semantic_conflict(
            request,
            liveness
                .as_ref()
                .expect("coordination snapshot requires liveness projection"),
        )
    });

    let mut enforced = Vec::new();
    let mut shadow = Vec::new();
    let mut recovery_applied = false;
    let mut execution_budgets = ExecutionBudgets::new();
    let operation_effect = if prepared.rules.iter().any(|prepared_rule| {
        prepared_rule.mode == RuleMode::Enforce
            && prepared_rule.rule.timeout_posture == TimeoutPosture::EffectGated
    }) {
        crate::effect::classify(raw, request)
    } else {
        OperationEffectClass::Unknown
    };
    let mut read_only_bypassed_rule_ids = BTreeSet::new();
    for prepared_rule in &prepared.rules {
        let rule = prepared_rule.rule;
        if prepared_rule.mode == RuleMode::Shadow {
            let outcome = evaluate_shadow(
                &rule.capability,
                request,
                raw,
                &mut execution_budgets.shadow,
            );
            shadow.push(ShadowObservation {
                rule_id: rule.id.clone(),
                action: outcome.action,
                code: outcome.code,
            });
            continue;
        }
        if prepared_rule.recovery {
            recovery_applied = true;
            enforced.push((
                rule.id.clone(),
                RuleOutcome {
                    action: DecisionAction::Allow,
                    code: "recovery-exact-bypass".to_string(),
                    context: None,
                    replacement: None,
                    provider_output: None,
                },
            ));
            continue;
        }
        if matches!(rule.capability, Capability::SessionCoordination { .. }) {
            continue;
        }
        if let Capability::ExecutionReadOnly {
            reason_code,
            fallback_handler_id: Some(handler_id),
        } = &rule.capability
        {
            let outcome = match evaluate_read_only(
                request,
                raw,
                &mut execution_budgets.enforced,
                reason_code,
            ) {
                Ok(outcome) => outcome,
                Err(error) if error.code == "capability-timeout" => timeout_outcome(
                    loaded,
                    request,
                    rule,
                    operation_effect,
                    RULE_CHILD_DEADLINE,
                    raw,
                ),
                Err(error) => simple(DecisionAction::Block, &error.code),
            };
            if outcome.action == DecisionAction::Allow {
                let paired_rule = prepared
                    .rules
                    .iter()
                    .find(|candidate| {
                        candidate.mode == RuleMode::Enforce
                            && candidate.rule.priority > rule.priority
                            && candidate.rule.matcher == rule.matcher
                            && matches!(
                                &candidate.rule.capability,
                                Capability::RuntimeKitHandler {
                                    handler_id: candidate_handler_id
                                } if candidate_handler_id == handler_id
                            )
                    })
                    .expect("validated read-only fallback pair must be selected");
                read_only_bypassed_rule_ids.insert(paired_rule.rule.id.clone());
                enforced.push((rule.id.clone(), outcome));
            } else {
                enforced.push((
                    rule.id.clone(),
                    simple(
                        DecisionAction::Allow,
                        &format!("{}:project-dev-fallback", outcome.code),
                    ),
                ));
            }
            continue;
        }
        if matches!(rule.capability, Capability::RuntimeKitHandler { .. })
            && read_only_bypassed_rule_ids.contains(&rule.id)
        {
            enforced.push((
                rule.id.clone(),
                simple(DecisionAction::Allow, "read-only-capability-bypass"),
            ));
            continue;
        }
        let outcome = match evaluate_capability(
            &rule.capability,
            request,
            raw,
            &mut execution_budgets.enforced,
            liveness.as_ref(),
        ) {
            Ok(outcome) => outcome,
            Err(error) if error.code == "capability-timeout" => timeout_outcome(
                loaded,
                request,
                rule,
                operation_effect,
                RULE_CHILD_DEADLINE,
                raw,
            ),
            Err(error) if error.code.starts_with("dispatch-") => return Err(error),
            Err(_) => failure_outcome(rule.failure_posture, &rule.id),
        };
        enforced.push((rule.id.clone(), outcome));
    }

    let decision = aggregate(loaded, request, enforced, shadow, recovery_applied)?;
    let coordination_mode = liveness
        .as_ref()
        .and_then(liveness::DispatchProjection::mode);
    apply_session_coordination(
        Some(loaded),
        decision,
        request,
        raw,
        prepared.session_coordination,
        coordination_mode,
        operation_effect,
    )
}

fn exact_bootstrap_candidate_for_session_coordination(
    request: &NormalizedRequest,
    raw: &[u8],
    has_session_coordination: bool,
) -> bool {
    has_session_coordination
        && request.event == "PreToolUse"
        && request.matcher.as_deref() == Some("Bash")
        && crate::adapter::exact_main_agent_bootstrap_command(raw)
}

fn evaluate_shadow(
    capability: &Capability,
    request: &NormalizedRequest,
    raw: &[u8],
    execution_budget: &mut ExecutionBudget,
) -> RuleOutcome {
    match capability {
        Capability::Allow { reason_code } => simple(DecisionAction::Allow, reason_code),
        Capability::Warn { reason_code, .. } => simple(DecisionAction::Warn, reason_code),
        Capability::Block { reason_code, .. } => simple(DecisionAction::Block, reason_code),
        Capability::Context { reason_code, .. } => simple(DecisionAction::Context, reason_code),
        Capability::Transform { reason_code, .. } => simple(DecisionAction::Transform, reason_code),
        Capability::SemanticConflict { reason_code } => simple(DecisionAction::Warn, reason_code),
        Capability::OwnerLiveness { reason_code, .. } => simple(DecisionAction::Warn, reason_code),
        Capability::ExecutionReadOnly { reason_code, .. } => {
            evaluate_read_only(request, raw, execution_budget, reason_code)
                .unwrap_or_else(|error| simple(DecisionAction::Block, &error.code))
        }
        Capability::SessionActivity { .. }
        | Capability::SessionCoordination { .. }
        | Capability::RuntimeKitHandler { .. } => {
            simple(DecisionAction::Allow, "shadow-side-effect-skipped")
        }
    }
}

fn evaluate_read_only(
    request: &NormalizedRequest,
    raw: &[u8],
    execution_budget: &mut ExecutionBudget,
    reason_code: &str,
) -> Result<RuleOutcome, HookError> {
    let verified = crate::read_only::candidate(raw, request).and_then(|candidate| {
        let output = run_with_budget(candidate.descriptor_command(), &[], execution_budget)?;
        crate::read_only::verify_output(&candidate, &output)
    });
    verified.map(|()| simple(DecisionAction::Allow, reason_code))
}

fn evaluate_capability(
    capability: &Capability,
    request: &NormalizedRequest,
    raw: &[u8],
    execution_budget: &mut ExecutionBudget,
    liveness: Option<&liveness::DispatchProjection<'_, '_>>,
) -> Result<RuleOutcome, HookError> {
    Ok(match capability {
        Capability::Allow { reason_code } => simple(DecisionAction::Allow, reason_code),
        Capability::Warn {
            reason_code,
            message,
        } => RuleOutcome {
            action: DecisionAction::Warn,
            code: reason_code.clone(),
            context: Some(message.clone()),
            replacement: None,
            provider_output: None,
        },
        Capability::Block {
            reason_code,
            message,
        } => RuleOutcome {
            action: DecisionAction::Block,
            code: reason_code.clone(),
            context: Some(message.clone()),
            replacement: None,
            provider_output: None,
        },
        Capability::Context { reason_code, text } => RuleOutcome {
            action: DecisionAction::Context,
            code: reason_code.clone(),
            context: Some(text.clone()),
            replacement: None,
            provider_output: None,
        },
        Capability::Transform {
            reason_code,
            replacement,
        } => RuleOutcome {
            action: DecisionAction::Transform,
            code: reason_code.clone(),
            context: None,
            replacement: Some(replacement.clone()),
            provider_output: None,
        },
        Capability::SemanticConflict { reason_code } => simple(
            liveness::semantic_conflict_action(
                request.semantic_conflict,
                liveness.and_then(liveness::DispatchProjection::mode),
            ),
            reason_code,
        ),
        Capability::OwnerLiveness {
            reason_code: _,
            legacy_ttl_seconds,
        } => {
            let projection = liveness.ok_or_else(|| {
                HookError::runtime(
                    "coordination-unavailable",
                    "owner liveness projection is unavailable",
                )
            })?;
            let outcome = liveness::classify(request, *legacy_ttl_seconds, projection);
            simple(outcome.action, &outcome.reason_code)
        }
        Capability::SessionActivity { reason_code } => {
            match run_session_activity(request, raw, execution_budget) {
                Ok(()) => simple(DecisionAction::Allow, reason_code),
                Err(error) => {
                    match terminal_activity_failure_decision(capability, request.event.as_str()) {
                        Some((action, code)) => simple(action, code),
                        None => return Err(error),
                    }
                }
            }
        }
        Capability::SessionCoordination { .. } => {
            unreachable!("session coordination is evaluated after aggregate policy")
        }
        Capability::ExecutionReadOnly { reason_code, .. } => {
            match evaluate_read_only(request, raw, execution_budget, reason_code) {
                Ok(outcome) => outcome,
                Err(error) if error.code == "capability-timeout" => return Err(error),
                Err(error) => simple(DecisionAction::Block, &error.code),
            }
        }
        Capability::RuntimeKitHandler { handler_id } => {
            run_runtime_handler(request.product, handler_id, raw, execution_budget)?
        }
    })
}

pub(crate) fn terminal_activity_failure_decision(
    capability: &Capability,
    event: &str,
) -> Option<(DecisionAction, &'static str)> {
    (event == "Stop" && matches!(capability, Capability::SessionActivity { .. }))
        .then_some((DecisionAction::Warn, ACTIVITY_STOP_RECONCILIATION_REQUIRED))
}

pub fn apply_session_coordination(
    loaded: Option<&LoadedPolicy>,
    mut decision: NormalizedDecision,
    request: &NormalizedRequest,
    raw: &[u8],
    rule: Option<&crate::model::PolicyRule>,
    coordination_mode: Option<nils_common::coordination_projection::CoordinationMode>,
    operation_effect: OperationEffectClass,
) -> Result<NormalizedDecision, HookError> {
    let Some(rule) = rule else {
        return Ok(decision);
    };
    let typed_bootstrap_may_supersede_owner =
        exact_bootstrap_candidate_for_session_coordination(request, raw, true)
            && owner_active_foreign_is_only_block(loaded, &decision);
    if decision.action == DecisionAction::Block
        && !matches!(
            request.event.as_str(),
            "PostToolUse" | "PostToolUseFailure" | "Stop"
        )
        && !typed_bootstrap_may_supersede_owner
    {
        return Ok(decision);
    }
    let Capability::SessionCoordination { reason_code } = &rule.capability else {
        unreachable!("prepared coordination rule has the typed capability")
    };
    let outcome = match run_session_coordination(request.product, raw) {
        Ok(mut outcome) => {
            if outcome.code == SESSION_COORDINATION_HANDLER {
                outcome.code.clone_from(reason_code);
            }
            outcome
        }
        Err(error) if error.code == "capability-timeout" => coordination_timeout_outcome(
            loaded,
            request,
            rule,
            coordination_mode,
            operation_effect,
            raw,
        ),
        Err(_) => failure_outcome(rule.failure_posture, &rule.id),
    };
    if typed_bootstrap_may_supersede_owner
        && outcome.action == DecisionAction::Allow
        && outcome.code == TYPED_BOOTSTRAP_AUTHORIZATION_CODE
    {
        supersede_owner_active_foreign(loaded, &mut decision);
    }
    merge_coordination_outcome(&mut decision, &rule.id, outcome)?;
    Ok(decision)
}

fn owner_active_foreign_is_only_block(
    loaded: Option<&LoadedPolicy>,
    decision: &NormalizedDecision,
) -> bool {
    let Some(loaded) = loaded else {
        return false;
    };
    let mut owner_block = false;
    for reason in &decision.reasons {
        if reason.disposition == "transform" {
            return false;
        }
        if reason.disposition != "block" {
            continue;
        }
        let owner_rule = loaded.bundle.rules.iter().any(|rule| {
            rule.id == reason.rule_id && matches!(rule.capability, Capability::OwnerLiveness { .. })
        });
        if !owner_rule || reason.code != "owner-active-foreign" {
            return false;
        }
        owner_block = true;
    }
    owner_block
}

fn supersede_owner_active_foreign(
    loaded: Option<&LoadedPolicy>,
    decision: &mut NormalizedDecision,
) {
    let Some(loaded) = loaded else {
        return;
    };
    for reason in &mut decision.reasons {
        let owner_rule = loaded.bundle.rules.iter().any(|rule| {
            rule.id == reason.rule_id && matches!(rule.capability, Capability::OwnerLiveness { .. })
        });
        if owner_rule && reason.code == "owner-active-foreign" && reason.disposition == "block" {
            reason.code = "owner-liveness-superseded-by-typed-bootstrap".to_string();
            reason.disposition = "allow".to_string();
        }
    }
    decision.action = decision
        .reasons
        .iter()
        .map(|reason| match reason.disposition.as_str() {
            "allow" => DecisionAction::Allow,
            "warn" => DecisionAction::Warn,
            "context" => DecisionAction::Context,
            "transform" => DecisionAction::Transform,
            "block" => DecisionAction::Block,
            _ => DecisionAction::Block,
        })
        .max_by_key(|action| rank(*action))
        .unwrap_or(DecisionAction::Allow);
}

fn coordination_timeout_outcome(
    loaded: Option<&LoadedPolicy>,
    request: &NormalizedRequest,
    rule: &crate::model::PolicyRule,
    coordination_mode: Option<nils_common::coordination_projection::CoordinationMode>,
    operation_effect: OperationEffectClass,
    raw: &[u8],
) -> RuleOutcome {
    use nils_common::coordination_projection::CoordinationMode;

    let allowed = matches!(
        coordination_mode,
        Some(CoordinationMode::Advisory | CoordinationMode::Off)
    );
    let disposition = if allowed {
        "allow_with_warning"
    } else {
        "block"
    };
    let incident = loaded.map(|loaded| {
        crate::degraded::record_timeout(
            loaded,
            request,
            rule,
            operation_effect,
            disposition,
            SESSION_COORDINATION_TIMEOUT.as_millis() as u64,
            raw,
        )
    });
    if allowed {
        match incident {
            Some(Ok(incident_id)) => RuleOutcome {
                action: DecisionAction::Warn,
                code: format!("{}:capability-timeout-warn", rule.id),
                context: Some(format!(
                    "agent-hook degraded admission: {} timed out in advisory coordination. The operation was allowed. Continue the task and report incident {} in the final response.",
                    rule.id, incident_id
                )),
                replacement: None,
                provider_output: None,
            },
            _ => simple(
                DecisionAction::Block,
                &format!("{}:degraded-incident-write-failed", rule.id),
            ),
        }
    } else {
        let _ = incident;
        simple(
            DecisionAction::Block,
            &format!("{}:capability-timeout-closed", rule.id),
        )
    }
}

fn merge_coordination_outcome(
    decision: &mut NormalizedDecision,
    rule_id: &str,
    outcome: RuleOutcome,
) -> Result<(), HookError> {
    if decision.reasons.len() >= MAX_REASONS {
        return Err(HookError::data(
            "decision-reason-limit",
            "aggregate decision exceeds the reason limit",
        ));
    }
    if let Some(context) = outcome.context {
        let joined = match decision.context.take() {
            Some(existing) => format!("{existing}\n{context}"),
            None => context,
        };
        if joined.len() > MAX_AGGREGATE_CONTEXT {
            return Err(HookError::data(
                "decision-context-too-large",
                "aggregate decision context exceeds 16 KiB",
            ));
        }
        decision.context = Some(joined);
    }
    if rank(outcome.action) > rank(decision.action) {
        decision.action = outcome.action;
        decision.provider_output = outcome.provider_output;
    } else if rank(outcome.action) == rank(decision.action)
        && decision.provider_output.is_none()
        && outcome.provider_output.is_some()
    {
        decision.provider_output = outcome.provider_output;
    }
    decision.reasons.push(DecisionReason {
        rule_id: rule_id.to_string(),
        code: outcome.code,
        disposition: disposition(outcome.action).to_string(),
    });
    Ok(())
}

fn failure_outcome(posture: FailurePosture, rule_id: &str) -> RuleOutcome {
    match posture {
        FailurePosture::Open => simple(
            DecisionAction::Allow,
            &format!("{rule_id}:capability-failure-open"),
        ),
        FailurePosture::Warn => simple(
            DecisionAction::Warn,
            &format!("{rule_id}:capability-failure-warn"),
        ),
        FailurePosture::Closed => simple(
            DecisionAction::Block,
            &format!("{rule_id}:capability-failure-closed"),
        ),
    }
}

fn timeout_outcome(
    loaded: &LoadedPolicy,
    request: &NormalizedRequest,
    rule: &crate::model::PolicyRule,
    effect: OperationEffectClass,
    deadline: Duration,
    raw: &[u8],
) -> RuleOutcome {
    let (action, code, disposition, allowed) = match rule.timeout_posture {
        TimeoutPosture::Closed => (
            DecisionAction::Block,
            format!("{}:capability-timeout-closed", rule.id),
            "block",
            false,
        ),
        TimeoutPosture::Warn => (
            DecisionAction::Warn,
            format!("{}:capability-timeout-warn", rule.id),
            "allow_with_warning",
            true,
        ),
        TimeoutPosture::EffectGated if effect == OperationEffectClass::LocalReversible => (
            DecisionAction::Warn,
            format!("{}:capability-timeout-effect-gated", rule.id),
            "allow_with_warning",
            true,
        ),
        TimeoutPosture::EffectGated => (
            DecisionAction::Block,
            format!("{}:capability-timeout-effect-unknown", rule.id),
            "block",
            false,
        ),
    };
    let incident = crate::degraded::record_timeout(
        loaded,
        request,
        rule,
        effect,
        disposition,
        deadline.as_millis() as u64,
        raw,
    );
    if allowed {
        match incident {
            Ok(incident_id) => RuleOutcome {
                action,
                code,
                context: Some(format!(
                    "agent-hook degraded admission: {} timed out while evaluating a {} operation. The operation was allowed. Continue the task and report incident {} in the final response.",
                    rule.id,
                    effect.as_str().replace('_', "-"),
                    incident_id
                )),
                replacement: None,
                provider_output: None,
            },
            Err(_) => simple(
                DecisionAction::Block,
                &format!("{}:degraded-incident-write-failed", rule.id),
            ),
        }
    } else {
        let _ = incident;
        simple(action, &code)
    }
}

fn simple(action: DecisionAction, code: &str) -> RuleOutcome {
    RuleOutcome {
        action,
        code: code.to_string(),
        context: None,
        replacement: None,
        provider_output: None,
    }
}

fn aggregate(
    loaded: &LoadedPolicy,
    request: &NormalizedRequest,
    outcomes: Vec<(String, RuleOutcome)>,
    shadow: Vec<ShadowObservation>,
    recovery_applied: bool,
) -> Result<NormalizedDecision, HookError> {
    let mut action = DecisionAction::Allow;
    let mut reasons = Vec::new();
    let mut contexts = Vec::new();
    let mut replacement: Option<Value> = None;
    let mut provider_output = None;
    let mut transform_conflicted = false;
    for (rule_id, outcome) in outcomes {
        if reasons.len() >= MAX_REASONS {
            return Err(HookError::data(
                "decision-reason-limit",
                "aggregate decision exceeds the reason limit",
            ));
        }
        if outcome.action == DecisionAction::Transform {
            if let (Some(existing), Some(candidate)) = (&replacement, &outcome.replacement)
                && existing != candidate
            {
                action = DecisionAction::Block;
                reasons.push(DecisionReason {
                    rule_id,
                    code: "transform-conflict".to_string(),
                    disposition: "block".to_string(),
                });
                replacement = None;
                provider_output = None;
                transform_conflicted = true;
                continue;
            }
            if !transform_conflicted && replacement.is_none() {
                replacement = outcome.replacement.clone();
            }
        }
        if let Some(context) = outcome.context {
            contexts.push(context);
        }
        if rank(outcome.action) > rank(action) {
            action = outcome.action;
            provider_output = outcome.provider_output;
        } else if rank(outcome.action) == rank(action)
            && provider_output.is_none()
            && outcome.provider_output.is_some()
        {
            provider_output = outcome.provider_output;
        }
        reasons.push(DecisionReason {
            rule_id,
            code: outcome.code,
            disposition: disposition(outcome.action).to_string(),
        });
    }
    let context = if contexts.is_empty() {
        None
    } else {
        let joined = contexts.join("\n");
        if joined.len() > MAX_AGGREGATE_CONTEXT {
            return Err(HookError::data(
                "decision-context-too-large",
                "aggregate decision context exceeds 16 KiB",
            ));
        }
        Some(joined)
    };
    Ok(NormalizedDecision {
        schema_version: DECISION_VERSION.to_string(),
        request_id: request.request_id.clone(),
        product: request.product,
        event: request.event.clone(),
        action,
        reasons,
        context,
        replacement,
        shadow,
        config_digest: loaded.config_digest.clone(),
        policy_digest: loaded.policy_digest.clone(),
        recovery_applied,
        provider_output,
    })
}

fn rank(action: DecisionAction) -> u8 {
    match action {
        DecisionAction::Allow => 0,
        DecisionAction::Warn => 1,
        DecisionAction::Context => 2,
        DecisionAction::Transform => 3,
        DecisionAction::Block => 4,
    }
}

fn disposition(action: DecisionAction) -> &'static str {
    match action {
        DecisionAction::Allow => "allow",
        DecisionAction::Warn => "warn",
        DecisionAction::Context => "context",
        DecisionAction::Transform => "transform",
        DecisionAction::Block => "block",
    }
}

fn run_session_activity(
    request: &NormalizedRequest,
    raw: &[u8],
    execution_budget: &mut ExecutionBudget,
) -> Result<(), HookError> {
    let Some(session_id) = std::env::var("AGENT_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };
    let Some(runtime_id) = std::env::var("AGENT_SESSION_RUNTIME_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };
    let Some(event) = crate::adapter::normalize_activity_event(request, raw, &runtime_id)? else {
        return Ok(());
    };
    let binary = std::env::var_os("AGENT_SESSION_BIN").unwrap_or_else(|| "agent-session".into());
    let mut command = Command::new(binary);
    command
        .args(["activity", "event", "--stdin", &session_id])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let output = run_with_budget(command, &event, execution_budget)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(HookError::runtime(
            "session-activity-failed",
            "agent-session activity capability failed",
        ))
    }
}

fn run_runtime_handler(
    product: Product,
    handler_id: &str,
    raw: &[u8],
    execution_budget: &mut ExecutionBudget,
) -> Result<RuleOutcome, HookError> {
    let filename = runtime_handler_filename(handler_id).ok_or_else(|| {
        HookError::data(
            "handler-id-unsupported",
            "runtime-kit handler is not in the compiled v1 allowlist",
        )
    })?;
    let path = runtime_hook_root(product)?.join(filename);
    validate_handler(&path)?;
    let mut command = Command::new(&path);
    command
        .env("AGENT_RUNTIME_PRODUCT", product.as_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let output = run_with_budget(command, raw, execution_budget)?;
    if output.stdout.len() > MAX_HANDLER_OUTPUT {
        return Err(HookError::data(
            "handler-output-too-large",
            "runtime-kit handler output exceeds 256 KiB",
        ));
    }
    if !output.status.success() {
        return Err(HookError::runtime(
            "handler-failed",
            "runtime-kit handler returned failure",
        ));
    }
    handler_outcome(handler_id, &output.stdout)
}

fn run_session_coordination(product: Product, raw: &[u8]) -> Result<RuleOutcome, HookError> {
    let path = runtime_hook_root(product)?.join(SESSION_COORDINATION_HANDLER);
    validate_handler(&path)?;
    let mut command = Command::new(&path);
    command
        .env("AGENT_RUNTIME_PRODUCT", product.as_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let output = run_bounded(
        command,
        raw,
        SESSION_COORDINATION_TIMEOUT,
        MAX_HANDLER_OUTPUT,
        false,
    )?;
    if output.stdout.len() > MAX_HANDLER_OUTPUT {
        return Err(HookError::data(
            "handler-output-too-large",
            "session coordination handler output exceeds 256 KiB",
        ));
    }
    if !output.status.success() {
        return Err(HookError::runtime(
            "handler-failed",
            "session coordination handler returned failure",
        ));
    }
    session_coordination_outcome(&output.stdout)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TypedBootstrapAuthorization {
    schema_version: String,
    authorization: String,
}

fn session_coordination_outcome(stdout: &[u8]) -> Result<RuleOutcome, HookError> {
    if let Ok(value) = crate::strict_json::from_slice(stdout)
        && let Ok(authorization) = serde_json::from_value::<TypedBootstrapAuthorization>(value)
        && authorization.schema_version == TYPED_BOOTSTRAP_AUTHORIZATION_SCHEMA
        && authorization.authorization == TYPED_BOOTSTRAP_AUTHORIZATION_CODE
    {
        return Ok(simple(
            DecisionAction::Allow,
            TYPED_BOOTSTRAP_AUTHORIZATION_CODE,
        ));
    }
    handler_outcome(SESSION_COORDINATION_HANDLER, stdout)
}

fn handler_outcome(handler_id: &str, stdout: &[u8]) -> Result<RuleOutcome, HookError> {
    if stdout.is_empty() {
        return Ok(simple(DecisionAction::Allow, handler_id));
    }
    let value: Value = serde_json::from_slice(stdout).map_err(|_| {
        HookError::data(
            "handler-output-invalid",
            "runtime-kit handler output is not JSON",
        )
    })?;
    let action = infer_provider_action(&value);
    let context = value
        .pointer("/hookSpecificOutput/additionalContext")
        .or_else(|| value.get("additionalContext"))
        .and_then(Value::as_str)
        .map(|value| value.chars().take(16 * 1024).collect());
    let replacement = value
        .pointer("/hookSpecificOutput/updatedInput")
        .or_else(|| value.get("updatedInput"))
        .filter(|value| value.is_object())
        .cloned();
    Ok(RuleOutcome {
        action,
        code: handler_id.to_string(),
        context,
        replacement,
        provider_output: Some(value),
    })
}

fn infer_provider_action(value: &Value) -> DecisionAction {
    let decision = value
        .get("decision")
        .or_else(|| value.pointer("/hookSpecificOutput/permissionDecision"))
        .and_then(Value::as_str);
    if value.get("continue").and_then(Value::as_bool) == Some(false)
        || matches!(decision, Some("block" | "deny" | "denied"))
    {
        return DecisionAction::Block;
    }
    if value.pointer("/hookSpecificOutput/updatedInput").is_some()
        || value.get("updatedInput").is_some()
    {
        return DecisionAction::Transform;
    }
    if value
        .pointer("/hookSpecificOutput/additionalContext")
        .is_some()
        || value.get("additionalContext").is_some()
    {
        return DecisionAction::Context;
    }
    DecisionAction::Allow
}

fn runtime_hook_root(product: Product) -> Result<PathBuf, HookError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| HookError::runtime("home-unavailable", "HOME is required"))?;
    Ok(match product {
        Product::Codex => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".codex"))
            .join("hooks"),
        Product::Claude => home.join(".claude/hooks"),
        Product::Hermes => home.join(".hermes/hooks"),
    })
}

pub(crate) fn validate_policy_handlers(
    loaded: &LoadedPolicy,
    product: Product,
) -> Result<(), HookError> {
    let mut handlers = BTreeSet::new();
    for rule in &loaded.bundle.rules {
        if !rule.products.contains(&product) {
            continue;
        }
        match &rule.capability {
            Capability::RuntimeKitHandler { handler_id } => {
                if let Some(filename) = runtime_handler_filename(handler_id) {
                    handlers.insert((handler_id.clone(), filename));
                }
            }
            Capability::SessionCoordination { .. } => {
                handlers.insert((
                    "session-coordination".to_string(),
                    SESSION_COORDINATION_HANDLER,
                ));
            }
            _ => {}
        }
    }

    let root = runtime_hook_root(product)?;
    let failures = handlers
        .into_iter()
        .filter_map(|(handler_id, filename)| {
            validate_handler(&root.join(filename))
                .err()
                .map(|error| (handler_id, error.code))
        })
        .collect::<Vec<_>>();
    if failures.is_empty() {
        return Ok(());
    }
    let summary = failures
        .iter()
        .map(|(handler_id, code)| format!("{handler_id} ({code})"))
        .collect::<Vec<_>>()
        .join(", ");
    if failures.iter().any(|(_, code)| code == "handler-untrusted") {
        Err(HookError::data(
            "handler-untrusted",
            format!(
                "runtime-kit handlers are untrusted for {}: {summary}",
                product.as_str()
            ),
        ))
    } else {
        Err(HookError::runtime(
            "handler-unavailable",
            format!(
                "runtime-kit handlers are unavailable for {}: {summary}",
                product.as_str()
            ),
        ))
    }
}

fn validate_handler(path: &Path) -> Result<(), HookError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        HookError::runtime("handler-unavailable", "runtime-kit handler is unavailable")
    })?;
    let mode = metadata.permissions().mode();
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || mode & 0o100 == 0
        || mode & 0o022 != 0
    {
        return Err(HookError::data(
            "handler-untrusted",
            "runtime-kit handler type, executable bit, owner, or mode is untrusted",
        ));
    }
    Ok(())
}

fn run_with_budget(
    command: Command,
    input: &[u8],
    budget: &mut ExecutionBudget,
) -> Result<std::process::Output, HookError> {
    let (timeout, output_limit) = budget.reserve_child()?;
    let output = run_bounded(command, input, timeout, output_limit, false)?;
    budget.retain_output(output.stdout.len().saturating_add(output.stderr.len()))?;
    Ok(output)
}

fn run_bounded(
    mut command: Command,
    input: &[u8],
    timeout: Duration,
    output_limit: usize,
    dispatch_deadline: bool,
) -> Result<std::process::Output, HookError> {
    command.process_group(0);
    let deadline = Instant::now() + timeout;
    let mut child = command
        .spawn()
        .map_err(|_| HookError::runtime("capability-unavailable", "capability could not start"))?;
    let input = input.to_vec();
    let input_handle = child.stdin.take().map(|mut stdin| {
        thread::spawn(move || {
            stdin.write_all(&input).map_err(|_| {
                HookError::runtime("capability-input-failed", "capability input failed")
            })
        })
    });
    let stdout_handle = child
        .stdout
        .take()
        .map(|stdout| thread::spawn(move || read_capped(stdout, output_limit + 1)));
    let stderr_handle = child
        .stderr
        .take()
        .map(|stderr| thread::spawn(move || read_capped(stderr, output_limit + 1)));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                terminate_process_group(&mut child);
                return Err(if dispatch_deadline {
                    HookError::data("dispatch-deadline-exceeded", "dispatch deadline exceeded")
                } else {
                    HookError::runtime(
                        "capability-timeout",
                        "capability exceeded its fixed timeout",
                    )
                });
            }
            Err(_) => {
                terminate_process_group(&mut child);
                return Err(HookError::runtime(
                    "capability-wait-failed",
                    "capability state could not be read",
                ));
            }
        }
    };
    terminate_descendants(child.id());
    if let Some(handle) = input_handle {
        wait_for_thread(&handle, deadline)?;
        handle.join().map_err(|_| {
            HookError::runtime("capability-input-failed", "capability input failed")
        })??;
    }
    let stdout = join_capped(stdout_handle, deadline)?;
    let stderr = join_capped(stderr_handle, deadline)?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn read_capped(mut pipe: impl Read, limit: usize) -> Result<Vec<u8>, HookError> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = pipe.read(&mut buffer).map_err(|_| {
            HookError::runtime("capability-output-failed", "capability output failed")
        })?;
        if read == 0 {
            return Ok(retained);
        }
        let remaining = limit.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

fn join_capped(
    handle: Option<thread::JoinHandle<Result<Vec<u8>, HookError>>>,
    deadline: Instant,
) -> Result<Vec<u8>, HookError> {
    match handle {
        Some(handle) => {
            wait_for_thread(&handle, deadline)?;
            handle.join().map_err(|_| {
                HookError::runtime("capability-output-failed", "capability output failed")
            })?
        }
        None => Ok(Vec::new()),
    }
}

fn wait_for_thread<T>(handle: &thread::JoinHandle<T>, deadline: Instant) -> Result<(), HookError> {
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return Err(HookError::runtime(
                "capability-timeout",
                "capability exceeded its deadline while draining pipes",
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn terminate_descendants(process_group: u32) {
    let process_group = process_group as libc::pid_t;
    unsafe {
        libc::kill(-process_group, libc::SIGKILL);
    }
}

fn terminate_process_group(child: &mut std::process::Child) {
    terminate_descendants(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use nils_common::coordination_projection::{CoordinationMode, ReadError};

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Default)]
    struct CountingLivenessIo {
        session_reads: Cell<usize>,
        dirty_probes: Cell<usize>,
        age_probes: Cell<usize>,
        age_seconds: u64,
    }

    impl liveness::LivenessIo for CountingLivenessIo {
        fn session_coordination_mode(
            &self,
            _state_root: &Path,
            _session_id: &str,
            _incarnation: &str,
        ) -> Result<CoordinationMode, ReadError> {
            self.session_reads.set(self.session_reads.get() + 1);
            Ok(CoordinationMode::Enforce)
        }

        fn checkout_dirty(&self, _path: &Path) -> Option<bool> {
            self.dirty_probes.set(self.dirty_probes.get() + 1);
            Some(false)
        }

        fn checkout_age(&self, _path: &Path) -> Option<u64> {
            self.age_probes.set(self.age_probes.get() + 1);
            Some(self.age_seconds)
        }
    }

    struct EnvRestore(Vec<(&'static str, Option<OsString>)>);

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                set_env(name, value);
            }
        }
    }

    #[test]
    fn coordination_projection_reads_session_once_and_probes_each_root_once() {
        let _env = ENV_LOCK.lock().expect("environment lock");
        let temp = tempfile::TempDir::new().expect("tempdir");
        let _restore = EnvRestore(vec![
            ("AGENT_SESSION_ID", std::env::var_os("AGENT_SESSION_ID")),
            (
                "AGENT_SESSION_RUNTIME_ID",
                std::env::var_os("AGENT_SESSION_RUNTIME_ID"),
            ),
            (
                "AGENT_SESSION_STATE_DIR",
                std::env::var_os("AGENT_SESSION_STATE_DIR"),
            ),
            (
                "AGENT_SESSION_COORDINATION_MODE",
                std::env::var_os("AGENT_SESSION_COORDINATION_MODE"),
            ),
        ]);
        set_env("AGENT_SESSION_ID", Some("current".into()));
        set_env("AGENT_SESSION_RUNTIME_ID", Some("inc-current".into()));
        set_env(
            "AGENT_SESSION_STATE_DIR",
            Some(temp.path().as_os_str().to_os_string()),
        );
        set_env("AGENT_SESSION_COORDINATION_MODE", None);

        for cardinality in [1, 16, 64] {
            let loaded = owner_policy(cardinality);
            let mut request = owner_request(temp.path());
            let prepared = prepare(&loaded, &request, false, &BTreeSet::new())
                .expect("valid reason cardinality");
            let io = CountingLivenessIo::default();
            let decision =
                evaluate_with_io(&loaded, &mut request, b"{}", &prepared, None, None, &io)
                    .expect("evaluate owner rules");
            assert_eq!(decision.reasons.len(), cardinality);
            assert_eq!(io.session_reads.get(), 1, "rules={cardinality}");
            assert_eq!(io.dirty_probes.get(), 1, "rules={cardinality}");
            assert_eq!(io.age_probes.get(), 1, "rules={cardinality}");
        }

        for cardinality in [65, 512] {
            let loaded = owner_policy(cardinality);
            let request = owner_request(temp.path());
            let io = CountingLivenessIo::default();
            let error = prepare(&loaded, &request, false, &BTreeSet::new())
                .expect_err("over-limit reasons must fail before liveness projection");
            assert_eq!(error.code, "decision-reason-limit");
            assert_eq!(io.session_reads.get(), 0, "rules={cardinality}");
            assert_eq!(io.dirty_probes.get(), 0, "rules={cardinality}");
            assert_eq!(io.age_probes.get(), 0, "rules={cardinality}");
        }

        let mut loaded = owner_policy(2);
        for (capability, ttl) in loaded
            .bundle
            .rules
            .iter_mut()
            .map(|rule| &mut rule.capability)
            .zip([30, 300])
        {
            let Capability::OwnerLiveness {
                legacy_ttl_seconds, ..
            } = capability
            else {
                panic!("owner capability");
            };
            *legacy_ttl_seconds = ttl;
        }
        let mut request = owner_request(temp.path());
        let prepared = prepare(&loaded, &request, false, &BTreeSet::new()).expect("TTL rules");
        let io = CountingLivenessIo {
            age_seconds: 60,
            ..Default::default()
        };
        let decision = evaluate_with_io(&loaded, &mut request, b"{}", &prepared, None, None, &io)
            .expect("evaluate TTL rules");
        assert_eq!(
            decision.reasons[0].code,
            concat!("leg", "acy-owner-stale-clean-reclaim")
        );
        assert_eq!(
            decision.reasons[1].code,
            concat!("leg", "acy-owner-unknown")
        );
        assert_eq!(io.session_reads.get(), 1);
        assert_eq!(io.dirty_probes.get(), 1);
        assert_eq!(io.age_probes.get(), 1);
    }

    fn owner_policy(cardinality: usize) -> LoadedPolicy {
        let rules = (0..cardinality)
            .map(|index| crate::model::PolicyRule {
                id: format!("runtime.owner-{index}"),
                products: vec![Product::Codex],
                events: vec!["PreToolUse".to_string()],
                matcher: Some("Write".to_string()),
                priority: index as i32,
                mode: RuleMode::Enforce,
                failure_posture: FailurePosture::Closed,
                timeout_posture: TimeoutPosture::Closed,
                override_class: crate::model::OverrideClass::Locked,
                capability: Capability::OwnerLiveness {
                    reason_code: format!("owner-{index}"),
                    legacy_ttl_seconds: 300,
                },
            })
            .collect();
        LoadedPolicy {
            config: crate::model::Config {
                schema_version: crate::model::CONFIG_VERSION.to_string(),
                policy: crate::model::PolicySelection {
                    path: PathBuf::from("/policy.toml"),
                    digest: "sha256:test".to_string(),
                },
                providers: BTreeMap::new(),
                overrides: BTreeMap::new(),
            },
            bundle: crate::model::PolicyBundle {
                schema_version: crate::model::POLICY_VERSION.to_string(),
                bundle_id: "runtime-kit".to_string(),
                version: "2026.07.20.1".to_string(),
                rules,
            },
            config_digest: "sha256:config".to_string(),
            policy_digest: "sha256:policy".to_string(),
        }
    }

    fn owner_request(root: &Path) -> NormalizedRequest {
        NormalizedRequest {
            schema_version: crate::model::REQUEST_VERSION.to_string(),
            request_id: "request".to_string(),
            product: Product::Codex,
            event: "PreToolUse".to_string(),
            matcher: Some("Write".to_string()),
            target_digest: "sha256:target".to_string(),
            command_digest: "sha256:command".to_string(),
            snapshot_digest: "sha256:snapshot".to_string(),
            worktree_fingerprint: None,
            semantic_conflict: None,
            target_paths: vec![root.join("target.txt")],
            execution_path: Some(root.to_path_buf()),
            binding_roots: vec![root.to_path_buf()],
        }
    }

    #[test]
    fn exact_bootstrap_candidate_requires_a_selected_coordination_rule() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut request = owner_request(temp.path());
        request.matcher = Some("Bash".to_string());
        let exact = br#"{
          "hook_event_name": "PreToolUse",
          "tool_name": "Bash",
          "tool_input": {
            "command": "'/trusted release/main-agent' bootstrap --idempotency-key bootstrap-12345678 --format json"
          }
        }"#;
        assert!(exact_bootstrap_candidate_for_session_coordination(
            &request, exact, true
        ));
        assert!(!exact_bootstrap_candidate_for_session_coordination(
            &request, exact, false
        ));

        for command in [
            "./main-agent bootstrap --idempotency-key bootstrap-12345678 --format json",
            "main-agent bootstrap --format json --idempotency-key bootstrap-12345678",
            "main-agent bootstrap --idempotency-key short --format json",
            "main-agent bootstrap --idempotency-key bootstrap-12345678 --format json extra",
            "main-agent bootstrap --idempotency-key bootstrap-12345678 --format json; touch forbidden",
            "sh -c 'main-agent bootstrap --idempotency-key bootstrap-12345678 --format json'",
            "/tmp/$(touch>/tmp/owner-bypass)/main-agent bootstrap --idempotency-key bootstrap-12345678 --format json",
            "/${ATTACKER_BIN}/main-agent bootstrap --idempotency-key bootstrap-12345678 --format json",
            "/tmp/`touch /tmp/owner-bypass`/main-agent bootstrap --idempotency-key bootstrap-12345678 --format json",
            "/tmp/$((1+1))/main-agent bootstrap --idempotency-key bootstrap-12345678 --format json",
            "/trusted/bin/main-agent bootstrap --idempotency-key bootstrap-12345678 --format json > /tmp/result",
        ] {
            let raw = serde_json::to_vec(&serde_json::json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": {"command": command}
            }))
            .expect("provider request");
            assert!(
                !exact_bootstrap_candidate_for_session_coordination(&request, &raw, true),
                "unexpected deferral for {command}"
            );
        }
    }

    fn set_env(name: &str, value: Option<OsString>) {
        // SAFETY: this test serializes every environment mutation under ENV_LOCK and restores it.
        unsafe {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }

    #[test]
    fn bounded_runner_drains_overflowing_stdout_and_stderr_without_deadlock() {
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "head -c 307200 /dev/zero; head -c 307200 /dev/zero >&2",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let started = Instant::now();
        let output = run_bounded(command, &[], HANDLER_TIMEOUT, MAX_HANDLER_OUTPUT, false)
            .expect("bounded output");

        assert!(output.status.success());
        assert_eq!(output.stdout.len(), MAX_HANDLER_OUTPUT + 1);
        assert_eq!(output.stderr.len(), MAX_HANDLER_OUTPUT + 1);
        assert!(started.elapsed() < HANDLER_TIMEOUT);
    }

    #[test]
    fn bounded_runner_handles_child_exit_before_stdin_is_consumed() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "exit 0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let started = Instant::now();
        let error = run_bounded(
            command,
            &vec![b'x'; 1024 * 1024],
            HANDLER_TIMEOUT,
            MAX_HANDLER_OUTPUT,
            false,
        )
        .expect_err("closed child stdin must be reported");

        assert_eq!(error.code, "capability-input-failed");
        assert!(started.elapsed() < HANDLER_TIMEOUT);
    }

    #[test]
    fn shadow_child_budget_cannot_exhaust_enforced_budget() {
        let mut budgets = ExecutionBudgets::new();
        budgets.shadow.children = MAX_EXECUTABLE_CAPABILITIES;

        assert_eq!(
            budgets
                .shadow
                .reserve_child()
                .expect_err("shadow budget exhausted")
                .code,
            "dispatch-child-budget-exceeded"
        );
        assert!(budgets.enforced.reserve_child().is_ok());
        assert_eq!(budgets.enforced.children, 1);
    }
}

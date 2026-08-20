use std::fs;
use std::path::PathBuf;

use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::Deserialize;
use serde_json::Value;

use crate::cli;
use crate::{CliContext, CliError};

use super::{authenticate_recovery_token, authenticate_token, claims, mailbox};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaimBody {
    pub candidate: Value,
    pub idempotency_key: String,
    pub if_revision: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckBody {
    pub candidate: Option<Value>,
    #[serde(default)]
    pub self_selector: bool,
    #[serde(default)]
    pub allow_incomplete: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaimMutationBody {
    pub claim: String,
    pub if_revision: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdmitBody {
    pub claim: String,
    pub if_revision: u64,
    pub targets: Value,
    pub operation: String,
    pub execution_token: String,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompleteBody {
    pub lease: String,
    pub if_revision: u64,
    pub execution_token: String,
    pub outcome: String,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReconcileBody {
    pub lease: String,
    pub if_revision: u64,
    pub proof: Value,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendBody {
    pub body: String,
    pub idempotency_key: String,
    pub reply_to: Option<String>,
    pub expires_in: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AckBody {
    pub if_revision: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplyBody {
    pub body: String,
    pub if_revision: u64,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WaitBody {
    pub if_revision: u64,
    pub timeout: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrokerRecoveryBody {
    pub proof: Value,
    pub idempotency_key: String,
    pub operation: Option<String>,
    pub if_revision: Option<u64>,
    #[serde(default)]
    pub attest_inactive: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperatorOperationReconcileBody {
    pub schema_version: String,
    pub expected_session_incarnation: String,
    pub expected_session_generation: u64,
    pub if_revision: u64,
    pub reason: String,
    #[serde(default)]
    pub attest_inactive: bool,
    #[serde(default)]
    pub confirmed: bool,
    pub idempotency_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperatorProviderTurnReconcileBody {
    pub schema_version: String,
    pub expected_session_incarnation: String,
    pub expected_runtime_launch_id: String,
    pub expected_runtime_generation: u64,
    pub if_activity_revision: u64,
    pub expected_provider_turn_id: String,
    pub reason: String,
    #[serde(default)]
    pub attest_inactive: bool,
    #[serde(default)]
    pub confirmed: bool,
    pub idempotency_key: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InboxQuery {
    pub state: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

pub(crate) fn show(context: &CliContext, id: &str) -> Result<Value, CliError> {
    claims::show(
        context,
        cli::WorkContextShowArgs {
            session: id.to_string(),
            capability_file: None,
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

pub(crate) fn check(
    context: &CliContext,
    id: &str,
    token: Option<&str>,
    body: CheckBody,
) -> Result<Value, CliError> {
    validate_session_check_body(&body)?;
    let capability = if body.self_selector {
        let token = token.ok_or_else(super::unauthorized)?;
        Some(authorize(context, id, token)?)
    } else {
        None
    };
    claims::check(
        context,
        cli::WorkContextCheckArgs {
            session: (!body.self_selector).then(|| id.to_string()),
            self_selector: body.self_selector,
            capability_file: capability.as_ref().map(|value| value.path.clone()),
            candidate: None,
            allow_incomplete: body.allow_incomplete,
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

fn validate_session_check_body(body: &CheckBody) -> Result<(), CliError> {
    if body.candidate.is_some() {
        return Err(CliError::usage(
            "invalid-check-selector",
            "session-level checks accept only the path session or authenticated self",
            None,
        ));
    }
    Ok(())
}

pub(crate) fn check_candidate(context: &CliContext, body: CheckBody) -> Result<Value, CliError> {
    if body.self_selector {
        return Err(CliError::usage(
            "invalid-check-selector",
            "registry-level checks accept only an explicit candidate",
            None,
        ));
    }
    let candidate = body.candidate.as_ref().ok_or_else(|| {
        CliError::usage(
            "invalid-check-selector",
            "registry-level conflict checks require a candidate",
            None,
        )
    })?;
    with_json(context, "registry", "candidate", candidate, |candidate| {
        claims::check(
            context,
            cli::WorkContextCheckArgs {
                session: None,
                self_selector: false,
                capability_file: None,
                candidate: Some(candidate),
                allow_incomplete: body.allow_incomplete,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    })
}

pub(crate) fn claim(
    context: &CliContext,
    id: &str,
    token: &str,
    body: ClaimBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    with_json(context, id, "candidate", &body.candidate, |file| {
        claims::claim(
            context,
            cli::WorkContextClaimArgs {
                session: id.to_string(),
                file,
                capability_file: Some(server_capability.path.clone()),
                idempotency_key: body.idempotency_key,
                if_revision: body.if_revision,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    })
}

pub(crate) fn claim_mutation(
    context: &CliContext,
    id: &str,
    token: &str,
    body: ClaimMutationBody,
    release: bool,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    let capability_file = Some(server_capability.path.clone());
    if release {
        claims::release(
            context,
            cli::WorkContextReleaseArgs {
                session: id.to_string(),
                claim: body.claim,
                if_revision: body.if_revision,
                capability_file,
                idempotency_key: body.idempotency_key,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    } else {
        claims::renew(
            context,
            cli::WorkContextRenewArgs {
                session: id.to_string(),
                claim: body.claim,
                if_revision: body.if_revision,
                capability_file,
                idempotency_key: body.idempotency_key,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    }
}

pub(crate) fn admit(
    context: &CliContext,
    id: &str,
    token: &str,
    body: AdmitBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    with_json(context, id, "targets", &body.targets, |targets_file| {
        with_text(
            context,
            id,
            "execution-token",
            &body.execution_token,
            |execution_token_file| {
                claims::admit(
                    context,
                    cli::WorkContextAdmitArgs {
                        session: id.to_string(),
                        claim: body.claim,
                        if_revision: body.if_revision,
                        targets_file,
                        operation: body.operation,
                        execution_token_file,
                        capability_file: Some(server_capability.path.clone()),
                        idempotency_key: body.idempotency_key,
                        format: nils_common::cli_contract::OutputFormat::Json,
                    },
                )
            },
        )
    })
}

pub(crate) fn complete(
    context: &CliContext,
    id: &str,
    token: &str,
    body: CompleteBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    let outcome = match body.outcome.as_str() {
        "pass" => cli::OperationOutcome::Pass,
        "fail" => cli::OperationOutcome::Fail,
        _ => {
            return Err(CliError::usage(
                "invalid-operation-outcome",
                "operation outcome must be pass or fail",
                None,
            ));
        }
    };
    with_text(
        context,
        id,
        "execution-token",
        &body.execution_token,
        |execution_token_file| {
            claims::complete(
                context,
                cli::WorkContextCompleteArgs {
                    session: id.to_string(),
                    lease: body.lease,
                    if_revision: body.if_revision,
                    execution_token_file,
                    outcome,
                    capability_file: Some(server_capability.path.clone()),
                    idempotency_key: body.idempotency_key,
                    format: nils_common::cli_contract::OutputFormat::Json,
                },
            )
        },
    )
}

pub(crate) fn reconcile(
    context: &CliContext,
    id: &str,
    token: &str,
    body: ReconcileBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    with_json(context, id, "proof", &body.proof, |proof_file| {
        claims::reconcile(
            context,
            cli::WorkContextReconcileArgs {
                session: id.to_string(),
                lease: body.lease,
                if_revision: body.if_revision,
                proof_file,
                capability_file: Some(server_capability.path.clone()),
                idempotency_key: body.idempotency_key,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    })
}

pub(crate) fn broker_status(context: &CliContext, id: &str) -> Result<Value, CliError> {
    super::broker::status(
        context,
        cli::BrokerStatusArgs {
            session: id.to_string(),
            capability_file: None,
            authenticated: false,
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

pub(crate) fn broker_recover(
    context: &CliContext,
    id: &str,
    token: &str,
    body: BrokerRecoveryBody,
    reconcile: bool,
) -> Result<Value, CliError> {
    let server_capability = authorize_recovery(context, id, token)?;
    with_json(
        context,
        id,
        "broker-recovery-proof",
        &body.proof,
        |proof_file| {
            super::broker::recover(
                context,
                cli::BrokerRecoveryArgs {
                    session: id.to_string(),
                    capability_file: Some(server_capability.path.clone()),
                    proof_file,
                    idempotency_key: body.idempotency_key,
                    operation: body.operation,
                    if_revision: body.if_revision,
                    attest_inactive: body.attest_inactive,
                    format: nils_common::cli_contract::OutputFormat::Json,
                },
                reconcile,
            )
        },
    )
}

pub(crate) fn operator_reconcile_operation(
    context: &CliContext,
    id: &str,
    lease_id: &str,
    body: OperatorOperationReconcileBody,
) -> Result<Value, CliError> {
    if body.schema_version != "agent-session.operator-operation-reconcile-request.v1"
        || body.reason != "post-tool-outcome-missing"
    {
        return Err(CliError::usage(
            "invalid-operator-reconcile-request",
            "operator reconciliation request is unsupported",
            None,
        ));
    }
    if !body.attest_inactive || !body.confirmed {
        return Err(CliError::data(
            "operator-reconcile-confirmation-required",
            "operator reconciliation requires confirmed inactive attestation",
            None,
        ));
    }
    crate::validate_id(id)?;
    let observed = crate::load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _session_lock = crate::acquire_session_record_lock(context, &canonical_id)?;
    let record = crate::load_session_record(context, &canonical_id)?;
    crate::ensure_same_session_identity(&observed, &record)?;
    let incarnation = super::incarnation(&record)?;
    let generation = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.generation)
        .ok_or_else(|| {
            CliError::data(
                "session-incarnation-conflict",
                "operator reconciliation requires a current runtime generation",
                None,
            )
        })?;
    if incarnation != body.expected_session_incarnation
        || generation != body.expected_session_generation
    {
        return Err(CliError::data(
            "session-incarnation-conflict",
            "operator reconciliation selectors do not match the current runtime",
            None,
        ));
    }
    let operation = "operator-operation-reconcile";
    let digest = super::request_digest(
        operation,
        &serde_json::json!({
            "session_id": canonical_id,
            "session_incarnation": incarnation,
            "session_generation": generation,
            "lease_id": lease_id,
            "if_revision": body.if_revision,
            "reason": body.reason,
            "attest_inactive": body.attest_inactive,
            "confirmed": body.confirmed,
        }),
    );
    let now = super::now_epoch();
    let _activity_fence =
        crate::activity::acquire_coordination_activity_lock(context, &canonical_id)?;
    let snapshot = {
        let locked = super::lock_registry(context)?;
        if let Some(replay) = super::idempotency_replay(
            &locked.registry,
            &body.idempotency_key,
            &canonical_id,
            &incarnation,
            operation,
            &digest,
        )? {
            return Ok(replay);
        }
        claims::exact_nonterminal_operation_snapshot(
            &locked.registry,
            &canonical_id,
            &incarnation,
            lease_id,
            body.if_revision,
        )?
    };
    let evidence = claims::operator_reconcile_evidence(context, &record, &snapshot)?;
    let mut locked = super::lock_registry(context)?;
    if let Some(replay) = super::idempotency_replay(
        &locked.registry,
        &body.idempotency_key,
        &canonical_id,
        &incarnation,
        operation,
        &digest,
    )? {
        return Ok(replay);
    }
    if claims::drain_completion_events_for_lease_in_registry(
        &mut locked.registry,
        &canonical_id,
        &incarnation,
        lease_id,
        now,
    )? > 0
    {
        locked.save()?;
        return Err(claims::revision_conflict("operation-revision-conflict"));
    }
    let reconciliation = claims::operator_attested_transition_in_registry(
        &mut locked.registry,
        &canonical_id,
        &incarnation,
        lease_id,
        body.if_revision,
        now,
        evidence,
    )?;
    let result = serde_json::json!({
        "schema_version": "agent-session.operator-operation-reconcile-result.v1",
        "session_id": canonical_id,
        "session_incarnation": incarnation,
        "session_generation": generation,
        "reason": body.reason,
        "operation_reconciliation": reconciliation,
    });
    super::store_receipt(
        &mut locked.registry,
        body.idempotency_key,
        canonical_id,
        incarnation,
        operation.to_string(),
        digest,
        result.clone(),
        now,
    )?;
    locked.save()?;
    Ok(result)
}

pub(crate) fn operator_reconcile_provider_turn(
    context: &CliContext,
    id: &str,
    body: OperatorProviderTurnReconcileBody,
) -> Result<Value, CliError> {
    if body.schema_version != "agent-session.operator-provider-turn-reconcile-request.v1"
        || body.reason != "authoritative-completion-signal-missing"
        || body.expected_session_incarnation.is_empty()
        || body.expected_session_incarnation.len() > 256
        || body.expected_runtime_launch_id.is_empty()
        || body.expected_runtime_launch_id.len() > 256
        || body.expected_provider_turn_id.is_empty()
        || body.expected_provider_turn_id.len() > 256
    {
        return Err(CliError::usage(
            "invalid-operator-provider-turn-reconcile-request",
            "operator provider turn reconciliation request is unsupported",
            None,
        ));
    }
    super::validate_idempotency_key(&body.idempotency_key)?;
    if !body.attest_inactive || !body.confirmed {
        return Err(CliError::data(
            "operator-provider-turn-reconcile-confirmation-required",
            "operator provider turn reconciliation requires confirmed inactive attestation",
            None,
        ));
    }
    crate::validate_id(id)?;
    let observed = crate::load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _session_lock = crate::acquire_session_record_lock(context, &canonical_id)?;
    let record = crate::load_session_record(context, &canonical_id)?;
    crate::ensure_same_session_identity(&observed, &record)?;
    let incarnation = super::incarnation(&record)?;
    let runtime = record.runtime.as_ref().ok_or_else(|| {
        CliError::data(
            "session-incarnation-conflict",
            "operator provider turn reconciliation requires a current runtime",
            None,
        )
    })?;
    if incarnation != body.expected_session_incarnation
        || runtime.launch_id != body.expected_runtime_launch_id
        || runtime.generation != body.expected_runtime_generation
    {
        return Err(CliError::data(
            "session-incarnation-conflict",
            "operator provider turn reconciliation selectors do not match the current runtime",
            None,
        ));
    }
    let operation = "operator-provider-turn-reconcile";
    let digest = super::request_digest(
        operation,
        &serde_json::json!({
            "session_id": canonical_id,
            "session_incarnation": incarnation,
            "runtime_launch_id": runtime.launch_id,
            "runtime_generation": runtime.generation,
            "activity_revision": body.if_activity_revision,
            "provider_turn_id": body.expected_provider_turn_id,
            "reason": body.reason,
            "attest_inactive": body.attest_inactive,
            "confirmed": body.confirmed,
        }),
    );
    // Global order for writers that take more than one fence is:
    // session record -> activity -> runtime health -> coordination registry.
    // Provider ingestion takes the same session/activity/health prefix.
    // Registry maintenance reads activity snapshots without taking any of
    // those locks, so this route never creates a registry-to-activity edge.
    let _activity_fence =
        crate::activity::acquire_coordination_activity_lock(context, &canonical_id)?;
    let _health_fence = crate::activity::acquire_runtime_health_fence(context, &record)?;
    if let Some(replay) = crate::activity::operator_provider_turn_replay_locked(
        context,
        &canonical_id,
        &_activity_fence,
        crate::activity::OperatorProviderTurnReplaySelector {
            session_incarnation: &incarnation,
            runtime_launch_id: &runtime.launch_id,
            runtime_generation: runtime.generation,
        },
        &body.idempotency_key,
        &digest,
    )? {
        return Ok(replay);
    }
    let runtime_evidence = crate::coordination_runtime_evidence(context, &record)?;
    if runtime_evidence.status != crate::CoordinationRuntimeStatus::Running {
        return Err(CliError::data(
            "operator-provider-turn-reconcile-runtime-conflict",
            "operator provider turn reconciliation requires the exact live runtime",
            None,
        ));
    }
    let quiescence =
        super::lock_session_quiescence_observational(context, &canonical_id, &incarnation)?;
    if !quiescence.broker_present
        || !quiescence.broker_identity_matched
        || !quiescence.broker_authoritative
        || quiescence.broker_generation != Some(runtime.generation)
        || quiescence.broker_runtime_identity_digest.as_deref()
            != Some(runtime_evidence.identity_digest.as_str())
        || quiescence.active_operation
        || quiescence.uncertain_operation
    {
        return Err(CliError::data(
            "operator-provider-turn-reconcile-operation-conflict",
            "operator provider turn reconciliation requires the exact authoritative live broker with no active or uncertain operation",
            None,
        ));
    }
    crate::activity::operator_reconcile_provider_turn_locked(
        context,
        &canonical_id,
        &_activity_fence,
        &_health_fence,
        crate::activity::OperatorProviderTurnReconcileInput {
            session_incarnation: &incarnation,
            runtime_launch_id: &runtime.launch_id,
            runtime_generation: runtime.generation,
            activity_revision: body.if_activity_revision,
            provider: &record.agent,
            provider_turn_id: &body.expected_provider_turn_id,
            reason: &body.reason,
            idempotency_key: &body.idempotency_key,
            request_digest: &digest,
        },
    )
}

pub(crate) fn inbox(
    context: &CliContext,
    id: &str,
    token: &str,
    query: InboxQuery,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    mailbox::inbox(
        context,
        cli::MessageInboxArgs {
            session: id.to_string(),
            capability_file: Some(server_capability.path.clone()),
            state: query.state,
            cursor: query.cursor,
            limit: query.limit,
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

pub(crate) fn send(
    context: &CliContext,
    id: &str,
    token: &str,
    body: SendBody,
) -> Result<Value, CliError> {
    let (sender, server_capability) = authorize_any(context, token)?;
    with_text(context, &sender.id, "body", &body.body, |body_file| {
        mailbox::send(
            context,
            cli::MessageSendArgs {
                from_session: sender.id.clone(),
                to_session: id.to_string(),
                body_file,
                capability_file: Some(server_capability.path.clone()),
                idempotency_key: body.idempotency_key,
                reply_to: body.reply_to,
                expires_in: body.expires_in,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    })
}

pub(crate) fn message_show(
    context: &CliContext,
    id: &str,
    message: &str,
    token: &str,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    mailbox::show(
        context,
        cli::MessageShowArgs {
            session: id.to_string(),
            message: message.to_string(),
            capability_file: Some(server_capability.path.clone()),
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

pub(crate) fn ack(
    context: &CliContext,
    id: &str,
    message: &str,
    token: &str,
    body: AckBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    mailbox::ack(
        context,
        cli::MessageAckArgs {
            session: id.to_string(),
            message: message.to_string(),
            if_revision: body.if_revision,
            capability_file: Some(server_capability.path.clone()),
            idempotency_key: body.idempotency_key,
            format: nils_common::cli_contract::OutputFormat::Json,
        },
    )
}

pub(crate) fn reply(
    context: &CliContext,
    id: &str,
    message: &str,
    token: &str,
    body: ReplyBody,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    with_text(context, id, "reply", &body.body, |body_file| {
        mailbox::reply(
            context,
            cli::MessageReplyArgs {
                session: id.to_string(),
                message: message.to_string(),
                if_revision: body.if_revision,
                body_file,
                capability_file: Some(server_capability.path.clone()),
                idempotency_key: body.idempotency_key,
                format: nils_common::cli_contract::OutputFormat::Json,
            },
        )
    })
}

pub(crate) fn wait_with_cancellation(
    context: &CliContext,
    id: &str,
    message: &str,
    token: &str,
    body: WaitBody,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Value, CliError> {
    let server_capability = authorize(context, id, token)?;
    mailbox::wait_with_cancellation(
        context,
        cli::MessageWaitArgs {
            session: id.to_string(),
            message: message.to_string(),
            if_revision: body.if_revision,
            timeout: body.timeout,
            capability_file: Some(server_capability.path.clone()),
            format: nils_common::cli_contract::OutputFormat::Json,
        },
        cancelled,
    )
}

struct ServerCapability {
    path: PathBuf,
}

impl Drop for ServerCapability {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn authorize(context: &CliContext, id: &str, token: &str) -> Result<ServerCapability, CliError> {
    authenticate_token(context, id, token)?;
    stage_server_capability(context, id, token)
}

fn authorize_recovery(
    context: &CliContext,
    id: &str,
    token: &str,
) -> Result<ServerCapability, CliError> {
    authenticate_recovery_token(context, id, token)?;
    stage_server_capability(context, id, token)
}

fn stage_server_capability(
    context: &CliContext,
    id: &str,
    token: &str,
) -> Result<ServerCapability, CliError> {
    let directory = if id == "registry" {
        drop(super::lock_registry(context)?);
        context.state_dir.join("coordination")
    } else {
        super::coordination_dir(context, id)
    };
    let path = directory.join(format!(
        ".server-capability-{}.request",
        uuid::Uuid::new_v4()
    ));
    write_atomic(&path, token.as_bytes(), SECRET_FILE_MODE).map_err(|_| {
        CliError::runtime(
            "coordination-unavailable",
            "server capability staging failed",
            None,
        )
    })?;
    Ok(ServerCapability { path })
}

fn authorize_any(
    context: &CliContext,
    token: &str,
) -> Result<(crate::SessionRecord, ServerCapability), CliError> {
    let (record, _) = super::authenticate_any_token(context, token)?;
    let capability = stage_server_capability(context, &record.id, token)?;
    Ok((record, capability))
}

fn with_json<T>(
    context: &CliContext,
    id: &str,
    label: &str,
    value: &Value,
    operation: impl FnOnce(PathBuf) -> Result<T, CliError>,
) -> Result<T, CliError> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid_server_body())?;
    with_bytes(context, id, label, &bytes, operation)
}

fn with_text<T>(
    context: &CliContext,
    id: &str,
    label: &str,
    value: &str,
    operation: impl FnOnce(PathBuf) -> Result<T, CliError>,
) -> Result<T, CliError> {
    with_bytes(context, id, label, value.as_bytes(), operation)
}

fn with_bytes<T>(
    context: &CliContext,
    id: &str,
    label: &str,
    bytes: &[u8],
    operation: impl FnOnce(PathBuf) -> Result<T, CliError>,
) -> Result<T, CliError> {
    let directory = if id == "registry" {
        drop(super::lock_registry(context)?);
        context.state_dir.join("coordination")
    } else {
        super::coordination_dir(context, id)
    };
    let path = directory.join(format!(".{label}-{}.request", uuid::Uuid::new_v4()));
    write_atomic(&path, bytes, SECRET_FILE_MODE).map_err(|_| {
        CliError::runtime(
            "coordination-unavailable",
            "server coordination request staging failed",
            None,
        )
    })?;
    let result = operation(path.clone());
    let cleanup = fs::remove_file(&path);
    if cleanup.is_err() && result.is_ok() {
        return Err(CliError::runtime(
            "coordination-unavailable",
            "server coordination request cleanup failed",
            None,
        ));
    }
    result
}

fn invalid_server_body() -> CliError {
    CliError::usage(
        "invalid-json-body",
        "coordination request body is invalid",
        None,
    )
}

pub(crate) fn capability_from_headers(headers: &axum::http::HeaderMap) -> Result<&str, CliError> {
    headers
        .get("x-agent-session-capability")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .ok_or_else(super::unauthorized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn send_body_uses_path_recipient_and_contains_no_redirect_selector() {
        let parsed = serde_json::from_value::<SendBody>(json!({
            "body": "hello",
            "idempotency_key": "send-key-0001",
            "reply_to": null,
            "expires_in": null
        }));
        assert!(
            parsed.is_ok(),
            "recipient must come only from the route path"
        );
        assert!(
            serde_json::from_value::<SendBody>(json!({
                "to": "redirect-target",
                "body": "hello",
                "idempotency_key": "send-key-0002",
                "reply_to": null,
                "expires_in": null
            }))
            .is_err()
        );
    }

    #[test]
    fn session_check_rejects_a_candidate_selector() {
        let body = CheckBody {
            candidate: Some(json!({"schema_version": "agent-session.work-context-input.v1"})),
            self_selector: false,
            allow_incomplete: false,
        };
        let error = validate_session_check_body(&body)
            .expect_err("candidate belongs only on the registry route");
        assert_eq!(error.code(), "invalid-check-selector");
    }
}

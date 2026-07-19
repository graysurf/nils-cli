use std::fs;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::{
    WorkContextAdmitArgs, WorkContextCheckArgs, WorkContextClaimArgs, WorkContextCompleteArgs,
    WorkContextReconcileArgs, WorkContextReleaseArgs, WorkContextRenewArgs, WorkContextShowArgs,
};
use crate::{CliContext, CliError};

use super::context::{
    ConflictClassification, Scope, WORK_CONTEXT_VERSION, WorkContextInput, WorkContextRecord,
    canonicalize_targets, evaluate, fingerprint_epoch, scope_covers, validate_physical_targets,
};
use super::{
    Registry, authenticate_from_file, clean_expired, digest_bytes, ensure_fingerprint_key,
    idempotency_replay, json_value, lock_registry, now_epoch, read_bounded_json, read_private_text,
    request_digest, store_receipt, timestamp, worktree_fingerprint,
};

const CLAIM_TTL_SECS: i64 = 30 * 60;
const OPERATION_TTL_SECS: i64 = 7 * 24 * 60 * 60;
const OPERATION_LEASE_VERSION: &str = "agent-session.operation-lease.v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct OperationLease {
    pub schema_version: String,
    pub lease_id: String,
    pub session_id: String,
    pub session_incarnation: String,
    pub claim_id: String,
    pub claim_revision: u64,
    pub operation: String,
    pub targets: Vec<Scope>,
    pub state: String,
    pub revision: u64,
    pub started_at: String,
    pub expires_at: String,
    pub expires_at_epoch: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_at_epoch: Option<i64>,
    pub execution_token_digest: String,
    #[serde(default)]
    pub activity_revision: u64,
    #[serde(default)]
    pub runtime_identity_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OperationTargetsInput {
    schema_version: String,
    targets: Vec<Scope>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReconcileProof {
    schema_version: String,
    execution_token: String,
    outcome: String,
}

pub(crate) fn claim(context: &CliContext, args: WorkContextClaimArgs) -> Result<Value, CliError> {
    let (record, incarnation) =
        authenticate_from_file(context, &args.session, args.capability_file.as_deref())?;
    let candidate: WorkContextInput =
        read_bounded_json(&args.file, 64 * 1024, "invalid-work-context")?;
    let mut candidate = candidate.validate_and_canonicalize()?;
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    ensure_fingerprint_key(&mut locked.registry);
    let checkout_fingerprint =
        worktree_fingerprint(&locked.registry, std::path::Path::new(&record.cwd))?;
    if !candidate.worktrees.contains(&checkout_fingerprint) {
        if candidate.worktrees.len() >= 16 {
            return Err(CliError::data(
                "invalid-work-context",
                "work context exceeds the worktree fingerprint limit",
                None,
            ));
        }
        candidate.worktrees.push(checkout_fingerprint);
        candidate.worktrees.sort();
    }
    let digest = request_digest(
        "work-context-claim",
        &json!({
            "candidate": candidate,
            "if_revision": args.if_revision,
        }),
    );
    clean_expired(&mut locked.registry, now);
    ensure_current_broker(context, &locked.registry, &record.id, &incarnation)?;
    if let Some(replay) = idempotency_replay(
        &locked.registry,
        &args.idempotency_key,
        &record.id,
        &incarnation,
        "work-context-claim",
        &digest,
    )? {
        return Ok(replay);
    }
    if let Some(existing_index) = locked.registry.claims.iter().position(|claim| {
        claim.session_id == record.id
            && claim.session_incarnation == incarnation
            && claim.state == "active"
    }) {
        let existing_claim_id = locked.registry.claims[existing_index].claim_id.clone();
        if has_nonterminal_operation(&locked.registry, &existing_claim_id) {
            return Err(operation_in_progress());
        }
        let existing = &mut locked.registry.claims[existing_index];
        if args.if_revision != Some(existing.revision) {
            return Err(revision_conflict("claim-revision-conflict"));
        }
        existing.state = "released".to_string();
        existing.revision = existing.revision.saturating_add(1);
        existing.updated_at = timestamp(now);
        existing.terminal_at_epoch = Some(now);
    } else if args.if_revision.is_some() {
        return Err(revision_conflict("claim-revision-conflict"));
    }

    let complete = complete_relevant_universe(context, &locked.registry, &record.id, &incarnation);
    let evaluation = evaluate(
        &record.id,
        &incarnation,
        &candidate,
        &locked.registry.claims,
        complete,
        false,
    );
    if evaluation.classification == ConflictClassification::Conflict {
        return Err(CliError::data(
            "claim-conflict",
            "work context conflicts with an active claim",
            Some(json!({ "evaluation": evaluation })),
        ));
    }
    let claim = WorkContextRecord {
        schema_version: WORK_CONTEXT_VERSION.to_string(),
        session_id: record.id.clone(),
        session_incarnation: incarnation.clone(),
        claim_id: uuid::Uuid::new_v4().to_string(),
        revision: 1,
        state: "active".to_string(),
        intent: candidate.intent,
        tier: candidate.tier,
        repositories: candidate.repositories,
        worktrees: candidate.worktrees,
        provider_refs: candidate.provider_refs,
        plan_refs: candidate.plan_refs,
        scopes: candidate.scopes,
        summary: candidate.summary,
        updated_at: timestamp(now),
        expires_at: timestamp(now.saturating_add(CLAIM_TTL_SECS)),
        expires_at_epoch: now.saturating_add(CLAIM_TTL_SECS),
        terminal_at_epoch: None,
    };
    locked.registry.claims.push(claim.clone());
    let outcome = json!({
        "schema_version": "agent-session.work-context-claim-result.v1",
        "context": public_context(&claim)?,
        "evaluation": evaluation,
    });
    store_receipt(
        &mut locked.registry,
        args.idempotency_key,
        record.id,
        incarnation,
        "work-context-claim".to_string(),
        digest,
        outcome.clone(),
        now,
    );
    locked.save()?;
    Ok(outcome)
}

pub(crate) fn show(context: &CliContext, args: WorkContextShowArgs) -> Result<Value, CliError> {
    let (record, incarnation) =
        authenticate_from_file(context, &args.session, args.capability_file.as_deref())?;
    let locked = lock_registry(context)?;
    let claim = active_claim(&locked.registry, &record.id, &incarnation)?;
    public_context(claim)
}

pub(crate) fn check(context: &CliContext, args: WorkContextCheckArgs) -> Result<Value, CliError> {
    let (record, incarnation) =
        authenticate_from_file(context, &args.session, args.capability_file.as_deref())?;
    let candidate = match args.candidate.as_deref() {
        Some(path) => {
            read_bounded_json::<WorkContextInput>(path, 64 * 1024, "invalid-work-context")?
                .validate_and_canonicalize()?
        }
        None => {
            let locked = lock_registry(context)?;
            input_from_record(active_claim(&locked.registry, &record.id, &incarnation)?)
        }
    };
    let locked = lock_registry(context)?;
    let complete = complete_relevant_universe(context, &locked.registry, &record.id, &incarnation);
    json_value(evaluate(
        &record.id,
        &incarnation,
        &candidate,
        &locked.registry.claims,
        complete,
        args.allow_incomplete,
    ))
}

pub(crate) fn renew(context: &CliContext, args: WorkContextRenewArgs) -> Result<Value, CliError> {
    let (record, incarnation) =
        authenticate_from_file(context, &args.session, args.capability_file.as_deref())?;
    let digest = request_digest(
        "work-context-renew",
        &json!({
            "claim": args.claim,
            "if_revision": args.if_revision,
        }),
    );
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    clean_expired(&mut locked.registry, now);
    if let Some(replay) = idempotency_replay(
        &locked.registry,
        &args.idempotency_key,
        &record.id,
        &incarnation,
        "work-context-renew",
        &digest,
    )? {
        return Ok(replay);
    }
    ensure_current_broker(context, &locked.registry, &record.id, &incarnation)?;
    let claim = locked
        .registry
        .claims
        .iter_mut()
        .find(|claim| {
            claim.session_id == record.id
                && claim.session_incarnation == incarnation
                && claim.claim_id == args.claim
                && claim.state == "active"
        })
        .ok_or_else(claim_unavailable)?;
    if claim.revision != args.if_revision {
        return Err(revision_conflict("claim-revision-conflict"));
    }
    claim.revision = claim.revision.saturating_add(1);
    claim.updated_at = timestamp(now);
    claim.expires_at_epoch = now.saturating_add(CLAIM_TTL_SECS);
    claim.expires_at = timestamp(claim.expires_at_epoch);
    let outcome = public_context(claim)?;
    store_receipt(
        &mut locked.registry,
        args.idempotency_key,
        record.id,
        incarnation,
        "work-context-renew".to_string(),
        digest,
        outcome.clone(),
        now,
    );
    locked.save()?;
    Ok(outcome)
}

pub(crate) fn release(
    context: &CliContext,
    args: WorkContextReleaseArgs,
) -> Result<Value, CliError> {
    let (record, incarnation) =
        authenticate_from_file(context, &args.session, args.capability_file.as_deref())?;
    let digest = request_digest(
        "work-context-release",
        &json!({
            "claim": args.claim,
            "if_revision": args.if_revision,
        }),
    );
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    if clean_expired(&mut locked.registry, now) {
        locked.save()?;
    }
    if let Some(replay) = idempotency_replay(
        &locked.registry,
        &args.idempotency_key,
        &record.id,
        &incarnation,
        "work-context-release",
        &digest,
    )? {
        return Ok(replay);
    }
    let claim_index = locked
        .registry
        .claims
        .iter()
        .position(|claim| {
            claim.session_id == record.id
                && claim.session_incarnation == incarnation
                && claim.claim_id == args.claim
                && claim.state == "active"
        })
        .ok_or_else(claim_unavailable)?;
    if has_nonterminal_operation(&locked.registry, &args.claim) {
        return Err(operation_in_progress());
    }
    let claim = &mut locked.registry.claims[claim_index];
    if claim.revision != args.if_revision {
        return Err(revision_conflict("claim-revision-conflict"));
    }
    claim.state = "released".to_string();
    claim.revision = claim.revision.saturating_add(1);
    claim.updated_at = timestamp(now);
    claim.terminal_at_epoch = Some(now);
    let outcome = public_context(claim)?;
    store_receipt(
        &mut locked.registry,
        args.idempotency_key,
        record.id,
        incarnation,
        "work-context-release".to_string(),
        digest,
        outcome.clone(),
        now,
    );
    locked.save()?;
    Ok(outcome)
}

pub(crate) fn admit(context: &CliContext, args: WorkContextAdmitArgs) -> Result<Value, CliError> {
    let (record, incarnation) =
        authenticate_from_file(context, &args.session, args.capability_file.as_deref())?;
    validate_operation_kind(&args.operation)?;
    let execution_token =
        read_private_text(&args.execution_token_file, 256, "invalid-execution-token")?;
    validate_execution_token(&execution_token)?;
    let input: OperationTargetsInput =
        read_bounded_json(&args.targets_file, 64 * 1024, "invalid-scope")?;
    if input.schema_version != "agent-session.operation-targets.v1" {
        return Err(CliError::data(
            "invalid-scope",
            "operation target schema is unsupported",
            None,
        ));
    }
    let targets = canonicalize_targets(input.targets)?;
    let digest = request_digest(
        "work-context-admit",
        &json!({
            "claim": args.claim,
            "if_revision": args.if_revision,
            "operation": args.operation,
            "targets": targets,
            "execution_token_digest": digest_bytes(execution_token.as_bytes()),
        }),
    );
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    clean_expired(&mut locked.registry, now);
    if let Some(replay) = idempotency_replay(
        &locked.registry,
        &args.idempotency_key,
        &record.id,
        &incarnation,
        "work-context-admit",
        &digest,
    )? {
        return Ok(replay);
    }
    ensure_current_broker(context, &locked.registry, &record.id, &incarnation)?;
    if locked.registry.operations.iter().any(|lease| {
        lease.session_id == record.id && matches!(lease.state.as_str(), "active" | "completing")
    }) {
        return Err(CliError::data(
            "coordination-unavailable",
            "a prior mutation operation is still active or uncertain",
            None,
        ));
    }
    let claim = active_claim(&locked.registry, &record.id, &incarnation)?;
    if claim.claim_id != args.claim || claim.revision != args.if_revision {
        return Err(revision_conflict("claim-revision-conflict"));
    }
    if targets
        .iter()
        .any(|target| !claim.scopes.iter().any(|scope| scope_covers(scope, target)))
    {
        return Err(CliError::data(
            "uncovered-mutation-scope",
            "operation target is not covered by the active claim",
            None,
        ));
    }
    validate_physical_targets(&record.cwd, &targets)?;
    let candidate = input_from_record(claim);
    let complete = complete_relevant_universe(context, &locked.registry, &record.id, &incarnation);
    let evaluation = evaluate(
        &record.id,
        &incarnation,
        &candidate,
        &locked.registry.claims,
        complete,
        false,
    );
    if evaluation.classification == ConflictClassification::Conflict {
        return Err(CliError::data(
            "claim-conflict",
            "operation admission conflicts with an active peer",
            Some(json!({ "evaluation": evaluation })),
        ));
    }
    let activity = crate::activity::state_for_view(context, &record).ok_or_else(|| {
        CliError::runtime(
            "coordination-unavailable",
            "controller-owned activity state is unavailable for operation admission",
            None,
        )
    })?;
    if activity.phase != crate::activity::TurnPhase::Working {
        return Err(CliError::data(
            "operation-not-working",
            "mutation operation admission requires controller-observed working state",
            None,
        ));
    }
    let runtime = crate::coordination_runtime_evidence(&record)?;
    if runtime.status != crate::CoordinationRuntimeStatus::Running {
        return Err(CliError::runtime(
            "coordination-unavailable",
            "the exact persisted runtime is not confirmed running",
            None,
        ));
    }
    let lease = OperationLease {
        schema_version: OPERATION_LEASE_VERSION.to_string(),
        lease_id: uuid::Uuid::new_v4().to_string(),
        session_id: record.id.clone(),
        session_incarnation: incarnation.clone(),
        claim_id: claim.claim_id.clone(),
        claim_revision: claim.revision,
        operation: args.operation,
        targets,
        state: "active".to_string(),
        revision: 1,
        started_at: timestamp(now),
        expires_at: timestamp(now.saturating_add(OPERATION_TTL_SECS)),
        expires_at_epoch: now.saturating_add(OPERATION_TTL_SECS),
        terminal_at_epoch: None,
        execution_token_digest: digest_bytes(execution_token.as_bytes()),
        activity_revision: activity.revision,
        runtime_identity_digest: runtime.identity_digest,
        outcome: None,
    };
    locked.registry.operations.push(lease.clone());
    let outcome = public_lease(&lease)?;
    store_receipt(
        &mut locked.registry,
        args.idempotency_key,
        record.id,
        incarnation,
        "work-context-admit".to_string(),
        digest,
        outcome.clone(),
        now,
    );
    locked.save()?;
    Ok(outcome)
}

pub(crate) fn complete(
    context: &CliContext,
    args: WorkContextCompleteArgs,
) -> Result<Value, CliError> {
    let (record, incarnation) =
        authenticate_from_file(context, &args.session, args.capability_file.as_deref())?;
    let execution_token =
        read_private_text(&args.execution_token_file, 256, "invalid-execution-token")?;
    validate_execution_token(&execution_token)?;
    let token_digest = digest_bytes(execution_token.as_bytes());
    let digest = request_digest(
        "work-context-complete",
        &json!({
            "lease": args.lease,
            "if_revision": args.if_revision,
            "execution_token_digest": token_digest,
            "outcome": args.outcome.as_str(),
        }),
    );
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    if clean_expired(&mut locked.registry, now) {
        locked.save()?;
    }
    if let Some(replay) = idempotency_replay(
        &locked.registry,
        &args.idempotency_key,
        &record.id,
        &incarnation,
        "work-context-complete",
        &digest,
    )? {
        return Ok(replay);
    }
    let lease = locked
        .registry
        .operations
        .iter_mut()
        .find(|lease| {
            lease.session_id == record.id
                && lease.session_incarnation == incarnation
                && lease.lease_id == args.lease
        })
        .ok_or_else(operation_unavailable)?;
    if lease.revision != args.if_revision || lease.state != "active" {
        return Err(revision_conflict("operation-revision-conflict"));
    }
    if lease.execution_token_digest != token_digest {
        return Err(super::unauthorized());
    }
    lease.revision = lease.revision.saturating_add(1);
    lease.state = if args.outcome.as_str() == "pass" {
        "completed".to_string()
    } else {
        "failed".to_string()
    };
    lease.outcome = Some(args.outcome.as_str().to_string());
    lease.terminal_at_epoch = Some(now);
    let outcome = public_lease(lease)?;
    store_receipt(
        &mut locked.registry,
        args.idempotency_key,
        record.id,
        incarnation,
        "work-context-complete".to_string(),
        digest,
        outcome.clone(),
        now,
    );
    locked.save()?;
    Ok(outcome)
}

pub(crate) fn reconcile(
    context: &CliContext,
    args: WorkContextReconcileArgs,
) -> Result<Value, CliError> {
    let (record, incarnation) =
        authenticate_from_file(context, &args.session, args.capability_file.as_deref())?;
    let proof: ReconcileProof =
        read_bounded_json(&args.proof_file, 16 * 1024, "invalid-reconcile-proof")?;
    if proof.schema_version != "agent-session.operation-reconcile-proof.v1"
        || !matches!(proof.outcome.as_str(), "pass" | "fail")
    {
        return Err(CliError::data(
            "invalid-reconcile-proof",
            "operation recovery proof does not establish terminality",
            None,
        ));
    }
    let token_digest = digest_bytes(proof.execution_token.as_bytes());
    let digest = request_digest(
        "work-context-reconcile",
        &json!({
            "lease": args.lease,
            "if_revision": args.if_revision,
            "proof": proof,
        }),
    );
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    if let Some(replay) = idempotency_replay(
        &locked.registry,
        &args.idempotency_key,
        &record.id,
        &incarnation,
        "work-context-reconcile",
        &digest,
    )? {
        return Ok(replay);
    }
    let lease_snapshot = locked
        .registry
        .operations
        .iter()
        .find(|lease| {
            lease.session_id == record.id
                && lease.session_incarnation == incarnation
                && lease.lease_id == args.lease
        })
        .cloned()
        .ok_or_else(operation_unavailable)?;
    if lease_snapshot.revision != args.if_revision
        || lease_snapshot.execution_token_digest != token_digest
    {
        return Err(revision_conflict("operation-revision-conflict"));
    }
    if !matches!(lease_snapshot.state.as_str(), "active" | "completing") {
        return Err(revision_conflict("operation-revision-conflict"));
    }
    if !controller_observed_terminal(context, &record, &lease_snapshot) {
        return Err(CliError::data(
            "operation-still-running",
            "controller-owned state does not prove the exact operation runtime is terminal",
            None,
        ));
    }
    let lease = locked
        .registry
        .operations
        .iter_mut()
        .find(|lease| lease.lease_id == lease_snapshot.lease_id)
        .ok_or_else(operation_unavailable)?;
    lease.revision = lease.revision.saturating_add(1);
    lease.state = if proof.outcome == "pass" {
        "completed".to_string()
    } else {
        "failed".to_string()
    };
    lease.outcome = Some(proof.outcome);
    lease.terminal_at_epoch = Some(now);
    let outcome = public_lease(lease)?;
    store_receipt(
        &mut locked.registry,
        args.idempotency_key,
        record.id,
        incarnation,
        "work-context-reconcile".to_string(),
        digest,
        outcome.clone(),
        now,
    );
    locked.save()?;
    Ok(outcome)
}

fn ensure_current_broker(
    context: &CliContext,
    registry: &Registry,
    session_id: &str,
    incarnation: &str,
) -> Result<(), CliError> {
    let broker = registry.brokers.get(session_id).ok_or_else(|| {
        CliError::runtime(
            "coordination-unavailable",
            "coordination broker is unavailable",
            None,
        )
    })?;
    if broker.incarnation != incarnation {
        return Err(CliError::data(
            "session-incarnation-conflict",
            "session incarnation was replaced",
            None,
        ));
    }
    if broker.state != "ready" {
        return Err(CliError::runtime(
            "coordination-broker-lost",
            "coordination broker is not ready",
            None,
        ));
    }
    if !super::broker::heartbeat_fresh(context, session_id, incarnation, broker.heartbeat_epoch) {
        return Err(CliError::runtime(
            "coordination-broker-lost",
            "coordination broker heartbeat is stale",
            None,
        ));
    }
    Ok(())
}

fn active_claim<'a>(
    registry: &'a Registry,
    session_id: &str,
    incarnation: &str,
) -> Result<&'a WorkContextRecord, CliError> {
    registry
        .claims
        .iter()
        .find(|claim| {
            claim.session_id == session_id
                && claim.session_incarnation == incarnation
                && claim.state == "active"
        })
        .ok_or_else(claim_unavailable)
}

fn input_from_record(claim: &WorkContextRecord) -> WorkContextInput {
    WorkContextInput {
        schema_version: super::context::WORK_CONTEXT_INPUT_VERSION.to_string(),
        intent: claim.intent.clone(),
        tier: claim.tier.clone(),
        repositories: claim.repositories.clone(),
        worktrees: claim.worktrees.clone(),
        provider_refs: claim.provider_refs.clone(),
        plan_refs: claim.plan_refs.clone(),
        scopes: claim.scopes.clone(),
        summary: claim.summary.clone(),
    }
}

fn public_context(claim: &WorkContextRecord) -> Result<Value, CliError> {
    let mut value = json_value(claim)?;
    let object = value
        .as_object_mut()
        .expect("work context serializes as an object");
    object.remove("expires_at_epoch");
    object.remove("terminal_at_epoch");
    Ok(value)
}

fn public_lease(lease: &OperationLease) -> Result<Value, CliError> {
    let mut value = json_value(lease)?;
    let object = value
        .as_object_mut()
        .expect("operation lease serializes as an object");
    object.remove("expires_at_epoch");
    object.remove("terminal_at_epoch");
    object.remove("execution_token_digest");
    object.remove("activity_revision");
    object.remove("runtime_identity_digest");
    Ok(value)
}

fn complete_relevant_universe(
    context: &CliContext,
    registry: &Registry,
    subject_id: &str,
    subject_incarnation: &str,
) -> bool {
    if registry.claims.iter().any(|claim| {
        claim.state == "active"
            && (claim.schema_version != WORK_CONTEXT_VERSION
                || claim.worktrees.iter().any(|fingerprint| {
                    fingerprint_epoch(fingerprint) != Some(registry.fingerprint_epoch)
                }))
    }) {
        return false;
    }
    let sessions = context.state_dir.join("sessions");
    let Ok(entries) = fs::read_dir(sessions) else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.file_type() else {
            return false;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            return false;
        };
        if id == subject_id {
            continue;
        }
        let Some(broker) = registry.brokers.get(&id) else {
            return false;
        };
        if broker.state != "ready" {
            continue;
        }
        if id == subject_id && broker.incarnation == subject_incarnation {
            continue;
        }
        if !registry.claims.iter().any(|claim| {
            claim.session_id == id
                && claim.session_incarnation == broker.incarnation
                && claim.state == "active"
        }) {
            return false;
        }
    }
    true
}

fn has_nonterminal_operation(registry: &Registry, claim_id: &str) -> bool {
    registry.operations.iter().any(|operation| {
        operation.claim_id == claim_id
            && matches!(operation.state.as_str(), "active" | "completing")
    })
}

fn controller_observed_terminal(
    _context: &CliContext,
    record: &crate::SessionRecord,
    lease: &OperationLease,
) -> bool {
    let Ok(runtime) = crate::coordination_runtime_evidence(record) else {
        return false;
    };
    if runtime.identity_digest != lease.runtime_identity_digest {
        return false;
    }
    matches!(runtime.status, crate::CoordinationRuntimeStatus::Stopped)
}

fn validate_operation_kind(value: &str) -> Result<(), CliError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(CliError::usage(
            "invalid-operation-kind",
            "operation kind must use 1-64 lower-kebab ASCII bytes",
            None,
        ));
    }
    Ok(())
}

fn validate_execution_token(value: &str) -> Result<(), CliError> {
    if !(8..=256).contains(&value.len())
        || !value.is_ascii()
        || value.bytes().any(|b| b.is_ascii_control())
    {
        return Err(CliError::usage(
            "invalid-execution-token",
            "execution token is invalid",
            None,
        ));
    }
    Ok(())
}

fn revision_conflict(code: &'static str) -> CliError {
    CliError::data(code, "coordination revision fence did not match", None)
}

fn claim_unavailable() -> CliError {
    CliError::data(
        "claim-not-active",
        "no matching active work claim exists",
        None,
    )
}

fn operation_unavailable() -> CliError {
    CliError::data("operation-not-found", "operation lease was not found", None)
}

fn operation_in_progress() -> CliError {
    CliError::data(
        "operation-in-progress",
        "the claim remains bound to an active or uncertain mutation operation",
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_tokens_are_never_serialized() {
        let lease = OperationLease {
            schema_version: OPERATION_LEASE_VERSION.to_string(),
            lease_id: "lease".to_string(),
            session_id: "session".to_string(),
            session_incarnation: "incarnation".to_string(),
            claim_id: "claim".to_string(),
            claim_revision: 1,
            operation: "edit".to_string(),
            targets: Vec::new(),
            state: "active".to_string(),
            revision: 1,
            started_at: "time".to_string(),
            expires_at: "time".to_string(),
            expires_at_epoch: 0,
            terminal_at_epoch: None,
            execution_token_digest: "canary".to_string(),
            activity_revision: 1,
            runtime_identity_digest: "runtime".to_string(),
            outcome: None,
        };
        let value = public_lease(&lease).expect("serialize");
        assert!(!value.to_string().contains("canary"));
    }
}

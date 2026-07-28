use std::env;
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::{
    WorkContextAdmitArgs, WorkContextCheckArgs, WorkContextClaimArgs, WorkContextCompleteArgs,
    WorkContextReconcileArgs, WorkContextReleaseArgs, WorkContextRenewArgs, WorkContextShowArgs,
};
use crate::{CliContext, CliError};

use super::context::{
    CheckoutBinding, ConflictClassification, ProviderRef, Scope, ScopeKind, WORK_CONTEXT_VERSION,
    WorkContextInput, WorkContextRecord, canonical_repository, canonicalize_provider_refs,
    canonicalize_targets, checkout_root, evaluate, fingerprint_epoch, scope_covers,
    validate_physical_targets,
};
use super::{
    Registry, authenticate_any_from_file, authenticate_from_file, clean_expired, digest_bytes,
    ensure_fingerprint_key, idempotency_replay, json_value, lock_registry, now_epoch,
    read_bounded_json, read_private_text, request_digest, store_receipt, timestamp,
    worktree_fingerprint,
};

const CLAIM_TTL_SECS: i64 = 30 * 60;
const OPERATION_TTL_SECS: i64 = 30 * 60;
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_targets: Vec<ProviderRef>,
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
    pub activity_identity_digest: String,
    #[serde(default)]
    pub runtime_identity_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descendant: Option<DescendantIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconcile_observed_at_epoch: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

pub(crate) struct MainAgentWorkerStartFence {
    lease_id: String,
    session_id: String,
    session_incarnation: String,
    execution_token: String,
    _owner_lock: fs::File,
}

impl MainAgentWorkerStartFence {
    pub(crate) fn finish(self, context: &CliContext, succeeded: bool) -> Result<(), CliError> {
        finish_main_agent_worker_start_fence(
            context,
            &self.session_id,
            &self.session_incarnation,
            &self.lease_id,
            succeeded,
            true,
            Some(&self.execution_token),
        )
    }
}

fn finish_main_agent_worker_start_fence(
    context: &CliContext,
    session_id: &str,
    session_incarnation: &str,
    lease_id: &str,
    succeeded: bool,
    required: bool,
    execution_token: Option<&str>,
) -> Result<(), CliError> {
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    let Some(lease) = locked.registry.operations.iter_mut().find(|lease| {
        lease.lease_id == lease_id
            && lease.session_id == session_id
            && lease.session_incarnation == session_incarnation
            && lease.operation == "main-agent-worker-start"
    }) else {
        return if required {
            Err(operation_unavailable())
        } else {
            Ok(())
        };
    };
    if execution_token
        .is_some_and(|token| lease.execution_token_digest != digest_bytes(token.as_bytes()))
    {
        return Err(revision_conflict("operation-revision-conflict"));
    }
    if matches!(lease.state.as_str(), "completed" | "failed" | "abandoned") {
        return Ok(());
    }
    if !matches!(
        lease.state.as_str(),
        "active" | "completing" | "reconcile_pending"
    ) {
        return Err(revision_conflict("operation-revision-conflict"));
    }
    lease.state = if succeeded {
        "completed".to_string()
    } else {
        "failed".to_string()
    };
    lease.revision = lease.revision.saturating_add(1);
    lease.terminal_at_epoch = Some(now);
    lease.outcome = Some(if succeeded { "pass" } else { "fail" }.to_string());
    locked.save()
}

fn main_agent_worker_start_fence_lease_id(
    record: &crate::SessionRecord,
    incarnation: &str,
    fence_key: &str,
) -> String {
    request_digest(
        "main-agent-worker-start-fence",
        &(record.id.as_str(), incarnation, fence_key),
    )
}

fn acquire_main_agent_worker_start_owner_lock(
    context: &CliContext,
    lease_id: &str,
) -> Result<fs::File, CliError> {
    let coordination_root = super::coordination_root(context)?;
    let directory = coordination_root.join("worker-start-fences");
    match fs::symlink_metadata(&directory) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::create_dir(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(super::store_unavailable()),
            }
        }
        Err(_) => return Err(super::store_unavailable()),
    }
    let directory_metadata =
        fs::symlink_metadata(&directory).map_err(|_| super::store_unavailable())?;
    if directory_metadata.file_type().is_symlink()
        || !directory_metadata.is_dir()
        || directory_metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(super::store_untrusted());
    }
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|_| super::store_unavailable())?;
    let canonical_directory =
        fs::canonicalize(&directory).map_err(|_| super::store_unavailable())?;
    if !canonical_directory
        .starts_with(fs::canonicalize(coordination_root).map_err(|_| super::store_unavailable())?)
    {
        return Err(super::store_untrusted());
    }
    let shard_digest = digest_bytes(lease_id.as_bytes());
    let shard_hex = shard_digest
        .strip_prefix("sha256:")
        .unwrap_or(&shard_digest);
    let shard = shard_hex.get(..2).ok_or_else(super::store_corrupt)?;
    // A fixed 256-way shard set keeps owner-lock storage bounded. Unrelated
    // starts that collide on one shard only serialize for the short
    // launch/attachment window; their registry leases and tokens stay distinct.
    let path = directory.join(format!("shard-{shard}.lock"));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| super::store_unavailable())?;
    let metadata = file.metadata().map_err(|_| super::store_unavailable())?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
    {
        return Err(super::store_untrusted());
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut contention_reported = false;
    loop {
        // SAFETY: `file` owns a valid descriptor for the lifetime of the
        // returned fence. The bounded nonblocking loop lets unrelated shard
        // collisions serialize and prevents an exact replay from stealing.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        #[cfg(debug_assertions)]
        if !contention_reported
            && let Some(path) =
                env::var_os("NILS_AGENT_SESSION_TEST_FENCE_CONTENDER_READY").map(PathBuf::from)
        {
            fs::write(path, b"contended\n").map_err(|_| super::store_unavailable())?;
            contention_reported = true;
        }
        if Instant::now() >= deadline {
            return Err(CliError::runtime(
                "worker-start-fence-wait-timeout",
                "worker start authority remained owned past the bounded wait",
                Some(json!({ "retryable": true })),
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ = contention_reported;
    Ok(file)
}

pub(crate) fn finish_retained_main_agent_worker_start_fence(
    context: &CliContext,
    record: &crate::SessionRecord,
    incarnation: &str,
    fence_key: &str,
) -> Result<(), CliError> {
    let lease_id = main_agent_worker_start_fence_lease_id(record, incarnation, fence_key);
    finish_main_agent_worker_start_fence(
        context,
        &record.id,
        incarnation,
        &lease_id,
        true,
        false,
        None,
    )
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CompletionEvent {
    pub schema_version: String,
    pub event_id: String,
    pub session_id: String,
    pub session_incarnation: String,
    pub lease_id: String,
    pub if_revision: u64,
    pub execution_token_digest: String,
    pub outcome: String,
    pub idempotency_key: String,
    pub request_digest: String,
    pub created_at_epoch: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OperationTargetsInput {
    schema_version: String,
    #[serde(default)]
    targets: Vec<Scope>,
    #[serde(default)]
    provider_refs: Vec<ProviderRef>,
    #[serde(default)]
    checkouts: Vec<CheckoutBinding>,
    #[serde(default)]
    descendant: Option<DescendantIdentity>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DescendantIdentity {
    pub pid: u32,
    pub start_time: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReconcileProof {
    schema_version: String,
    execution_token: String,
    outcome: String,
}

pub(crate) fn claim(context: &CliContext, args: WorkContextClaimArgs) -> Result<Value, CliError> {
    claim_impl(context, args, None, false)
}

pub(crate) fn claim_main_agent_worker(
    context: &CliContext,
    args: WorkContextClaimArgs,
    previous_incarnation: Option<&str>,
) -> Result<Value, CliError> {
    claim_impl(context, args, previous_incarnation, true)
}

fn claim_impl(
    context: &CliContext,
    args: WorkContextClaimArgs,
    resume_from_incarnation: Option<&str>,
    checkout_shell_grant: bool,
) -> Result<Value, CliError> {
    let (record, incarnation) =
        authenticate_from_file(context, &args.session, args.capability_file.as_deref())?;
    let candidate: WorkContextInput =
        read_bounded_json(&args.file, 16 * 1024, "invalid-work-context")?;
    let mut candidate = candidate.validate_and_canonicalize()?;
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    crate::orchestration::ensure_session_not_quarantined(context, &record)?;
    ensure_fingerprint_key(&mut locked.registry);
    let record_cwd = std::path::Path::new(&record.cwd);
    let checkout = checkout_root(record_cwd).unwrap_or_else(|_| record_cwd.to_path_buf());
    let checkout_fingerprint = worktree_fingerprint(&locked.registry, &checkout)?;
    if !candidate.worktrees.contains(&checkout_fingerprint) {
        if candidate.worktrees.len() >= 8 {
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
            "resume_from_incarnation": resume_from_incarnation,
            "checkout_shell_grant": checkout_shell_grant,
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
    if let Some(previous_incarnation) = resume_from_incarnation {
        if previous_incarnation == incarnation {
            return Err(CliError::data(
                "worker-resume-claim-conflict",
                "resumed worker claim must name a distinct prior incarnation",
                None,
            ));
        }
        let stale_claim_ids = locked
            .registry
            .claims
            .iter()
            .filter(|claim| {
                claim.session_id == record.id
                    && claim.session_incarnation == previous_incarnation
                    && claim.state == "active"
            })
            .map(|claim| claim.claim_id.clone())
            .collect::<Vec<_>>();
        if stale_claim_ids
            .iter()
            .any(|claim_id| has_nonterminal_operation(&locked.registry, claim_id))
        {
            return Err(operation_in_progress());
        }
        for claim in &mut locked.registry.claims {
            if stale_claim_ids.contains(&claim.claim_id) {
                claim.state = "released".to_string();
                claim.revision = claim.revision.saturating_add(1);
                claim.updated_at = timestamp(now);
                claim.terminal_at_epoch = Some(now);
            }
        }
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

    let complete =
        complete_relevant_universe(context, &locked.registry, Some((&record.id, &incarnation)));
    let evaluation = evaluate(
        Some((&record.id, &incarnation)),
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
        checkout_shell_grant,
        provider_refs: candidate.provider_refs,
        plan_refs: candidate.plan_refs,
        scopes: candidate.scopes,
        summary: candidate.summary,
        updated_at: timestamp(now),
        expires_at: timestamp(now.saturating_add(CLAIM_TTL_SECS)),
        expires_at_epoch: now.saturating_add(CLAIM_TTL_SECS),
        terminal_at_epoch: None,
    };
    if serde_json::to_vec(&claim).map_or(true, |bytes| bytes.len() > 16 * 1024) {
        return Err(CliError::data(
            "invalid-work-context",
            "public work context exceeds 16 KiB",
            None,
        ));
    }
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
    )?;
    locked.save()?;
    Ok(outcome)
}

pub(crate) fn set_declared(
    context: &CliContext,
    record: &crate::SessionRecord,
    incarnation: &str,
    candidate: WorkContextInput,
    reject_conflict: bool,
) -> Result<Value, CliError> {
    let mut candidate = candidate.validate_and_canonicalize()?;
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    ensure_fingerprint_key(&mut locked.registry);
    let checkout = checkout_root(std::path::Path::new(&record.cwd))?;
    let checkout_fingerprint = worktree_fingerprint(&locked.registry, &checkout)?;
    if !candidate.worktrees.contains(&checkout_fingerprint) {
        if candidate.worktrees.len() >= 8 {
            return Err(CliError::data(
                "invalid-work-context",
                "work context exceeds the worktree fingerprint limit",
                None,
            ));
        }
        candidate.worktrees.push(checkout_fingerprint);
        candidate.worktrees.sort();
    }
    clean_expired(&mut locked.registry, now);
    ensure_current_broker(context, &locked.registry, &record.id, incarnation)?;
    let existing_index = locked.registry.claims.iter().position(|claim| {
        claim.session_id == record.id
            && claim.session_incarnation == incarnation
            && claim.state == "active"
    });
    let complete =
        complete_relevant_universe(context, &locked.registry, Some((&record.id, incarnation)));
    let evaluation = evaluate(
        Some((&record.id, incarnation)),
        &candidate,
        &locked.registry.claims,
        complete,
        !reject_conflict,
    );
    if reject_conflict && evaluation.classification == ConflictClassification::Conflict {
        return Err(CliError::data(
            "claim-conflict",
            "work context conflicts with an active claim",
            Some(json!({ "evaluation": evaluation })),
        ));
    }
    if let Some(index) = existing_index {
        let existing = &locked.registry.claims[index];
        if input_from_record(existing) == candidate {
            return Ok(json!({
                "schema_version": "agent-session.work-context-set-result.v1",
                "changed": false,
                "context": public_context(existing)?,
                "evaluation": evaluation,
            }));
        }
        if has_nonterminal_operation(&locked.registry, &existing.claim_id) {
            return Err(operation_in_progress());
        }
        let existing = &mut locked.registry.claims[index];
        existing.state = "released".to_string();
        existing.revision = existing.revision.saturating_add(1);
        existing.updated_at = timestamp(now);
        existing.terminal_at_epoch = Some(now);
    }
    let claim = WorkContextRecord {
        schema_version: WORK_CONTEXT_VERSION.to_string(),
        session_id: record.id.clone(),
        session_incarnation: incarnation.to_string(),
        claim_id: uuid::Uuid::new_v4().to_string(),
        revision: 1,
        state: "active".to_string(),
        intent: candidate.intent,
        tier: candidate.tier,
        repositories: candidate.repositories,
        worktrees: candidate.worktrees,
        checkout_shell_grant: false,
        provider_refs: candidate.provider_refs,
        plan_refs: candidate.plan_refs,
        scopes: candidate.scopes,
        summary: candidate.summary,
        updated_at: timestamp(now),
        expires_at: timestamp(now.saturating_add(CLAIM_TTL_SECS)),
        expires_at_epoch: now.saturating_add(CLAIM_TTL_SECS),
        terminal_at_epoch: None,
    };
    if serde_json::to_vec(&claim).map_or(true, |bytes| bytes.len() > 16 * 1024) {
        return Err(CliError::data(
            "invalid-work-context",
            "public work context exceeds 16 KiB",
            None,
        ));
    }
    locked.registry.claims.push(claim.clone());
    locked.save()?;
    Ok(json!({
        "schema_version": "agent-session.work-context-set-result.v1",
        "changed": true,
        "context": public_context(&claim)?,
        "evaluation": evaluation,
    }))
}

pub(crate) fn clear_declared(
    context: &CliContext,
    record: &crate::SessionRecord,
    incarnation: &str,
) -> Result<Value, CliError> {
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    clean_expired(&mut locked.registry, now);
    ensure_current_broker(context, &locked.registry, &record.id, incarnation)?;
    let Some(index) = locked.registry.claims.iter().position(|claim| {
        claim.session_id == record.id
            && claim.session_incarnation == incarnation
            && claim.state == "active"
    }) else {
        return Ok(json!({
            "schema_version": "agent-session.work-context-clear-result.v1",
            "released": false,
        }));
    };
    let claim_id = locked.registry.claims[index].claim_id.clone();
    if has_nonterminal_operation(&locked.registry, &claim_id) {
        return Err(operation_in_progress());
    }
    let claim = &mut locked.registry.claims[index];
    claim.state = "released".to_string();
    claim.revision = claim.revision.saturating_add(1);
    claim.updated_at = timestamp(now);
    claim.terminal_at_epoch = Some(now);
    locked.save()?;
    Ok(json!({
        "schema_version": "agent-session.work-context-clear-result.v1",
        "released": true,
        "claim_id": claim_id,
    }))
}

pub(crate) fn show(context: &CliContext, args: WorkContextShowArgs) -> Result<Value, CliError> {
    let locked = lock_registry(context)?;
    let claim = active_claim_for_session(&locked.registry, &args.session)?;
    public_context(claim)
}

pub(crate) fn check(context: &CliContext, args: WorkContextCheckArgs) -> Result<Value, CliError> {
    let selector_count = usize::from(args.self_selector)
        + usize::from(args.session.is_some())
        + usize::from(args.candidate.is_some());
    if selector_count != 1 {
        return Err(CliError::usage(
            "invalid-check-selector",
            "work-context check requires exactly one of --self, --session, or --candidate",
            None,
        ));
    }
    let (candidate, excluded): (WorkContextInput, Option<(String, String)>) = if args.self_selector
    {
        let (record, incarnation) =
            authenticate_any_from_file(context, args.capability_file.as_deref())?;
        let locked = lock_registry(context)?;
        (
            input_from_record(active_claim(&locked.registry, &record.id, &incarnation)?),
            Some((record.id, incarnation)),
        )
    } else if let Some(session) = args.session.as_deref() {
        let locked = lock_registry(context)?;
        let claim = active_claim_for_session(&locked.registry, session)?;
        (
            input_from_record(claim),
            Some((claim.session_id.clone(), claim.session_incarnation.clone())),
        )
    } else {
        let path = args.candidate.as_deref().expect("selector checked");
        (
            read_bounded_json::<WorkContextInput>(path, 16 * 1024, "invalid-work-context")?
                .validate_and_canonicalize()?,
            None,
        )
    };
    let locked = lock_registry(context)?;
    let complete = complete_relevant_universe(
        context,
        &locked.registry,
        excluded
            .as_ref()
            .map(|(session, incarnation)| (session.as_str(), incarnation.as_str())),
    );
    json_value(evaluate(
        excluded
            .as_ref()
            .map(|(session, incarnation)| (session.as_str(), incarnation.as_str())),
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
    )?;
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
    )?;
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
    validate_descendant(input.descendant.as_ref())?;
    let targets = canonicalize_targets(input.targets)?;
    let provider_targets = canonicalize_provider_refs(input.provider_refs)?;
    if targets.is_empty() && provider_targets.is_empty() {
        return Err(CliError::data(
            "invalid-scope",
            "operation targets must name at least one filesystem or provider mutation",
            None,
        ));
    }
    let digest = request_digest(
        "work-context-admit",
        &json!({
            "claim": args.claim,
            "if_revision": args.if_revision,
            "operation": args.operation,
            "targets": targets,
            "provider_targets": provider_targets,
            "checkouts": input.checkouts,
            "descendant": input.descendant,
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
        lease.session_id == record.id
            && matches!(
                lease.state.as_str(),
                "active" | "completing" | "reconcile_pending"
            )
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
    let ordinary_scope_coverage = targets
        .iter()
        .all(|target| claim.scopes.iter().any(|scope| scope_covers(scope, target)));
    let checkout_bound_shell = !ordinary_scope_coverage
        && checkout_bound_shell_is_covered(
            &args.operation,
            &targets,
            &input.checkouts,
            claim,
            &locked.registry,
        )?;
    if !ordinary_scope_coverage && !checkout_bound_shell {
        return Err(CliError::data(
            "uncovered-mutation-scope",
            "operation target is not covered by the active claim",
            None,
        ));
    }
    if provider_targets
        .iter()
        .any(|target| !claim.provider_refs.contains(target))
    {
        return Err(CliError::data(
            "uncovered-mutation-scope",
            "provider mutation target is not covered by the active claim",
            None,
        ));
    }
    validate_physical_targets(&record.cwd, &targets, &input.checkouts)?;
    let candidate = input_from_record(claim);
    let complete =
        complete_relevant_universe(context, &locked.registry, Some((&record.id, &incarnation)));
    let evaluation = evaluate(
        Some((&record.id, &incarnation)),
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
        provider_targets,
        state: "active".to_string(),
        revision: 1,
        started_at: timestamp(now),
        expires_at: timestamp(now.saturating_add(OPERATION_TTL_SECS)),
        expires_at_epoch: now.saturating_add(OPERATION_TTL_SECS),
        terminal_at_epoch: None,
        execution_token_digest: digest_bytes(execution_token.as_bytes()),
        activity_revision: activity.revision,
        activity_identity_digest: activity_identity_digest(&activity),
        runtime_identity_digest: runtime.identity_digest,
        descendant: input.descendant,
        reconcile_observed_at_epoch: None,
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
    )?;
    locked.save()?;
    Ok(outcome)
}

fn checkout_bound_shell_is_covered(
    operation: &str,
    targets: &[Scope],
    bindings: &[CheckoutBinding],
    claim: &WorkContextRecord,
    registry: &Registry,
) -> Result<bool, CliError> {
    if !claim.checkout_shell_grant
        || operation != "shell"
        || targets.len() != 1
        || bindings.len() != 1
    {
        return Ok(false);
    }
    let target = &targets[0];
    if target.kind != ScopeKind::Repository || target.value != "." {
        return Ok(false);
    }
    let binding = &bindings[0];
    if canonical_repository(binding.repository.clone())? != target.repository
        || !claim.repositories.contains(&target.repository)
    {
        return Ok(false);
    }
    let checkout = checkout_root(std::path::Path::new(&binding.path))?;
    let fingerprint = worktree_fingerprint(registry, &checkout)?;
    Ok(claim.worktrees.contains(&fingerprint))
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
        .iter()
        .find(|lease| {
            lease.session_id == record.id
                && lease.session_incarnation == incarnation
                && lease.lease_id == args.lease
        })
        .ok_or_else(operation_unavailable)?;
    if lease.revision != args.if_revision
        || !matches!(
            lease.state.as_str(),
            "active" | "completing" | "reconcile_pending"
        )
    {
        return Err(revision_conflict("operation-revision-conflict"));
    }
    if lease.execution_token_digest != token_digest {
        return Err(super::unauthorized());
    }
    let event_id = request_digest(
        "operation-completion-event",
        &(&record.id, &incarnation, &args.lease, &args.idempotency_key),
    );
    let pending = locked
        .registry
        .completion_events
        .iter()
        .find(|event| event.event_id == event_id);
    if pending.is_some_and(|event| event.request_digest != digest) {
        return Err(CliError::data(
            "idempotency-key-reused",
            "idempotency key is already bound to another request",
            None,
        ));
    }
    if pending.is_none() {
        if locked
            .registry
            .completion_events
            .iter()
            .filter(|event| event.session_id == record.id)
            .count()
            >= 256
        {
            return Err(CliError::data(
                "quota-exceeded",
                "operation completion queue quota exceeded",
                None,
            ));
        }
        locked.registry.completion_events.push(CompletionEvent {
            schema_version: "agent-session.operation-completion-event.v1".to_string(),
            event_id: event_id.clone(),
            session_id: record.id.clone(),
            session_incarnation: incarnation.clone(),
            lease_id: args.lease.clone(),
            if_revision: args.if_revision,
            execution_token_digest: token_digest,
            outcome: args.outcome.as_str().to_string(),
            idempotency_key: args.idempotency_key.clone(),
            request_digest: digest.clone(),
            created_at_epoch: now,
        });
        locked.save()?;
    }
    drop(locked);

    drain_completion_events(context)?;
    let locked = lock_registry(context)?;
    idempotency_replay(
        &locked.registry,
        &args.idempotency_key,
        &record.id,
        &incarnation,
        "work-context-complete",
        &digest,
    )?
    .ok_or_else(operation_unavailable)
}

pub(crate) fn drain_completion_events(context: &CliContext) -> Result<usize, CliError> {
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    let cleaned = clean_expired(&mut locked.registry, now);
    let drained = drain_completion_events_in_registry(&mut locked.registry, now)?;
    if cleaned || drained > 0 {
        locked.save()?;
    }
    Ok(drained)
}

fn drain_completion_events_in_registry(
    registry: &mut Registry,
    now: i64,
) -> Result<usize, CliError> {
    let event_ids: Vec<_> = registry
        .completion_events
        .iter()
        .map(|event| event.event_id.clone())
        .collect();
    let mut drained = 0;
    for event_id in event_ids {
        let Some(event) = registry
            .completion_events
            .iter()
            .find(|event| event.event_id == event_id)
            .cloned()
        else {
            continue;
        };
        let Some(index) = registry.operations.iter().position(|lease| {
            lease.session_id == event.session_id
                && lease.session_incarnation == event.session_incarnation
                && lease.lease_id == event.lease_id
        }) else {
            continue;
        };
        let lease = &registry.operations[index];
        let revision_matches = lease.revision == event.if_revision
            || (matches!(lease.state.as_str(), "completing" | "reconcile_pending")
                && lease.revision == event.if_revision.saturating_add(1));
        if !revision_matches
            || lease.execution_token_digest != event.execution_token_digest
            || !matches!(
                lease.state.as_str(),
                "active" | "completing" | "reconcile_pending"
            )
        {
            continue;
        }
        let mut completed = lease.clone();
        completed.revision = completed.revision.saturating_add(1);
        completed.state = if event.outcome == "pass" {
            "completed".to_string()
        } else {
            "failed".to_string()
        };
        completed.outcome = Some(event.outcome.clone());
        completed.terminal_at_epoch = Some(now);
        let outcome = public_lease(&completed)?;
        if store_receipt(
            registry,
            event.idempotency_key,
            event.session_id,
            event.session_incarnation,
            "work-context-complete".to_string(),
            event.request_digest,
            outcome,
            now,
        )
        .is_err()
        {
            continue;
        }
        registry.operations[index] = completed;
        registry
            .completion_events
            .retain(|candidate| candidate.event_id != event_id);
        drained += 1;
    }
    Ok(drained)
}

pub(crate) fn reconcile(
    context: &CliContext,
    args: WorkContextReconcileArgs,
) -> Result<Value, CliError> {
    let (record, incarnation) =
        authenticate_from_file(context, &args.session, args.capability_file.as_deref())?;
    let _session_fence = crate::acquire_session_record_lock(context, &record.id)?;
    let record = crate::load_session_record(context, &record.id)?;
    if super::incarnation(&record)? != incarnation {
        return Err(super::unauthorized());
    }
    let _activity_fence = crate::activity::acquire_coordination_activity_lock(context, &record.id)?;
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
    if drain_completion_events_in_registry(&mut locked.registry, now)? > 0 {
        locked.save()?;
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
    if !matches!(
        lease_snapshot.state.as_str(),
        "active" | "completing" | "reconcile_pending"
    ) {
        return Err(revision_conflict("operation-revision-conflict"));
    }
    let runtime = crate::coordination_runtime_evidence(&record)?;
    if runtime.identity_digest != lease_snapshot.runtime_identity_digest
        || runtime.status == crate::CoordinationRuntimeStatus::Unknown
    {
        return Err(CliError::runtime(
            "coordination-runtime-unverified",
            "the exact operation runtime could not be verified",
            None,
        ));
    }
    let runtime_stopped = runtime.status == crate::CoordinationRuntimeStatus::Stopped;
    if !runtime_stopped && !controller_observed_quiescent(context, &record, &lease_snapshot) {
        return Err(CliError::data(
            "operation-still-running",
            "controller-owned activity or descendant evidence still identifies the operation as live",
            None,
        ));
    }
    if !runtime_stopped {
        if lease_snapshot.state != "reconcile_pending" {
            let lease = locked
                .registry
                .operations
                .iter_mut()
                .find(|lease| lease.lease_id == lease_snapshot.lease_id)
                .ok_or_else(operation_unavailable)?;
            lease.state = "reconcile_pending".to_string();
            lease.revision = lease.revision.saturating_add(1);
            lease.reconcile_observed_at_epoch = Some(now);
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
            )?;
            locked.save()?;
            return Ok(outcome);
        }
        let observed = lease_snapshot.reconcile_observed_at_epoch.unwrap_or(now);
        if observed > now.saturating_sub(5) {
            return Err(CliError::data(
                "operation-reconcile-pending",
                "operation quiescence requires a second observation after five seconds",
                Some(json!({ "lease": args.lease, "revision": lease_snapshot.revision })),
            ));
        }
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
    lease.reconcile_observed_at_epoch = None;
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
    )?;
    locked.save()?;
    Ok(outcome)
}

pub(crate) fn ensure_current_broker(
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
    if !super::broker::capability_available(
        context,
        session_id,
        incarnation,
        &broker.capability_digest,
    ) {
        return Err(CliError::runtime(
            "coordination-broker-lost",
            "coordination broker capability is unavailable",
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

pub(crate) fn active_claim<'a>(
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

pub(crate) fn acquire_main_agent_worker_start_fence(
    context: &CliContext,
    record: &crate::SessionRecord,
    incarnation: &str,
    fence_key: &str,
) -> Result<MainAgentWorkerStartFence, CliError> {
    let now = now_epoch();
    let lease_id = main_agent_worker_start_fence_lease_id(record, incarnation, fence_key);
    let owner_lock = acquire_main_agent_worker_start_owner_lock(context, &lease_id)?;
    let mut locked = lock_registry(context)?;
    clean_expired(&mut locked.registry, now);
    ensure_current_broker(context, &locked.registry, &record.id, incarnation)?;
    let claim = active_claim(&locked.registry, &record.id, incarnation)?.clone();
    let execution_token = uuid::Uuid::new_v4().to_string();
    if let Some(lease) = locked.registry.operations.iter_mut().find(|lease| {
        lease.lease_id == lease_id
            && lease.session_id == record.id
            && lease.session_incarnation == incarnation
            && lease.operation == "main-agent-worker-start"
    }) {
        lease.claim_id = claim.claim_id;
        lease.claim_revision = claim.revision;
        lease.state = "active".to_string();
        lease.revision = lease.revision.saturating_add(1);
        lease.expires_at = timestamp(now.saturating_add(OPERATION_TTL_SECS));
        lease.expires_at_epoch = now.saturating_add(OPERATION_TTL_SECS);
        lease.terminal_at_epoch = None;
        lease.outcome = None;
        lease.execution_token_digest = digest_bytes(execution_token.as_bytes());
        locked.save()?;
        return Ok(MainAgentWorkerStartFence {
            lease_id,
            session_id: record.id.clone(),
            session_incarnation: incarnation.to_string(),
            execution_token,
            _owner_lock: owner_lock,
        });
    }
    locked.registry.operations.push(OperationLease {
        schema_version: OPERATION_LEASE_VERSION.to_string(),
        lease_id: lease_id.clone(),
        session_id: record.id.clone(),
        session_incarnation: incarnation.to_string(),
        claim_id: claim.claim_id,
        claim_revision: claim.revision,
        operation: "main-agent-worker-start".to_string(),
        targets: Vec::new(),
        provider_targets: Vec::new(),
        state: "active".to_string(),
        revision: 1,
        started_at: timestamp(now),
        expires_at: timestamp(now.saturating_add(OPERATION_TTL_SECS)),
        expires_at_epoch: now.saturating_add(OPERATION_TTL_SECS),
        terminal_at_epoch: None,
        execution_token_digest: digest_bytes(execution_token.as_bytes()),
        activity_revision: 0,
        activity_identity_digest: String::new(),
        runtime_identity_digest: String::new(),
        descendant: None,
        reconcile_observed_at_epoch: None,
        outcome: None,
    });
    locked.save()?;
    Ok(MainAgentWorkerStartFence {
        lease_id,
        session_id: record.id.clone(),
        session_incarnation: incarnation.to_string(),
        execution_token,
        _owner_lock: owner_lock,
    })
}

/// Return whether the authenticated worker's active claim is the exact
/// assignment-derived context carrying the private checkout-shell grant.
///
/// `None` means no active claim. `Some(false)` is deliberately distinct: a
/// worker must not bootstrap over a pre-existing arbitrary claim.
pub(crate) fn main_agent_worker_claim_match(
    context: &CliContext,
    record: &crate::SessionRecord,
    candidate: &WorkContextInput,
) -> Result<Option<bool>, CliError> {
    let incarnation = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(claim_unavailable)?;
    let mut expected = candidate.clone().validate_and_canonicalize()?;
    let locked = lock_registry(context)?;
    ensure_current_broker(context, &locked.registry, &record.id, incarnation)?;
    let Some(claim) = locked.registry.claims.iter().find(|claim| {
        claim.session_id == record.id
            && claim.session_incarnation == incarnation
            && claim.state == "active"
    }) else {
        return Ok(None);
    };
    let record_cwd = std::path::Path::new(&record.cwd);
    let checkout = checkout_root(record_cwd).unwrap_or_else(|_| record_cwd.to_path_buf());
    let fingerprint = worktree_fingerprint(&locked.registry, &checkout)?;
    if !expected.worktrees.contains(&fingerprint) {
        expected.worktrees.push(fingerprint);
        expected.worktrees.sort();
    }
    Ok(Some(
        claim.checkout_shell_grant && input_from_record(claim) == expected,
    ))
}

fn active_claim_for_session<'a>(
    registry: &'a Registry,
    session_id: &str,
) -> Result<&'a WorkContextRecord, CliError> {
    let broker = registry
        .brokers
        .get(session_id)
        .ok_or_else(claim_unavailable)?;
    active_claim(registry, session_id, &broker.incarnation)
}

pub(crate) fn input_from_record(claim: &WorkContextRecord) -> WorkContextInput {
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

pub(crate) fn conflict_severity_for_session(
    context: &CliContext,
    registry: &Registry,
    session_id: &str,
) -> Option<String> {
    let broker = registry.brokers.get(session_id)?;
    let claim = registry.claims.iter().find(|claim| {
        claim.session_id == session_id
            && claim.session_incarnation == broker.incarnation
            && claim.state == "active"
    })?;
    let candidate = input_from_record(claim);
    let complete =
        complete_relevant_universe(context, registry, Some((session_id, &broker.incarnation)));
    let classification = evaluate(
        Some((session_id, &broker.incarnation)),
        &candidate,
        &registry.claims,
        complete,
        false,
    )
    .classification;
    Some(
        match classification {
            ConflictClassification::Conflict => "conflict",
            ConflictClassification::PotentialConflict => "potential_conflict",
            ConflictClassification::Unknown => "unknown",
            ConflictClassification::NoKnownConflict => "no_known_conflict",
            ConflictClassification::Clear => "clear",
        }
        .to_string(),
    )
}

pub(crate) fn public_context(claim: &WorkContextRecord) -> Result<Value, CliError> {
    let mut value = json_value(claim)?;
    let object = value
        .as_object_mut()
        .expect("work context serializes as an object");
    object.remove("expires_at_epoch");
    object.remove("terminal_at_epoch");
    object.remove("checkout_shell_grant");
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
    object.remove("activity_identity_digest");
    object.remove("runtime_identity_digest");
    object.remove("descendant");
    Ok(value)
}

fn complete_relevant_universe(
    context: &CliContext,
    registry: &Registry,
    excluded_principal: Option<(&str, &str)>,
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
        if excluded_principal.is_some_and(|(subject_id, _)| id == subject_id) {
            continue;
        }
        let Some(broker) = registry.brokers.get(&id) else {
            return false;
        };
        if broker.state == "stopped" {
            continue;
        }
        if broker.state != "ready"
            || !super::broker::heartbeat_fresh(
                context,
                &id,
                &broker.incarnation,
                broker.heartbeat_epoch,
            )
        {
            return false;
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
            && matches!(
                operation.state.as_str(),
                "active" | "completing" | "reconcile_pending"
            )
    })
}

fn controller_observed_quiescent(
    context: &CliContext,
    record: &crate::SessionRecord,
    lease: &OperationLease,
) -> bool {
    if lease.descendant.as_ref().is_some_and(descendant_is_live) {
        return false;
    }
    let Some(activity) = crate::activity::state_for_view(context, record) else {
        return false;
    };
    controller_activity_supersedes_lease(&activity, &lease.activity_identity_digest)
}

fn controller_activity_supersedes_lease(
    activity: &crate::activity::TurnState,
    lease_activity_identity_digest: &str,
) -> bool {
    if activity_identity_digest(activity) == lease_activity_identity_digest {
        return false;
    }
    match activity.phase {
        crate::activity::TurnPhase::Waiting => activity.current_turn.is_none(),
        crate::activity::TurnPhase::Starting
        | crate::activity::TurnPhase::Working
        | crate::activity::TurnPhase::NeedsInput => activity.current_turn.is_some(),
        crate::activity::TurnPhase::Unknown => false,
    }
}

pub(crate) fn operator_reconcile_in_registry(
    context: &CliContext,
    registry: &mut Registry,
    record: &crate::SessionRecord,
    lease_id: &str,
    if_revision: u64,
    now: i64,
) -> Result<Value, CliError> {
    let incarnation = super::incarnation(record)?;
    let snapshot = registry
        .operations
        .iter()
        .find(|lease| {
            lease.lease_id == lease_id
                && lease.session_id == record.id
                && lease.session_incarnation == incarnation
        })
        .cloned()
        .ok_or_else(operation_unavailable)?;
    if snapshot.revision != if_revision
        || !matches!(
            snapshot.state.as_str(),
            "active" | "completing" | "reconcile_pending"
        )
    {
        return Err(revision_conflict("operation-revision-conflict"));
    }
    if !controller_observed_quiescent(context, record, &snapshot) {
        return Err(CliError::data(
            "operation-still-running",
            "operator reconciliation could not prove the operation inactive",
            None,
        ));
    }
    let lease = registry
        .operations
        .iter_mut()
        .find(|lease| lease.lease_id == lease_id)
        .ok_or_else(operation_unavailable)?;
    lease.state = "abandoned".to_string();
    lease.revision = lease.revision.saturating_add(1);
    lease.terminal_at_epoch = Some(now);
    lease.reconcile_observed_at_epoch = None;
    lease.outcome = Some("operator-attested-inactive".to_string());
    public_lease(lease)
}

pub(crate) fn activity_identity_digest(activity: &crate::activity::TurnState) -> String {
    let identity = if let Some(turn) = activity.current_turn.as_ref() {
        format!(
            "turn:{}",
            turn.provider_turn_id
                .as_deref()
                .unwrap_or(turn.started_at.as_str())
        )
    } else if let Some(turn) = activity.last_turn.as_ref() {
        format!(
            "idle:{}:{}",
            turn.provider_turn_id.as_deref().unwrap_or_default(),
            turn.completed_at
        )
    } else {
        "idle:initial".to_string()
    };
    digest_bytes(identity.as_bytes())
}

pub(crate) fn descendant_is_live(descendant: &DescendantIdentity) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", descendant.pid)) else {
            return false;
        };
        let Some(end) = stat.rfind(')') else {
            return false;
        };
        let fields: Vec<_> = stat[end + 1..].split_whitespace().collect();
        fields.get(19).and_then(|value| value.parse::<u64>().ok()) == Some(descendant.start_time)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = descendant;
        true
    }
}

fn validate_descendant(descendant: Option<&DescendantIdentity>) -> Result<(), CliError> {
    let Some(descendant) = descendant else {
        return Ok(());
    };
    if descendant.pid <= 1 || descendant.start_time == 0 {
        return Err(CliError::data(
            "invalid-scope",
            "descendant identity requires a positive non-system PID and start time",
            None,
        ));
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(CliError::runtime(
            "coordination-unavailable",
            "exact descendant identity is unsupported on this platform",
            None,
        ))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(())
    }
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
    use serde_json::json;

    #[test]
    fn operation_private_proofs_are_never_serialized() {
        let lease = OperationLease {
            schema_version: OPERATION_LEASE_VERSION.to_string(),
            lease_id: "lease".to_string(),
            session_id: "session".to_string(),
            session_incarnation: "incarnation".to_string(),
            claim_id: "claim".to_string(),
            claim_revision: 1,
            operation: "edit".to_string(),
            targets: Vec::new(),
            provider_targets: Vec::new(),
            state: "active".to_string(),
            revision: 1,
            started_at: "time".to_string(),
            expires_at: "time".to_string(),
            expires_at_epoch: 0,
            terminal_at_epoch: None,
            execution_token_digest: "canary".to_string(),
            activity_revision: 1,
            activity_identity_digest: "activity".to_string(),
            runtime_identity_digest: "runtime".to_string(),
            descendant: Some(DescendantIdentity {
                pid: 42,
                start_time: 99,
            }),
            reconcile_observed_at_epoch: None,
            outcome: None,
        };
        let value = public_lease(&lease).expect("serialize");
        assert!(!value.to_string().contains("canary"));
        assert!(value.get("activity_identity_digest").is_none());
        assert!(value.get("descendant").is_none());
    }

    #[test]
    fn coordination_review_idless_activity_identity_tracks_turn_generation_not_progress() {
        let turn = |started_at: &str, revision: u64| {
            serde_json::from_value::<crate::activity::TurnState>(json!({
                "schema_version": "agent-session.turn-state.v1",
                "phase": "working",
                "phase_changed_at": started_at,
                "revision": revision,
                "source": {
                    "kind": "provider_hook",
                    "provider": "codex",
                    "confidence": "authoritative"
                },
                "current_turn": {
                    "started_at": started_at,
                    "last_progress_at": started_at
                }
            }))
            .expect("turn state")
        };
        let original = turn("2030-01-01T00:00:00Z", 1);
        let same_turn_progress = turn("2030-01-01T00:00:00Z", 2);
        let next_turn = turn("2030-01-01T00:01:00Z", 3);
        assert_eq!(
            activity_identity_digest(&original),
            activity_identity_digest(&same_turn_progress)
        );
        assert_ne!(
            activity_identity_digest(&original),
            activity_identity_digest(&next_turn)
        );
    }

    #[test]
    fn coordination_review_new_working_turn_supersedes_prior_operation_activity() {
        let turn = |started_at: &str, revision: u64| {
            serde_json::from_value::<crate::activity::TurnState>(json!({
                "schema_version": "agent-session.turn-state.v1",
                "phase": "working",
                "phase_changed_at": started_at,
                "revision": revision,
                "source": {
                    "kind": "provider_hook",
                    "provider": "codex",
                    "confidence": "authoritative"
                },
                "current_turn": {
                    "started_at": started_at,
                    "last_progress_at": started_at
                }
            }))
            .expect("turn state")
        };
        let original = turn("2030-01-01T00:00:00Z", 1);
        let next_working_turn = turn("2030-01-01T00:01:00Z", 2);
        assert!(controller_activity_supersedes_lease(
            &next_working_turn,
            &activity_identity_digest(&original)
        ));
    }

    #[test]
    fn coordination_review_durable_completion_survives_safety_ttl_transition() {
        let mut registry: Registry = serde_json::from_value(json!({
            "schema_version": super::super::REGISTRY_VERSION,
            "operations": [{
                "schema_version": OPERATION_LEASE_VERSION,
                "lease_id": "lease",
                "session_id": "session",
                "session_incarnation": "incarnation",
                "claim_id": "claim",
                "claim_revision": 1,
                "operation": "edit",
                "targets": [],
                "state": "completing",
                "revision": 2,
                "started_at": "2030-01-01T00:00:00Z",
                "expires_at": "2030-01-01T00:00:01Z",
                "expires_at_epoch": 1,
                "execution_token_digest": "token"
            }],
            "completion_events": [{
                "schema_version": "agent-session.operation-completion-event.v1",
                "event_id": "event",
                "session_id": "session",
                "session_incarnation": "incarnation",
                "lease_id": "lease",
                "if_revision": 1,
                "execution_token_digest": "token",
                "outcome": "pass",
                "idempotency_key": "completion-key",
                "request_digest": "request",
                "created_at_epoch": 1
            }]
        }))
        .expect("registry");

        assert_eq!(
            drain_completion_events_in_registry(&mut registry, 2).expect("drain"),
            1
        );
        assert!(registry.completion_events.is_empty());
        assert_eq!(registry.operations[0].state, "completed");
        assert_eq!(registry.operations[0].revision, 3);
        assert_eq!(registry.receipts.len(), 1);
    }

    #[test]
    fn coordination_review_durable_completion_survives_reconcile_pending_transition() {
        let mut registry: Registry = serde_json::from_value(json!({
            "schema_version": super::super::REGISTRY_VERSION,
            "operations": [{
                "schema_version": OPERATION_LEASE_VERSION,
                "lease_id": "lease",
                "session_id": "session",
                "session_incarnation": "incarnation",
                "claim_id": "claim",
                "claim_revision": 1,
                "operation": "edit",
                "targets": [],
                "state": "reconcile_pending",
                "revision": 2,
                "started_at": "2030-01-01T00:00:00Z",
                "expires_at": "2030-01-01T00:00:01Z",
                "expires_at_epoch": 1,
                "execution_token_digest": "token"
            }],
            "completion_events": [{
                "schema_version": "agent-session.operation-completion-event.v1",
                "event_id": "event",
                "session_id": "session",
                "session_incarnation": "incarnation",
                "lease_id": "lease",
                "if_revision": 1,
                "execution_token_digest": "token",
                "outcome": "pass",
                "idempotency_key": "completion-key",
                "request_digest": "request",
                "created_at_epoch": 1
            }]
        }))
        .expect("registry");

        assert_eq!(
            drain_completion_events_in_registry(&mut registry, 2).expect("drain"),
            1
        );
        assert!(registry.completion_events.is_empty());
        assert_eq!(registry.operations[0].state, "completed");
        assert_eq!(registry.operations[0].revision, 3);
    }
}

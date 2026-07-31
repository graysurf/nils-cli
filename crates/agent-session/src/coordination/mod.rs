pub(crate) mod advisory;
pub(crate) mod broker;
pub(crate) mod claims;
pub(crate) mod context;
pub(crate) mod mailbox;
mod notification;
pub(crate) mod server;

pub(crate) use notification::NotificationCandidate;

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jiff::Timestamp;
use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::cli::{self, BrokerCommand, MessageCommand, WorkContextCommand};
use crate::{
    CliContext, CliError, SessionRecord, load_session_record, render_error, render_single_success,
    session_dir,
};

const REGISTRY_VERSION: &str = "agent-session.coordination-registry.v1";
const CLAIM_FENCE_REGISTRY_VERSION: &str = "agent-session.coordination-registry.v2";
const REGISTRY_FILE: &str = "registry.json";
const REGISTRY_LOCK: &str = "registry.lock";
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
// Single source of truth lives in nils-common; keep the projection reader and
// this read/write path on the same whole-registry cap.
const MAX_REGISTRY_BYTES: u64 = nils_common::coordination_projection::MAX_REGISTRY_BYTES;
const RECEIPT_TTL_SECS: i64 = 24 * 60 * 60;
const MAX_RECEIPTS_PER_PRINCIPAL: usize = 4_096;
const MAX_RECEIPTS_GLOBAL: usize = 32_768;
const TERMINAL_RETENTION_SECS: i64 = 5 * 60;
const ACKNOWLEDGED_MESSAGE_RETENTION_SECS: i64 = 24 * 60 * 60;
const CLAIM_MUTATION_FENCES_DIR: &str = "claim-mutation-fences";
const CLAIM_MUTATION_FENCE_LOCKS_DIR: &str = "claim-mutation-fence-locks";
const CLAIM_MUTATION_FENCE_SCHEMA: &str = "agent-session.claim-mutation-fence.v1";
const MAX_CLAIM_MUTATION_FENCE_FILES: usize = 256;
const MAX_CLAIM_MUTATION_OPERATION_FILES: usize = 512;
const MAX_CLAIM_MUTATION_FENCE_BYTES: u64 = 64 * 1024;
pub(crate) const CAPABILITY_ENV: &str = "AGENT_SESSION_CAPABILITY_FILE";
pub(crate) const CHECKPOINT_ENV: &str = "AGENT_SESSION_CHECKPOINT_FILE";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Registry {
    schema_version: String,
    fingerprint_epoch: u64,
    fingerprint_key: String,
    brokers: BTreeMap<String, BrokerRecord>,
    claims: Vec<context::WorkContextRecord>,
    operations: Vec<claims::OperationLease>,
    completion_events: Vec<claims::CompletionEvent>,
    messages: Vec<mailbox::StoredMessage>,
    cursors: BTreeMap<String, mailbox::InboxCursor>,
    receipts: BTreeMap<String, IdempotencyReceipt>,
    notifications: BTreeMap<String, notification::NotificationReceipt>,
    advisory_acknowledgements: BTreeMap<String, advisory::AdvisoryAcknowledgement>,
    advisory_observations: BTreeMap<String, advisory::AdvisoryObservation>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ClaimMutationFence {
    schema_version: String,
    assignment_id: String,
    assignment_revision: u64,
    worker_session_id: String,
    worker_incarnation: String,
    worker_claim: ControllerClaimTuple,
    controller_session_id: String,
    controller_incarnation: String,
    controller_claim: ControllerClaimTuple,
    request_digest: String,
    lock_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct BrokerRecord {
    pub session_id: String,
    pub incarnation: String,
    #[serde(default)]
    pub coordination_mode: crate::cli::CoordinationMode,
    pub capability_digest: String,
    pub generation: u64,
    pub state: String,
    pub heartbeat_at: String,
    pub heartbeat_epoch: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_identity: Option<Value>,
    #[serde(default)]
    pub runtime_identity_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lost_since_epoch: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IdempotencyReceipt {
    principal: String,
    incarnation: String,
    operation: String,
    digest: String,
    outcome: Value,
    expires_at_epoch: i64,
}

pub(crate) struct LockedRegistry {
    _lock: File,
    path: PathBuf,
    pub registry: Registry,
}

impl Drop for LockedRegistry {
    fn drop(&mut self) {
        // SAFETY: the descriptor remains owned by `_lock` for this call.
        unsafe {
            libc::flock(self._lock.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl LockedRegistry {
    pub fn save(&mut self) -> Result<(), CliError> {
        if self.registry.schema_version.is_empty() {
            self.registry.schema_version = REGISTRY_VERSION.to_string();
        }
        if !matches!(
            self.registry.schema_version.as_str(),
            REGISTRY_VERSION | CLAIM_FENCE_REGISTRY_VERSION
        ) {
            return Err(store_corrupt());
        }
        let bytes = serde_json::to_vec_pretty(&self.registry).map_err(|_| store_corrupt())?;
        if bytes.len() as u64 > MAX_REGISTRY_BYTES {
            return Err(CliError::data(
                "quota-exceeded",
                "coordination registry byte limit exceeded",
                None,
            ));
        }
        write_atomic(&self.path, &bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())
    }
}

pub(crate) fn run_work_context(context: &CliContext, args: cli::WorkContextArgs) -> i32 {
    let (command, format, result) = match args.command {
        WorkContextCommand::Status(args) => (
            "work-context-status",
            args.format,
            advisory::status(context, args),
        ),
        WorkContextCommand::Set(args) => (
            "work-context-set",
            args.format,
            advisory::set(context, args),
        ),
        WorkContextCommand::Clear(args) => (
            "work-context-clear",
            args.format,
            advisory::clear(context, args),
        ),
        WorkContextCommand::Advise(args) => (
            "work-context-advise",
            args.format,
            advisory::advise(context, args),
        ),
        WorkContextCommand::Acknowledge(args) => (
            "work-context-acknowledge",
            args.format,
            advisory::acknowledge(context, args),
        ),
        WorkContextCommand::Claim(args) => (
            "work-context-claim",
            args.format,
            claims::claim(context, args),
        ),
        WorkContextCommand::Show(args) => (
            "work-context-show",
            args.format,
            claims::show(context, args),
        ),
        WorkContextCommand::Check(args) => (
            "work-context-check",
            args.format,
            claims::check(context, args),
        ),
        WorkContextCommand::Renew(args) => (
            "work-context-renew",
            args.format,
            claims::renew(context, args),
        ),
        WorkContextCommand::Release(args) => (
            "work-context-release",
            args.format,
            claims::release(context, args),
        ),
        WorkContextCommand::Admit(args) => (
            "work-context-admit",
            args.format,
            claims::admit(context, args),
        ),
        WorkContextCommand::Complete(args) => (
            "work-context-complete",
            args.format,
            claims::complete(context, args),
        ),
        WorkContextCommand::Reconcile(args) => (
            "work-context-reconcile",
            args.format,
            claims::reconcile(context, args),
        ),
    };
    render_coordination(command, format, result)
}

pub(crate) fn run_broker(context: &CliContext, args: cli::BrokerArgs) -> i32 {
    let (command, format, result) = match args.command {
        BrokerCommand::Status(args) => {
            ("broker-status", args.format, broker::status(context, args))
        }
        BrokerCommand::Adopt(args) => (
            "broker-adopt",
            args.format,
            broker::recover(context, args, false),
        ),
        BrokerCommand::Reconcile(args) => (
            "broker-reconcile",
            args.format,
            broker::recover(context, args, true),
        ),
        BrokerCommand::Stop(args) => ("broker-stop", args.format, broker::stop(context, args)),
        BrokerCommand::Heartbeat(args) => (
            "broker-heartbeat",
            args.format,
            broker::run_heartbeat_sidecar(context, args),
        ),
    };
    render_coordination(command, format, result)
}

pub(crate) fn run_message(context: &CliContext, args: cli::MessageArgs) -> i32 {
    let (command, format, result) = match args.command {
        MessageCommand::Send(args) => ("message-send", args.format, mailbox::send(context, args)),
        MessageCommand::Inbox(args) => {
            ("message-inbox", args.format, mailbox::inbox(context, args))
        }
        MessageCommand::Show(args) => ("message-show", args.format, mailbox::show(context, args)),
        MessageCommand::Ack(args) => ("message-ack", args.format, mailbox::ack(context, args)),
        MessageCommand::Reply(args) => {
            ("message-reply", args.format, mailbox::reply(context, args))
        }
        MessageCommand::Wait(args) => ("message-wait", args.format, mailbox::wait(context, args)),
    };
    render_coordination(command, format, result)
}

pub(crate) fn session_has_active_claim_or_operation(
    context: &CliContext,
    session_id: &str,
    incarnation: &str,
) -> Result<(bool, bool), CliError> {
    let locked = lock_registry(context)?;
    let active_claim_ids = locked
        .registry
        .claims
        .iter()
        .filter(|claim| {
            claim.session_id == session_id
                && claim.session_incarnation == incarnation
                && claim.state == "active"
        })
        .map(|claim| claim.claim_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let has_operation = locked.registry.operations.iter().any(|operation| {
        active_claim_ids.contains(operation.claim_id.as_str())
            && matches!(
                operation.state.as_str(),
                "active" | "completing" | "reconcile_pending"
            )
    });
    Ok((!active_claim_ids.is_empty(), has_operation))
}

pub(crate) struct SessionQuiescenceGuard {
    _locked: LockedRegistry,
    session_id: String,
    incarnation: String,
    pub broker_present: bool,
    pub broker_identity_matched: bool,
    pub broker_authoritative: bool,
    pub broker_generation: Option<u64>,
    pub broker_runtime_identity_digest: Option<String>,
    pub broker_lost_since_epoch: Option<i64>,
    pub active_claim: bool,
    pub claim_id: Option<String>,
    pub claim_revision: Option<u64>,
    pub claim_expires_at: Option<String>,
    pub claim_expires_at_epoch: Option<i64>,
    pub active_operation: bool,
    pub uncertain_operation: bool,
}

impl SessionQuiescenceGuard {
    pub(crate) fn begin_notification_attempt(
        &mut self,
        candidate: &NotificationCandidate,
    ) -> Result<bool, CliError> {
        if candidate.target_session_id != self.session_id
            || candidate.target_incarnation != self.incarnation
            || !self.broker_authoritative
            || self.active_claim
            || self.active_operation
            || self.uncertain_operation
        {
            return Ok(false);
        }
        let attempted_at = jiff::Timestamp::now();
        let Some(_) = notification::transition_attempt_at(
            &mut self._locked.registry,
            candidate,
            attempted_at.as_second(),
            attempted_at,
        ) else {
            return Ok(false);
        };
        self._locked.save()?;
        Ok(true)
    }

    pub(crate) fn has_active_claim(&self, session_id: &str, incarnation: &str) -> bool {
        self._locked.registry.claims.iter().any(|claim| {
            claim.session_id == session_id
                && claim.session_incarnation == incarnation
                && claim.state == "active"
        })
    }

    pub(crate) fn guidance_summary(
        &self,
        session_id: &str,
        incarnation: &str,
        controller_session_id: &str,
        controller_incarnation: &str,
    ) -> GuidanceSummary {
        guidance_summary_from_registry(
            &self._locked.registry,
            session_id,
            incarnation,
            controller_session_id,
            controller_incarnation,
            now_epoch(),
        )
    }
}

pub(crate) struct GroupCleanupQuiescenceGuard {
    locked: LockedRegistry,
    sessions: Vec<(String, String)>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ControllerClaimTuple {
    pub(crate) claim_id: String,
    pub(crate) revision: u64,
    pub(crate) expires_at_epoch: i64,
}

pub(crate) struct StoppedWorkerTerminalizationGuard {
    seal: WorkerAuthoritySealGuard,
    worker_claim_observed: bool,
}

pub(crate) struct WorkerClaimRevocationGuard {
    seal: WorkerAuthoritySealGuard,
}

pub(crate) struct WorkerRuntimeStopGuard {
    seal: WorkerAuthoritySealGuard,
}

pub(crate) struct ClaimedWorkerRuntimeStopGuard {
    seal: WorkerAuthoritySealGuard,
    worker_claim: ControllerClaimTuple,
}

pub(crate) struct ClaimMutationFenceOwnerGuard {
    _owner_lock: File,
}

struct WorkerAuthoritySealGuard {
    locked: LockedRegistry,
    worker: (String, String),
    controller: (String, String),
    controller_claim: ControllerClaimTuple,
    allow_active_worker_claims: bool,
    require_unexpired_at_seal: bool,
}

fn controller_claim_matches_admission(
    registry: &Registry,
    controller: &(String, String),
    authorized: &ControllerClaimTuple,
    now: i64,
    require_unexpired: bool,
) -> bool {
    registry.claims.iter().any(|claim| {
        claim.session_id == controller.0
            && claim.session_incarnation == controller.1
            && claim.claim_id == authorized.claim_id
            && claim.revision == authorized.revision
            && claim.expires_at_epoch == authorized.expires_at_epoch
            && claim.state == "active"
            && (!require_unexpired || claim.expires_at_epoch > now)
    })
}

impl WorkerAuthoritySealGuard {
    fn seal(&mut self, context: &CliContext) -> Result<(), CliError> {
        let now = now_epoch();
        let controller_claim_current = controller_claim_matches_admission(
            &self.locked.registry,
            &self.controller,
            &self.controller_claim,
            now,
            self.require_unexpired_at_seal,
        );
        if !controller_claim_current {
            return Err(CliError::data(
                "claim-not-active",
                "Main Agent claim no longer satisfies the exact admitted worker authority seal",
                None,
            ));
        }
        ensure_group_cleanup_quiescence(
            &self.locked.registry,
            std::slice::from_ref(&self.worker),
            self.allow_active_worker_claims,
        )?;
        seal_coordination_sessions(
            &mut self.locked,
            context,
            std::slice::from_ref(&self.worker),
        )
    }
}

impl WorkerRuntimeStopGuard {
    /// Seal only the exact pre-claim worker before its runtime is stopped.
    /// Admission requires an unexpired controller claim. The caller persists
    /// its orchestration reservation while this registry lock is held, then
    /// seals coordination authority before dropping the lock and performing
    /// bounded external runtime termination.
    pub(crate) fn seal(&mut self, context: &CliContext) -> Result<(), CliError> {
        self.seal.seal(context)
    }
}

impl ClaimedWorkerRuntimeStopGuard {
    pub(crate) fn worker_claim(&self) -> &ControllerClaimTuple {
        &self.worker_claim
    }

    /// Persist an exact claim-mutation fence while this guard still owns the
    /// observational coordination lock. External runtime termination can then
    /// proceed without monopolizing the global registry lock.
    pub(crate) fn persist_claim_mutation_fence(
        &mut self,
        context: &CliContext,
        assignment_id: &str,
        assignment_revision: u64,
        request_digest: &str,
    ) -> Result<ClaimMutationFenceOwnerGuard, CliError> {
        let lock_id =
            claim_mutation_fence_lock_id(assignment_id, assignment_revision, request_digest);
        let fence = ClaimMutationFence {
            schema_version: CLAIM_MUTATION_FENCE_SCHEMA.to_string(),
            assignment_id: assignment_id.to_string(),
            assignment_revision,
            worker_session_id: self.seal.worker.0.clone(),
            worker_incarnation: self.seal.worker.1.clone(),
            worker_claim: self.worker_claim.clone(),
            controller_session_id: self.seal.controller.0.clone(),
            controller_incarnation: self.seal.controller.1.clone(),
            controller_claim: self.seal.controller_claim.clone(),
            request_digest: request_digest.to_string(),
            lock_id,
        };
        let owner_lock = acquire_claim_mutation_fence_owner(context, &fence.lock_id)?;
        let persisted = (|| {
            self.seal.locked.registry.schema_version = CLAIM_FENCE_REGISTRY_VERSION.to_string();
            self.seal.locked.save()?;
            pause_claim_mutation_fence_activation_for_test("after_registry_v2")?;
            persist_claim_mutation_fence_manifest(context, &fence)?;
            pause_claim_mutation_fence_activation_for_test("after_manifest")?;
            persist_claim_mutation_fence_sidecar(
                context,
                &fence.worker_session_id,
                &fence.worker_incarnation,
                &fence.worker_claim,
                &fence,
            )?;
            pause_claim_mutation_fence_activation_for_test("after_worker_sidecar")?;
            persist_claim_mutation_fence_sidecar(
                context,
                &fence.controller_session_id,
                &fence.controller_incarnation,
                &fence.controller_claim,
                &fence,
            )?;
            pause_claim_mutation_fence_activation_for_test("after_controller_sidecar")
        })();
        if let Err(error) = persisted {
            drop(owner_lock);
            let _ = rollback_claim_mutation_fence_files(context, &fence);
            return Err(error);
        }
        Ok(ClaimMutationFenceOwnerGuard {
            _owner_lock: owner_lock,
        })
    }
}

impl StoppedWorkerTerminalizationGuard {
    pub(crate) fn worker_claim_observed(&self) -> bool {
        self.worker_claim_observed
    }

    pub(crate) fn controller_claim(&self) -> &ControllerClaimTuple {
        &self.seal.controller_claim
    }

    /// Seal only the exact stopped worker while the exact active, unexpired
    /// controller claim remains unchanged under this same registry lock.
    pub(crate) fn seal(&mut self, context: &CliContext) -> Result<(), CliError> {
        self.seal.seal(context)
    }
}

impl WorkerClaimRevocationGuard {
    /// Fence only the exact idle worker while the admitted controller claim,
    /// assignment-derived worker claim, and zero-operation proof remain bound
    /// to this one coordination registry lock.
    pub(crate) fn seal(&mut self, context: &CliContext) -> Result<(), CliError> {
        self.seal.seal(context)
    }
}

impl GroupCleanupQuiescenceGuard {
    /// Revoke the exact worker incarnations while the coordination registry is
    /// still locked. Once this commits, a racing worker cannot acquire a new
    /// claim or operation before orchestration terminalizes its assignment.
    pub(crate) fn seal(mut self, context: &CliContext) -> Result<(), CliError> {
        seal_coordination_sessions(&mut self.locked, context, &self.sessions)
    }
}

fn seal_coordination_sessions(
    locked: &mut LockedRegistry,
    context: &CliContext,
    sessions: &[(String, String)],
) -> Result<(), CliError> {
    let now = now_epoch();
    for (session_id, incarnation) in sessions {
        if let Some(broker) = locked.registry.brokers.get_mut(session_id)
            && broker.incarnation == *incarnation
        {
            broker.state = "stopped".to_string();
            broker.heartbeat_at = timestamp(now);
            broker.heartbeat_epoch = now;
            broker.capability_digest.clear();
        }
        for claim in &mut locked.registry.claims {
            if claim.session_id == *session_id
                && claim.session_incarnation == *incarnation
                && claim.state == "active"
            {
                claim.state = "released".to_string();
                claim.revision = claim.revision.saturating_add(1);
                claim.updated_at = timestamp(now);
                claim.terminal_at_epoch = Some(now);
            }
        }
        let _ = fs::remove_file(capability_path(context, session_id, incarnation));
    }
    locked.save()?;
    Ok(())
}

fn ensure_group_cleanup_quiescence(
    registry: &Registry,
    sessions: &[(String, String)],
    allow_active_claims: bool,
) -> Result<(), CliError> {
    for (session_id, incarnation) in sessions {
        if registry
            .brokers
            .get(session_id)
            .is_some_and(|broker| broker.incarnation != *incarnation)
        {
            return Err(CliError::data(
                "session-incarnation-conflict",
                "worker coordination identity changed before group cleanup",
                Some(serde_json::json!({ "session_id": session_id })),
            ));
        }
        let active_claim = registry.claims.iter().any(|claim| {
            claim.session_id == *session_id
                && claim.session_incarnation == *incarnation
                && claim.state == "active"
        });
        let operation_state = registry
            .operations
            .iter()
            .find(|operation| {
                operation.session_id == *session_id
                    && operation.session_incarnation == *incarnation
                    && matches!(
                        operation.state.as_str(),
                        "active" | "completing" | "reconcile_pending"
                    )
            })
            .map(|operation| operation.state.as_str());
        if operation_state.is_some() || (active_claim && !allow_active_claims) {
            return Err(CliError::data(
                "worker-not-quiescent",
                "group cleanup refuses worker authority with active or uncertain mutations",
                Some(serde_json::json!({
                    "session_id": session_id,
                    "active_claim": active_claim,
                    "operation_state": operation_state
                })),
            ));
        }
    }
    Ok(())
}

pub(crate) fn lock_group_cleanup_quiescence(
    context: &CliContext,
    sessions: &[(String, String)],
    allow_active_claims: bool,
) -> Result<GroupCleanupQuiescenceGuard, CliError> {
    let locked = lock_registry(context)?;
    ensure_group_cleanup_quiescence(&locked.registry, sessions, allow_active_claims)?;
    Ok(GroupCleanupQuiescenceGuard {
        locked,
        sessions: sessions.to_vec(),
    })
}

pub(crate) fn lock_worker_runtime_stop(
    context: &CliContext,
    worker_session_id: &str,
    worker_incarnation: &str,
    controller_session_id: &str,
    controller_incarnation: &str,
    authorized_controller_claim: &ControllerClaimTuple,
) -> Result<WorkerRuntimeStopGuard, CliError> {
    let (seal, _) = lock_worker_authority_seal(
        context,
        worker_session_id,
        worker_incarnation,
        controller_session_id,
        controller_incarnation,
        authorized_controller_claim,
        false,
        false,
    )?;
    Ok(WorkerRuntimeStopGuard { seal })
}

pub(crate) fn lock_claimed_worker_runtime_stop(
    context: &CliContext,
    worker_record: &SessionRecord,
    worker_incarnation: &str,
    expected_work_context: &context::WorkContextInput,
    controller_session_id: &str,
    controller_incarnation: &str,
    authorized_controller_claim: &ControllerClaimTuple,
) -> Result<ClaimedWorkerRuntimeStopGuard, CliError> {
    let (seal, worker_claim_observed) = lock_worker_authority_seal(
        context,
        &worker_record.id,
        worker_incarnation,
        controller_session_id,
        controller_incarnation,
        authorized_controller_claim,
        true,
        true,
    )?;
    if !worker_claim_observed {
        return Err(CliError::data(
            "claim-not-active",
            "claimed runtime stop requires the exact worker claim to remain active",
            None,
        ));
    }
    match claims::main_agent_worker_claim_match_in_registry(
        context,
        &seal.locked.registry,
        worker_record,
        worker_incarnation,
        expected_work_context,
    )? {
        Some(true) => {}
        Some(false) => {
            return Err(CliError::data(
                "worker-claim-mismatch",
                "claimed runtime stop requires the exact assignment-derived worker claim",
                None,
            ));
        }
        None => {
            return Err(CliError::data(
                "claim-not-active",
                "claimed runtime stop requires the exact worker claim to remain active",
                None,
            ));
        }
    }
    let worker_claim = seal
        .locked
        .registry
        .claims
        .iter()
        .find(|claim| {
            claim.session_id == worker_record.id
                && claim.session_incarnation == worker_incarnation
                && claim.state == "active"
        })
        .map(|claim| ControllerClaimTuple {
            claim_id: claim.claim_id.clone(),
            revision: claim.revision,
            expires_at_epoch: claim.expires_at_epoch,
        })
        .ok_or_else(|| {
            CliError::data(
                "claim-not-active",
                "claimed runtime stop requires the exact worker claim to remain active",
                None,
            )
        })?;
    if worker_claim.expires_at_epoch <= now_epoch() {
        return Err(CliError::data(
            "claim-not-active",
            "claimed runtime stop requires the exact worker claim to remain active",
            None,
        ));
    }
    Ok(ClaimedWorkerRuntimeStopGuard { seal, worker_claim })
}

pub(crate) fn exact_claim_active_observational(
    context: &CliContext,
    session_id: &str,
    session_incarnation: &str,
    expected: &ControllerClaimTuple,
) -> Result<bool, CliError> {
    let locked = lock_registry_observational(context)?;
    let now = now_epoch();
    Ok(locked.registry.claims.iter().any(|claim| {
        claim.session_id == session_id
            && claim.session_incarnation == session_incarnation
            && claim.claim_id == expected.claim_id
            && claim.revision == expected.revision
            && claim.expires_at_epoch == expected.expires_at_epoch
            && claim.expires_at_epoch > now
            && claim.state == "active"
    }))
}

fn claim_fence_binds_tuple(
    fence: &ClaimMutationFence,
    session_id: &str,
    session_incarnation: &str,
    claim: &ControllerClaimTuple,
) -> bool {
    (fence.worker_session_id == session_id
        && fence.worker_incarnation == session_incarnation
        && fence.worker_claim == *claim)
        || (fence.controller_session_id == session_id
            && fence.controller_incarnation == session_incarnation
            && fence.controller_claim == *claim)
}

fn claim_mutation_fence_same_operation(
    left: &ClaimMutationFence,
    right: &ClaimMutationFence,
) -> bool {
    left.schema_version == right.schema_version
        && left.assignment_id == right.assignment_id
        && left.assignment_revision == right.assignment_revision
        && left.worker_session_id == right.worker_session_id
        && left.worker_incarnation == right.worker_incarnation
        && left.worker_claim == right.worker_claim
        && left.controller_session_id == right.controller_session_id
        && left.controller_incarnation == right.controller_incarnation
        && left.controller_claim == right.controller_claim
        && left.request_digest == right.request_digest
        && left.lock_id == right.lock_id
}

fn claim_mutation_fence_directory(context: &CliContext, name: &str) -> Result<PathBuf, CliError> {
    let root = coordination_root(context)?;
    let directory = root.join(name);
    match fs::symlink_metadata(&directory) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() } =>
        {
            return Err(store_untrusted());
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&directory).map_err(|_| store_unavailable())?;
        }
        Err(_) => return Err(store_unavailable()),
    }
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|_| store_unavailable())?;
    Ok(directory)
}

fn claim_mutation_fence_sidecar_path(
    context: &CliContext,
    session_id: &str,
    session_incarnation: &str,
    claim: &ControllerClaimTuple,
) -> Result<PathBuf, CliError> {
    let tuple_digest = digest_bytes(
        format!(
            "{session_id}\0{session_incarnation}\0{}\0{}\0{}",
            claim.claim_id, claim.revision, claim.expires_at_epoch
        )
        .as_bytes(),
    );
    Ok(
        claim_mutation_fence_directory(context, CLAIM_MUTATION_FENCES_DIR)?
            .join(tuple_digest.trim_start_matches("sha256:")),
    )
}

fn claim_mutation_fence_lock_id(
    assignment_id: &str,
    assignment_revision: u64,
    request_digest: &str,
) -> String {
    digest_bytes(format!("{assignment_id}\0{assignment_revision}\0{request_digest}").as_bytes())
        .trim_start_matches("sha256:")
        .to_string()
}

fn claim_mutation_fence_lock_path(
    context: &CliContext,
    lock_id: &str,
) -> Result<PathBuf, CliError> {
    if lock_id.len() != 64
        || !lock_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(store_corrupt());
    }
    Ok(
        claim_mutation_fence_directory(context, CLAIM_MUTATION_FENCE_LOCKS_DIR)?
            .join(format!("{lock_id}.lock")),
    )
}

fn claim_mutation_fence_manifest_path(
    context: &CliContext,
    lock_id: &str,
) -> Result<PathBuf, CliError> {
    claim_mutation_fence_lock_path_from_existing(lock_id)?;
    Ok(
        claim_mutation_fence_directory(context, CLAIM_MUTATION_FENCE_LOCKS_DIR)?
            .join(format!("{lock_id}.json")),
    )
}

fn open_claim_mutation_fence_lock(context: &CliContext, lock_id: &str) -> Result<File, CliError> {
    let path = claim_mutation_fence_lock_path(context, lock_id)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(SECRET_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                store_untrusted()
            } else {
                store_unavailable()
            }
        })?;
    lock.set_permissions(fs::Permissions::from_mode(SECRET_FILE_MODE))
        .map_err(|_| store_unavailable())?;
    let metadata = lock.metadata().map_err(|_| store_unavailable())?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(store_untrusted());
    }
    Ok(lock)
}

fn open_existing_claim_mutation_fence_lock(
    context: &CliContext,
    lock_id: &str,
) -> Result<File, CliError> {
    let path = claim_mutation_fence_lock_path(context, lock_id)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                store_untrusted()
            } else {
                store_unavailable()
            }
        })?;
    let metadata = lock.metadata().map_err(|_| store_unavailable())?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(store_untrusted());
    }
    Ok(lock)
}

fn acquire_claim_mutation_fence_owner(
    context: &CliContext,
    lock_id: &str,
) -> Result<File, CliError> {
    let directory = claim_mutation_fence_directory(context, CLAIM_MUTATION_FENCE_LOCKS_DIR)?;
    let lock_path = claim_mutation_fence_lock_path(context, lock_id)?;
    let manifest_path = claim_mutation_fence_manifest_path(context, lock_id)?;
    if !lock_path.exists() && !manifest_path.exists() {
        let count = fs::read_dir(&directory)
            .map_err(|_| store_unavailable())?
            .take(MAX_CLAIM_MUTATION_OPERATION_FILES.saturating_add(1))
            .count();
        if count.saturating_add(2) > MAX_CLAIM_MUTATION_OPERATION_FILES {
            return Err(CliError::data(
                "quota-exceeded",
                "claim-mutation fence operation file limit exceeded",
                None,
            ));
        }
    }
    let lock = open_claim_mutation_fence_lock(context, lock_id)?;
    // SAFETY: flock is called with a valid, owned file descriptor.
    let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(lock);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Err(CliError::unavailable(
            "claim-mutation-fenced",
            "the claimed runtime stop mutation fence is already owned",
            None,
        ));
    }
    Err(store_unavailable())
}

fn pause_claim_mutation_fence_activation_for_test(stage: &str) -> Result<(), CliError> {
    #[cfg(debug_assertions)]
    if std::env::var("NILS_AGENT_SESSION_TEST_CLAIM_MUTATION_FENCE_BARRIER_STAGE")
        .ok()
        .as_deref()
        == Some(stage)
        && let Some(directory) =
            std::env::var_os("NILS_AGENT_SESSION_TEST_CLAIM_MUTATION_FENCE_BARRIER_DIR")
                .map(PathBuf::from)
    {
        fs::create_dir_all(&directory).map_err(|_| store_unavailable())?;
        fs::write(directory.join("ready"), stage.as_bytes()).map_err(|_| store_unavailable())?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !directory.join("release").is_file() {
            if Instant::now() >= deadline {
                return Err(CliError::runtime(
                    "test-barrier-timeout",
                    "claim-mutation fence activation test barrier timed out",
                    None,
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

fn read_claim_mutation_fence(path: &Path) -> Result<ClaimMutationFence, CliError> {
    let bytes = read_private_file(path, MAX_CLAIM_MUTATION_FENCE_BYTES).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            CliError::data(
                "claim-mutation-fence-conflict",
                "the exact claim-mutation fence sidecar is missing",
                None,
            )
        } else {
            store_corrupt()
        }
    })?;
    let fence: ClaimMutationFence = serde_json::from_slice(&bytes).map_err(|_| store_corrupt())?;
    if fence.schema_version != CLAIM_MUTATION_FENCE_SCHEMA {
        return Err(store_corrupt());
    }
    claim_mutation_fence_lock_path_from_existing(&fence.lock_id)?;
    Ok(fence)
}

fn claim_mutation_fence_lock_path_from_existing(lock_id: &str) -> Result<(), CliError> {
    if lock_id.len() == 64
        && lock_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(store_corrupt())
    }
}

fn persist_claim_mutation_fence_sidecar(
    context: &CliContext,
    session_id: &str,
    session_incarnation: &str,
    claim: &ControllerClaimTuple,
    fence: &ClaimMutationFence,
) -> Result<(), CliError> {
    if !claim_fence_binds_tuple(fence, session_id, session_incarnation, claim) {
        return Err(store_corrupt());
    }
    let path = claim_mutation_fence_sidecar_path(context, session_id, session_incarnation, claim)?;
    match read_claim_mutation_fence(&path) {
        Ok(existing) if !claim_mutation_fence_same_operation(&existing, fence) => {
            return Err(CliError::unavailable(
                "claim-mutation-fenced",
                "the exact claim tuple is fenced by another claimed runtime stop",
                None,
            ));
        }
        Ok(_) => {}
        Err(error) if error.code() == "claim-mutation-fence-conflict" => {
            let directory = claim_mutation_fence_directory(context, CLAIM_MUTATION_FENCES_DIR)?;
            let count = fs::read_dir(&directory)
                .map_err(|_| store_unavailable())?
                .take(MAX_CLAIM_MUTATION_FENCE_FILES.saturating_add(1))
                .count();
            if count >= MAX_CLAIM_MUTATION_FENCE_FILES {
                return Err(CliError::data(
                    "quota-exceeded",
                    "claim-mutation fence sidecar limit exceeded",
                    None,
                ));
            }
        }
        Err(error) => return Err(error),
    }
    let bytes = serde_json::to_vec_pretty(fence).map_err(|_| store_corrupt())?;
    write_atomic(&path, &bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())
}

fn persist_claim_mutation_fence_manifest(
    context: &CliContext,
    fence: &ClaimMutationFence,
) -> Result<(), CliError> {
    let path = claim_mutation_fence_manifest_path(context, &fence.lock_id)?;
    match read_claim_mutation_fence(&path) {
        Ok(existing) if !claim_mutation_fence_same_operation(&existing, fence) => {
            return Err(CliError::unavailable(
                "claim-mutation-fenced",
                "the claimed runtime stop operation manifest belongs to another operation",
                None,
            ));
        }
        Ok(_) => {}
        Err(error) if error.code() == "claim-mutation-fence-conflict" => {}
        Err(error) => return Err(error),
    }
    let bytes = serde_json::to_vec_pretty(fence).map_err(|_| store_corrupt())?;
    write_atomic(&path, &bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())
}

fn rollback_claim_mutation_fence_files(
    context: &CliContext,
    fence: &ClaimMutationFence,
) -> Result<(), CliError> {
    let paths = [
        claim_mutation_fence_sidecar_path(
            context,
            &fence.worker_session_id,
            &fence.worker_incarnation,
            &fence.worker_claim,
        )?,
        claim_mutation_fence_sidecar_path(
            context,
            &fence.controller_session_id,
            &fence.controller_incarnation,
            &fence.controller_claim,
        )?,
        claim_mutation_fence_manifest_path(context, &fence.lock_id)?,
    ];
    for path in paths {
        match read_claim_mutation_fence(&path) {
            Ok(existing) if claim_mutation_fence_same_operation(&existing, fence) => {
                fs::remove_file(path).map_err(|_| store_unavailable())?;
            }
            Ok(_) => return Err(store_corrupt()),
            Err(error) if error.code() == "claim-mutation-fence-conflict" => {}
            Err(error) => return Err(error),
        }
    }
    let lock_path = claim_mutation_fence_lock_path(context, &fence.lock_id)?;
    match fs::remove_file(lock_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(store_unavailable()),
    }
}

fn claim_mutation_fence_owner_active(
    context: &CliContext,
    fence: &ClaimMutationFence,
) -> Result<bool, CliError> {
    let lock = open_claim_mutation_fence_lock(context, &fence.lock_id)?;
    // SAFETY: flock is called with a valid, owned file descriptor.
    let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
    if result == 0 {
        // SAFETY: the descriptor remains owned by `lock` for this call.
        unsafe {
            libc::flock(lock.as_raw_fd(), libc::LOCK_UN);
        }
        return Ok(false);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Ok(true);
    }
    Err(store_unavailable())
}

pub(crate) fn sweep_inactive_claim_mutation_fence_orphans(
    context: &CliContext,
) -> Result<(), CliError> {
    let directory = claim_mutation_fence_directory(context, CLAIM_MUTATION_FENCE_LOCKS_DIR)?;
    let entries = fs::read_dir(&directory)
        .map_err(|_| store_unavailable())?
        .take(MAX_CLAIM_MUTATION_OPERATION_FILES.saturating_add(1))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| store_unavailable())?;
    if entries.len() > MAX_CLAIM_MUTATION_OPERATION_FILES {
        return Err(CliError::data(
            "quota-exceeded",
            "claim-mutation fence operation file limit exceeded",
            None,
        ));
    }
    for entry in entries {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(lock_id) = name.strip_suffix(".lock") else {
            continue;
        };
        if claim_mutation_fence_lock_path_from_existing(lock_id).is_err() {
            continue;
        }
        let lock = match open_existing_claim_mutation_fence_lock(context, lock_id) {
            Ok(lock) => lock,
            Err(_) if !entry.path().exists() => continue,
            Err(error) => return Err(error),
        };
        // SAFETY: flock is called with a valid, owned file descriptor.
        let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
                continue;
            }
            return Err(store_unavailable());
        }
        let manifest_path = claim_mutation_fence_manifest_path(context, lock_id)?;
        let fence = match read_claim_mutation_fence(&manifest_path) {
            Ok(fence) if fence.lock_id == lock_id => Some(fence),
            Ok(_) => return Err(store_corrupt()),
            Err(error) if error.code() == "claim-mutation-fence-conflict" => None,
            Err(error) => return Err(error),
        };
        let has_tuple_sidecar = if let Some(fence) = fence.as_ref() {
            let paths = [
                claim_mutation_fence_sidecar_path(
                    context,
                    &fence.worker_session_id,
                    &fence.worker_incarnation,
                    &fence.worker_claim,
                )?,
                claim_mutation_fence_sidecar_path(
                    context,
                    &fence.controller_session_id,
                    &fence.controller_incarnation,
                    &fence.controller_claim,
                )?,
            ];
            let mut present = false;
            for path in paths {
                match read_claim_mutation_fence(&path) {
                    Ok(sidecar) if claim_mutation_fence_same_operation(&sidecar, fence) => {
                        present = true;
                    }
                    Ok(_) => return Err(store_corrupt()),
                    Err(error) if error.code() == "claim-mutation-fence-conflict" => {}
                    Err(error) => return Err(error),
                }
            }
            present
        } else {
            false
        };
        if !has_tuple_sidecar {
            if fence.is_some() {
                fs::remove_file(manifest_path).map_err(|_| store_unavailable())?;
            }
            fs::remove_file(entry.path()).map_err(|_| store_unavailable())?;
        }
    }
    Ok(())
}

fn remove_inactive_claim_mutation_fence_sidecar(
    context: &CliContext,
    path: &Path,
    fence: &ClaimMutationFence,
) -> Result<(), CliError> {
    if claim_mutation_fence_owner_active(context, fence)? {
        return Err(CliError::unavailable(
            "claim-mutation-fenced",
            "the exact claim tuple is fenced by an in-flight claimed runtime stop",
            None,
        ));
    }
    fs::remove_file(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            store_corrupt()
        } else {
            store_unavailable()
        }
    })?;
    let worker_path = claim_mutation_fence_sidecar_path(
        context,
        &fence.worker_session_id,
        &fence.worker_incarnation,
        &fence.worker_claim,
    )?;
    let controller_path = claim_mutation_fence_sidecar_path(
        context,
        &fence.controller_session_id,
        &fence.controller_incarnation,
        &fence.controller_claim,
    )?;
    let other_path = if path == worker_path {
        controller_path
    } else if path == controller_path {
        worker_path
    } else {
        return Err(store_corrupt());
    };
    match read_claim_mutation_fence(&other_path) {
        Ok(other) if claim_mutation_fence_same_operation(&other, fence) => return Ok(()),
        Ok(_) => return Err(store_corrupt()),
        Err(error) if error.code() == "claim-mutation-fence-conflict" => {}
        Err(error) => return Err(error),
    }
    let manifest_path = claim_mutation_fence_manifest_path(context, &fence.lock_id)?;
    match fs::remove_file(manifest_path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(store_unavailable()),
    }
    let lock_path = claim_mutation_fence_lock_path(context, &fence.lock_id)?;
    match fs::remove_file(lock_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(store_unavailable()),
    }
}

pub(crate) fn ensure_claim_mutation_not_fenced(
    context: &CliContext,
    session_id: &str,
    session_incarnation: &str,
    claim: &context::WorkContextRecord,
) -> Result<(), CliError> {
    let claim = ControllerClaimTuple {
        claim_id: claim.claim_id.clone(),
        revision: claim.revision,
        expires_at_epoch: claim.expires_at_epoch,
    };
    let path = claim_mutation_fence_sidecar_path(context, session_id, session_incarnation, &claim)?;
    let fence = match read_claim_mutation_fence(&path) {
        Ok(fence) => fence,
        Err(error) if error.code() == "claim-mutation-fence-conflict" => return Ok(()),
        Err(error) => return Err(error),
    };
    if !claim_fence_binds_tuple(&fence, session_id, session_incarnation, &claim) {
        return Err(store_corrupt());
    }
    if claim_mutation_fence_owner_active(context, &fence)? {
        return Err(CliError::unavailable(
            "claim-mutation-fenced",
            "the exact claim tuple is fenced by an in-flight claimed runtime stop",
            None,
        ));
    }
    remove_inactive_claim_mutation_fence_sidecar(context, &path, &fence)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn verify_claimed_worker_runtime_stop_claim_fence(
    context: &CliContext,
    assignment_id: &str,
    assignment_revision: u64,
    worker_session_id: &str,
    worker_incarnation: &str,
    worker_claim: &ControllerClaimTuple,
    controller_session_id: &str,
    controller_incarnation: &str,
    controller_claim: &ControllerClaimTuple,
    request_digest: &str,
    require_mutation_fence: bool,
) -> Result<bool, CliError> {
    let locked = lock_registry_observational(context)?;
    let now = now_epoch();
    let claims_active = controller_claim_matches_admission(
        &locked.registry,
        &(
            controller_session_id.to_string(),
            controller_incarnation.to_string(),
        ),
        controller_claim,
        now,
        true,
    ) && locked.registry.claims.iter().any(|claim| {
        claim.session_id == worker_session_id
            && claim.session_incarnation == worker_incarnation
            && claim.claim_id == worker_claim.claim_id
            && claim.revision == worker_claim.revision
            && claim.expires_at_epoch == worker_claim.expires_at_epoch
            && claim.expires_at_epoch > now
            && claim.state == "active"
    });
    if require_mutation_fence {
        if locked.registry.schema_version != CLAIM_FENCE_REGISTRY_VERSION {
            return Err(CliError::data(
                "coordination-facade-version-skew",
                "claimed runtime stop requires the fence-aware coordination registry schema",
                None,
            ));
        }
        let lock_id =
            claim_mutation_fence_lock_id(assignment_id, assignment_revision, request_digest);
        let manifest_path = claim_mutation_fence_manifest_path(context, &lock_id)?;
        let worker_path = claim_mutation_fence_sidecar_path(
            context,
            worker_session_id,
            worker_incarnation,
            worker_claim,
        )?;
        let controller_path = claim_mutation_fence_sidecar_path(
            context,
            controller_session_id,
            controller_incarnation,
            controller_claim,
        )?;
        let manifest = read_claim_mutation_fence(&manifest_path)?;
        let worker_fence = read_claim_mutation_fence(&worker_path)?;
        let controller_fence = read_claim_mutation_fence(&controller_path)?;
        let expected = ClaimMutationFence {
            schema_version: CLAIM_MUTATION_FENCE_SCHEMA.to_string(),
            assignment_id: assignment_id.to_string(),
            assignment_revision,
            worker_session_id: worker_session_id.to_string(),
            worker_incarnation: worker_incarnation.to_string(),
            worker_claim: worker_claim.clone(),
            controller_session_id: controller_session_id.to_string(),
            controller_incarnation: controller_incarnation.to_string(),
            controller_claim: controller_claim.clone(),
            request_digest: request_digest.to_string(),
            lock_id,
        };
        if !claim_mutation_fence_same_operation(&manifest, &expected)
            || !claim_mutation_fence_same_operation(&worker_fence, &expected)
            || !claim_mutation_fence_same_operation(&controller_fence, &expected)
        {
            return Err(CliError::data(
                "claim-mutation-fence-conflict",
                "claimed runtime stop has no matching exact claim-mutation fence",
                None,
            ));
        }
    }
    Ok(claims_active)
}

pub(crate) fn clear_claimed_worker_runtime_stop_claim_fence(
    context: &CliContext,
    assignment_id: &str,
    assignment_revision: u64,
    request_digest: &str,
) -> Result<(), CliError> {
    let _locked = lock_registry_observational(context)?;
    let lock_id = claim_mutation_fence_lock_id(assignment_id, assignment_revision, request_digest);
    let manifest_path = claim_mutation_fence_manifest_path(context, &lock_id)?;
    let fence = match read_claim_mutation_fence(&manifest_path) {
        Ok(fence) => fence,
        Err(error) if error.code() == "claim-mutation-fence-conflict" => return Ok(()),
        Err(error) => return Err(error),
    };
    if fence.assignment_id != assignment_id
        || fence.assignment_revision != assignment_revision
        || fence.request_digest != request_digest
        || fence.lock_id != lock_id
    {
        return Err(store_corrupt());
    }
    if claim_mutation_fence_owner_active(context, &fence)? {
        return Err(CliError::unavailable(
            "claim-mutation-fenced",
            "the claimed runtime stop mutation fence is still owned",
            None,
        ));
    }
    let paths = [
        claim_mutation_fence_sidecar_path(
            context,
            &fence.worker_session_id,
            &fence.worker_incarnation,
            &fence.worker_claim,
        )?,
        claim_mutation_fence_sidecar_path(
            context,
            &fence.controller_session_id,
            &fence.controller_incarnation,
            &fence.controller_claim,
        )?,
    ];
    for path in paths {
        match read_claim_mutation_fence(&path) {
            Ok(sidecar) if claim_mutation_fence_same_operation(&sidecar, &fence) => {
                fs::remove_file(path).map_err(|_| store_unavailable())?;
            }
            Ok(_) => return Err(store_corrupt()),
            Err(error) if error.code() == "claim-mutation-fence-conflict" => {}
            Err(error) => return Err(error),
        }
    }
    fs::remove_file(manifest_path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            store_corrupt()
        } else {
            store_unavailable()
        }
    })?;
    let lock_path = claim_mutation_fence_lock_path(context, &fence.lock_id)?;
    match fs::remove_file(lock_path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(store_unavailable()),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn lock_worker_authority_seal(
    context: &CliContext,
    worker_session_id: &str,
    worker_incarnation: &str,
    controller_session_id: &str,
    controller_incarnation: &str,
    authorized_controller_claim: &ControllerClaimTuple,
    allow_active_worker_claims: bool,
    require_unexpired_at_seal: bool,
) -> Result<(WorkerAuthoritySealGuard, bool), CliError> {
    let locked = lock_registry_observational(context)?;
    ensure_group_cleanup_quiescence(
        &locked.registry,
        &[(
            worker_session_id.to_string(),
            worker_incarnation.to_string(),
        )],
        allow_active_worker_claims,
    )?;
    let now = now_epoch();
    let controller_claim = locked
        .registry
        .claims
        .iter()
        .find(|claim| {
            claim.session_id == controller_session_id
                && claim.session_incarnation == controller_incarnation
                && claim.claim_id == authorized_controller_claim.claim_id
                && claim.revision == authorized_controller_claim.revision
                && claim.expires_at_epoch == authorized_controller_claim.expires_at_epoch
                && claim.state == "active"
                && claim.expires_at_epoch > now
        })
        .ok_or_else(|| {
            CliError::data(
                "claim-not-active",
                "Main Agent claim must be exact, active, and unexpired for worker authority admission",
                None,
            )
        })?;
    let worker_claim_observed = locked.registry.claims.iter().any(|claim| {
        claim.session_id == worker_session_id
            && claim.session_incarnation == worker_incarnation
            && claim.state == "active"
    });
    let controller_claim = ControllerClaimTuple {
        claim_id: controller_claim.claim_id.clone(),
        revision: controller_claim.revision,
        expires_at_epoch: controller_claim.expires_at_epoch,
    };
    Ok((
        WorkerAuthoritySealGuard {
            locked,
            worker: (
                worker_session_id.to_string(),
                worker_incarnation.to_string(),
            ),
            controller: (
                controller_session_id.to_string(),
                controller_incarnation.to_string(),
            ),
            controller_claim,
            allow_active_worker_claims,
            require_unexpired_at_seal,
        },
        worker_claim_observed,
    ))
}

pub(crate) fn lock_stopped_worker_terminalization(
    context: &CliContext,
    worker_session_id: &str,
    worker_incarnation: &str,
    controller_session_id: &str,
    controller_incarnation: &str,
    authorized_controller_claim: &ControllerClaimTuple,
) -> Result<StoppedWorkerTerminalizationGuard, CliError> {
    // Destructive authority sealing must observe the controller claim exactly
    // as persisted. The normal registry lock opportunistically renews claims
    // for healthy brokers, which would turn a claim that expired between the
    // two durable stages back into valid sealing authority.
    let (seal, worker_claim_observed) = lock_worker_authority_seal(
        context,
        worker_session_id,
        worker_incarnation,
        controller_session_id,
        controller_incarnation,
        authorized_controller_claim,
        true,
        true,
    )?;
    Ok(StoppedWorkerTerminalizationGuard {
        seal,
        worker_claim_observed,
    })
}

pub(crate) fn lock_worker_claim_revocation(
    context: &CliContext,
    worker_record: &SessionRecord,
    worker_incarnation: &str,
    expected_work_context: &context::WorkContextInput,
    controller_session_id: &str,
    controller_incarnation: &str,
    authorized_controller_claim: &ControllerClaimTuple,
) -> Result<WorkerClaimRevocationGuard, CliError> {
    lock_worker_claim_revocation_inner(
        context,
        worker_record,
        worker_incarnation,
        expected_work_context,
        controller_session_id,
        controller_incarnation,
        authorized_controller_claim,
        true,
    )
}

pub(crate) fn lock_worker_claim_revocation_replay(
    context: &CliContext,
    worker_record: &SessionRecord,
    worker_incarnation: &str,
    expected_work_context: &context::WorkContextInput,
    controller_session_id: &str,
    controller_incarnation: &str,
    authorized_controller_claim: &ControllerClaimTuple,
) -> Result<WorkerClaimRevocationGuard, CliError> {
    lock_worker_claim_revocation_inner(
        context,
        worker_record,
        worker_incarnation,
        expected_work_context,
        controller_session_id,
        controller_incarnation,
        authorized_controller_claim,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn lock_worker_claim_revocation_inner(
    context: &CliContext,
    worker_record: &SessionRecord,
    worker_incarnation: &str,
    expected_work_context: &context::WorkContextInput,
    controller_session_id: &str,
    controller_incarnation: &str,
    authorized_controller_claim: &ControllerClaimTuple,
    require_active_worker_claim: bool,
) -> Result<WorkerClaimRevocationGuard, CliError> {
    let (seal, worker_claim_observed) = lock_worker_authority_seal(
        context,
        &worker_record.id,
        worker_incarnation,
        controller_session_id,
        controller_incarnation,
        authorized_controller_claim,
        true,
        true,
    )?;
    if require_active_worker_claim && !worker_claim_observed {
        return Err(CliError::data(
            "claim-not-active",
            "the exact worker has no active claim to revoke",
            None,
        ));
    }
    match claims::main_agent_worker_claim_match_in_registry(
        context,
        &seal.locked.registry,
        worker_record,
        worker_incarnation,
        expected_work_context,
    )? {
        Some(true) => Ok(WorkerClaimRevocationGuard { seal }),
        Some(false) => Err(CliError::data(
            "worker-claim-mismatch",
            "Main Agent claim revocation requires the exact assignment-derived worker claim",
            None,
        )),
        None if require_active_worker_claim => Err(CliError::data(
            "claim-not-active",
            "the exact worker has no active claim to revoke",
            None,
        )),
        None => Ok(WorkerClaimRevocationGuard { seal }),
    }
}

pub(crate) fn lock_session_quiescence(
    context: &CliContext,
    session_id: &str,
    incarnation: &str,
) -> Result<SessionQuiescenceGuard, CliError> {
    lock_session_quiescence_with_maintenance(
        context,
        session_id,
        incarnation,
        RegistryMaintenance::Full,
    )
}

pub(crate) fn lock_session_quiescence_observational(
    context: &CliContext,
    session_id: &str,
    incarnation: &str,
) -> Result<SessionQuiescenceGuard, CliError> {
    lock_session_quiescence_with_maintenance(
        context,
        session_id,
        incarnation,
        RegistryMaintenance::Observational,
    )
}

fn lock_session_quiescence_with_maintenance(
    context: &CliContext,
    session_id: &str,
    incarnation: &str,
    maintenance: RegistryMaintenance,
) -> Result<SessionQuiescenceGuard, CliError> {
    let locked = lock_registry_with_maintenance(context, maintenance)?;
    let now = now_epoch();
    let broker = locked.registry.brokers.get(session_id);
    let broker_present = broker.is_some();
    let broker_identity_matched = broker.is_some_and(|broker| broker.incarnation == incarnation);
    let broker_authoritative = broker.is_some_and(|broker| {
        broker.incarnation == incarnation
            && broker.state == "ready"
            && broker::capability_available(
                context,
                session_id,
                incarnation,
                &broker.capability_digest,
            )
            && broker::heartbeat_fresh(context, session_id, incarnation, broker.heartbeat_epoch)
    });
    let broker_generation = broker.map(|broker| broker.generation);
    let broker_runtime_identity_digest =
        broker.map(|broker| broker.runtime_identity_digest.clone());
    let broker_lost_since_epoch = broker.and_then(|broker| broker.lost_since_epoch);
    let active_claim_record = locked.registry.claims.iter().find(|claim| {
        claim.session_id == session_id
            && claim.session_incarnation == incarnation
            && claim.state == "active"
    });
    let active_claim = active_claim_record.is_some_and(|claim| claim.expires_at_epoch > now);
    let claim_id = active_claim_record.map(|claim| claim.claim_id.clone());
    let claim_revision = active_claim_record.map(|claim| claim.revision);
    let claim_expires_at = active_claim_record.map(|claim| claim.expires_at.clone());
    let claim_expires_at_epoch = active_claim_record.map(|claim| claim.expires_at_epoch);
    let active_operation = locked.registry.operations.iter().any(|operation| {
        operation.session_id == session_id
            && operation.session_incarnation == incarnation
            && operation.state == "active"
    });
    let uncertain_operation = locked.registry.operations.iter().any(|operation| {
        operation.session_id == session_id
            && operation.session_incarnation == incarnation
            && !matches!(
                operation.state.as_str(),
                "active" | "completed" | "failed" | "abandoned"
            )
    });
    Ok(SessionQuiescenceGuard {
        _locked: locked,
        session_id: session_id.to_string(),
        incarnation: incarnation.to_string(),
        broker_present,
        broker_identity_matched,
        broker_authoritative,
        broker_generation,
        broker_runtime_identity_digest,
        broker_lost_since_epoch,
        active_claim,
        claim_id,
        claim_revision,
        claim_expires_at,
        claim_expires_at_epoch,
        active_operation,
        uncertain_operation,
    })
}

fn render_coordination(
    command: &'static str,
    format: nils_common::cli_contract::OutputFormat,
    result: Result<Value, CliError>,
) -> i32 {
    match result {
        Ok(value) => render_single_success(command, format, &value, render_value_text),
        Err(error) => render_error(command, format, error),
    }
}

fn render_value_text(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string()) + "\n"
}

pub(crate) fn provision(context: &CliContext, record: &SessionRecord) -> Result<PathBuf, CliError> {
    broker::provision(context, record)
}

pub(crate) fn prepare(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    broker::prepare(context, record)
}

pub(crate) fn prepare_in_dir(session_dir: &Path, _record: &SessionRecord) -> Result<(), CliError> {
    broker::prepare_in_dir(session_dir)
}

pub(crate) fn activate_ready(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    broker::activate_ready(context, record)
}

pub(crate) fn ensure_ready(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    broker::ensure_ready(context, record)
}

pub(crate) fn ensure_recovery_registry_schema(context: &CliContext) -> Result<(), CliError> {
    let path = coordination_root(context)?.join(REGISTRY_FILE);
    let bytes = read_private_file(&path, MAX_REGISTRY_BYTES).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            store_unavailable()
        } else {
            store_corrupt()
        }
    })?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| store_corrupt())?;
    let found = value
        .get("schema_version")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(found, REGISTRY_VERSION | CLAIM_FENCE_REGISTRY_VERSION) {
        return Err(CliError::data(
            "coordination-facade-version-skew",
            "the durable coordination registry requires a matching agent-session/main-agent facade",
            Some(serde_json::json!({
                "found_schema_version": found,
                "supported_schema_version": CLAIM_FENCE_REGISTRY_VERSION,
                "supported_schema_versions": [REGISTRY_VERSION, CLAIM_FENCE_REGISTRY_VERSION],
                "required_action": "use the agent-session and main-agent binaries matching this durable registry; do not reinterpret the failure as an authorization problem"
            })),
        ));
    }
    Ok(())
}

pub(crate) fn validate_recovery_capability(
    context: &CliContext,
    record: &SessionRecord,
    capability_file: &Path,
) -> Result<(), CliError> {
    let expected_incarnation = incarnation(record)?;
    let token = read_private_file(capability_file, 512).map_err(|_| {
        CliError::runtime(
            "controller-recovery-capability-unavailable",
            "the exact controller recovery capability is unavailable",
            None,
        )
    })?;
    let token = String::from_utf8(token).map_err(|_| {
        CliError::runtime(
            "controller-recovery-capability-unavailable",
            "the exact controller recovery capability is unavailable",
            None,
        )
    })?;
    let locked = lock_registry(context)?;
    locked
        .registry
        .brokers
        .get(&record.id)
        .filter(|broker| {
            broker.incarnation == expected_incarnation
                && matches!(broker.state.as_str(), "ready" | "recovering")
                && digest_eq(
                    &broker.capability_digest,
                    &digest_bytes(token.trim().as_bytes()),
                )
                && broker::capability_available(
                    context,
                    &record.id,
                    &expected_incarnation,
                    &broker.capability_digest,
                )
        })
        .ok_or_else(|| {
            CliError::runtime(
                "controller-recovery-capability-unavailable",
                "the exact controller recovery capability is unavailable",
                None,
            )
        })?;
    Ok(())
}

pub(crate) fn recover_broker(
    context: &CliContext,
    args: cli::BrokerRecoveryArgs,
) -> Result<Value, CliError> {
    broker::recover(context, args, false)
}

pub(crate) fn revoke(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    broker::revoke(context, record)
}

pub(crate) fn forget_revoked_failed_launch(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<(), CliError> {
    broker::forget_revoked_failed_launch(context, record)
}

pub(crate) fn pending_notifications(
    context: &CliContext,
) -> Result<Vec<NotificationCandidate>, CliError> {
    notification::pending(context)
}

pub(crate) fn unresolved_notifications(
    context: &CliContext,
) -> Result<Vec<NotificationCandidate>, CliError> {
    notification::unresolved(context)
}

#[cfg(test)]
pub(crate) fn begin_notification_attempt(
    context: &CliContext,
    candidate: &NotificationCandidate,
) -> Result<bool, CliError> {
    notification::begin_attempt(context, candidate)
}

pub(crate) fn ensure_notification_submission_not_in_progress(
    registry: &Registry,
    session_id: &str,
    incarnation: &str,
) -> Result<(), CliError> {
    if notification::submission_fences_session(registry, session_id, incarnation) {
        return Err(CliError::unavailable(
            "coordination-notification-submission-in-progress",
            "claim or operation admission is fenced during exact terminal notification submission",
            Some(serde_json::json!({
                "retryable": true,
                "next_action": "wait-for-notification-outcome",
                "recovery": {
                    "kind": "notification-submission-wait",
                    "owner": "agent-session-serve",
                    "automatic": true
                }
            })),
        ));
    }
    Ok(())
}

pub(crate) fn mark_notification_submitted(
    context: &CliContext,
    candidate: &NotificationCandidate,
) -> Result<bool, CliError> {
    notification::mark_submitted(context, candidate)
}

#[allow(dead_code)]
pub(crate) fn mark_notification_known_failure(
    context: &CliContext,
    candidate: &NotificationCandidate,
    reason: &str,
    retry_after_seconds: i64,
) -> Result<bool, CliError> {
    notification::mark_known_failure(context, candidate, reason, retry_after_seconds)
}

pub(crate) fn mark_notification_unknown(
    context: &CliContext,
    candidate: &NotificationCandidate,
    reason: &str,
) -> Result<bool, CliError> {
    notification::mark_unknown(context, candidate, reason)
}

pub(crate) fn mark_notification_undeliverable(
    context: &CliContext,
    candidate: &NotificationCandidate,
    reason: &str,
) -> Result<bool, CliError> {
    notification::mark_undeliverable(context, candidate, reason)
}

pub(crate) fn defer_notification(
    context: &CliContext,
    candidate: &NotificationCandidate,
    reason: &str,
    retry_after_seconds: i64,
) -> Result<bool, CliError> {
    notification::defer(context, candidate, reason, retry_after_seconds)
}

pub(crate) fn reconcile_notification_submitted(
    context: &CliContext,
    candidate: &NotificationCandidate,
) -> Result<bool, CliError> {
    notification::reconcile_submitted(context, candidate)
}

pub(crate) fn reconcile_notification_absent(
    context: &CliContext,
    candidate: &NotificationCandidate,
) -> Result<bool, CliError> {
    notification::reconcile_absent(context, candidate)
}

pub(crate) fn notification_prompt(message_id: &str, session_id: &str) -> String {
    notification::fixed_prompt(message_id, session_id)
}

pub(crate) fn retry_notification(
    context: &CliContext,
    target_session_id: &str,
    target_incarnation: &str,
    expected_generation: u64,
) -> Result<notification::NotificationProjection, CliError> {
    notification::retry_existing(
        context,
        target_session_id,
        target_incarnation,
        expected_generation,
    )
}

pub(crate) fn coordination_dir(context: &CliContext, session_id: &str) -> PathBuf {
    session_dir(context, session_id).join("coordination")
}

pub(crate) fn capability_path(
    context: &CliContext,
    session_id: &str,
    incarnation: &str,
) -> PathBuf {
    capability_path_for_state(&context.state_dir, session_id, incarnation)
}

pub(crate) fn capability_path_for_state(
    state_dir: &Path,
    session_id: &str,
    incarnation: &str,
) -> PathBuf {
    state_dir
        .join("sessions")
        .join(session_id)
        .join("coordination")
        .join(format!(
            "capability-{}",
            digest_bytes(incarnation.as_bytes())
        ))
}

pub(crate) fn checkpoint_path_for_state(
    state_dir: &Path,
    session_id: &str,
    incarnation: &str,
) -> PathBuf {
    state_dir
        .join("sessions")
        .join(session_id)
        .join("coordination")
        .join(format!(
            "main-agent-checkpoint-{}.json",
            digest_bytes(incarnation.as_bytes())
        ))
}

pub(crate) fn heartbeat_path(state_dir: &Path, session_id: &str) -> PathBuf {
    state_dir
        .join("sessions")
        .join(session_id)
        .join("coordination")
        .join("heartbeat")
}

fn authenticate_from_file(
    context: &CliContext,
    session_id: &str,
    capability_file: Option<&Path>,
) -> Result<(SessionRecord, String), CliError> {
    let token = capability_token_from_file(capability_file)?;
    authenticate_token(context, session_id, &token)
}

pub(crate) fn authenticate_recovery_from_file(
    context: &CliContext,
    session_id: &str,
    capability_file: Option<&Path>,
) -> Result<(SessionRecord, String), CliError> {
    let token = capability_token_from_file(capability_file)?;
    authenticate_recovery_token(context, session_id, &token)
}

fn capability_token_from_file(capability_file: Option<&Path>) -> Result<String, CliError> {
    let path = capability_file
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(CAPABILITY_ENV).map(PathBuf::from))
        .ok_or_else(unauthorized)?;
    let token = read_private_file(&path, 512).map_err(|_| unauthorized())?;
    let token = String::from_utf8(token).map_err(|_| unauthorized())?;
    Ok(token.trim().to_string())
}

pub(crate) fn authenticate_any_from_file(
    context: &CliContext,
    capability_file: Option<&Path>,
) -> Result<(SessionRecord, String), CliError> {
    let (record, incarnation, _) = authenticate_any_from_file_with_maintenance(
        context,
        capability_file,
        RegistryMaintenance::Full,
        false,
    )?;
    Ok((record, incarnation))
}

pub(crate) fn authenticate_any_from_file_with_active_claim_observational(
    context: &CliContext,
    capability_file: Option<&Path>,
) -> Result<(SessionRecord, String, ControllerClaimTuple), CliError> {
    let (record, incarnation, claim) = authenticate_any_from_file_with_maintenance(
        context,
        capability_file,
        RegistryMaintenance::Observational,
        true,
    )?;
    Ok((
        record,
        incarnation,
        claim.expect("active claim was required by authentication"),
    ))
}

fn authenticate_any_from_file_with_maintenance(
    context: &CliContext,
    capability_file: Option<&Path>,
    maintenance: RegistryMaintenance,
    require_active_claim: bool,
) -> Result<(SessionRecord, String, Option<ControllerClaimTuple>), CliError> {
    let path = capability_file
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(CAPABILITY_ENV).map(PathBuf::from))
        .ok_or_else(unauthorized)?;
    let token = read_private_file(&path, 512).map_err(|_| unauthorized())?;
    let token = String::from_utf8(token).map_err(|_| unauthorized())?;
    let token = token.trim();
    if token.len() < 32 || token.len() > 256 || !token.is_ascii() {
        return Err(unauthorized());
    }
    let digest = digest_bytes(token.as_bytes());
    let locked = lock_registry_with_maintenance(context, maintenance)?;
    let mut matches = locked.registry.brokers.values().filter(|broker| {
        broker.state == "ready"
            && digest_eq(&broker.capability_digest, &digest)
            && broker::capability_available(
                context,
                &broker.session_id,
                &broker.incarnation,
                &broker.capability_digest,
            )
            && broker::heartbeat_fresh(
                context,
                &broker.session_id,
                &broker.incarnation,
                broker.heartbeat_epoch,
            )
    });
    let broker = matches.next().cloned().ok_or_else(unauthorized)?;
    if matches.next().is_some() {
        return Err(unauthorized());
    }
    let claim = if require_active_claim {
        let now = now_epoch();
        Some(
            locked
                .registry
                .claims
                .iter()
                .find(|claim| {
                    claim.session_id == broker.session_id
                        && claim.session_incarnation == broker.incarnation
                        && claim.state == "active"
                        && claim.expires_at_epoch > now
                })
                .map(|claim| ControllerClaimTuple {
                    claim_id: claim.claim_id.clone(),
                    revision: claim.revision,
                    expires_at_epoch: claim.expires_at_epoch,
                })
                .ok_or_else(|| {
                    CliError::data(
                        "claim-not-active",
                        "Main Agent claim must be active and unexpired",
                        None,
                    )
                })?,
        )
    } else {
        None
    };
    drop(locked);
    let record = load_session_record(context, &broker.session_id).map_err(|_| unauthorized())?;
    if incarnation(&record)? != broker.incarnation {
        return Err(unauthorized());
    }
    Ok((record, broker.incarnation, claim))
}

pub(crate) fn authenticate_token(
    context: &CliContext,
    session_id: &str,
    token: &str,
) -> Result<(SessionRecord, String), CliError> {
    if token.len() < 32 || token.len() > 256 || !token.is_ascii() {
        return Err(unauthorized());
    }
    let record = load_session_record(context, session_id).map_err(|_| unauthorized())?;
    let incarnation = incarnation(&record)?;
    let locked = lock_registry(context)?;
    let broker = locked
        .registry
        .brokers
        .get(&record.id)
        .ok_or_else(unauthorized)?;
    if broker.state != "ready"
        || broker.incarnation != incarnation
        || !digest_eq(&broker.capability_digest, &digest_bytes(token.as_bytes()))
        || !broker::capability_available(
            context,
            &record.id,
            &incarnation,
            &broker.capability_digest,
        )
    {
        return Err(unauthorized());
    }
    if !broker::heartbeat_fresh(context, &record.id, &incarnation, broker.heartbeat_epoch) {
        return Err(CliError::runtime(
            "coordination-broker-lost",
            "coordination broker heartbeat is stale",
            None,
        ));
    }
    Ok((record, incarnation))
}

pub(crate) fn authenticate_recovery_token(
    context: &CliContext,
    session_id: &str,
    token: &str,
) -> Result<(SessionRecord, String), CliError> {
    if token.len() < 32 || token.len() > 256 || !token.is_ascii() {
        return Err(unauthorized());
    }
    let record = load_session_record(context, session_id).map_err(|_| unauthorized())?;
    let incarnation = incarnation(&record)?;
    let generation = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.generation)
        .unwrap_or_default();
    let locked = lock_registry(context)?;
    let broker = locked
        .registry
        .brokers
        .get(&record.id)
        .ok_or_else(unauthorized)?;
    if !matches!(broker.state.as_str(), "ready" | "recovering")
        || broker.session_id != record.id
        || broker.incarnation != incarnation
        || broker.generation != generation
        || !digest_eq(&broker.capability_digest, &digest_bytes(token.as_bytes()))
        || !broker::capability_available(
            context,
            &record.id,
            &incarnation,
            &broker.capability_digest,
        )
    {
        return Err(unauthorized());
    }
    Ok((record, incarnation))
}

pub(crate) fn authenticate_any_token(
    context: &CliContext,
    token: &str,
) -> Result<(SessionRecord, String), CliError> {
    if token.len() < 32 || token.len() > 256 || !token.is_ascii() {
        return Err(unauthorized());
    }
    let digest = digest_bytes(token.as_bytes());
    let locked = lock_registry(context)?;
    let mut matches = locked.registry.brokers.values().filter(|broker| {
        broker.state == "ready"
            && digest_eq(&broker.capability_digest, &digest)
            && broker::capability_available(
                context,
                &broker.session_id,
                &broker.incarnation,
                &broker.capability_digest,
            )
            && broker::heartbeat_fresh(
                context,
                &broker.session_id,
                &broker.incarnation,
                broker.heartbeat_epoch,
            )
    });
    let broker = matches.next().cloned().ok_or_else(unauthorized)?;
    if matches.next().is_some() {
        return Err(unauthorized());
    }
    drop(locked);
    let record = load_session_record(context, &broker.session_id).map_err(|_| unauthorized())?;
    if incarnation(&record)? != broker.incarnation {
        return Err(unauthorized());
    }
    Ok((record, broker.incarnation))
}

pub(crate) fn revalidate_capability_file(
    context: &CliContext,
    registry: &Registry,
    record: &SessionRecord,
    expected_incarnation: &str,
    capability_file: &Path,
) -> Result<(), CliError> {
    if incarnation(record)? != expected_incarnation {
        return Err(unauthorized());
    }
    let token = read_private_file(capability_file, 512).map_err(|_| unauthorized())?;
    let token = String::from_utf8(token).map_err(|_| unauthorized())?;
    let broker = registry
        .brokers
        .get(&record.id)
        .filter(|broker| {
            broker.incarnation == expected_incarnation
                && broker.state == "ready"
                && digest_eq(
                    &broker.capability_digest,
                    &digest_bytes(token.trim().as_bytes()),
                )
                && broker::capability_available(
                    context,
                    &record.id,
                    expected_incarnation,
                    &broker.capability_digest,
                )
                && broker::heartbeat_fresh(
                    context,
                    &record.id,
                    expected_incarnation,
                    broker.heartbeat_epoch,
                )
        })
        .ok_or_else(unauthorized)?;
    let _ = broker;
    Ok(())
}

pub(crate) fn incarnation(record: &SessionRecord) -> Result<String, CliError> {
    record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            CliError::data(
                "session-incarnation-conflict",
                "session incarnation is unavailable",
                None,
            )
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegistryMaintenance {
    Full,
    Observational,
}

pub(crate) fn lock_registry(context: &CliContext) -> Result<LockedRegistry, CliError> {
    lock_registry_with_maintenance(context, RegistryMaintenance::Full)
}

pub(crate) fn lock_registry_observational(
    context: &CliContext,
) -> Result<LockedRegistry, CliError> {
    lock_registry_with_maintenance(context, RegistryMaintenance::Observational)
}

fn lock_registry_with_maintenance(
    context: &CliContext,
    maintenance: RegistryMaintenance,
) -> Result<LockedRegistry, CliError> {
    let root = coordination_root(context)?;
    let lock_path = root.join(REGISTRY_LOCK);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(SECRET_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock_path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                store_untrusted()
            } else {
                store_unavailable()
            }
        })?;
    lock.set_permissions(fs::Permissions::from_mode(SECRET_FILE_MODE))
        .map_err(|_| store_unavailable())?;
    let lock_metadata = lock.metadata().map_err(|_| store_unavailable())?;
    if !lock_metadata.is_file()
        || lock_metadata.uid() != unsafe { libc::geteuid() }
        || lock_metadata.mode() & 0o077 != 0
        || lock_metadata.nlink() != 1
    {
        return Err(store_untrusted());
    }
    let started = Instant::now();
    loop {
        // SAFETY: flock is called with a valid, owned file descriptor.
        let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EWOULDBLOCK) || started.elapsed() >= LOCK_TIMEOUT {
            return Err(CliError::runtime(
                "coordination-lock-timeout",
                "coordination registry lock could not be acquired",
                None,
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    let path = root.join(REGISTRY_FILE);
    let mut registry = match read_private_file(&path, MAX_REGISTRY_BYTES) {
        Ok(bytes) => {
            let registry: Registry = serde_json::from_slice(&bytes).map_err(|_| store_corrupt())?;
            if !registry.schema_version.is_empty()
                && !matches!(
                    registry.schema_version.as_str(),
                    REGISTRY_VERSION | CLAIM_FENCE_REGISTRY_VERSION
                )
            {
                return Err(store_corrupt());
            }
            registry
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Registry {
            schema_version: REGISTRY_VERSION.to_string(),
            ..Registry::default()
        },
        Err(_) => return Err(store_unavailable()),
    };
    let now = now_epoch();
    let mut renewed = if maintenance == RegistryMaintenance::Full {
        notification::normalize_registry(&mut registry, now)
    } else {
        false
    };
    if maintenance == RegistryMaintenance::Full {
        let Registry {
            claims, brokers, ..
        } = &mut registry;
        for claim in claims {
            if claim.state != "active" || claim.expires_at_epoch > now.saturating_add(15 * 60) {
                continue;
            }
            let Some(broker) = brokers.get(&claim.session_id) else {
                continue;
            };
            if broker.incarnation != claim.session_incarnation || broker.state != "ready" {
                continue;
            }
            if ensure_claim_mutation_not_fenced(
                context,
                &claim.session_id,
                &claim.session_incarnation,
                claim,
            )
            .is_err()
            {
                continue;
            }
            let Ok(record) = load_session_record(context, &claim.session_id) else {
                continue;
            };
            if crate::orchestration::ensure_session_not_runtime_stop_fenced(context, &record)
                .is_err()
            {
                continue;
            }
            if broker::capability_available(
                context,
                &claim.session_id,
                &claim.session_incarnation,
                &broker.capability_digest,
            ) && broker::heartbeat_fresh(
                context,
                &claim.session_id,
                &claim.session_incarnation,
                broker.heartbeat_epoch,
            ) {
                claim.expires_at_epoch = now.saturating_add(30 * 60);
                claim.expires_at = timestamp(claim.expires_at_epoch);
                renewed = true;
            }
        }
    }
    if maintenance == RegistryMaintenance::Full {
        let operation_snapshots: Vec<_> = registry
            .operations
            .iter()
            .enumerate()
            .filter(|(_, lease)| {
                matches!(
                    lease.state.as_str(),
                    "active" | "completing" | "reconcile_pending"
                ) && lease.expires_at_epoch <= now.saturating_add(15 * 60)
            })
            .map(|(index, lease)| (index, lease.clone()))
            .collect();
        for (index, lease) in operation_snapshots {
            #[cfg(debug_assertions)]
            if let Some(path) =
                std::env::var_os("NILS_AGENT_SESSION_TEST_COORDINATION_OPERATION_PROBE_LOG")
            {
                use std::io::Write;

                if let Ok(mut log) = OpenOptions::new().create(true).append(true).open(path) {
                    let _ = writeln!(log, "{}", lease.lease_id);
                }
            }
            let Some(broker) = registry.brokers.get(&lease.session_id) else {
                continue;
            };
            if broker.incarnation != lease.session_incarnation
                || broker.state != "ready"
                || !broker::capability_available(
                    context,
                    &lease.session_id,
                    &lease.session_incarnation,
                    &broker.capability_digest,
                )
                || !broker::heartbeat_fresh(
                    context,
                    &lease.session_id,
                    &lease.session_incarnation,
                    broker.heartbeat_epoch,
                )
            {
                continue;
            }
            let Ok(record) = load_session_record(context, &lease.session_id) else {
                continue;
            };
            let runtime_matches =
                crate::coordination_runtime_evidence(&record).is_ok_and(|runtime| {
                    runtime.status == crate::CoordinationRuntimeStatus::Running
                        && runtime.identity_digest == lease.runtime_identity_digest
                });
            let activity_matches =
                crate::activity::state_for_view(context, &record).is_some_and(|activity| {
                    activity.phase == crate::activity::TurnPhase::Working
                        && claims::activity_identity_digest(&activity)
                            == lease.activity_identity_digest
                });
            let descendant_matches = lease
                .descendant
                .as_ref()
                .is_some_and(claims::descendant_is_live);
            if runtime_matches && (activity_matches || descendant_matches) {
                let current = &mut registry.operations[index];
                current.state = "active".to_string();
                current.reconcile_observed_at_epoch = None;
                current.expires_at_epoch = now.saturating_add(30 * 60);
                current.expires_at = timestamp(current.expires_at_epoch);
                renewed = true;
            }
        }
    }
    if renewed {
        let bytes = serde_json::to_vec_pretty(&registry).map_err(|_| store_corrupt())?;
        write_atomic(&path, &bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())?;
    }
    Ok(LockedRegistry {
        _lock: lock,
        path,
        registry,
    })
}

fn coordination_root(context: &CliContext) -> Result<PathBuf, CliError> {
    let root = context.state_dir.join("coordination");
    if let Ok(metadata) = fs::symlink_metadata(&root) {
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(store_untrusted());
        }
    } else {
        fs::create_dir_all(&root).map_err(|_| store_unavailable())?;
    }
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .map_err(|_| store_unavailable())?;
    let canonical_state = fs::canonicalize(&context.state_dir).map_err(|_| store_unavailable())?;
    let canonical_root = fs::canonicalize(&root).map_err(|_| store_unavailable())?;
    if !canonical_root.starts_with(&canonical_state) {
        return Err(store_untrusted());
    }
    Ok(root)
}

fn read_private_file(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.len() > max_bytes
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private coordination file is untrusted",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private coordination file is oversized",
        ));
    }
    Ok(bytes)
}

pub(crate) fn read_private_text(
    path: &Path,
    max_bytes: u64,
    code: &'static str,
) -> Result<String, CliError> {
    let bytes = read_private_file(path, max_bytes)
        .map_err(|_| CliError::data(code, "private coordination input is invalid", None))?;
    let value = String::from_utf8(bytes)
        .map_err(|_| CliError::data(code, "private coordination input is invalid", None))?;
    Ok(value.trim().to_string())
}

fn expire_claims_with_runtime_resolver(
    claims: &mut [context::WorkContextRecord],
    operations: &[claims::OperationLease],
    now: i64,
    mut runtime_stopped: impl FnMut(&str, &str) -> bool,
) -> bool {
    let mut changed = false;
    let mut stopped_runtime_cache = BTreeMap::new();
    for claim in claims {
        let bound_operation = operations.iter().any(|operation| {
            operation.claim_id == claim.claim_id
                && matches!(
                    operation.state.as_str(),
                    "active" | "completing" | "reconcile_pending"
                )
        });
        let can_expire =
            claim.state == "active" && claim.expires_at_epoch <= now && !bound_operation;
        let owner_runtime_stopped = can_expire
            && *stopped_runtime_cache
                .entry((claim.session_id.clone(), claim.session_incarnation.clone()))
                .or_insert_with(|| runtime_stopped(&claim.session_id, &claim.session_incarnation));
        if can_expire && owner_runtime_stopped {
            claim.state = "expired".to_string();
            claim.revision = claim.revision.saturating_add(1);
            claim.terminal_at_epoch = Some(now);
            changed = true;
        } else if matches!(claim.state.as_str(), "released" | "expired" | "stale")
            && claim.terminal_at_epoch.is_none()
        {
            claim.terminal_at_epoch = Some(now);
            changed = true;
        }
    }
    changed
}

pub(crate) fn clean_expired(registry: &mut Registry, now: i64) -> bool {
    let mut changed = false;
    for operation in &mut registry.operations {
        if operation.state == "active" && operation.expires_at_epoch <= now {
            operation.state = "completing".to_string();
            operation.revision = operation.revision.saturating_add(1);
            changed = true;
        }
        if matches!(
            operation.state.as_str(),
            "completed" | "failed" | "abandoned"
        ) && operation.terminal_at_epoch.is_none()
        {
            operation.terminal_at_epoch = Some(now);
            changed = true;
        }
    }
    changed |= expire_claims_with_runtime_resolver(
        &mut registry.claims,
        &registry.operations,
        now,
        |session_id, session_incarnation| {
            registry
                .brokers
                .get(session_id)
                .filter(|broker| broker.incarnation == session_incarnation)
                .and_then(|broker| broker.runtime_identity.as_ref())
                .is_some_and(|identity| {
                    crate::coordination_runtime_status_for_identity(identity)
                        == crate::CoordinationRuntimeStatus::Stopped
                })
        },
    );
    for message in &mut registry.messages {
        if !matches!(
            message.state.as_str(),
            "acknowledged" | "expired" | "deleted"
        ) && message.expires_at_epoch <= now
        {
            message.state = "expired".to_string();
            message.revision = message.revision.saturating_add(1);
            message.terminal_at_epoch = Some(now);
            changed = true;
        } else if matches!(
            message.state.as_str(),
            "acknowledged" | "expired" | "deleted"
        ) && message.terminal_at_epoch.is_none()
        {
            message.terminal_at_epoch = Some(now);
            changed = true;
        }
    }
    let removed_messages: std::collections::BTreeSet<_> = registry
        .messages
        .iter()
        .filter(|message| {
            message.terminal_at_epoch.is_some_and(|terminal| {
                let retention = if message.state == "acknowledged" {
                    ACKNOWLEDGED_MESSAGE_RETENTION_SECS
                } else {
                    TERMINAL_RETENTION_SECS
                };
                terminal <= now.saturating_sub(retention)
            })
        })
        .map(|message| message.message_id.clone())
        .collect();
    let message_count = registry.messages.len();
    let notification_count = registry.notifications.len();
    let operation_count = registry.operations.len();
    let claim_count = registry.claims.len();
    let receipt_count = registry.receipts.len();
    let cursor_count = registry.cursors.len();
    let completion_event_count = registry.completion_events.len();
    let acknowledgement_count = registry.advisory_acknowledgements.len();
    let observation_count = registry.advisory_observations.len();
    registry
        .messages
        .retain(|message| !removed_messages.contains(&message.message_id));
    let retained_notification_targets: std::collections::BTreeSet<_> = registry
        .messages
        .iter()
        .map(|message| {
            (
                message.recipient_session_id.clone(),
                message.recipient_incarnation.clone(),
            )
        })
        .collect();
    registry.notifications.retain(|key, receipt| {
        !removed_messages.contains(key)
            && retained_notification_targets.contains(&(
                receipt.target_session_id.clone(),
                receipt.target_incarnation.clone(),
            ))
    });
    registry.operations.retain(|operation| {
        operation
            .terminal_at_epoch
            .is_none_or(|terminal| terminal > now.saturating_sub(TERMINAL_RETENTION_SECS))
    });
    registry.claims.retain(|claim| {
        claim
            .terminal_at_epoch
            .is_none_or(|terminal| terminal > now.saturating_sub(TERMINAL_RETENTION_SECS))
    });
    registry
        .receipts
        .retain(|_, receipt| receipt.expires_at_epoch > now);
    registry
        .cursors
        .retain(|_, cursor| cursor.expires_at_epoch > now);
    registry
        .advisory_acknowledgements
        .retain(|_, acknowledgement| acknowledgement.expires_at_epoch > now);
    registry.advisory_observations.retain(|_, observation| {
        observation.observed_at_epoch > now.saturating_sub(RECEIPT_TTL_SECS)
    });
    let operations = &registry.operations;
    registry.completion_events.retain(|event| {
        event.created_at_epoch > now.saturating_sub(RECEIPT_TTL_SECS)
            && operations.iter().any(|operation| {
                operation.lease_id == event.lease_id
                    && matches!(
                        operation.state.as_str(),
                        "active" | "completing" | "reconcile_pending"
                    )
            })
    });
    changed
        || registry.messages.len() != message_count
        || registry.notifications.len() != notification_count
        || registry.operations.len() != operation_count
        || registry.claims.len() != claim_count
        || registry.receipts.len() != receipt_count
        || registry.cursors.len() != cursor_count
        || registry.advisory_acknowledgements.len() != acknowledgement_count
        || registry.advisory_observations.len() != observation_count
        || registry.completion_events.len() != completion_event_count
}

pub(crate) fn idempotency_replay(
    registry: &Registry,
    key: &str,
    principal: &str,
    incarnation: &str,
    operation: &str,
    digest: &str,
) -> Result<Option<Value>, CliError> {
    validate_idempotency_key(key)?;
    let receipt_key = receipt_key(principal, incarnation, operation, key);
    let Some(receipt) = registry.receipts.get(&receipt_key) else {
        return Ok(None);
    };
    if receipt.expires_at_epoch <= now_epoch() {
        return Ok(None);
    }
    if receipt.principal == principal
        && receipt.incarnation == incarnation
        && receipt.operation == operation
        && receipt.digest == digest
    {
        return Ok(Some(receipt.outcome.clone()));
    }
    Err(CliError::data(
        "idempotency-key-reused",
        "idempotency key is already bound to another request",
        None,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn store_receipt(
    registry: &mut Registry,
    key: String,
    principal: String,
    incarnation: String,
    operation: String,
    digest: String,
    outcome: Value,
    now: i64,
) -> Result<(), CliError> {
    let receipt_key = receipt_key(&principal, &incarnation, &operation, &key);
    if !registry.receipts.contains_key(&receipt_key)
        && (registry.receipts.len() >= MAX_RECEIPTS_GLOBAL
            || registry
                .receipts
                .values()
                .filter(|receipt| receipt.principal == principal)
                .count()
                >= MAX_RECEIPTS_PER_PRINCIPAL)
    {
        return Err(CliError::data(
            "quota-exceeded",
            "coordination idempotency receipt quota exceeded",
            None,
        ));
    }
    registry.receipts.insert(
        receipt_key,
        IdempotencyReceipt {
            principal,
            incarnation,
            operation,
            digest,
            outcome,
            expires_at_epoch: now.saturating_add(RECEIPT_TTL_SECS),
        },
    );
    Ok(())
}

fn receipt_key(principal: &str, incarnation: &str, operation: &str, key: &str) -> String {
    request_digest(
        "idempotency-receipt-key",
        &(principal, incarnation, operation, key),
    )
}

fn validate_idempotency_key(key: &str) -> Result<(), CliError> {
    if !(8..=128).contains(&key.len())
        || !key.is_ascii()
        || key
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(CliError::usage(
            "invalid-idempotency-key",
            "idempotency key must be 8-128 printable non-space ASCII bytes",
            None,
        ));
    }
    Ok(())
}

pub(crate) fn request_digest<T: Serialize>(operation: &str, value: &T) -> String {
    let mut digest = Sha256::new();
    digest.update(operation.as_bytes());
    digest.update([0]);
    digest.update(serde_json::to_vec(value).unwrap_or_default());
    hex(&digest.finalize())
}

pub(crate) fn ensure_fingerprint_key(registry: &mut Registry) {
    if registry.fingerprint_epoch == 0 {
        registry.fingerprint_epoch = 1;
    }
    if registry.fingerprint_key.is_empty() {
        registry.fingerprint_key = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
    }
}

pub(crate) fn worktree_fingerprint(
    registry: &Registry,
    checkout: &Path,
) -> Result<String, CliError> {
    if registry.fingerprint_epoch == 0 || registry.fingerprint_key.len() < 32 {
        return Err(store_corrupt());
    }
    nils_common::coordination_projection::worktree_fingerprint(
        registry.fingerprint_epoch,
        &registry.fingerprint_key,
        checkout,
    )
    .ok_or_else(store_corrupt)
}

#[cfg(test)]
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut normalized = [0_u8; BLOCK];
    if key.len() > BLOCK {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; BLOCK];
    let mut outer_pad = [0x5c_u8; BLOCK];
    for index in 0..BLOCK {
        inner_pad[index] ^= normalized[index];
        outer_pad[index] ^= normalized[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner);
    outer.finalize().into()
}

pub(crate) fn digest_bytes(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn digest_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

pub(crate) fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

pub(crate) fn timestamp(epoch: i64) -> String {
    Timestamp::from_second(epoch)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| Timestamp::now().to_string())
}

pub(crate) fn unauthorized() -> CliError {
    CliError::data(
        "coordination-unauthorized",
        "coordination authority could not be verified",
        None,
    )
    .with_hint(
        "this session is not a verified coordination broker incarnation; confirm authority with `main-agent self show` and ensure it was launched under enforced coordination before retrying",
    )
}

fn store_untrusted() -> CliError {
    CliError::runtime(
        "coordination-store-untrusted",
        "coordination store ownership or path is untrusted",
        None,
    )
}

fn store_corrupt() -> CliError {
    CliError::runtime(
        "coordination-store-corrupt",
        "coordination store is corrupt or unsupported",
        None,
    )
}

fn store_unavailable() -> CliError {
    CliError::runtime(
        "coordination-unavailable",
        "coordination store is unavailable",
        None,
    )
}

pub(crate) fn public_summary(context: &CliContext, session_id: &str) -> CoordinationSummary {
    let registry_path = context.state_dir.join("coordination").join(REGISTRY_FILE);
    if matches!(
        fs::symlink_metadata(&registry_path),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    ) {
        return CoordinationSummary::default();
    }
    let Ok(locked) = lock_registry(context) else {
        return CoordinationSummary::default();
    };
    let claim = locked
        .registry
        .claims
        .iter()
        .find(|claim| claim.session_id == session_id && claim.state == "active");
    CoordinationSummary {
        work_context_state: claim.map(|claim| claim.state.clone()),
        claim_id: claim.map(|claim| claim.claim_id.clone()),
        claim_expires_at: claim.map(|claim| claim.expires_at.clone()),
        unread_message_count: locked
            .registry
            .messages
            .iter()
            .filter(|message| {
                message.recipient_session_id == session_id && message.state == "unread"
            })
            .count(),
        coordination_conflict_severity: claims::conflict_severity_for_session(
            context,
            &locked.registry,
            session_id,
        ),
        coordination_available: locked
            .registry
            .brokers
            .get(session_id)
            .is_some_and(|broker| {
                broker.state == "ready"
                    && broker::capability_available(
                        context,
                        session_id,
                        &broker.incarnation,
                        &broker.capability_digest,
                    )
                    && broker::heartbeat_fresh(
                        context,
                        session_id,
                        &broker.incarnation,
                        broker.heartbeat_epoch,
                    )
            }),
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct CoordinationSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_context_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_expires_at: Option<String>,
    pub unread_message_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordination_conflict_severity: Option<String>,
    pub coordination_available: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct GuidanceSummary {
    pub unread_count: usize,
    pub consumed_count: usize,
    pub stale_incarnation_unread_count: usize,
}

fn guidance_summary_from_registry(
    registry: &Registry,
    session_id: &str,
    incarnation: &str,
    controller_session_id: &str,
    controller_incarnation: &str,
    now: i64,
) -> GuidanceSummary {
    let mut summary = GuidanceSummary::default();
    for message in registry.messages.iter().filter(|message| {
        message.recipient_session_id == session_id
            && message.sender_session_id == controller_session_id
            && message.sender_incarnation == controller_incarnation
    }) {
        if message.state == "unread"
            && message.expires_at_epoch > now
            && message.recipient_incarnation != incarnation
        {
            summary.stale_incarnation_unread_count =
                summary.stale_incarnation_unread_count.saturating_add(1);
            continue;
        }
        if message.recipient_incarnation != incarnation {
            continue;
        }
        match message.state.as_str() {
            "unread" if message.expires_at_epoch > now => {
                summary.unread_count = summary.unread_count.saturating_add(1);
            }
            "read" | "acknowledged" => {
                summary.consumed_count = summary.consumed_count.saturating_add(1);
            }
            _ => {}
        }
    }
    summary
}

pub(crate) fn carry_forward_unread_controller_guidance_with_authorization<G, F>(
    context: &CliContext,
    recipient_session_id: &str,
    previous_incarnation: &str,
    current_incarnation: &str,
    controller_session_id: &str,
    controller_incarnation: &str,
    authorize: F,
) -> Result<usize, CliError>
where
    F: FnOnce() -> Result<G, CliError>,
{
    mailbox::carry_forward_unread_controller_guidance_with_authorization(
        context,
        recipient_session_id,
        previous_incarnation,
        current_incarnation,
        controller_session_id,
        controller_incarnation,
        authorize,
    )
}

pub(crate) fn quarantine_orphaned_controller_guidance_with_authorization<G, F>(
    context: &CliContext,
    recipient_session_id: &str,
    current_incarnation: &str,
    controller_session_id: &str,
    controller_incarnation: &str,
    authorize: F,
) -> Result<usize, CliError>
where
    F: FnOnce() -> Result<G, CliError>,
{
    mailbox::quarantine_orphaned_controller_guidance_with_authorization(
        context,
        recipient_session_id,
        current_incarnation,
        controller_session_id,
        controller_incarnation,
        authorize,
    )
}

pub(crate) fn json_value<T: Serialize>(value: T) -> Result<Value, CliError> {
    serde_json::to_value(value).map_err(|_| {
        CliError::runtime(
            "coordination-store-corrupt",
            "coordination result could not be serialized",
            None,
        )
    })
}

pub(crate) fn read_bounded_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    max_bytes: u64,
    code: &'static str,
) -> Result<T, CliError> {
    let bytes = read_bounded_bytes(path, max_bytes, code)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| CliError::data(code, "coordination input is invalid", None))
}

pub(crate) fn read_bounded_bytes(
    path: &Path,
    max_bytes: u64,
    code: &'static str,
) -> Result<Vec<u8>, CliError> {
    let mut file = File::open(path)
        .map_err(|_| CliError::data(code, "coordination input could not be read", None))?;
    let metadata = file
        .metadata()
        .map_err(|_| CliError::data(code, "coordination input could not be read", None))?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(CliError::data(code, "coordination input is invalid", None));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| CliError::data(code, "coordination input could not be read", None))?;
    if bytes.len() as u64 > max_bytes {
        return Err(CliError::data(code, "coordination input is invalid", None));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_version_skew_preserves_singular_and_adds_supported_versions() {
        let temporary = tempfile::TempDir::new().expect("temporary state");
        let state_dir = temporary.path().join("state");
        let coordination = state_dir.join("coordination");
        fs::create_dir_all(&coordination).expect("coordination directory");
        write_atomic(
            &coordination.join(REGISTRY_FILE),
            br#"{"schema_version":"agent-session.coordination-registry.v999"}"#,
            SECRET_FILE_MODE,
        )
        .expect("unsupported registry");
        let context = CliContext {
            state_dir,
            host: None,
        };
        let error =
            ensure_recovery_registry_schema(&context).expect_err("unsupported registry must fail");
        assert_eq!(error.code(), "coordination-facade-version-skew");
        let details = error.0.details.as_ref().expect("version-skew details");
        assert_eq!(
            details["supported_schema_version"],
            CLAIM_FENCE_REGISTRY_VERSION
        );
        assert_eq!(
            details["supported_schema_versions"],
            json!([REGISTRY_VERSION, CLAIM_FENCE_REGISTRY_VERSION])
        );
        assert_eq!(
            details["found_schema_version"],
            "agent-session.coordination-registry.v999"
        );
        assert!(details["required_action"].is_string());
    }
    use serde_json::json;

    #[test]
    fn bounded_reader_rejects_an_oversized_regular_file_before_allocating_its_length() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("oversized.json");
        fs::write(&path, vec![b'x'; 65]).expect("fixture");

        let error =
            read_bounded_bytes(&path, 64, "oversized-input").expect_err("must reject oversized");
        assert_eq!(error.code(), "oversized-input");
    }

    #[test]
    fn runtime_stop_seal_accepts_admitted_claim_after_wall_clock_expiry_under_guard() {
        let registry = serde_json::from_value::<Registry>(json!({
            "claims": [{
                "schema_version": "agent-session.work-context.v1",
                "session_id": "main",
                "session_incarnation": "main-incarnation",
                "claim_id": "main-claim",
                "revision": 7,
                "state": "active",
                "intent": "implementation",
                "tier": "L2",
                "repositories": [],
                "worktrees": [],
                "provider_refs": [],
                "plan_refs": [],
                "scopes": [],
                "summary": "runtime stop admission fixture",
                "updated_at": "2030-01-01T00:00:00Z",
                "expires_at": "2030-01-01T00:00:10Z",
                "expires_at_epoch": 100
            }]
        }))
        .expect("registry");
        let controller = ("main".to_string(), "main-incarnation".to_string());
        let admitted = ControllerClaimTuple {
            claim_id: "main-claim".to_string(),
            revision: 7,
            expires_at_epoch: 100,
        };

        assert!(controller_claim_matches_admission(
            &registry,
            &controller,
            &admitted,
            101,
            false,
        ));
        assert!(
            !controller_claim_matches_admission(&registry, &controller, &admitted, 101, true,),
            "a fresh command or stopped-worker stage must reauthenticate after expiry"
        );
    }

    #[test]
    fn group_cleanup_never_overrides_operations_and_force_only_overrides_claims() {
        let sessions = vec![("worker".to_string(), "incarnation".to_string())];
        let registry_with = |operation_state: Option<&str>| {
            let operations = operation_state
                .map(|state| {
                    json!([{
                        "schema_version": "agent-session.operation-lease.v1",
                        "lease_id": "lease",
                        "session_id": "worker",
                        "session_incarnation": "incarnation",
                        "claim_id": "claim",
                        "claim_revision": 1,
                        "operation": "edit",
                        "targets": [],
                        "state": state,
                        "revision": 1,
                        "started_at": "2030-01-01T00:00:00Z",
                        "expires_at": "2030-01-01T00:10:00Z",
                        "expires_at_epoch": i64::MAX,
                        "terminal_at_epoch": null,
                        "execution_token_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "activity_revision": 1,
                        "activity_identity_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                        "runtime_identity_digest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                        "outcome": null
                    }])
                })
                .unwrap_or_else(|| json!([]));
            serde_json::from_value::<Registry>(json!({
                "claims": [{
                    "schema_version": "agent-session.work-context.v1",
                    "session_id": "worker",
                    "session_incarnation": "incarnation",
                    "claim_id": "claim",
                    "revision": 1,
                    "state": "active",
                    "intent": "implementation",
                    "tier": "L2",
                    "repositories": ["example/repository"],
                    "worktrees": [],
                    "provider_refs": [],
                    "plan_refs": [],
                    "scopes": [],
                    "summary": "fixture",
                    "updated_at": "2030-01-01T00:00:00Z",
                    "expires_at": "2030-01-01T00:10:00Z",
                    "expires_at_epoch": i64::MAX
                }],
                "operations": operations
            }))
            .expect("registry")
        };

        let safe_claim = ensure_group_cleanup_quiescence(&registry_with(None), &sessions, false)
            .expect_err("safe cleanup must retain an active claim");
        assert_eq!(safe_claim.code(), "worker-not-quiescent");
        ensure_group_cleanup_quiescence(&registry_with(None), &sessions, true)
            .expect("force may revoke a claim after proving operation quiescence");
        for state in ["active", "completing", "reconcile_pending"] {
            let error =
                ensure_group_cleanup_quiescence(&registry_with(Some(state)), &sessions, true)
                    .expect_err("force must never override an operation");
            assert_eq!(error.code(), "worker-not-quiescent", "state={state}");
        }

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        let mut registry = registry_with(None);
        registry.brokers.insert(
            "worker".to_string(),
            BrokerRecord {
                session_id: "worker".to_string(),
                incarnation: "incarnation".to_string(),
                coordination_mode: crate::cli::CoordinationMode::Enforce,
                capability_digest:
                    "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
                        .to_string(),
                generation: 1,
                state: "ready".to_string(),
                heartbeat_at: "2030-01-01T00:00:00Z".to_string(),
                heartbeat_epoch: i64::MAX,
                runtime_identity: None,
                runtime_identity_digest: String::new(),
                lost_since_epoch: None,
            },
        );
        {
            let mut locked = lock_registry(&context).expect("lock");
            locked.registry = registry;
            locked.save().expect("seed registry");
        }
        let capability = capability_path(&context, "worker", "incarnation");
        fs::create_dir_all(capability.parent().expect("capability parent"))
            .expect("capability dir");
        fs::write(&capability, b"private capability").expect("capability");
        let guard = lock_group_cleanup_quiescence(&context, &sessions, true).expect("force guard");
        let observer_context = context.clone();
        let observer_capability = capability.clone();
        let (observed_tx, observed_rx) = std::sync::mpsc::channel();
        let observer = std::thread::spawn(move || {
            let locked = lock_registry(&observer_context).expect("observer lock");
            let broker_state = locked.registry.brokers["worker"].state.clone();
            let active_claim = locked
                .registry
                .claims
                .iter()
                .any(|claim| claim.state == "active");
            let authority = claims::ensure_current_broker(
                &observer_context,
                &locked.registry,
                "worker",
                "incarnation",
            )
            .map_err(|error| error.code().to_string());
            observed_tx
                .send((
                    broker_state,
                    active_claim,
                    observer_capability.exists(),
                    authority,
                ))
                .expect("observer result");
        });
        assert!(
            observed_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "a competing authority check must remain blocked before the seal"
        );
        guard.seal(&context).expect("seal");
        let (broker_state, active_claim, capability_exists, authority) = observed_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("observer must complete without lock-order deadlock");
        observer.join().expect("observer");
        assert_eq!(broker_state, "stopped");
        assert!(!active_claim);
        assert!(!capability_exists);
        assert_eq!(
            authority.expect_err("pre-authenticated waiter must fail under the sealed snapshot"),
            "coordination-broker-lost"
        );
    }

    #[test]
    fn digest_comparison_is_exact() {
        assert!(digest_eq("abc", "abc"));
        assert!(!digest_eq("abc", "abd"));
        assert!(!digest_eq("abc", "ab"));
    }

    #[test]
    fn hmac_sha256_matches_the_rfc_4231_vector() {
        let digest = hmac_sha256(&[0x0b; 20], b"Hi There");
        assert_eq!(
            hex(&digest),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn coordination_review_expired_operation_and_bound_claim_remain_fail_closed() {
        let mut registry: Registry = serde_json::from_value(json!({
            "schema_version": REGISTRY_VERSION,
            "claims": [{
                "schema_version": "agent-session.work-context.v1",
                "session_id": "session",
                "session_incarnation": "incarnation",
                "claim_id": "claim",
                "revision": 1,
                "state": "active",
                "intent": "implementation",
                "tier": "L2",
                "repositories": ["example/repository"],
                "worktrees": [],
                "provider_refs": [],
                "plan_refs": [],
                "scopes": [{"kind": "repository", "repository": "example/repository", "value": ""}],
                "summary": "fixture",
                "updated_at": "2030-01-01T00:00:00Z",
                "expires_at": "2030-01-01T00:00:01Z",
                "expires_at_epoch": 1
            }],
            "operations": [{
                "schema_version": "agent-session.operation-lease.v1",
                "lease_id": "lease",
                "session_id": "session",
                "session_incarnation": "incarnation",
                "claim_id": "claim",
                "claim_revision": 1,
                "operation": "edit",
                "targets": [{"kind": "repository", "repository": "example/repository", "value": ""}],
                "state": "active",
                "revision": 1,
                "started_at": "2030-01-01T00:00:00Z",
                "expires_at": "2030-01-01T00:00:01Z",
                "expires_at_epoch": 1,
                "execution_token_digest": "digest"
            }]
        })).expect("registry");
        clean_expired(&mut registry, 2);
        assert_eq!(registry.operations[0].state, "completing");
        assert_eq!(registry.claims[0].state, "active");
    }

    #[test]
    fn coordination_review_terminal_retention_reclaims_mail_and_notifications() {
        let mut registry: Registry = serde_json::from_value(json!({
            "schema_version": REGISTRY_VERSION,
            "messages": [{
                "schema_version": "agent-session.message.v1",
                "message_id": "message",
                "sender_session_id": "sender",
                "sender_incarnation": "sender-incarnation",
                "recipient_session_id": "recipient",
                "recipient_incarnation": "recipient-incarnation",
                "state": "acknowledged",
                "revision": 2,
                "reply_to": null,
                "reply_depth": 0,
                "created_at": "2030-01-01T00:00:00Z",
                "created_at_epoch": 0,
                "expires_at": "2030-01-01T00:00:01Z",
                "expires_at_epoch": 1,
                "terminal_at_epoch": 1,
                "body_bytes": 4,
                "body": "body"
            }],
            "notifications": {
                "message": {
                    "message_id": "message",
                    "target_session_id": "recipient",
                    "target_incarnation": "recipient-incarnation",
                    "state": "queued",
                    "attempted_at_epoch": 1
                }
            }
        }))
        .expect("registry");
        assert!(clean_expired(
            &mut registry,
            ACKNOWLEDGED_MESSAGE_RETENTION_SECS + 2
        ));
        assert!(registry.messages.is_empty());
        assert!(registry.notifications.is_empty());
    }

    #[test]
    fn coordination_review_round2_acknowledged_mail_is_retained_for_24_hours() {
        let now = 24 * 60 * 60;
        let mut registry: Registry = serde_json::from_value(json!({
            "schema_version": REGISTRY_VERSION,
            "messages": [{
                "schema_version": "agent-session.message.v1",
                "message_id": "acknowledged-message",
                "sender_session_id": "sender",
                "sender_incarnation": "sender-incarnation",
                "recipient_session_id": "recipient",
                "recipient_incarnation": "recipient-incarnation",
                "state": "acknowledged",
                "revision": 2,
                "reply_to": null,
                "reply_depth": 0,
                "created_at": "2030-01-01T00:00:00Z",
                "created_at_epoch": 0,
                "expires_at": "2030-01-08T00:00:00Z",
                "expires_at_epoch": i64::MAX,
                "terminal_at_epoch": now - 6 * 60,
                "body_bytes": 4,
                "body": "body"
            }]
        }))
        .expect("registry");
        clean_expired(&mut registry, now);
        assert_eq!(registry.messages.len(), 1);
    }

    #[test]
    fn coordination_review_unknown_runtime_retains_expired_conflict_fence() {
        let mut registry: Registry = serde_json::from_value(json!({
            "schema_version": REGISTRY_VERSION,
            "brokers": {
                "session": {
                    "session_id": "session",
                    "incarnation": "incarnation",
                    "capability_digest": "digest",
                    "generation": 1,
                    "state": "degraded",
                    "heartbeat_at": "2030-01-01T00:00:00Z",
                    "heartbeat_epoch": 1,
                    "runtime_identity": {"malformed": true},
                    "runtime_identity_digest": "runtime"
                }
            },
            "claims": [{
                "schema_version": "agent-session.work-context.v1",
                "session_id": "session",
                "session_incarnation": "incarnation",
                "claim_id": "claim",
                "revision": 1,
                "state": "active",
                "intent": "implementation",
                "tier": "L2",
                "repositories": ["example/repository"],
                "worktrees": [],
                "provider_refs": [],
                "plan_refs": [],
                "scopes": [{"kind": "repository", "repository": "example/repository", "value": ""}],
                "summary": "fixture",
                "updated_at": "2030-01-01T00:00:00Z",
                "expires_at": "2030-01-01T00:00:01Z",
                "expires_at_epoch": 1
            }]
        }))
        .expect("registry");
        assert_eq!(
            registry.brokers["session"].coordination_mode,
            crate::cli::CoordinationMode::Advisory
        );

        clean_expired(&mut registry, 2);
        assert_eq!(registry.claims[0].state, "active");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn expired_claim_runtime_cache_is_fenced_by_exact_incarnation() {
        let pid_namespace =
            fs::metadata("/proc/self/ns/pid").expect("current PID namespace metadata");
        let claim = |claim_id: &str, incarnation: &str| {
            json!({
                "schema_version": "agent-session.work-context.v1",
                "session_id": "session",
                "session_incarnation": incarnation,
                "claim_id": claim_id,
                "revision": 1,
                "state": "active",
                "intent": "implementation",
                "tier": "L2",
                "repositories": ["example/repository"],
                "worktrees": [],
                "provider_refs": [],
                "plan_refs": [],
                "scopes": [{
                    "kind": "repository",
                    "repository": "example/repository",
                    "value": ""
                }],
                "summary": "fixture",
                "updated_at": "2030-01-01T00:00:00Z",
                "expires_at": "2030-01-01T00:00:01Z",
                "expires_at_epoch": 1
            })
        };
        let mut registry: Registry = serde_json::from_value(json!({
            "schema_version": REGISTRY_VERSION,
            "brokers": {
                "session": {
                    "session_id": "session",
                    "incarnation": "incarnation-a",
                    "capability_digest": "digest",
                    "generation": 1,
                    "state": "degraded",
                    "heartbeat_at": "2030-01-01T00:00:00Z",
                    "heartbeat_epoch": 1,
                    "runtime_identity": {
                        "launch_id": "stopped-runtime",
                        "session_id": "$1",
                        "pane_id": "%1",
                        "pane_pid": 2147483647,
                        "process_group_id": 2147483647,
                        "pid_namespace": {
                            "device": pid_namespace.dev(),
                            "inode": pid_namespace.ino(),
                            "boot_id": crate::linux_boot_id().expect("boot id")
                        }
                    },
                    "runtime_identity_digest": "runtime"
                }
            },
            "claims": [
                claim("claim-a-1", "incarnation-a"),
                claim("claim-a-2", "incarnation-a"),
                claim("claim-b", "incarnation-b")
            ]
        }))
        .expect("registry");

        clean_expired(&mut registry, 2);

        assert_eq!(registry.claims[0].state, "expired");
        assert_eq!(registry.claims[1].state, "expired");
        assert_eq!(
            registry.claims[2].state, "active",
            "a cached stopped result must never cross the incarnation fence"
        );
    }

    #[test]
    fn claim_expiry_probes_only_eligible_exact_runtime_pairs_once() {
        let claim = |claim_id: &str, incarnation: &str, state: &str, expires_at_epoch: i64| {
            json!({
                "schema_version": "agent-session.work-context.v1",
                "session_id": "session",
                "session_incarnation": incarnation,
                "claim_id": claim_id,
                "revision": 1,
                "state": state,
                "intent": "implementation",
                "tier": "L2",
                "repositories": ["example/repository"],
                "worktrees": [],
                "provider_refs": [],
                "plan_refs": [],
                "scopes": [],
                "summary": "fixture",
                "updated_at": "2030-01-01T00:00:00Z",
                "expires_at": "2030-01-01T00:00:01Z",
                "expires_at_epoch": expires_at_epoch
            })
        };
        let mut registry: Registry = serde_json::from_value(json!({
            "claims": [
                claim("eligible-a-1", "incarnation-a", "active", 1),
                claim("eligible-a-2", "incarnation-a", "active", 1),
                claim("eligible-b", "incarnation-b", "active", 1),
                claim("unexpired", "incarnation-unexpired", "active", i64::MAX),
                claim("terminal", "incarnation-terminal", "released", 1),
                claim("bound", "incarnation-bound", "active", 1)
            ],
            "operations": [{
                "schema_version": "agent-session.operation-lease.v1",
                "lease_id": "lease",
                "session_id": "session",
                "session_incarnation": "incarnation-bound",
                "claim_id": "bound",
                "claim_revision": 1,
                "operation": "edit",
                "targets": [],
                "state": "active",
                "revision": 1,
                "started_at": "2030-01-01T00:00:00Z",
                "expires_at": "2030-01-01T00:10:00Z",
                "expires_at_epoch": i64::MAX,
                "execution_token_digest": "digest"
            }]
        }))
        .expect("registry");
        let mut calls = BTreeMap::new();

        assert!(expire_claims_with_runtime_resolver(
            &mut registry.claims,
            &registry.operations,
            2,
            |session_id, incarnation| {
                *calls
                    .entry((session_id.to_string(), incarnation.to_string()))
                    .or_insert(0) += 1;
                incarnation == "incarnation-a"
            }
        ));

        assert_eq!(
            calls,
            BTreeMap::from([
                (("session".to_string(), "incarnation-a".to_string()), 1),
                (("session".to_string(), "incarnation-b".to_string()), 1)
            ])
        );
        assert_eq!(registry.claims[0].state, "expired");
        assert_eq!(registry.claims[1].state, "expired");
        assert_eq!(registry.claims[2].state, "active");
        assert_eq!(registry.claims[3].state, "active");
        assert_eq!(registry.claims[4].state, "released");
        assert_eq!(registry.claims[5].state, "active");
    }
}

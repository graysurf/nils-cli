use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::{BrokerHeartbeatArgs, BrokerRecoveryArgs, BrokerStatusArgs, BrokerStopArgs};
use crate::{CliContext, CliError, SessionRecord};

use super::{
    BrokerRecord, authenticate_from_file, authenticate_recovery_from_file, capability_path,
    checkpoint_path_for_state, clean_expired, coordination_dir, digest_bytes,
    ensure_fingerprint_key, idempotency_replay, incarnation, json_value, lock_registry, now_epoch,
    read_bounded_json, request_digest, store_receipt, timestamp,
};

pub(crate) const BROKER_VERSION: &str = "agent-session.coordination-broker.v1";

fn prepare_checkpoint_file(path: &Path) -> Result<bool, CliError> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(SECRET_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => {
            if file
                .set_permissions(fs::Permissions::from_mode(SECRET_FILE_MODE))
                .is_err()
            {
                let _ = fs::remove_file(path);
                return Err(unavailable());
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path).map_err(|_| unavailable())?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.nlink() != 1
                || metadata.permissions().mode() & 0o777 != SECRET_FILE_MODE
            {
                return Err(CliError::runtime(
                    "coordination-store-untrusted",
                    "session checkpoint file is untrusted",
                    None,
                ));
            }
            Ok(false)
        }
        Err(_) => Err(unavailable()),
    }
}

#[derive(Clone, Debug, Serialize)]
struct BrokerStatus {
    schema_version: String,
    session_id: String,
    state: String,
    generation: u64,
    capability_available: bool,
    heartbeat_fresh: bool,
    claim: Option<ClaimSummary>,
    operation: OperationSummary,
}

#[derive(Clone, Debug, Serialize)]
struct ClaimSummary {
    claim_id: String,
    revision: u64,
    state: String,
    expires_at: String,
}

#[derive(Clone, Debug, Default, Serialize)]
struct OperationSummary {
    active: usize,
    uncertain: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecoveryProof {
    schema_version: String,
    session_incarnation: String,
    generation: u64,
}

pub(crate) fn prepare(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    let capability_dir = coordination_dir(context, &record.id);
    match fs::symlink_metadata(&capability_dir) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err(CliError::runtime(
                    "coordination-store-untrusted",
                    "session coordination credential directory is untrusted",
                    None,
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&capability_dir).map_err(|_| unavailable())?;
        }
        Err(_) => return Err(unavailable()),
    }
    fs::set_permissions(&capability_dir, fs::Permissions::from_mode(0o700))
        .map_err(|_| unavailable())
}

pub(crate) fn provision(context: &CliContext, record: &SessionRecord) -> Result<PathBuf, CliError> {
    crate::orchestration::ensure_session_not_quarantined(context, record)?;
    prepare(context, record)?;
    let incarnation = incarnation(record)?;
    let runtime = crate::coordination_runtime_evidence(record).ok();
    let generation = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.generation)
        .unwrap_or_default();
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let path = capability_path(context, &record.id, &incarnation);
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    let previous_broker = locked
        .registry
        .brokers
        .get(&record.id)
        .filter(|broker| broker.incarnation != incarnation)
        .cloned();
    if let Some(previous) = previous_broker.as_ref() {
        let heartbeat_live = heartbeat_fresh(
            context,
            &record.id,
            &previous.incarnation,
            previous.heartbeat_epoch,
        );
        let previous_runtime_status = previous
            .runtime_identity
            .as_ref()
            .map(crate::coordination_runtime_status_for_identity)
            .unwrap_or(crate::CoordinationRuntimeStatus::Unknown);
        if heartbeat_live || previous_runtime_status == crate::CoordinationRuntimeStatus::Running {
            if previous.lost_since_epoch.is_some() {
                if let Some(previous) = locked.registry.brokers.get_mut(&record.id) {
                    previous.lost_since_epoch = None;
                }
                locked.save()?;
            }
            return Err(CliError::data(
                "session-incarnation-conflict",
                "the prior coordination incarnation is still live",
                None,
            ));
        }
        if previous_runtime_status == crate::CoordinationRuntimeStatus::Unknown {
            return Err(CliError::runtime(
                "coordination-runtime-unverified",
                "the prior coordination runtime identity cannot be proven stopped",
                None,
            ));
        }
        let previous_operation = locked.registry.operations.iter().any(|lease| {
            lease.session_id == record.id
                && lease.session_incarnation == previous.incarnation
                && matches!(
                    lease.state.as_str(),
                    "active" | "completing" | "reconcile_pending"
                )
        });
        if previous_operation {
            let unexpired = locked.registry.operations.iter().any(|lease| {
                lease.session_id == record.id
                    && lease.session_incarnation == previous.incarnation
                    && matches!(
                        lease.state.as_str(),
                        "active" | "completing" | "reconcile_pending"
                    )
                    && lease.expires_at_epoch > now
            });
            if unexpired {
                return Err(CliError::data(
                    "operation-in-progress",
                    "an unexpired operation remains bound to the prior incarnation",
                    None,
                ));
            }
            let lost_since = previous.lost_since_epoch.unwrap_or(now);
            if previous.lost_since_epoch.is_none() {
                if let Some(previous) = locked.registry.brokers.get_mut(&record.id) {
                    previous.lost_since_epoch = Some(now);
                }
                locked.save()?;
            }
            if lost_since > now.saturating_sub(10 * 60) {
                return Err(CliError::data(
                    "broker-replacement-grace",
                    "expired prior operations require ten minutes of continuously stopped runtime evidence",
                    Some(json!({ "retry_after_epoch": lost_since.saturating_add(10 * 60) })),
                ));
            }
        }
    } else if locked.registry.operations.iter().any(|lease| {
        lease.session_id == record.id
            && lease.session_incarnation != incarnation
            && matches!(
                lease.state.as_str(),
                "active" | "completing" | "reconcile_pending"
            )
    }) {
        return Err(CliError::data(
            "operation-in-progress",
            "an unresolved prior operation has no terminal runtime evidence",
            None,
        ));
    }
    let previous_capability = previous_broker
        .as_ref()
        .map(|broker| capability_path(context, &record.id, &broker.incarnation));
    let previous_checkpoint = previous_broker.as_ref().map(|broker| {
        checkpoint_path_for_state(&context.state_dir, &record.id, &broker.incarnation)
    });
    write_atomic(&path, token.as_bytes(), SECRET_FILE_MODE).map_err(|_| unavailable())?;
    let checkpoint_path = checkpoint_path_for_state(&context.state_dir, &record.id, &incarnation);
    let checkpoint_created = match prepare_checkpoint_file(&checkpoint_path) {
        Ok(created) => created,
        Err(error) => {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
    };
    ensure_fingerprint_key(&mut locked.registry);
    clean_expired(&mut locked.registry, now);
    for claim in &mut locked.registry.claims {
        if claim.session_id == record.id
            && claim.session_incarnation != incarnation
            && claim.state == "active"
        {
            claim.state = "released".to_string();
            claim.revision = claim.revision.saturating_add(1);
            claim.updated_at = timestamp(now);
            claim.terminal_at_epoch = Some(now);
        }
    }
    for lease in &mut locked.registry.operations {
        if lease.session_id == record.id
            && lease.session_incarnation != incarnation
            && matches!(
                lease.state.as_str(),
                "active" | "completing" | "reconcile_pending"
            )
        {
            lease.state = "abandoned".to_string();
            lease.revision = lease.revision.saturating_add(1);
            lease.terminal_at_epoch = Some(now);
        }
    }
    locked.registry.brokers.insert(
        record.id.clone(),
        BrokerRecord {
            session_id: record.id.clone(),
            incarnation,
            coordination_mode: record.coordination_mode,
            capability_digest: digest_bytes(token.as_bytes()),
            generation,
            state: "starting".to_string(),
            heartbeat_at: String::new(),
            heartbeat_epoch: 0,
            runtime_identity: runtime.as_ref().map(|runtime| runtime.identity.clone()),
            runtime_identity_digest: runtime
                .as_ref()
                .map(|runtime| runtime.identity_digest.clone())
                .unwrap_or_default(),
            lost_since_epoch: None,
        },
    );
    if let Err(error) = locked.save() {
        let _ = fs::remove_file(&path);
        if checkpoint_created {
            let _ = fs::remove_file(&checkpoint_path);
        }
        return Err(error);
    }
    if let Some(previous) = previous_capability {
        let _ = fs::remove_file(previous);
    }
    if let Some(previous) = previous_checkpoint {
        let _ = fs::remove_file(previous);
    }
    Ok(path)
}

pub(crate) fn activate_ready(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    let incarnation = incarnation(record)?;
    let _runtime = crate::coordination_runtime_evidence(record)?;
    let started = Instant::now();
    while !heartbeat_fresh(context, &record.id, &incarnation, 0) {
        if started.elapsed() >= Duration::from_secs(2) {
            return Err(CliError::runtime(
                "coordination-broker-start-timeout",
                "the identity-bound coordination heartbeat did not become ready",
                None,
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
    let path = capability_path(context, &record.id, &incarnation);
    let token = read_private_capability(&path)?;
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    let broker = locked
        .registry
        .brokers
        .get_mut(&record.id)
        .filter(|broker| {
            broker.incarnation == incarnation
                && broker.state == "starting"
                && super::digest_eq(
                    &broker.capability_digest,
                    &digest_bytes(token.trim().as_bytes()),
                )
        })
        .ok_or_else(unavailable)?;
    broker.state = "ready".to_string();
    broker.heartbeat_at = timestamp(now);
    broker.heartbeat_epoch = now;
    locked.save()
}

pub(crate) fn ensure_ready(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    let incarnation = incarnation(record)?;
    let token = read_private_capability(&capability_path(context, &record.id, &incarnation))?;
    let locked = lock_registry(context)?;
    locked
        .registry
        .brokers
        .get(&record.id)
        .filter(|broker| {
            broker.incarnation == incarnation
                && broker.state == "ready"
                && super::digest_eq(
                    &broker.capability_digest,
                    &digest_bytes(token.trim().as_bytes()),
                )
                && heartbeat_fresh(context, &record.id, &incarnation, broker.heartbeat_epoch)
        })
        .ok_or_else(unavailable)?;
    Ok(())
}

pub(crate) fn revoke(context: &CliContext, record: &SessionRecord) -> Result<(), CliError> {
    let now = now_epoch();
    let current_incarnation = incarnation(record).ok();
    let mut locked = lock_registry(context)?;
    if let Some(broker) = locked.registry.brokers.get_mut(&record.id)
        && current_incarnation
            .as_deref()
            .is_some_and(|current| current == broker.incarnation)
    {
        broker.state = "stopped".to_string();
        broker.heartbeat_at = timestamp(now);
        broker.heartbeat_epoch = now;
        broker.capability_digest.clear();
    }
    for claim in &mut locked.registry.claims {
        if claim.session_id == record.id
            && current_incarnation
                .as_deref()
                .is_some_and(|current| current == claim.session_incarnation)
            && claim.state == "active"
        {
            claim.state = "released".to_string();
            claim.revision = claim.revision.saturating_add(1);
            claim.updated_at = timestamp(now);
            claim.terminal_at_epoch = Some(now);
        }
    }
    for operation in &mut locked.registry.operations {
        if operation.session_id == record.id
            && current_incarnation
                .as_deref()
                .is_some_and(|current| current == operation.session_incarnation)
            && matches!(
                operation.state.as_str(),
                "active" | "completing" | "reconcile_pending"
            )
        {
            operation.state = "abandoned".to_string();
            operation.revision = operation.revision.saturating_add(1);
            operation.terminal_at_epoch = Some(now);
        }
    }
    if let Some(current) = current_incarnation.as_deref() {
        remove_advisory_state_for_incarnation(&mut locked.registry, &record.id, current);
    }
    locked.save()?;
    if let Some(current) = current_incarnation.as_deref() {
        let _ = fs::remove_file(capability_path(context, &record.id, current));
        let _ = fs::remove_file(checkpoint_path_for_state(
            &context.state_dir,
            &record.id,
            current,
        ));
    }
    Ok(())
}

fn remove_advisory_state_for_incarnation(
    registry: &mut super::Registry,
    session_id: &str,
    session_incarnation: &str,
) {
    if registry
        .advisory_acknowledgements
        .get(session_id)
        .is_some_and(|acknowledgement| acknowledgement.session_incarnation == session_incarnation)
    {
        registry.advisory_acknowledgements.remove(session_id);
    }
    if registry
        .advisory_observations
        .get(session_id)
        .is_some_and(|observation| observation.session_incarnation == session_incarnation)
    {
        registry.advisory_observations.remove(session_id);
    }
}

pub(crate) fn stop(context: &CliContext, args: BrokerStopArgs) -> Result<Value, CliError> {
    let (record, _) =
        authenticate_from_file(context, &args.session, args.capability_file.as_deref())?;
    revoke(context, &record)?;
    Ok(json!({
        "schema_version": BROKER_VERSION,
        "session_id": record.id,
        "state": "stopped"
    }))
}

pub(crate) fn status(context: &CliContext, args: BrokerStatusArgs) -> Result<Value, CliError> {
    let locked = lock_registry(context)?;
    let broker = locked
        .registry
        .brokers
        .get(&args.session)
        .ok_or_else(unavailable)?;
    let incarnation = broker.incarnation.clone();
    let claim = locked
        .registry
        .claims
        .iter()
        .find(|claim| {
            claim.session_id == args.session
                && claim.session_incarnation == incarnation
                && claim.state == "active"
        })
        .map(|claim| ClaimSummary {
            claim_id: claim.claim_id.clone(),
            revision: claim.revision,
            state: claim.state.clone(),
            expires_at: claim.expires_at.clone(),
        });
    let operation = OperationSummary {
        active: locked
            .registry
            .operations
            .iter()
            .filter(|lease| {
                lease.session_id == args.session
                    && lease.session_incarnation == incarnation
                    && lease.state == "active"
            })
            .count(),
        uncertain: locked
            .registry
            .operations
            .iter()
            .filter(|lease| {
                lease.session_id == args.session
                    && lease.session_incarnation == incarnation
                    && matches!(lease.state.as_str(), "completing" | "reconcile_pending")
            })
            .count(),
    };
    json_value(BrokerStatus {
        schema_version: BROKER_VERSION.to_string(),
        session_id: args.session.clone(),
        state: broker.state.clone(),
        generation: broker.generation,
        capability_available: capability_available(
            context,
            &args.session,
            &incarnation,
            &broker.capability_digest,
        ),
        heartbeat_fresh: heartbeat_fresh(
            context,
            &args.session,
            &incarnation,
            broker.heartbeat_epoch,
        ),
        claim,
        operation,
    })
}

pub(crate) fn recover(
    context: &CliContext,
    args: BrokerRecoveryArgs,
    reconcile: bool,
) -> Result<Value, CliError> {
    let (authenticated_record, authenticated_incarnation) =
        authenticate_recovery_from_file(context, &args.session, args.capability_file.as_deref())?;
    let proof: RecoveryProof =
        read_bounded_json(&args.proof_file, 8 * 1024, "invalid-recovery-proof")?;
    if proof.schema_version != "agent-session.coordination-recovery-proof.v1" {
        return Err(CliError::data(
            "invalid-recovery-proof",
            "recovery proof schema is unsupported",
            None,
        ));
    }
    let _session_lock = crate::acquire_session_record_lock(context, &args.session)?;
    let record = crate::load_session_record(context, &args.session)?;
    crate::ensure_same_session_identity(&authenticated_record, &record)?;
    let record_incarnation = incarnation(&record)?;
    if record_incarnation != authenticated_incarnation {
        return Err(CliError::data(
            "session-incarnation-conflict",
            "the authenticated recovery capability no longer matches the current runtime",
            None,
        ));
    }
    let record_generation = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.generation)
        .unwrap_or_default();
    if proof.session_incarnation != record_incarnation || proof.generation != record_generation {
        return Err(CliError::data(
            "session-incarnation-conflict",
            "recovery proof does not match the current runtime",
            None,
        ));
    }
    let operation = if reconcile {
        "broker-reconcile"
    } else {
        "broker-adopt"
    };
    let reconcile_selector = if reconcile {
        if !args.attest_inactive {
            return Err(CliError::usage(
                "invalid-recovery-proof",
                "broker reconcile requires --attest-inactive",
                None,
            ));
        }
        Some((
            args.operation.as_deref().ok_or_else(|| {
                CliError::usage(
                    "invalid-recovery-proof",
                    "broker reconcile requires --operation",
                    None,
                )
            })?,
            args.if_revision.ok_or_else(|| {
                CliError::usage(
                    "invalid-recovery-proof",
                    "broker reconcile requires --if-revision",
                    None,
                )
            })?,
        ))
    } else {
        if args.operation.is_some() || args.if_revision.is_some() || args.attest_inactive {
            return Err(CliError::usage(
                "invalid-recovery-proof",
                "broker adopt does not accept operation reconciliation selectors",
                None,
            ));
        }
        None
    };
    let digest = request_digest(
        operation,
        &json!({
            "proof": proof,
            "operation": args.operation,
            "if_revision": args.if_revision,
            "attest_inactive": args.attest_inactive,
        }),
    );
    {
        let locked = lock_registry(context)?;
        if let Some(replay) = idempotency_replay(
            &locked.registry,
            &args.idempotency_key,
            &record.id,
            &record_incarnation,
            operation,
            &digest,
        )? {
            return Ok(replay);
        }
        if locked
            .registry
            .brokers
            .get(&record.id)
            .is_some_and(|broker| {
                broker.incarnation == record_incarnation
                    && broker.state == "ready"
                    && capability_available(
                        context,
                        &record.id,
                        &record_incarnation,
                        &broker.capability_digest,
                    )
                    && heartbeat_fresh(
                        context,
                        &record.id,
                        &record_incarnation,
                        broker.heartbeat_epoch,
                    )
            })
        {
            return Err(CliError::data(
                "coordination-broker-not-lost",
                "the exact coordination broker is still healthy",
                None,
            ));
        }
    }
    let runtime = crate::coordination_runtime_evidence(&record)?;
    if runtime.status != crate::CoordinationRuntimeStatus::Running {
        return Err(CliError::runtime(
            "coordination-runtime-unverified",
            "broker recovery requires the exact persisted runtime to be running",
            None,
        ));
    }
    let mut locked = lock_registry(context)?;
    if let Some(replay) = idempotency_replay(
        &locked.registry,
        &args.idempotency_key,
        &record.id,
        &record_incarnation,
        operation,
        &digest,
    )? {
        return Ok(replay);
    }
    let broker_snapshot = locked
        .registry
        .brokers
        .get(&record.id)
        .filter(|broker| {
            broker.incarnation == record_incarnation && broker.generation == record_generation
        })
        .cloned()
        .ok_or_else(unavailable)?;
    if broker_snapshot.state == "ready"
        && capability_available(
            context,
            &record.id,
            &record_incarnation,
            &broker_snapshot.capability_digest,
        )
        && heartbeat_fresh(
            context,
            &record.id,
            &record_incarnation,
            broker_snapshot.heartbeat_epoch,
        )
    {
        return Err(CliError::data(
            "coordination-broker-not-lost",
            "the exact coordination broker is still healthy",
            None,
        ));
    }
    if broker_snapshot.runtime_identity_digest != runtime.identity_digest
        || !capability_available(
            context,
            &record.id,
            &record_incarnation,
            &broker_snapshot.capability_digest,
        )
    {
        return Err(CliError::runtime(
            "coordination-runtime-unverified",
            "recovery evidence does not match the persisted broker identity and capability",
            None,
        ));
    }
    let sidecar_already_running = broker_snapshot.state == "recovering"
        && heartbeat_fresh(context, &record.id, &record_incarnation, 0);
    if broker_snapshot.state != "recovering" {
        let broker = locked
            .registry
            .brokers
            .get_mut(&record.id)
            .filter(|broker| {
                broker.incarnation == record_incarnation
                    && broker.generation == record_generation
                    && broker.runtime_identity_digest == runtime.identity_digest
            })
            .ok_or_else(unavailable)?;
        broker.state = "recovering".to_string();
        broker.lost_since_epoch.get_or_insert(now_epoch());
        locked.save()?;
    }
    drop(locked);
    let heartbeat = super::heartbeat_path(&context.state_dir, &record.id);
    let heartbeat_before = fs::metadata(&heartbeat)
        .and_then(|metadata| metadata.modified())
        .ok();
    if !sidecar_already_running {
        match fs::remove_file(&heartbeat) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(unavailable()),
        }
    }
    spawn_heartbeat_sidecar(
        context,
        &record.id,
        &record_incarnation,
        record_generation,
        &capability_path(context, &record.id, &record_incarnation),
    )?;
    let started = Instant::now();
    while !heartbeat_fresh(context, &record.id, &record_incarnation, 0)
        || !heartbeat_advanced_since(&heartbeat, heartbeat_before)
    {
        if started.elapsed() >= Duration::from_secs(4) {
            return Err(CliError::runtime(
                "coordination-broker-start-timeout",
                "recovered broker sidecar did not become ready",
                None,
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
    let runtime = crate::coordination_runtime_evidence(&record)?;
    if runtime.status != crate::CoordinationRuntimeStatus::Running
        || runtime.identity_digest != broker_snapshot.runtime_identity_digest
    {
        return Err(CliError::runtime(
            "coordination-runtime-unverified",
            "the recovered runtime changed before broker commit",
            None,
        ));
    }
    let _activity_fence = if reconcile {
        Some(crate::activity::acquire_coordination_activity_lock(
            context, &record.id,
        )?)
    } else {
        None
    };
    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    if let Some(replay) = idempotency_replay(
        &locked.registry,
        &args.idempotency_key,
        &record.id,
        &record_incarnation,
        operation,
        &digest,
    )? {
        return Ok(replay);
    }
    let broker = locked
        .registry
        .brokers
        .get_mut(&record.id)
        .filter(|broker| {
            broker.incarnation == record_incarnation
                && broker.generation == record_generation
                && broker.state == "recovering"
                && broker.runtime_identity_digest == runtime.identity_digest
        })
        .ok_or_else(unavailable)?;
    broker.state = "ready".to_string();
    broker.heartbeat_at = timestamp(now);
    broker.heartbeat_epoch = now;
    broker.lost_since_epoch = None;
    let operation_reconciliation = reconcile_selector
        .map(|(lease_id, revision)| {
            super::claims::operator_reconcile_in_registry(
                context,
                &mut locked.registry,
                &record,
                lease_id,
                revision,
                now,
            )
        })
        .transpose()?;
    let result = json!({
        "schema_version": BROKER_VERSION,
        "session_id": record.id,
        "state": "ready",
        "generation": record_generation,
        "recovery": if reconcile { "reconciled" } else { "adopted" },
        "operation_reconciliation": operation_reconciliation,
    });
    store_receipt(
        &mut locked.registry,
        args.idempotency_key,
        record.id,
        record_incarnation,
        operation.to_string(),
        digest,
        result.clone(),
        now,
    )?;
    locked.save()?;
    Ok(result)
}

fn spawn_heartbeat_sidecar(
    context: &CliContext,
    session_id: &str,
    incarnation: &str,
    generation: u64,
    capability_file: &std::path::Path,
) -> Result<(), CliError> {
    let executable = crate::resolve_agent_session_executable().map_err(|_| unavailable())?;
    let mut command = Command::new(executable);
    command
        .arg("--state-dir")
        .arg(&context.state_dir)
        .arg("broker")
        .arg("heartbeat")
        .arg("--session")
        .arg(session_id)
        .arg("--incarnation")
        .arg(incarnation)
        .arg("--generation")
        .arg(generation.to_string())
        .arg("--capability-file")
        .arg(capability_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the child performs only async-signal-safe `setsid` before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command.spawn().map_err(|_| unavailable())?;
    Ok(())
}

pub(crate) fn run_heartbeat_sidecar(
    context: &CliContext,
    args: BrokerHeartbeatArgs,
) -> Result<Value, CliError> {
    if !heartbeat_owner_authorized(context, &args) {
        return Err(super::unauthorized());
    }
    let directory = super::coordination_dir(context, &args.session);
    let lock_path = directory.join(format!(
        "broker-{}.lock",
        digest_bytes(args.incarnation.as_bytes())
    ));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(SECRET_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(lock_path)
        .map_err(|_| unavailable())?;
    use std::os::fd::AsRawFd;
    // SAFETY: `lock` owns a valid descriptor for the lifetime of the loop.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(CliError::data(
            "coordination-broker-not-lost",
            "an exact broker heartbeat owner already exists",
            None,
        ));
    }
    let started = Instant::now();
    let mut established_owner = false;
    let mut observed_stopped = false;
    loop {
        let record = match crate::load_session_record(context, &args.session) {
            Ok(record) => record,
            Err(_) if started.elapsed() < Duration::from_secs(2) => {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(_) => break,
        };
        let matches = incarnation(&record).is_ok_and(|value| value == args.incarnation)
            && record
                .runtime
                .as_ref()
                .is_some_and(|runtime| runtime.generation == args.generation);
        if !matches {
            break;
        }
        if !heartbeat_owner_authorized(context, &args) {
            break;
        }
        established_owner = true;
        match crate::coordination_runtime_evidence(&record) {
            Ok(runtime) if runtime.status == crate::CoordinationRuntimeStatus::Running => {}
            Ok(runtime)
                if runtime.status == crate::CoordinationRuntimeStatus::Stopped
                    && started.elapsed() >= Duration::from_secs(5) =>
            {
                observed_stopped = true;
                break;
            }
            Ok(_) | Err(_) if started.elapsed() < Duration::from_secs(5) => {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            Ok(_) | Err(_) => {
                mark_degraded(context, &args);
                break;
            }
        }
        let _ = super::claims::drain_completion_events(context);
        let now = now_epoch();
        write_atomic(
            &super::heartbeat_path(&context.state_dir, &args.session),
            format!("{}:{}\n", args.incarnation, now).as_bytes(),
            SECRET_FILE_MODE,
        )
        .map_err(|_| unavailable())?;
        thread::sleep(Duration::from_secs(2));
    }
    if established_owner
        && observed_stopped
        && let Ok(record) = crate::load_session_record(context, &args.session)
        && incarnation(&record).is_ok_and(|value| value == args.incarnation)
        && record
            .runtime
            .as_ref()
            .is_some_and(|runtime| runtime.generation == args.generation)
    {
        let _ = revoke(context, &record);
    }
    Ok(json!({
        "schema_version": BROKER_VERSION,
        "session_id": args.session,
        "state": "stopped"
    }))
}

fn heartbeat_advanced_since(
    heartbeat: &std::path::Path,
    previous: Option<std::time::SystemTime>,
) -> bool {
    let Ok(current) = fs::metadata(heartbeat).and_then(|metadata| metadata.modified()) else {
        return false;
    };
    previous.is_none_or(|previous| current > previous)
}

fn heartbeat_owner_authorized(context: &CliContext, args: &BrokerHeartbeatArgs) -> bool {
    let Ok(token) = read_private_capability(&args.capability_file) else {
        return false;
    };
    let Ok(locked) = lock_registry(context) else {
        return false;
    };
    locked
        .registry
        .brokers
        .get(&args.session)
        .is_some_and(|broker| {
            broker.incarnation == args.incarnation
                && broker.generation == args.generation
                && matches!(broker.state.as_str(), "starting" | "recovering" | "ready")
                && super::digest_eq(
                    &broker.capability_digest,
                    &digest_bytes(token.trim().as_bytes()),
                )
        })
}

fn mark_degraded(context: &CliContext, args: &BrokerHeartbeatArgs) {
    let Ok(mut locked) = lock_registry(context) else {
        return;
    };
    let Some(broker) = locked.registry.brokers.get_mut(&args.session) else {
        return;
    };
    if broker.incarnation != args.incarnation || broker.generation != args.generation {
        return;
    }
    broker.state = "degraded".to_string();
    broker.lost_since_epoch.get_or_insert(now_epoch());
    let _ = locked.save();
}

fn unavailable() -> CliError {
    CliError::runtime(
        "coordination-broker-lost",
        "coordination broker is unavailable",
        None,
    )
}

fn read_private_capability(path: &std::path::Path) -> Result<String, CliError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| unavailable())?;
    let metadata = file.metadata().map_err(|_| unavailable())?;
    if !metadata.is_file()
        || metadata.len() > 512
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
    {
        return Err(CliError::runtime(
            "coordination-store-untrusted",
            "coordination capability file is untrusted",
            None,
        ));
    }
    let mut token = String::new();
    file.by_ref()
        .take(513)
        .read_to_string(&mut token)
        .map_err(|_| unavailable())?;
    if token.len() > 512 {
        return Err(unavailable());
    }
    Ok(token)
}

pub(crate) fn capability_available(
    context: &CliContext,
    session_id: &str,
    incarnation: &str,
    expected_digest: &str,
) -> bool {
    if expected_digest.is_empty() {
        return false;
    }
    read_private_capability(&capability_path(context, session_id, incarnation)).is_ok_and(|token| {
        super::digest_eq(expected_digest, &digest_bytes(token.trim().as_bytes()))
    })
}

pub(crate) fn heartbeat_fresh(
    context: &CliContext,
    session_id: &str,
    incarnation: &str,
    _registry_heartbeat_epoch: i64,
) -> bool {
    nils_common::coordination_projection::heartbeat_fresh(
        &context.state_dir,
        session_id,
        incarnation,
        now_epoch(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_json::json;
    use std::os::unix::fs::symlink;

    #[test]
    fn checkpoint_file_is_private_and_reuses_only_a_trusted_inode() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("checkpoint.json");

        assert!(prepare_checkpoint_file(&path).expect("create checkpoint"));
        let metadata = fs::symlink_metadata(&path).expect("checkpoint metadata");
        assert!(metadata.is_file());
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.permissions().mode() & 0o777, SECRET_FILE_MODE);

        fs::write(&path, b"{\"state\":\"working\"}\n").expect("seed checkpoint");
        assert!(!prepare_checkpoint_file(&path).expect("reuse trusted checkpoint"));
        assert_eq!(
            fs::read(&path).expect("read checkpoint"),
            b"{\"state\":\"working\"}\n"
        );
    }

    #[test]
    fn checkpoint_file_rejects_public_symlink_and_hardlink_targets() {
        let tmp = tempfile::TempDir::new().expect("tempdir");

        let public = tmp.path().join("public.json");
        fs::write(&public, b"public").expect("seed public");
        fs::set_permissions(&public, fs::Permissions::from_mode(0o644)).expect("set public mode");
        let error = prepare_checkpoint_file(&public).expect_err("public file rejected");
        assert_eq!(error.code(), "coordination-store-untrusted");
        assert_eq!(fs::read(&public).expect("public unchanged"), b"public");

        let sentinel = tmp.path().join("sentinel");
        fs::write(&sentinel, b"sentinel").expect("seed sentinel");
        fs::set_permissions(&sentinel, fs::Permissions::from_mode(SECRET_FILE_MODE))
            .expect("set private mode");
        let linked = tmp.path().join("linked.json");
        fs::hard_link(&sentinel, &linked).expect("hard link");
        let error = prepare_checkpoint_file(&linked).expect_err("hard link rejected");
        assert_eq!(error.code(), "coordination-store-untrusted");
        assert_eq!(
            fs::read(&sentinel).expect("sentinel unchanged"),
            b"sentinel"
        );

        let symbolic = tmp.path().join("symbolic.json");
        symlink(&sentinel, &symbolic).expect("symlink");
        let error = prepare_checkpoint_file(&symbolic).expect_err("symlink rejected");
        assert_eq!(error.code(), "coordination-store-untrusted");
        assert_eq!(
            fs::read(&sentinel).expect("sentinel unchanged"),
            b"sentinel"
        );
    }

    #[test]
    fn broker_projection_schema_is_stable() {
        let value = serde_json::to_value(BrokerStatus {
            schema_version: BROKER_VERSION.to_string(),
            session_id: "session".to_string(),
            state: "ready".to_string(),
            generation: 1,
            capability_available: true,
            heartbeat_fresh: true,
            claim: None,
            operation: OperationSummary::default(),
        })
        .expect("serialize");
        assert!(value.get("capability_path").is_none());
        assert!(value.get("capability_digest").is_none());
    }

    #[test]
    fn coordination_review_recent_registry_timestamp_is_not_readiness() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let context = CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        };
        assert!(!heartbeat_fresh(
            &context,
            "session",
            "incarnation",
            now_epoch()
        ));
    }

    #[test]
    fn coordination_review_recovery_proof_rejects_raw_operator_tokens() {
        let value = json!({
            "schema_version": "agent-session.coordination-recovery-proof.v1",
            "session_incarnation": "incarnation",
            "generation": 1,
            "operator_token": "raw-secret"
        });
        assert!(serde_json::from_value::<RecoveryProof>(value).is_err());
    }

    #[test]
    fn coordination_review_heartbeat_requires_private_launch_authority() {
        let missing = crate::cli::Cli::try_parse_from([
            "agent-session",
            "broker",
            "heartbeat",
            "--session",
            "session",
            "--incarnation",
            "incarnation",
            "--generation",
            "1",
        ]);
        assert!(missing.is_err());
        assert!(
            crate::cli::Cli::try_parse_from([
                "agent-session",
                "broker",
                "heartbeat",
                "--session",
                "session",
                "--incarnation",
                "incarnation",
                "--generation",
                "1",
                "--capability-file",
                "/private/capability",
            ])
            .is_ok()
        );
    }

    #[test]
    fn stale_revoke_preserves_replacement_advisory_state() {
        let mut registry = super::super::Registry::default();
        registry.advisory_acknowledgements.insert(
            "session".to_string(),
            crate::coordination::advisory::AdvisoryAcknowledgement {
                session_incarnation: "replacement".to_string(),
                advisory_digest: "digest".to_string(),
                expires_at: "2030-01-01T00:00:00Z".to_string(),
                expires_at_epoch: i64::MAX,
            },
        );
        registry.advisory_observations.insert(
            "session".to_string(),
            crate::coordination::advisory::AdvisoryObservation {
                session_incarnation: "replacement".to_string(),
                advisory_digest: "digest".to_string(),
                observed_at_epoch: i64::MAX,
            },
        );

        remove_advisory_state_for_incarnation(&mut registry, "session", "stale");
        assert!(registry.advisory_acknowledgements.contains_key("session"));
        assert!(registry.advisory_observations.contains_key("session"));

        remove_advisory_state_for_incarnation(&mut registry, "session", "replacement");
        assert!(!registry.advisory_acknowledgements.contains_key("session"));
        assert!(!registry.advisory_observations.contains_key("session"));
    }
}

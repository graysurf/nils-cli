use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::{BrokerRecoveryArgs, BrokerStatusArgs, BrokerStopArgs};
use crate::{CliContext, CliError, SessionRecord};

use super::{
    BrokerRecord, authenticate_from_file, capability_path, clean_expired, coordination_dir,
    digest_bytes, ensure_fingerprint_key, idempotency_replay, incarnation, json_value,
    lock_registry, now_epoch, read_bounded_json, request_digest, store_receipt, timestamp,
};

pub(crate) const BROKER_VERSION: &str = "agent-session.coordination-broker.v1";

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
    prepare(context, record)?;
    let incarnation = incarnation(record)?;
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
        let previous_live = capability_available(
            context,
            &record.id,
            &previous.incarnation,
            &previous.capability_digest,
        ) || heartbeat_fresh(
            context,
            &record.id,
            &previous.incarnation,
            previous.heartbeat_epoch,
        );
        if previous_live {
            return Err(CliError::data(
                "session-incarnation-conflict",
                "the prior coordination incarnation is still live",
                None,
            ));
        }
    } else if locked.registry.operations.iter().any(|lease| {
        lease.session_id == record.id
            && lease.session_incarnation != incarnation
            && matches!(lease.state.as_str(), "active" | "completing")
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
    write_atomic(&path, token.as_bytes(), SECRET_FILE_MODE).map_err(|_| unavailable())?;
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
            && matches!(lease.state.as_str(), "active" | "completing")
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
            capability_digest: digest_bytes(token.as_bytes()),
            generation,
            state: "starting".to_string(),
            heartbeat_at: String::new(),
            heartbeat_epoch: 0,
        },
    );
    if let Err(error) = locked.save() {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    if let Some(previous) = previous_capability {
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
        if claim.session_id == record.id && claim.state == "active" {
            claim.state = "released".to_string();
            claim.revision = claim.revision.saturating_add(1);
            claim.updated_at = timestamp(now);
            claim.terminal_at_epoch = Some(now);
        }
    }
    for operation in &mut locked.registry.operations {
        if operation.session_id == record.id
            && matches!(operation.state.as_str(), "active" | "completing")
        {
            operation.state = "abandoned".to_string();
            operation.revision = operation.revision.saturating_add(1);
            operation.terminal_at_epoch = Some(now);
        }
    }
    locked.save()?;
    if let Some(current) = current_incarnation.as_deref() {
        let _ = fs::remove_file(capability_path(context, &record.id, current));
    }
    Ok(())
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
    let (record, incarnation) =
        authenticate_from_file(context, &args.session, args.capability_file.as_deref())?;
    let locked = lock_registry(context)?;
    let broker = locked
        .registry
        .brokers
        .get(&record.id)
        .filter(|broker| broker.incarnation == incarnation)
        .ok_or_else(unavailable)?;
    let claim = locked
        .registry
        .claims
        .iter()
        .find(|claim| {
            claim.session_id == record.id
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
                lease.session_id == record.id
                    && lease.session_incarnation == incarnation
                    && lease.state == "active"
            })
            .count(),
        uncertain: locked
            .registry
            .operations
            .iter()
            .filter(|lease| {
                lease.session_id == record.id
                    && lease.session_incarnation == incarnation
                    && lease.state == "completing"
            })
            .count(),
    };
    json_value(BrokerStatus {
        schema_version: BROKER_VERSION.to_string(),
        session_id: record.id.clone(),
        state: broker.state.clone(),
        generation: broker.generation,
        capability_available: capability_available(
            context,
            &record.id,
            &incarnation,
            &broker.capability_digest,
        ),
        heartbeat_fresh: heartbeat_fresh(context, &record.id, &incarnation, broker.heartbeat_epoch),
        claim,
        operation,
    })
}

pub(crate) fn recover(
    context: &CliContext,
    args: BrokerRecoveryArgs,
    reconcile: bool,
) -> Result<Value, CliError> {
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
    let record_incarnation = incarnation(&record)?;
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
    let digest = request_digest(operation, &proof);
    let now = now_epoch();
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
    let _path = provision(context, &record)?;
    activate_ready(context, &record)?;
    let result = json!({
        "schema_version": BROKER_VERSION,
        "session_id": record.id,
        "state": "ready",
        "generation": record_generation,
        "recovery": if reconcile { "reconciled" } else { "adopted" }
    });
    let mut locked = lock_registry(context)?;
    store_receipt(
        &mut locked.registry,
        args.idempotency_key,
        record.id,
        record_incarnation,
        operation.to_string(),
        digest,
        result.clone(),
        now,
    );
    locked.save()?;
    Ok(result)
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
    let now = now_epoch();
    let path = super::heartbeat_path(&context.state_dir, session_id);
    let Ok(mut file) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.is_file()
        || metadata.len() > 256
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
    {
        return false;
    }
    let mut value = String::new();
    if file.by_ref().take(257).read_to_string(&mut value).is_err() || value.len() > 256 {
        return false;
    }
    let Some((observed_incarnation, observed_epoch)) = value.trim().rsplit_once(':') else {
        return false;
    };
    if observed_incarnation != incarnation {
        return false;
    }
    let Ok(observed_epoch) = observed_epoch.parse::<i64>() else {
        return false;
    };
    (0..=30).contains(&now.saturating_sub(observed_epoch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}

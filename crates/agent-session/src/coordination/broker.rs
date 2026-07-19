use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;

use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::{BrokerRecoveryArgs, BrokerStatusArgs};
use crate::{CliContext, CliError, SessionRecord};

use super::{
    BrokerRecord, authenticate_from_file, capability_path, clean_expired, digest_bytes,
    ensure_fingerprint_key, idempotency_replay, incarnation, json_value, lock_registry, now_epoch,
    read_bounded_json, request_digest, store_receipt, timestamp, unauthorized,
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
    operator_token: String,
}

pub(crate) fn provision(context: &CliContext, record: &SessionRecord) -> Result<PathBuf, CliError> {
    let incarnation = incarnation(record)?;
    let generation = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.generation)
        .unwrap_or_default();
    let capability_dir = capability_path(context, &record.id)
        .parent()
        .expect("capability has parent")
        .to_path_buf();
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
        .map_err(|_| unavailable())?;
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let path = capability_path(context, &record.id);
    write_atomic(&path, token.as_bytes(), SECRET_FILE_MODE).map_err(|_| unavailable())?;

    let now = now_epoch();
    let mut locked = lock_registry(context)?;
    ensure_fingerprint_key(&mut locked.registry);
    clean_expired(&mut locked.registry, now);
    for claim in &mut locked.registry.claims {
        if claim.session_id == record.id
            && claim.session_incarnation != incarnation
            && claim.state == "active"
        {
            claim.state = "stale".to_string();
            claim.revision = claim.revision.saturating_add(1);
        }
    }
    for lease in &mut locked.registry.operations {
        if lease.session_id == record.id
            && lease.session_incarnation != incarnation
            && lease.state == "active"
        {
            lease.state = "abandoned".to_string();
            lease.revision = lease.revision.saturating_add(1);
        }
    }
    locked.registry.brokers.insert(
        record.id.clone(),
        BrokerRecord {
            session_id: record.id.clone(),
            incarnation,
            capability_digest: digest_bytes(token.as_bytes()),
            generation,
            state: "ready".to_string(),
            heartbeat_at: timestamp(now),
            heartbeat_epoch: now,
        },
    );
    if let Err(error) = locked.save() {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
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
        }
    }
    for operation in &mut locked.registry.operations {
        if operation.session_id == record.id && operation.state == "active" {
            operation.state = "abandoned".to_string();
            operation.revision = operation.revision.saturating_add(1);
        }
    }
    locked.save()?;
    let _ = fs::remove_file(capability_path(context, &record.id));
    Ok(())
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
            .filter(|lease| lease.session_id == record.id && lease.state == "active")
            .count(),
        uncertain: locked
            .registry
            .operations
            .iter()
            .filter(|lease| lease.session_id == record.id && lease.state == "completing")
            .count(),
    };
    json_value(BrokerStatus {
        schema_version: BROKER_VERSION.to_string(),
        session_id: record.id.clone(),
        state: broker.state.clone(),
        generation: broker.generation,
        capability_available: !broker.capability_digest.is_empty(),
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
    let expected_operator = std::env::var("AGENT_SESSION_TOKEN").map_err(|_| unauthorized())?;
    if digest_bytes(expected_operator.as_bytes()) != digest_bytes(proof.operator_token.as_bytes()) {
        return Err(unauthorized());
    }
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
    }
    let _path = provision(context, &record)?;
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

pub(crate) fn heartbeat_fresh(
    context: &CliContext,
    session_id: &str,
    incarnation: &str,
    registry_heartbeat_epoch: i64,
) -> bool {
    let now = now_epoch();
    let registry_age = now.saturating_sub(registry_heartbeat_epoch);
    if (0..=60).contains(&registry_age) {
        return true;
    }
    let path = super::heartbeat_path(&context.state_dir, session_id);
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return false;
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > 256
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return false;
    }
    let Ok(value) = fs::read_to_string(path) else {
        return false;
    };
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
}

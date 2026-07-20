use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jiff::Timestamp;
use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::{Deserialize, Serialize};

use crate::contract::{digest, digest_serializable, valid_digest};
use crate::error::HookError;
use crate::model::{NormalizedRequest, PolicyRule, Product, RecoveryScope};

const CHALLENGE_VERSION: &str = "agent-hook.recovery-challenge.v1";
const CAPABILITY_VERSION: &str = "agent-hook.recovery-capability.v1";
const RECORD_VERSION: &str = "agent-hook.recovery-record.v1";
const MAX_RECOVERY_BYTES: u64 = 64 * 1024;
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    pub schema_version: String,
    pub challenge_id: String,
    pub product: Product,
    pub event: String,
    pub target_digest: String,
    pub command_digest: String,
    pub snapshot_digest: String,
    pub rules: Vec<String>,
    pub manifest: RecoveryManifest,
    pub scope: RecoveryScope,
    pub issued_at: String,
    pub issued_at_epoch: i64,
    pub expires_at: String,
    pub expires_at_epoch: i64,
    pub state_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityFile {
    pub schema_version: String,
    pub capability_id: String,
    pub challenge_digest: String,
    pub product: Product,
    pub event: String,
    pub target_digest: String,
    pub command_digest: String,
    pub snapshot_digest: String,
    pub rules: Vec<String>,
    pub manifest: RecoveryManifest,
    pub scope: RecoveryScope,
    pub expires_at: String,
    pub expires_at_epoch: i64,
    pub state_revision: u64,
    pub principal_digest: String,
    pub nonce: String,
    pub key_digest: String,
    pub authorization_proof: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecoveryRecord {
    schema_version: String,
    capability_id: String,
    challenge_id: String,
    capability_digest: String,
    challenge_digest: String,
    scope: RecoveryScope,
    rules: Vec<String>,
    manifest_digest: String,
    expires_at_epoch: i64,
    state_revision: u64,
    principal_digest: String,
    key_digest: String,
    status: String,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChallengeResult {
    pub schema_version: String,
    pub challenge_id: String,
    pub challenge_digest: String,
    pub scope: String,
    pub expires_at: String,
    pub rule_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationResult {
    pub schema_version: String,
    pub capability_id: String,
    pub capability_digest: String,
    pub scope: String,
    pub expires_at: String,
    pub rule_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumeResult {
    pub schema_version: String,
    pub capability_id: String,
    pub scope: String,
    pub status: String,
    pub rule_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusResult {
    pub schema_version: String,
    pub capability_id: String,
    pub scope: String,
    pub status: String,
    pub expires_at_epoch: i64,
    pub rule_count: usize,
}

#[derive(Clone, Debug, Default)]
pub struct RecoveryGrant {
    pub capability_id: Option<String>,
    pub rules: BTreeSet<String>,
    pub manifest: Option<RecoveryManifest>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryManifest {
    pub schema_version: String,
    pub product: Product,
    pub event: String,
    pub config_digest: String,
    pub policy_digest: String,
    pub rules: Vec<PolicyRule>,
}

pub struct ChallengeInput<'a> {
    pub product: Product,
    pub event: &'a str,
    pub target_digest: &'a str,
    pub command_digest: &'a str,
    pub snapshot_digest: &'a str,
    pub rules: &'a [String],
    pub manifest: RecoveryManifest,
    pub scope: RecoveryScope,
    pub ttl_seconds: u64,
    pub out: &'a Path,
}

pub fn create_challenge(
    state_root: &Path,
    input: ChallengeInput<'_>,
) -> Result<ChallengeResult, HookError> {
    validate_binding(
        input.event,
        input.target_digest,
        input.command_digest,
        input.snapshot_digest,
        input.rules,
    )?;
    let max_ttl = match input.scope {
        RecoveryScope::OneShot => 300,
        RecoveryScope::RepairWindow => 900,
    };
    if input.ttl_seconds == 0 || input.ttl_seconds > max_ttl {
        return Err(HookError::usage(
            "recovery-ttl-invalid",
            format!("recovery TTL must be 1..={max_ttl} seconds"),
        ));
    }
    let mut locked = lock(state_root)?;
    let revision = next_revision(state_root)?;
    let now = now_epoch();
    let expires = now.saturating_add(input.ttl_seconds as i64);
    validate_manifest(&input.manifest, input.product, input.event, input.rules)?;
    let challenge = Challenge {
        schema_version: CHALLENGE_VERSION.to_string(),
        challenge_id: uuid::Uuid::new_v4().to_string(),
        product: input.product,
        event: input.event.to_string(),
        target_digest: input.target_digest.to_string(),
        command_digest: input.command_digest.to_string(),
        snapshot_digest: input.snapshot_digest.to_string(),
        rules: canonical_rules(input.rules)?,
        manifest: input.manifest,
        scope: input.scope,
        issued_at: timestamp(now),
        issued_at_epoch: now,
        expires_at: timestamp(expires),
        expires_at_epoch: expires,
        state_revision: revision,
    };
    let bytes = serde_json::to_vec_pretty(&challenge)
        .map_err(|_| HookError::runtime("recovery-render-failed", "challenge render failed"))?;
    write_new_private(input.out, &bytes)?;
    let challenge_digest = digest(&bytes);
    let state_path = challenge_path(state_root, &challenge.challenge_id);
    write_atomic(&state_path, &bytes, SECRET_FILE_MODE).map_err(|_| {
        HookError::runtime(
            "recovery-state-write-failed",
            "challenge state write failed",
        )
    })?;
    locked.save_revision(revision)?;
    Ok(ChallengeResult {
        schema_version: "agent-hook.recovery-challenge-result.v1".to_string(),
        challenge_id: challenge.challenge_id,
        challenge_digest,
        scope: input.scope.as_str().to_string(),
        expires_at: challenge.expires_at,
        rule_count: challenge.rules.len(),
    })
}

pub fn authorize(
    state_root: &Path,
    challenge_file: &Path,
    expected_digest: &str,
    out: &Path,
) -> Result<AuthorizationResult, HookError> {
    if !valid_digest(expected_digest) {
        return Err(HookError::usage(
            "challenge-digest-invalid",
            "expected challenge digest must be lowercase sha256",
        ));
    }
    let bytes = read_private(challenge_file)?;
    if digest(&bytes) != expected_digest {
        return Err(HookError::data(
            "challenge-digest-mismatch",
            "reviewed challenge digest does not match the challenge file",
        ));
    }
    let challenge: Challenge = serde_json::from_slice(&bytes)
        .map_err(|_| HookError::data("challenge-invalid", "challenge file is invalid"))?;
    validate_challenge(&challenge)?;
    let mut locked = lock(state_root)?;
    let canonical = read_private(&challenge_path(state_root, &challenge.challenge_id))?;
    if digest(&canonical) != expected_digest {
        return Err(HookError::data(
            "challenge-state-drift",
            "challenge state no longer matches the reviewed file",
        ));
    }
    let now = now_epoch();
    if challenge.expires_at_epoch <= now {
        return Err(HookError::data(
            "challenge-expired",
            "recovery challenge has expired",
        ));
    }
    let revision = next_revision(state_root)?;
    if revision != challenge.state_revision.saturating_add(1) {
        return Err(HookError::data(
            "challenge-state-revision-drift",
            "recovery state changed after challenge creation",
        ));
    }
    let principal_digest = principal_digest(challenge.scope)?;
    let key = load_or_create_key(state_root)?;
    let key_digest = digest(&key);
    let mut capability = CapabilityFile {
        schema_version: CAPABILITY_VERSION.to_string(),
        capability_id: uuid::Uuid::new_v4().to_string(),
        challenge_digest: expected_digest.to_string(),
        product: challenge.product,
        event: challenge.event.clone(),
        target_digest: challenge.target_digest.clone(),
        command_digest: challenge.command_digest.clone(),
        snapshot_digest: challenge.snapshot_digest.clone(),
        rules: challenge.rules.clone(),
        manifest: challenge.manifest.clone(),
        scope: challenge.scope,
        expires_at: challenge.expires_at.clone(),
        expires_at_epoch: challenge.expires_at_epoch,
        state_revision: revision,
        principal_digest: principal_digest.clone(),
        nonce: format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ),
        key_digest: key_digest.clone(),
        authorization_proof: String::new(),
    };
    capability.authorization_proof = sign_capability(&capability, &key)?;
    let capability_bytes = serde_json::to_vec_pretty(&capability)
        .map_err(|_| HookError::runtime("recovery-render-failed", "capability render failed"))?;
    write_new_private(out, &capability_bytes)?;
    let capability_digest = digest(&capability_bytes);
    let record = RecoveryRecord {
        schema_version: RECORD_VERSION.to_string(),
        capability_id: capability.capability_id.clone(),
        challenge_id: challenge.challenge_id,
        capability_digest: capability_digest.clone(),
        challenge_digest: expected_digest.to_string(),
        scope: capability.scope,
        rules: capability.rules.clone(),
        manifest_digest: digest_serializable(&capability.manifest)?,
        expires_at_epoch: capability.expires_at_epoch,
        state_revision: revision,
        principal_digest,
        key_digest,
        status: "authorized".to_string(),
        updated_at: timestamp(now),
    };
    save_record(state_root, &record)?;
    locked.save_revision(revision)?;
    Ok(AuthorizationResult {
        schema_version: "agent-hook.recovery-authorization-result.v1".to_string(),
        capability_id: capability.capability_id,
        capability_digest,
        scope: capability.scope.as_str().to_string(),
        expires_at: capability.expires_at,
        rule_count: capability.rules.len(),
    })
}

pub fn consume_exact(
    state_root: &Path,
    capability_file: &Path,
    product: Product,
    event: &str,
    target_digest: &str,
    command_digest: &str,
    snapshot_digest: &str,
) -> Result<(ConsumeResult, RecoveryGrant), HookError> {
    let bytes = read_private(capability_file)?;
    let capability: CapabilityFile = serde_json::from_slice(&bytes)
        .map_err(|_| HookError::data("capability-invalid", "recovery capability is invalid"))?;
    validate_capability(&capability)?;
    if capability.product != product
        || capability.event != event
        || capability.target_digest != target_digest
        || capability.command_digest != command_digest
        || capability.snapshot_digest != snapshot_digest
    {
        return Err(HookError::data(
            "capability-binding-mismatch",
            "recovery capability does not match the exact request",
        ));
    }
    let mut locked = lock(state_root)?;
    let mut record = load_record(state_root, &capability.capability_id)?;
    if record.status != "authorized" {
        return Err(HookError::data(
            "capability-replay-or-revoked",
            "recovery capability is consumed or revoked",
        ));
    }
    if record.capability_digest != digest(&bytes)
        || record.challenge_digest != capability.challenge_digest
        || record.key_digest != capability.key_digest
        || record.principal_digest != capability.principal_digest
        || record.manifest_digest != digest_serializable(&capability.manifest)?
    {
        return Err(HookError::data(
            "capability-state-drift",
            "recovery capability no longer matches its state record",
        ));
    }
    if capability.expires_at_epoch <= now_epoch() {
        return Err(HookError::data(
            "capability-expired",
            "recovery capability has expired",
        ));
    }
    let expected_principal = principal_digest(capability.scope)?;
    if expected_principal != capability.principal_digest {
        return Err(HookError::data(
            "capability-principal-mismatch",
            "recovery capability belongs to another session principal",
        ));
    }
    let key = read_private(&key_path(state_root))?;
    if digest(&key) != capability.key_digest
        || sign_capability(&capability, &key)? != capability.authorization_proof
    {
        return Err(HookError::data(
            "capability-key-rotated",
            "recovery authorization key changed or proof is invalid",
        ));
    }
    let revision = next_revision(state_root)?;
    if capability.scope == RecoveryScope::OneShot {
        record.status = "consumed".to_string();
    }
    record.state_revision = revision;
    record.updated_at = timestamp(now_epoch());
    save_record(state_root, &record)?;
    locked.save_revision(revision)?;
    let result = ConsumeResult {
        schema_version: "agent-hook.recovery-consume-result.v1".to_string(),
        capability_id: capability.capability_id.clone(),
        scope: capability.scope.as_str().to_string(),
        status: if capability.scope == RecoveryScope::OneShot {
            "consumed"
        } else {
            "active"
        }
        .to_string(),
        rule_count: capability.rules.len(),
    };
    let grant = RecoveryGrant {
        capability_id: Some(capability.capability_id),
        rules: capability.rules.into_iter().collect(),
        manifest: Some(capability.manifest),
    };
    Ok((result, grant))
}

pub fn consume_for_dispatch(
    state_root: &Path,
    capability_file: Option<&Path>,
    request: &NormalizedRequest,
) -> Result<RecoveryGrant, HookError> {
    let Some(path) = capability_file else {
        return Ok(RecoveryGrant::default());
    };
    consume_exact(
        state_root,
        path,
        request.product,
        &request.event,
        &request.target_digest,
        &request.command_digest,
        &request.snapshot_digest,
    )
    .map(|(_, grant)| grant)
}

pub fn status(state_root: &Path, capability_id: &str) -> Result<StatusResult, HookError> {
    let record = load_record(state_root, capability_id)?;
    Ok(StatusResult {
        schema_version: "agent-hook.recovery-status.v1".to_string(),
        capability_id: record.capability_id,
        scope: record.scope.as_str().to_string(),
        status: if record.expires_at_epoch <= now_epoch() && record.status == "authorized" {
            "expired".to_string()
        } else {
            record.status
        },
        expires_at_epoch: record.expires_at_epoch,
        rule_count: record.rules.len(),
    })
}

pub fn revoke(state_root: &Path, capability_id: &str) -> Result<StatusResult, HookError> {
    let mut locked = lock(state_root)?;
    let mut record = load_record(state_root, capability_id)?;
    if record.status == "consumed" {
        return Err(HookError::data(
            "capability-already-consumed",
            "a consumed one-shot capability cannot be revoked",
        ));
    }
    let revision = next_revision(state_root)?;
    record.status = "revoked".to_string();
    record.state_revision = revision;
    record.updated_at = timestamp(now_epoch());
    save_record(state_root, &record)?;
    locked.save_revision(revision)?;
    status(state_root, capability_id)
}

fn validate_binding(
    event: &str,
    target_digest: &str,
    command_digest: &str,
    snapshot_digest: &str,
    rules: &[String],
) -> Result<(), HookError> {
    if event.is_empty() || event.len() > 128 || rules.is_empty() || rules.len() > 64 {
        return Err(HookError::usage(
            "recovery-binding-invalid",
            "recovery event/rule binding is empty or exceeds its limit",
        ));
    }
    if ![target_digest, command_digest, snapshot_digest]
        .iter()
        .all(|value| valid_digest(value))
    {
        return Err(HookError::usage(
            "recovery-binding-digest-invalid",
            "recovery bindings require lowercase sha256 digests",
        ));
    }
    let _ = canonical_rules(rules)?;
    Ok(())
}

fn canonical_rules(rules: &[String]) -> Result<Vec<String>, HookError> {
    let mut result = rules.to_vec();
    if result.iter().any(|rule| {
        rule.is_empty()
            || rule.len() > 128
            || !rule
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    }) {
        return Err(HookError::usage(
            "recovery-rule-invalid",
            "recovery rule IDs must be bounded stable identifiers",
        ));
    }
    result.sort();
    result.dedup();
    if result.len() != rules.len() {
        return Err(HookError::usage(
            "recovery-rule-duplicate",
            "recovery rule IDs must be unique",
        ));
    }
    Ok(result)
}

fn validate_challenge(challenge: &Challenge) -> Result<(), HookError> {
    if challenge.schema_version != CHALLENGE_VERSION {
        return Err(HookError::data(
            "challenge-version-unsupported",
            "unsupported recovery challenge schema",
        ));
    }
    validate_binding(
        &challenge.event,
        &challenge.target_digest,
        &challenge.command_digest,
        &challenge.snapshot_digest,
        &challenge.rules,
    )?;
    validate_manifest(
        &challenge.manifest,
        challenge.product,
        &challenge.event,
        &challenge.rules,
    )
}

fn validate_capability(capability: &CapabilityFile) -> Result<(), HookError> {
    if capability.schema_version != CAPABILITY_VERSION
        || !valid_digest(&capability.challenge_digest)
        || !valid_digest(&capability.key_digest)
        || !valid_digest(&capability.authorization_proof)
    {
        return Err(HookError::data(
            "capability-version-unsupported",
            "unsupported or malformed recovery capability",
        ));
    }
    validate_binding(
        &capability.event,
        &capability.target_digest,
        &capability.command_digest,
        &capability.snapshot_digest,
        &capability.rules,
    )?;
    validate_manifest(
        &capability.manifest,
        capability.product,
        &capability.event,
        &capability.rules,
    )
}

fn validate_manifest(
    manifest: &RecoveryManifest,
    product: Product,
    event: &str,
    requested_rules: &[String],
) -> Result<(), HookError> {
    if manifest.schema_version != "agent-hook.recovery-manifest.v1"
        || manifest.product != product
        || manifest.event != event
        || !valid_digest(&manifest.config_digest)
        || !valid_digest(&manifest.policy_digest)
    {
        return Err(HookError::data(
            "recovery-manifest-invalid",
            "recovery manifest is invalid or does not match the binding",
        ));
    }
    let manifest_ids = manifest
        .rules
        .iter()
        .map(|rule| rule.id.as_str())
        .collect::<BTreeSet<_>>();
    if requested_rules
        .iter()
        .any(|rule| !manifest_ids.contains(rule.as_str()))
    {
        return Err(HookError::data(
            "recovery-rule-unknown",
            "recovery requested an unknown or non-recoverable rule",
        ));
    }
    Ok(())
}

fn principal_digest(scope: RecoveryScope) -> Result<String, HookError> {
    let uid = unsafe { libc::geteuid() };
    let material = match scope {
        RecoveryScope::OneShot => format!("uid:{uid}"),
        RecoveryScope::RepairWindow => {
            let session = std::env::var("AGENT_SESSION_ID").map_err(|_| {
                HookError::data(
                    "repair-window-session-required",
                    "repair windows require a managed agent session principal",
                )
            })?;
            format!("uid:{uid}:session:{session}")
        }
    };
    Ok(digest(material.as_bytes()))
}

fn sign_capability(capability: &CapabilityFile, key: &[u8]) -> Result<String, HookError> {
    let mut unsigned = capability.clone();
    unsigned.authorization_proof.clear();
    let bytes = serde_json::to_vec(&unsigned)
        .map_err(|_| HookError::runtime("recovery-sign-failed", "capability signing failed"))?;
    let mut digest_input = Vec::with_capacity(key.len() + bytes.len() + 1);
    digest_input.extend_from_slice(key);
    digest_input.push(0);
    digest_input.extend_from_slice(&bytes);
    Ok(digest(&digest_input))
}

fn load_or_create_key(state_root: &Path) -> Result<Vec<u8>, HookError> {
    let path = key_path(state_root);
    match read_private(&path) {
        Ok(key) => Ok(key),
        Err(error) if error.code == "recovery-file-unavailable" => {
            let key = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            )
            .into_bytes();
            write_new_private(&path, &key)?;
            Ok(key)
        }
        Err(error) => Err(error),
    }
}

fn save_record(state_root: &Path, record: &RecoveryRecord) -> Result<(), HookError> {
    let bytes = serde_json::to_vec_pretty(record).map_err(|_| {
        HookError::runtime("recovery-render-failed", "recovery record render failed")
    })?;
    write_atomic(
        &record_path(state_root, &record.capability_id),
        &bytes,
        SECRET_FILE_MODE,
    )
    .map_err(|_| {
        HookError::runtime(
            "recovery-state-write-failed",
            "recovery record write failed",
        )
    })
}

fn load_record(state_root: &Path, capability_id: &str) -> Result<RecoveryRecord, HookError> {
    if capability_id.is_empty()
        || capability_id.len() > 128
        || !capability_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(HookError::usage(
            "capability-id-invalid",
            "capability ID is invalid",
        ));
    }
    let bytes = read_private(&record_path(state_root, capability_id))?;
    let record: RecoveryRecord = serde_json::from_slice(&bytes)
        .map_err(|_| HookError::data("recovery-record-invalid", "recovery record is invalid"))?;
    if record.schema_version != RECORD_VERSION || record.capability_id != capability_id {
        return Err(HookError::data(
            "recovery-record-invalid",
            "recovery record identity is invalid",
        ));
    }
    Ok(record)
}

fn read_private(path: &Path) -> Result<Vec<u8>, HookError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        HookError::data(
            "recovery-file-unavailable",
            "private recovery file is unavailable",
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_RECOVERY_BYTES
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(HookError::data(
            "recovery-file-untrusted",
            "private recovery file type, owner, size, or mode is untrusted",
        ));
    }
    fs::read(path).map_err(|_| {
        HookError::runtime(
            "recovery-file-read-failed",
            "private recovery file read failed",
        )
    })
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<(), HookError> {
    let parent = path.parent().ok_or_else(|| {
        HookError::usage("recovery-output-invalid", "recovery output has no parent")
    })?;
    let metadata = fs::symlink_metadata(parent).map_err(|_| {
        HookError::runtime(
            "recovery-output-parent-unavailable",
            "recovery output parent is unavailable",
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(HookError::data(
            "recovery-output-parent-untrusted",
            "recovery output parent owner or type is untrusted",
        ));
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(SECRET_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| {
            HookError::data(
                "recovery-output-exists",
                "recovery output must be a new private file",
            )
        })?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| {
            HookError::runtime(
                "recovery-output-write-failed",
                "recovery output write failed",
            )
        })
}

struct StateLock {
    file: File,
    revision_path: PathBuf,
}

impl StateLock {
    fn save_revision(&mut self, revision: u64) -> Result<(), HookError> {
        write_atomic(
            &self.revision_path,
            format!("{revision}\n").as_bytes(),
            SECRET_FILE_MODE,
        )
        .map_err(|_| {
            HookError::runtime(
                "recovery-revision-write-failed",
                "recovery revision write failed",
            )
        })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn lock(state_root: &Path) -> Result<StateLock, HookError> {
    crate::paths::ensure_private_state_dir(state_root, "recovery-state-dir")?;
    let root = state_root.join("recovery");
    ensure_private_dir(&root, "recovery-root")?;
    ensure_private_dir(&root.join("challenges"), "recovery-challenges-dir")?;
    ensure_private_dir(&root.join("records"), "recovery-records-dir")?;
    let path = root.join("lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(SECRET_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|_| {
            HookError::runtime("recovery-lock-unavailable", "recovery lock is unavailable")
        })?;
    let started = Instant::now();
    loop {
        let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if status == 0 {
            break;
        }
        if started.elapsed() >= LOCK_TIMEOUT {
            return Err(HookError::unavailable(
                "recovery-lock-timeout",
                "recovery state is busy",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(StateLock {
        file,
        revision_path: root.join("revision"),
    })
}

fn next_revision(state_root: &Path) -> Result<u64, HookError> {
    let path = state_root.join("recovery/revision");
    let current = match fs::read_to_string(path) {
        Ok(value) => value.trim().parse::<u64>().map_err(|_| {
            HookError::data("recovery-revision-invalid", "recovery revision is invalid")
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(_) => {
            return Err(HookError::runtime(
                "recovery-revision-read-failed",
                "recovery revision could not be read",
            ));
        }
    };
    Ok(current.saturating_add(1))
}

fn ensure_private_dir(path: &Path, role: &str) -> Result<(), HookError> {
    crate::paths::ensure_private_state_dir(path, role)
}

fn challenge_path(state_root: &Path, challenge_id: &str) -> PathBuf {
    state_root
        .join("recovery/challenges")
        .join(format!("{challenge_id}.json"))
}

fn record_path(state_root: &Path, capability_id: &str) -> PathBuf {
    state_root
        .join("recovery/records")
        .join(format!("{capability_id}.json"))
}

fn key_path(state_root: &Path) -> PathBuf {
    state_root.join("recovery/authorization.key")
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn timestamp(epoch: i64) -> String {
    Timestamp::from_second(epoch)
        .map(|timestamp| timestamp.to_string())
        .unwrap_or_else(|_| Timestamp::now().to_string())
}

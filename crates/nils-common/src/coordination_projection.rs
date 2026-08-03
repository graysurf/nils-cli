//! Bounded, read-only projection of agent-session coordination ownership data.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const REGISTRY_VERSION: &str = "agent-session.coordination-registry.v1";
pub const CLAIM_FENCE_REGISTRY_VERSION: &str = "agent-session.coordination-registry.v2";
pub const CLAIM_VERSION: &str = "agent-session.work-context.v1";
/// Canonical whole-registry byte cap, shared by this projection reader and the
/// agent-session coordination read/write path (which references this constant).
pub const MAX_REGISTRY_BYTES: u64 = 68 * 1024 * 1024;
const MAX_HEARTBEAT_BYTES: u64 = 256;
const MAX_SESSION_BYTES: u64 = 2 * 1024 * 1024;
const HEARTBEAT_FRESH_SECONDS: i64 = 30;
const SESSION_VERSION: &str = "agent-session.session.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    Unavailable,
    Untrusted,
    Invalid,
    /// The registry parsed as this schema family but declares a version this
    /// release does not implement, so it was written by a different release
    /// generation. This is recoverable version drift, not corruption; keeping it
    /// distinct from [`ReadError::Invalid`] is what lets a consumer offer a
    /// bounded upgrade-recovery path instead of a dead end.
    Incompatible,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RegistryProjection {
    schema_version: String,
    pub fingerprint_epoch: u64,
    pub fingerprint_key: String,
    #[serde(default)]
    pub brokers: BTreeMap<String, BrokerProjection>,
    #[serde(default)]
    pub claims: Vec<ClaimProjection>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BrokerProjection {
    pub session_id: String,
    pub incarnation: String,
    pub state: String,
    #[serde(default)]
    pub coordination_mode: CoordinationMode,
    #[serde(default)]
    pub heartbeat_epoch: i64,
    /// Release that produced this broker record. Absent for a broker created
    /// before the field existed, which is compatibility state rather than
    /// evidence of drift.
    #[serde(default)]
    pub binary_version: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CoordinationMode {
    #[default]
    Advisory,
    Enforce,
    Off,
}

#[derive(Debug, Deserialize)]
struct SessionProjection {
    schema_version: String,
    id: String,
    #[serde(default)]
    coordination_mode: CoordinationMode,
    runtime: Option<RuntimeProjection>,
}

#[derive(Debug, Deserialize)]
struct RuntimeProjection {
    launch_id: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ClaimProjection {
    schema_version: String,
    pub session_id: String,
    pub session_incarnation: String,
    pub state: String,
    #[serde(default)]
    pub worktrees: Vec<String>,
    #[serde(default)]
    pub repositories: Vec<String>,
    #[serde(default)]
    pub provider_refs: Vec<ProviderRefProjection>,
    #[serde(default)]
    pub scopes: Vec<ScopeProjection>,
    pub expires_at_epoch: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ProviderRefProjection {
    pub kind: String,
    pub repository: String,
    pub number: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ScopeProjection {
    pub kind: String,
    pub repository: String,
    #[serde(default)]
    pub value: String,
}

pub fn load(state_dir: &Path) -> Result<Option<RegistryProjection>, ReadError> {
    let path = state_dir.join("coordination/registry.json");
    let bytes = match read_private(&path, MAX_REGISTRY_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Err(ReadError::Untrusted);
        }
        Err(_) => return Err(ReadError::Unavailable),
    };
    let projection: RegistryProjection =
        serde_json::from_slice(&bytes).map_err(|_| ReadError::Invalid)?;
    if !matches!(
        projection.schema_version.as_str(),
        REGISTRY_VERSION | CLAIM_FENCE_REGISTRY_VERSION
    ) {
        // A well-formed body in this schema family that declares an unknown
        // version came from another release generation. Report drift so the
        // consumer can offer recovery; anything else stays corruption.
        if crate::runtime_compat::registry_generation_drift(
            &projection.schema_version,
            &[REGISTRY_VERSION, CLAIM_FENCE_REGISTRY_VERSION],
        ) {
            return Err(ReadError::Incompatible);
        }
        return Err(ReadError::Invalid);
    }
    if projection.fingerprint_epoch == 0
        || projection.fingerprint_key.len() < 32
        || projection
            .claims
            .iter()
            .any(|claim| claim.schema_version != CLAIM_VERSION)
    {
        return Err(ReadError::Invalid);
    }
    Ok(Some(projection))
}

pub fn heartbeat_fresh(
    state_dir: &Path,
    session_id: &str,
    incarnation: &str,
    now_epoch: i64,
) -> bool {
    heartbeat_age_seconds(state_dir, session_id, incarnation, now_epoch)
        .is_some_and(|age| age <= HEARTBEAT_FRESH_SECONDS)
}

/// Return the privacy-safe age of the exact broker heartbeat sidecar.
///
/// The sidecar body and path remain private. Consumers can compare this age
/// with their own operation horizon instead of retaining a freshness boolean
/// that may expire before the guarded operation starts.
pub fn heartbeat_age_seconds(
    state_dir: &Path,
    session_id: &str,
    incarnation: &str,
    now_epoch: i64,
) -> Option<i64> {
    let path = heartbeat_path(state_dir, session_id);
    let Ok(bytes) = read_private(&path, MAX_HEARTBEAT_BYTES) else {
        return None;
    };
    let Ok(value) = std::str::from_utf8(&bytes) else {
        return None;
    };
    let (observed_incarnation, observed_epoch) = value.trim().rsplit_once(':')?;
    if observed_incarnation != incarnation {
        return None;
    }
    let Ok(observed_epoch) = observed_epoch.parse::<i64>() else {
        return None;
    };
    let age = now_epoch.saturating_sub(observed_epoch);
    (age >= 0).then_some(age)
}

pub fn session_coordination_mode(
    state_dir: &Path,
    session_id: &str,
    incarnation: &str,
) -> Result<CoordinationMode, ReadError> {
    if session_id.is_empty()
        || incarnation.is_empty()
        || !session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(ReadError::Invalid);
    }
    let path = state_dir
        .join("sessions")
        .join(session_id)
        .join("session.json");
    let bytes = read_private(&path, MAX_SESSION_BYTES).map_err(|error| {
        if error.kind() == io::ErrorKind::PermissionDenied {
            ReadError::Untrusted
        } else {
            ReadError::Unavailable
        }
    })?;
    let projection: SessionProjection =
        serde_json::from_slice(&bytes).map_err(|_| ReadError::Invalid)?;
    if projection.schema_version != SESSION_VERSION
        || projection.id != session_id
        || projection
            .runtime
            .as_ref()
            .is_none_or(|runtime| runtime.launch_id != incarnation)
    {
        return Err(ReadError::Invalid);
    }
    Ok(projection.coordination_mode)
}

pub fn heartbeat_path(state_dir: &Path, session_id: &str) -> PathBuf {
    state_dir
        .join("sessions")
        .join(session_id)
        .join("coordination/heartbeat")
}

pub fn worktree_fingerprint(epoch: u64, key: &str, checkout: &Path) -> Option<String> {
    if epoch == 0 || key.len() < 32 {
        return None;
    }
    let canonical = fs::canonicalize(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    let digest = hmac_sha256(key.as_bytes(), canonical.as_os_str().as_encoded_bytes());
    Some(format!("hmac-sha256:{epoch}:{}", hex(&digest)))
}

fn read_private(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.len() > max_bytes
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private coordination input is untrusted",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private coordination input is oversized",
        ));
    }
    Ok(bytes)
}

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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_reader_accepts_legacy_and_fence_aware_registry_markers() {
        for schema_version in [REGISTRY_VERSION, CLAIM_FENCE_REGISTRY_VERSION] {
            let temporary = tempfile::TempDir::new().expect("temporary state");
            let coordination = temporary.path().join("coordination");
            fs::create_dir_all(&coordination).expect("coordination directory");
            let registry = serde_json::json!({
                "schema_version": schema_version,
                "fingerprint_epoch": 1,
                "fingerprint_key": "k".repeat(32),
                "brokers": {},
                "claims": []
            });
            let path = coordination.join("registry.json");
            fs::write(&path, serde_json::to_vec(&registry).expect("registry JSON"))
                .expect("registry");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("secure registry");

            assert!(
                load(temporary.path())
                    .expect("supported registry marker")
                    .is_some(),
                "schema_version={schema_version}"
            );
        }
    }

    #[test]
    fn another_registry_generation_is_incompatible_while_corruption_stays_invalid() {
        for (body, expected) in [
            (
                serde_json::json!({
                    "schema_version": "agent-session.coordination-registry.v9",
                    "fingerprint_epoch": 1,
                    "fingerprint_key": "k".repeat(32),
                    "brokers": {},
                    "claims": []
                })
                .to_string(),
                ReadError::Incompatible,
            ),
            (
                serde_json::json!({
                    "schema_version": "agent-session.something-else.v1",
                    "fingerprint_epoch": 1,
                    "fingerprint_key": "k".repeat(32),
                    "brokers": {},
                    "claims": []
                })
                .to_string(),
                ReadError::Invalid,
            ),
            ("{".to_string(), ReadError::Invalid),
        ] {
            let temporary = tempfile::TempDir::new().expect("temporary state");
            let coordination = temporary.path().join("coordination");
            fs::create_dir_all(&coordination).expect("coordination directory");
            let path = coordination.join("registry.json");
            fs::write(&path, body.as_bytes()).expect("registry");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("secure registry");

            assert_eq!(
                load(temporary.path()).expect_err("classified read failure"),
                expected,
                "body={body}"
            );
        }
    }

    #[test]
    fn a_broker_release_is_projected_and_stays_optional_for_unpublished_records() {
        let temporary = tempfile::TempDir::new().expect("temporary state");
        let coordination = temporary.path().join("coordination");
        fs::create_dir_all(&coordination).expect("coordination directory");
        let registry = serde_json::json!({
            "schema_version": REGISTRY_VERSION,
            "fingerprint_epoch": 1,
            "fingerprint_key": "k".repeat(32),
            "brokers": {
                "published": {
                    "session_id": "published",
                    "incarnation": "inc",
                    "state": "ready",
                    "binary_version": "1.25.13"
                },
                "unpublished": {
                    "session_id": "unpublished",
                    "incarnation": "inc",
                    "state": "ready"
                }
            },
            "claims": []
        });
        let path = coordination.join("registry.json");
        fs::write(&path, serde_json::to_vec(&registry).expect("registry JSON")).expect("registry");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("secure registry");

        let projection = load(temporary.path())
            .expect("supported registry")
            .expect("registry present");
        assert_eq!(
            projection.brokers["published"].binary_version.as_deref(),
            Some("1.25.13")
        );
        assert_eq!(projection.brokers["unpublished"].binary_version, None);
    }

    #[test]
    fn heartbeat_age_is_exact_incarnation_private_and_preserves_hard_freshness() {
        let temporary = tempfile::TempDir::new().expect("temporary state");
        let heartbeat = heartbeat_path(temporary.path(), "worker");
        fs::create_dir_all(heartbeat.parent().expect("heartbeat parent"))
            .expect("create heartbeat directory");
        fs::write(&heartbeat, b"worker-inc:100\n").expect("write heartbeat");
        fs::set_permissions(&heartbeat, fs::Permissions::from_mode(0o600))
            .expect("secure heartbeat");

        assert_eq!(
            heartbeat_age_seconds(temporary.path(), "worker", "worker-inc", 129),
            Some(29)
        );
        assert!(heartbeat_fresh(
            temporary.path(),
            "worker",
            "worker-inc",
            130
        ));
        assert!(!heartbeat_fresh(
            temporary.path(),
            "worker",
            "worker-inc",
            131
        ));
        assert_eq!(
            heartbeat_age_seconds(temporary.path(), "worker", "other-inc", 101),
            None
        );
        assert_eq!(
            heartbeat_age_seconds(temporary.path(), "worker", "worker-inc", 99),
            None
        );
    }
}

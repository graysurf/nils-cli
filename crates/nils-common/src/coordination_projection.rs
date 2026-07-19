//! Bounded, read-only projection of agent-session coordination ownership data.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const REGISTRY_VERSION: &str = "agent-session.coordination-registry.v1";
pub const CLAIM_VERSION: &str = "agent-session.work-context.v1";
const MAX_REGISTRY_BYTES: u64 = 68 * 1024 * 1024;
const MAX_HEARTBEAT_BYTES: u64 = 256;
const HEARTBEAT_FRESH_SECONDS: i64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    Unavailable,
    Untrusted,
    Invalid,
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
    pub heartbeat_epoch: i64,
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
    if projection.schema_version != REGISTRY_VERSION
        || projection.fingerprint_epoch == 0
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
    let path = heartbeat_path(state_dir, session_id);
    let Ok(bytes) = read_private(&path, MAX_HEARTBEAT_BYTES) else {
        return false;
    };
    let Ok(value) = std::str::from_utf8(&bytes) else {
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
    (0..=HEARTBEAT_FRESH_SECONDS).contains(&now_epoch.saturating_sub(observed_epoch))
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

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::HookError;
use crate::model::{DecisionAction, NormalizedRequest, SemanticConflict};
use crate::paths::agent_session_state_root;

const MAX_REGISTRY_BYTES: u64 = 68 * 1024 * 1024;
const HEARTBEAT_FRESH_SECONDS: i64 = 30;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LivenessClass {
    Active,
    Stale,
    Orphaned,
    Unknown,
    Unclaimed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerLiveness {
    pub schema_version: String,
    pub classification: LivenessClass,
    pub semantic_conflict: String,
    pub action: DecisionAction,
    pub reason_code: String,
    pub dirty: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct Registry {
    #[serde(default)]
    fingerprint_epoch: u64,
    #[serde(default)]
    fingerprint_key: String,
    #[serde(default)]
    brokers: BTreeMap<String, Broker>,
    #[serde(default)]
    claims: Vec<Claim>,
}

#[derive(Debug, Deserialize)]
struct Broker {
    session_id: String,
    incarnation: String,
    state: String,
    heartbeat_epoch: i64,
}

#[derive(Debug, Deserialize)]
struct Claim {
    session_id: String,
    session_incarnation: String,
    state: String,
    #[serde(default)]
    worktrees: Vec<String>,
    #[serde(default)]
    repositories: Vec<String>,
    #[serde(default)]
    provider_refs: Vec<ProviderRef>,
    #[serde(default)]
    scopes: Vec<Scope>,
    expires_at_epoch: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct ProviderRef {
    kind: String,
    repository: String,
    number: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct Scope {
    kind: String,
    repository: String,
    #[serde(default)]
    value: String,
}

pub fn derive_semantic_conflict(request: &NormalizedRequest) -> SemanticConflict {
    let Ok(Some(registry)) = load_registry() else {
        return SemanticConflict::Unknown;
    };
    let Some(current_session) = std::env::var("AGENT_SESSION_ID")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return SemanticConflict::Unknown;
    };
    let now = now_epoch();
    let Some(current_broker) = registry.brokers.get(&current_session).filter(|broker| {
        broker.session_id == current_session
            && broker.state == "ready"
            && now.saturating_sub(broker.heartbeat_epoch) <= HEARTBEAT_FRESH_SECONDS
    }) else {
        return SemanticConflict::Unknown;
    };
    let mut own_claims = registry.claims.iter().filter(|claim| {
        claim.session_id == current_session
            && claim.session_incarnation == current_broker.incarnation
            && claim.state == "active"
            && claim.expires_at_epoch >= now
    });
    let Some(own) = own_claims.next() else {
        return SemanticConflict::Unknown;
    };
    if own_claims.next().is_some() {
        return SemanticConflict::Unknown;
    }

    let mut potential = false;
    let mut incomplete = false;
    for peer in registry.claims.iter().filter(|claim| {
        claim.session_id != current_session
            && claim.state == "active"
            && claim.expires_at_epoch >= now
    }) {
        let peer_fresh = registry
            .brokers
            .get(&peer.session_id)
            .is_some_and(|broker| {
                broker.session_id == peer.session_id
                    && broker.incarnation == peer.session_incarnation
                    && broker.state == "ready"
                    && now.saturating_sub(broker.heartbeat_epoch) <= HEARTBEAT_FRESH_SECONDS
            });
        if !peer_fresh {
            incomplete = true;
            continue;
        }
        if intersects(&own.worktrees, &peer.worktrees)
            || own
                .provider_refs
                .iter()
                .any(|reference| peer.provider_refs.contains(reference))
            || scopes_overlap(&own.scopes, &peer.scopes)
        {
            return SemanticConflict::Definite;
        }
        if own
            .repositories
            .iter()
            .any(|repository| peer.repositories.contains(repository))
        {
            potential = true;
        }
    }
    if incomplete {
        SemanticConflict::Unknown
    } else if potential {
        SemanticConflict::Potential
    } else {
        // A trusted target binding with no matching peer remains clear. The
        // request path is not serialized or exposed.
        let _ = request;
        SemanticConflict::Clear
    }
}

pub fn classify(request: &NormalizedRequest, legacy_ttl_seconds: u64) -> OwnerLiveness {
    if matches!(request.semantic_conflict, Some(SemanticConflict::Definite)) {
        return result(
            LivenessClass::Active,
            "definite",
            DecisionAction::Block,
            "semantic-conflict-definite",
            None,
        );
    }
    if matches!(request.semantic_conflict, Some(SemanticConflict::Potential)) {
        return result(
            LivenessClass::Unknown,
            "potential",
            DecisionAction::Warn,
            "semantic-conflict-potential",
            None,
        );
    }
    let Some(target) = request.target_path.as_deref() else {
        return unknown("owner-target-unavailable");
    };
    match load_registry() {
        Ok(Some(registry)) => classify_registry(request, target, &registry),
        Ok(None) => legacy_classify(target, legacy_ttl_seconds),
        Err(_) => unknown("coordination-evidence-untrusted"),
    }
}

fn classify_registry(
    request: &NormalizedRequest,
    target: &Path,
    registry: &Registry,
) -> OwnerLiveness {
    if registry.fingerprint_epoch == 0 || registry.fingerprint_key.len() < 32 {
        return unknown("coordination-registry-invalid");
    }
    let fingerprint = worktree_fingerprint(registry, target);
    let now = now_epoch();
    let current_session = std::env::var("AGENT_SESSION_ID").ok();
    let mut matching = registry.claims.iter().filter(|claim| {
        claim.state == "active"
            && claim.expires_at_epoch >= now
            && claim.worktrees.iter().any(|value| value == &fingerprint)
    });
    let Some(claim) = matching.next() else {
        return result(
            LivenessClass::Unclaimed,
            semantic_name(request.semantic_conflict),
            DecisionAction::Allow,
            "owner-unclaimed",
            Some(false),
        );
    };
    if matching.next().is_some() {
        return unknown("coordination-owner-ambiguous");
    }
    let dirty = checkout_dirty(target);
    let own = current_session
        .as_deref()
        .is_some_and(|session| session == claim.session_id);
    let broker = registry.brokers.get(&claim.session_id).filter(|broker| {
        broker.session_id == claim.session_id
            && broker.incarnation == claim.session_incarnation
            && broker.state == "ready"
    });
    match broker {
        Some(broker) if now.saturating_sub(broker.heartbeat_epoch) <= HEARTBEAT_FRESH_SECONDS => {
            result(
                LivenessClass::Active,
                semantic_name(request.semantic_conflict),
                if own {
                    DecisionAction::Allow
                } else {
                    DecisionAction::Block
                },
                if own {
                    "owner-active-self"
                } else {
                    "owner-active-foreign"
                },
                dirty,
            )
        }
        Some(_) => stale_or_dirty(dirty, "owner-stale"),
        None => {
            let (action, reason) = match dirty {
                Some(true) => (DecisionAction::Block, "owner-orphaned-dirty"),
                Some(false) => (DecisionAction::Warn, "owner-orphaned-clean"),
                None => (DecisionAction::Block, "owner-orphaned-unknown"),
            };
            result(
                LivenessClass::Orphaned,
                semantic_name(request.semantic_conflict),
                action,
                reason,
                dirty,
            )
        }
    }
}

fn legacy_classify(target: &Path, legacy_ttl_seconds: u64) -> OwnerLiveness {
    let dirty = checkout_dirty(target);
    let age = fs::metadata(target)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|duration| duration.as_secs());
    if age.is_some_and(|age| age > legacy_ttl_seconds.min(900)) {
        stale_or_dirty(dirty, "legacy-owner-stale")
    } else {
        result(
            LivenessClass::Unknown,
            "unknown",
            DecisionAction::Block,
            "legacy-owner-unknown",
            dirty,
        )
    }
}

fn stale_or_dirty(dirty: Option<bool>, prefix: &str) -> OwnerLiveness {
    let (action, suffix) = match dirty {
        Some(true) => (DecisionAction::Block, "dirty"),
        Some(false) => (DecisionAction::Allow, "clean-reclaim"),
        None => (DecisionAction::Block, "unknown"),
    };
    result(
        LivenessClass::Stale,
        "unknown",
        action,
        &format!("{prefix}-{suffix}"),
        dirty,
    )
}

fn unknown(reason: &str) -> OwnerLiveness {
    result(
        LivenessClass::Unknown,
        "unknown",
        DecisionAction::Block,
        reason,
        None,
    )
}

fn result(
    classification: LivenessClass,
    semantic_conflict: &str,
    action: DecisionAction,
    reason_code: &str,
    dirty: Option<bool>,
) -> OwnerLiveness {
    OwnerLiveness {
        schema_version: "agent-hook.owner-liveness.v1".to_string(),
        classification,
        semantic_conflict: semantic_conflict.to_string(),
        action,
        reason_code: reason_code.to_string(),
        dirty,
    }
}

fn semantic_name(value: Option<SemanticConflict>) -> &'static str {
    match value {
        Some(SemanticConflict::Definite) => "definite",
        Some(SemanticConflict::Potential) => "potential",
        Some(SemanticConflict::Unknown) => "unknown",
        Some(SemanticConflict::Clear) => "clear",
        None => "unavailable",
    }
}

fn intersects(left: &[String], right: &[String]) -> bool {
    left.iter().any(|value| right.contains(value))
}

fn scopes_overlap(left: &[Scope], right: &[Scope]) -> bool {
    left.iter().any(|left| {
        right.iter().any(|right| {
            if left.repository != right.repository {
                return false;
            }
            match (left.kind.as_str(), right.kind.as_str()) {
                ("repository", _) | (_, "repository") => true,
                ("path-exact", "path-exact") => left.value == right.value,
                ("path-prefix", "path-prefix") => {
                    path_in_prefix(&left.value, &right.value)
                        || path_in_prefix(&right.value, &left.value)
                }
                ("path-exact", "path-prefix") => path_in_prefix(&left.value, &right.value),
                ("path-prefix", "path-exact") => path_in_prefix(&right.value, &left.value),
                _ => false,
            }
        })
    })
}

fn path_in_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn load_registry() -> Result<Option<Registry>, HookError> {
    let path = agent_session_state_root()?.join("coordination/registry.json");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(HookError::runtime(
                "coordination-unavailable",
                "coordination registry is unavailable",
            ));
        }
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_REGISTRY_BYTES
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(HookError::data(
            "coordination-untrusted",
            "coordination registry ownership, mode, or type is untrusted",
        ));
    }
    let bytes = fs::read(path).map_err(|_| {
        HookError::runtime(
            "coordination-unavailable",
            "coordination registry could not be read",
        )
    })?;
    let registry = serde_json::from_slice(&bytes).map_err(|_| {
        HookError::data(
            "coordination-invalid",
            "coordination registry could not be classified",
        )
    })?;
    Ok(Some(registry))
}

fn worktree_fingerprint(registry: &Registry, checkout: &Path) -> String {
    let canonical = fs::canonicalize(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    let hash = hmac_sha256(
        registry.fingerprint_key.as_bytes(),
        canonical.as_os_str().as_encoded_bytes(),
    );
    let mut output = format!("hmac-sha256:{}:", registry.fingerprint_epoch);
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
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

fn checkout_dirty(path: &Path) -> Option<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["status", "--porcelain=v1", "--untracked-files=normal"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(!output.stdout.is_empty())
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

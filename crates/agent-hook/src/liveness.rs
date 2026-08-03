use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use nils_common::coordination_projection::{self, CoordinationMode, RegistryProjection};
use serde::{Deserialize, Serialize};

use crate::error::HookError;
use crate::model::{DecisionAction, NormalizedRequest, SemanticConflict};
use crate::paths::agent_session_state_root;

#[derive(Debug)]
pub struct Snapshot {
    state_root: PathBuf,
    registry: RegistryProjection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CurrentPrincipal {
    session_id: String,
    incarnation: String,
}

impl CurrentPrincipal {
    fn from_env() -> Option<Self> {
        Some(Self {
            session_id: std::env::var("AGENT_SESSION_ID")
                .ok()
                .filter(|value| !value.is_empty())?,
            incarnation: std::env::var("AGENT_SESSION_RUNTIME_ID")
                .ok()
                .filter(|value| !value.is_empty())?,
        })
    }
}

const MANAGED_METADATA_ENV: [&str; 6] = [
    "AGENT_SESSION_ID",
    "AGENT_SESSION_RUNTIME_ID",
    "AGENT_SESSION_STATE_DIR",
    "AGENT_SESSION_COORDINATION_MODE",
    "AGENT_SESSION_CAPABILITY_FILE",
    "AGENT_SESSION_CHECKPOINT_FILE",
];

fn managed_metadata_present() -> bool {
    MANAGED_METADATA_ENV
        .iter()
        .any(|name| std::env::var_os(name).is_some())
}

pub(crate) fn current_process_is_unmanaged() -> bool {
    !managed_metadata_present()
}

#[derive(Clone, Copy, Debug)]
struct RootInputs {
    dirty: Option<bool>,
    age_seconds: Option<u64>,
}

pub(crate) trait LivenessIo {
    fn session_coordination_mode(
        &self,
        state_root: &Path,
        session_id: &str,
        incarnation: &str,
    ) -> Result<CoordinationMode, coordination_projection::ReadError>;

    fn checkout_dirty(&self, path: &Path) -> Option<bool>;

    fn checkout_age(&self, path: &Path) -> Option<u64>;
}

struct SystemLivenessIo;

impl LivenessIo for SystemLivenessIo {
    fn session_coordination_mode(
        &self,
        state_root: &Path,
        session_id: &str,
        incarnation: &str,
    ) -> Result<CoordinationMode, coordination_projection::ReadError> {
        coordination_projection::session_coordination_mode(state_root, session_id, incarnation)
    }

    fn checkout_dirty(&self, path: &Path) -> Option<bool> {
        checkout_dirty(path)
    }

    fn checkout_age(&self, path: &Path) -> Option<u64> {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .map(|duration| duration.as_secs())
    }
}

static SYSTEM_LIVENESS_IO: SystemLivenessIo = SystemLivenessIo;

pub(crate) fn system_io() -> &'static dyn LivenessIo {
    &SYSTEM_LIVENESS_IO
}

pub(crate) struct DispatchProjection<'snapshot, 'io> {
    snapshot: Option<&'snapshot Snapshot>,
    mode: Option<CoordinationMode>,
    now: i64,
    principal: Option<CurrentPrincipal>,
    unmanaged: bool,
    roots: RefCell<BTreeMap<PathBuf, RootInputs>>,
    io: &'io dyn LivenessIo,
}

impl<'snapshot, 'io> DispatchProjection<'snapshot, 'io> {
    pub(crate) fn new(
        snapshot: Option<&'snapshot Snapshot>,
        mode_override: Option<CoordinationMode>,
        unmanaged: bool,
        io: &'io dyn LivenessIo,
    ) -> Self {
        let now = now_epoch();
        let principal = CurrentPrincipal::from_env();
        let mode = mode_override.or_else(|| effective_mode(snapshot, principal.as_ref(), now, io));
        Self {
            snapshot,
            mode,
            now,
            principal,
            unmanaged,
            roots: RefCell::new(BTreeMap::new()),
            io,
        }
    }

    pub(crate) fn mode(&self) -> Option<CoordinationMode> {
        self.mode
    }

    pub(crate) fn is_unmanaged(&self) -> bool {
        self.unmanaged
    }

    fn root_inputs(&self, checkout: &Path) -> RootInputs {
        if let Some(inputs) = self.roots.borrow().get(checkout).copied() {
            return inputs;
        }
        let inputs = RootInputs {
            dirty: self.io.checkout_dirty(checkout),
            age_seconds: self.io.checkout_age(checkout),
        };
        self.roots
            .borrow_mut()
            .insert(checkout.to_path_buf(), inputs);
        inputs
    }
}

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

pub fn load_snapshot(unmanaged: bool) -> Result<Option<Snapshot>, HookError> {
    if unmanaged {
        return Ok(None);
    }
    let state_root = agent_session_state_root()?;
    coordination_projection::load(&state_root)
        .map(|registry| {
            registry.map(|registry| Snapshot {
                state_root,
                registry,
            })
        })
        .map_err(|error| match error {
            coordination_projection::ReadError::Unavailable => HookError::runtime(
                "coordination-unavailable",
                "coordination registry is unavailable",
            ),
            coordination_projection::ReadError::Untrusted => HookError::data(
                "coordination-untrusted",
                "coordination registry ownership, mode, or type is untrusted",
            ),
            coordination_projection::ReadError::Invalid => HookError::data(
                "coordination-invalid",
                "coordination registry schema or projection is invalid",
            ),
            // Drift across a release boundary is recoverable, so it carries its
            // own code and a bounded next action instead of reading as
            // corruption. Mutation still fails closed on it.
            coordination_projection::ReadError::Incompatible => HookError::data_with(
                crate::degradation::RUNTIME_VERSION_SKEW,
                "coordination registry was written by a different release generation",
                serde_json::json!({
                    "recovery_action": crate::observe::RECOVERY_BROKER_RECONCILE,
                }),
            ),
        })
}

/// Release published by the current principal's live broker, when both the
/// principal and the record are known.
///
/// A broker that predates the field returns `None`: that is compatibility
/// state, not evidence that a boundary was crossed.
pub fn current_broker_release(snapshot: &Snapshot) -> Option<&str> {
    let principal = CurrentPrincipal::from_env()?;
    let broker = snapshot.registry.brokers.get(&principal.session_id)?;
    if broker.incarnation != principal.incarnation {
        return None;
    }
    broker.binary_version.as_deref()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeModeHint {
    Missing,
    Valid(CoordinationMode),
    Invalid,
}

fn runtime_mode_hint() -> RuntimeModeHint {
    let Some(value) = std::env::var_os("AGENT_SESSION_COORDINATION_MODE") else {
        return RuntimeModeHint::Missing;
    };
    match value.to_str() {
        Some("advisory") => RuntimeModeHint::Valid(CoordinationMode::Advisory),
        Some("enforce") => RuntimeModeHint::Valid(CoordinationMode::Enforce),
        Some("off") => RuntimeModeHint::Valid(CoordinationMode::Off),
        Some(_) | None => RuntimeModeHint::Invalid,
    }
}

pub fn coordination_failure_mode() -> Option<CoordinationMode> {
    let Ok(state_root) = agent_session_state_root() else {
        return None;
    };
    let principal = CurrentPrincipal::from_env()?;
    trusted_runtime_mode(&state_root, None, &principal, system_io())
        .filter(|mode| matches!(mode, CoordinationMode::Advisory | CoordinationMode::Off))
}

pub fn semantic_conflict_action(
    conflict: Option<SemanticConflict>,
    mode: Option<CoordinationMode>,
) -> DecisionAction {
    match mode {
        Some(CoordinationMode::Off) => DecisionAction::Allow,
        Some(CoordinationMode::Advisory) => match conflict {
            Some(SemanticConflict::Clear) => DecisionAction::Allow,
            Some(
                SemanticConflict::Definite
                | SemanticConflict::Potential
                | SemanticConflict::Unknown,
            )
            | None => DecisionAction::Warn,
        },
        Some(CoordinationMode::Enforce) | None => match conflict {
            Some(SemanticConflict::Definite) => DecisionAction::Block,
            Some(SemanticConflict::Potential | SemanticConflict::Unknown) | None => {
                DecisionAction::Warn
            }
            Some(SemanticConflict::Clear) => DecisionAction::Allow,
        },
    }
}

pub fn derive_semantic_conflict(
    request: &NormalizedRequest,
    projection: &DispatchProjection<'_, '_>,
) -> SemanticConflict {
    let Some(snapshot) = projection.snapshot else {
        return SemanticConflict::Unknown;
    };
    let registry = &snapshot.registry;
    let Some(principal) = projection.principal.as_ref() else {
        return SemanticConflict::Unknown;
    };
    let now = projection.now;
    let Some(current_broker) = broker_for_session(snapshot, &principal.session_id, now)
        .filter(|broker| broker.incarnation == principal.incarnation)
    else {
        return SemanticConflict::Unknown;
    };
    if projection.mode == Some(CoordinationMode::Off) {
        return SemanticConflict::Clear;
    }
    let mut own_claims = registry.claims.iter().filter(|claim| {
        claim.session_id == principal.session_id
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
        claim.session_id != principal.session_id
            && claim.state == "active"
            && claim.expires_at_epoch >= now
    }) {
        let Some(peer_broker) = registry.brokers.get(&peer.session_id) else {
            incomplete = true;
            continue;
        };
        if peer_broker.coordination_mode == CoordinationMode::Off {
            continue;
        }
        let peer_fresh = peer_broker.session_id == peer.session_id
            && peer_broker.incarnation == peer.session_incarnation
            && peer_broker.state == "ready"
            && coordination_projection::heartbeat_fresh(
                &snapshot.state_root,
                &peer.session_id,
                &peer_broker.incarnation,
                now,
            );
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
        let _ = request;
        SemanticConflict::Clear
    }
}

pub fn classify(
    request: &NormalizedRequest,
    legacy_ttl_seconds: u64,
    projection: &DispatchProjection<'_, '_>,
) -> OwnerLiveness {
    if projection.is_unmanaged() {
        return result(
            LivenessClass::Unclaimed,
            "clear",
            DecisionAction::Allow,
            "coordination-unmanaged",
            Some(false),
        );
    }
    let mode = projection.mode;
    if mode == Some(CoordinationMode::Off) {
        return result(
            LivenessClass::Unclaimed,
            "clear",
            DecisionAction::Allow,
            "coordination-off",
            Some(false),
        );
    }
    let outcome = if matches!(request.semantic_conflict, Some(SemanticConflict::Definite)) {
        result(
            LivenessClass::Active,
            "definite",
            DecisionAction::Block,
            "semantic-conflict-definite",
            None,
        )
    } else {
        let potential = matches!(request.semantic_conflict, Some(SemanticConflict::Potential))
            .then(|| {
                result(
                    LivenessClass::Unknown,
                    "potential",
                    DecisionAction::Warn,
                    "semantic-conflict-potential",
                    None,
                )
            });

        let path_outcomes = request
            .binding_roots
            .iter()
            .map(|root| match projection.snapshot {
                Some(snapshot) => classify_registry_root(request, root, snapshot, projection),
                None => legacy_classify(root, legacy_ttl_seconds, projection.root_inputs(root)),
            });
        strongest_outcome(potential.into_iter().chain(path_outcomes))
            .unwrap_or_else(|| unknown("owner-target-unavailable"))
    };
    apply_mode(outcome, mode)
}

fn apply_mode(mut outcome: OwnerLiveness, mode: Option<CoordinationMode>) -> OwnerLiveness {
    if mode == Some(CoordinationMode::Advisory) && outcome.action == DecisionAction::Block {
        outcome.action = DecisionAction::Warn;
    }
    outcome
}

fn strongest_outcome(outcomes: impl Iterator<Item = OwnerLiveness>) -> Option<OwnerLiveness> {
    outcomes.reduce(|strongest, candidate| {
        if outcome_rank(&candidate) > outcome_rank(&strongest) {
            candidate
        } else {
            strongest
        }
    })
}

fn outcome_rank(outcome: &OwnerLiveness) -> (u8, u8) {
    let specificity = if outcome.reason_code == "owner-active-foreign" {
        5
    } else {
        match outcome.classification {
            LivenessClass::Active => 4,
            LivenessClass::Orphaned => 3,
            LivenessClass::Stale => 2,
            LivenessClass::Unknown => 1,
            LivenessClass::Unclaimed => 0,
        }
    };
    (action_rank(outcome.action), specificity)
}

fn classify_registry_root(
    request: &NormalizedRequest,
    checkout: &Path,
    snapshot: &Snapshot,
    projection: &DispatchProjection<'_, '_>,
) -> OwnerLiveness {
    let Some(fingerprint) = coordination_projection::worktree_fingerprint(
        snapshot.registry.fingerprint_epoch,
        &snapshot.registry.fingerprint_key,
        checkout,
    ) else {
        return unknown("coordination-registry-invalid");
    };
    let now = projection.now;
    let mut matching = snapshot.registry.claims.iter().filter(|claim| {
        claim.state == "active"
            && claim.expires_at_epoch >= now
            && claim.worktrees.iter().any(|value| value == &fingerprint)
            && snapshot
                .registry
                .brokers
                .get(&claim.session_id)
                .is_none_or(|broker| broker.coordination_mode != CoordinationMode::Off)
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
    let dirty = projection.root_inputs(checkout).dirty;
    let own = projection.principal.as_ref().is_some_and(|principal| {
        principal.session_id == claim.session_id
            && principal.incarnation == claim.session_incarnation
    });
    let broker = snapshot
        .registry
        .brokers
        .get(&claim.session_id)
        .filter(|broker| {
            broker.session_id == claim.session_id
                && broker.incarnation == claim.session_incarnation
                && broker.state == "ready"
        });
    match broker {
        Some(broker)
            if coordination_projection::heartbeat_fresh(
                &snapshot.state_root,
                &claim.session_id,
                &broker.incarnation,
                now,
            ) =>
        {
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

fn effective_mode(
    snapshot: Option<&Snapshot>,
    principal: Option<&CurrentPrincipal>,
    now: i64,
    io: &dyn LivenessIo,
) -> Option<CoordinationMode> {
    let principal = principal?;
    let state_root = snapshot
        .map(|snapshot| snapshot.state_root.clone())
        .or_else(|| agent_session_state_root().ok())?;
    let broker =
        snapshot.and_then(|snapshot| broker_for_session(snapshot, &principal.session_id, now));
    trusted_runtime_mode(&state_root, broker, principal, io)
}

fn trusted_runtime_mode(
    state_root: &Path,
    broker: Option<&coordination_projection::BrokerProjection>,
    principal: &CurrentPrincipal,
    io: &dyn LivenessIo,
) -> Option<CoordinationMode> {
    if broker.is_some_and(|broker| {
        broker.session_id != principal.session_id || broker.incarnation != principal.incarnation
    }) {
        return None;
    }
    let mode = io
        .session_coordination_mode(state_root, &principal.session_id, &principal.incarnation)
        .ok()?;
    if broker.is_some_and(|broker| broker.coordination_mode != mode) {
        return None;
    }
    match runtime_mode_hint() {
        RuntimeModeHint::Missing => {}
        RuntimeModeHint::Valid(hint) if hint == mode => {}
        RuntimeModeHint::Valid(_) | RuntimeModeHint::Invalid => return None,
    }
    Some(mode)
}

fn broker_for_session<'a>(
    snapshot: &'a Snapshot,
    session_id: &str,
    now: i64,
) -> Option<&'a coordination_projection::BrokerProjection> {
    snapshot.registry.brokers.get(session_id).filter(|broker| {
        broker.session_id == session_id
            && broker.state == "ready"
            && coordination_projection::heartbeat_fresh(
                &snapshot.state_root,
                session_id,
                &broker.incarnation,
                now,
            )
    })
}

fn legacy_classify(_checkout: &Path, legacy_ttl_seconds: u64, inputs: RootInputs) -> OwnerLiveness {
    let dirty = inputs.dirty;
    let age = inputs.age_seconds;
    if age.is_some_and(|age| age > legacy_ttl_seconds.min(900)) {
        stale_or_dirty(dirty, concat!("leg", "acy-owner-stale"))
    } else {
        result(
            LivenessClass::Unknown,
            "unknown",
            DecisionAction::Block,
            concat!("leg", "acy-owner-unknown"),
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

fn scopes_overlap(
    left: &[coordination_projection::ScopeProjection],
    right: &[coordination_projection::ScopeProjection],
) -> bool {
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

fn action_rank(action: DecisionAction) -> u8 {
    match action {
        DecisionAction::Allow => 0,
        DecisionAction::Warn => 1,
        DecisionAction::Context => 2,
        DecisionAction::Transform => 3,
        DecisionAction::Block => 4,
    }
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

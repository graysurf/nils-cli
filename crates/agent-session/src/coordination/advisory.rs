use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::{
    CoordinationMode, WorkContextAcknowledgeArgs, WorkContextAdviseArgs, WorkContextClearArgs,
    WorkContextSetArgs, WorkContextStatusArgs,
};
use crate::{CliContext, CliError, SessionRecord, load_session_record};

use super::claims;
use super::context::{
    ConflictClassification, ProviderRef, Scope, ScopeKind, WORK_CONTEXT_INPUT_VERSION,
    WORK_CONTEXT_VERSION, WorkContextInput, WorkContextRecord, canonical_repository,
    canonicalize_provider_refs, canonicalize_targets, checkout_root, evaluate,
    repository_for_checkout, repository_for_checkout_with_timeout,
};
use super::{
    Registry, authenticate_from_file, broker, clean_expired, incarnation, lock_registry, now_epoch,
    request_digest, timestamp, worktree_fingerprint,
};

const STATUS_VERSION: &str = "agent-session.work-context-status.v1";
const ADVISORY_VERSION: &str = "agent-session.work-context-advisory.v1";
const ACK_VERSION: &str = "agent-session.work-context-acknowledgement.v1";
const OPERATION_TARGETS_VERSION: &str = "agent-session.operation-targets.v1";
const MAX_ACKNOWLEDGEMENT_SECS: u64 = 8 * 60 * 60;
const MAX_ADVISORY_PEERS: usize = 64;
const ADVISORY_RESOLUTION_TIMEOUT: Duration = Duration::from_millis(750);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct AdvisoryAcknowledgement {
    pub session_incarnation: String,
    #[serde(default)]
    pub advisory_digest: String,
    pub expires_at: String,
    pub expires_at_epoch: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct AdvisoryObservation {
    pub session_incarnation: String,
    pub advisory_digest: String,
    pub observed_at_epoch: i64,
}

#[derive(Clone, Debug, Serialize)]
struct PresenceStatus {
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    repository: Option<String>,
    worktree_known: bool,
}

#[derive(Clone, Debug, Serialize)]
struct AdvisoryReason {
    code: String,
    peer_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    repository: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct AdvisoryPeer {
    session_id: String,
    mode: CoordinationMode,
    declared_context: bool,
}

struct AdvisoryEvaluation {
    available: bool,
    severity: &'static str,
    reasons: Vec<AdvisoryReason>,
    peers: Vec<AdvisoryPeer>,
    digest: String,
}

#[derive(Clone)]
struct PresenceResolution {
    repository: Option<String>,
    worktree: String,
}

struct PresenceResolver {
    deadline: Instant,
    by_checkout: BTreeMap<PathBuf, PresenceResolution>,
    complete: bool,
}

impl PresenceResolver {
    fn new() -> Self {
        Self {
            deadline: Instant::now() + ADVISORY_RESOLUTION_TIMEOUT,
            by_checkout: BTreeMap::new(),
            complete: true,
        }
    }

    fn resolve(&mut self, registry: &Registry, cwd: &Path) -> Result<PresenceResolution, CliError> {
        let checkout = checkout_root(cwd).ok();
        let fingerprint_path = checkout.as_deref().unwrap_or(cwd);
        if let Some(resolution) = self.by_checkout.get(fingerprint_path) {
            return Ok(resolution.clone());
        }
        let repository = if let Some(root) = checkout.as_deref() {
            match self.deadline.checked_duration_since(Instant::now()) {
                Some(remaining) if !remaining.is_zero() => {
                    let repository = repository_for_checkout_with_timeout(root, remaining);
                    if repository.is_none() && Instant::now() >= self.deadline {
                        self.complete = false;
                    }
                    repository
                }
                Some(_) | None => {
                    self.complete = false;
                    None
                }
            }
        } else {
            None
        };
        let resolution = PresenceResolution {
            repository,
            worktree: worktree_fingerprint(registry, fingerprint_path)?,
        };
        self.by_checkout
            .insert(fingerprint_path.to_path_buf(), resolution.clone());
        Ok(resolution)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdvisoryTargetsInput {
    schema_version: String,
    #[serde(default)]
    targets: Vec<Scope>,
    #[serde(default)]
    provider_refs: Vec<ProviderRef>,
    #[serde(default, rename = "checkouts")]
    _checkouts: Vec<Value>,
    #[serde(default, rename = "descendant")]
    _descendant: Option<Value>,
}

pub(crate) fn status(
    context: &CliContext,
    _args: WorkContextStatusArgs,
) -> Result<Value, CliError> {
    let Some((record, incarnation)) = authenticate_self(context)? else {
        return Ok(json!({
            "schema_version": STATUS_VERSION,
            "managed": false,
            "mode": CoordinationMode::Off,
            "presence": { "state": "unmanaged", "worktree_known": false },
            "context": Value::Null,
            "acknowledged_until": Value::Null,
        }));
    };
    let locked = lock_registry(context)?;
    let declared = active_claim(&locked.registry, &record.id, &incarnation)
        .map(claims::public_context)
        .transpose()?;
    let acknowledgement = locked
        .registry
        .advisory_acknowledgements
        .get(&record.id)
        .filter(|ack| ack.session_incarnation == incarnation && ack.expires_at_epoch > now_epoch())
        .map(|ack| ack.expires_at.clone());
    Ok(json!({
        "schema_version": STATUS_VERSION,
        "managed": true,
        "mode": record.coordination_mode,
        "presence": presence_status(&locked.registry, &record),
        "context": declared,
        "acknowledged_until": acknowledgement,
    }))
}

pub(crate) fn set(context: &CliContext, args: WorkContextSetArgs) -> Result<Value, CliError> {
    let (record, incarnation) = require_self(context)?;
    let checkout = checkout_root(Path::new(&record.cwd))?;
    let repository = args
        .repository
        .map(canonical_repository)
        .transpose()?
        .or_else(|| repository_for_checkout(&checkout))
        .ok_or_else(|| {
            CliError::data(
                "repository-unavailable",
                "the current checkout origin could not be resolved; pass --repository owner/repo",
                None,
            )
        })?;
    let mut scopes = Vec::new();
    for path in args.paths {
        let prefix = path.ends_with('/');
        scopes.push(Scope {
            kind: if prefix {
                ScopeKind::PathPrefix
            } else {
                ScopeKind::PathExact
            },
            repository: repository.clone(),
            value: path.trim_end_matches('/').to_string(),
        });
    }
    let mut provider_refs = Vec::new();
    provider_refs.extend(args.issue.into_iter().map(|number| ProviderRef {
        kind: "issue".to_string(),
        repository: repository.clone(),
        number,
    }));
    provider_refs.extend(args.pr.into_iter().map(|number| ProviderRef {
        kind: "pr".to_string(),
        repository: repository.clone(),
        number,
    }));
    let candidate = WorkContextInput {
        schema_version: WORK_CONTEXT_INPUT_VERSION.to_string(),
        intent: args.intent,
        tier: args.tier,
        repositories: vec![repository],
        worktrees: Vec::new(),
        provider_refs,
        plan_refs: args.plan_refs,
        scopes,
        summary: args
            .summary
            .unwrap_or_else(|| "managed agent session".to_string()),
    };
    let mut value = claims::set_declared(
        context,
        &record,
        &incarnation,
        candidate,
        record.coordination_mode == CoordinationMode::Enforce,
        args.if_absent,
    )?;
    value
        .as_object_mut()
        .expect("set result is an object")
        .insert("mode".to_string(), json!(record.coordination_mode));
    Ok(value)
}

pub(crate) fn clear(context: &CliContext, _args: WorkContextClearArgs) -> Result<Value, CliError> {
    let (record, incarnation) = require_self(context)?;
    let mut value = claims::clear_declared(context, &record, &incarnation)?;
    value
        .as_object_mut()
        .expect("clear result is an object")
        .insert("mode".to_string(), json!(record.coordination_mode));
    Ok(value)
}

pub(crate) fn advise(context: &CliContext, args: WorkContextAdviseArgs) -> Result<Value, CliError> {
    let Some((record, self_incarnation, registry)) = authenticate_self_with_registry(context)?
    else {
        return Ok(unmanaged_advisory());
    };
    if record.coordination_mode == CoordinationMode::Off {
        return Ok(json!({
            "schema_version": ADVISORY_VERSION,
            "managed": true,
            "mode": CoordinationMode::Off,
            "available": true,
            "severity": "none",
            "suppressed": false,
            "reasons": [],
            "peers": [],
        }));
    }

    let evaluation = evaluate_advisory(
        context,
        &registry,
        &record,
        &self_incarnation,
        args.targets_file.as_deref(),
    )?;
    let snapshot_observation = registry.advisory_observations.get(&record.id).cloned();
    let observed_now = now_epoch();
    let suppressed = !evaluation.reasons.is_empty()
        && registry
            .advisory_acknowledgements
            .get(&record.id)
            .is_some_and(|ack| {
                ack.session_incarnation == self_incarnation
                    && ack.expires_at_epoch > observed_now
                    && ack.advisory_digest == evaluation.digest
            });
    let observation_changed = if evaluation.reasons.is_empty() {
        snapshot_observation.is_some()
    } else {
        let observation = AdvisoryObservation {
            session_incarnation: self_incarnation.clone(),
            advisory_digest: evaluation.digest.clone(),
            observed_at_epoch: observed_now,
        };
        snapshot_observation.as_ref().is_none_or(|prior| {
            prior.session_incarnation != observation.session_incarnation
                || prior.advisory_digest != observation.advisory_digest
        })
    };
    if observation_changed {
        let now = now_epoch();
        let mut locked = lock_registry(context)?;
        let mut changed = clean_expired(&mut locked.registry, now);
        let broker_matches = locked
            .registry
            .brokers
            .get(&record.id)
            .is_some_and(|broker| {
                broker.state == "ready" && broker.incarnation == self_incarnation
            });
        let observation_matches =
            locked.registry.advisory_observations.get(&record.id) == snapshot_observation.as_ref();
        if broker_matches && observation_matches {
            if evaluation.reasons.is_empty() {
                if snapshot_observation
                    .as_ref()
                    .is_some_and(|observation| observation.session_incarnation == self_incarnation)
                {
                    locked.registry.advisory_observations.remove(&record.id);
                    changed = true;
                }
            } else if snapshot_observation
                .as_ref()
                .is_none_or(|observation| observation.session_incarnation == self_incarnation)
            {
                locked.registry.advisory_observations.insert(
                    record.id.clone(),
                    AdvisoryObservation {
                        session_incarnation: self_incarnation.clone(),
                        advisory_digest: evaluation.digest.clone(),
                        observed_at_epoch: now,
                    },
                );
                changed = true;
            }
        }
        if changed {
            locked.save()?;
        }
    }
    Ok(json!({
        "schema_version": ADVISORY_VERSION,
        "managed": true,
        "mode": record.coordination_mode,
        "available": evaluation.available,
        "severity": evaluation.severity,
        "suppressed": suppressed,
        "reasons": evaluation.reasons,
        "peers": evaluation.peers,
    }))
}

fn evaluate_advisory(
    context: &CliContext,
    registry: &Registry,
    record: &SessionRecord,
    self_incarnation: &str,
    targets_file: Option<&Path>,
) -> Result<AdvisoryEvaluation, CliError> {
    let mut resolver = PresenceResolver::new();
    let mut candidate = presence_context(registry, record, self_incarnation, &mut resolver)?;
    if let Some(path) = targets_file {
        merge_targets(&mut candidate, path)?;
    }
    let mut peer_records = Vec::new();
    let mut peer_modes = BTreeMap::new();
    let mut peer_incarnations = BTreeMap::new();
    let mut complete = true;
    let mut relevant_peers = 0_usize;
    for peer_broker in registry.brokers.values() {
        if peer_broker.session_id == record.id && peer_broker.incarnation == self_incarnation {
            continue;
        }
        if peer_broker.state == "stopped" {
            continue;
        }
        if relevant_peers >= MAX_ADVISORY_PEERS {
            complete = false;
            break;
        }
        relevant_peers = relevant_peers.saturating_add(1);
        let Ok(peer_record) = load_session_record(context, &peer_broker.session_id) else {
            complete = false;
            continue;
        };
        if !incarnation(&peer_record)
            .is_ok_and(|incarnation| incarnation == peer_broker.incarnation)
        {
            complete = false;
            continue;
        }
        if peer_record.coordination_mode == CoordinationMode::Off {
            continue;
        }
        if peer_broker.state != "ready"
            || !broker::capability_available(
                context,
                &peer_broker.session_id,
                &peer_broker.incarnation,
                &peer_broker.capability_digest,
            )
            || !broker::heartbeat_fresh(
                context,
                &peer_broker.session_id,
                &peer_broker.incarnation,
                peer_broker.heartbeat_epoch,
            )
        {
            complete = false;
            continue;
        }
        match presence_record(
            registry,
            &peer_record,
            &peer_broker.incarnation,
            &mut resolver,
        ) {
            Ok(peer) => {
                peer_modes.insert(
                    peer_broker.session_id.clone(),
                    peer_record.coordination_mode,
                );
                peer_incarnations.insert(
                    peer_broker.session_id.clone(),
                    peer_broker.incarnation.clone(),
                );
                peer_records.push(peer);
            }
            Err(_) => complete = false,
        }
    }
    complete &= resolver.complete;
    let evaluation = evaluate(
        Some((&record.id, self_incarnation)),
        &candidate,
        &peer_records,
        complete,
        true,
    );
    let mut reasons = evaluation
        .reasons
        .into_iter()
        .map(|reason| AdvisoryReason {
            code: if reason.code == "same-repository-incomplete-scope" {
                "same-repository".to_string()
            } else {
                reason.code
            },
            peer_session_id: reason.peer_session_id,
            repository: reason.repository,
        })
        .collect::<Vec<_>>();
    reasons.sort_by(|left, right| {
        reason_rank(&left.code)
            .cmp(&reason_rank(&right.code))
            .then_with(|| left.peer_session_id.cmp(&right.peer_session_id))
            .then_with(|| left.code.cmp(&right.code))
    });
    reasons.dedup_by(|left, right| {
        left.code == right.code
            && left.peer_session_id == right.peer_session_id
            && left.repository == right.repository
    });
    let peer_ids = reasons
        .iter()
        .map(|reason| reason.peer_session_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut peers = peer_records
        .iter()
        .filter(|peer| peer_ids.contains(peer.session_id.as_str()))
        .map(|peer| AdvisoryPeer {
            session_id: peer.session_id.clone(),
            mode: peer_modes
                .get(&peer.session_id)
                .copied()
                .unwrap_or_default(),
            declared_context: !peer.claim_id.is_empty(),
        })
        .collect::<Vec<_>>();
    peers.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    peers.dedup_by(|left, right| left.session_id == right.session_id);
    let severity = advisory_severity(&evaluation.classification, &reasons, complete);
    // Acknowledgement follows the stable overlap identity, not each individual
    // operation path. Target changes that alter a reason or repository change
    // this signature; per-file churn inside the same known overlap does not.
    let digest = request_digest(
        "work-context-advisory-observation",
        &json!({
            "available": complete,
            "severity": severity,
            "reasons": &reasons,
            "peers": &peers,
            "peer_incarnations": peer_incarnations,
        }),
    );
    Ok(AdvisoryEvaluation {
        available: complete,
        severity,
        reasons,
        peers,
        digest,
    })
}

pub(crate) fn acknowledge(
    context: &CliContext,
    args: WorkContextAcknowledgeArgs,
) -> Result<Value, CliError> {
    let (record, session_incarnation) = require_self(context)?;
    let seconds = parse_duration(&args.duration)?;
    if seconds == 0 || seconds > MAX_ACKNOWLEDGEMENT_SECS {
        return Err(CliError::usage(
            "invalid-acknowledgement-duration",
            "advisory acknowledgement must be between 1 second and 8 hours",
            None,
        ));
    }
    let now = now_epoch();
    let expires_at_epoch = now.saturating_add(seconds as i64);
    let registry = registry_snapshot(context)?;
    let fallback_digest =
        evaluate_advisory(context, &registry, &record, &session_incarnation, None)?.digest;
    let mut locked = lock_registry(context)?;
    clean_expired(&mut locked.registry, now);
    let advisory_digest = if let Some(observation) = locked
        .registry
        .advisory_observations
        .get(&record.id)
        .filter(|observation| observation.session_incarnation == session_incarnation)
    {
        observation.advisory_digest.clone()
    } else {
        fallback_digest
    };
    let acknowledgement = AdvisoryAcknowledgement {
        session_incarnation: session_incarnation.clone(),
        advisory_digest,
        expires_at: timestamp(expires_at_epoch),
        expires_at_epoch,
    };
    locked
        .registry
        .advisory_acknowledgements
        .insert(record.id.clone(), acknowledgement.clone());
    locked.save()?;
    Ok(json!({
        "schema_version": ACK_VERSION,
        "session_id": record.id,
        "expires_at": acknowledgement.expires_at,
        "mode": record.coordination_mode,
    }))
}

fn registry_snapshot(context: &CliContext) -> Result<Registry, CliError> {
    let mut locked = lock_registry(context)?;
    if clean_expired(&mut locked.registry, now_epoch()) {
        locked.save()?;
    }
    Ok(locked.registry.clone())
}

fn authenticate_self(context: &CliContext) -> Result<Option<(SessionRecord, String)>, CliError> {
    let Some(session_id) = std::env::var("AGENT_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    authenticate_from_file(context, &session_id, None).map(Some)
}

fn authenticate_self_with_registry(
    context: &CliContext,
) -> Result<Option<(SessionRecord, String, Registry)>, CliError> {
    let Some(session_id) = std::env::var("AGENT_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    let capability_path = std::env::var_os(super::CAPABILITY_ENV)
        .map(std::path::PathBuf::from)
        .ok_or_else(super::unauthorized)?;
    let token =
        super::read_private_file(&capability_path, 512).map_err(|_| super::unauthorized())?;
    let token = String::from_utf8(token).map_err(|_| super::unauthorized())?;
    let token = token.trim();
    if token.len() < 32 || token.len() > 256 || !token.is_ascii() {
        return Err(super::unauthorized());
    }
    let record = load_session_record(context, &session_id).map_err(|_| super::unauthorized())?;
    let session_incarnation = incarnation(&record)?;
    let mut locked = lock_registry(context)?;
    let cleaned = clean_expired(&mut locked.registry, now_epoch());
    let broker = locked
        .registry
        .brokers
        .get(&record.id)
        .ok_or_else(super::unauthorized)?;
    if broker.state != "ready"
        || broker.incarnation != session_incarnation
        || !super::digest_eq(
            &broker.capability_digest,
            &super::digest_bytes(token.as_bytes()),
        )
        || !broker::capability_available(
            context,
            &record.id,
            &session_incarnation,
            &broker.capability_digest,
        )
    {
        return Err(super::unauthorized());
    }
    if !broker::heartbeat_fresh(
        context,
        &record.id,
        &session_incarnation,
        broker.heartbeat_epoch,
    ) {
        return Err(CliError::runtime(
            "coordination-broker-lost",
            "coordination broker heartbeat is stale",
            None,
        ));
    }
    let registry = locked.registry.clone();
    if cleaned {
        locked.save()?;
    }
    Ok(Some((record, session_incarnation, registry)))
}

fn require_self(context: &CliContext) -> Result<(SessionRecord, String), CliError> {
    authenticate_self(context)?.ok_or_else(|| {
        CliError::usage(
            "session-not-managed",
            "this command requires an agent-session managed runtime",
            None,
        )
    })
}

fn active_claim<'a>(
    registry: &'a Registry,
    session_id: &str,
    session_incarnation: &str,
) -> Option<&'a WorkContextRecord> {
    registry.claims.iter().find(|claim| {
        claim.session_id == session_id
            && claim.session_incarnation == session_incarnation
            && claim.state == "active"
    })
}

fn presence_status(registry: &Registry, record: &SessionRecord) -> PresenceStatus {
    let root = checkout_root(Path::new(&record.cwd)).ok();
    PresenceStatus {
        state: "active".to_string(),
        repository: root.as_deref().and_then(repository_for_checkout),
        worktree_known: root
            .as_deref()
            .and_then(|path| worktree_fingerprint(registry, path).ok())
            .is_some(),
    }
}

fn presence_context(
    registry: &Registry,
    record: &SessionRecord,
    session_incarnation: &str,
    resolver: &mut PresenceResolver,
) -> Result<WorkContextInput, CliError> {
    let mut context = active_claim(registry, &record.id, session_incarnation)
        .map(claims::input_from_record)
        .unwrap_or_else(|| WorkContextInput {
            schema_version: WORK_CONTEXT_INPUT_VERSION.to_string(),
            intent: "presence".to_string(),
            tier: "L0".to_string(),
            repositories: Vec::new(),
            worktrees: Vec::new(),
            provider_refs: Vec::new(),
            plan_refs: Vec::new(),
            scopes: Vec::new(),
            summary: "managed agent session".to_string(),
        });
    let resolution = resolver.resolve(registry, Path::new(&record.cwd))?;
    if let Some(repository) = resolution.repository
        && !context.repositories.contains(&repository)
    {
        context.repositories.push(repository);
        context.repositories.sort();
    }
    if !context.worktrees.contains(&resolution.worktree) {
        context.worktrees.push(resolution.worktree);
        context.worktrees.sort();
    }
    Ok(context)
}

fn presence_record(
    registry: &Registry,
    record: &SessionRecord,
    session_incarnation: &str,
    resolver: &mut PresenceResolver,
) -> Result<WorkContextRecord, CliError> {
    if let Some(claim) = active_claim(registry, &record.id, session_incarnation) {
        let mut claim = claim.clone();
        let presence = presence_context(registry, record, session_incarnation, resolver)?;
        claim.repositories = presence.repositories;
        claim.worktrees = presence.worktrees;
        return Ok(claim);
    }
    let presence = presence_context(registry, record, session_incarnation, resolver)?;
    Ok(WorkContextRecord {
        schema_version: WORK_CONTEXT_VERSION.to_string(),
        session_id: record.id.clone(),
        session_incarnation: session_incarnation.to_string(),
        claim_id: String::new(),
        revision: 0,
        state: "active".to_string(),
        intent: presence.intent,
        tier: presence.tier,
        repositories: presence.repositories,
        worktrees: presence.worktrees,
        checkout_shell_grant: false,
        provider_refs: presence.provider_refs,
        plan_refs: presence.plan_refs,
        scopes: presence.scopes,
        summary: String::new(),
        updated_at: String::new(),
        expires_at: String::new(),
        expires_at_epoch: i64::MAX,
        terminal_at_epoch: None,
    })
}

fn merge_targets(candidate: &mut WorkContextInput, path: &Path) -> Result<(), CliError> {
    let input: AdvisoryTargetsInput =
        super::read_bounded_json(path, 16 * 1024, "invalid-operation-targets")?;
    if input.schema_version != OPERATION_TARGETS_VERSION {
        return Err(CliError::data(
            "invalid-operation-targets",
            "operation targets schema_version is unsupported",
            None,
        ));
    }
    let targets = canonicalize_targets(input.targets)?;
    let provider_refs = canonicalize_provider_refs(input.provider_refs)?;
    for target in targets {
        if !candidate.repositories.contains(&target.repository) {
            candidate.repositories.push(target.repository.clone());
        }
        if !candidate.scopes.contains(&target) {
            candidate.scopes.push(target);
        }
    }
    for reference in provider_refs {
        if !candidate.repositories.contains(&reference.repository) {
            candidate.repositories.push(reference.repository.clone());
        }
        if !candidate.provider_refs.contains(&reference) {
            candidate.provider_refs.push(reference);
        }
    }
    candidate.repositories.sort();
    candidate.scopes.sort();
    candidate.provider_refs.sort();
    Ok(())
}

fn reason_rank(code: &str) -> u8 {
    match code {
        "same-worktree" | "same-provider-ref" | "same-plan-ref" | "overlapping-scope" => 0,
        "same-repository" => 1,
        _ => 2,
    }
}

fn advisory_severity(
    classification: &ConflictClassification,
    reasons: &[AdvisoryReason],
    complete: bool,
) -> &'static str {
    if reasons.iter().any(|reason| reason_rank(&reason.code) == 0)
        || *classification == ConflictClassification::Conflict
    {
        "warning"
    } else if !reasons.is_empty() {
        "info"
    } else if !complete || *classification == ConflictClassification::Unknown {
        "degraded"
    } else {
        "none"
    }
}

fn parse_duration(value: &str) -> Result<u64, CliError> {
    let value = value.trim();
    let (digits, multiplier) = if let Some(value) = value.strip_suffix('s') {
        (value, 1)
    } else if let Some(value) = value.strip_suffix('m') {
        (value, 60)
    } else if let Some(value) = value.strip_suffix('h') {
        (value, 60 * 60)
    } else {
        (value, 1)
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(multiplier))
        .ok_or_else(|| {
            CliError::usage(
                "invalid-acknowledgement-duration",
                "duration must be a positive integer with optional s, m, or h suffix",
                None,
            )
        })
}

fn unmanaged_advisory() -> Value {
    json!({
        "schema_version": ADVISORY_VERSION,
        "managed": false,
        "mode": CoordinationMode::Off,
        "available": false,
        "severity": "none",
        "suppressed": false,
        "reasons": [],
        "peers": [],
    })
}

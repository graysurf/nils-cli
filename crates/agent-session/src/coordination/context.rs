use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::CliError;

pub const WORK_CONTEXT_INPUT_VERSION: &str = "agent-session.work-context-input.v1";
pub const WORK_CONTEXT_VERSION: &str = "agent-session.work-context.v1";
pub const CONFLICT_EVALUATION_VERSION: &str = "agent-session.conflict-evaluation.v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum ScopeKind {
    Repository,
    PathExact,
    PathPrefix,
    Capability,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub kind: ScopeKind,
    pub repository: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ProviderRef {
    pub kind: String,
    pub repository: String,
    pub number: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkContextInput {
    pub schema_version: String,
    pub intent: String,
    pub tier: String,
    #[serde(default)]
    pub repositories: Vec<String>,
    #[serde(default)]
    pub worktrees: Vec<String>,
    #[serde(default)]
    pub provider_refs: Vec<ProviderRef>,
    #[serde(default)]
    pub plan_refs: Vec<String>,
    #[serde(default)]
    pub scopes: Vec<Scope>,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkContextRecord {
    pub schema_version: String,
    pub session_id: String,
    pub session_incarnation: String,
    pub claim_id: String,
    pub revision: u64,
    pub state: String,
    pub intent: String,
    pub tier: String,
    pub repositories: Vec<String>,
    pub worktrees: Vec<String>,
    pub provider_refs: Vec<ProviderRef>,
    pub plan_refs: Vec<String>,
    pub scopes: Vec<Scope>,
    pub summary: String,
    pub updated_at: String,
    pub expires_at: String,
    pub expires_at_epoch: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ConflictClassification {
    Conflict,
    PotentialConflict,
    Unknown,
    NoKnownConflict,
    Clear,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConflictReason {
    pub code: String,
    pub peer_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PeerProjection {
    pub session_id: String,
    pub claim_id: String,
    pub intent: String,
    pub tier: String,
    pub repositories: Vec<String>,
    pub provider_refs: Vec<ProviderRef>,
    pub plan_refs: Vec<String>,
    pub scopes: Vec<Scope>,
    pub summary: String,
    pub expires_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConflictEvaluation {
    pub schema_version: String,
    pub classification: ConflictClassification,
    pub complete: bool,
    pub reasons: Vec<ConflictReason>,
    pub peers: Vec<PeerProjection>,
}

impl WorkContextInput {
    pub fn validate_and_canonicalize(mut self) -> Result<Self, CliError> {
        if self.schema_version != WORK_CONTEXT_INPUT_VERSION {
            return Err(CliError::data(
                "unsupported-work-context-version",
                "unsupported work-context input schema",
                Some(json!({ "schema_version": self.schema_version })),
            ));
        }
        self.intent = bounded_text("intent", self.intent, 64)?;
        self.tier = bounded_text("tier", self.tier, 16)?;
        if !matches!(self.tier.as_str(), "L0" | "L1" | "L2" | "L3") {
            return Err(invalid_context("tier must be L0, L1, L2, or L3"));
        }
        self.summary = bounded_text("summary", self.summary, 240)?;
        canonicalize_unique(&mut self.repositories, 16, canonical_repository)?;
        canonicalize_unique(&mut self.worktrees, 16, canonical_worktree)?;
        canonicalize_unique(&mut self.plan_refs, 32, canonical_plan_ref)?;
        if self.provider_refs.len() > 32 || self.scopes.len() > 64 {
            return Err(invalid_context("work context exceeds collection limits"));
        }
        for reference in &mut self.provider_refs {
            reference.kind = bounded_slug("provider reference kind", &reference.kind, 32)?;
            reference.repository = canonical_repository(reference.repository.clone())?;
            if reference.number == 0 {
                return Err(invalid_context(
                    "provider reference number must be positive",
                ));
            }
        }
        self.provider_refs.sort();
        reject_duplicates(&self.provider_refs, "provider reference")?;
        for scope in &mut self.scopes {
            scope.repository = canonical_repository(scope.repository.clone())?;
            scope.value = match scope.kind {
                ScopeKind::Repository => {
                    if !scope.value.trim().is_empty() {
                        return Err(invalid_scope("repository scope value must be empty"));
                    }
                    String::new()
                }
                ScopeKind::PathExact => canonical_relative_path(&scope.value, false)?,
                ScopeKind::PathPrefix => canonical_relative_path(&scope.value, true)?,
                ScopeKind::Capability => bounded_slug("capability scope", &scope.value, 64)?,
            };
            if !self.repositories.contains(&scope.repository) {
                return Err(invalid_scope(
                    "scope repository is not declared by the context",
                ));
            }
        }
        self.scopes.sort();
        reject_duplicates(&self.scopes, "scope")?;
        Ok(self)
    }
}

pub fn evaluate(
    candidate_session: &str,
    candidate_incarnation: &str,
    candidate: &WorkContextInput,
    active: &[WorkContextRecord],
    complete_registry: bool,
    allow_incomplete: bool,
) -> ConflictEvaluation {
    let mut reasons = Vec::new();
    let mut peers = Vec::new();
    let mut potential = false;
    for peer in active {
        if peer.session_id == candidate_session && peer.session_incarnation == candidate_incarnation
        {
            continue;
        }
        if peer.state != "active" || peer.schema_version != WORK_CONTEXT_VERSION {
            continue;
        }
        let mut peer_conflict = false;
        let mut peer_potential = false;
        if intersects(&candidate.worktrees, &peer.worktrees) {
            reasons.push(reason("same-worktree", peer, None));
            peer_conflict = true;
        }
        if candidate
            .provider_refs
            .iter()
            .any(|item| peer.provider_refs.contains(item))
        {
            reasons.push(reason("same-provider-ref", peer, None));
            peer_conflict = true;
        }
        if intersects(&candidate.plan_refs, &peer.plan_refs) {
            reasons.push(reason("same-plan-ref", peer, None));
            peer_conflict = true;
        }
        for repository in candidate
            .repositories
            .iter()
            .filter(|repository| peer.repositories.contains(repository))
        {
            let candidate_scopes: Vec<_> = candidate
                .scopes
                .iter()
                .filter(|scope| &scope.repository == repository)
                .collect();
            let peer_scopes: Vec<_> = peer
                .scopes
                .iter()
                .filter(|scope| &scope.repository == repository)
                .collect();
            if candidate_scopes.is_empty() || peer_scopes.is_empty() {
                reasons.push(reason(
                    "same-repository-incomplete-scope",
                    peer,
                    Some(repository.clone()),
                ));
                peer_potential = true;
            } else if candidate_scopes
                .iter()
                .any(|left| peer_scopes.iter().any(|right| scopes_overlap(left, right)))
            {
                reasons.push(reason("overlapping-scope", peer, Some(repository.clone())));
                peer_conflict = true;
            }
        }
        potential |= peer_potential;
        if peer_conflict || peer_potential {
            peers.push(PeerProjection::from(peer));
        }
    }
    reasons.sort();
    reasons.dedup();
    peers.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    peers.dedup_by(|left, right| left.session_id == right.session_id);
    let has_conflict = reasons.iter().any(|reason| {
        matches!(
            reason.code.as_str(),
            "same-worktree" | "same-provider-ref" | "same-plan-ref" | "overlapping-scope"
        )
    });
    let classification = if has_conflict {
        ConflictClassification::Conflict
    } else if potential {
        ConflictClassification::PotentialConflict
    } else if !complete_registry && allow_incomplete {
        ConflictClassification::NoKnownConflict
    } else if !complete_registry {
        ConflictClassification::Unknown
    } else {
        ConflictClassification::Clear
    };
    ConflictEvaluation {
        schema_version: CONFLICT_EVALUATION_VERSION.to_string(),
        classification,
        complete: complete_registry,
        reasons,
        peers,
    }
}

impl From<&WorkContextRecord> for PeerProjection {
    fn from(peer: &WorkContextRecord) -> Self {
        Self {
            session_id: peer.session_id.clone(),
            claim_id: peer.claim_id.clone(),
            intent: peer.intent.clone(),
            tier: peer.tier.clone(),
            repositories: peer.repositories.clone(),
            provider_refs: peer.provider_refs.clone(),
            plan_refs: peer.plan_refs.clone(),
            scopes: peer.scopes.clone(),
            summary: peer.summary.clone(),
            expires_at: peer.expires_at.clone(),
        }
    }
}

fn reason(code: &str, peer: &WorkContextRecord, repository: Option<String>) -> ConflictReason {
    ConflictReason {
        code: code.to_string(),
        peer_session_id: peer.session_id.clone(),
        repository,
    }
}

pub fn scopes_overlap(left: &Scope, right: &Scope) -> bool {
    if left.repository != right.repository {
        return false;
    }
    match (&left.kind, &right.kind) {
        (ScopeKind::Repository, _) | (_, ScopeKind::Repository) => true,
        (ScopeKind::Capability, ScopeKind::Capability) => left.value == right.value,
        (ScopeKind::Capability, _) | (_, ScopeKind::Capability) => false,
        (ScopeKind::PathExact, ScopeKind::PathExact) => left.value == right.value,
        (ScopeKind::PathExact, ScopeKind::PathPrefix) => left.value.starts_with(&right.value),
        (ScopeKind::PathPrefix, ScopeKind::PathExact) => right.value.starts_with(&left.value),
        (ScopeKind::PathPrefix, ScopeKind::PathPrefix) => {
            left.value.starts_with(&right.value) || right.value.starts_with(&left.value)
        }
    }
}

pub fn scope_covers(claim: &Scope, target: &Scope) -> bool {
    if claim.repository != target.repository {
        return false;
    }
    match (&claim.kind, &target.kind) {
        (ScopeKind::Repository, _) => true,
        (ScopeKind::PathExact, ScopeKind::PathExact) => claim.value == target.value,
        (ScopeKind::PathPrefix, ScopeKind::PathExact)
        | (ScopeKind::PathPrefix, ScopeKind::PathPrefix) => target.value.starts_with(&claim.value),
        (ScopeKind::Capability, ScopeKind::Capability) => claim.value == target.value,
        _ => false,
    }
}

pub fn canonicalize_targets(mut targets: Vec<Scope>) -> Result<Vec<Scope>, CliError> {
    if targets.is_empty() || targets.len() > 64 {
        return Err(invalid_scope("operation targets must contain 1-64 scopes"));
    }
    let repositories: BTreeSet<_> = targets
        .iter()
        .map(|target| canonical_repository(target.repository.clone()))
        .collect::<Result<_, _>>()?;
    for target in &mut targets {
        target.repository = canonical_repository(target.repository.clone())?;
        target.value = match target.kind {
            ScopeKind::Repository => {
                if !target.value.trim().is_empty() {
                    return Err(invalid_scope("repository target value must be empty"));
                }
                String::new()
            }
            ScopeKind::PathExact => canonical_relative_path(&target.value, false)?,
            ScopeKind::PathPrefix => canonical_relative_path(&target.value, true)?,
            ScopeKind::Capability => bounded_slug("capability target", &target.value, 64)?,
        };
        if !repositories.contains(&target.repository) {
            return Err(invalid_scope("operation target repository is invalid"));
        }
    }
    targets.sort();
    reject_duplicates(&targets, "operation target")?;
    Ok(targets)
}

fn canonical_repository(value: String) -> Result<String, CliError> {
    let value = value.trim().to_ascii_lowercase();
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    if parts.next().is_some() || !valid_slug_component(owner) || !valid_slug_component(repo) {
        return Err(invalid_context("repository must be canonical owner/name"));
    }
    Ok(format!("{owner}/{repo}"))
}

fn canonical_worktree(value: String) -> Result<String, CliError> {
    let value = value.trim().to_string();
    let mut parts = value.split(':');
    if parts.next() != Some("hmac-sha256")
        || parts.next().is_none_or(|epoch| epoch.is_empty())
        || parts.next().is_none_or(|digest| {
            digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        || parts.next().is_some()
    {
        return Err(invalid_context(
            "worktree fingerprint must use hmac-sha256:epoch:digest",
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn canonical_plan_ref(value: String) -> Result<String, CliError> {
    canonical_relative_path(&value, false)
}

fn canonical_relative_path(value: &str, require_trailing_slash: bool) -> Result<String, CliError> {
    let value = value.trim().replace('\\', "/");
    if value.is_empty()
        || value.starts_with('/')
        || value.starts_with('~')
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        return Err(invalid_scope(
            "path scope must be a safe repository-relative path",
        ));
    }
    let trailing = value.ends_with('/');
    if require_trailing_slash != trailing {
        return Err(invalid_scope(if require_trailing_slash {
            "path-prefix scope must end with /"
        } else {
            "exact path must not end with /"
        }));
    }
    let components: Vec<_> = value.trim_end_matches('/').split('/').collect();
    if components
        .iter()
        .any(|component| component.is_empty() || *component == "." || *component == "..")
    {
        return Err(invalid_scope("path scope contains an invalid component"));
    }
    Ok(if trailing {
        format!("{}/", components.join("/"))
    } else {
        components.join("/")
    })
}

fn canonicalize_unique<F>(
    values: &mut Vec<String>,
    max: usize,
    canonicalize: F,
) -> Result<(), CliError>
where
    F: Fn(String) -> Result<String, CliError>,
{
    if values.len() > max {
        return Err(invalid_context("work context exceeds collection limits"));
    }
    let mut canonical = BTreeSet::new();
    for value in values.drain(..) {
        if !canonical.insert(canonicalize(value)?) {
            return Err(invalid_context("work context contains a duplicate value"));
        }
    }
    values.extend(canonical);
    Ok(())
}

fn reject_duplicates<T: Ord + Clone>(values: &[T], kind: &str) -> Result<(), CliError> {
    let mut seen = BTreeSet::new();
    if values.iter().any(|value| !seen.insert(value.clone())) {
        return Err(invalid_context(&format!(
            "work context contains a duplicate {kind}"
        )));
    }
    Ok(())
}

fn bounded_text(name: &str, value: String, max: usize) -> Result<String, CliError> {
    let value = value.trim().to_string();
    if value.is_empty()
        || value.chars().count() > max
        || value.chars().any(|character| character.is_control())
    {
        return Err(invalid_context(&format!(
            "{name} is empty, too long, or contains controls"
        )));
    }
    Ok(value)
}

fn bounded_slug(name: &str, value: &str, max: usize) -> Result<String, CliError> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty()
        || value.len() > max
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(invalid_context(&format!(
            "{name} must be lower-kebab ASCII"
        )));
    }
    Ok(value)
}

fn valid_slug_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
}

fn intersects<T: Ord + Clone>(left: &[T], right: &[T]) -> bool {
    let right: BTreeSet<_> = right.iter().cloned().collect();
    left.iter().any(|value| right.contains(value))
}

fn invalid_context(message: &str) -> CliError {
    CliError::data("invalid-work-context", message, None)
}

fn invalid_scope(message: &str) -> CliError {
    CliError::data("invalid-scope", message, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(kind: ScopeKind, value: &str) -> Scope {
        Scope {
            kind,
            repository: "example/repo".to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn closed_scope_overlap_is_symmetric_and_boundary_safe() {
        let prefix = scope(ScopeKind::PathPrefix, "src/");
        assert!(scopes_overlap(
            &prefix,
            &scope(ScopeKind::PathExact, "src/lib.rs")
        ));
        assert!(!scopes_overlap(
            &prefix,
            &scope(ScopeKind::PathExact, "src2/lib.rs")
        ));
        assert!(scopes_overlap(
            &scope(ScopeKind::Repository, ""),
            &scope(ScopeKind::Capability, "release")
        ));
    }

    #[test]
    fn a_prefix_claim_covers_only_descendant_targets() {
        let prefix = scope(ScopeKind::PathPrefix, "src/");
        assert!(scope_covers(
            &prefix,
            &scope(ScopeKind::PathExact, "src/lib.rs")
        ));
        assert!(!scope_covers(
            &prefix,
            &scope(ScopeKind::PathExact, "tests/lib.rs")
        ));
    }
}

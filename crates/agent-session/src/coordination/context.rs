use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::CliError;

pub const WORK_CONTEXT_INPUT_VERSION: &str = "agent-session.work-context-input.v1";
pub const WORK_CONTEXT_VERSION: &str = "agent-session.work-context.v1";
pub const CONFLICT_EVALUATION_VERSION: &str = "agent-session.conflict-evaluation.v1";
pub const WORKTREE_FINGERPRINT_EPOCH: u64 = 1;
const GIT_REMOTE_TIMEOUT: Duration = Duration::from_millis(750);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum ScopeKind {
    Repository,
    PathExact,
    PathPrefix,
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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct CheckoutBinding {
    pub repository: String,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_at_epoch: Option<i64>,
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
        canonicalize_unique(&mut self.repositories, 8, canonical_repository)?;
        canonicalize_unique(&mut self.worktrees, 8, canonical_worktree)?;
        canonicalize_unique(&mut self.plan_refs, 16, canonical_plan_ref)?;
        if self.provider_refs.len() > 16 || self.scopes.len() > 32 {
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
                    if scope.value.trim() != "." {
                        return Err(invalid_scope("repository scope value must be ."));
                    }
                    ".".to_string()
                }
                ScopeKind::PathExact => canonical_relative_path(&scope.value, false)?,
                ScopeKind::PathPrefix => canonical_relative_path(&scope.value, false)?,
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
    excluded_principal: Option<(&str, &str)>,
    candidate: &WorkContextInput,
    active: &[WorkContextRecord],
    complete_registry: bool,
    allow_incomplete: bool,
) -> ConflictEvaluation {
    let mut reasons = Vec::new();
    let mut peers = Vec::new();
    let mut potential = false;
    let mut effective_complete = complete_registry;
    for peer in active {
        if excluded_principal.is_some_and(|(session, incarnation)| {
            peer.session_id == session && peer.session_incarnation == incarnation
        }) {
            continue;
        }
        if peer.state != "active" {
            continue;
        }
        if peer.schema_version != WORK_CONTEXT_VERSION {
            effective_complete = false;
            continue;
        }
        let mut peer_conflict = false;
        let mut peer_potential = false;
        let candidate_worktrees_comparable = candidate
            .worktrees
            .iter()
            .all(|fingerprint| fingerprint_epoch(fingerprint) == Some(WORKTREE_FINGERPRINT_EPOCH));
        let peer_worktrees_comparable = peer
            .worktrees
            .iter()
            .all(|fingerprint| fingerprint_epoch(fingerprint) == Some(WORKTREE_FINGERPRINT_EPOCH));
        if !candidate_worktrees_comparable || !peer_worktrees_comparable {
            effective_complete = false;
        } else if intersects(&candidate.worktrees, &peer.worktrees) {
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
    } else if !effective_complete && allow_incomplete {
        ConflictClassification::NoKnownConflict
    } else if !effective_complete {
        ConflictClassification::Unknown
    } else {
        ConflictClassification::Clear
    };
    ConflictEvaluation {
        schema_version: CONFLICT_EVALUATION_VERSION.to_string(),
        classification,
        complete: effective_complete,
        reasons,
        peers,
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn coordination_review_unsupported_peer_schema_makes_the_universe_incomplete() {
        let candidate: WorkContextInput = serde_json::from_value(json!({
            "schema_version": "agent-session.work-context-input.v1",
            "intent": "implementation",
            "tier": "L2",
            "repositories": ["example/repository"],
            "worktrees": ["hmac-sha256:1:candidate"],
            "provider_refs": [],
            "plan_refs": [],
            "scopes": [{"kind": "path-prefix", "repository": "example/repository", "value": "src/"}],
            "summary": "candidate"
        })).expect("candidate");
        let peer: WorkContextRecord = serde_json::from_value(json!({
            "schema_version": "agent-session.work-context.v999",
            "session_id": "peer",
            "session_incarnation": "peer-incarnation",
            "claim_id": "peer-claim",
            "revision": 1,
            "state": "active",
            "intent": "implementation",
            "tier": "L2",
            "repositories": ["other/repository"],
            "worktrees": ["hmac-sha256:999:unknown"],
            "provider_refs": [],
            "plan_refs": [],
            "scopes": [{"kind": "path-prefix", "repository": "other/repository", "value": "src/"}],
            "summary": "peer",
            "updated_at": "2030-01-01T00:00:00Z",
            "expires_at": "2030-01-01T01:00:00Z",
            "expires_at_epoch": 1
        }))
        .expect("peer");
        let evaluation = evaluate(
            Some(("candidate", "candidate-incarnation")),
            &candidate,
            &[peer],
            true,
            false,
        );
        assert_eq!(evaluation.classification, ConflictClassification::Unknown);
        assert!(!evaluation.complete);
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
        (ScopeKind::PathExact, ScopeKind::PathExact) => left.value == right.value,
        (ScopeKind::PathExact, ScopeKind::PathPrefix) => path_is_within(&left.value, &right.value),
        (ScopeKind::PathPrefix, ScopeKind::PathExact) => path_is_within(&right.value, &left.value),
        (ScopeKind::PathPrefix, ScopeKind::PathPrefix) => {
            path_is_within(&left.value, &right.value) || path_is_within(&right.value, &left.value)
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
        | (ScopeKind::PathPrefix, ScopeKind::PathPrefix) => {
            path_is_within(&target.value, &claim.value)
        }
        _ => false,
    }
}

fn path_is_within(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub fn canonicalize_targets(mut targets: Vec<Scope>) -> Result<Vec<Scope>, CliError> {
    if targets.len() > 32 {
        return Err(invalid_scope(
            "operation targets may contain at most 32 scopes",
        ));
    }
    let repositories: BTreeSet<_> = targets
        .iter()
        .map(|target| canonical_repository(target.repository.clone()))
        .collect::<Result<_, _>>()?;
    for target in &mut targets {
        target.repository = canonical_repository(target.repository.clone())?;
        target.value = match target.kind {
            ScopeKind::Repository => {
                if target.value.trim() != "." {
                    return Err(invalid_scope("repository target value must be ."));
                }
                ".".to_string()
            }
            ScopeKind::PathExact => canonical_relative_path(&target.value, false)?,
            ScopeKind::PathPrefix => canonical_relative_path(&target.value, false)?,
        };
        if !repositories.contains(&target.repository) {
            return Err(invalid_scope("operation target repository is invalid"));
        }
    }
    targets.sort();
    reject_duplicates(&targets, "operation target")?;
    Ok(targets)
}

pub fn canonicalize_provider_refs(
    mut references: Vec<ProviderRef>,
) -> Result<Vec<ProviderRef>, CliError> {
    if references.len() > 16 {
        return Err(invalid_scope(
            "operation targets exceed the provider reference limit",
        ));
    }
    for reference in &mut references {
        reference.kind = bounded_slug("provider reference kind", &reference.kind, 32)?;
        reference.repository = canonical_repository(reference.repository.clone())?;
        if reference.number == 0 {
            return Err(invalid_scope("provider reference number must be positive"));
        }
    }
    references.sort();
    reject_duplicates(&references, "provider reference")?;
    Ok(references)
}

pub fn validate_physical_targets(
    cwd: &str,
    targets: &[Scope],
    bindings: &[CheckoutBinding],
) -> Result<(), CliError> {
    let repositories: BTreeSet<_> = targets
        .iter()
        .map(|target| target.repository.as_str())
        .collect();
    let mut roots = std::collections::BTreeMap::new();
    for binding in bindings {
        let repository = canonical_repository(binding.repository.clone())?;
        if !repositories.contains(repository.as_str()) {
            return Err(physical_target_unavailable());
        }
        if roots.contains_key(&repository) {
            return Err(physical_target_unavailable());
        }
        roots.insert(repository, checkout_root(Path::new(&binding.path))?);
    }
    if repositories.len() == 1 {
        let repository = (*repositories.first().expect("single repository")).to_string();
        if let std::collections::btree_map::Entry::Vacant(entry) = roots.entry(repository) {
            entry.insert(checkout_root(Path::new(cwd))?);
        }
    }
    for repository in &repositories {
        let root = roots
            .get(*repository)
            .ok_or_else(physical_target_unavailable)?;
        if repository_for_checkout(root).as_deref() != Some(*repository) {
            return Err(physical_target_unavailable());
        }
        for target in targets.iter().filter(|target| {
            target.repository == **repository
                && matches!(target.kind, ScopeKind::PathExact | ScopeKind::PathPrefix)
        }) {
            validate_target_path(root, &target.value)?;
        }
    }
    Ok(())
}

pub(crate) fn checkout_root(path: &Path) -> Result<PathBuf, CliError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| physical_target_unavailable())?;
    if metadata.file_type().is_symlink() {
        return Err(physical_target_unavailable());
    }
    let canonical = fs::canonicalize(path).map_err(|_| physical_target_unavailable())?;
    let root = repository_root(&canonical).ok_or_else(physical_target_unavailable)?;
    fs::canonicalize(root).map_err(|_| physical_target_unavailable())
}

fn validate_target_path(root: &Path, relative: &str) -> Result<(), CliError> {
    let mut current = PathBuf::from(root);
    for component in Path::new(relative).components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(physical_target_unavailable());
                }
                let resolved =
                    fs::canonicalize(&current).map_err(|_| physical_target_unavailable())?;
                if !resolved.starts_with(root) {
                    return Err(physical_target_unavailable());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err(physical_target_unavailable()),
        }
    }
    Ok(())
}

pub(crate) fn repository_for_checkout(root: &Path) -> Option<String> {
    repository_for_checkout_with_timeout(root, GIT_REMOTE_TIMEOUT)
}

pub(crate) fn repository_for_checkout_with_timeout(
    root: &Path,
    timeout: Duration,
) -> Option<String> {
    let deadline = Instant::now().checked_add(timeout)?;
    if timeout.is_zero() {
        return None;
    }
    let mut child = Command::new("git")
        .args(["-C", root.to_str()?, "remote", "get-url", "origin"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let remote = String::from_utf8(output.stdout).ok()?;
    let remote = remote.trim().trim_end_matches(".git");
    let path = remote
        .rsplit_once(':')
        .filter(|(prefix, _)| !prefix.contains('/'))
        .map_or(remote, |(_, path)| path)
        .trim_end_matches('/');
    let mut parts = path.rsplit('/');
    let repository = parts.next()?;
    let owner = parts.next()?;
    canonical_repository(format!("{owner}/{repository}")).ok()
}

fn repository_root(path: &Path) -> Option<&Path> {
    path.ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
}

fn physical_target_unavailable() -> CliError {
    CliError::data(
        "uncovered-mutation-scope",
        "operation target could not be proven inside the physical checkout boundary",
        None,
    )
}

pub(crate) fn canonical_repository(value: String) -> Result<String, CliError> {
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

pub fn fingerprint_epoch(value: &str) -> Option<u64> {
    let mut parts = value.splitn(3, ':');
    (parts.next()? == "hmac-sha256").then_some(())?;
    let epoch = parts.next()?.parse().ok()?;
    let digest = parts.next()?;
    (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(epoch)
}

fn canonical_plan_ref(value: String) -> Result<String, CliError> {
    canonical_relative_path(&value, false)
}

fn canonical_relative_path(value: &str, _require_trailing_slash: bool) -> Result<String, CliError> {
    let value = value.trim().replace('\\', "/");
    if value.is_empty()
        || value.starts_with('/')
        || value.starts_with('~')
        || value.contains('\0')
        || value.contains(['*', '?', '[', ']', '{', '}', '!'])
        || value.chars().any(char::is_control)
    {
        return Err(invalid_scope(
            "path scope must be a safe repository-relative path",
        ));
    }
    if value.ends_with('/') {
        return Err(invalid_scope("path scope must not end with /"));
    }
    let components: Vec<_> = value.split('/').collect();
    if components
        .iter()
        .any(|component| component.is_empty() || *component == "." || *component == "..")
    {
        return Err(invalid_scope("path scope contains an invalid component"));
    }
    Ok(components.join("/"))
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
        || value.len() > max
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
        let prefix = scope(ScopeKind::PathPrefix, "src");
        assert!(scopes_overlap(
            &prefix,
            &scope(ScopeKind::PathExact, "src/lib.rs")
        ));
        assert!(!scopes_overlap(
            &prefix,
            &scope(ScopeKind::PathExact, "src2/lib.rs")
        ));
        assert!(scopes_overlap(
            &scope(ScopeKind::Repository, "."),
            &scope(ScopeKind::PathExact, "release")
        ));
    }

    #[test]
    fn a_prefix_claim_covers_only_descendant_targets() {
        let prefix = scope(ScopeKind::PathPrefix, "src");
        assert!(scope_covers(
            &prefix,
            &scope(ScopeKind::PathExact, "src/lib.rs")
        ));
        assert!(!scope_covers(
            &prefix,
            &scope(ScopeKind::PathExact, "tests/lib.rs")
        ));
    }

    #[test]
    fn summary_limit_is_measured_in_utf8_bytes() {
        let mut input = WorkContextInput {
            schema_version: WORK_CONTEXT_INPUT_VERSION.to_string(),
            intent: "implementation".to_string(),
            tier: "L2".to_string(),
            repositories: vec!["example/repo".to_string()],
            worktrees: Vec::new(),
            provider_refs: Vec::new(),
            plan_refs: Vec::new(),
            scopes: vec![scope(ScopeKind::Repository, ".")],
            summary: "界".repeat(81),
        };
        assert!(input.clone().validate_and_canonicalize().is_err());
        input.summary = "a".repeat(240);
        assert!(input.validate_and_canonicalize().is_ok());
    }

    #[test]
    fn coordination_review_round2_physical_target_validation_rejects_symlink_escape() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().join("checkout");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(root.join("src")).expect("checkout");
        assert!(
            Command::new("git")
                .args(["init", root.to_str().expect("root")])
                .status()
                .expect("git init")
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-C",
                    root.to_str().expect("root"),
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/example/repo.git"
                ])
                .status()
                .expect("git remote")
                .success()
        );
        fs::create_dir(&outside).expect("outside");
        std::os::unix::fs::symlink(&outside, root.join("src/link")).expect("symlink");
        let escaped = scope(ScopeKind::PathExact, "src/link/file.rs");
        assert!(validate_physical_targets(root.to_str().expect("root"), &[escaped], &[]).is_err());
        let inside = scope(ScopeKind::PathExact, "src/new.rs");
        assert!(validate_physical_targets(root.to_str().expect("root"), &[inside], &[]).is_ok());
    }
}

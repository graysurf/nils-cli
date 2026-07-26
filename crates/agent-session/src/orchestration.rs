use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{CliContext, CliError, SessionRecord};

pub(crate) const REGISTRY_SCHEMA: &str = "agent-session.orchestration-registry.v2";
const LEGACY_REGISTRY_SCHEMA: &str = "agent-session.orchestration-registry.v1";
pub(crate) const RUN_SCHEMA: &str = "agent-session.orchestration-run.v1";
pub(crate) const ASSIGNMENT_SCHEMA: &str = "agent-session.orchestration-assignment.v2";
const LEGACY_ASSIGNMENT_SCHEMA: &str = "agent-session.orchestration-assignment.v1";
pub(crate) const SESSION_PROJECTION_SCHEMA: &str = "agent-session.session-orchestration.v1";
pub(crate) const PACKET_SCHEMA: &str = "main-agent.objective-packet.v1";
pub(crate) const ASSIGNMENT_INPUT_SCHEMA: &str = "main-agent.assignment-input.v1";
pub(crate) const CHECKPOINT_INPUT_SCHEMA: &str = "main-agent.checkpoint-input.v1";
pub(crate) const SUBMIT_RECOVERY_SCHEMA: &str = "main-agent.submit-recovery.v1";

const ORCHESTRATION_DIR: &str = "orchestration";
const REGISTRY_FILE: &str = "registry.json";
const REGISTRY_LOCK: &str = "registry.lock";
const PACKETS_DIR: &str = "packets";
const MAX_REGISTRY_BYTES: u64 = 4 * 1024 * 1024;
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    pub session_id: String,
    pub session_incarnation: String,
    pub session_created_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunCheckpoint {
    pub revision: u64,
    pub summary: String,
    pub next_action: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunRecord {
    pub schema_version: String,
    pub run_id: String,
    pub revision: u64,
    pub state: String,
    pub tier: String,
    pub objective_summary: String,
    pub objective_packet_digest: String,
    pub controller: SessionRef,
    #[serde(default)]
    pub durable_refs: Vec<String>,
    /// Ephemeral runs are created by `main-agent quick` and auto-close once
    /// their last assignment's worker is torn down, so a fast-path caller never
    /// runs an explicit `close`.
    #[serde(default)]
    pub ephemeral: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<RunCheckpoint>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TimedRelationship {
    pub session: SessionRef,
    pub expires_at: String,
    pub expires_at_epoch: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubmitRecoveryRecord {
    pub schema_version: String,
    pub attempt_id: String,
    #[serde(default = "default_submit_recovery_origin")]
    pub origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<SessionRef>,
    pub session_incarnation: String,
    pub reserved_revision: u64,
    pub state: String,
    pub attempt_count: u8,
    pub result: String,
    pub attempted_at: String,
    pub updated_at: String,
}

fn default_submit_recovery_origin() -> String {
    "explicit".to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssignmentRecord {
    pub schema_version: String,
    pub assignment_id: String,
    pub run_id: String,
    pub revision: u64,
    pub state: String,
    pub task_summary: String,
    pub private_packet_digest: String,
    pub primary_manager: SessionRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<SessionRef>,
    #[serde(default)]
    pub collaborators: Vec<SessionRef>,
    #[serde(default)]
    pub borrowed_by: Vec<TimedRelationship>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub durable_refs: Vec<String>,
    /// Assignment ids in the same run this assignment depends on. Advisory
    /// ordering: `worker start` refuses to launch until every dependency has
    /// reached a satisfied terminal state (see `dependency_state_satisfies`).
    /// Stored durably so a launched dependent's ordering survives compaction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<RunCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submit_recovery: Option<SubmitRecoveryRecord>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdempotencyReceipt {
    pub principal_session_id: String,
    pub principal_incarnation: String,
    pub operation: String,
    pub request_digest: String,
    pub outcome: serde_json::Value,
    pub created_at_epoch: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Registry {
    pub schema_version: String,
    pub runs: BTreeMap<String, RunRecord>,
    pub assignments: BTreeMap<String, AssignmentRecord>,
    pub receipts: BTreeMap<String, IdempotencyReceipt>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct WorkerCounts {
    pub assigned: usize,
    pub starting: usize,
    pub working: usize,
    pub blocked: usize,
    pub submitted: usize,
    pub accepted: usize,
    pub cleanup_pending: usize,
    pub orphaned: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SessionOrchestrationProjection {
    pub schema_version: &'static str,
    pub run_id: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_manager: Option<SessionRef>,
    pub relationship_revision: u64,
    pub run_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignment_state: Option<String>,
    pub objective_summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collaborators: Vec<SessionRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub borrowed_by: Vec<SessionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_counts: Option<WorkerCounts>,
}

impl Registry {
    fn empty() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA.to_string(),
            ..Self::default()
        }
    }

    fn validate(&self) -> Result<(), CliError> {
        if self.schema_version != REGISTRY_SCHEMA {
            return Err(store_invalid("unsupported orchestration registry schema"));
        }
        if self.runs.len() > 1_024
            || self.assignments.len() > 16_384
            || self.receipts.len() > 32_768
        {
            return Err(store_invalid(
                "orchestration registry exceeds collection limits",
            ));
        }
        for (id, run) in &self.runs {
            if run.schema_version != RUN_SCHEMA || id != &run.run_id || run.revision == 0 {
                return Err(store_invalid("orchestration run identity is invalid"));
            }
            validate_slug("run id", id, 128)?;
            validate_state(
                &run.state,
                &["active", "orphaned", "recovery_needed", "closed"],
            )?;
            validate_summary("objective summary", &run.objective_summary)?;
            validate_digest(&run.objective_packet_digest)?;
            validate_session_ref(&run.controller)?;
        }
        for (id, assignment) in &self.assignments {
            if assignment.schema_version != ASSIGNMENT_SCHEMA
                || id != &assignment.assignment_id
                || assignment.revision == 0
                || !self.runs.contains_key(&assignment.run_id)
            {
                return Err(store_invalid(
                    "orchestration assignment identity is invalid",
                ));
            }
            validate_slug("assignment id", id, 128)?;
            validate_state(
                &assignment.state,
                &[
                    "assigned",
                    "starting",
                    "working",
                    "blocked",
                    "submitted",
                    "accepted",
                    "released",
                    "cancelled",
                ],
            )?;
            validate_summary("task summary", &assignment.task_summary)?;
            validate_digest(&assignment.private_packet_digest)?;
            validate_session_ref(&assignment.primary_manager)?;
            if let Some(worker) = &assignment.worker {
                validate_session_ref(worker)?;
            }
            if let Some(recovery) = &assignment.submit_recovery {
                if recovery.schema_version != SUBMIT_RECOVERY_SCHEMA
                    || recovery.attempt_id.is_empty()
                    || recovery.attempt_id.len() > 128
                    || !matches!(recovery.origin.as_str(), "automatic" | "explicit")
                    || recovery.attempt_count != 1
                    || recovery.reserved_revision == 0
                    || recovery.session_incarnation.is_empty()
                    || recovery.session_incarnation.len() > 128
                {
                    return Err(store_invalid(
                        "orchestration submit recovery identity is invalid",
                    ));
                }
                match (&recovery.run_id, &recovery.controller) {
                    (Some(run_id), Some(controller)) => {
                        validate_slug("submit recovery run id", run_id, 128)?;
                        validate_session_ref(controller)?;
                    }
                    (None, None) => {}
                    _ => {
                        return Err(store_invalid(
                            "orchestration submit recovery controller binding is incomplete",
                        ));
                    }
                }
                validate_state(
                    &recovery.state,
                    &[
                        "attempting",
                        "sent",
                        "failed",
                        "checkpoint_confirmed",
                        "reconciled",
                    ],
                )?;
                validate_summary("submit recovery result", &recovery.result)?;
            }
            for collaborator in &assignment.collaborators {
                validate_session_ref(collaborator)?;
            }
            for relationship in &assignment.borrowed_by {
                validate_session_ref(&relationship.session)?;
            }
            // Dependency edges are bounds/format-checked only. Referential
            // existence is intentionally NOT a registry invariant: a dependency
            // may be released and deleted after a dependent launches, and that
            // must not brick registry reads. Existence/satisfaction is enforced
            // at `worker start` gate time against live state instead.
            if assignment.depends_on.len() > 64 {
                return Err(store_invalid(
                    "orchestration assignment exceeds dependency limit",
                ));
            }
            for dependency in &assignment.depends_on {
                validate_slug("assignment dependency id", dependency, 128)?;
                if dependency == &assignment.assignment_id {
                    return Err(store_invalid(
                        "orchestration assignment cannot depend on itself",
                    ));
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn session_projection(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<Option<SessionOrchestrationProjection>, CliError> {
    let registry = load_registry_readonly(context)?;
    let Some(incarnation) = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    if let Some(run) = registry
        .runs
        .values()
        .find(|run| session_ref_matches(&run.controller, record, incarnation))
    {
        return Ok(Some(SessionOrchestrationProjection {
            schema_version: SESSION_PROJECTION_SCHEMA,
            run_id: run.run_id.clone(),
            role: "main".to_string(),
            assignment_id: None,
            primary_manager: None,
            relationship_revision: run.revision,
            run_state: run.state.clone(),
            assignment_state: None,
            objective_summary: run.objective_summary.clone(),
            collaborators: Vec::new(),
            borrowed_by: Vec::new(),
            relationship_state: (run.state != "active").then(|| run.state.clone()),
            worker_counts: Some(worker_counts(context, &registry, run)),
        }));
    }
    if let Some(assignment) = registry.assignments.values().find(|assignment| {
        assignment.worker.as_ref().is_some_and(|worker| {
            worker.session_id == record.id && worker.session_created_at == record.created_at
        })
    }) {
        let Some(run) = registry.runs.get(&assignment.run_id) else {
            return Ok(None);
        };
        let rebind_required = assignment
            .worker
            .as_ref()
            .is_some_and(|worker| worker.session_incarnation != incarnation);
        let now = crate::coordination::now_epoch();
        let borrowed_by = assignment
            .borrowed_by
            .iter()
            .filter(|relationship| relationship.expires_at_epoch > now)
            .map(|relationship| relationship.session.clone())
            .collect::<Vec<_>>();
        let relationship_state = if rebind_required {
            Some("rebind_required".to_string())
        } else if !controller_is_current(context, run) {
            Some("orphaned".to_string())
        } else if !borrowed_by.is_empty() {
            Some("borrowed".to_string())
        } else if !assignment.collaborators.is_empty() {
            Some("cross_managed".to_string())
        } else {
            None
        };
        return Ok(Some(SessionOrchestrationProjection {
            schema_version: SESSION_PROJECTION_SCHEMA,
            run_id: run.run_id.clone(),
            role: "worker".to_string(),
            assignment_id: Some(assignment.assignment_id.clone()),
            primary_manager: Some(assignment.primary_manager.clone()),
            relationship_revision: assignment.revision,
            run_state: run.state.clone(),
            assignment_state: Some(assignment.state.clone()),
            objective_summary: run.objective_summary.clone(),
            collaborators: assignment.collaborators.clone(),
            borrowed_by,
            relationship_state,
            worker_counts: None,
        }));
    }
    Ok(None)
}

fn worker_counts(context: &CliContext, registry: &Registry, run: &RunRecord) -> WorkerCounts {
    let mut counts = WorkerCounts {
        assigned: 0,
        starting: 0,
        working: 0,
        blocked: 0,
        submitted: 0,
        accepted: 0,
        cleanup_pending: 0,
        orphaned: 0,
    };
    for assignment in registry
        .assignments
        .values()
        .filter(|item| item.run_id == run.run_id)
    {
        match assignment.state.as_str() {
            "assigned" => counts.assigned += 1,
            "starting" => counts.starting += 1,
            "working" => counts.working += 1,
            "blocked" => counts.blocked += 1,
            "submitted" => counts.submitted += 1,
            "accepted" => counts.accepted += 1,
            "released" | "cancelled" => {
                counts.cleanup_pending += usize::from(
                    assignment
                        .worker
                        .as_ref()
                        .is_some_and(|worker| session_ref_is_live(context, worker)),
                )
            }
            _ => {}
        }
        if assignment.worker.is_some() && !controller_is_current(context, run) {
            counts.orphaned += 1;
        }
    }
    counts
}

pub(crate) fn controller_is_current(context: &CliContext, run: &RunRecord) -> bool {
    session_ref_is_live(context, &run.controller)
}

pub(crate) fn session_ref_is_live(context: &CliContext, reference: &SessionRef) -> bool {
    crate::load_session_record(context, &reference.session_id)
        .ok()
        .and_then(|record| {
            record.runtime.as_ref().map(|runtime| {
                runtime.launch_id == reference.session_incarnation
                    && record.created_at == reference.session_created_at
            })
        })
        .unwrap_or(false)
}

pub(crate) fn session_ref_matches(
    reference: &SessionRef,
    record: &SessionRecord,
    incarnation: &str,
) -> bool {
    reference.session_id == record.id
        && reference.session_incarnation == incarnation
        && reference.session_created_at == record.created_at
}

pub(crate) fn load_registry_readonly(context: &CliContext) -> Result<Registry, CliError> {
    let path = orchestration_root(context).join(REGISTRY_FILE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Registry::empty()),
        Err(_) => return Err(store_unavailable()),
    };
    validate_private_file(&metadata)?;
    if metadata.len() > MAX_REGISTRY_BYTES {
        return Err(store_invalid("orchestration registry exceeds byte limit"));
    }
    let bytes = fs::read(&path).map_err(|_| store_unavailable())?;
    let mut registry: Registry = serde_json::from_slice(&bytes)
        .map_err(|_| store_invalid("orchestration registry is invalid"))?;
    if registry.schema_version == LEGACY_REGISTRY_SCHEMA {
        registry.schema_version = REGISTRY_SCHEMA.to_string();
    }
    for assignment in registry.assignments.values_mut() {
        if assignment.schema_version == LEGACY_ASSIGNMENT_SCHEMA {
            assignment.schema_version = ASSIGNMENT_SCHEMA.to_string();
        }
    }
    registry.validate()?;
    Ok(registry)
}

pub(crate) struct LockedRegistry {
    _lock: File,
    path: PathBuf,
    pub registry: Registry,
}

impl LockedRegistry {
    pub fn save(&mut self) -> Result<(), CliError> {
        self.registry.schema_version = REGISTRY_SCHEMA.to_string();
        self.registry.validate()?;
        let bytes = serde_json::to_vec_pretty(&self.registry)
            .map_err(|_| store_invalid("orchestration registry is invalid"))?;
        if bytes.len() as u64 > MAX_REGISTRY_BYTES {
            return Err(store_invalid("orchestration registry exceeds byte limit"));
        }
        write_atomic(&self.path, &bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())
    }
}

pub(crate) fn lock_registry(context: &CliContext) -> Result<LockedRegistry, CliError> {
    let root = ensure_orchestration_root(context)?;
    let path = root.join(REGISTRY_FILE);
    let lock_path = root.join(REGISTRY_LOCK);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(SECRET_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock_path)
        .map_err(|_| store_unavailable())?;
    let started = Instant::now();
    loop {
        // SAFETY: the descriptor remains open for the duration of the lock guard.
        let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            break;
        }
        if started.elapsed() >= LOCK_TIMEOUT {
            return Err(CliError::unavailable(
                "orchestration-store-busy",
                "orchestration store is busy; retry with the same idempotency key",
                None,
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
    let registry = load_registry_readonly(context)?;
    Ok(LockedRegistry {
        _lock: lock,
        path,
        registry,
    })
}

pub(crate) fn packet_path(context: &CliContext, digest: &str) -> Result<PathBuf, CliError> {
    validate_digest(digest)?;
    let root = ensure_orchestration_root(context)?.join(PACKETS_DIR);
    ensure_private_directory(&root)?;
    Ok(root.join(digest.trim_start_matches("sha256:")))
}

pub(crate) fn store_packet(context: &CliContext, value: &Value) -> Result<String, CliError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| store_invalid("private orchestration packet is invalid"))?;
    if bytes.len() > 256 * 1024 {
        return Err(store_invalid(
            "private orchestration packet exceeds byte limit",
        ));
    }
    let digest = packet_digest_bytes(&bytes);
    let path = packet_path(context, &digest)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            validate_private_file(&metadata)?;
            let existing = fs::read(&path).map_err(|_| store_unavailable())?;
            if existing != bytes {
                return Err(store_invalid(
                    "private orchestration packet digest collision",
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_atomic(&path, &bytes, SECRET_FILE_MODE).map_err(|_| store_unavailable())?;
        }
        Err(_) => return Err(store_unavailable()),
    }
    Ok(digest)
}

pub(crate) fn packet_digest(value: &Value) -> Result<String, CliError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| store_invalid("private orchestration packet is invalid"))?;
    if bytes.len() > 256 * 1024 {
        return Err(store_invalid(
            "private orchestration packet exceeds byte limit",
        ));
    }
    Ok(packet_digest_bytes(&bytes))
}

pub(crate) fn read_packet(context: &CliContext, digest: &str) -> Result<Value, CliError> {
    let path = packet_path(context, digest)?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| store_unavailable())?;
    validate_private_file(&metadata)?;
    if metadata.len() > 256 * 1024 {
        return Err(store_invalid(
            "private orchestration packet exceeds byte limit",
        ));
    }
    let bytes = fs::read(&path).map_err(|_| store_unavailable())?;
    let actual = packet_digest_bytes(&bytes);
    if actual != digest {
        return Err(store_invalid(
            "private orchestration packet digest is invalid",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| store_invalid("private orchestration packet is invalid"))
}

fn orchestration_root(context: &CliContext) -> PathBuf {
    context.state_dir.join(ORCHESTRATION_DIR)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn packet_digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}

fn ensure_orchestration_root(context: &CliContext) -> Result<PathBuf, CliError> {
    let root = orchestration_root(context);
    ensure_private_directory(&root)?;
    Ok(root)
}

fn ensure_private_directory(path: &Path) -> Result<(), CliError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(store_unavailable()),
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| store_unavailable())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(store_invalid("orchestration store root is unsafe"));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| store_unavailable())?;
    Ok(())
}

fn validate_private_file(metadata: &fs::Metadata) -> Result<(), CliError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(store_invalid(
            "orchestration registry permissions are unsafe",
        ));
    }
    Ok(())
}

fn validate_session_ref(reference: &SessionRef) -> Result<(), CliError> {
    crate::validate_id(&reference.session_id)?;
    validate_slug("session incarnation", &reference.session_incarnation, 128)?;
    if reference.session_created_at.trim().is_empty() || reference.session_created_at.len() > 64 {
        return Err(store_invalid("session reference timestamp is invalid"));
    }
    if let Some(machine) = &reference.machine {
        crate::validate_host(machine)?;
    }
    Ok(())
}

pub(crate) fn validate_slug(name: &str, value: &str, max: usize) -> Result<(), CliError> {
    if value.is_empty()
        || value.len() > max
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'))
    {
        return Err(store_invalid(&format!("{name} is invalid")));
    }
    Ok(())
}

fn validate_state(value: &str, allowed: &[&str]) -> Result<(), CliError> {
    if !allowed.contains(&value) {
        return Err(store_invalid("orchestration state is unsupported"));
    }
    Ok(())
}

pub(crate) fn validate_summary(name: &str, value: &str) -> Result<(), CliError> {
    if value.trim().is_empty() || value.chars().count() > 240 || value.chars().any(char::is_control)
    {
        return Err(store_invalid(&format!("{name} is invalid")));
    }
    Ok(())
}

pub(crate) fn validate_digest(value: &str) -> Result<(), CliError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(store_invalid("packet digest is invalid"));
    };
    if hex.len() != 64
        || !hex
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
    {
        return Err(store_invalid("packet digest is invalid"));
    }
    Ok(())
}

fn store_invalid(message: &str) -> CliError {
    CliError::data("orchestration-store-invalid", message, None)
}

fn store_unavailable() -> CliError {
    CliError::unavailable(
        "orchestration-store-unavailable",
        "orchestration store is unavailable",
        Some(json!({ "retryable": true })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_rejects_unknown_run_state_without_leaking_record_content() {
        let mut registry = Registry::empty();
        registry.runs.insert(
            "run-one".to_string(),
            RunRecord {
                schema_version: RUN_SCHEMA.to_string(),
                run_id: "run-one".to_string(),
                revision: 1,
                state: "future".to_string(),
                tier: "L0".to_string(),
                objective_summary: "safe summary".to_string(),
                objective_packet_digest: format!("sha256:{}", "a".repeat(64)),
                controller: SessionRef {
                    machine: None,
                    session_id: "main".to_string(),
                    session_incarnation: "incarnation".to_string(),
                    session_created_at: "2030-01-01T00:00:00Z".to_string(),
                },
                durable_refs: Vec::new(),
                ephemeral: false,
                checkpoint: None,
                created_at: "2030-01-01T00:00:00Z".to_string(),
                updated_at: "2030-01-01T00:00:00Z".to_string(),
            },
        );
        let error = registry
            .validate()
            .expect_err("future state must fail closed");
        assert_eq!(error.code(), "orchestration-store-invalid");
        assert!(!error.message().contains("safe summary"));
    }
}

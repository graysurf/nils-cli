//! Strict WorkspaceLease v1 automation boundary.
//!
//! DSH/runtime-kit owns lifecycle attribution. This module owns canonical
//! physical workspace identity, durable cross-process exclusivity, operation
//! fences, idempotency, and conservative stale recovery.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::dsh_policy::{GitLayout, checkout_dirty, git_layout};
use crate::error::HookError;

const PROTOCOL_VERSION: u64 = 1;
const STATE_SCHEMA: &str = "agent-hook.workspace-lease.state.v1";
const BIND_SCHEMA: &str = "agent-hook.workspace-lease.bind.v1";
const BEGIN_SCHEMA: &str = "agent-hook.workspace-lease.begin.v1";
const COMPLETE_SCHEMA: &str = "agent-hook.workspace-lease.complete.v1";
const RENEW_SCHEMA: &str = "agent-hook.workspace-lease.renew.v1";
const RELEASE_SCHEMA: &str = "agent-hook.workspace-lease.release.v1";
const BIND_RESULT_SCHEMA: &str = "agent-hook.workspace-lease.bind-result.v1";
const BEGIN_RESULT_SCHEMA: &str = "agent-hook.workspace-lease.begin-result.v1";
const COMPLETE_RESULT_SCHEMA: &str = "agent-hook.workspace-lease.complete-result.v1";
const RENEW_RESULT_SCHEMA: &str = "agent-hook.workspace-lease.renew-result.v1";
const RELEASE_RESULT_SCHEMA: &str = "agent-hook.workspace-lease.release-result.v1";
const REQUEST_MAX_BYTES: u64 = 256 * 1024;
const STATE_MAX_BYTES: u64 = 512 * 1024;
const MAX_TEXT_BYTES: usize = 512;
const MAX_OPERATIONS: usize = 256;
const MAX_TOMBSTONES: usize = 64;
const LEASE_TTL_SECONDS: u64 = 30;
const RENEW_AFTER_MS: u64 = 10_000;
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug)]
pub(crate) enum Operation {
    Bind,
    Begin,
    Complete,
    Renew,
    Release,
}

pub(crate) struct Outcome {
    pub(crate) data: Value,
    pub(crate) text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindRequest {
    #[serde(rename = "schema_version")]
    _schema_version: String,
    version: u64,
    request_id: String,
    session_id: String,
    #[serde(default)]
    parent_session_id: Option<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    source: SessionStartSource,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BeginRequest {
    #[serde(rename = "schema_version")]
    _schema_version: String,
    version: u64,
    request_id: String,
    session_id: String,
    #[serde(default)]
    parent_session_id: Option<String>,
    binding_id: String,
    workspace_id: String,
    generation: String,
    binding_state: BindingMode,
    call_id: String,
    root_call_id: String,
    tool_name: String,
    arguments: Value,
    nested: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteRequest {
    #[serde(rename = "schema_version")]
    _schema_version: String,
    version: u64,
    request_id: String,
    session_id: String,
    #[serde(default)]
    parent_session_id: Option<String>,
    binding_id: String,
    workspace_id: String,
    generation: String,
    operation_id: String,
    fence: String,
    call_id: String,
    root_call_id: String,
    tool_name: String,
    outcome: OperationOutcome,
    #[serde(default)]
    error_code: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenewRequest {
    #[serde(rename = "schema_version")]
    _schema_version: String,
    version: u64,
    request_id: String,
    session_id: String,
    #[serde(default)]
    parent_session_id: Option<String>,
    binding_id: String,
    workspace_id: String,
    generation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRequest {
    #[serde(rename = "schema_version")]
    _schema_version: String,
    version: u64,
    request_id: String,
    session_id: String,
    #[serde(default)]
    parent_session_id: Option<String>,
    binding_id: String,
    workspace_id: String,
    generation: String,
    reason: ReleaseReason,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum BindingMode {
    Owned,
    Unmanaged,
}

impl BindingMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::Unmanaged => "unmanaged",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum BindingStatus {
    Active,
    Released,
    Recovered,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum OperationStatus {
    Active,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum OperationOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

impl OperationOutcome {
    fn status(self) -> OperationStatus {
        match self {
            Self::Succeeded => OperationStatus::Succeeded,
            Self::Failed => OperationStatus::Failed,
            Self::Cancelled => OperationStatus::Cancelled,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum ReleaseReason {
    AgentDisposed,
    SessionRebound,
    ProviderDisposed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum SessionStartSource {
    Startup,
    Resume,
    Clear,
    Compact,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ManagedIdentity {
    root: String,
    root_dev: u64,
    root_ino: u64,
    git_dir: String,
    git_dev: u64,
    git_ino: u64,
    common_dir: String,
    common_dev: u64,
    common_ino: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum WorkspaceIdentity {
    Managed { identity: ManagedIdentity },
    Unmanaged { root: Option<String> },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    binding_id: String,
    generation: String,
    session_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_session_digest: Option<String>,
    mode: BindingMode,
    status: BindingStatus,
    bind_request_digest: String,
    bind_request_id_digest: String,
    refreshed_at_epoch: u64,
    expires_at_epoch: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OperationRecord {
    operation_id: String,
    fence: String,
    request_id_digest: String,
    request_digest: String,
    execution_digest: String,
    completion_execution_digest: String,
    status: OperationStatus,
    started_at_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completed_at_epoch: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completion_digest: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BindingTombstone {
    binding_id: String,
    generation: String,
    session_digest: String,
    workspace_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema_version: String,
    workspace_key: String,
    workspace_id: String,
    identity: WorkspaceIdentity,
    binding: Binding,
    operations: Vec<OperationRecord>,
    tombstones: Vec<BindingTombstone>,
}

struct LockedState {
    _lock: File,
    directory: PathBuf,
    key: String,
    fingerprint_key: String,
}

pub(crate) fn run(state_root: &Path, operation: Operation) -> Result<Outcome, HookError> {
    match operation {
        Operation::Bind => bind(state_root, read_request(BIND_SCHEMA)?),
        Operation::Begin => begin(state_root, read_request(BEGIN_SCHEMA)?),
        Operation::Complete => complete(state_root, read_request(COMPLETE_SCHEMA)?),
        Operation::Renew => renew(state_root, read_request(RENEW_SCHEMA)?),
        Operation::Release => release(state_root, read_request(RELEASE_SCHEMA)?),
    }
}

fn bind(state_root: &Path, request: BindRequest) -> Result<Outcome, HookError> {
    validate_common(
        request.version,
        &request.request_id,
        &request.session_id,
        request.parent_session_id.as_deref(),
    )?;
    let session_digest = digest(request.session_id.as_bytes());
    let parent_session_digest = request
        .parent_session_id
        .as_deref()
        .map(|value| digest(value.as_bytes()));
    let fingerprint_key = workspace_fingerprint_key(state_root, true)?;
    let (identity, key, managed) =
        resolve_identity(request.cwd.as_deref(), &session_digest, &fingerprint_key)?;
    let request_digest = digest_value(
        "workspace-bind",
        &json!({
            "version": request.version,
            "session_digest": session_digest,
            "parent_session_digest": parent_session_digest,
            "identity": identity,
            "source": request.source,
        }),
    )?;
    let request_id_digest = digest(request.request_id.as_bytes());
    let locked = lock_workspace(state_root, &key, Some(fingerprint_key))?;
    let mut state = read_state(&locked)?;
    let now = now_epoch()?;
    let same_principal_recovery = state.as_ref().is_some_and(|existing| {
        matches!(
            request.source,
            SessionStartSource::Resume | SessionStartSource::Compact
        ) && existing.binding.session_digest == session_digest
            && existing.binding.parent_session_digest == parent_session_digest
            && existing
                .operations
                .iter()
                .all(|operation| operation.status != OperationStatus::Active)
    });
    if let Some(existing) = state.as_ref() {
        validate_state(existing, &locked, &identity)?;
        if existing.binding.bind_request_id_digest == request_id_digest {
            if existing.binding.bind_request_digest != request_digest {
                return Err(idempotency_reused());
            }
            if existing.binding.status == BindingStatus::Active
                && existing.binding.expires_at_epoch > now
            {
                return Ok(bound(existing));
            }
        }
    }

    if managed {
        if let Some(existing) = state.as_ref()
            && existing.binding.status == BindingStatus::Active
            && existing.binding.expires_at_epoch > now
        {
            return Ok(denied(
                BIND_RESULT_SCHEMA,
                "foreign-active",
                "WORKSPACE_FOREIGN_ACTIVE",
                "another live session owns this workspace",
            ));
        }
        if let Some(existing) = state.as_ref()
            && existing.binding.status == BindingStatus::Active
            && existing.operations.iter().any(|operation| {
                operation.status == OperationStatus::Active
                    && operation.operation_id.starts_with("wlo1.")
            })
        {
            return Ok(denied(
                BIND_RESULT_SCHEMA,
                "uncertain",
                "WORKSPACE_OPERATION_UNCERTAIN",
                "an expired workspace operation has no durable terminal outcome",
            ));
        }
        let identity = managed_identity(&identity)?;
        if !same_principal_recovery && dirty(identity)? {
            return Ok(denied(
                BIND_RESULT_SCHEMA,
                "dirty",
                "WORKSPACE_DIRTY",
                "the workspace has uncommitted state and cannot be reassigned safely",
            ));
        }
    }

    let workspace_id = state
        .as_ref()
        .map(|state| state.workspace_id.clone())
        .unwrap_or_else(|| opaque("wlw1"));
    let mode = if managed {
        BindingMode::Owned
    } else {
        BindingMode::Unmanaged
    };
    let binding = Binding {
        binding_id: format!("wlb1.{key}.{}", Uuid::new_v4().simple()),
        generation: opaque("wlg1"),
        session_digest: session_digest.clone(),
        parent_session_digest,
        mode,
        status: BindingStatus::Active,
        bind_request_digest: request_digest,
        bind_request_id_digest: request_id_digest,
        refreshed_at_epoch: now,
        expires_at_epoch: now.saturating_add(LEASE_TTL_SECONDS),
    };
    let mut tombstones = state
        .as_ref()
        .map(|state| state.tombstones.clone())
        .unwrap_or_default();
    if let Some(existing) = state.as_mut() {
        existing.binding.status = BindingStatus::Recovered;
        tombstones.push(tombstone(existing));
        compact_tombstones(&mut tombstones);
    }
    let state = State {
        schema_version: STATE_SCHEMA.to_string(),
        workspace_key: key,
        workspace_id,
        identity,
        binding,
        operations: Vec::new(),
        tombstones,
    };
    write_state(&locked, &state)?;
    Ok(bound(&state))
}

fn begin(state_root: &Path, request: BeginRequest) -> Result<Outcome, HookError> {
    validate_common(
        request.version,
        &request.request_id,
        &request.session_id,
        request.parent_session_id.as_deref(),
    )?;
    for (value, field) in [
        (&request.binding_id, "binding_id"),
        (&request.workspace_id, "workspace_id"),
        (&request.generation, "generation"),
        (&request.call_id, "call_id"),
        (&request.root_call_id, "root_call_id"),
        (&request.tool_name, "tool_name"),
    ] {
        validate_text(value, field)?;
    }
    let key = binding_key(&request.binding_id)?;
    let locked = lock_workspace(state_root, key, None)?;
    let mut state = read_required_state(&locked)?;
    validate_loaded_state(&state, &locked)?;
    if let Some(denial) = binding_denial(
        &state,
        &request.binding_id,
        &request.workspace_id,
        &request.generation,
        &request.session_id,
        request.parent_session_id.as_deref(),
    ) {
        return Ok(denial);
    }
    revalidate_managed_identity(&state)?;
    if request.binding_state != state.binding.mode {
        return Err(HookError::data(
            "workspace-binding-state-invalid",
            "workspace binding state does not match durable authority",
        ));
    }
    if state.binding.mode == BindingMode::Unmanaged || tool_is_read_only(&request) {
        return Ok(Outcome {
            data: json!({
                "schema_version": BEGIN_RESULT_SCHEMA,
                "kind": "not-required",
            }),
            text: "workspace lease: operation does not require a mutation fence\n".to_string(),
        });
    }
    let now = now_epoch()?;
    if state.binding.expires_at_epoch <= now {
        return expired_denial(&state);
    }
    let execution_digest = execution_digest(&request)?;
    let request_digest = digest_value(
        "workspace-begin",
        &json!({
            "request": execution_digest,
            "binding_id": request.binding_id,
            "generation": request.generation,
        }),
    )?;
    let request_id_digest = digest(request.request_id.as_bytes());
    if let Some(existing) = state
        .operations
        .iter()
        .find(|operation| operation.request_id_digest == request_id_digest)
    {
        if existing.request_digest != request_digest {
            return Err(idempotency_reused());
        }
        if existing.status != OperationStatus::Active {
            return Ok(denied(
                BEGIN_RESULT_SCHEMA,
                "uncertain",
                "WORKSPACE_OPERATION_REPLAYED",
                "the operation identity already reached a terminal state",
            ));
        }
        return Ok(granted(existing));
    }
    if state.operations.len() >= MAX_OPERATIONS {
        compact_operations(&mut state.operations);
    }
    if state.operations.len() >= MAX_OPERATIONS {
        return Err(workspace_unavailable(
            "workspace-operation-capacity-exhausted",
            "workspace operation history is at its conservative capacity",
            "complete or release prior operations, then retry the exact request once",
        ));
    }
    let completion_execution_digest = digest_value(
        "workspace-execution-complete",
        &json!({
            "call_id": request.call_id,
            "root_call_id": request.root_call_id,
            "tool_name": request.tool_name,
        }),
    )?;
    state.operations.push(OperationRecord {
        operation_id: opaque("wlo1"),
        fence: opaque("wlf1"),
        request_id_digest,
        request_digest,
        execution_digest,
        completion_execution_digest,
        status: OperationStatus::Active,
        started_at_epoch: now,
        completed_at_epoch: None,
        completion_digest: None,
    });
    let outcome = state
        .operations
        .last()
        .map(granted)
        .ok_or_else(state_invalid)?;
    write_state(&locked, &state)?;
    Ok(outcome)
}

fn complete(state_root: &Path, request: CompleteRequest) -> Result<Outcome, HookError> {
    validate_common(
        request.version,
        &request.request_id,
        &request.session_id,
        request.parent_session_id.as_deref(),
    )?;
    for (value, field) in [
        (&request.binding_id, "binding_id"),
        (&request.workspace_id, "workspace_id"),
        (&request.generation, "generation"),
        (&request.operation_id, "operation_id"),
        (&request.fence, "fence"),
        (&request.call_id, "call_id"),
        (&request.root_call_id, "root_call_id"),
        (&request.tool_name, "tool_name"),
    ] {
        validate_text(value, field)?;
    }
    if let Some(error_code) = request.error_code.as_deref() {
        validate_text(error_code, "error_code")?;
    }
    let key = binding_key(&request.binding_id)?;
    let locked = lock_workspace(state_root, key, None)?;
    let mut state = read_required_state(&locked)?;
    validate_loaded_state(&state, &locked)?;
    require_binding(
        &state,
        &request.binding_id,
        &request.workspace_id,
        &request.generation,
        &request.session_id,
        request.parent_session_id.as_deref(),
    )?;
    let operation = state
        .operations
        .iter_mut()
        .find(|operation| operation.operation_id == request.operation_id)
        .ok_or_else(|| {
            HookError::data(
                "workspace-operation-unavailable",
                "workspace operation identity is unavailable",
            )
        })?;
    if operation.fence != request.fence {
        return Err(HookError::data(
            "workspace-operation-fence-invalid",
            "workspace operation fence is invalid",
        ));
    }
    let observed_execution = digest_value(
        "workspace-execution-complete",
        &json!({
            "call_id": request.call_id,
            "root_call_id": request.root_call_id,
            "tool_name": request.tool_name,
        }),
    )?;
    if observed_execution != operation.completion_execution_digest {
        return Err(HookError::data(
            "workspace-operation-identity-invalid",
            "workspace operation completion identity is invalid",
        ));
    }
    let completion_digest = digest_value(
        "workspace-complete",
        &json!({
            "operation_id": request.operation_id,
            "fence": request.fence,
            "execution": observed_execution,
            "outcome": request.outcome,
            "error_code": request.error_code,
        }),
    )?;
    if operation.status != OperationStatus::Active {
        if operation.completion_digest.as_deref() == Some(completion_digest.as_str()) {
            return Ok(ack(
                COMPLETE_RESULT_SCHEMA,
                "duplicate",
                "completion already recorded",
            ));
        }
        return Err(HookError::data(
            "workspace-operation-outcome-conflict",
            "workspace operation already has a different terminal outcome",
        ));
    }
    operation.status = request.outcome.status();
    operation.completed_at_epoch = Some(now_epoch()?);
    operation.completion_digest = Some(completion_digest);
    write_state(&locked, &state)?;
    Ok(ack(
        COMPLETE_RESULT_SCHEMA,
        "completed",
        "operation completed",
    ))
}

fn renew(state_root: &Path, request: RenewRequest) -> Result<Outcome, HookError> {
    validate_common(
        request.version,
        &request.request_id,
        &request.session_id,
        request.parent_session_id.as_deref(),
    )?;
    let key = binding_key(&request.binding_id)?;
    let locked = lock_workspace(state_root, key, None)?;
    let mut state = read_required_state(&locked)?;
    validate_loaded_state(&state, &locked)?;
    if let Some(denial) = binding_denial(
        &state,
        &request.binding_id,
        &request.workspace_id,
        &request.generation,
        &request.session_id,
        request.parent_session_id.as_deref(),
    ) {
        return Ok(lost_from_denial(denial));
    }
    revalidate_managed_identity(&state)?;
    let now = now_epoch()?;
    if state.binding.expires_at_epoch <= now {
        return Ok(lost_from_denial(expired_denial(&state)?));
    }
    state.binding.refreshed_at_epoch = now;
    state.binding.expires_at_epoch = now.saturating_add(LEASE_TTL_SECONDS);
    write_state(&locked, &state)?;
    Ok(Outcome {
        data: json!({
            "schema_version": RENEW_RESULT_SCHEMA,
            "kind": "renewed",
            "renew_after_ms": RENEW_AFTER_MS,
        }),
        text: "workspace lease renewed\n".to_string(),
    })
}

fn release(state_root: &Path, request: ReleaseRequest) -> Result<Outcome, HookError> {
    validate_common(
        request.version,
        &request.request_id,
        &request.session_id,
        request.parent_session_id.as_deref(),
    )?;
    let _ = request.reason;
    let key = binding_key(&request.binding_id)?;
    let locked = lock_workspace(state_root, key, None)?;
    let mut state = read_required_state(&locked)?;
    validate_loaded_state(&state, &locked)?;
    let session_digest = digest(request.session_id.as_bytes());
    if state.binding.status != BindingStatus::Active {
        if exact_binding(
            &state.binding,
            &request.binding_id,
            &request.workspace_id,
            &request.generation,
            &session_digest,
            request.parent_session_id.as_deref(),
            &state.workspace_id,
        ) || state.tombstones.iter().any(|tombstone| {
            tombstone.binding_id == request.binding_id
                && tombstone.generation == request.generation
                && tombstone.session_digest == session_digest
                && tombstone.workspace_id == request.workspace_id
        }) {
            return Ok(ack(
                RELEASE_RESULT_SCHEMA,
                "duplicate",
                "binding already released",
            ));
        }
        return Err(stale_binding());
    }
    require_binding(
        &state,
        &request.binding_id,
        &request.workspace_id,
        &request.generation,
        &request.session_id,
        request.parent_session_id.as_deref(),
    )?;
    if state
        .operations
        .iter()
        .any(|operation| operation.status == OperationStatus::Active)
    {
        return Err(workspace_unavailable(
            "workspace-release-uncertain",
            "workspace binding has an operation without a terminal outcome",
            "complete the exact active operation, then retry the exact release once",
        ));
    }
    state.binding.status = BindingStatus::Released;
    state.binding.expires_at_epoch = now_epoch()?;
    write_state(&locked, &state)?;
    Ok(ack(RELEASE_RESULT_SCHEMA, "released", "binding released"))
}

fn read_request<T: DeserializeOwned>(expected_schema: &str) -> Result<T, HookError> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(REQUEST_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| wire_invalid())?;
    if bytes.is_empty() || bytes.len() as u64 > REQUEST_MAX_BYTES {
        return Err(wire_invalid());
    }
    let value = crate::strict_json::from_slice(&bytes).map_err(|_| wire_invalid())?;
    let actual_schema = value
        .get("schema_version")
        .and_then(Value::as_str)
        .ok_or_else(wire_invalid)?;
    if actual_schema != expected_schema {
        return Err(wire_invalid());
    }
    serde_json::from_value(value).map_err(|_| wire_invalid())
}

fn validate_common(
    version: u64,
    request_id: &str,
    session_id: &str,
    parent_session_id: Option<&str>,
) -> Result<(), HookError> {
    if version != PROTOCOL_VERSION {
        return Err(HookError::data(
            "workspace-protocol-unsupported",
            "workspace lease protocol version is unsupported",
        ));
    }
    validate_text(request_id, "request_id")?;
    validate_text(session_id, "session_id")?;
    if let Some(parent) = parent_session_id {
        validate_text(parent, "parent_session_id")?;
        if parent == session_id {
            return Err(wire_invalid());
        }
    }
    Ok(())
}

fn validate_text(value: &str, _field: &str) -> Result<(), HookError> {
    if value.is_empty()
        || value.len() > MAX_TEXT_BYTES
        || value.chars().any(|character| character.is_control())
    {
        return Err(wire_invalid());
    }
    Ok(())
}

fn resolve_identity(
    cwd: Option<&Path>,
    session_digest: &str,
    fingerprint_key: &str,
) -> Result<(WorkspaceIdentity, String, bool), HookError> {
    let Some(cwd) = cwd else {
        let identity = WorkspaceIdentity::Unmanaged { root: None };
        let key = keyed_value(
            fingerprint_key,
            "workspace-unmanaged-key",
            &json!({"session": session_digest, "root": null}),
        )?;
        return Ok((identity, key, false));
    };
    if !cwd.is_absolute() {
        return Err(HookError::data(
            "workspace-cwd-invalid",
            "workspace cwd must be an absolute path",
        ));
    }
    let canonical = fs::canonicalize(cwd).map_err(|_| {
        workspace_unavailable(
            "workspace-identity-unavailable",
            "workspace identity could not be resolved",
            "restore the exact workspace and retry the exact request once",
        )
    })?;
    if let Some(layout) = git_layout(&canonical) {
        let identity = WorkspaceIdentity::Managed {
            identity: identity_from_layout(&layout)?,
        };
        let key = keyed_value(fingerprint_key, "workspace-key", &identity)?;
        Ok((identity, key, true))
    } else if has_git_marker(&canonical)? {
        Err(identity_unavailable())
    } else {
        let root = path_text(&canonical)?;
        let identity = WorkspaceIdentity::Unmanaged {
            root: Some(root.clone()),
        };
        let key = keyed_value(
            fingerprint_key,
            "workspace-unmanaged-key",
            &json!({"session": session_digest, "root": root}),
        )?;
        Ok((identity, key, false))
    }
}

fn identity_from_layout(layout: &GitLayout) -> Result<ManagedIdentity, HookError> {
    let root = fs::metadata(&layout.root).map_err(|_| identity_unavailable())?;
    let git = fs::metadata(&layout.git_dir).map_err(|_| identity_unavailable())?;
    let common = fs::metadata(&layout.common_dir).map_err(|_| identity_unavailable())?;
    Ok(ManagedIdentity {
        root: path_text(&layout.root)?,
        root_dev: root.dev(),
        root_ino: root.ino(),
        git_dir: path_text(&layout.git_dir)?,
        git_dev: git.dev(),
        git_ino: git.ino(),
        common_dir: path_text(&layout.common_dir)?,
        common_dev: common.dev(),
        common_ino: common.ino(),
    })
}

fn managed_identity(identity: &WorkspaceIdentity) -> Result<&ManagedIdentity, HookError> {
    match identity {
        WorkspaceIdentity::Managed { identity } => Ok(identity),
        WorkspaceIdentity::Unmanaged { .. } => Err(identity_unavailable()),
    }
}

fn dirty(identity: &ManagedIdentity) -> Result<bool, HookError> {
    let layout = GitLayout {
        root: PathBuf::from(&identity.root),
        git_dir: PathBuf::from(&identity.git_dir),
        common_dir: PathBuf::from(&identity.common_dir),
    };
    let mut run_child = |mut command: Command| {
        command.output().map_err(|_| {
            workspace_unavailable(
                "workspace-git-unavailable",
                "workspace dirty-state inspection is unavailable",
                "verify trusted Git and retry the exact request once",
            )
        })
    };
    checkout_dirty(&layout, &mut run_child).map_err(|_| {
        workspace_unavailable(
            "workspace-git-unavailable",
            "workspace dirty-state inspection is unavailable",
            "verify trusted Git and retry the exact request once",
        )
    })
}

fn has_git_marker(start: &Path) -> Result<bool, HookError> {
    for ancestor in start.ancestors() {
        match fs::symlink_metadata(ancestor.join(".git")) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(identity_unavailable()),
        }
    }
    Ok(false)
}

fn revalidate_managed_identity(state: &State) -> Result<(), HookError> {
    let WorkspaceIdentity::Managed { identity } = &state.identity else {
        return Ok(());
    };
    let layout = git_layout(Path::new(&identity.root)).ok_or_else(identity_unavailable)?;
    let observed = identity_from_layout(&layout)?;
    if &observed != identity {
        return Err(workspace_unavailable(
            "workspace-identity-changed",
            "workspace physical identity changed after binding",
            "release the stale binding and bind the exact current workspace",
        ));
    }
    Ok(())
}

fn workspace_fingerprint_key(state_root: &Path, create: bool) -> Result<String, HookError> {
    let root = state_root.join("workspace-leases");
    crate::paths::ensure_private_state_dir(&root, "workspace-lease-state")?;
    let lock_path = root.join("fingerprint.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&lock_path)
        .map_err(|_| lock_unavailable())?;
    validate_private_file(&lock.metadata().map_err(|_| lock_unavailable())?, 0)?;
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        if io::Error::last_os_error().raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(lock_unavailable());
        }
        if Instant::now() >= deadline {
            return Err(workspace_temporary(
                "workspace-fingerprint-lock-busy",
                "workspace fingerprint state is busy",
                "wait for the bounded lock window and retry the exact request once",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }

    let key_path = root.join("fingerprint.key");
    let key = match read_private_bytes(&key_path, 64)? {
        Some(bytes) => {
            let value = std::str::from_utf8(&bytes).map_err(|_| state_invalid())?;
            if value.len() != 64 || !is_lower_hex(value) {
                return Err(state_invalid());
            }
            value.to_string()
        }
        None if create => {
            let mut retained_state = false;
            for entry in fs::read_dir(&root).map_err(|_| state_unavailable())? {
                let entry = entry.map_err(|_| state_unavailable())?;
                if entry.file_name().to_str().is_some_and(is_hex_digest) {
                    retained_state = true;
                    break;
                }
            }
            if retained_state {
                return Err(state_invalid());
            }
            let value = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
            nils_common::fs::write_atomic(&key_path, value.as_bytes(), 0o600)
                .map_err(|_| state_unavailable())?;
            value
        }
        None => {
            return Err(state_unavailable());
        }
    };
    unsafe {
        libc::flock(lock.as_raw_fd(), libc::LOCK_UN);
    }
    Ok(key)
}

fn lock_workspace(
    state_root: &Path,
    key: &str,
    fingerprint_key: Option<String>,
) -> Result<LockedState, HookError> {
    if !is_hex_digest(key) {
        return Err(wire_invalid());
    }
    let root = state_root.join("workspace-leases");
    crate::paths::ensure_private_state_dir(&root, "workspace-lease-state")?;
    let fingerprint_key = match fingerprint_key {
        Some(fingerprint_key) => fingerprint_key,
        None => workspace_fingerprint_key(state_root, false)?,
    };
    let directory = root.join(key);
    crate::paths::ensure_private_state_dir(&directory, "workspace-lease-state")?;
    let path = directory.join("state.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|_| lock_unavailable())?;
    validate_private_file(&lock.metadata().map_err(|_| lock_unavailable())?, 0)?;
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(lock_unavailable());
        }
        if Instant::now() >= deadline {
            return Err(workspace_temporary(
                "workspace-lease-lock-busy",
                "workspace lease state is busy; retry the exact request",
                "wait for the bounded lock window and retry the exact request once",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(LockedState {
        _lock: lock,
        directory,
        key: key.to_string(),
        fingerprint_key,
    })
}

impl Drop for LockedState {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self._lock.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn read_required_state(locked: &LockedState) -> Result<State, HookError> {
    read_state(locked)?.ok_or_else(stale_binding)
}

fn read_state(locked: &LockedState) -> Result<Option<State>, HookError> {
    let path = locked.directory.join("state.json");
    let Some(bytes) = read_private_bytes(&path, STATE_MAX_BYTES)? else {
        return Ok(None);
    };
    let value = crate::strict_json::from_slice(&bytes).map_err(|_| state_invalid())?;
    let state: State = serde_json::from_value(value).map_err(|_| state_invalid())?;
    validate_loaded_state(&state, locked)?;
    Ok(Some(state))
}

fn write_state(locked: &LockedState, state: &State) -> Result<(), HookError> {
    validate_loaded_state(state, locked)?;
    let bytes = serde_json::to_vec_pretty(state).map_err(|_| state_unavailable())?;
    if bytes.len() as u64 > STATE_MAX_BYTES {
        return Err(workspace_unavailable(
            "workspace-lease-capacity-exhausted",
            "workspace lease state exceeded its conservative capacity",
            "complete or release prior lifecycle state before retrying",
        ));
    }
    nils_common::fs::write_atomic(&locked.directory.join("state.json"), &bytes, 0o600)
        .map_err(|_| state_unavailable())
}

fn read_private_bytes(path: &Path, max: u64) -> Result<Option<Vec<u8>>, HookError> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(state_unavailable()),
    };
    let metadata = file.metadata().map_err(|_| state_unavailable())?;
    validate_private_file(&metadata, max)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| state_unavailable())?;
    if bytes.len() as u64 > max {
        return Err(HookError::data(
            "workspace-lease-state-untrusted",
            "workspace lease state is not a bounded private file",
        ));
    }
    Ok(Some(bytes))
}

fn validate_private_file(metadata: &fs::Metadata, max: u64) -> Result<(), HookError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() != 1
        || (max > 0 && metadata.len() > max)
    {
        return Err(HookError::data(
            "workspace-lease-state-untrusted",
            "workspace lease state is not a bounded private file",
        ));
    }
    Ok(())
}

fn validate_state(
    state: &State,
    locked: &LockedState,
    identity: &WorkspaceIdentity,
) -> Result<(), HookError> {
    validate_loaded_state(state, locked)?;
    if &state.identity != identity {
        return Err(state_invalid());
    }
    Ok(())
}

fn validate_loaded_state(state: &State, locked: &LockedState) -> Result<(), HookError> {
    let expected_key = match &state.identity {
        WorkspaceIdentity::Managed { .. } => {
            keyed_value(&locked.fingerprint_key, "workspace-key", &state.identity)
        }
        WorkspaceIdentity::Unmanaged { root } => keyed_value(
            &locked.fingerprint_key,
            "workspace-unmanaged-key",
            &json!({"session": state.binding.session_digest, "root": root}),
        ),
    }
    .map_err(|_| state_invalid())?;
    if state.schema_version != STATE_SCHEMA
        || state.workspace_key != locked.key
        || state.workspace_key != expected_key
        || !is_hex_digest(&state.workspace_key)
        || !valid_opaque(&state.workspace_id, "wlw1")
        || !valid_binding_id(&state.binding.binding_id, &locked.key)
        || !valid_opaque(&state.binding.generation, "wlg1")
        || !is_hex_digest(&state.binding.session_digest)
        || state
            .binding
            .parent_session_digest
            .as_deref()
            .is_some_and(|value| !is_hex_digest(value))
        || !is_hex_digest(&state.binding.bind_request_digest)
        || !is_hex_digest(&state.binding.bind_request_id_digest)
        || state.binding.refreshed_at_epoch > state.binding.expires_at_epoch
        || state.operations.len() > MAX_OPERATIONS
        || state.tombstones.len() > MAX_TOMBSTONES
    {
        return Err(state_invalid());
    }
    match &state.identity {
        WorkspaceIdentity::Managed { identity } => {
            if !absolute_state_path(&identity.root)
                || !absolute_state_path(&identity.git_dir)
                || !absolute_state_path(&identity.common_dir)
            {
                return Err(state_invalid());
            }
        }
        WorkspaceIdentity::Unmanaged { root } => {
            if root
                .as_deref()
                .is_some_and(|path| !absolute_state_path(path))
            {
                return Err(state_invalid());
            }
        }
    }
    let mut operation_ids = BTreeSet::new();
    let mut fences = BTreeSet::new();
    for operation in &state.operations {
        if !valid_opaque(&operation.operation_id, "wlo1")
            || !valid_opaque(&operation.fence, "wlf1")
            || !operation_ids.insert(&operation.operation_id)
            || !fences.insert(&operation.fence)
            || !is_hex_digest(&operation.request_id_digest)
            || !is_hex_digest(&operation.request_digest)
            || !is_hex_digest(&operation.execution_digest)
            || !is_hex_digest(&operation.completion_execution_digest)
            || operation
                .completion_digest
                .as_deref()
                .is_some_and(|value| !is_hex_digest(value))
            || (operation.status == OperationStatus::Active
                && (operation.completed_at_epoch.is_some()
                    || operation.completion_digest.is_some()))
            || (operation.status != OperationStatus::Active
                && (operation.completed_at_epoch.is_none()
                    || operation.completion_digest.is_none()))
        {
            return Err(state_invalid());
        }
    }
    Ok(())
}

fn absolute_state_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16 * 1024
        && !value.chars().any(|character| character == '\0')
        && Path::new(value).is_absolute()
}

fn bound(state: &State) -> Outcome {
    Outcome {
        data: json!({
            "schema_version": BIND_RESULT_SCHEMA,
            "kind": "bound",
            "binding_id": state.binding.binding_id,
            "workspace_id": state.workspace_id,
            "generation": state.binding.generation,
            "state": state.binding.mode.as_str(),
            "renew_after_ms": RENEW_AFTER_MS,
        }),
        text: format!("workspace lease: {}\n", state.binding.mode.as_str()),
    }
}

fn granted(operation: &OperationRecord) -> Outcome {
    Outcome {
        data: json!({
            "schema_version": BEGIN_RESULT_SCHEMA,
            "kind": "granted",
            "operation_id": operation.operation_id,
            "fence": operation.fence,
        }),
        text: "workspace lease: operation granted\n".to_string(),
    }
}

fn denied(schema: &str, state: &str, code: &str, reason: &str) -> Outcome {
    Outcome {
        data: json!({
            "schema_version": schema,
            "kind": "denied",
            "state": state,
            "code": code,
            "reason": reason,
        }),
        text: format!("workspace lease denied: {code}\n"),
    }
}

fn ack(schema: &str, kind: &str, text: &str) -> Outcome {
    Outcome {
        data: json!({"schema_version": schema, "kind": kind}),
        text: format!("workspace lease: {text}\n"),
    }
}

fn binding_denial(
    state: &State,
    binding_id: &str,
    workspace_id: &str,
    generation: &str,
    session_id: &str,
    parent_session_id: Option<&str>,
) -> Option<Outcome> {
    let session_digest = digest(session_id.as_bytes());
    let exact = exact_binding(
        &state.binding,
        binding_id,
        workspace_id,
        generation,
        &session_digest,
        parent_session_id,
        &state.workspace_id,
    );
    if exact && state.binding.status == BindingStatus::Active {
        return None;
    }
    Some(if state.binding.status == BindingStatus::Active {
        denied(
            BEGIN_RESULT_SCHEMA,
            "foreign-active",
            "WORKSPACE_BINDING_STALE",
            "workspace binding generation no longer owns this workspace",
        )
    } else {
        denied(
            BEGIN_RESULT_SCHEMA,
            "stale-clean",
            "WORKSPACE_BINDING_RELEASED",
            "workspace binding generation was released and must be rebound",
        )
    })
}

fn exact_binding(
    binding: &Binding,
    binding_id: &str,
    workspace_id: &str,
    generation: &str,
    session_digest: &str,
    parent_session_id: Option<&str>,
    expected_workspace_id: &str,
) -> bool {
    binding.binding_id == binding_id
        && binding.generation == generation
        && workspace_id == expected_workspace_id
        && binding.session_digest == session_digest
        && binding.parent_session_digest
            == parent_session_id.map(|parent| digest(parent.as_bytes()))
}

fn require_binding(
    state: &State,
    binding_id: &str,
    workspace_id: &str,
    generation: &str,
    session_id: &str,
    parent_session_id: Option<&str>,
) -> Result<(), HookError> {
    let session_digest = digest(session_id.as_bytes());
    if state.binding.status != BindingStatus::Active
        || !exact_binding(
            &state.binding,
            binding_id,
            workspace_id,
            generation,
            &session_digest,
            parent_session_id,
            &state.workspace_id,
        )
    {
        return Err(stale_binding());
    }
    Ok(())
}

fn expired_denial(state: &State) -> Result<Outcome, HookError> {
    if state
        .operations
        .iter()
        .any(|operation| operation.status == OperationStatus::Active)
    {
        return Ok(denied(
            BEGIN_RESULT_SCHEMA,
            "uncertain",
            "WORKSPACE_OPERATION_UNCERTAIN",
            "workspace lease expired with an operation lacking a terminal outcome",
        ));
    }
    if state.binding.mode == BindingMode::Owned && dirty(managed_identity(&state.identity)?)? {
        return Ok(denied(
            BEGIN_RESULT_SCHEMA,
            "dirty",
            "WORKSPACE_DIRTY",
            "workspace lease expired while the workspace remained dirty",
        ));
    }
    Ok(denied(
        BEGIN_RESULT_SCHEMA,
        "stale-clean",
        "WORKSPACE_LEASE_EXPIRED",
        "workspace lease generation expired and must be rebound",
    ))
}

fn lost_from_denial(mut outcome: Outcome) -> Outcome {
    if let Some(object) = outcome.data.as_object_mut() {
        object.insert("schema_version".to_string(), json!(RENEW_RESULT_SCHEMA));
        object.insert("kind".to_string(), json!("lost"));
    }
    outcome.text = "workspace lease lost\n".to_string();
    outcome
}

fn execution_digest(request: &BeginRequest) -> Result<String, HookError> {
    digest_value(
        "workspace-execution",
        &json!({
            "call_id": request.call_id,
            "root_call_id": request.root_call_id,
            "tool_name": request.tool_name,
            "arguments": request.arguments,
            "nested": request.nested,
        }),
    )
}

fn tool_is_read_only(request: &BeginRequest) -> bool {
    match request.tool_name.as_str() {
        "Read" | "read" | "Glob" | "glob" | "Grep" | "grep" => true,
        "str_replace_editor" => request
            .arguments
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| command == "view"),
        _ => false,
    }
}

fn compact_operations(operations: &mut Vec<OperationRecord>) {
    let mut terminal_to_remove = operations.len().saturating_sub(MAX_OPERATIONS - 1);
    operations.retain(|operation| {
        if terminal_to_remove > 0 && operation.status != OperationStatus::Active {
            terminal_to_remove -= 1;
            false
        } else {
            true
        }
    });
}

fn compact_tombstones(tombstones: &mut Vec<BindingTombstone>) {
    if tombstones.len() > MAX_TOMBSTONES {
        tombstones.drain(..tombstones.len() - MAX_TOMBSTONES);
    }
}

fn tombstone(state: &State) -> BindingTombstone {
    BindingTombstone {
        binding_id: state.binding.binding_id.clone(),
        generation: state.binding.generation.clone(),
        session_digest: state.binding.session_digest.clone(),
        workspace_id: state.workspace_id.clone(),
    }
}

fn binding_key(binding_id: &str) -> Result<&str, HookError> {
    let mut parts = binding_id.split('.');
    let prefix = parts.next();
    let key = parts.next();
    let nonce = parts.next();
    if prefix != Some("wlb1")
        || key.is_none_or(|value| !is_hex_digest(value))
        || nonce.is_none_or(|value| value.len() != 32 || !is_lower_hex(value))
        || parts.next().is_some()
    {
        return Err(wire_invalid());
    }
    key.ok_or_else(wire_invalid)
}

fn valid_binding_id(value: &str, key: &str) -> bool {
    binding_key(value).is_ok_and(|observed| observed == key)
}

fn valid_opaque(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(prefix)
        .and_then(|value| value.strip_prefix('.'))
        .is_some_and(|value| value.len() == 32 && is_lower_hex(value))
}

fn opaque(prefix: &str) -> String {
    format!("{prefix}.{}", Uuid::new_v4().simple())
}

fn path_text(path: &Path) -> Result<String, HookError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(identity_unavailable)
}

fn digest_value(domain: &str, value: &impl Serialize) -> Result<String, HookError> {
    let bytes = serde_json::to_vec(value).map_err(|_| wire_invalid())?;
    let mut material = domain.as_bytes().to_vec();
    material.push(0);
    material.extend_from_slice(&bytes);
    Ok(digest(&material))
}

fn keyed_value(key: &str, domain: &str, value: &impl Serialize) -> Result<String, HookError> {
    let bytes = serde_json::to_vec(value).map_err(|_| wire_invalid())?;
    let mut material = domain.as_bytes().to_vec();
    material.push(0);
    material.extend_from_slice(&bytes);
    nils_common::coordination_projection::keyed_digest(key.as_bytes(), &material)
        .ok_or_else(state_invalid)
}

fn digest(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64 && is_lower_hex(value)
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn now_epoch() -> Result<u64, HookError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| {
            workspace_unavailable(
                "workspace-clock-unavailable",
                "workspace lease clock is unavailable",
                "restore the system clock and retry the exact request once",
            )
        })
}

fn wire_invalid() -> HookError {
    HookError::data(
        "workspace-wire-invalid",
        "workspace lease request does not match the strict protocol",
    )
}

fn identity_unavailable() -> HookError {
    workspace_unavailable(
        "workspace-identity-unavailable",
        "workspace identity could not be resolved",
        "restore the exact workspace and retry the exact request once",
    )
}

fn lock_unavailable() -> HookError {
    workspace_unavailable(
        "workspace-lease-lock-unavailable",
        "workspace lease lock is unavailable",
        "verify the private state directory and retry the exact request once",
    )
}

fn state_unavailable() -> HookError {
    workspace_unavailable(
        "workspace-lease-state-unavailable",
        "workspace lease state is unavailable",
        "verify the private state directory and retry the exact request once",
    )
}

fn workspace_unavailable(code: &str, message: &str, next_action: &str) -> HookError {
    HookError::unavailable_with(code, message, recoverable_details(next_action))
}

fn workspace_temporary(code: &str, message: &str, next_action: &str) -> HookError {
    HookError::temporary_with(code, message, recoverable_details(next_action))
}

fn recoverable_details(next_action: &str) -> Value {
    json!({
        "retryable": true,
        "next_action": next_action,
        "recovery": {
            "kind": "bounded-retry",
            "max_attempts": 1,
        },
    })
}

fn state_invalid() -> HookError {
    HookError::data(
        "workspace-lease-state-invalid",
        "workspace lease state invariants are invalid",
    )
}

fn stale_binding() -> HookError {
    HookError::data(
        "workspace-binding-stale",
        "workspace binding generation no longer owns this workspace",
    )
}

fn idempotency_reused() -> HookError {
    HookError::data(
        "workspace-idempotency-key-reused",
        "workspace request id is already bound to different facts",
    )
}

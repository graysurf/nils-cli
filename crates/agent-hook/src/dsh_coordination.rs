//! Native DSH operation-lifecycle bridge to `agent-session`.
//!
//! The bridge stores only hashed provider correlation plus the exact private
//! lease material required for idempotent recovery. Tool arguments and tool
//! output never enter this state.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::HookError;
use crate::model::{NormalizedRequest, OperationEffectClass};

const STATE_SCHEMA: &str = "agent-hook.dsh-operation.v1";
const TARGETS_SCHEMA: &str = "agent-session.operation-targets.v1";
const CONTEXT_SCHEMA: &str = "agent-session.work-context.v1";
const LEASE_SCHEMA: &str = "agent-session.operation-lease.v1";
const MAX_STATE_BYTES: u64 = 64 * 1024;
const MAX_TERMINAL_OPERATIONS: usize = 64;
const MAX_OPERATION_DIRECTORIES: usize = 128;
const TERMINAL_SEQUENCE_FILE: &str = "terminal.sequence";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    NotRun,
    Clean,
    Pending,
    Unavailable,
}

#[derive(Debug)]
pub(crate) struct Outcome {
    pub(crate) status: Status,
    pub(crate) message: Option<String>,
}

impl Outcome {
    fn clean() -> Self {
        Self {
            status: Status::Clean,
            message: None,
        }
    }

    fn not_run() -> Self {
        Self {
            status: Status::NotRun,
            message: None,
        }
    }

    fn pending(message: &'static str) -> Self {
        Self {
            status: Status::Pending,
            message: Some(message.to_string()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: String,
    session_digest: String,
    call_digest: String,
    request_digest: String,
    target_digest: String,
    tool_name: String,
    operation: String,
    phase: Phase,
    claim_id: String,
    claim_revision: u64,
    repository: String,
    checkout: String,
    admit_idempotency_key: String,
    complete_idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lease_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<TerminalOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_sequence: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Phase {
    Admitting,
    Active,
    Completing,
    Terminal,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum TerminalOutcome {
    Pass,
    Fail,
}

impl TerminalOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

struct Identity {
    session_id: String,
    capability_file: PathBuf,
    state_dir: PathBuf,
}

struct OperationPaths {
    root: PathBuf,
    directory: PathBuf,
    state: PathBuf,
    targets: PathBuf,
    token: PathBuf,
    _lock: fs::File,
}

struct CallFailure {
    certain: bool,
}

pub(crate) fn run(
    request: &NormalizedRequest,
    raw: &[u8],
    effect: OperationEffectClass,
    run_child: &mut dyn FnMut(Command) -> Result<Output, HookError>,
) -> Result<Outcome, HookError> {
    let Some(identity) = identity()? else {
        return Ok(Outcome::not_run());
    };
    match request.event.as_str() {
        "PreToolUse" => pre_tool(request, effect, &identity, run_child),
        "PostToolUse" | "PostToolUseFailure" => post_tool(request, raw, &identity, run_child),
        "Stop" => stop(&identity, run_child),
        _ => Ok(Outcome::not_run()),
    }
}

fn identity() -> Result<Option<Identity>, HookError> {
    let session_id = env_nonempty("AGENT_SESSION_ID");
    let capability_file = env_nonempty("AGENT_SESSION_CAPABILITY_FILE").map(PathBuf::from);
    let state_dir = env_nonempty("AGENT_SESSION_STATE_DIR").map(PathBuf::from);
    let any_selector = session_id.is_some()
        || capability_file.is_some()
        || state_dir.is_some()
        || env_nonempty("AGENT_SESSION_RUNTIME_ID").is_some()
        || env_nonempty("AGENT_SESSION_BIN").is_some();
    if !any_selector {
        return Ok(None);
    }
    let (Some(session_id), Some(capability_file), Some(state_dir)) =
        (session_id, capability_file, state_dir)
    else {
        return Err(HookError::data(
            "operation-lifecycle-identity-incomplete",
            "managed DSH operation lifecycle requires session, capability, and state identity",
        ));
    };
    if !capability_file.is_absolute() || !state_dir.is_absolute() {
        return Err(HookError::data(
            "operation-lifecycle-identity-invalid",
            "managed DSH operation lifecycle paths must be absolute",
        ));
    }
    Ok(Some(Identity {
        session_id,
        capability_file,
        state_dir,
    }))
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn pre_tool(
    request: &NormalizedRequest,
    effect: OperationEffectClass,
    identity: &Identity,
    run_child: &mut dyn FnMut(Command) -> Result<Output, HookError>,
) -> Result<Outcome, HookError> {
    if effect == OperationEffectClass::ReadOnly {
        return Ok(Outcome::not_run());
    }
    let paths = operation_paths(request, true)?.ok_or_else(state_unavailable)?;
    if let Some(record) = read_record(&paths.state)? {
        validate_record(request, &record)?;
        return Ok(Outcome::pending(match record.phase {
            Phase::Admitting => {
                "operation admission is already pending or uncertain; reconcile it before issuing another mutation"
            }
            Phase::Active => {
                "this operation identity was already admitted and cannot authorize a second execution"
            }
            Phase::Completing | Phase::Terminal => {
                "this completed operation identity cannot authorize another mutation"
            }
        }));
    }

    let context = show_context(identity, run_child).map_err(|_| {
        HookError::runtime(
            "operation-lifecycle-context-unavailable",
            "the active managed work context is unavailable",
        )
    })?;
    let (repository, checkout, targets, operation) = operation_targets(request, &context)?;
    write_json(&paths.targets, &targets)?;
    write_private(&paths.token, uuid::Uuid::new_v4().to_string().as_bytes())?;
    let call_digest = operation_call_digest(request)?;
    let mut record = Record {
        schema_version: STATE_SCHEMA.to_string(),
        session_digest: digest(subject_session(request)?.as_bytes()),
        call_digest: call_digest.clone(),
        request_digest: request.command_digest.clone(),
        target_digest: request.target_digest.clone(),
        tool_name: request.matcher.clone().unwrap_or_default(),
        operation,
        phase: Phase::Admitting,
        claim_id: context.claim_id,
        claim_revision: context.revision,
        repository,
        checkout,
        admit_idempotency_key: format!("dsh-admit-{}", &call_digest[..32]),
        complete_idempotency_key: format!("dsh-complete-{}", &call_digest[..32]),
        lease_id: None,
        lease_revision: None,
        outcome: None,
        terminal_sequence: None,
    };
    write_record(&paths.state, &record)?;
    match admit(identity, &paths, &mut record, run_child) {
        Ok(()) => Ok(Outcome::clean()),
        Err(error) if error.certain => {
            clear_provisional(paths)?;
            Ok(Outcome::pending(
                "operation admission was denied; resolve the active claim and retry",
            ))
        }
        Err(_) => Ok(Outcome::pending(
            "operation admission outcome is uncertain; retry the exact tool call or reconcile the session",
        )),
    }
}

fn post_tool(
    request: &NormalizedRequest,
    _raw: &[u8],
    identity: &Identity,
    run_child: &mut dyn FnMut(Command) -> Result<Output, HookError>,
) -> Result<Outcome, HookError> {
    let Some(paths) = operation_paths(request, false)? else {
        return Ok(Outcome::not_run());
    };
    let Some(mut record) = read_record(&paths.state)? else {
        return Ok(Outcome::not_run());
    };
    validate_record(request, &record)?;
    let observed = if request.event == "PostToolUseFailure" {
        TerminalOutcome::Fail
    } else {
        TerminalOutcome::Pass
    };
    if record.phase == Phase::Terminal {
        if record.outcome != Some(observed) {
            return Ok(Outcome::pending(
                "the repeated post-tool outcome conflicts with the persisted terminal operation",
            ));
        }
        return match complete(identity, &paths, &mut record, run_child) {
            Ok(()) => Ok(Outcome::clean()),
            Err(_) => Ok(Outcome::pending(
                "the terminal operation could not be reauthenticated; Stop remains blocked",
            )),
        };
    }
    if record.phase == Phase::Admitting && admit(identity, &paths, &mut record, run_child).is_err()
    {
        return Ok(Outcome::pending(
            "operation admission is still uncertain after tool completion",
        ));
    }
    if record.outcome.is_some_and(|outcome| outcome != observed) {
        return Ok(Outcome::pending(
            "the repeated post-tool outcome conflicts with the persisted completion attempt",
        ));
    }
    record.outcome = Some(observed);
    record.phase = Phase::Completing;
    write_record(&paths.state, &record)?;
    match complete(identity, &paths, &mut record, run_child) {
        Ok(()) => Ok(Outcome::clean()),
        Err(_) => Ok(Outcome::pending(
            "operation completion is uncertain; Stop remains blocked until exact reconciliation succeeds",
        )),
    }
}

fn stop(
    identity: &Identity,
    run_child: &mut dyn FnMut(Command) -> Result<Output, HookError>,
) -> Result<Outcome, HookError> {
    match broker_operations_quiescent(identity, run_child) {
        Ok(true) => Ok(Outcome::clean()),
        Ok(false) => Ok(Outcome::pending(
            "agent-session still reports an active or uncertain managed mutation",
        )),
        Err(_) => Ok(Outcome {
            status: Status::Unavailable,
            message: Some(
                "authoritative agent-session operation state could not be verified".to_string(),
            ),
        }),
    }
}

#[derive(Debug)]
struct ContextProjection {
    claim_id: String,
    revision: u64,
    repositories: Vec<String>,
}

fn broker_operations_quiescent(
    identity: &Identity,
    run_child: &mut dyn FnMut(Command) -> Result<Output, HookError>,
) -> Result<bool, CallFailure> {
    let mut command =
        agent_session_command(identity).map_err(|_| CallFailure { certain: false })?;
    command.args([
        "broker",
        "status",
        "--session",
        &identity.session_id,
        "--capability-file",
        identity.capability_file.to_string_lossy().as_ref(),
        "--authenticated",
        "--format",
        "json",
    ]);
    let status = call(command, "cli.agent-session.broker-status.v1", run_child)?;
    exact_object(
        &status,
        &[
            "schema_version",
            "session_id",
            "state",
            "generation",
            "capability_available",
            "heartbeat_fresh",
            "claim",
            "operation",
        ],
    )?;
    if status.get("schema_version").and_then(Value::as_str)
        != Some("agent-session.coordination-broker.v1")
        || status.get("session_id").and_then(Value::as_str) != Some(identity.session_id.as_str())
        || status.get("state").and_then(Value::as_str).is_none()
        || status.get("generation").and_then(Value::as_u64).is_none()
        || status.get("capability_available").and_then(Value::as_bool) != Some(true)
        || status
            .get("heartbeat_fresh")
            .and_then(Value::as_bool)
            .is_none()
        || !(status.get("claim").is_some_and(Value::is_null)
            || status.get("claim").is_some_and(Value::is_object))
    {
        return Err(CallFailure { certain: true });
    }
    let operation = status
        .get("operation")
        .ok_or(CallFailure { certain: true })?;
    exact_object(operation, &["active", "uncertain"])?;
    let active = operation
        .get("active")
        .and_then(Value::as_u64)
        .ok_or(CallFailure { certain: true })?;
    let uncertain = operation
        .get("uncertain")
        .and_then(Value::as_u64)
        .ok_or(CallFailure { certain: true })?;
    Ok(active == 0 && uncertain == 0)
}

fn show_context(
    identity: &Identity,
    run_child: &mut dyn FnMut(Command) -> Result<Output, HookError>,
) -> Result<ContextProjection, CallFailure> {
    let mut command =
        agent_session_command(identity).map_err(|_| CallFailure { certain: false })?;
    command.args([
        "work-context",
        "show",
        "--session",
        &identity.session_id,
        "--capability-file",
        identity.capability_file.to_string_lossy().as_ref(),
        "--format",
        "json",
    ]);
    let data = call(command, "cli.agent-session.work-context-show.v1", run_child)?;
    exact_object(
        &data,
        &[
            "schema_version",
            "session_id",
            "session_incarnation",
            "claim_id",
            "revision",
            "state",
            "intent",
            "tier",
            "repositories",
            "worktrees",
            "provider_refs",
            "plan_refs",
            "scopes",
            "summary",
            "updated_at",
            "expires_at",
        ],
    )?;
    if data.get("schema_version").and_then(Value::as_str) != Some(CONTEXT_SCHEMA)
        || data.get("state").and_then(Value::as_str) != Some("active")
    {
        return Err(CallFailure { certain: true });
    }
    let claim_id = bounded_string(&data, "claim_id")?;
    let revision = data
        .get("revision")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or(CallFailure { certain: true })?;
    let repositories = data
        .get("repositories")
        .and_then(Value::as_array)
        .ok_or(CallFailure { certain: true })?
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty() && value.len() <= 512)
                .map(str::to_string)
                .ok_or(CallFailure { certain: true })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ContextProjection {
        claim_id,
        revision,
        repositories,
    })
}

fn admit(
    identity: &Identity,
    paths: &OperationPaths,
    record: &mut Record,
    run_child: &mut dyn FnMut(Command) -> Result<Output, HookError>,
) -> Result<(), CallFailure> {
    let mut command =
        agent_session_command(identity).map_err(|_| CallFailure { certain: false })?;
    command.args([
        "work-context",
        "admit",
        "--session",
        &identity.session_id,
        "--claim",
        &record.claim_id,
        "--if-revision",
        &record.claim_revision.to_string(),
        "--targets-file",
        paths.targets.to_string_lossy().as_ref(),
        "--operation",
        &record.operation,
        "--execution-token-file",
        paths.token.to_string_lossy().as_ref(),
        "--capability-file",
        identity.capability_file.to_string_lossy().as_ref(),
        "--idempotency-key",
        &record.admit_idempotency_key,
        "--format",
        "json",
    ]);
    let lease = call(
        command,
        "cli.agent-session.work-context-admit.v1",
        run_child,
    )?;
    validate_lease(&lease, &record.operation, &record.claim_id, "active", None)?;
    record.lease_id = Some(bounded_string(&lease, "lease_id")?);
    record.lease_revision = lease.get("revision").and_then(Value::as_u64);
    if record.lease_revision.is_none_or(|revision| revision == 0) {
        return Err(CallFailure { certain: true });
    }
    record.phase = Phase::Active;
    write_record(&paths.state, record).map_err(|_| CallFailure { certain: false })
}

fn complete(
    identity: &Identity,
    paths: &OperationPaths,
    record: &mut Record,
    run_child: &mut dyn FnMut(Command) -> Result<Output, HookError>,
) -> Result<(), CallFailure> {
    let lease_id = record
        .lease_id
        .as_deref()
        .ok_or(CallFailure { certain: false })?;
    let revision = record
        .lease_revision
        .ok_or(CallFailure { certain: false })?;
    let outcome = record.outcome.ok_or(CallFailure { certain: false })?;
    let mut command =
        agent_session_command(identity).map_err(|_| CallFailure { certain: false })?;
    command.args([
        "work-context",
        "complete",
        "--session",
        &identity.session_id,
        "--lease",
        lease_id,
        "--if-revision",
        &revision.to_string(),
        "--execution-token-file",
        paths.token.to_string_lossy().as_ref(),
        "--outcome",
        outcome.as_str(),
        "--capability-file",
        identity.capability_file.to_string_lossy().as_ref(),
        "--idempotency-key",
        &record.complete_idempotency_key,
        "--format",
        "json",
    ]);
    let lease = call(
        command,
        "cli.agent-session.work-context-complete.v1",
        run_child,
    )?;
    let expected_state = if outcome == TerminalOutcome::Pass {
        "completed"
    } else {
        "failed"
    };
    validate_lease(
        &lease,
        &record.operation,
        &record.claim_id,
        expected_state,
        Some(outcome.as_str()),
    )?;
    if lease.get("lease_id").and_then(Value::as_str) != Some(lease_id) {
        return Err(CallFailure { certain: true });
    }
    let terminal_revision = lease.get("revision").and_then(Value::as_u64);
    if terminal_revision.is_none_or(|value| value == 0) {
        return Err(CallFailure { certain: true });
    }
    if record.terminal_sequence.is_none() {
        record.terminal_sequence =
            Some(next_terminal_sequence(&paths.root).map_err(|_| CallFailure { certain: false })?);
    }
    record.phase = Phase::Terminal;
    write_record(&paths.state, record).map_err(|_| CallFailure { certain: false })
}

fn validate_lease(
    lease: &Value,
    operation: &str,
    claim_id: &str,
    state: &str,
    outcome: Option<&str>,
) -> Result<(), CallFailure> {
    let allowed = [
        "schema_version",
        "lease_id",
        "session_id",
        "session_incarnation",
        "claim_id",
        "claim_revision",
        "operation",
        "targets",
        "provider_targets",
        "state",
        "revision",
        "started_at",
        "expires_at",
        "reconcile_observed_at_epoch",
        "outcome",
    ];
    allowed_object(lease, &allowed)?;
    if lease.get("schema_version").and_then(Value::as_str) != Some(LEASE_SCHEMA)
        || lease.get("operation").and_then(Value::as_str) != Some(operation)
        || lease.get("claim_id").and_then(Value::as_str) != Some(claim_id)
        || lease.get("state").and_then(Value::as_str) != Some(state)
        || lease.get("outcome").and_then(Value::as_str) != outcome
        || bounded_string(lease, "lease_id").is_err()
        || lease
            .get("revision")
            .and_then(Value::as_u64)
            .is_none_or(|value| value == 0)
    {
        return Err(CallFailure { certain: true });
    }
    Ok(())
}

fn call(
    command: Command,
    schema: &str,
    run_child: &mut dyn FnMut(Command) -> Result<Output, HookError>,
) -> Result<Value, CallFailure> {
    let output = run_child(command).map_err(|_| CallFailure { certain: false })?;
    let value = crate::strict_json::from_slice(&output.stdout)
        .map_err(|_| CallFailure { certain: false })?;
    let object = value.as_object().ok_or(CallFailure { certain: false })?;
    if output.status.success() {
        if object.len() != 3
            || object.get("schema_version").and_then(Value::as_str) != Some(schema)
            || object.get("ok").and_then(Value::as_bool) != Some(true)
        {
            return Err(CallFailure { certain: false });
        }
        object
            .get("data")
            .cloned()
            .ok_or(CallFailure { certain: false })
    } else if object.len() == 3
        && object.get("schema_version").and_then(Value::as_str) == Some(schema)
        && object.get("ok").and_then(Value::as_bool) == Some(false)
        && object.get("error").is_some_and(Value::is_object)
    {
        Err(CallFailure { certain: true })
    } else {
        Err(CallFailure { certain: false })
    }
}

fn exact_object(value: &Value, keys: &[&str]) -> Result<(), CallFailure> {
    let object = value.as_object().ok_or(CallFailure { certain: true })?;
    let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = keys.iter().copied().collect::<BTreeSet<_>>();
    if actual == expected {
        Ok(())
    } else {
        Err(CallFailure { certain: true })
    }
}

fn allowed_object(value: &Value, keys: &[&str]) -> Result<(), CallFailure> {
    let object = value.as_object().ok_or(CallFailure { certain: true })?;
    let allowed = keys.iter().copied().collect::<BTreeSet<_>>();
    if object.keys().all(|key| allowed.contains(key.as_str())) {
        Ok(())
    } else {
        Err(CallFailure { certain: true })
    }
}

fn bounded_string(value: &Value, key: &str) -> Result<String, CallFailure> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 512)
        .map(str::to_string)
        .ok_or(CallFailure { certain: true })
}

fn agent_session_command(identity: &Identity) -> Result<Command, HookError> {
    let executable = trusted_sibling("agent-session")?;
    let mut command = Command::new(executable);
    command
        .args(["--state-dir", identity.state_dir.to_string_lossy().as_ref()])
        .env_clear()
        .env("HOME", "/nonexistent")
        .env("LC_ALL", "C")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    Ok(command)
}

fn trusted_sibling(name: &str) -> Result<PathBuf, HookError> {
    let current = std::env::current_exe()
        .ok()
        .and_then(|path| fs::canonicalize(path).ok())
        .ok_or_else(|| {
            HookError::runtime(
                "operation-lifecycle-helper-unavailable",
                "agent-hook executable is unavailable",
            )
        })?;
    let candidate = current
        .parent()
        .map(|parent| parent.join(name))
        .and_then(|path| fs::canonicalize(path).ok())
        .ok_or_else(|| {
            HookError::runtime(
                "operation-lifecycle-helper-unavailable",
                "same-release agent-session is unavailable",
            )
        })?;
    let metadata = fs::metadata(&candidate).map_err(|_| {
        HookError::runtime(
            "operation-lifecycle-helper-unavailable",
            "same-release agent-session is unavailable",
        )
    })?;
    if candidate.parent() != current.parent()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.permissions().mode() & 0o111 == 0
    {
        return Err(HookError::data(
            "operation-lifecycle-helper-untrusted",
            "same-release agent-session is not an owner-controlled executable",
        ));
    }
    Ok(candidate)
}

fn operation_targets(
    request: &NormalizedRequest,
    context: &ContextProjection,
) -> Result<(String, String, Value, String), HookError> {
    let [repository] = context.repositories.as_slice() else {
        return Err(HookError::data(
            "operation-lifecycle-repository-ambiguous",
            "the active claim must name exactly one repository",
        ));
    };
    let mut roots = request
        .binding_roots
        .iter()
        .filter_map(|root| fs::canonicalize(root).ok())
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    let [root] = roots.as_slice() else {
        return Err(HookError::data(
            "operation-lifecycle-checkout-ambiguous",
            "the mutation must bind to exactly one canonical checkout",
        ));
    };
    let (targets, operation) = if request.matcher.as_deref() == Some("bash") {
        (
            vec![json!({"kind": "repository", "repository": repository, "value": "."})],
            "shell".to_string(),
        )
    } else if matches!(
        request.matcher.as_deref(),
        Some("write" | "edit" | "str_replace_editor")
    ) && !request.target_paths.is_empty()
    {
        let mut targets = Vec::new();
        for target in &request.target_paths {
            let relative = target.strip_prefix(root).map_err(|_| {
                HookError::data(
                    "operation-lifecycle-target-outside-checkout",
                    "the mutation target is outside its canonical checkout",
                )
            })?;
            if relative.as_os_str().is_empty()
                || relative
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
            {
                return Err(HookError::data(
                    "operation-lifecycle-target-invalid",
                    "the mutation target is not an exact repository-relative path",
                ));
            }
            targets.push(json!({
                "kind": "path-exact",
                "repository": repository,
                "value": relative.to_string_lossy(),
            }));
        }
        (targets, "file-write".to_string())
    } else {
        return Err(HookError::data(
            "operation-lifecycle-target-unknown",
            "the mutation target cannot be proven for operation admission",
        ));
    };
    let checkout = root.to_string_lossy().into_owned();
    Ok((
        repository.clone(),
        checkout.clone(),
        json!({
            "schema_version": TARGETS_SCHEMA,
            "targets": targets,
            "provider_refs": [],
            "checkouts": [{"repository": repository, "path": checkout}],
        }),
        operation,
    ))
}

fn operation_paths(
    request: &NormalizedRequest,
    create: bool,
) -> Result<Option<OperationPaths>, HookError> {
    let subject = request.dsh_subject.as_ref().ok_or_else(|| {
        HookError::data(
            "operation-lifecycle-identity-incomplete",
            "DSH subject is unavailable",
        )
    })?;
    let base = operations_root(&subject.agent_docs_state_home);
    let root = base.join(digest(subject.session_id.as_bytes()));
    if !create {
        if !private_directory_exists(&root)? {
            return Ok(None);
        }
    } else {
        ensure_private_directory(&base)?;
        ensure_private_directory(&root)?;
    }
    let directory = root.join(operation_call_digest(request)?);
    if !create {
        if !private_directory_exists(&directory)? {
            return Ok(None);
        }
    } else {
        let _session_lock = open_private_lock(&root.join("session.lock"), true, false)?
            .ok_or_else(state_unavailable)?;
        if !private_directory_exists(&directory)? {
            compact_terminal_operations(&root)?;
            if operation_directory_count(&root)? >= MAX_OPERATION_DIRECTORIES {
                return Err(HookError::runtime(
                    "operation-lifecycle-capacity-exhausted",
                    "operation lifecycle state is at its conservative capacity; finish or reconcile pending operations before retrying",
                ));
            }
            ensure_private_directory(&directory)?;
        }
    }
    let paths = paths_for_directory_without_lock(directory.clone());
    let lock = open_private_lock(&directory.join("operation.lock"), create, false)?
        .ok_or_else(state_unavailable)?;
    Ok(Some(OperationPaths {
        root,
        directory,
        state: paths.0,
        targets: paths.1,
        token: paths.2,
        _lock: lock,
    }))
}

fn paths_for_directory_without_lock(directory: PathBuf) -> (PathBuf, PathBuf, PathBuf) {
    (
        directory.join("state.json"),
        directory.join("targets.json"),
        directory.join("execution-token"),
    )
}

fn operations_root(state_home: &Path) -> PathBuf {
    state_home.join("agent-hook/dsh-operations")
}

fn ensure_private_directory(path: &Path) -> Result<(), HookError> {
    if private_directory_exists(path)? {
        return Ok(());
    }
    fs::create_dir_all(path).map_err(|_| state_unavailable())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| state_unavailable())?;
    let metadata = fs::symlink_metadata(path).map_err(|_| state_unavailable())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(HookError::data(
            "operation-lifecycle-state-untrusted",
            "operation lifecycle state is not a private owner-controlled directory",
        ));
    }
    Ok(())
}

fn private_directory_exists(path: &Path) -> Result<bool, HookError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(state_unavailable()),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(HookError::data(
            "operation-lifecycle-state-untrusted",
            "operation lifecycle state is not a private owner-controlled directory",
        ));
    }
    Ok(true)
}

fn open_private_lock(
    path: &Path,
    create: bool,
    nonblocking: bool,
) -> Result<Option<fs::File>, HookError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(state_unavailable()),
    };
    let metadata = file.metadata().map_err(|_| state_unavailable())?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(HookError::data(
            "operation-lifecycle-state-untrusted",
            "operation lifecycle lock is not a private owner-controlled file",
        ));
    }
    let mut flags = libc::LOCK_EX;
    if nonblocking {
        flags |= libc::LOCK_NB;
    }
    if unsafe { libc::flock(file.as_raw_fd(), flags) } != 0 {
        if nonblocking {
            return Ok(None);
        }
        return Err(state_unavailable());
    }
    Ok(Some(file))
}

fn operation_directory_count(root: &Path) -> Result<usize, HookError> {
    let mut count = 0usize;
    for entry in fs::read_dir(root).map_err(|_| state_unavailable())? {
        let entry = entry.map_err(|_| state_unavailable())?;
        if entry.file_name() == "session.lock" || entry.file_name() == TERMINAL_SEQUENCE_FILE {
            continue;
        }
        if !private_directory_exists(&entry.path())? {
            return Err(state_unavailable());
        }
        count = count.saturating_add(1);
    }
    Ok(count)
}

fn compact_terminal_operations(root: &Path) -> Result<(), HookError> {
    let mut terminal = Vec::new();
    for entry in fs::read_dir(root).map_err(|_| state_unavailable())? {
        let entry = entry.map_err(|_| state_unavailable())?;
        if entry.file_name() == "session.lock" || entry.file_name() == TERMINAL_SEQUENCE_FILE {
            continue;
        }
        let directory = entry.path();
        if !private_directory_exists(&directory)? {
            return Err(state_unavailable());
        }
        let state = directory.join("state.json");
        let Some(record) = read_record(&state)? else {
            continue;
        };
        if record.phase == Phase::Terminal {
            let sequence = record.terminal_sequence.ok_or_else(|| {
                HookError::data(
                    "operation-lifecycle-state-invalid",
                    "terminal operation state is missing its durable sequence",
                )
            })?;
            terminal.push((sequence, directory));
        }
    }
    terminal.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let prune = terminal
        .len()
        .saturating_add(1)
        .saturating_sub(MAX_TERMINAL_OPERATIONS);
    for (_, directory) in terminal.into_iter().take(prune) {
        let _ = prune_terminal_operation(&directory)?;
    }
    Ok(())
}

fn prune_terminal_operation(directory: &Path) -> Result<bool, HookError> {
    let Some(lock) = open_private_lock(&directory.join("operation.lock"), false, true)? else {
        return Ok(false);
    };
    let state = directory.join("state.json");
    if read_record(&state)?.is_none_or(|record| record.phase != Phase::Terminal) {
        return Ok(false);
    }
    let allowed = BTreeSet::from([
        "execution-token".to_string(),
        "operation.lock".to_string(),
        "state.json".to_string(),
        "targets.json".to_string(),
    ]);
    let observed = fs::read_dir(directory)
        .map_err(|_| state_unavailable())?
        .map(|entry| {
            entry.map_err(|_| state_unavailable()).and_then(|entry| {
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| state_unavailable())
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if !observed.is_subset(&allowed) {
        return Err(HookError::data(
            "operation-lifecycle-state-untrusted",
            "operation lifecycle directory contains unexpected state",
        ));
    }
    for name in [
        "state.json",
        "targets.json",
        "execution-token",
        "operation.lock",
    ] {
        match fs::remove_file(directory.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(state_unavailable()),
        }
    }
    fs::remove_dir(directory).map_err(|_| state_unavailable())?;
    drop(lock);
    Ok(true)
}

fn read_record(path: &Path) -> Result<Option<Record>, HookError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(state_unavailable()),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.len() > MAX_STATE_BYTES
    {
        return Err(HookError::data(
            "operation-lifecycle-state-untrusted",
            "operation lifecycle record is not a bounded private file",
        ));
    }
    let value = fs::read(path).map_err(|_| state_unavailable())?;
    let value = crate::strict_json::from_slice(&value).map_err(|_| {
        HookError::data(
            "operation-lifecycle-state-invalid",
            "operation lifecycle record is malformed",
        )
    })?;
    let record: Record = serde_json::from_value(value).map_err(|_| {
        HookError::data(
            "operation-lifecycle-state-invalid",
            "operation lifecycle record is malformed",
        )
    })?;
    if record.schema_version != STATE_SCHEMA {
        return Err(HookError::data(
            "operation-lifecycle-state-invalid",
            "operation lifecycle record schema is unsupported",
        ));
    }
    Ok(Some(record))
}

fn validate_record(request: &NormalizedRequest, record: &Record) -> Result<(), HookError> {
    if record.session_digest != digest(subject_session(request)?.as_bytes())
        || record.call_digest != operation_call_digest(request)?
        || record.request_digest != request.command_digest
        || record.target_digest != request.target_digest
        || request.matcher.as_deref() != Some(record.tool_name.as_str())
    {
        return Err(HookError::data(
            "operation-lifecycle-correlation-invalid",
            "operation lifecycle correlation does not match the persisted operation",
        ));
    }
    Ok(())
}

fn subject_session(request: &NormalizedRequest) -> Result<&str, HookError> {
    request
        .dsh_subject
        .as_ref()
        .map(|subject| subject.session_id.as_str())
        .ok_or_else(|| {
            HookError::data(
                "operation-lifecycle-identity-incomplete",
                "DSH subject is unavailable",
            )
        })
}

fn operation_call_digest(request: &NormalizedRequest) -> Result<String, HookError> {
    request
        .dsh_subject
        .as_ref()
        .and_then(|subject| subject.call_id.as_deref())
        .map(|value| digest(value.as_bytes()))
        .ok_or_else(|| {
            HookError::data(
                "operation-lifecycle-correlation-invalid",
                "DSH operation request identity is invalid",
            )
        })
}

fn write_record(path: &Path, record: &Record) -> Result<(), HookError> {
    let bytes = serde_json::to_vec(record).map_err(|_| state_unavailable())?;
    write_private(path, &bytes)
}

fn write_json(path: &Path, value: &Value) -> Result<(), HookError> {
    let bytes = serde_json::to_vec(value).map_err(|_| state_unavailable())?;
    write_private(path, &bytes)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), HookError> {
    nils_common::fs::write_atomic(path, bytes, 0o600).map_err(|_| state_unavailable())
}

fn clear_provisional(paths: OperationPaths) -> Result<(), HookError> {
    let _session_lock = open_private_lock(&paths.root.join("session.lock"), true, false)?
        .ok_or_else(state_unavailable)?;
    let OperationPaths {
        directory,
        state,
        targets,
        token,
        _lock,
        ..
    } = paths;
    for path in [&state, &targets, &token, &directory.join("operation.lock")] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(state_unavailable()),
        }
    }
    fs::remove_dir(&directory).map_err(|_| state_unavailable())?;
    drop(_lock);
    Ok(())
}

fn next_terminal_sequence(root: &Path) -> Result<u64, HookError> {
    let _session_lock = open_private_lock(&root.join("session.lock"), true, false)?
        .ok_or_else(state_unavailable)?;
    let path = root.join(TERMINAL_SEQUENCE_FILE);
    let current = match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o077 != 0
                || metadata.len() > 32
            {
                return Err(HookError::data(
                    "operation-lifecycle-state-untrusted",
                    "terminal operation sequence is not a bounded private file",
                ));
            }
            let value = fs::read_to_string(&path).map_err(|_| state_unavailable())?;
            value.trim().parse::<u64>().map_err(|_| {
                HookError::data(
                    "operation-lifecycle-state-invalid",
                    "terminal operation sequence is malformed",
                )
            })?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(_) => return Err(state_unavailable()),
    };
    let next = current.checked_add(1).ok_or_else(state_unavailable)?;
    write_private(&path, next.to_string().as_bytes())?;
    Ok(next)
}

fn state_unavailable() -> HookError {
    HookError::runtime(
        "operation-lifecycle-state-unavailable",
        "operation lifecycle state is unavailable",
    )
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};
#[cfg(target_os = "linux")]
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(target_os = "linux")]
use std::io::{Seek, SeekFrom};
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agent_docs::dsh::validation_contracts_from_roots;
use agent_docs::env::ResolvedRoots;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::HookError;

const STATE_SCHEMA: &str = "agent-hook.finish-line.state.v1";
const MAX_RELEASE_TOMBSTONES: usize = 64;
const CAPABILITY_LEASE_DURATION_SECS: u64 = 24 * 60 * 60;
const REQUEST_MAX_BYTES: u64 = 64 * 1024;
const STATE_MAX_BYTES: u64 = 384 * 1024;
const MAX_ID_BYTES: usize = 256;
const MAX_COMMAND_BYTES: usize = 16 * 1024;
const MAX_PROVIDER_ARGV_BYTES: usize = 48 * 1024;
const MAX_PROVIDER_ARGV_ENTRIES: usize = 256;
const MAX_SANDBOX_SIGNATURES: usize = 32;
const MAX_SANDBOX_RULES: usize = 16;
const MAX_SANDBOX_RULE_EXIT_CODES: usize = 32;
const MAX_SANDBOX_SIGNATURE_BYTES: usize = 512;
const MAX_CONTRACTS: usize = 32;
const MAX_TARGETS: usize = 128;
const MAX_SESSIONS: usize = 64;
const MAX_OPERATIONS: usize = 512;
const MAX_EXPIRED_ORPHAN_CANDIDATES: usize = 8;
const MAX_EXPIRED_ORPHAN_OPERATIONS: usize = 16;
const COMPACTION_TRIGGER_OPERATIONS: usize = 256;
const COMPACTED_OPERATION_COUNT: usize = 192;
const DEFAULT_VALIDATION_TIMEOUT_MS: u64 = 30 * 60 * 1_000;
const MAX_VALIDATION_TIMEOUT_MS: u64 = 60 * 60 * 1_000;
const MIN_EXECUTION_TIMEOUT_MS: u64 = 100;
const MAX_VALIDATION_OUTPUT_BYTES: usize = 64 * 1_024;
const PRIVATE_MODE: u32 = 0o600;
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const TRUSTED_BASH: &str = "/bin/bash";
const TRUSTED_SYSTEMD_RUN: &str = "/usr/bin/systemd-run";
const TRUSTED_SYSTEMCTL: &str = "/usr/bin/systemctl";
pub(crate) const CONTAINED_RUNNER_ARG: &str = "__finish-line-contained-runner";
const CONTAINED_RUNNER_SCHEMA: &str = "agent-hook.finish-line.contained-runner.v1";
const CONTAINED_RUNNER_CONTROL_SCHEMA: &str = "agent-hook.finish-line.contained-runner-control.v1";
const CONTAINED_RUNNER_MAX_BYTES: u64 = 256 * 1024;
const CONTAINED_RUNNER_CONTROL_MAX_BYTES: usize = 4 * 1024;
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(250);

/// Fixed containment properties for one transient validation unit.
///
/// `TimeoutStopSec` must stay a positive bounded duration. Systemd reads `0` as
/// a stop deadline that has already expired, not as "no stop timeout", so a
/// teardown that still has to `SIGKILL` a live descendant can be recorded as
/// `Result=timeout` with `ActiveState=failed` even though the main process
/// exited cleanly and the cgroup drained. That made the `systemd-run --wait`
/// client exit non-zero and turned a correct containment into
/// `finish-line-containment-failed`, most often when the unit stopped within a
/// few milliseconds of starting.
///
/// The bound does not soften containment. Descendants are still killed
/// immediately by `KillMode=control-group` with `SIGKILL` as both the kill and
/// final-kill signal; the bound only limits how long the manager waits for the
/// cgroup to empty before reporting a genuine teardown failure.
#[cfg(target_os = "linux")]
const CONTAINED_UNIT_PROPERTIES: [&str; 11] = [
    "--property=Type=exec",
    "--property=KillMode=control-group",
    "--property=KillSignal=SIGKILL",
    "--property=FinalKillSignal=SIGKILL",
    "--property=TimeoutStopSec=2s",
    "--property=SendSIGKILL=yes",
    "--property=Delegate=no",
    "--property=PrivateUsers=yes",
    "--property=RestrictSUIDSGID=yes",
    "--property=RestrictAddressFamilies=AF_INET AF_INET6",
    "--property=IPAddressDeny=localhost",
];

static VALIDATION_CANCEL_SIGNAL: AtomicI32 = AtomicI32::new(0);

#[derive(Clone, Copy, Debug)]
pub enum Operation {
    Open,
    Begin,
    Run,
    Stop,
    Status,
    Quiesce,
    Release,
}

pub struct Outcome {
    pub data: Value,
    pub text: String,
    pub exit_code: i32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BeginRequest {
    schema_version: String,
    product: String,
    session_id: String,
    turn_id: String,
    cwd: PathBuf,
    operation_id: String,
    attempt_token: String,
    operation: BeginOperation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    schema_version: String,
    product: String,
    session_id: String,
    turn_id: String,
    cwd: PathBuf,
    attempt_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum BeginOperation {
    Edit,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunRequest {
    schema_version: String,
    product: String,
    session_id: String,
    turn_id: String,
    cwd: PathBuf,
    operation_id: String,
    intent: String,
    command: String,
    runner_capability: String,
    #[serde(default)]
    execution: Option<RunExecution>,
    #[serde(default = "default_validation_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ContainedRunnerConfig {
    schema_version: String,
    cwd: PathBuf,
    argv: Vec<String>,
    environment: BTreeMap<String, String>,
    supervisor_pid: u32,
    control_nonce: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ContainedRunnerControlMessage {
    schema_version: String,
    nonce: String,
    outcome: ContainedRunnerOutcome,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum ContainedRunnerOutcome {
    Ready,
    Acknowledged,
    Exited { exit_code: i32 },
    Signaled { signal: i32 },
    InfrastructureFailure { code: String },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum RunExecution {
    BashV1 {
        workdir: PathBuf,
        output_max_bytes: usize,
        runner: ValidationRunner,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum ValidationRunner {
    Unsandboxed,
    DangerFullAccess,
    Confined {
        argv: Vec<String>,
        mode: ConfinedMode,
        enforcement: SandboxEnforcement,
        denial_signatures: Vec<String>,
        runner_failure_rules: Vec<RunnerFailureRule>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ConfinedMode {
    ReadOnly,
    WorkspaceWrite,
}

impl ConfinedMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum SandboxEnforcement {
    Full,
    Partial,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RunnerFailureRule {
    #[serde(default)]
    allowed_exit_codes: Option<Vec<i32>>,
    fatal_signatures: Vec<String>,
    #[serde(default)]
    informational_lines: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
enum NormalizedOutcome {
    Success { exit_code: u8 },
    Failure { exit_code: u8 },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StopRequest {
    schema_version: String,
    product: String,
    session_id: String,
    turn_id: String,
    cwd: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuiesceRequest {
    schema_version: String,
    product: String,
    session_id: String,
    turn_id: String,
    cwd: PathBuf,
    operation_id: String,
    runner_capability: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRequest {
    schema_version: String,
    product: String,
    session_id: String,
    turn_id: String,
    cwd: PathBuf,
    runner_capability: String,
}

trait IdentityInput {
    fn schema_version(&self) -> &str;
    fn product(&self) -> &str;
    fn session_id(&self) -> &str;
    fn turn_id(&self) -> &str;
    fn cwd(&self) -> &Path;
}

macro_rules! identity_input {
    ($type:ty) => {
        impl IdentityInput for $type {
            fn schema_version(&self) -> &str {
                &self.schema_version
            }
            fn product(&self) -> &str {
                &self.product
            }
            fn session_id(&self) -> &str {
                &self.session_id
            }
            fn turn_id(&self) -> &str {
                &self.turn_id
            }
            fn cwd(&self) -> &Path {
                &self.cwd
            }
        }
    };
}

identity_input!(BeginRequest);
identity_input!(OpenRequest);
identity_input!(RunRequest);
identity_input!(StopRequest);
identity_input!(QuiesceRequest);
identity_input!(ReleaseRequest);

#[derive(Debug)]
struct RequestIdentity {
    repo_root: PathBuf,
    repo_digest: String,
    repo_key: String,
    session_key: String,
    turn_key: String,
    correlation_id: String,
}

#[derive(Debug)]
struct GitRepositoryIdentity {
    root: PathBuf,
    common_dir: PathBuf,
}

#[derive(Clone, Debug)]
struct ContractSnapshot {
    global_digest: String,
    targets: Vec<ContractTarget>,
    prior_markers: Vec<String>,
}

#[derive(Clone, Debug)]
struct ContractTarget {
    intent: String,
    command: String,
    contract_digest: String,
    target_digest: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema_version: String,
    repo_digest: String,
    generation: u64,
    next_sequence: u64,
    #[serde(default)]
    contract_digest: Option<String>,
    sessions: BTreeMap<String, SessionState>,
    operations: BTreeMap<String, OperationRecord>,
    #[serde(default)]
    released_sessions: BTreeMap<String, ReleasedSession>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expired_reclaim_cursor: Option<String>,
}

impl State {
    fn new(repo_digest: &str) -> Self {
        Self {
            schema_version: STATE_SCHEMA.to_string(),
            repo_digest: repo_digest.to_string(),
            generation: 0,
            next_sequence: 0,
            contract_digest: None,
            sessions: BTreeMap::new(),
            operations: BTreeMap::new(),
            released_sessions: BTreeMap::new(),
            expired_reclaim_cursor: None,
        }
    }

    fn next_sequence(&mut self) -> Result<u64, HookError> {
        self.next_sequence = self.next_sequence.checked_add(1).ok_or_else(|| {
            HookError::data(
                "finish-line-generation-exhausted",
                "finish-line sequence is exhausted",
            )
        })?;
        Ok(self.next_sequence)
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionState {
    targets: BTreeMap<String, TargetState>,
    #[serde(default)]
    runner_capability_digest: Option<String>,
    #[serde(default)]
    runner_capability_incarnation: Option<u64>,
    #[serde(default)]
    capability_lease_expires_at_epoch: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReleasedSession {
    capability_digest: String,
    #[serde(default)]
    session_key: Option<String>,
    sequence: u64,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct ExpiredOrphanOperation {
    operation_key: String,
    sequence: u64,
    active_unit: String,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
struct ExpiredOrphanCandidate {
    session_key: String,
    capability_digest: String,
    capability_incarnation: Option<u64>,
    lease_expires_at_epoch: u64,
    operations: Vec<ExpiredOrphanOperation>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TargetState {
    intent: String,
    contract_digest: String,
    generation: u64,
    attempt_sequence: u64,
    status: TargetStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum TargetStatus {
    Pending,
    Success,
    Failure,
}

impl TargetStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OperationRecord {
    session_key: String,
    turn_key: String,
    token_digest: String,
    generation: u64,
    sequence: u64,
    kind: StoredOperationKind,
    #[serde(default)]
    target_digest: Option<String>,
    #[serde(default)]
    contract_digest: Option<String>,
    #[serde(default)]
    terminal: Option<TerminalOperation>,
    #[serde(default)]
    active_unit: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum StoredOperationKind {
    Edit,
    Shell,
    Validation,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TerminalOperation {
    outcome: NormalizedOutcome,
    disposition: CompletionDisposition,
    #[serde(default)]
    execution: Option<StoredExecution>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredExecution {
    exit_code: Option<i32>,
    signal: Option<String>,
    timed_out: bool,
    aborted: bool,
    timeout_ms: u64,
    #[serde(default)]
    sandbox: Option<ValidationSandbox>,
}

#[derive(Debug, Serialize)]
struct ValidationStream {
    text: String,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct ValidationExecution {
    exit_code: Option<i32>,
    signal: Option<String>,
    timed_out: bool,
    aborted: bool,
    timeout_ms: u64,
    stdout: ValidationStream,
    stderr: ValidationStream,
    #[serde(skip_serializing_if = "Option::is_none")]
    sandbox: Option<ValidationSandbox>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ValidationSandbox {
    mode: String,
    denied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enforcement: Option<SandboxEnforcement>,
}

impl ValidationExecution {
    fn outcome(&self) -> NormalizedOutcome {
        if self.exit_code == Some(0) && self.signal.is_none() && !self.timed_out && !self.aborted {
            NormalizedOutcome::Success { exit_code: 0 }
        } else {
            let exit_code = self
                .exit_code
                .and_then(|code| u8::try_from(code).ok())
                .filter(|code| *code != 0)
                .unwrap_or(1);
            NormalizedOutcome::Failure { exit_code }
        }
    }

    fn stored(&self) -> StoredExecution {
        StoredExecution {
            exit_code: self.exit_code,
            signal: self.signal.clone(),
            timed_out: self.timed_out,
            aborted: self.aborted,
            timeout_ms: self.timeout_ms,
            sandbox: self.sandbox.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CompletionDisposition {
    Applied,
    Stale,
    Superseded,
}

impl CompletionDisposition {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Stale => "stale",
            Self::Superseded => "superseded",
        }
    }
}

pub fn run(state_root: &Path, operation: Operation) -> Result<Outcome, HookError> {
    let input = read_request()?;
    match operation {
        Operation::Open => open(state_root, parse_request(&input)?),
        Operation::Begin => begin(state_root, parse_request(&input)?),
        Operation::Run => run_validation(state_root, parse_request(&input)?),
        Operation::Stop => stop(state_root, parse_request(&input)?),
        Operation::Status => status(state_root, parse_request(&input)?),
        Operation::Quiesce => quiesce(state_root, parse_request(&input)?),
        Operation::Release => release(state_root, parse_request(&input)?),
    }
}

fn read_request() -> Result<Vec<u8>, HookError> {
    let mut input = Vec::new();
    io::stdin()
        .take(REQUEST_MAX_BYTES + 1)
        .read_to_end(&mut input)
        .map_err(|_| {
            HookError::runtime(
                "finish-line-request-read-failed",
                "finish-line request could not be read",
            )
        })?;
    if input.len() as u64 > REQUEST_MAX_BYTES {
        return Err(HookError::data(
            "finish-line-request-too-large",
            "finish-line request exceeds 64 KiB",
        ));
    }
    Ok(input)
}

fn parse_request<T: for<'de> Deserialize<'de>>(input: &[u8]) -> Result<T, HookError> {
    let value = crate::strict_json::from_slice(input).map_err(|_| {
        HookError::data(
            "finish-line-request-invalid",
            "finish-line request must be strict JSON without duplicate keys",
        )
    })?;
    serde_json::from_value(value).map_err(|_| {
        HookError::data(
            "finish-line-request-invalid",
            "finish-line request must match the strict command schema",
        )
    })
}

fn open(state_root: &Path, request: OpenRequest) -> Result<Outcome, HookError> {
    validate_containment_host()?;
    let identity = validate_identity(&request, "agent-hook.finish-line.open.v1")?;
    validate_identifier(&request.attempt_token)?;
    let lease_expires_at = capability_lease_expiry()?;
    let mut attempted_orphan_reclaim = false;
    loop {
        let mut store = Store::open(state_root, &identity)?;
        compact_obsolete_sessions(&mut store.state);

        if let Some(existing) = store.state.sessions.get_mut(&identity.session_key)
            && let Some(expected) = existing.runner_capability_digest.as_deref()
        {
            let capability = open_runner_capability(
                &identity,
                &request.attempt_token,
                existing.runner_capability_incarnation,
            );
            let capability_digest = runner_capability_digest(&identity, &capability);
            if !constant_time_eq(expected.as_bytes(), capability_digest.as_bytes()) {
                return Err(HookError::data(
                    "finish-line-session-active",
                    "finish-line session already belongs to a different private open attempt",
                ));
            }
            existing.capability_lease_expires_at_epoch = Some(lease_expires_at);
            store.save()?;
            return Ok(success_outcome(
                json!({
                    "schema_version": "agent-hook.finish-line.open-result.v1",
                    "status": "duplicate",
                    "runner_capability": capability,
                    "correlation_id": identity.correlation_id,
                }),
                "finish-line DSH runner capability renewed\n",
            ));
        }

        if store.state.sessions.len() >= MAX_SESSIONS
            && !store.state.sessions.contains_key(&identity.session_key)
        {
            drop(store);
            if attempted_orphan_reclaim
                || !reclaim_expired_crash_orphan_session(state_root, &identity)?
            {
                return Err(HookError::data(
                    "finish-line-state-limit",
                    "finish-line session limit is reached",
                ));
            }
            attempted_orphan_reclaim = true;
            continue;
        }
        let incarnation = store.state.next_sequence()?;
        let capability =
            open_runner_capability(&identity, &request.attempt_token, Some(incarnation));
        let capability_digest = runner_capability_digest(&identity, &capability);
        let session = store
            .state
            .sessions
            .entry(identity.session_key.clone())
            .or_default();
        session.runner_capability_digest = Some(capability_digest.clone());
        session.runner_capability_incarnation = Some(incarnation);
        session.capability_lease_expires_at_epoch = Some(lease_expires_at);
        store.save()?;
        return Ok(success_outcome(
            json!({
                "schema_version": "agent-hook.finish-line.open-result.v1",
                "status": "opened",
                "runner_capability": capability,
                "correlation_id": identity.correlation_id,
            }),
            "finish-line DSH runner capability opened\n",
        ));
    }
}

fn begin(state_root: &Path, request: BeginRequest) -> Result<Outcome, HookError> {
    let identity = validate_identity(&request, "agent-hook.finish-line.begin.v1")?;
    validate_identifier(&request.operation_id)?;
    validate_identifier(&request.attempt_token)?;
    let snapshot = resolve_contracts(&identity)?;
    let BeginOperation::Edit = request.operation;

    let operation_key = operation_key(&identity.session_key, &request.operation_id);
    let token_digest = digest_parts(
        "agent-hook.finish-line.token.v1",
        &[request.attempt_token.as_bytes()],
    );
    let mut attempted_orphan_reclaim = false;
    loop {
        let mut store = Store::open(state_root, &identity)?;
        if let Some(existing) = store.state.operations.get(&operation_key) {
            let exact_retry = existing.session_key == identity.session_key
                && existing.turn_key == identity.turn_key
                && existing.kind == StoredOperationKind::Edit
                && existing.target_digest.is_none()
                && existing.contract_digest.is_none()
                && constant_time_eq(existing.token_digest.as_bytes(), token_digest.as_bytes());
            if exact_retry {
                let existing_generation = existing.generation;
                store.save()?;
                return Ok(success_outcome(
                    json!({
                        "schema_version": "agent-hook.finish-line.begin-result.v1",
                        "status": "duplicate",
                        "operation_id": request.operation_id,
                        "generation": existing_generation,
                        "operation_kind": "edit",
                        "correlation_id": identity.correlation_id,
                    }),
                    "finish-line duplicate reservation accepted\n",
                ));
            }
            return Err(HookError::data(
                "finish-line-operation-exists",
                "finish-line operation_id is already registered with a different binding",
            ));
        }
        compact_state(&mut store.state);
        let at_capacity = store.state.operations.len() >= MAX_OPERATIONS
            || (store.state.sessions.len() >= MAX_SESSIONS
                && !store.state.sessions.contains_key(&identity.session_key));
        if at_capacity && !attempted_orphan_reclaim {
            drop(store);
            attempted_orphan_reclaim = true;
            let _ = reclaim_expired_crash_orphan_session(state_root, &identity)?;
            continue;
        }

        ensure_state_capacity(&store.state, &identity.session_key)?;
        store.state.generation = store.state.generation.checked_add(1).ok_or_else(|| {
            HookError::data(
                "finish-line-generation-exhausted",
                "finish-line edit generation is exhausted",
            )
        })?;
        let generation = store.state.generation;
        compact_obsolete_sessions(&mut store.state);
        let sequence = store.state.next_sequence()?;

        store
            .state
            .sessions
            .entry(identity.session_key.clone())
            .or_default();

        store.state.operations.insert(
            operation_key,
            OperationRecord {
                session_key: identity.session_key,
                turn_key: identity.turn_key,
                token_digest,
                generation,
                sequence,
                kind: StoredOperationKind::Edit,
                target_digest: None,
                contract_digest: None,
                terminal: Some(TerminalOperation {
                    outcome: NormalizedOutcome::Success { exit_code: 0 },
                    disposition: CompletionDisposition::Applied,
                    execution: None,
                }),
                active_unit: None,
            },
        );
        store.state.contract_digest = Some(snapshot.global_digest);
        store.save()?;

        return Ok(success_outcome(
            json!({
                "schema_version": "agent-hook.finish-line.begin-result.v1",
                "status": "registered",
                "operation_id": request.operation_id,
                "generation": generation,
                "operation_kind": "edit",
                "correlation_id": identity.correlation_id,
            }),
            "finish-line operation registered\n",
        ));
    }
}

fn run_validation(state_root: &Path, request: RunRequest) -> Result<Outcome, HookError> {
    let identity = validate_identity(&request, "agent-hook.finish-line.run.v1")?;
    validate_identifier(&request.operation_id)?;
    validate_identifier(&request.runner_capability)?;
    validate_intent(&request.intent)?;
    validate_command(&request.command)?;
    if request.timeout_ms == 0 || request.timeout_ms > MAX_VALIDATION_TIMEOUT_MS {
        return Err(HookError::data(
            "finish-line-timeout-invalid",
            "finish-line validation timeout must be between 1 ms and 60 minutes",
        ));
    }
    let snapshot = resolve_contracts(&identity)?;
    if request.execution.is_some() && request.timeout_ms < MIN_EXECUTION_TIMEOUT_MS {
        return Err(HookError::data(
            "finish-line-timeout-invalid",
            "finish-line execution timeout must be between 100 ms and 60 minutes",
        ));
    }
    let target = snapshot
        .targets
        .iter()
        .find(|target| target.intent == request.intent && target.command == request.command)
        .cloned();
    let Some(target) = target else {
        if request.execution.is_some() {
            return run_ordinary_shell(state_root, request, identity, snapshot);
        }
        let store = Store::open(state_root, &identity)?;
        validate_runner_capability(&store, &identity, &request.runner_capability)?;
        store.save()?;
        return Ok(success_outcome(
            json!({
                "schema_version": "agent-hook.finish-line.run-result.v1",
                "status": "ordinary-ready",
                "operation_id": request.operation_id,
                "correlation_id": identity.correlation_id,
            }),
            "finish-line ordinary shell command is ready for supervised execution\n",
        ));
    };

    if request.execution.is_none() {
        let store = Store::open(state_root, &identity)?;
        validate_runner_capability(&store, &identity, &request.runner_capability)?;
        store.save()?;
        return Ok(success_outcome(
            json!({
                "schema_version": "agent-hook.finish-line.run-result.v1",
                "status": "ready",
                "operation_id": request.operation_id,
                "intent": target.intent,
                "contract_digest": target.contract_digest,
                "target_digest": target.target_digest,
                "correlation_id": identity.correlation_id,
            }),
            "finish-line validation candidate is ready for provider execution\n",
        ));
    }
    let (runner, output_max_bytes, workdir) = validate_run_execution(
        request.execution.as_ref().expect("execution checked"),
        &identity.repo_root,
        &request.command,
        true,
    )?;
    let operation_key = operation_key(&identity.session_key, &request.operation_id);
    let unit = format!("nils-finish-line-{}", Uuid::new_v4().simple());
    let sequence;
    let generation;
    {
        let mut store = Store::open(state_root, &identity)?;
        validate_runner_capability(&store, &identity, &request.runner_capability)?;
        if let Some(existing) = store.state.operations.get(&operation_key) {
            let exact_retry = existing.session_key == identity.session_key
                && existing.turn_key == identity.turn_key
                && existing.kind == StoredOperationKind::Validation
                && existing.target_digest.as_deref() == Some(target.target_digest.as_str())
                && existing.contract_digest.as_deref() == Some(target.contract_digest.as_str());
            if !exact_retry {
                return Err(HookError::data(
                    "finish-line-operation-exists",
                    "finish-line operation_id is already registered with a different binding",
                ));
            }
            let existing_generation = existing.generation;
            let terminal = existing.terminal.clone();
            store.save()?;
            let Some(terminal) = terminal else {
                return Err(finish_line_temporary(
                    "finish-line-validation-pending",
                    "the exact finish-line validation operation is still pending; use a new operation_id only after confirming the prior runner stopped",
                ));
            };
            let stored = terminal.execution.ok_or_else(|| {
                HookError::data(
                    "finish-line-state-invalid",
                    "finish-line validation result has no execution record",
                )
            })?;
            let execution = ValidationExecution {
                exit_code: stored.exit_code,
                signal: stored.signal,
                timed_out: stored.timed_out,
                aborted: stored.aborted,
                timeout_ms: stored.timeout_ms,
                stdout: ValidationStream {
                    text: String::new(),
                    truncated: false,
                },
                stderr: ValidationStream {
                    text: String::new(),
                    truncated: false,
                },
                sandbox: stored.sandbox,
            };
            return Ok(success_outcome(
                json!({
                    "schema_version": "agent-hook.finish-line.run-result.v1",
                    "status": "duplicate",
                    "operation_id": request.operation_id,
                    "generation": existing_generation,
                    "intent": target.intent,
                    "contract_digest": target.contract_digest,
                    "target_digest": target.target_digest,
                    "correlation_id": identity.correlation_id,
                    "output_replayed": false,
                    "execution": execution,
                }),
                "finish-line duplicate validation result accepted\n",
            ));
        }

        compact_state(&mut store.state);
        compact_obsolete_sessions(&mut store.state);
        ensure_state_capacity(&store.state, &identity.session_key)?;
        generation = store.state.generation;
        sequence = store.state.next_sequence()?;
        let session = store
            .state
            .sessions
            .entry(identity.session_key.clone())
            .or_default();
        if session.targets.len() >= MAX_TARGETS
            && !session.targets.contains_key(&target.target_digest)
        {
            return Err(HookError::data(
                "finish-line-state-limit",
                "finish-line target limit is reached",
            ));
        }
        session.targets.insert(
            target.target_digest.clone(),
            TargetState {
                intent: target.intent.clone(),
                contract_digest: target.contract_digest.clone(),
                generation,
                attempt_sequence: sequence,
                status: TargetStatus::Pending,
            },
        );
        store.state.operations.insert(
            operation_key.clone(),
            OperationRecord {
                session_key: identity.session_key.clone(),
                turn_key: identity.turn_key.clone(),
                token_digest: digest_parts(
                    "agent-hook.finish-line.nils-runner.v1",
                    &[operation_key.as_bytes()],
                ),
                generation,
                sequence,
                kind: StoredOperationKind::Validation,
                target_digest: Some(target.target_digest.clone()),
                contract_digest: Some(target.contract_digest.clone()),
                terminal: None,
                active_unit: Some(unit.clone()),
            },
        );
        store.state.contract_digest = Some(snapshot.global_digest);
        store.save()?;
    }

    let execution = execute_validation_command(
        state_root,
        &workdir,
        &request.command,
        runner,
        Duration::from_millis(request.timeout_ms),
        output_max_bytes,
        &unit,
    )?;
    let observed_outcome = execution.outcome();
    let current_snapshot = resolve_contracts(&identity)?;
    let disposition;
    {
        let mut store = Store::open(state_root, &identity)?;
        let current_generation = store.state.generation;
        let contract_current = current_snapshot.targets.iter().any(|candidate| {
            candidate.target_digest == target.target_digest
                && candidate.contract_digest == target.contract_digest
        });
        let target_state = store
            .state
            .sessions
            .get_mut(&identity.session_key)
            .and_then(|session| session.targets.get_mut(&target.target_digest));
        disposition = if generation != current_generation || !contract_current {
            CompletionDisposition::Stale
        } else if target_state.as_ref().is_none_or(|state| {
            state.generation != generation || state.attempt_sequence != sequence
        }) {
            CompletionDisposition::Superseded
        } else {
            let state = target_state.expect("target state checked");
            state.status = match observed_outcome {
                NormalizedOutcome::Success { .. } => TargetStatus::Success,
                NormalizedOutcome::Failure { .. } => TargetStatus::Failure,
            };
            CompletionDisposition::Applied
        };
        let operation = store
            .state
            .operations
            .get_mut(&operation_key)
            .ok_or_else(|| {
                HookError::data(
                    "finish-line-state-invalid",
                    "finish-line validation operation disappeared before completion",
                )
            })?;
        if operation.terminal.is_some() {
            return Err(HookError::data(
                "finish-line-state-invalid",
                "finish-line validation operation became terminal unexpectedly",
            ));
        }
        operation.terminal = Some(TerminalOperation {
            outcome: observed_outcome,
            disposition,
            execution: Some(execution.stored()),
        });
        operation.active_unit = None;
        store.state.contract_digest = Some(current_snapshot.global_digest);
        store.save()?;
    }

    Ok(success_outcome(
        json!({
            "schema_version": "agent-hook.finish-line.run-result.v1",
            "status": disposition.as_str(),
            "operation_id": request.operation_id,
            "generation": generation,
            "intent": target.intent,
            "contract_digest": target.contract_digest,
            "target_digest": target.target_digest,
            "correlation_id": identity.correlation_id,
            "output_replayed": true,
            "execution": execution,
        }),
        "finish-line validation executed and recorded\n",
    ))
}

fn run_ordinary_shell(
    state_root: &Path,
    request: RunRequest,
    identity: RequestIdentity,
    snapshot: ContractSnapshot,
) -> Result<Outcome, HookError> {
    let (runner, output_max_bytes, workdir) = validate_run_execution(
        request.execution.as_ref().expect("execution checked"),
        &identity.repo_root,
        &request.command,
        false,
    )?;
    let execution_binding = serde_json::to_vec(
        request.execution.as_ref().expect("execution checked"),
    )
    .map_err(|_| {
        HookError::runtime(
            "finish-line-execution-serialize-failed",
            "finish-line shell execution binding could not be serialized",
        )
    })?;
    let shell_digest = digest_parts(
        "agent-hook.finish-line.shell.v1",
        &[
            request.command.as_bytes(),
            &request.timeout_ms.to_le_bytes(),
            &execution_binding,
        ],
    );
    let operation_key = operation_key(&identity.session_key, &request.operation_id);
    let unit = format!("nils-finish-line-{}", Uuid::new_v4().simple());
    let sequence;
    let generation;
    {
        let mut store = Store::open(state_root, &identity)?;
        validate_runner_capability(&store, &identity, &request.runner_capability)?;
        if let Some(existing) = store.state.operations.get(&operation_key) {
            let exact_retry = existing.session_key == identity.session_key
                && existing.turn_key == identity.turn_key
                && existing.kind == StoredOperationKind::Shell
                && existing.target_digest.as_deref() == Some(shell_digest.as_str())
                && existing.contract_digest.is_none();
            if !exact_retry {
                return Err(HookError::data(
                    "finish-line-operation-exists",
                    "finish-line operation_id is already registered with a different binding",
                ));
            }
            let existing_generation = existing.generation;
            let terminal = existing.terminal.clone();
            store.save()?;
            let Some(terminal) = terminal else {
                return Err(finish_line_temporary(
                    "finish-line-shell-pending",
                    "the exact finish-line shell operation is still pending; retry only after confirming the prior runner stopped",
                ));
            };
            let stored = terminal.execution.ok_or_else(|| {
                HookError::data(
                    "finish-line-state-invalid",
                    "finish-line shell result has no execution record",
                )
            })?;
            let execution = ValidationExecution {
                exit_code: stored.exit_code,
                signal: stored.signal,
                timed_out: stored.timed_out,
                aborted: stored.aborted,
                timeout_ms: stored.timeout_ms,
                stdout: ValidationStream {
                    text: String::new(),
                    truncated: false,
                },
                stderr: ValidationStream {
                    text: String::new(),
                    truncated: false,
                },
                sandbox: stored.sandbox,
            };
            return Ok(success_outcome(
                json!({
                    "schema_version": "agent-hook.finish-line.run-result.v1",
                    "status": "duplicate",
                    "operation_id": request.operation_id,
                    "generation": existing_generation,
                    "correlation_id": identity.correlation_id,
                    "output_replayed": false,
                    "execution": execution,
                }),
                "finish-line duplicate shell result accepted\n",
            ));
        }

        compact_state(&mut store.state);
        ensure_state_capacity(&store.state, &identity.session_key)?;
        store.state.generation = store.state.generation.checked_add(1).ok_or_else(|| {
            HookError::data(
                "finish-line-generation-exhausted",
                "finish-line shell generation is exhausted",
            )
        })?;
        generation = store.state.generation;
        compact_obsolete_sessions(&mut store.state);
        sequence = store.state.next_sequence()?;
        store
            .state
            .sessions
            .entry(identity.session_key.clone())
            .or_default();
        store.state.operations.insert(
            operation_key.clone(),
            OperationRecord {
                session_key: identity.session_key.clone(),
                turn_key: identity.turn_key.clone(),
                token_digest: digest_parts(
                    "agent-hook.finish-line.shell-runner.v1",
                    &[operation_key.as_bytes(), shell_digest.as_bytes()],
                ),
                generation,
                sequence,
                kind: StoredOperationKind::Shell,
                target_digest: Some(shell_digest),
                contract_digest: None,
                terminal: None,
                active_unit: Some(unit.clone()),
            },
        );
        store.state.contract_digest = Some(snapshot.global_digest);
        store.save()?;
    }

    let execution = execute_validation_command(
        state_root,
        &workdir,
        &request.command,
        runner,
        Duration::from_millis(request.timeout_ms),
        output_max_bytes,
        &unit,
    )?;
    let observed_outcome = execution.outcome();
    let current_snapshot = resolve_contracts(&identity)?;
    {
        let mut store = Store::open(state_root, &identity)?;
        let operation = store
            .state
            .operations
            .get_mut(&operation_key)
            .ok_or_else(|| {
                HookError::data(
                    "finish-line-state-invalid",
                    "finish-line shell operation disappeared before completion",
                )
            })?;
        if operation.terminal.is_some() {
            return Err(HookError::data(
                "finish-line-state-invalid",
                "finish-line shell operation became terminal unexpectedly",
            ));
        }
        operation.terminal = Some(TerminalOperation {
            outcome: observed_outcome,
            disposition: CompletionDisposition::Applied,
            execution: Some(execution.stored()),
        });
        operation.active_unit = None;
        store.state.contract_digest = Some(current_snapshot.global_digest);
        store.save()?;
    }

    Ok(success_outcome(
        json!({
            "schema_version": "agent-hook.finish-line.run-result.v1",
            "status": "ordinary-applied",
            "operation_id": request.operation_id,
            "generation": generation,
            "correlation_id": identity.correlation_id,
            "output_replayed": true,
            "execution": execution,
        }),
        "finish-line ordinary shell command executed and recorded\n",
    ))
}

fn validate_runner_capability(
    store: &Store,
    identity: &RequestIdentity,
    capability: &str,
) -> Result<(), HookError> {
    let supplied = runner_capability_digest(identity, capability);
    let valid = store
        .state
        .sessions
        .get(&identity.session_key)
        .and_then(|session| session.runner_capability_digest.as_deref())
        .is_some_and(|expected| constant_time_eq(expected.as_bytes(), supplied.as_bytes()));
    if !valid {
        return Err(HookError::data(
            "finish-line-runner-capability-invalid",
            "finish-line validation requires the private capability minted for this DSH session",
        ));
    }
    Ok(())
}

fn quiesce(state_root: &Path, request: QuiesceRequest) -> Result<Outcome, HookError> {
    let identity = validate_identity(&request, "agent-hook.finish-line.quiesce.v1")?;
    validate_identifier(&request.operation_id)?;
    validate_identifier(&request.runner_capability)?;
    let operation_key = operation_key(&identity.session_key, &request.operation_id);
    let pending = {
        let store = Store::open(state_root, &identity)?;
        validate_runner_capability(&store, &identity, &request.runner_capability)?;
        store
            .state
            .operations
            .get(&operation_key)
            .and_then(|operation| {
                (operation.session_key == identity.session_key
                    && operation.turn_key == identity.turn_key
                    && operation.terminal.is_none())
                .then(|| {
                    (
                        operation.kind,
                        operation.generation,
                        operation.sequence,
                        operation.target_digest.clone(),
                        operation.active_unit.clone(),
                    )
                })
            })
    };

    if let Some((_, _, _, _, Some(unit))) = pending.as_ref() {
        quiesce_contained_unit(unit)?;
    }

    if let Some((kind, generation, sequence, target_digest, active_unit)) = pending {
        let mut store = Store::open(state_root, &identity)?;
        let still_pending = store
            .state
            .operations
            .get(&operation_key)
            .is_some_and(|operation| {
                operation.session_key == identity.session_key
                    && operation.turn_key == identity.turn_key
                    && operation.kind == kind
                    && operation.generation == generation
                    && operation.sequence == sequence
                    && operation.terminal.is_none()
                    && operation.active_unit == active_unit
            });
        if still_pending {
            store.state.operations.remove(&operation_key);
            if kind == StoredOperationKind::Validation
                && let Some(target_digest) = target_digest
                && let Some(session) = store.state.sessions.get_mut(&identity.session_key)
            {
                let removable = session.targets.get(&target_digest).is_some_and(|target| {
                    target.generation == generation
                        && target.attempt_sequence == sequence
                        && matches!(target.status, TargetStatus::Pending)
                });
                if removable {
                    session.targets.remove(&target_digest);
                }
            }
        }
        store.save()?;
    }

    Ok(success_outcome(
        json!({
            "schema_version": "agent-hook.finish-line.quiesce-result.v1",
            "status": "quiescent",
            "operation_id": request.operation_id,
            "correlation_id": identity.correlation_id,
        }),
        "finish-line contained execution is quiescent\n",
    ))
}

fn release(state_root: &Path, request: ReleaseRequest) -> Result<Outcome, HookError> {
    let identity = validate_identity(&request, "agent-hook.finish-line.release.v1")?;
    validate_identifier(&request.runner_capability)?;
    let supplied_digest = runner_capability_digest(&identity, &request.runner_capability);
    let mut store = Store::open(state_root, &identity)?;

    let live_capability_matches = store
        .state
        .sessions
        .get(&identity.session_key)
        .and_then(|session| session.runner_capability_digest.as_deref())
        .is_some_and(|expected| constant_time_eq(expected.as_bytes(), supplied_digest.as_bytes()));
    if !live_capability_matches
        && store.state.released_sessions.values().any(|released| {
            constant_time_eq(
                released.capability_digest.as_bytes(),
                supplied_digest.as_bytes(),
            )
        })
    {
        return Ok(success_outcome(
            json!({
                "schema_version": "agent-hook.finish-line.release-result.v1",
                "status": "duplicate",
                "correlation_id": identity.correlation_id,
            }),
            "finish-line session was already released\n",
        ));
    }

    validate_runner_capability(&store, &identity, &request.runner_capability)?;
    let busy = store.state.operations.values().any(|operation| {
        operation.session_key == identity.session_key
            && (operation.terminal.is_none() || operation.active_unit.is_some())
    });
    if busy {
        return Err(finish_line_temporary(
            "finish-line-session-busy",
            "finish-line session cannot be released while contained execution is pending",
        ));
    }

    store
        .state
        .operations
        .retain(|_, operation| operation.session_key != identity.session_key);
    store.state.sessions.remove(&identity.session_key);
    let sequence = store.state.next_sequence()?;
    let released_key = released_session_key(&identity.session_key, &supplied_digest);
    store.state.released_sessions.insert(
        released_key,
        ReleasedSession {
            capability_digest: supplied_digest,
            session_key: Some(identity.session_key.clone()),
            sequence,
        },
    );
    compact_release_tombstones(&mut store.state);
    store.save()?;
    Ok(success_outcome(
        json!({
            "schema_version": "agent-hook.finish-line.release-result.v1",
            "status": "released",
            "correlation_id": identity.correlation_id,
        }),
        "finish-line session released\n",
    ))
}

fn stop(state_root: &Path, request: StopRequest) -> Result<Outcome, HookError> {
    let identity = validate_identity(&request, "agent-hook.finish-line.stop.v1")?;
    let snapshot = resolve_contracts(&identity)?;
    let store = Store::open(state_root, &identity)?;
    let generation = store.state.generation;

    let session_active = store
        .state
        .sessions
        .get(&identity.session_key)
        .is_some_and(|session| !session.targets.is_empty());
    let enforcement_active = generation > 0 || session_active;
    let mut reason_codes = BTreeSet::new();
    let mut unsatisfied = false;

    if enforcement_active && store.state.contract_digest.as_deref() != Some(&snapshot.global_digest)
    {
        reason_codes.insert("validation-contract-drift");
    }

    if enforcement_active {
        for target in &snapshot.targets {
            let session = store.state.sessions.get(&identity.session_key);
            let target_satisfied = session
                .and_then(|session| session.targets.get(&target.target_digest))
                .is_some_and(|state| {
                    state.generation == generation
                        && state.contract_digest == target.contract_digest
                        && matches!(state.status, TargetStatus::Success)
                });
            if target_satisfied {
                continue;
            }

            unsatisfied = true;
            match session.and_then(|session| session.targets.get(&target.target_digest)) {
                Some(state)
                    if state.generation != generation
                        || state.contract_digest != target.contract_digest =>
                {
                    reason_codes.insert("validation-stale");
                }
                Some(state) => match state.status {
                    TargetStatus::Pending => {
                        reason_codes.insert("validation-pending");
                    }
                    TargetStatus::Failure => {
                        reason_codes.insert("validation-failed");
                    }
                    TargetStatus::Success => {
                        reason_codes.insert("validation-stale");
                    }
                },
                None => {
                    reason_codes.insert("validation-missing");
                }
            }
        }
    }

    if unsatisfied && !snapshot.prior_markers.is_empty() {
        for marker in &snapshot.prior_markers {
            reason_codes.insert(classify_prior_marker(&identity.repo_root, marker));
        }
    }
    let action = if unsatisfied || reason_codes.contains("validation-contract-drift") {
        "block"
    } else {
        "allow"
    };
    let reasons = reason_codes.into_iter().collect::<Vec<_>>();
    let remediation = remediations(&reasons);
    Ok(Outcome {
        data: json!({
            "schema_version": "agent-hook.finish-line.stop-result.v1",
            "action": action,
            "generation": generation,
            "contract_digest": snapshot.global_digest,
            "correlation_id": identity.correlation_id,
            "reason_codes": reasons,
            "remediation": remediation,
        }),
        text: format!("finish-line stop: {action}\n"),
        exit_code: if action == "allow" { 0 } else { 1 },
    })
}

fn status(state_root: &Path, request: StopRequest) -> Result<Outcome, HookError> {
    let identity = validate_identity(&request, "agent-hook.finish-line.status.v1")?;
    let snapshot = resolve_contracts(&identity)?;
    let store = Store::open(state_root, &identity)?;
    let generation = store.state.generation;
    let session = store.state.sessions.get(&identity.session_key);
    let targets = snapshot
        .targets
        .iter()
        .map(|target| {
            let state = session.and_then(|session| session.targets.get(&target.target_digest));
            let status = match state {
                Some(state)
                    if state.generation != generation
                        || state.contract_digest != target.contract_digest =>
                {
                    "stale"
                }
                Some(state) => state.status.as_str(),
                None => "missing",
            };
            json!({
                "intent": target.intent,
                "target_digest": target.target_digest,
                "status": status,
                "attempt_generation": state.map(|state| state.generation),
            })
        })
        .collect::<Vec<_>>();
    Ok(success_outcome(
        json!({
            "schema_version": "agent-hook.finish-line.status-result.v1",
            "generation": generation,
            "contract_digest": snapshot.global_digest,
            "correlation_id": identity.correlation_id,
            "targets": targets,
        }),
        "finish-line status inspected\n",
    ))
}

fn validate_identity<T: IdentityInput>(
    request: &T,
    expected_schema: &str,
) -> Result<RequestIdentity, HookError> {
    if request.schema_version() != expected_schema {
        return Err(HookError::data(
            "finish-line-version-invalid",
            "finish-line request schema_version is unsupported",
        ));
    }
    if request.product() != "dsh" {
        return Err(HookError::data(
            "finish-line-product-invalid",
            "finish-line v1 supports only product dsh",
        ));
    }
    validate_identifier(request.session_id())?;
    validate_identifier(request.turn_id())?;
    if !request.cwd().is_absolute() {
        return Err(HookError::data(
            "finish-line-repository-invalid",
            "finish-line cwd must be an absolute canonical directory",
        ));
    }
    let requested_metadata = fs::symlink_metadata(request.cwd()).map_err(|_| {
        HookError::data(
            "finish-line-repository-invalid",
            "finish-line cwd is unavailable",
        )
    })?;
    if requested_metadata.file_type().is_symlink() || !requested_metadata.is_dir() {
        return Err(HookError::data(
            "finish-line-repository-invalid",
            "finish-line cwd must be a non-symlink directory",
        ));
    }
    let requested_dir = fs::canonicalize(request.cwd()).map_err(|_| {
        HookError::data(
            "finish-line-repository-invalid",
            "finish-line cwd cannot be canonicalized",
        )
    })?;
    if requested_dir != request.cwd() {
        return Err(HookError::data(
            "finish-line-repository-invalid",
            "finish-line cwd must already be canonical",
        ));
    }
    let requested_git = resolve_git_identity(&requested_dir)?;
    let process_dir = std::env::current_dir().map_err(|_| {
        finish_line_unavailable(
            "finish-line-repository-unavailable",
            "finish-line process cwd is unavailable",
        )
    })?;
    let process_git = resolve_git_identity(&process_dir)?;
    if requested_dir != requested_git.root
        || requested_git.root != process_git.root
        || requested_git.common_dir != process_git.common_dir
    {
        return Err(HookError::data(
            "finish-line-repository-mismatch",
            "finish-line cwd must be the authoritative Git root of the running process",
        ));
    }
    let repo_root = requested_git.root;
    let metadata = fs::symlink_metadata(&repo_root).map_err(|_| {
        HookError::data(
            "finish-line-repository-invalid",
            "finish-line Git root is unavailable",
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(HookError::data(
            "finish-line-repository-invalid",
            "finish-line Git root must be a non-symlink directory",
        ));
    }
    let common_metadata = fs::symlink_metadata(&requested_git.common_dir).map_err(|_| {
        HookError::data(
            "finish-line-repository-invalid",
            "finish-line Git common directory is unavailable",
        )
    })?;
    if common_metadata.file_type().is_symlink() || !common_metadata.is_dir() {
        return Err(HookError::data(
            "finish-line-repository-invalid",
            "finish-line Git common directory must be a non-symlink directory",
        ));
    }
    let repo_digest = digest_parts(
        "agent-hook.finish-line.repo.v1",
        &[
            repo_root.as_os_str().as_encoded_bytes(),
            &metadata.dev().to_le_bytes(),
            &metadata.ino().to_le_bytes(),
            requested_git.common_dir.as_os_str().as_encoded_bytes(),
            &common_metadata.dev().to_le_bytes(),
            &common_metadata.ino().to_le_bytes(),
        ],
    );
    let repo_key = repo_digest
        .strip_prefix("sha256:")
        .expect("digest prefix")
        .to_string();
    let session_key = digest_parts(
        "agent-hook.finish-line.session.v1",
        &[
            request.product().as_bytes(),
            repo_digest.as_bytes(),
            request.session_id().as_bytes(),
        ],
    );
    let turn_key = digest_parts(
        "agent-hook.finish-line.turn.v1",
        &[session_key.as_bytes(), request.turn_id().as_bytes()],
    );
    let correlation_id = digest_parts(
        "agent-hook.finish-line.correlation.v1",
        &[
            request.product().as_bytes(),
            repo_digest.as_bytes(),
            session_key.as_bytes(),
        ],
    );
    Ok(RequestIdentity {
        repo_root,
        repo_digest,
        repo_key,
        session_key,
        turn_key,
        correlation_id,
    })
}

fn resolve_git_identity(start: &Path) -> Result<GitRepositoryIdentity, HookError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(start)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
        ])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .env_remove("GIT_CONFIG")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM")
        .env_remove("GIT_CONFIG_COUNT")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| {
            finish_line_unavailable(
                "finish-line-git-unavailable",
                "finish-line could not invoke Git repository discovery",
            )
        })?;
    if !output.status.success() || output.stdout.len() > 4096 {
        return Err(HookError::data(
            "finish-line-repository-invalid",
            "finish-line cwd is not inside an authoritative Git repository",
        ));
    }
    let fields = std::str::from_utf8(&output.stdout)
        .ok()
        .map(str::lines)
        .map(Iterator::collect::<Vec<_>>)
        .filter(|fields| fields.len() == 2 && fields.iter().all(|field| !field.is_empty()))
        .ok_or_else(|| {
            HookError::data(
                "finish-line-repository-invalid",
                "finish-line Git identity is invalid",
            )
        })?;
    let root = fs::canonicalize(fields[0]).map_err(|_| {
        HookError::data(
            "finish-line-repository-invalid",
            "finish-line Git root cannot be canonicalized",
        )
    })?;
    let common_dir = fs::canonicalize(fields[1]).map_err(|_| {
        HookError::data(
            "finish-line-repository-invalid",
            "finish-line Git common directory cannot be canonicalized",
        )
    })?;
    Ok(GitRepositoryIdentity { root, common_dir })
}

fn validate_identifier(value: &str) -> Result<(), HookError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(HookError::data(
            "finish-line-identifier-invalid",
            "finish-line identifiers must contain 1..256 non-space ASCII bytes",
        ));
    }
    Ok(())
}

fn validate_intent(intent: &str) -> Result<(), HookError> {
    agent_docs::model::Context::parse(intent).map_err(|_| {
        HookError::data(
            "finish-line-intent-invalid",
            "finish-line intent is invalid",
        )
    })?;
    Ok(())
}

fn validate_command(command: &str) -> Result<(), HookError> {
    if command.is_empty()
        || command.len() > MAX_COMMAND_BYTES
        || command.contains('\0')
        || command
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(HookError::data(
            "finish-line-command-invalid",
            "finish-line command is empty, oversized, or contains unsafe control bytes",
        ));
    }
    Ok(())
}

const fn default_validation_timeout_ms() -> u64 {
    DEFAULT_VALIDATION_TIMEOUT_MS
}

extern "C" fn record_validation_signal(signal: libc::c_int) {
    let _ =
        VALIDATION_CANCEL_SIGNAL.compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst);
}

struct ValidationSignalGuard {
    previous: Vec<(libc::c_int, libc::sighandler_t)>,
}

impl ValidationSignalGuard {
    fn install() -> Result<Self, HookError> {
        VALIDATION_CANCEL_SIGNAL.store(0, Ordering::SeqCst);
        let mut previous = Vec::new();
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            let prior = unsafe {
                libc::signal(
                    signal,
                    record_validation_signal as *const () as libc::sighandler_t,
                )
            };
            if prior == libc::SIG_ERR {
                for (installed, handler) in previous.iter().rev() {
                    unsafe {
                        libc::signal(*installed, *handler);
                    }
                }
                return Err(finish_line_unavailable(
                    "finish-line-signal-unavailable",
                    "finish-line validation cancellation could not be installed",
                ));
            }
            previous.push((signal, prior));
        }
        Ok(Self { previous })
    }
}

impl Drop for ValidationSignalGuard {
    fn drop(&mut self) {
        for (signal, handler) in self.previous.iter().rev() {
            unsafe {
                libc::signal(*signal, *handler);
            }
        }
        VALIDATION_CANCEL_SIGNAL.store(0, Ordering::SeqCst);
    }
}

fn execute_validation_command(
    state_root: &Path,
    repo_root: &Path,
    command: &str,
    runner: &ValidationRunner,
    timeout: Duration,
    output_max_bytes: usize,
    unit: &str,
) -> Result<ValidationExecution, HookError> {
    execute_validation_command_platform(
        state_root,
        repo_root,
        command,
        runner,
        timeout,
        output_max_bytes,
        unit,
    )
}

#[cfg(not(target_os = "linux"))]
fn execute_validation_command_platform(
    _state_root: &Path,
    _repo_root: &Path,
    _command: &str,
    _runner: &ValidationRunner,
    _timeout: Duration,
    _output_max_bytes: usize,
    _unit: &str,
) -> Result<ValidationExecution, HookError> {
    Err(finish_line_unavailable(
        "finish-line-containment-unavailable",
        "authoritative finish-line execution requires Linux systemd cgroup containment",
    ))
}

#[cfg(target_os = "linux")]
fn execute_validation_command_platform(
    _state_root: &Path,
    repo_root: &Path,
    command: &str,
    runner: &ValidationRunner,
    timeout: Duration,
    output_max_bytes: usize,
    unit: &str,
) -> Result<ValidationExecution, HookError> {
    if !matches!(runner, ValidationRunner::Confined { .. }) {
        validate_trusted_bash()?;
    }
    validate_trusted_systemd()?;
    let _signals = ValidationSignalGuard::install()?;
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(MAX_VALIDATION_TIMEOUT_MS);
    let argv = match runner {
        ValidationRunner::Confined { argv, .. } => argv.clone(),
        ValidationRunner::Unsandboxed | ValidationRunner::DangerFullAccess => vec![
            TRUSTED_BASH.to_string(),
            "-c".to_string(),
            command.to_string(),
        ],
    };
    let mut control = ContainedRunnerControlFile::create()?;
    let contained = sealed_contained_runner_config(repo_root, argv, control.nonce.clone())?;
    let executable_source = format!("/proc/{}/exe", std::process::id());
    let executable = fs::canonicalize(std::env::current_exe().map_err(|_| {
        finish_line_unavailable(
            "finish-line-runner-unavailable",
            "finish-line could not resolve its contained runner executable",
        )
    })?)
    .map_err(|_| {
        finish_line_unavailable(
            "finish-line-runner-unavailable",
            "finish-line could not canonicalize its contained runner executable",
        )
    })?;
    let interpreter = trusted_elf_interpreter(&executable)?;
    let runtime_max_ms = timeout_ms.saturating_add(2_000);
    let mut process = Command::new(TRUSTED_SYSTEMD_RUN);
    process
        .args(["--user", "--quiet", "--wait", "--pipe"])
        .arg(format!("--unit={unit}"))
        .args(CONTAINED_UNIT_PROPERTIES)
        .arg(format!(
            "--property=OpenFile={executable_source}:nils-runner:read-only"
        ))
        .arg(format!(
            "--property=OpenFile={}:nils-config:read-only",
            contained.path()
        ))
        .arg(format!(
            "--property=OpenFile={}:nils-control",
            control.path()
        ))
        .arg(format!("--property=RuntimeMaxSec={runtime_max_ms}ms"))
        .arg(format!("--working-directory={}", repo_root.display()))
        .arg("--")
        .arg(interpreter)
        .arg("/proc/self/fd/3")
        .arg(CONTAINED_RUNNER_ARG)
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = process.spawn().map_err(|_| {
        finish_line_unavailable(
            "finish-line-validation-spawn-failed",
            "finish-line could not start the authoritative validation command",
        )
    })?;
    let process_group = child.id();
    if let Err(error) = control.wait_ready(Instant::now() + Duration::from_secs(2)) {
        let _ = stop_contained_unit(unit);
        terminate_validation_process_group(&mut child);
        let _ = child.wait();
        return Err(error);
    }
    drop(contained);
    if let Err(error) = make_process_nondumpable() {
        let _ = stop_contained_unit(unit);
        terminate_validation_process_group(&mut child);
        let _ = child.wait();
        return Err(error);
    }
    if let Err(error) = control.acknowledge() {
        let _ = stop_contained_unit(unit);
        terminate_validation_process_group(&mut child);
        let _ = child.wait();
        return Err(error);
    }
    let stdout = child.stdout.take().ok_or_else(|| {
        finish_line_unavailable(
            "finish-line-validation-output-unavailable",
            "finish-line validation stdout is unavailable",
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        finish_line_unavailable(
            "finish-line-validation-output-unavailable",
            "finish-line validation stderr is unavailable",
        )
    })?;
    let output_settled = Arc::new(AtomicBool::new(false));
    let stdout_settled = Arc::clone(&output_settled);
    let stderr_settled = Arc::clone(&output_settled);
    let stdout =
        thread::spawn(move || read_validation_stream(stdout, output_max_bytes, stdout_settled));
    let stderr =
        thread::spawn(move || read_validation_stream(stderr, output_max_bytes, stderr_settled));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut aborted = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let signal = VALIDATION_CANCEL_SIGNAL.load(Ordering::SeqCst);
                if signal != 0 {
                    aborted = true;
                    stop_contained_unit(unit)?;
                    terminate_validation_process_group(&mut child);
                    break child.wait().map_err(|_| {
                        finish_line_unavailable(
                            "finish-line-validation-wait-failed",
                            "finish-line validation could not be reaped after cancellation",
                        )
                    })?;
                }
                if Instant::now() >= deadline {
                    timed_out = true;
                    stop_contained_unit(unit)?;
                    terminate_validation_process_group(&mut child);
                    break child.wait().map_err(|_| {
                        finish_line_unavailable(
                            "finish-line-validation-wait-failed",
                            "finish-line validation could not be reaped after timeout",
                        )
                    })?;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = stop_contained_unit(unit);
                terminate_validation_process_group(&mut child);
                let _ = child.wait();
                return Err(finish_line_unavailable(
                    "finish-line-validation-wait-failed",
                    "finish-line validation process state is unavailable",
                ));
            }
        }
    };
    terminate_validation_descendants(process_group);
    output_settled.store(true, Ordering::SeqCst);
    let (stdout, stdout_truncated) = join_validation_stream(stdout)?;
    let (stderr, stderr_truncated) = join_validation_stream(stderr)?;
    let unit_status = contained_unit_status(unit).inspect_err(|_| {
        let _ = stop_contained_unit(unit);
    })?;
    let _ = reset_contained_unit(unit);
    let reported = control.receive()?;
    let runner_record = contained_runner_record_name(reported.as_ref());
    let (exit_code, observed_signal) = match reported {
        Some(ContainedRunnerOutcome::Exited { exit_code })
            if status.success()
                && unit_status.exit_code == Some(0)
                && unit_status.signal.is_none() =>
        {
            (Some(exit_code), None)
        }
        Some(ContainedRunnerOutcome::Signaled { signal })
            if status.success()
                && unit_status.exit_code == Some(0)
                && unit_status.signal.is_none() =>
        {
            (None, Some(signal))
        }
        Some(ContainedRunnerOutcome::InfrastructureFailure { .. }) => {
            return Err(finish_line_unavailable(
                "finish-line-validation-spawn-failed",
                "finish-line contained runner could not initialize or execute the authoritative command",
            ));
        }
        None if timed_out || aborted => (unit_status.exit_code, unit_status.signal),
        _ => {
            return Err(finish_line_unavailable(
                "finish-line-containment-failed",
                &contained_boundary_disagreement(runner_record, status.success(), &unit_status),
            ));
        }
    };
    let stderr_text = String::from_utf8_lossy(&stderr).into_owned();
    let sandbox = match runner {
        ValidationRunner::Unsandboxed => None,
        ValidationRunner::DangerFullAccess => Some(ValidationSandbox {
            mode: "danger-full-access".to_string(),
            denied: false,
            enforcement: None,
        }),
        ValidationRunner::Confined {
            mode,
            enforcement,
            denial_signatures,
            runner_failure_rules,
            ..
        } => {
            if classify_runner_failure(exit_code, &stderr_text, runner_failure_rules).is_some() {
                return Err(finish_line_unavailable(
                    "finish-line-sandbox-runner-failed",
                    "the DSH sandbox runner failed before the validation command could run",
                ));
            }
            Some(ValidationSandbox {
                mode: mode.as_str().to_string(),
                denied: matches_sandbox_signature(exit_code, &stderr_text, denial_signatures),
                enforcement: Some(*enforcement),
            })
        }
    };
    Ok(ValidationExecution {
        exit_code,
        signal: observed_signal.map(signal_name).transpose()?,
        timed_out,
        aborted,
        timeout_ms,
        stdout: ValidationStream {
            text: String::from_utf8_lossy(&stdout).into_owned(),
            truncated: stdout_truncated,
        },
        stderr: ValidationStream {
            text: stderr_text,
            truncated: stderr_truncated,
        },
        sandbox,
    })
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct ContainedUnitStatus {
    exit_code: Option<i32>,
    signal: Option<i32>,
}

#[cfg(target_os = "linux")]
fn contained_runner_record_name(reported: Option<&ContainedRunnerOutcome>) -> &'static str {
    match reported {
        None => "absent",
        Some(ContainedRunnerOutcome::Ready) => "ready",
        Some(ContainedRunnerOutcome::Acknowledged) => "acknowledged",
        Some(ContainedRunnerOutcome::Exited { .. }) => "exited",
        Some(ContainedRunnerOutcome::Signaled { .. }) => "signaled",
        Some(ContainedRunnerOutcome::InfrastructureFailure { .. }) => "infrastructure-failure",
    }
}

/// Name the three independent teardown observers and what each of them saw.
///
/// An outcome is trusted only when the sealed runner record, the
/// `systemd-run --wait` client status, and the manager's own accounting of the
/// unit main process all agree. Reporting only that they disagreed makes an
/// unavailable containment substrate, a manager that recorded a teardown
/// failure, and a real containment regression render identically, so a reader
/// cannot tell an environment problem from a lost security property. The
/// rendered facts are the same bounded classifications a successful envelope
/// already carries; no unit name, path, command, or caller identity appears.
#[cfg(target_os = "linux")]
fn contained_boundary_disagreement(
    runner_record: &str,
    client_reported_success: bool,
    unit_status: &ContainedUnitStatus,
) -> String {
    let unit = match (unit_status.exit_code, unit_status.signal) {
        (Some(code), _) => format!("exit:{code}"),
        (None, Some(signal)) => format!("signal:{signal}"),
        (None, None) => "indeterminate".to_string(),
    };
    let client = if client_reported_success {
        "ok"
    } else {
        "failed"
    };
    format!(
        "finish-line contained runner did not reach an authenticated execution boundary \
         (sealed runner record={runner_record}; systemd-run client={client}; \
         unit main={unit}); all three must agree before an outcome is trusted"
    )
}

#[cfg(target_os = "linux")]
struct ContainedRunnerControlFile {
    reader: File,
    writer: Option<File>,
    path: String,
    nonce: String,
}

#[cfg(target_os = "linux")]
impl ContainedRunnerControlFile {
    fn create() -> Result<Self, HookError> {
        let name = CString::new("nils-finish-line-control").expect("static memfd name");
        let descriptor = unsafe {
            libc::memfd_create(name.as_ptr(), libc::MFD_ALLOW_SEALING | libc::MFD_CLOEXEC)
        };
        if descriptor < 0 {
            return Err(finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line could not create its contained runner control file",
            ));
        }
        let writer = unsafe { File::from_raw_fd(descriptor) };
        if unsafe { libc::fchmod(writer.as_raw_fd(), 0o600) } != 0 {
            return Err(finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line could not initialize its contained runner control file",
            ));
        }
        let path = format!("/proc/{}/fd/{}", std::process::id(), writer.as_raw_fd());
        let reader = File::open(&path).map_err(|_| {
            finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line could not create a read-only contained runner control view",
            )
        })?;
        Ok(Self {
            reader,
            writer: Some(writer),
            path,
            nonce: Uuid::new_v4().simple().to_string(),
        })
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn wait_ready(&mut self, deadline: Instant) -> Result<(), HookError> {
        loop {
            match self.read_message(false)? {
                Some(ContainedRunnerOutcome::Ready) => return Ok(()),
                Some(ContainedRunnerOutcome::InfrastructureFailure { .. }) => {
                    return Err(finish_line_unavailable(
                        "finish-line-validation-spawn-failed",
                        "finish-line contained runner could not initialize before provider execution",
                    ));
                }
                Some(_) | None if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5));
                }
                Some(_) | None => {
                    return Err(finish_line_unavailable(
                        "finish-line-containment-failed",
                        "finish-line contained runner did not reach its pre-exec boundary",
                    ));
                }
            }
        }
    }

    fn acknowledge(&mut self) -> Result<(), HookError> {
        let writer = self.writer.as_mut().ok_or_else(|| {
            finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line contained runner control writer is unavailable",
            )
        })?;
        write_contained_runner_control_message(
            writer,
            &self.nonce,
            ContainedRunnerOutcome::Acknowledged,
            false,
        )?;
        self.writer.take();
        if unsafe { libc::fchmod(self.reader.as_raw_fd(), 0o400) } != 0 {
            return Err(finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line contained runner control could not become read-only before provider execution",
            ));
        }
        Ok(())
    }

    fn receive(&mut self) -> Result<Option<ContainedRunnerOutcome>, HookError> {
        self.read_message(true)
    }

    fn read_message(
        &mut self,
        require_immutable: bool,
    ) -> Result<Option<ContainedRunnerOutcome>, HookError> {
        let required_seals =
            libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
        let metadata = self.reader.metadata().map_err(|_| {
            finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line contained runner control result metadata is unavailable",
            )
        })?;
        let seals = unsafe { libc::fcntl(self.reader.as_raw_fd(), libc::F_GET_SEALS) };
        if metadata.len() == 0 && seals == 0 {
            return Ok(None);
        }
        if require_immutable && (seals < 0 || seals & required_seals != required_seals) {
            return Err(finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line contained runner control result is not immutable",
            ));
        }
        self.reader.seek(SeekFrom::Start(0)).map_err(|_| {
            finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line contained runner control result is unavailable",
            )
        })?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut self.reader)
            .take((CONTAINED_RUNNER_CONTROL_MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                finish_line_unavailable(
                    "finish-line-containment-failed",
                    "finish-line contained runner control result is unavailable",
                )
            })?;
        if bytes.is_empty() {
            return Ok(None);
        }
        if bytes.len() > CONTAINED_RUNNER_CONTROL_MAX_BYTES {
            return Err(HookError::data(
                "finish-line-containment-invalid",
                "finish-line contained runner control result exceeds its bounded size",
            ));
        }
        let message = match serde_json::from_slice::<ContainedRunnerControlMessage>(&bytes) {
            Ok(message) => message,
            Err(_) if !require_immutable => return Ok(None),
            Err(_) => {
                return Err(HookError::data(
                    "finish-line-containment-invalid",
                    "finish-line contained runner control result does not match its strict schema",
                ));
            }
        };
        if message.schema_version != CONTAINED_RUNNER_CONTROL_SCHEMA || message.nonce != self.nonce
        {
            if !require_immutable {
                return Ok(None);
            }
            return Err(HookError::data(
                "finish-line-containment-invalid",
                "finish-line contained runner control result is not authenticated",
            ));
        }
        Ok(Some(message.outcome))
    }
}

#[cfg(target_os = "linux")]
struct SealedContainedRunnerConfig {
    _file: File,
    path: String,
}

#[cfg(target_os = "linux")]
impl SealedContainedRunnerConfig {
    fn path(&self) -> &str {
        &self.path
    }
}

#[cfg(target_os = "linux")]
fn sealed_contained_runner_config(
    repo_root: &Path,
    argv: Vec<String>,
    control_nonce: String,
) -> Result<SealedContainedRunnerConfig, HookError> {
    let config = ContainedRunnerConfig {
        schema_version: CONTAINED_RUNNER_SCHEMA.to_string(),
        cwd: repo_root.to_path_buf(),
        argv,
        environment: contained_environment(),
        supervisor_pid: std::process::id(),
        control_nonce,
    };
    validate_contained_runner_config(&config)?;
    let bytes = serde_json::to_vec(&config).map_err(|_| {
        finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line contained runner config could not be serialized",
        )
    })?;
    if bytes.len() as u64 > CONTAINED_RUNNER_MAX_BYTES {
        return Err(HookError::data(
            "finish-line-containment-invalid",
            "finish-line contained runner config exceeds its bounded size",
        ));
    }
    let name = CString::new("nils-finish-line-config").expect("static memfd name");
    let descriptor =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_ALLOW_SEALING | libc::MFD_CLOEXEC) };
    if descriptor < 0 {
        return Err(finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line could not create a sealed contained runner config",
        ));
    }
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    if file.write_all(&bytes).is_err()
        || unsafe { libc::fchmod(file.as_raw_fd(), 0o400) } != 0
        || unsafe { libc::lseek(file.as_raw_fd(), 0, libc::SEEK_SET) } != 0
    {
        return Err(finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line could not initialize its sealed contained runner config",
        ));
    }
    let required_seals =
        libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, required_seals) } != 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) } & required_seals
            != required_seals
    {
        return Err(finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line could not seal its contained runner config",
        ));
    }
    let path = format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd());
    Ok(SealedContainedRunnerConfig { _file: file, path })
}

#[cfg(target_os = "linux")]
fn contained_environment() -> BTreeMap<String, String> {
    const GIT_OVERRIDES: &[&str] = &[
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_COUNT",
    ];
    std::env::vars_os()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            let value = value.into_string().ok()?;
            if GIT_OVERRIDES.contains(&name.as_str()) || sensitive_environment_name(&name) {
                return None;
            }
            Some((name, value))
        })
        .collect()
}

fn sensitive_environment_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "PASSWORD", "SECRET", "TOKEN"]
        .iter()
        .any(|part| upper.contains(part))
}

fn validate_contained_runner_config(config: &ContainedRunnerConfig) -> Result<(), HookError> {
    let argv_bytes = config.argv.iter().map(String::len).sum::<usize>();
    let environment_bytes = config
        .environment
        .iter()
        .map(|(name, value)| name.len().saturating_add(value.len()))
        .sum::<usize>();
    let invalid = config.schema_version != CONTAINED_RUNNER_SCHEMA
        || !config.cwd.is_absolute()
        || config.argv.is_empty()
        || config.argv.len() > MAX_PROVIDER_ARGV_ENTRIES
        || argv_bytes > MAX_PROVIDER_ARGV_BYTES
        || config
            .argv
            .iter()
            .any(|argument| argument.is_empty() || argument.contains('\0'))
        || config.environment.len() > 512
        || environment_bytes > 128 * 1024
        || config.supervisor_pid == 0
        || config.control_nonce.len() != 32
        || !config
            .control_nonce
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || config.environment.iter().any(|(name, value)| {
            name.is_empty()
                || name.contains(['=', '\0'])
                || value.contains('\0')
                || sensitive_environment_name(name)
        });
    if invalid {
        return Err(HookError::data(
            "finish-line-containment-invalid",
            "finish-line contained runner config is invalid or exceeds its bounds",
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn quiesce_contained_unit(_unit: &str) -> Result<(), HookError> {
    Err(finish_line_unavailable(
        "finish-line-containment-unavailable",
        "authoritative finish-line execution requires Linux systemd cgroup containment",
    ))
}

#[cfg(target_os = "linux")]
fn quiesce_contained_unit(unit: &str) -> Result<(), HookError> {
    validate_contained_unit_name(unit)?;
    stop_contained_unit(unit)?;
    if !contained_unit_is_quiescent(unit)? {
        return Err(finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line contained execution unit did not become quiescent",
        ));
    }
    let _ = reset_contained_unit(unit);
    Ok(())
}

#[cfg(target_os = "linux")]
fn stop_contained_unit(unit: &str) -> Result<(), HookError> {
    validate_contained_unit_name(unit)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    wait_for_stable_contained_unit_quiescence(deadline, Duration::from_millis(25), || {
        let _ = bounded_systemctl(&["--user", "stop", unit], false)?;
        Ok((
            contained_unit_is_quiescent(unit)?,
            contained_unit_has_pending_job(unit)?,
        ))
    })
}

#[cfg(target_os = "linux")]
fn wait_for_stable_contained_unit_quiescence(
    deadline: Instant,
    pause: Duration,
    mut observe: impl FnMut() -> Result<(bool, bool), HookError>,
) -> Result<(), HookError> {
    let mut stable_observations = 0_u8;
    loop {
        let (quiescent, pending_job) = observe()?;
        if quiescent && !pending_job {
            stable_observations = stable_observations.saturating_add(1);
            if stable_observations >= 3 {
                return Ok(());
            }
        } else {
            stable_observations = 0;
        }
        if Instant::now() >= deadline {
            return Err(finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line contained execution unit did not stop cleanly",
            ));
        }
        if !pause.is_zero() {
            thread::sleep(pause);
        }
    }
}

#[cfg(target_os = "linux")]
fn contained_unit_has_pending_job(unit: &str) -> Result<bool, HookError> {
    validate_contained_unit_name(unit)?;
    let (status, output) = bounded_systemctl(
        &[
            "--user",
            "list-jobs",
            unit,
            "--no-legend",
            "--plain",
            "--no-pager",
        ],
        true,
    )?;
    if !status.success() || output.len() > 4 * 1024 {
        return Err(finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line contained execution job state is unavailable",
        ));
    }
    let output = String::from_utf8(output).map_err(|_| {
        finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line contained execution job state is invalid",
        )
    })?;
    Ok(output.lines().any(|line| !line.trim().is_empty()))
}

#[cfg(target_os = "linux")]
fn validate_contained_unit_name(unit: &str) -> Result<(), HookError> {
    let suffix = unit.strip_prefix("nils-finish-line-");
    if suffix.is_none_or(|suffix| {
        suffix.len() != 32 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return Err(HookError::data(
            "finish-line-containment-invalid",
            "finish-line contained execution unit identity is invalid",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn bounded_systemctl(
    args: &[&str],
    capture_stdout: bool,
) -> Result<(std::process::ExitStatus, Vec<u8>), HookError> {
    let mut child = Command::new(TRUSTED_SYSTEMCTL)
        .args(args)
        .stdin(Stdio::null())
        .stdout(if capture_stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| {
            finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line could not query its contained execution unit",
            )
        })?;
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut output = Vec::new();
                if let Some(stdout) = child.stdout.take() {
                    stdout
                        .take(16 * 1024)
                        .read_to_end(&mut output)
                        .map_err(|_| {
                            finish_line_unavailable(
                                "finish-line-containment-failed",
                                "finish-line contained unit status is unreadable",
                            )
                        })?;
                }
                return Ok((status, output));
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) | Err(_) => {
                terminate_validation_process_group(&mut child);
                let _ = child.wait();
                return Err(finish_line_unavailable(
                    "finish-line-containment-failed",
                    "finish-line contained unit control exceeded its bounded deadline",
                ));
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn contained_unit_is_quiescent(unit: &str) -> Result<bool, HookError> {
    validate_contained_unit_name(unit)?;
    let (status, output) = bounded_systemctl(
        &[
            "--user",
            "show",
            unit,
            "--property=LoadState",
            "--property=ActiveState",
            "--property=SubState",
            "--property=ControlGroup",
        ],
        true,
    )?;
    if !status.success() {
        return Ok(false);
    }
    let text = String::from_utf8(output).map_err(|_| {
        finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line contained unit status is invalid",
        )
    })?;
    let mut load = None;
    let mut active = None;
    let mut sub = None;
    let mut control_group = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("LoadState=") {
            load = Some(value);
        } else if let Some(value) = line.strip_prefix("ActiveState=") {
            active = Some(value);
        } else if let Some(value) = line.strip_prefix("SubState=") {
            sub = Some(value);
        } else if let Some(value) = line.strip_prefix("ControlGroup=") {
            control_group = Some(value);
        }
    }
    if load == Some("not-found") {
        return Ok(true);
    }
    if !matches!(active, Some("inactive" | "failed")) || !matches!(sub, Some("dead" | "failed")) {
        return Ok(false);
    }
    let Some(control_group) = control_group.filter(|value| !value.is_empty()) else {
        return Ok(true);
    };
    if !control_group.starts_with('/')
        || control_group
            .split('/')
            .any(|component| component == ".." || component.contains('\0'))
    {
        return Err(HookError::data(
            "finish-line-containment-invalid",
            "finish-line contained unit cgroup identity is invalid",
        ));
    }
    let events = Path::new("/sys/fs/cgroup")
        .join(control_group.trim_start_matches('/'))
        .join("cgroup.events");
    match fs::read_to_string(events) {
        Ok(events) => Ok(events.lines().any(|line| line == "populated 0")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(_) => Err(finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line contained unit cgroup state is unavailable",
        )),
    }
}

#[cfg(target_os = "linux")]
fn reset_contained_unit(unit: &str) -> Result<(), HookError> {
    validate_contained_unit_name(unit)?;
    let _ = bounded_systemctl(&["--user", "reset-failed", unit], false)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn contained_unit_status(unit: &str) -> Result<ContainedUnitStatus, HookError> {
    validate_contained_unit_name(unit)?;
    let (status, stdout) = bounded_systemctl(
        &[
            "--user",
            "show",
            unit,
            "--property=Result",
            "--property=ExecMainCode",
            "--property=ExecMainStatus",
            "--property=ActiveState",
            "--no-pager",
        ],
        true,
    )?;
    if !status.success() || stdout.len() > 4 * 1024 || !contained_unit_is_quiescent(unit)? {
        return Err(finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line contained execution status is unavailable",
        ));
    }
    let rendered = String::from_utf8_lossy(&stdout);
    let mut fields = BTreeMap::new();
    for line in rendered.lines() {
        let Some((name, value)) = line.split_once('=') else {
            return Err(finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line contained execution status is malformed",
            ));
        };
        fields.insert(name, value);
    }
    let code = fields
        .get("ExecMainCode")
        .and_then(|value| value.parse::<i32>().ok())
        .ok_or_else(|| {
            finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line contained execution code is unavailable",
            )
        })?;
    let status = fields
        .get("ExecMainStatus")
        .and_then(|value| value.parse::<i32>().ok())
        .ok_or_else(|| {
            finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line contained execution status is unavailable",
            )
        })?;
    if fields.get("ActiveState") != Some(&"inactive")
        && fields.get("ActiveState") != Some(&"failed")
    {
        return Err(finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line contained execution unit is not quiescent",
        ));
    }
    match code {
        0 | 1 => Ok(ContainedUnitStatus {
            exit_code: Some(status),
            signal: None,
        }),
        2 | 3 if status > 0 => Ok(ContainedUnitStatus {
            exit_code: None,
            signal: Some(status),
        }),
        _ => Err(finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line contained execution facts are invalid",
        )),
    }
}

fn validate_run_execution<'a>(
    execution: &'a RunExecution,
    repo_root: &Path,
    command: &str,
    require_repo_root: bool,
) -> Result<(&'a ValidationRunner, usize, PathBuf), HookError> {
    let RunExecution::BashV1 {
        workdir,
        output_max_bytes,
        runner,
    } = execution;
    let canonical_workdir = fs::canonicalize(workdir).map_err(|_| {
        HookError::data(
            "finish-line-execution-invalid",
            "finish-line execution workdir must be the authoritative repository root",
        )
    })?;
    if (require_repo_root && canonical_workdir != repo_root)
        || *output_max_bytes == 0
        || *output_max_bytes > MAX_VALIDATION_OUTPUT_BYTES
    {
        return Err(HookError::data(
            "finish-line-execution-invalid",
            "finish-line execution workdir or output budget is invalid for this command class",
        ));
    }
    if let ValidationRunner::Confined {
        argv,
        denial_signatures,
        runner_failure_rules,
        ..
    } = runner
    {
        validate_provider_argv(argv, command)?;
        validate_sandbox_classifier(denial_signatures, runner_failure_rules)?;
    }
    Ok((runner, *output_max_bytes, canonical_workdir))
}

fn validate_provider_argv(argv: &[String], command: &str) -> Result<(), HookError> {
    let invalid = argv.len() < 4
        || argv.len() > MAX_PROVIDER_ARGV_ENTRIES
        || argv
            .iter()
            .any(|argument| argument.is_empty() || argument.contains('\0'))
        || argv.iter().map(|argument| argument.len()).sum::<usize>() > MAX_PROVIDER_ARGV_BYTES
        || argv[argv.len() - 4] != "--"
        || argv[argv.len() - 3] != "bash"
        || argv[argv.len() - 2] != "-c"
        || argv[argv.len() - 1] != command;
    if invalid {
        return Err(HookError::data(
            "finish-line-provider-argv-invalid",
            "finish-line provider argv must be a bounded DSH confinement wrapper around the exact validation command",
        ));
    }
    Ok(())
}

fn validate_sandbox_classifier(
    denial_signatures: &[String],
    rules: &[RunnerFailureRule],
) -> Result<(), HookError> {
    let valid_signature = |signature: &String| {
        !signature.trim().is_empty()
            && signature.len() <= MAX_SANDBOX_SIGNATURE_BYTES
            && !signature.contains(['\r', '\n', '\0'])
    };
    let invalid = denial_signatures.len() > MAX_SANDBOX_SIGNATURES
        || !denial_signatures.iter().all(valid_signature)
        || rules.len() > MAX_SANDBOX_RULES
        || rules.iter().any(|rule| {
            rule.fatal_signatures.is_empty()
                || rule.fatal_signatures.len() > MAX_SANDBOX_SIGNATURES
                || !rule.fatal_signatures.iter().all(valid_signature)
                || rule.informational_lines.len() > MAX_SANDBOX_SIGNATURES
                || !rule.informational_lines.iter().all(valid_signature)
                || rule.allowed_exit_codes.as_ref().is_some_and(|codes| {
                    codes.is_empty()
                        || codes.len() > MAX_SANDBOX_RULE_EXIT_CODES
                        || codes.iter().any(|code| !(1..=255).contains(code))
                })
        });
    if invalid {
        return Err(HookError::data(
            "finish-line-sandbox-classifier-invalid",
            "finish-line sandbox classifier exceeds the bounded DSH contract",
        ));
    }
    Ok(())
}

fn classify_runner_failure<'a>(
    exit_code: Option<i32>,
    stderr: &'a str,
    rules: &[RunnerFailureRule],
) -> Option<&'a str> {
    let exit_code = exit_code.filter(|code| *code != 0)?;
    for rule in rules {
        if rule
            .allowed_exit_codes
            .as_ref()
            .is_some_and(|codes| !codes.contains(&exit_code))
        {
            continue;
        }
        for line in stderr.lines() {
            if rule
                .informational_lines
                .iter()
                .any(|info| line.eq_ignore_ascii_case(info))
            {
                continue;
            }
            if rule
                .fatal_signatures
                .iter()
                .any(|signature| line.to_lowercase().contains(&signature.to_lowercase()))
            {
                return Some(line);
            }
        }
    }
    None
}

fn matches_sandbox_signature(exit_code: Option<i32>, stderr: &str, signatures: &[String]) -> bool {
    if exit_code.is_none_or(|code| code == 0) {
        return false;
    }
    let stderr = stderr.to_lowercase();
    signatures
        .iter()
        .any(|signature| stderr.contains(&signature.to_lowercase()))
}

#[cfg(target_os = "linux")]
fn trusted_elf_interpreter(executable: &Path) -> Result<PathBuf, HookError> {
    let mut file = File::open(executable).map_err(|_| {
        finish_line_unavailable(
            "finish-line-runner-unavailable",
            "finish-line contained runner executable is unreadable",
        )
    })?;
    let mut header = [0_u8; 64];
    file.read_exact(&mut header).map_err(|_| {
        finish_line_unavailable(
            "finish-line-runner-unavailable",
            "finish-line contained runner ELF header is unavailable",
        )
    })?;
    if &header[..4] != b"\x7fELF" || header[4] != 2 || header[5] != 1 {
        return Err(finish_line_unavailable(
            "finish-line-runner-unavailable",
            "finish-line contained runner requires a dynamic ELF64 little-endian executable",
        ));
    }
    let program_offset = u64::from_le_bytes(header[32..40].try_into().expect("ELF offset"));
    let entry_size = u16::from_le_bytes(header[54..56].try_into().expect("ELF entry size"));
    let entry_count = u16::from_le_bytes(header[56..58].try_into().expect("ELF entry count"));
    if entry_size < 56 || entry_count == 0 || entry_count > 128 {
        return Err(finish_line_unavailable(
            "finish-line-runner-unavailable",
            "finish-line contained runner ELF program table is invalid",
        ));
    }
    let mut interpreter = None;
    for index in 0..entry_count {
        file.seek(SeekFrom::Start(
            program_offset + u64::from(index) * u64::from(entry_size),
        ))
        .map_err(|_| {
            finish_line_unavailable(
                "finish-line-runner-unavailable",
                "finish-line contained runner ELF program table is unreadable",
            )
        })?;
        let mut entry = vec![0_u8; usize::from(entry_size)];
        file.read_exact(&mut entry).map_err(|_| {
            finish_line_unavailable(
                "finish-line-runner-unavailable",
                "finish-line contained runner ELF program entry is unavailable",
            )
        })?;
        if u32::from_le_bytes(entry[0..4].try_into().expect("ELF program type")) != 3 {
            continue;
        }
        let offset = u64::from_le_bytes(entry[8..16].try_into().expect("ELF interpreter offset"));
        let size = u64::from_le_bytes(entry[32..40].try_into().expect("ELF interpreter size"));
        if !(2..=4_096).contains(&size) {
            break;
        }
        file.seek(SeekFrom::Start(offset)).map_err(|_| {
            finish_line_unavailable(
                "finish-line-runner-unavailable",
                "finish-line contained runner interpreter identity is unreadable",
            )
        })?;
        let mut bytes = vec![0_u8; usize::try_from(size).expect("bounded interpreter size")];
        file.read_exact(&mut bytes).map_err(|_| {
            finish_line_unavailable(
                "finish-line-runner-unavailable",
                "finish-line contained runner interpreter identity is unavailable",
            )
        })?;
        if bytes.pop() != Some(0) || bytes.is_empty() || bytes.contains(&0) {
            break;
        }
        interpreter = Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)));
        break;
    }
    let interpreter = interpreter.ok_or_else(|| {
        finish_line_unavailable(
            "finish-line-runner-unavailable",
            "finish-line contained runner has no trusted dynamic interpreter",
        )
    })?;
    let interpreter = fs::canonicalize(interpreter).map_err(|_| {
        finish_line_unavailable(
            "finish-line-runner-unavailable",
            "finish-line contained runner interpreter is unavailable",
        )
    })?;
    let metadata = fs::metadata(&interpreter).map_err(|_| {
        finish_line_unavailable(
            "finish-line-runner-unavailable",
            "finish-line contained runner interpreter metadata is unavailable",
        )
    })?;
    let mode = metadata.permissions().mode();
    if !metadata.is_file() || metadata.uid() != 0 || mode & 0o111 == 0 || mode & 0o022 != 0 {
        return Err(HookError::data(
            "finish-line-runner-untrusted",
            "finish-line dynamic interpreter owner, type, executable bit, or mode is untrusted",
        ));
    }
    Ok(interpreter)
}

fn validate_trusted_bash() -> Result<(), HookError> {
    let metadata = fs::symlink_metadata(TRUSTED_BASH).map_err(|_| {
        finish_line_unavailable(
            "finish-line-shell-unavailable",
            "finish-line trusted /bin/bash is unavailable",
        )
    })?;
    let mode = metadata.permissions().mode();
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || mode & 0o111 == 0
        || mode & 0o022 != 0
    {
        return Err(HookError::data(
            "finish-line-shell-untrusted",
            "finish-line /bin/bash owner, type, executable bit, or mode is untrusted",
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn validate_containment_host() -> Result<(), HookError> {
    Err(finish_line_unavailable(
        "finish-line-containment-unavailable",
        "authoritative finish-line execution requires Linux systemd cgroup containment",
    ))
}

#[cfg(target_os = "linux")]
fn validate_containment_host() -> Result<(), HookError> {
    validate_trusted_systemd()?;
    if !Path::new("/sys/fs/cgroup/cgroup.controllers").is_file() {
        return Err(finish_line_unavailable(
            "finish-line-containment-unavailable",
            "finish-line requires a unified cgroup v2 host",
        ));
    }
    let namespaces = fs::read_to_string("/proc/sys/user/max_user_namespaces")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    if namespaces == 0 {
        return Err(finish_line_unavailable(
            "finish-line-containment-unavailable",
            "finish-line requires unprivileged user namespaces for contained execution",
        ));
    }
    let status = bounded_systemctl(&["--user", "show-environment"], false)?.0;
    if !status.success() {
        return Err(finish_line_unavailable(
            "finish-line-containment-unavailable",
            "finish-line systemd user manager is unavailable",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_trusted_systemd() -> Result<(), HookError> {
    for path in [TRUSTED_SYSTEMD_RUN, TRUSTED_SYSTEMCTL] {
        let metadata = fs::symlink_metadata(path).map_err(|_| {
            finish_line_unavailable(
                "finish-line-containment-unavailable",
                "finish-line systemd containment executables are unavailable",
            )
        })?;
        let mode = metadata.permissions().mode();
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != 0
            || mode & 0o111 == 0
            || mode & 0o022 != 0
        {
            return Err(HookError::data(
                "finish-line-containment-untrusted",
                "finish-line systemd containment executable identity is untrusted",
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn supervisor_pidfd(pid: u32) -> Result<File, HookError> {
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
    if descriptor < 0 {
        return Err(finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line could not bind its supervisor process identity",
        ));
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(target_os = "linux")]
fn make_process_nondumpable() -> Result<(), HookError> {
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0
        || unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } != 0
    {
        return Err(finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line could not isolate its trusted descriptors from the provider",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn abort_contained_runner(message: &str) -> ! {
    eprintln!("agent-hook: finish-line contained runner failed: {message}");
    unsafe { libc::_exit(255) }
}

#[cfg(target_os = "linux")]
fn send_contained_runner_outcome(
    config: &ContainedRunnerConfig,
    control: &mut File,
    outcome: ContainedRunnerOutcome,
) -> Result<(), HookError> {
    write_contained_runner_control_message(control, &config.control_nonce, outcome, true)
}

#[cfg(target_os = "linux")]
fn write_contained_runner_control_message(
    control: &mut File,
    nonce: &str,
    outcome: ContainedRunnerOutcome,
    seal: bool,
) -> Result<(), HookError> {
    let message = ContainedRunnerControlMessage {
        schema_version: CONTAINED_RUNNER_CONTROL_SCHEMA.to_string(),
        nonce: nonce.to_string(),
        outcome,
    };
    let bytes = serde_json::to_vec(&message).map_err(|_| {
        finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line contained runner control result could not be serialized",
        )
    })?;
    if bytes.len() > CONTAINED_RUNNER_CONTROL_MAX_BYTES {
        return Err(HookError::data(
            "finish-line-containment-invalid",
            "finish-line contained runner control result exceeds its bounded size",
        ));
    }
    if control.set_len(0).is_err()
        || control.seek(SeekFrom::Start(0)).is_err()
        || control.write_all(&bytes).is_err()
        || control.sync_all().is_err()
    {
        return Err(finish_line_unavailable(
            "finish-line-containment-failed",
            "finish-line contained runner control result could not be delivered",
        ));
    }
    if seal {
        let required_seals =
            libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
        if unsafe { libc::fcntl(control.as_raw_fd(), libc::F_ADD_SEALS, required_seals) } != 0
            || unsafe { libc::fcntl(control.as_raw_fd(), libc::F_GET_SEALS) } & required_seals
                != required_seals
        {
            return Err(finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line contained runner control result could not be made immutable",
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn wait_for_contained_runner_acknowledgement(
    config: &ContainedRunnerConfig,
    control: &mut File,
) -> Result<(), HookError> {
    write_contained_runner_control_message(
        control,
        &config.control_nonce,
        ContainedRunnerOutcome::Ready,
        false,
    )?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        control.seek(SeekFrom::Start(0)).map_err(|_| {
            finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line contained runner acknowledgement is unavailable",
            )
        })?;
        let mut bytes = Vec::new();
        Read::by_ref(control)
            .take((CONTAINED_RUNNER_CONTROL_MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                finish_line_unavailable(
                    "finish-line-containment-failed",
                    "finish-line contained runner acknowledgement is unavailable",
                )
            })?;
        let acknowledged = serde_json::from_slice::<ContainedRunnerControlMessage>(&bytes)
            .ok()
            .is_some_and(|message| {
                message.schema_version == CONTAINED_RUNNER_CONTROL_SCHEMA
                    && message.nonce == config.control_nonce
                    && matches!(message.outcome, ContainedRunnerOutcome::Acknowledged)
            });
        let mode = control
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o777)
            .unwrap_or_default();
        if acknowledged && mode == 0o400 {
            if control.set_len(0).is_err()
                || control.seek(SeekFrom::Start(0)).is_err()
                || control.sync_all().is_err()
            {
                return Err(finish_line_unavailable(
                    "finish-line-containment-failed",
                    "finish-line contained runner could not clear its pre-exec acknowledgement",
                ));
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line contained runner did not receive its pre-exec acknowledgement",
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
fn finish_contained_runner(
    config: &ContainedRunnerConfig,
    control: &mut File,
    outcome: ContainedRunnerOutcome,
) -> ! {
    if send_contained_runner_outcome(config, control, outcome).is_err() {
        abort_contained_runner("control result delivery failed");
    }
    unsafe { libc::_exit(0) }
}

#[cfg(target_os = "linux")]
fn fail_contained_runner(
    config: &ContainedRunnerConfig,
    control: &mut File,
    code: &str,
    message: &str,
) -> ! {
    eprintln!("agent-hook: finish-line contained runner failed: {message}");
    if send_contained_runner_outcome(
        config,
        control,
        ContainedRunnerOutcome::InfrastructureFailure {
            code: code.to_string(),
        },
    )
    .is_err()
    {
        abort_contained_runner("control failure delivery failed");
    }
    unsafe { libc::_exit(255) }
}

#[cfg(target_os = "linux")]
pub(crate) fn exec_contained_runner() -> ! {
    let result = (|| -> Result<(ContainedRunnerConfig, File), HookError> {
        let listen_pid = std::env::var("LISTEN_PID")
            .ok()
            .and_then(|value| value.parse::<u32>().ok());
        let listen_fds = std::env::var("LISTEN_FDS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok());
        let listen_names = std::env::var("LISTEN_FDNAMES").ok();
        if listen_pid != Some(std::process::id())
            || listen_fds != Some(3)
            || listen_names.as_deref() != Some("nils-runner:nils-config:nils-control")
        {
            return Err(HookError::data(
                "finish-line-containment-invalid",
                "finish-line contained runner descriptors are invalid",
            ));
        }
        let control = unsafe { File::from_raw_fd(5) };
        let control_metadata = control.metadata().map_err(|_| {
            finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line contained runner control metadata is unavailable",
            )
        })?;
        let control_flags = unsafe { libc::fcntl(control.as_raw_fd(), libc::F_GETFD) };
        if !control_metadata.is_file()
            || control_metadata.permissions().mode() & 0o077 != 0
            || control_metadata.permissions().mode() & 0o600 != 0o600
            || control_metadata.nlink() != 0
            || control_flags < 0
            || unsafe {
                libc::fcntl(
                    control.as_raw_fd(),
                    libc::F_SETFD,
                    control_flags | libc::FD_CLOEXEC,
                )
            } != 0
        {
            return Err(HookError::data(
                "finish-line-containment-untrusted",
                "finish-line contained runner control file identity is untrusted",
            ));
        }
        let file = unsafe { File::from_raw_fd(4) };
        let metadata = file.metadata().map_err(|_| {
            finish_line_unavailable(
                "finish-line-containment-failed",
                "finish-line sealed contained runner config metadata is unavailable",
            )
        })?;
        let required_seals =
            libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o377 != 0
            || metadata.nlink() != 0
            || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) } & required_seals
                != required_seals
        {
            return Err(HookError::data(
                "finish-line-containment-untrusted",
                "finish-line contained runner config is not an immutable sealed file",
            ));
        }
        let mut bytes = Vec::new();
        file.take(CONTAINED_RUNNER_MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                finish_line_unavailable(
                    "finish-line-containment-failed",
                    "finish-line contained runner config could not be read",
                )
            })?;
        if bytes.len() as u64 > CONTAINED_RUNNER_MAX_BYTES {
            return Err(HookError::data(
                "finish-line-containment-invalid",
                "finish-line contained runner config exceeds its bounded size",
            ));
        }
        let config = serde_json::from_slice::<ContainedRunnerConfig>(&bytes).map_err(|_| {
            HookError::data(
                "finish-line-containment-invalid",
                "finish-line contained runner config does not match its strict schema",
            )
        })?;
        validate_contained_runner_config(&config)?;
        let canonical_cwd = fs::canonicalize(&config.cwd).map_err(|_| {
            HookError::data(
                "finish-line-containment-invalid",
                "finish-line contained runner cwd is unavailable",
            )
        })?;
        if canonical_cwd != config.cwd {
            return Err(HookError::data(
                "finish-line-containment-invalid",
                "finish-line contained runner cwd is not canonical",
            ));
        }
        Ok((config, control))
    })();
    let (config, mut control) = match result {
        Ok(result) => result,
        Err(error) => {
            abort_contained_runner(&format!("initialization ({})", error.code));
        }
    };
    if make_process_nondumpable().is_err() {
        fail_contained_runner(
            &config,
            &mut control,
            "provider-descriptor-isolation-unavailable",
            "trusted runner descriptor isolation is unavailable",
        );
    }
    if wait_for_contained_runner_acknowledgement(&config, &mut control).is_err() {
        fail_contained_runner(
            &config,
            &mut control,
            "pre-exec-acknowledgement-unavailable",
            "trusted pre-exec acknowledgement is unavailable",
        );
    }
    let disconnected = Arc::new(AtomicBool::new(false));
    let disconnected_reader = Arc::clone(&disconnected);
    let supervisor = match supervisor_pidfd(config.supervisor_pid) {
        Ok(supervisor) => supervisor,
        Err(_) => {
            fail_contained_runner(
                &config,
                &mut control,
                "supervisor-identity-unavailable",
                "supervisor identity unavailable",
            );
        }
    };
    let _control = thread::spawn(move || {
        let mut descriptor = libc::pollfd {
            fd: supervisor.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            descriptor.revents = 0;
            let result = unsafe { libc::poll(&mut descriptor, 1, 100) };
            if result > 0 && descriptor.revents & libc::POLLIN != 0 {
                disconnected_reader.store(true, Ordering::SeqCst);
                break;
            }
            if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                disconnected_reader.store(true, Ordering::SeqCst);
                break;
            }
        }
    });
    let mut command = Command::new(&config.argv[0]);
    command
        .args(&config.argv[1..])
        .current_dir(&config.cwd)
        .env_clear()
        .envs(&config.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            fail_contained_runner(
                &config,
                &mut control,
                "command-spawn-failed",
                "authoritative command spawn failed",
            );
        }
    };
    loop {
        if disconnected.load(Ordering::SeqCst) {
            terminate_validation_process_group(&mut child);
            let _ = child.wait();
            fail_contained_runner(
                &config,
                &mut control,
                "supervisor-disconnected",
                "supervisor disconnected before the command settled",
            );
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if let Some(code) = status.code() {
                    finish_contained_runner(
                        &config,
                        &mut control,
                        ContainedRunnerOutcome::Exited { exit_code: code },
                    );
                }
                use std::os::unix::process::ExitStatusExt;
                if let Some(signal) = status.signal() {
                    finish_contained_runner(
                        &config,
                        &mut control,
                        ContainedRunnerOutcome::Signaled { signal },
                    );
                }
                fail_contained_runner(
                    &config,
                    &mut control,
                    "command-status-unavailable",
                    "authoritative command status unavailable",
                );
            }
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(_) => {
                terminate_validation_process_group(&mut child);
                let _ = child.wait();
                fail_contained_runner(
                    &config,
                    &mut control,
                    "command-wait-failed",
                    "authoritative command wait failed",
                );
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    fn expired_candidate(session_key: &str) -> ExpiredOrphanCandidate {
        ExpiredOrphanCandidate {
            session_key: session_key.to_string(),
            capability_digest: format!("capability:{session_key}"),
            capability_incarnation: Some(1),
            lease_expires_at_epoch: 0,
            operations: Vec::new(),
        }
    }

    #[test]
    fn contained_unit_teardown_is_bounded_without_an_already_expired_stop_deadline() {
        let stop = CONTAINED_UNIT_PROPERTIES
            .iter()
            .find_map(|property| property.strip_prefix("--property=TimeoutStopSec="))
            .expect("a contained unit must declare its stop timeout");
        // Systemd reads `0` as a stop deadline that already expired, so a
        // teardown that has to kill a live descendant is recorded as
        // `Result=timeout` and the waiting client exits non-zero even though
        // containment succeeded. `infinity` would remove the bound entirely.
        assert_ne!(stop, "0", "an immediate stop deadline fabricates a timeout");
        assert_ne!(stop, "infinity", "unit teardown must stay bounded");
        let seconds = stop
            .strip_suffix('s')
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("stop timeout must be a bounded second count, got {stop}"));
        assert!(
            (1..=10).contains(&seconds),
            "stop timeout must stay a small positive bound, got {stop}",
        );
        // The bound never replaces the immediate kill; that stays with the
        // cgroup kill properties.
        for required in [
            "--property=KillMode=control-group",
            "--property=KillSignal=SIGKILL",
            "--property=FinalKillSignal=SIGKILL",
            "--property=SendSIGKILL=yes",
        ] {
            assert!(
                CONTAINED_UNIT_PROPERTIES.contains(&required),
                "missing containment property {required}",
            );
        }
    }

    #[test]
    fn a_disagreeing_teardown_boundary_names_every_observer() {
        let rendered = contained_boundary_disagreement(
            contained_runner_record_name(Some(&ContainedRunnerOutcome::Exited { exit_code: 0 })),
            false,
            &ContainedUnitStatus {
                exit_code: Some(0),
                signal: None,
            },
        );
        assert!(
            rendered.contains("sealed runner record=exited"),
            "{rendered}",
        );
        assert!(rendered.contains("systemd-run client=failed"), "{rendered}");
        assert!(rendered.contains("unit main=exit:0"), "{rendered}");

        let absent = contained_boundary_disagreement(
            contained_runner_record_name(None),
            true,
            &ContainedUnitStatus {
                exit_code: None,
                signal: Some(libc::SIGKILL),
            },
        );
        assert!(absent.contains("sealed runner record=absent"), "{absent}");
        assert!(absent.contains("systemd-run client=ok"), "{absent}");
        assert!(absent.contains("unit main=signal:9"), "{absent}");
    }

    #[test]
    fn stable_unit_wait_rejects_initial_absence_and_resets_on_late_appearance() {
        let mut observations = VecDeque::from([
            (true, false),
            (true, false),
            (false, true),
            (true, false),
            (true, false),
            (true, false),
        ]);
        let mut observed = 0_u8;
        wait_for_stable_contained_unit_quiescence(
            Instant::now() + Duration::from_secs(1),
            Duration::ZERO,
            || {
                observed = observed.saturating_add(1);
                observations.pop_front().ok_or_else(|| {
                    finish_line_unavailable(
                        "finish-line-containment-failed",
                        "scripted unit observation was exhausted",
                    )
                })
            },
        )
        .expect("late unit appearance must reset the stable observation count");
        assert_eq!(observed, 6);
        assert!(observations.is_empty());
    }

    #[test]
    fn expired_reclaim_window_rotates_past_a_busy_oldest_window() {
        let candidates = (0..=MAX_EXPIRED_ORPHAN_CANDIDATES)
            .map(|index| expired_candidate(&format!("session-{index:02}")))
            .collect::<Vec<_>>();

        let (first, cursor) = expired_reclaim_window(candidates.clone(), None);
        assert_eq!(first.len(), MAX_EXPIRED_ORPHAN_CANDIDATES);
        assert!(
            first
                .iter()
                .all(|candidate| candidate.session_key != "session-08")
        );

        let (second, _) = expired_reclaim_window(candidates, cursor.as_deref());
        assert_eq!(second[0].session_key, "session-08");
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn exec_contained_runner() -> ! {
    eprintln!("agent-hook: finish-line contained runner is unsupported on this platform");
    std::process::exit(203)
}

fn read_validation_stream(
    mut pipe: impl Read + AsRawFd,
    output_max_bytes: usize,
    settled: Arc<AtomicBool>,
) -> Result<(Vec<u8>, bool), HookError> {
    let descriptor = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(finish_line_unavailable(
            "finish-line-validation-output-failed",
            "finish-line validation output could not be bounded",
        ));
    }
    let mut retained = Vec::new();
    let mut total = 0_usize;
    let mut buffer = [0_u8; 8 * 1024];
    let mut drain_deadline = None;
    loop {
        if settled.load(Ordering::SeqCst) {
            let deadline =
                drain_deadline.get_or_insert_with(|| Instant::now() + OUTPUT_DRAIN_GRACE);
            if Instant::now() >= *deadline {
                return Ok((retained, total > output_max_bytes));
            }
        }
        let read = match pipe.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(_) => {
                return Err(finish_line_unavailable(
                    "finish-line-validation-output-failed",
                    "finish-line validation output could not be read",
                ));
            }
        };
        if read == 0 {
            return Ok((retained, total > output_max_bytes));
        }
        total = total.saturating_add(read);
        if read >= output_max_bytes {
            retained.clear();
            retained.extend_from_slice(&buffer[read - output_max_bytes..read]);
            continue;
        }
        let excess = retained
            .len()
            .saturating_add(read)
            .saturating_sub(output_max_bytes);
        if excess > 0 {
            retained.drain(..excess);
        }
        retained.extend_from_slice(&buffer[..read]);
    }
}

fn join_validation_stream(
    handle: thread::JoinHandle<Result<(Vec<u8>, bool), HookError>>,
) -> Result<(Vec<u8>, bool), HookError> {
    handle.join().map_err(|_| {
        finish_line_unavailable(
            "finish-line-validation-output-failed",
            "finish-line validation output reader stopped unexpectedly",
        )
    })?
}

fn terminate_validation_descendants(process_group: u32) {
    let process_group = process_group as libc::pid_t;
    unsafe {
        libc::kill(-process_group, libc::SIGKILL);
    }
}

fn terminate_validation_process_group(child: &mut std::process::Child) {
    terminate_validation_descendants(child.id());
    let _ = child.kill();
}

#[cfg(target_os = "linux")]
fn signal_name(signal: i32) -> Result<String, HookError> {
    let name = match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGSTKFLT => "SIGSTKFLT",
        libc::SIGCHLD => "SIGCHLD",
        libc::SIGCONT => "SIGCONT",
        libc::SIGSTOP => "SIGSTOP",
        libc::SIGTSTP => "SIGTSTP",
        libc::SIGTTIN => "SIGTTIN",
        libc::SIGTTOU => "SIGTTOU",
        libc::SIGURG => "SIGURG",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        libc::SIGVTALRM => "SIGVTALRM",
        libc::SIGPROF => "SIGPROF",
        libc::SIGWINCH => "SIGWINCH",
        libc::SIGIO => "SIGIO",
        libc::SIGPWR => "SIGPWR",
        libc::SIGSYS => "SIGSYS",
        _ => {
            return Err(finish_line_unavailable(
                "finish-line-signal-unsupported",
                "finish-line observed a provider signal outside the DSH signal contract",
            ));
        }
    };
    Ok(name.to_string())
}

fn resolve_contracts(identity: &RequestIdentity) -> Result<ContractSnapshot, HookError> {
    let roots = ResolvedRoots::for_paths(identity.repo_root.clone(), identity.repo_root.clone());
    let mut contracts = validation_contracts_from_roots(&roots).map_err(|_| {
        HookError::data(
            "finish-line-contract-invalid",
            "current agent-docs validation contracts are invalid",
        )
    })?;
    contracts.sort_by(|left, right| left.context.as_str().cmp(right.context.as_str()));
    if contracts.len() > MAX_CONTRACTS {
        return Err(HookError::data(
            "finish-line-contract-limit",
            "current validation contract count exceeds the finish-line limit",
        ));
    }

    let mut targets = Vec::new();
    let mut contract_digests = Vec::new();
    let mut prior_markers = Vec::new();
    for contract in contracts {
        if !contract.declared {
            continue;
        }
        validate_intent(contract.context.as_str())?;
        if let Some(marker) = contract.marker.as_deref() {
            if marker.len() > 4096 || marker.contains('\0') {
                return Err(HookError::data(
                    "finish-line-contract-invalid",
                    "validation marker exceeds the finish-line boundary",
                ));
            }
            prior_markers.push(marker.to_string());
        }
        let mut parts = vec![contract.context.as_str().as_bytes()];
        if let Some(marker) = contract.marker.as_deref() {
            parts.push(marker.as_bytes());
        }
        for command in &contract.commands {
            validate_command(command)?;
            parts.push(command.as_bytes());
        }
        let contract_digest = digest_parts("agent-hook.finish-line.contract.v1", &parts);
        contract_digests.push(contract_digest.clone());
        for command in contract.commands {
            if targets.len() >= MAX_TARGETS {
                return Err(HookError::data(
                    "finish-line-contract-limit",
                    "current validation target count exceeds the finish-line limit",
                ));
            }
            let target_digest = digest_parts(
                "agent-hook.finish-line.target.v1",
                &[
                    identity.repo_digest.as_bytes(),
                    b"dsh",
                    contract.context.as_str().as_bytes(),
                    contract_digest.as_bytes(),
                    command.as_bytes(),
                ],
            );
            targets.push(ContractTarget {
                intent: contract.context.as_str().to_string(),
                command,
                contract_digest: contract_digest.clone(),
                target_digest,
            });
        }
    }
    let contract_digest_bytes = contract_digests
        .iter()
        .map(String::as_bytes)
        .collect::<Vec<_>>();
    let global_digest = digest_parts(
        "agent-hook.finish-line.contract-set.v1",
        &contract_digest_bytes,
    );
    Ok(ContractSnapshot {
        global_digest,
        targets,
        prior_markers,
    })
}

fn operation_key(session_key: &str, operation_id: &str) -> String {
    digest_parts(
        "agent-hook.finish-line.operation.v1",
        &[session_key.as_bytes(), operation_id.as_bytes()],
    )
}

fn runner_capability_digest(identity: &RequestIdentity, capability: &str) -> String {
    digest_parts(
        "agent-hook.finish-line.runner-capability.v1",
        &[
            identity.repo_digest.as_bytes(),
            identity.session_key.as_bytes(),
            capability.as_bytes(),
        ],
    )
}

fn released_session_key(session_key: &str, capability_digest: &str) -> String {
    digest_parts(
        "agent-hook.finish-line.released-session.v1",
        &[session_key.as_bytes(), capability_digest.as_bytes()],
    )
}

fn open_runner_capability(
    identity: &RequestIdentity,
    attempt_token: &str,
    incarnation: Option<u64>,
) -> String {
    let incarnation_bytes = incarnation.map(u64::to_le_bytes);
    let digest = match incarnation_bytes.as_ref() {
        Some(incarnation) => digest_parts(
            "agent-hook.finish-line.open-capability.v2",
            &[
                identity.session_key.as_bytes(),
                attempt_token.as_bytes(),
                incarnation,
            ],
        ),
        None => digest_parts(
            "agent-hook.finish-line.open-capability.v1",
            &[identity.session_key.as_bytes(), attempt_token.as_bytes()],
        ),
    };
    format!(
        "finish-line-runner:{}",
        digest.strip_prefix("sha256:").expect("digest prefix")
    )
}

fn epoch_seconds() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn capability_lease_expiry() -> Result<u64, HookError> {
    epoch_seconds()
        .and_then(|now| now.checked_add(CAPABILITY_LEASE_DURATION_SECS))
        .ok_or_else(|| {
            finish_line_unavailable(
                "finish-line-clock-unavailable",
                "finish-line capability lease clock is unavailable",
            )
        })
}

fn digest_parts(domain: &str, parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    let hash = hasher.finalize();
    let encoded = hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{encoded}")
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn ensure_state_capacity(state: &State, session_key: &str) -> Result<(), HookError> {
    if state.operations.len() >= MAX_OPERATIONS {
        return Err(HookError::data(
            "finish-line-state-limit",
            "finish-line operation limit is reached",
        ));
    }
    if state.sessions.len() >= MAX_SESSIONS && !state.sessions.contains_key(session_key) {
        return Err(HookError::data(
            "finish-line-state-limit",
            "finish-line session limit is reached",
        ));
    }
    Ok(())
}

fn compact_state(state: &mut State) {
    compact_obsolete_sessions(state);
    if state.operations.len() < COMPACTION_TRIGGER_OPERATIONS {
        return;
    }
    let remove_count = state
        .operations
        .len()
        .saturating_sub(COMPACTED_OPERATION_COUNT);
    let mut terminal = state
        .operations
        .iter()
        .filter_map(|(key, operation)| {
            operation
                .terminal
                .as_ref()
                .map(|_| (operation.sequence, key.clone()))
        })
        .collect::<Vec<_>>();
    terminal.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for (_, key) in terminal.into_iter().take(remove_count) {
        state.operations.remove(&key);
    }
}

fn compact_obsolete_sessions(state: &mut State) {
    let generation = state.generation;
    for session in state.sessions.values_mut() {
        session
            .targets
            .retain(|_, target| target.generation == generation);
    }

    state.sessions.retain(|_, session| {
        !session.targets.is_empty() || session.runner_capability_digest.is_some()
    });
}

#[cfg(target_os = "linux")]
fn reclaim_expired_crash_orphan_session(
    state_root: &Path,
    identity: &RequestIdentity,
) -> Result<bool, HookError> {
    let Some(now) = epoch_seconds() else {
        return Ok(false);
    };
    let candidates = {
        let mut store = Store::open(state_root, identity)?;
        let mut candidates = store
            .state
            .sessions
            .iter()
            .filter_map(|(session_key, session)| {
                let lease_expires_at_epoch = session.capability_lease_expires_at_epoch?;
                if lease_expires_at_epoch > now {
                    return None;
                }
                let capability_digest = session.runner_capability_digest.clone()?;
                let operations = store
                    .state
                    .operations
                    .iter()
                    .filter(|(_, operation)| {
                        operation.session_key == *session_key
                            && (operation.terminal.is_none() || operation.active_unit.is_some())
                    })
                    .map(|(operation_key, operation)| {
                        Some(ExpiredOrphanOperation {
                            operation_key: operation_key.clone(),
                            sequence: operation.sequence,
                            active_unit: operation.active_unit.clone()?,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                if operations.len() > MAX_EXPIRED_ORPHAN_OPERATIONS {
                    return None;
                }
                Some(ExpiredOrphanCandidate {
                    session_key: session_key.clone(),
                    capability_digest,
                    capability_incarnation: session.runner_capability_incarnation,
                    lease_expires_at_epoch,
                    operations,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.lease_expires_at_epoch
                .cmp(&right.lease_expires_at_epoch)
                .then_with(|| left.session_key.cmp(&right.session_key))
        });
        let (candidates, cursor) =
            expired_reclaim_window(candidates, store.state.expired_reclaim_cursor.as_deref());
        store.state.expired_reclaim_cursor = cursor;
        store.save()?;
        candidates
    };

    for candidate in candidates {
        let units = candidate
            .operations
            .iter()
            .map(|operation| operation.active_unit.clone())
            .collect::<BTreeSet<_>>();
        let passively_quiescent = units.iter().all(|unit| {
            contained_unit_is_quiescent(unit).unwrap_or(false)
                && !contained_unit_has_pending_job(unit).unwrap_or(true)
        });
        if !passively_quiescent {
            continue;
        }
        if units.iter().any(|unit| {
            stop_contained_unit(unit).is_err()
                || !contained_unit_is_quiescent(unit).unwrap_or(false)
                || contained_unit_has_pending_job(unit).unwrap_or(true)
        }) {
            continue;
        }

        let reclaimed = {
            let mut store = Store::open(state_root, identity)?;
            let session_matches = store
                .state
                .sessions
                .get(&candidate.session_key)
                .is_some_and(|session| {
                    session.capability_lease_expires_at_epoch
                        == Some(candidate.lease_expires_at_epoch)
                        && session.runner_capability_incarnation == candidate.capability_incarnation
                        && session
                            .runner_capability_digest
                            .as_deref()
                            .is_some_and(|digest| {
                                constant_time_eq(
                                    digest.as_bytes(),
                                    candidate.capability_digest.as_bytes(),
                                )
                            })
                });
            let current_operations = store
                .state
                .operations
                .iter()
                .filter(|(_, operation)| {
                    operation.session_key == candidate.session_key
                        && (operation.terminal.is_none() || operation.active_unit.is_some())
                })
                .map(|(operation_key, operation)| {
                    Some(ExpiredOrphanOperation {
                        operation_key: operation_key.clone(),
                        sequence: operation.sequence,
                        active_unit: operation.active_unit.clone()?,
                    })
                })
                .collect::<Option<Vec<_>>>();
            let still_expired =
                epoch_seconds().is_some_and(|current| candidate.lease_expires_at_epoch <= current);
            if !session_matches
                || !still_expired
                || current_operations.as_ref() != Some(&candidate.operations)
            {
                false
            } else {
                store
                    .state
                    .operations
                    .retain(|_, operation| operation.session_key != candidate.session_key);
                store.state.sessions.remove(&candidate.session_key);
                let sequence = store.state.next_sequence()?;
                let released_key =
                    released_session_key(&candidate.session_key, &candidate.capability_digest);
                store.state.released_sessions.insert(
                    released_key,
                    ReleasedSession {
                        capability_digest: candidate.capability_digest.clone(),
                        session_key: Some(candidate.session_key.clone()),
                        sequence,
                    },
                );
                compact_release_tombstones(&mut store.state);
                store.save()?;
                true
            }
        };
        if reclaimed {
            for unit in units {
                let _ = reset_contained_unit(&unit);
            }
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn expired_reclaim_window(
    candidates: Vec<ExpiredOrphanCandidate>,
    cursor: Option<&str>,
) -> (Vec<ExpiredOrphanCandidate>, Option<String>) {
    if candidates.is_empty() {
        return (Vec::new(), None);
    }
    let start = cursor
        .and_then(|cursor| {
            candidates
                .iter()
                .position(|candidate| candidate.session_key == cursor)
        })
        .map_or(0, |index| (index + 1) % candidates.len());
    let take = candidates.len().min(MAX_EXPIRED_ORPHAN_CANDIDATES);
    let selected = (0..take)
        .map(|offset| candidates[(start + offset) % candidates.len()].clone())
        .collect::<Vec<_>>();
    let cursor = selected
        .last()
        .map(|candidate| candidate.session_key.clone());
    (selected, cursor)
}

#[cfg(not(target_os = "linux"))]
fn reclaim_expired_crash_orphan_session(
    _state_root: &Path,
    _identity: &RequestIdentity,
) -> Result<bool, HookError> {
    Ok(false)
}

fn compact_release_tombstones(state: &mut State) {
    if state.released_sessions.len() <= MAX_RELEASE_TOMBSTONES {
        return;
    }
    let remove_count = state.released_sessions.len() - MAX_RELEASE_TOMBSTONES;
    let mut oldest = state
        .released_sessions
        .iter()
        .map(|(key, released)| (released.sequence, key.clone()))
        .collect::<Vec<_>>();
    oldest.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    for (_, key) in oldest.into_iter().take(remove_count) {
        state.released_sessions.remove(&key);
    }
}

fn classify_prior_marker(repo_root: &Path, marker: &str) -> &'static str {
    let marker_path = Path::new(marker);
    if marker_path.is_absolute()
        || marker_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return "prior-validation-marker-unsafe";
    }
    match fs::symlink_metadata(repo_root.join(marker_path)) {
        Ok(metadata)
            if !metadata.file_type().is_symlink()
                && metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.permissions().mode() & 0o022 == 0 =>
        {
            "prior-validation-unresolved"
        }
        Ok(_) => "prior-validation-marker-unsafe",
        Err(error) if error.kind() == io::ErrorKind::NotFound => "prior-validation-unresolved",
        Err(_) => "prior-validation-marker-unsafe",
    }
}

fn remediations(reasons: &[&str]) -> Vec<&'static str> {
    let mut remediation = Vec::new();
    for reason in reasons {
        let value = match *reason {
            "validation-pending" => {
                "wait for the authoritative validation outcome or retry the exact command"
            }
            "validation-failed" => "fix the failure and rerun the exact current validation command",
            "validation-stale" | "validation-contract-drift" => {
                "rerun every exact command from the current agent-docs validation contracts"
            }
            "prior-validation-marker-unsafe" => {
                "remove or repair the unsafe prior marker and record native validation evidence"
            }
            "prior-validation-unresolved" => {
                "record native validation evidence; prior markers are not generation authority"
            }
            _ => "run every missing exact current validation command through DSH",
        };
        if !remediation.contains(&value) {
            remediation.push(value);
        }
    }
    remediation.truncate(8);
    remediation
}

fn success_outcome(data: Value, text: &str) -> Outcome {
    Outcome {
        data,
        text: text.to_string(),
        exit_code: 0,
    }
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

fn finish_line_unavailable(code: &str, message: &str) -> HookError {
    HookError::unavailable_with(
        code,
        message,
        recoverable_details("verify the local resource and retry the exact request once"),
    )
}

fn finish_line_temporary(code: &str, message: &str) -> HookError {
    HookError::temporary_with(
        code,
        message,
        recoverable_details(
            "wait for the bounded contention window and retry the exact request once",
        ),
    )
}

struct Store {
    state_path: PathBuf,
    state: State,
    _lock: RepoLock,
}

impl Store {
    fn open(state_root: &Path, identity: &RequestIdentity) -> Result<Self, HookError> {
        ensure_private_state_root(state_root)?;
        let finish_line_root = state_root.join("finish-line");
        ensure_private_dir(&finish_line_root)?;
        let repos = finish_line_root.join("repos");
        ensure_private_dir(&repos)?;
        let lock_path = repos.join(format!("{}.lock", identity.repo_key));
        let (lock, lock_created) = RepoLock::acquire(&lock_path)?;
        let state_path = repos.join(format!("{}.json", identity.repo_key));
        let state_missing = match fs::symlink_metadata(&state_path) {
            Ok(_) => false,
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(_) => {
                return Err(finish_line_unavailable(
                    "finish-line-state-unavailable",
                    "finish-line repository state metadata is unavailable",
                ));
            }
        };
        if state_missing && !lock_created {
            return Err(HookError::data(
                "finish-line-state-missing",
                "initialized finish-line repository state is missing",
            ));
        }
        let state = read_state(&state_path, &identity.repo_digest)?;
        let store = Self {
            state_path,
            state,
            _lock: lock,
        };
        if state_missing {
            store.save()?;
        }
        Ok(store)
    }

    fn save(&self) -> Result<(), HookError> {
        let bytes = serde_json::to_vec(&self.state).map_err(|_| {
            HookError::runtime(
                "finish-line-state-serialize-failed",
                "finish-line state could not be serialized",
            )
        })?;
        if bytes.len() as u64 > STATE_MAX_BYTES {
            return Err(HookError::data(
                "finish-line-state-limit",
                "finish-line state exceeds 384 KiB",
            ));
        }
        write_state_atomic(&self.state_path, &bytes)
    }
}

fn ensure_private_state_root(path: &Path) -> Result<(), HookError> {
    for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(HookError::data(
                    "finish-line-state-untrusted",
                    "finish-line state path contains a symlink",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(finish_line_unavailable(
                    "finish-line-state-unavailable",
                    "finish-line state ancestor metadata is unavailable",
                ));
            }
        }
    }
    let missing = fs::symlink_metadata(path).is_err();
    if missing {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        match builder.create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err(finish_line_unavailable(
                    "finish-line-state-unavailable",
                    "finish-line state root could not be created",
                ));
            }
        }
    }
    crate::paths::ensure_private_state_dir(path, "finish-line-state-dir")?;
    if missing && let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

struct RepoLock(File);

impl RepoLock {
    fn acquire(path: &Path) -> Result<(Self, bool), HookError> {
        let created;
        let file = match OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(PRIVATE_MODE)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => {
                created = true;
                file
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                created = false;
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                    .open(path)
                    .map_err(|_| {
                        finish_line_unavailable(
                            "finish-line-lock-unavailable",
                            "finish-line repository lock is unavailable",
                        )
                    })?
            }
            Err(_) => {
                return Err(finish_line_unavailable(
                    "finish-line-lock-unavailable",
                    "finish-line repository lock is unavailable",
                ));
            }
        };
        verify_private_regular(&file, "finish-line-lock-untrusted")?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
        let started = Instant::now();
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok((Self(file), created));
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::WouldBlock {
                return Err(finish_line_unavailable(
                    "finish-line-lock-unavailable",
                    "finish-line repository lock could not be acquired",
                ));
            }
            if started.elapsed() >= LOCK_TIMEOUT {
                return Err(finish_line_temporary(
                    "finish-line-lock-busy",
                    "finish-line repository state is busy; retry is bounded",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn ensure_private_dir(path: &Path) -> Result<(), HookError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => verify_private_dir_metadata(&metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let metadata = fs::symlink_metadata(path).map_err(|_| {
                        finish_line_unavailable(
                            "finish-line-state-unavailable",
                            "finish-line concurrently created state directory could not be verified",
                        )
                    })?;
                    return verify_private_dir_metadata(&metadata);
                }
                Err(_) => {
                    return Err(finish_line_unavailable(
                        "finish-line-state-unavailable",
                        "finish-line state directory could not be created",
                    ));
                }
            }
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
            let metadata = fs::symlink_metadata(path).map_err(|_| {
                finish_line_unavailable(
                    "finish-line-state-unavailable",
                    "finish-line state directory could not be verified",
                )
            })?;
            verify_private_dir_metadata(&metadata)
        }
        Err(_) => Err(finish_line_unavailable(
            "finish-line-state-unavailable",
            "finish-line state directory metadata is unavailable",
        )),
    }
}

fn verify_private_dir_metadata(metadata: &fs::Metadata) -> Result<(), HookError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(HookError::data(
            "finish-line-state-untrusted",
            "finish-line state directory owner, mode, or type is untrusted",
        ));
    }
    Ok(())
}

fn verify_private_regular(file: &File, code: &str) -> Result<(), HookError> {
    let metadata = file.metadata().map_err(|_| {
        finish_line_unavailable(
            "finish-line-state-unavailable",
            "finish-line state metadata is unavailable",
        )
    })?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(HookError::data(
            code,
            "finish-line state file owner, mode, or type is untrusted",
        ));
    }
    Ok(())
}

fn read_state(path: &Path, repo_digest: &str) -> Result<State, HookError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(State::new(repo_digest));
        }
        Err(_) => {
            return Err(HookError::data(
                "finish-line-state-untrusted",
                "finish-line state could not be opened without following links",
            ));
        }
    };
    verify_private_regular(&file, "finish-line-state-untrusted")?;
    let mut bytes = Vec::new();
    file.take(STATE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            finish_line_unavailable(
                "finish-line-state-unavailable",
                "finish-line state could not be read",
            )
        })?;
    if bytes.len() as u64 > STATE_MAX_BYTES {
        return Err(HookError::data(
            "finish-line-state-invalid",
            "finish-line state exceeds 384 KiB",
        ));
    }
    let state: State = serde_json::from_slice(&bytes).map_err(|_| {
        HookError::data(
            "finish-line-state-invalid",
            "finish-line state does not match its strict schema",
        )
    })?;
    if state.schema_version != STATE_SCHEMA || state.repo_digest != repo_digest {
        return Err(HookError::data(
            "finish-line-state-invalid",
            "finish-line state schema or repository binding is invalid",
        ));
    }
    Ok(state)
}

fn write_state_atomic(path: &Path, bytes: &[u8]) -> Result<(), HookError> {
    if let Ok(existing) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
    {
        verify_private_regular(&existing, "finish-line-state-untrusted")?;
    } else if fs::symlink_metadata(path).is_ok() {
        return Err(HookError::data(
            "finish-line-state-untrusted",
            "finish-line state destination is untrusted",
        ));
    }

    let parent = path.parent().ok_or_else(|| {
        HookError::data(
            "finish-line-state-invalid",
            "finish-line state path has no parent",
        )
    })?;
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        Uuid::new_v4()
    ));
    let mut temp = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(PRIVATE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temp_path)
        .map_err(|_| {
            finish_line_unavailable(
                "finish-line-state-write-failed",
                "finish-line temporary state could not be created",
            )
        })?;
    let write_result = temp.write_all(bytes).and_then(|_| temp.sync_all());
    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
        return Err(finish_line_unavailable(
            "finish-line-state-write-failed",
            "finish-line temporary state could not be synced",
        ));
    }
    drop(temp);
    fs::rename(&temp_path, path).map_err(|_| {
        let _ = fs::remove_file(&temp_path);
        finish_line_unavailable(
            "finish-line-state-write-failed",
            "finish-line state could not be atomically replaced",
        )
    })?;
    sync_directory(parent).map_err(|_| {
        finish_line_unavailable(
            "finish-line-state-write-failed",
            "finish-line parent directory could not be durably synced",
        )
    })
}

fn sync_directory(path: &Path) -> Result<(), HookError> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| {
            finish_line_unavailable(
                "finish-line-state-unavailable",
                "finish-line state directory could not be opened for sync",
            )
        })?;
    directory.sync_all().map_err(|_| {
        finish_line_unavailable(
            "finish-line-state-unavailable",
            "finish-line state directory sync failed",
        )
    })
}

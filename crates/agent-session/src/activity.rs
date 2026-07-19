use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use jiff::Zoned;
use nils_common::fs::{SECRET_FILE_MODE, display_path, home_dir, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use toml_edit::{
    Array as TomlArray, DocumentMut as TomlDocument, Item as TomlItem, Value as TomlValue,
    value as toml_value,
};

use crate::cli::AgentKind;
use crate::{
    CliContext, CliError, ProviderResume, SessionRecord, canonical_provider_resume_args,
    load_session_record, mutate_session_record, session_dir,
};

pub(crate) const TURN_EVENT_VERSION: &str = "agent-session.turn-event.v1";
pub(crate) const TURN_STATE_VERSION: &str = "agent-session.turn-state.v1";
const ACTIVITY_DOCUMENT_VERSION: &str = "agent-session.activity.v1";
const ACTIVITY_FILE: &str = "activity.json";
const ACTIVITY_JOURNAL_FILE: &str = "activity.journal.jsonl";
const ACTIVITY_REPLAY_FILE: &str = "activity.replay.bin";
const ACTIVITY_DIAGNOSTIC_FILE: &str = "activity.diagnostic.json";
const ACTIVITY_LOCK_FILE: &str = ".activity.lock";
const ACTIVITY_HEALTH_LOCK_FILE: &str = ".activity-health.lock";
const ACTIVITY_UNHEALTHY_FILE: &str = "activity.unhealthy.json";
const MAX_EVENT_BYTES: u64 = 64 * 1024;
const MAX_JOURNAL_EVENTS: usize = 256;
const MAX_JOURNAL_BYTES: usize = 64 * 1024;
const MAX_DEDUPE_EVENTS: usize = 4096;
const REPLAY_SLOT_COUNT: usize = MAX_DEDUPE_EVENTS * 2;
const REPLAY_SLOT_BYTES: usize = 32;
const REPLAY_HEADER_BYTES: usize = 64;
const REPLAY_MAGIC: &[u8; 16] = b"agent-session-r1";
const MAX_PENDING_ATTENTION: usize = 64;
const MAX_ID_CHARS: usize = 256;
const CODEX_NOTIFY_ARGV: [&str; 5] = ["agent-session", "activity", "notify", "--agent", "codex"];
const CODEX_NOTIFY_FORWARD_FLAG: &str = "--forward-notify-argv-json";
const CODEX_NOTIFY_FORWARD_ACTIVE_ENV: &str = "AGENT_SESSION_CODEX_NOTIFY_FANOUT_ACTIVE";
pub(crate) const ACTIVITY_RETRY_PROVIDER_ENV: &str = "AGENT_SESSION_ACTIVITY_RETRY_PROVIDER";
const MAX_CODEX_FORWARD_ARGS: usize = 64;
const MAX_CODEX_FORWARD_ARGV_BYTES: usize = 16 * 1024;
const CODEX_FORWARD_TIMEOUT: Duration = Duration::from_secs(2);
const CODEX_COMPLETION_RETRY_TIMEOUT: Duration = Duration::from_secs(5);
const RUNTIME_UNHEALTHY_LOCK_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnPhase {
    Starting,
    Working,
    Waiting,
    NeedsInput,
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Confidence {
    Authoritative,
    #[default]
    Observed,
    Inferred,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceKind {
    #[default]
    ProviderHook,
    ConsoleObservation,
    TerminalHeuristic,
    Runtime,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct TurnSource {
    pub(crate) kind: SourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider: Option<String>,
    pub(crate) confidence: Confidence,
    #[serde(default, flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct AttentionView {
    pub(crate) kind: String,
    pub(crate) requested_at: String,
    pub(crate) pending_count: usize,
    #[serde(default, flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CurrentTurn {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider_turn_id: Option<String>,
    pub(crate) started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_progress_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) attention: Option<AttentionView>,
    #[serde(default, flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LastTurn {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider_turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) started_at: Option<String>,
    pub(crate) completed_at: String,
    pub(crate) outcome: String,
    #[serde(default, flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct TurnState {
    pub(crate) schema_version: String,
    pub(crate) phase: TurnPhase,
    pub(crate) phase_changed_at: String,
    pub(crate) revision: u64,
    pub(crate) source: TurnSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) current_turn: Option<CurrentTurn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_turn: Option<LastTurn>,
    #[serde(default, flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StreamTurnSource {
    pub(crate) kind: SourceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider: Option<String>,
    pub(crate) confidence: Confidence,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StreamAttentionView {
    pub(crate) kind: String,
    pub(crate) requested_at: String,
    pub(crate) pending_count: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StreamCurrentTurn {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider_turn_id: Option<String>,
    pub(crate) started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_progress_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) attention: Option<StreamAttentionView>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StreamLastTurn {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider_turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) started_at: Option<String>,
    pub(crate) completed_at: String,
    pub(crate) outcome: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StreamTurnState {
    pub(crate) schema_version: String,
    pub(crate) phase: TurnPhase,
    pub(crate) phase_changed_at: String,
    pub(crate) revision: u64,
    pub(crate) source: StreamTurnSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) current_turn: Option<StreamCurrentTurn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_turn: Option<StreamLastTurn>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct PendingAttention {
    id: String,
    kind: String,
    requested_at: String,
    #[serde(default, flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct OverflowAttention {
    kind: String,
    requested_at: String,
    count: usize,
    #[serde(default, flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ActivityDocument {
    schema_version: String,
    runtime_id: String,
    #[serde(default)]
    runtime_generation: u64,
    state: TurnState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_attention: Vec<PendingAttention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    overflow_attention: Option<OverflowAttention>,
    #[serde(default)]
    seen_event_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_semantic_event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_semantic_event_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_event_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_journal: Option<JournalEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_unhealthy_reason: Option<String>,
    #[serde(default, flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RuntimeUnhealthyMarker {
    schema_version: String,
    runtime_id: String,
    runtime_generation: u64,
    reason: String,
    marked_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<TurnState>,
}

enum RuntimeUnhealthyStatus {
    Absent,
    Matching(Box<TurnState>),
    Pending(String),
    Invalid,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnEventKind {
    TurnStarted,
    AttentionRequested,
    AttentionCleared,
    Progress,
    StopObserved,
    TurnCompleted,
    TurnFailed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TurnEvent {
    pub(crate) schema_version: String,
    pub(crate) event_id: String,
    pub(crate) runtime_id: String,
    pub(crate) provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider_turn_id: Option<String>,
    pub(crate) kind: TurnEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) attention_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) attention_kind: Option<String>,
    #[serde(skip)]
    attention_correlation_ambiguous: bool,
    pub(crate) confidence: Confidence,
    #[serde(default)]
    pub(crate) source_kind: SourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider_time: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ActivityResult {
    pub(crate) id: String,
    pub(crate) turn_state: TurnState,
    pub(crate) duplicate: bool,
}

pub(crate) fn ingest_codex_app_server_failure(
    context: &CliContext,
    id: &str,
    runtime_id: &str,
    thread_id: &str,
    turn_id: &str,
) -> Result<ActivityResult, CliError> {
    let provider_session_id =
        projected_provider_identifier(runtime_id, AgentKind::Codex, "session", thread_id)?;
    let provider_turn_id =
        projected_provider_identifier(runtime_id, AgentKind::Codex, "turn", turn_id)?;
    ingest_event_retry(
        context,
        id,
        TurnEvent {
            schema_version: TURN_EVENT_VERSION.to_string(),
            event_id: format!("codex-app-server-failure:{provider_turn_id}"),
            runtime_id: runtime_id.to_string(),
            provider: AgentKind::Codex.as_str().to_string(),
            provider_session_id: Some(provider_session_id),
            provider_turn_id: Some(provider_turn_id),
            kind: TurnEventKind::TurnFailed,
            failure_reason: Some("usage_exhausted".to_string()),
            attention_id: None,
            attention_kind: None,
            attention_correlation_ambiguous: false,
            confidence: Confidence::Authoritative,
            // `provider_hook` is the stable v1 wire value for authoritative,
            // provider-structured evidence, including the app-server protocol.
            source_kind: SourceKind::ProviderHook,
            provider_time: None,
        },
    )
}

pub(crate) fn ingest_codex_app_server_attention(
    context: &CliContext,
    id: &str,
    runtime_id: &str,
    thread_id: &str,
    turn_id: Option<&str>,
    request_id: &str,
    requested_kind: Option<&str>,
) -> Result<ActivityResult, CliError> {
    let record = load_session_record(context, id)?;
    if crate::codex_app_server::attention_authority(&record) != "protocol" {
        return Err(CliError::data(
            "codex-attention-authority-mismatch",
            "Codex protocol attention evidence is not admitted for this runtime",
            Some(json!({ "id": record.id })),
        ));
    }
    let provider_session_id =
        projected_provider_identifier(runtime_id, AgentKind::Codex, "session", thread_id)?;
    let provider_turn_id = turn_id
        .map(|turn_id| projected_provider_identifier(runtime_id, AgentKind::Codex, "turn", turn_id))
        .transpose()?;
    let attention_id =
        projected_provider_identifier(runtime_id, AgentKind::Codex, "attention", request_id)?;
    let (kind, attention_kind, event_label) = match requested_kind {
        Some(kind @ ("approval" | "clarification" | "authentication" | "other")) => (
            TurnEventKind::AttentionRequested,
            Some(kind.to_string()),
            "requested",
        ),
        Some(_) => {
            return Err(CliError::data(
                "codex-attention-kind-invalid",
                "Codex protocol attention kind is outside the v1 allowlist",
                None,
            ));
        }
        None => (TurnEventKind::AttentionCleared, None, "resolved"),
    };
    ingest_event_retry(
        context,
        id,
        TurnEvent {
            schema_version: TURN_EVENT_VERSION.to_string(),
            event_id: format!("codex-app-server-attention:{event_label}:{attention_id}"),
            runtime_id: runtime_id.to_string(),
            provider: AgentKind::Codex.as_str().to_string(),
            provider_session_id: Some(provider_session_id),
            provider_turn_id,
            kind,
            failure_reason: None,
            attention_id: Some(attention_id),
            attention_kind,
            attention_correlation_ambiguous: false,
            confidence: Confidence::Authoritative,
            // `provider_hook` is the stable v1 wire value for all structured
            // provider evidence, including the app-server protocol.
            source_kind: SourceKind::ProviderHook,
            provider_time: None,
        },
    )
}

/// Project durable activity state onto the explicitly allowlisted stream
/// contract. Durable snapshots preserve additive fields for forward
/// compatibility; those unknown fields must not cross the daemon stream's
/// metadata-only privacy boundary.
pub(crate) fn stream_projection(state: &TurnState) -> StreamTurnState {
    StreamTurnState {
        schema_version: state.schema_version.clone(),
        phase: state.phase.clone(),
        phase_changed_at: state.phase_changed_at.clone(),
        revision: state.revision,
        source: StreamTurnSource {
            kind: state.source.kind.clone(),
            provider: state.source.provider.clone(),
            confidence: state.source.confidence.clone(),
        },
        current_turn: state.current_turn.as_ref().map(|turn| StreamCurrentTurn {
            provider_turn_id: turn.provider_turn_id.clone(),
            started_at: turn.started_at.clone(),
            last_progress_at: turn.last_progress_at.clone(),
            attention: turn
                .attention
                .as_ref()
                .map(|attention| StreamAttentionView {
                    kind: attention.kind.clone(),
                    requested_at: attention.requested_at.clone(),
                    pending_count: attention.pending_count,
                }),
        }),
        last_turn: state.last_turn.as_ref().map(|turn| StreamLastTurn {
            provider_turn_id: turn.provider_turn_id.clone(),
            started_at: turn.started_at.clone(),
            completed_at: turn.completed_at.clone(),
            outcome: turn.outcome.clone(),
        }),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetupAction {
    DryRun,
    RepairPreview,
    Apply,
    Remove,
    Repair,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ProviderDoctor {
    pub(crate) provider: String,
    pub(crate) classification: String,
    pub(crate) version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) version_error: Option<String>,
    pub(crate) configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) configuration_error: Option<String>,
    pub(crate) config_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hook_representation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hook_migration_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) representation_conflict: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) notification_config_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) notification_mode: Option<String>,
    pub(crate) completion: String,
    pub(crate) attention_correlation: String,
    pub(crate) exact_attention: String,
    pub(crate) attention_authority: String,
    pub(crate) trust: String,
    pub(crate) guidance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_event_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_error: Option<String>,
    pub(crate) helper_executable: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DoctorResult {
    pub(crate) providers: Vec<ProviderDoctor>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SetupResult {
    pub(crate) provider: String,
    pub(crate) action: String,
    pub(crate) changed: bool,
    pub(crate) would_change: bool,
    pub(crate) configured: bool,
    pub(crate) would_configure: bool,
    pub(crate) apply_allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) preview_digest: Option<String>,
    pub(crate) config_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hook_representation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hook_migration: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) representation_conflict: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) notification_config_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) notification_preview: Option<CodexNotificationPreview>,
    pub(crate) owned_events: Vec<String>,
    pub(crate) trust: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CodexNotificationPreview {
    current_mode: String,
    candidate_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) forwarded_argc: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) forwarded_argv_sha256: Option<String>,
    reversible: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) blocker_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ActivityDiagnostic {
    schema_version: String,
    provider: String,
    runtime_id: String,
    runtime_generation: u64,
    code: String,
    observed_at: String,
}

#[derive(Clone, Debug)]
struct VersionProbe {
    version: Option<String>,
    error: Option<String>,
}

pub(crate) struct ActivitySnapshot {
    document: Option<Vec<u8>>,
    replay: Option<Vec<u8>>,
    unhealthy: Option<Vec<u8>>,
}

#[derive(Debug)]
struct ActivityLock(fs::File);

impl Drop for ActivityLock {
    fn drop(&mut self) {
        // SAFETY: flock only observes the valid file descriptor owned by self.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[derive(Debug)]
pub(crate) struct RuntimeHealthFence(fs::File);

impl Drop for RuntimeHealthFence {
    fn drop(&mut self) {
        // SAFETY: flock only observes the valid descriptor owned by self.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn acquire_health_fence(dir: &Path) -> Result<RuntimeHealthFence, CliError> {
    let path = dir.join(ACTIVITY_HEALTH_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(SECRET_FILE_MODE)
        .open(&path)
        .map_err(|err| activity_io_error("activity-health-lock-open-failed", &path, err))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(SECRET_FILE_MODE))
        .map_err(|err| activity_io_error("activity-health-lock-permission-failed", &path, err))?;
    // SAFETY: flock only observes the valid descriptor owned by file.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(activity_io_error(
            "activity-health-lock-failed",
            &path,
            io::Error::last_os_error(),
        ));
    }
    Ok(RuntimeHealthFence(file))
}

pub(crate) fn acquire_runtime_health_fence(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<RuntimeHealthFence, CliError> {
    acquire_health_fence(&session_dir(context, &record.id))
}

fn acquire_lock(dir: &Path) -> Result<ActivityLock, CliError> {
    acquire_lock_with_mode(dir, ActivityLockMode::Blocking)
}

fn acquire_lock_nonblocking(dir: &Path) -> Result<ActivityLock, CliError> {
    acquire_lock_with_mode(dir, ActivityLockMode::NonBlocking)
}

fn acquire_lock_with_timeout(dir: &Path, timeout: Duration) -> Result<ActivityLock, CliError> {
    acquire_lock_with_mode(dir, ActivityLockMode::Timed(timeout))
}

#[derive(Clone, Copy, Debug)]
enum ActivityLockMode {
    Blocking,
    NonBlocking,
    Timed(Duration),
}

fn acquire_lock_with_mode(dir: &Path, mode: ActivityLockMode) -> Result<ActivityLock, CliError> {
    let path = dir.join(ACTIVITY_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(SECRET_FILE_MODE)
        .open(&path)
        .map_err(|err| activity_io_error("activity-lock-open-failed", &path, err))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(SECRET_FILE_MODE))
        .map_err(|err| activity_io_error("activity-lock-permission-failed", &path, err))?;
    // SAFETY: flock only observes the valid file descriptor owned by file.
    if matches!(mode, ActivityLockMode::Blocking) {
        // SAFETY: flock only observes the valid file descriptor owned by file.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(activity_io_error(
                "activity-lock-failed",
                &path,
                io::Error::last_os_error(),
            ));
        }
        return Ok(ActivityLock(file));
    }

    let deadline = match mode {
        ActivityLockMode::Timed(timeout) => Some(Instant::now() + timeout),
        ActivityLockMode::NonBlocking => None,
        ActivityLockMode::Blocking => unreachable!("blocking mode returned above"),
    };
    loop {
        // SAFETY: flock only observes the valid file descriptor owned by file.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(ActivityLock(file));
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::WouldBlock {
            return Err(activity_io_error("activity-lock-failed", &path, err));
        }
        let Some(deadline) = deadline else {
            return Err(activity_io_error("activity-lock-busy", &path, err));
        };
        if Instant::now() >= deadline {
            return Err(activity_io_error("activity-lock-timeout", &path, err));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn activity_io_error(code: &str, path: &Path, err: io::Error) -> CliError {
    CliError::runtime(
        code,
        format!("activity storage failed at {}: {err}", path.display()),
        Some(json!({ "path": display_path(path) })),
    )
}

fn runtime_unhealthy_marker_path(dir: &Path) -> PathBuf {
    dir.join(ACTIVITY_UNHEALTHY_FILE)
}

fn write_runtime_unhealthy_marker(
    dir: &Path,
    runtime_id: &str,
    runtime_generation: u64,
    reason: &str,
    state: &TurnState,
) -> Result<(), CliError> {
    write_runtime_unhealthy_marker_value(
        dir,
        runtime_id,
        runtime_generation,
        reason,
        &state.phase_changed_at,
        Some(state),
    )
}

fn write_runtime_unhealthy_pending_marker(
    dir: &Path,
    runtime_id: &str,
    runtime_generation: u64,
    reason: &str,
) -> Result<(), CliError> {
    write_runtime_unhealthy_marker_value(dir, runtime_id, runtime_generation, reason, &now(), None)
}

fn write_runtime_unhealthy_marker_value(
    dir: &Path,
    runtime_id: &str,
    runtime_generation: u64,
    reason: &str,
    marked_at: &str,
    state: Option<&TurnState>,
) -> Result<(), CliError> {
    let marker = RuntimeUnhealthyMarker {
        schema_version: "agent-session.activity-unhealthy.v1".to_string(),
        runtime_id: runtime_id.to_string(),
        runtime_generation,
        reason: reason.to_string(),
        marked_at: marked_at.to_string(),
        state: state.cloned(),
    };
    let bytes = serde_json::to_vec_pretty(&marker).map_err(|err| {
        CliError::runtime(
            "activity-unhealthy-render-failed",
            format!("failed to render the runtime activity health marker: {err}"),
            None,
        )
    })?;
    let path = runtime_unhealthy_marker_path(dir);
    write_atomic(&path, &bytes, SECRET_FILE_MODE).map_err(|err| {
        CliError::runtime(
            "activity-unhealthy-write-failed",
            format!("failed to persist the runtime activity health marker: {err}"),
            Some(json!({ "path": display_path(&path) })),
        )
    })
}

fn runtime_unhealthy_marker(
    dir: &Path,
    runtime_id: &str,
    runtime_generation: u64,
) -> RuntimeUnhealthyStatus {
    let bytes = match fs::read(runtime_unhealthy_marker_path(dir)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return RuntimeUnhealthyStatus::Absent;
        }
        Err(_) => return RuntimeUnhealthyStatus::Invalid,
    };
    let Ok(marker) = serde_json::from_slice::<RuntimeUnhealthyMarker>(&bytes) else {
        return RuntimeUnhealthyStatus::Invalid;
    };
    if marker.schema_version != "agent-session.activity-unhealthy.v1"
        || marker.marked_at.trim().is_empty()
        || marker.marked_at.parse::<jiff::Timestamp>().is_err()
        || marker.reason.trim().is_empty()
    {
        return RuntimeUnhealthyStatus::Invalid;
    }
    if marker.runtime_id != runtime_id || marker.runtime_generation != runtime_generation {
        return RuntimeUnhealthyStatus::Absent;
    }
    match marker.state {
        Some(state) if runtime_unhealthy_state_is_valid(&state) => {
            RuntimeUnhealthyStatus::Matching(Box::new(state))
        }
        Some(_) => RuntimeUnhealthyStatus::Invalid,
        None => RuntimeUnhealthyStatus::Pending(marker.marked_at),
    }
}

fn runtime_unhealthy_state_is_valid(state: &TurnState) -> bool {
    state.schema_version == TURN_STATE_VERSION
        && state.phase == TurnPhase::Unknown
        && state.phase_changed_at.parse::<jiff::Timestamp>().is_ok()
        && state.source.kind == SourceKind::Runtime
        && state.source.provider.is_none()
        && state.source.confidence == Confidence::Authoritative
        && state
            .current_turn
            .as_ref()
            .is_none_or(|turn| turn.attention.is_none())
}

fn degraded_state(mut state: TurnState, at: &str) -> TurnState {
    if let Some(current) = state.current_turn.as_mut() {
        current.attention = None;
    }
    if state.phase != TurnPhase::Unknown {
        state.phase_changed_at = at.to_string();
    }
    state.phase = TurnPhase::Unknown;
    state.revision = state.revision.saturating_add(1);
    state.source = runtime_source();
    state
}

fn marker_state_from_snapshot(
    dir: &Path,
    record: &SessionRecord,
    runtime_id: &str,
    runtime_generation: u64,
    at: &str,
) -> TurnState {
    read_document(&dir.join(ACTIVITY_FILE))
        .ok()
        .filter(|document| {
            document.runtime_id == runtime_id && document.runtime_generation == runtime_generation
        })
        .map(|document| {
            if document.runtime_unhealthy_reason.is_some()
                && document.state.phase == TurnPhase::Unknown
            {
                document.state
            } else {
                degraded_state(document.state, at)
            }
        })
        .unwrap_or_else(|| {
            let mut state = unknown_state(record);
            state.phase_changed_at = at.to_string();
            state
        })
}

fn remove_runtime_unhealthy_marker(dir: &Path) -> Result<(), CliError> {
    let path = runtime_unhealthy_marker_path(dir);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(activity_io_error(
            "activity-unhealthy-remove-failed",
            &path,
            err,
        )),
    }
}

fn now() -> String {
    Zoned::now().timestamp().to_string()
}

fn runtime_source() -> TurnSource {
    TurnSource {
        kind: SourceKind::Runtime,
        provider: None,
        confidence: Confidence::Authoritative,
        extra: Map::new(),
    }
}

fn provider_source(event: &TurnEvent) -> TurnSource {
    TurnSource {
        kind: event.source_kind.clone(),
        provider: Some(event.provider.clone()),
        confidence: event.confidence.clone(),
        extra: Map::new(),
    }
}

fn projected_provider_identifier(
    runtime_id: &str,
    agent: AgentKind,
    field: &str,
    value: &str,
) -> Result<String, CliError> {
    if value.is_empty()
        || value.chars().count() > MAX_ID_CHARS
        || value.chars().any(char::is_control)
    {
        return Err(CliError::data(
            "provider-hook-identifier-invalid",
            "provider hook identifier is empty, too long, or contains controls",
            Some(json!({ "field": field })),
        ));
    }
    let mut digest = Sha256::new();
    digest.update(b"agent-session.provider-identifier.v1\0");
    digest.update(runtime_id.as_bytes());
    digest.update(b"\0");
    digest.update(agent.as_str().as_bytes());
    digest.update(b"\0");
    digest.update(field.as_bytes());
    digest.update(b"\0");
    digest.update(value.as_bytes());
    Ok(format!("local:v1:{}", hex_digest(digest.finalize())))
}

fn hermes_approval_correlation_id(runtime_id: &str, raw: &Value) -> Result<String, CliError> {
    fn required_string<'a>(raw: &'a Value, field: &str) -> Result<&'a str, CliError> {
        raw.get(field).and_then(Value::as_str).ok_or_else(|| {
            CliError::data(
                "provider-hook-correlation-missing",
                "recognized Hermes approval hook is missing matching correlation metadata",
                Some(json!({ "field": field })),
            )
        })
    }

    let mut pattern_keys = raw
        .get("pattern_keys")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CliError::data(
                "provider-hook-correlation-missing",
                "recognized Hermes approval hook is missing matching correlation metadata",
                Some(json!({ "field": "pattern_keys" })),
            )
        })?
        .iter()
        .map(|value| {
            value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                CliError::data(
                    "provider-hook-correlation-missing",
                    "recognized Hermes approval hook has invalid matching correlation metadata",
                    Some(json!({ "field": "pattern_keys" })),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    pattern_keys.sort();
    pattern_keys.dedup();

    let canonical = serde_json::to_vec(&json!([
        required_string(raw, "command")?,
        required_string(raw, "description")?,
        required_string(raw, "pattern_key")?,
        pattern_keys,
        required_string(raw, "session_key")?,
        required_string(raw, "surface")?,
    ]))
    .map_err(|_| {
        CliError::data(
            "provider-hook-correlation-invalid",
            "Hermes approval correlation metadata could not be canonicalized",
            None,
        )
    })?;
    let mut digest = Sha256::new();
    digest.update(b"agent-session.hermes-approval-correlation.v1\0");
    digest.update(canonical);
    projected_provider_identifier(
        runtime_id,
        AgentKind::Hermes,
        "attention",
        &format!("sha256:{}", hex_digest(digest.finalize())),
    )
}

fn hermes_approval_metadata(raw: &Value) -> Result<&Value, CliError> {
    match raw.get("extra") {
        Some(extra) if extra.is_object() => Ok(extra),
        Some(Value::Null) | None => Ok(raw),
        Some(_) => Err(CliError::data(
            "provider-hook-correlation-invalid",
            "recognized Hermes approval hook has invalid matching correlation metadata",
            Some(json!({ "field": "extra" })),
        )),
    }
}

fn optional_hook_string<'a>(raw: &'a Value, field: &str) -> Result<Option<&'a str>, CliError> {
    match raw.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(CliError::data(
            "provider-hook-identifier-invalid",
            "provider hook identifier is invalid",
            Some(json!({ "field": field })),
        )),
    }
}

fn hermes_approval_correlation(
    runtime_id: &str,
    metadata: &Value,
) -> Result<(String, bool), CliError> {
    match metadata.get("tool_call_id") {
        Some(Value::String(value)) if !value.is_empty() => {
            projected_provider_identifier(runtime_id, AgentKind::Hermes, "attention", value)
                .map(|id| (id, false))
        }
        None | Some(Value::Null) | Some(Value::String(_)) => {
            hermes_approval_correlation_id(runtime_id, metadata).map(|id| (id, true))
        }
        Some(_) => Err(CliError::data(
            "provider-hook-correlation-invalid",
            "recognized Hermes approval hook has invalid matching correlation metadata",
            Some(json!({ "field": "tool_call_id" })),
        )),
    }
}

fn event_dedupe_key(runtime_id: &str, event_id: &str) -> [u8; REPLAY_SLOT_BYTES] {
    let mut digest = Sha256::new();
    digest.update(b"agent-session.event-id.v1\0");
    digest.update(runtime_id.as_bytes());
    digest.update(b"\0");
    digest.update(event_id.as_bytes());
    let mut key: [u8; REPLAY_SLOT_BYTES] = digest.finalize().into();
    if key.iter().all(|byte| *byte == 0) {
        key[REPLAY_SLOT_BYTES - 1] = 1;
    }
    key
}

fn stable_hermes_approval_event_id(
    runtime_id: &str,
    kind: &TurnEventKind,
    projected_tool_call_id: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"agent-session.provider-replay.v1\0");
    digest.update(runtime_id.as_bytes());
    digest.update(b"\0");
    digest.update(match kind {
        TurnEventKind::AttentionRequested => b"attention_requested".as_slice(),
        TurnEventKind::AttentionCleared => b"attention_cleared".as_slice(),
        _ => unreachable!("Hermes approval event kind"),
    });
    digest.update(b"\0");
    digest.update(projected_tool_call_id.as_bytes());
    format!("local:v1:{}", hex_digest(digest.finalize()))
}

fn semantic_event_key(event: &TurnEvent) -> String {
    let mut digest = Sha256::new();
    digest.update(b"agent-session.semantic-event.v1\0");
    let exact_attention = event
        .attention_id
        .as_deref()
        .is_some_and(|id| id.starts_with("local:v1:"))
        && !event.attention_correlation_ambiguous;
    let correlated_attention_id = if exact_attention
        || (event.provider == AgentKind::Hermes.as_str()
            && event.attention_kind.as_deref() == Some("approval"))
        || event.kind == TurnEventKind::AttentionCleared
    {
        event.attention_id.as_deref().unwrap_or("")
    } else {
        ""
    };
    for value in [
        event.provider.as_str(),
        event.provider_session_id.as_deref().unwrap_or(""),
        event.provider_turn_id.as_deref().unwrap_or(""),
        match event.kind {
            TurnEventKind::TurnStarted => "turn_started",
            TurnEventKind::AttentionRequested => "attention_requested",
            TurnEventKind::AttentionCleared => "attention_cleared",
            TurnEventKind::Progress => "progress",
            TurnEventKind::StopObserved => "stop_observed",
            TurnEventKind::TurnCompleted => "turn_completed",
            TurnEventKind::TurnFailed => "turn_failed",
        },
        event.attention_kind.as_deref().unwrap_or(""),
        correlated_attention_id,
    ] {
        digest.update(value.as_bytes());
        digest.update(b"\0");
    }
    format!("sha256:{}", hex_digest(digest.finalize()))
}

fn semantic_event_is_duplicate(
    document: &ActivityDocument,
    event: &TurnEvent,
    key: &str,
    received_at: &str,
) -> bool {
    if !matches!(event.source_kind, SourceKind::ProviderHook) {
        return false;
    }
    if event.provider == AgentKind::Hermes.as_str()
        && event.kind == TurnEventKind::AttentionRequested
        && event.attention_kind.as_deref() == Some("approval")
        && event.attention_correlation_ambiguous
    {
        // Hermes provides no delivery id distinct from the derived request
        // tuple. Preserve every observed pre-request as conservative
        // multiplicity; the runtime-scoped event-id replay index still rejects
        // exact normalized event replays.
        return false;
    }
    if event.kind == TurnEventKind::TurnStarted
        && event.provider_turn_id.is_some()
        && document
            .state
            .current_turn
            .as_ref()
            .and_then(|turn| turn.provider_turn_id.as_ref())
            == event.provider_turn_id.as_ref()
    {
        return true;
    }
    if matches!(
        event.kind,
        TurnEventKind::TurnCompleted | TurnEventKind::TurnFailed
    ) && let Some(event_turn_id) = event.provider_turn_id.as_ref()
        && document
            .state
            .current_turn
            .as_ref()
            .and_then(|turn| turn.provider_turn_id.as_ref())
            != Some(event_turn_id)
        && document.state.last_turn.as_ref().is_some_and(|turn| {
            turn.provider_turn_id.as_ref() == Some(event_turn_id)
                && matches!(turn.outcome.as_str(), "completed" | "failed")
        })
    {
        return true;
    }
    if document.last_semantic_event.as_deref() != Some(key) {
        return false;
    }
    if matches!(
        event.kind,
        TurnEventKind::TurnCompleted | TurnEventKind::TurnFailed
    ) {
        return true;
    }
    let Some(previous) = document
        .last_semantic_event_at
        .as_deref()
        .and_then(|value| value.parse::<jiff::Timestamp>().ok())
    else {
        return false;
    };
    let Ok(current) = received_at.parse::<jiff::Timestamp>() else {
        return false;
    };
    let elapsed = current.as_second().saturating_sub(previous.as_second());
    (0..=1).contains(&elapsed)
}

fn event_uses_exact_replay_horizon(event: &TurnEvent) -> bool {
    !(event.provider == AgentKind::Claude.as_str()
        && event.kind == TurnEventKind::Progress
        && event.source_kind == SourceKind::ProviderHook
        && event.provider_turn_id.is_none())
}

fn normalize_provider_identifier(
    runtime_id: &str,
    agent: AgentKind,
    field: &str,
    value: &str,
) -> Result<String, CliError> {
    if let Some(digest) = value.strip_prefix("local:v1:") {
        if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(value.to_string());
        }
        return Err(CliError::data(
            "provider-hook-identifier-invalid",
            "projected provider identifier must use local:v1 followed by 64 hexadecimal characters",
            Some(json!({ "field": field })),
        ));
    }
    projected_provider_identifier(runtime_id, agent, field, value)
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let mut rendered = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        use std::fmt::Write as _;
        let _ = write!(rendered, "{byte:02x}");
    }
    rendered
}

fn starting_state(at: String, revision: u64, last_turn: Option<LastTurn>) -> TurnState {
    TurnState {
        schema_version: TURN_STATE_VERSION.to_string(),
        phase: TurnPhase::Starting,
        phase_changed_at: at,
        revision,
        source: runtime_source(),
        current_turn: None,
        last_turn,
        extra: Map::new(),
    }
}

pub(crate) fn activate_runtime(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<TurnState, CliError> {
    let runtime = record.runtime.as_ref().ok_or_else(|| {
        CliError::data(
            "runtime-id-missing",
            "session runtime is missing its launch id",
            Some(json!({ "id": record.id })),
        )
    })?;
    let runtime_id = runtime.launch_id.as_str();
    if runtime_id.is_empty() {
        return Err(CliError::data(
            "runtime-id-missing",
            "session runtime is missing its launch id",
            Some(json!({ "id": record.id })),
        ));
    }
    let runtime_generation = runtime.generation;
    let dir = session_dir(context, &record.id);
    let _lock = acquire_lock(&dir)?;
    let path = dir.join(ACTIVITY_FILE);
    let journal_path = dir.join(ACTIVITY_JOURNAL_FILE);
    let replay_path = dir.join(ACTIVITY_REPLAY_FILE);
    let mut existing = if path.is_file() {
        match read_document(&path) {
            Ok(mut document) => {
                repair_pending_transaction(&path, &journal_path, &replay_path, &mut document)?;
                Some(document)
            }
            Err(err) => {
                quarantine_activity_snapshot(&path, err.code())?;
                None
            }
        }
    } else {
        None
    };
    if let Some(existing) = existing.as_ref()
        && existing.runtime_id == runtime_id
        && existing.runtime_generation == runtime_generation
    {
        return Ok(
            match runtime_unhealthy_marker(&dir, runtime_id, runtime_generation) {
                RuntimeUnhealthyStatus::Matching(state) => *state,
                RuntimeUnhealthyStatus::Pending(marked_at) => marker_state_from_snapshot(
                    &dir,
                    record,
                    runtime_id,
                    runtime_generation,
                    &marked_at,
                ),
                RuntimeUnhealthyStatus::Invalid => marker_state_from_snapshot(
                    &dir,
                    record,
                    runtime_id,
                    runtime_generation,
                    &existing.state.phase_changed_at,
                ),
                RuntimeUnhealthyStatus::Absent if replay_matches_document(&dir, existing) => {
                    existing.state.clone()
                }
                RuntimeUnhealthyStatus::Absent => unknown_state(record),
            },
        );
    }
    initialize_replay_index(&replay_path, runtime_id, runtime_generation)?;
    let at = runtime.started_at.clone();
    let mut last_turn = existing
        .as_ref()
        .and_then(|document| document.state.last_turn.clone());
    if let Some(current) = existing
        .as_ref()
        .and_then(|document| document.state.current_turn.as_ref())
    {
        last_turn = Some(LastTurn {
            provider_turn_id: current.provider_turn_id.clone(),
            started_at: Some(current.started_at.clone()),
            completed_at: at.clone(),
            outcome: "interrupted".to_string(),
            extra: current.extra.clone(),
        });
    }
    let revision = existing
        .as_ref()
        .map(|document| document.state.revision.saturating_add(1))
        .unwrap_or(1);
    let mut state = starting_state(at, revision, last_turn);
    if let Some(document) = existing.as_ref() {
        state.extra = document.state.extra.clone();
        state.source.extra = document.state.source.extra.clone();
    }
    let document = ActivityDocument {
        schema_version: ACTIVITY_DOCUMENT_VERSION.to_string(),
        runtime_id: runtime_id.to_string(),
        runtime_generation,
        state,
        pending_attention: Vec::new(),
        overflow_attention: None,
        seen_event_count: 0,
        last_semantic_event: None,
        last_semantic_event_at: None,
        provider_session_id: record
            .provider_resume
            .as_ref()
            .filter(|resume| resume.provider == record.agent)
            .map(|resume| {
                projected_provider_identifier(
                    runtime_id,
                    AgentKind::from_name(&record.agent).expect("validated session agent"),
                    "session",
                    &resume.session_id,
                )
            })
            .transpose()?,
        last_event_at: None,
        pending_journal: None,
        runtime_unhealthy_reason: None,
        extra: existing
            .take()
            .map_or_else(Map::new, |document| document.extra),
    };
    write_document(&path, &document)?;
    remove_runtime_unhealthy_marker(&dir)?;
    Ok(document.state)
}

pub(crate) fn mark_runtime_unhealthy(
    context: &CliContext,
    id: &str,
    runtime_id: &str,
    reason: &str,
) -> Result<(), CliError> {
    let observed = load_session_record(context, id)?;
    let observed_runtime = observed.runtime.as_ref().ok_or_else(|| {
        CliError::data(
            "runtime-id-missing",
            "session runtime is missing its launch id",
            Some(json!({ "id": observed.id })),
        )
    })?;
    if observed_runtime.launch_id != runtime_id {
        return Err(CliError::data(
            "runtime-id-mismatch",
            "activity degradation does not belong to the active runtime generation",
            Some(json!({ "id": observed.id })),
        ));
    }
    let dir = session_dir(context, id);
    {
        let _health_fence = acquire_health_fence(&dir)?;
        match runtime_unhealthy_marker(&dir, runtime_id, observed_runtime.generation) {
            RuntimeUnhealthyStatus::Matching(_) => return Ok(()),
            RuntimeUnhealthyStatus::Pending(_) | RuntimeUnhealthyStatus::Invalid => {}
            RuntimeUnhealthyStatus::Absent => {
                // Poison this exact runtime generation before waiting for the
                // shared session-record fence. The health fence linearizes the
                // poison write against activity commits and auto-resume claims.
                write_runtime_unhealthy_pending_marker(
                    &dir,
                    runtime_id,
                    observed_runtime.generation,
                    reason,
                )?;
            }
        }
    }
    let _record_lock = match crate::acquire_session_record_lock_timed(
        context,
        &observed.id,
        RUNTIME_UNHEALTHY_LOCK_TIMEOUT,
    ) {
        Ok(lock) => lock,
        Err(error) if error.code() == "session-record-lock-timeout" => {
            // The scoped pending marker is already a durable fail-closed
            // barrier. A later retry or runtime activation can reconcile the
            // public state after the record writer releases its fence.
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let record = load_session_record(context, &observed.id)?;
    crate::ensure_same_session_identity(&observed, &record)?;
    let runtime = record.runtime.as_ref().ok_or_else(|| {
        CliError::data(
            "runtime-id-missing",
            "session runtime is missing its launch id",
            Some(json!({ "id": record.id })),
        )
    })?;
    if runtime.launch_id != runtime_id {
        return Err(CliError::data(
            "runtime-id-mismatch",
            "activity degradation does not belong to the active runtime generation",
            Some(json!({ "id": record.id })),
        ));
    }
    if matches!(
        runtime_unhealthy_marker(&dir, runtime_id, runtime.generation),
        RuntimeUnhealthyStatus::Matching(_)
    ) {
        return Ok(());
    }
    let marker_state =
        marker_state_from_snapshot(&dir, &record, runtime_id, runtime.generation, &now());
    write_runtime_unhealthy_marker(&dir, runtime_id, runtime.generation, reason, &marker_state)?;
    let _lock = match acquire_lock_with_timeout(&dir, RUNTIME_UNHEALTHY_LOCK_TIMEOUT) {
        Ok(lock) => lock,
        Err(error) if error.code() == "activity-lock-timeout" => {
            // The marker is the durable fail-closed source of truth. Mirroring
            // into activity.json is best effort while another writer owns the
            // lock; views and later ingestion already reject this generation.
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let path = dir.join(ACTIVITY_FILE);
    let journal_path = dir.join(ACTIVITY_JOURNAL_FILE);
    let replay_path = dir.join(ACTIVITY_REPLAY_FILE);
    let mut document = read_document(&path)?;
    repair_pending_transaction(&path, &journal_path, &replay_path, &mut document)?;
    if document.runtime_id != runtime_id || document.runtime_generation != runtime.generation {
        return Err(CliError::data(
            "runtime-id-mismatch",
            "activity snapshot does not match the active runtime generation",
            Some(json!({ "id": record.id })),
        ));
    }
    if document.runtime_unhealthy_reason.is_some() {
        write_runtime_unhealthy_marker(
            &dir,
            runtime_id,
            runtime.generation,
            reason,
            &document.state,
        )?;
        return Ok(());
    }
    document.runtime_unhealthy_reason = Some(reason.to_string());
    document.pending_attention.clear();
    document.overflow_attention = None;
    document.state = if marker_state.revision > document.state.revision {
        marker_state
    } else {
        degraded_state(document.state, &now())
    };
    write_runtime_unhealthy_marker(
        &dir,
        runtime_id,
        runtime.generation,
        reason,
        &document.state,
    )?;
    write_document(&path, &document)
}

fn quarantine_activity_snapshot(path: &Path, code: &str) -> Result<(), CliError> {
    let parent = path.parent().ok_or_else(|| {
        CliError::runtime(
            "activity-quarantine-failed",
            "activity snapshot has no parent directory",
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    let destination = parent.join(format!(
        "activity.quarantine.{}.{}.json",
        code,
        uuid::Uuid::new_v4()
    ));
    fs::rename(path, &destination).map_err(|err| {
        CliError::runtime(
            "activity-quarantine-failed",
            format!("failed to quarantine {}: {err}", path.display()),
            Some(json!({
                "path": display_path(path),
                "quarantine_path": display_path(&destination)
            })),
        )
    })?;
    fs::set_permissions(&destination, fs::Permissions::from_mode(SECRET_FILE_MODE)).map_err(|err| {
        activity_io_error("activity-quarantine-permission-failed", &destination, err)
    })
}

fn unknown_state(record: &SessionRecord) -> TurnState {
    TurnState {
        schema_version: TURN_STATE_VERSION.to_string(),
        phase: TurnPhase::Unknown,
        phase_changed_at: record.updated_at.clone(),
        revision: 0,
        source: runtime_source(),
        current_turn: None,
        last_turn: None,
        extra: Map::new(),
    }
}

fn activity_matches_runtime(document: &ActivityDocument, record: &SessionRecord) -> bool {
    record.runtime.as_ref().is_some_and(|runtime| {
        !runtime.launch_id.is_empty()
            && document.runtime_id == runtime.launch_id
            && document.runtime_generation == runtime.generation
    })
}

fn replay_matches_document(dir: &Path, document: &ActivityDocument) -> bool {
    let replay_path = dir.join(ACTIVITY_REPLAY_FILE);
    if document.seen_event_count == 0 && !replay_path.is_file() {
        return true;
    }
    open_replay_index(
        &replay_path,
        &document.runtime_id,
        document.runtime_generation,
        false,
    )
    .is_ok()
}

pub(crate) fn state_for_view(context: &CliContext, record: &SessionRecord) -> Option<TurnState> {
    let dir = session_dir(context, &record.id);
    if let Some(runtime) = record.runtime.as_ref() {
        match runtime_unhealthy_marker(&dir, &runtime.launch_id, runtime.generation) {
            RuntimeUnhealthyStatus::Matching(state) => return Some(*state),
            RuntimeUnhealthyStatus::Pending(marked_at) => {
                return Some(marker_state_from_snapshot(
                    &dir,
                    record,
                    &runtime.launch_id,
                    runtime.generation,
                    &marked_at,
                ));
            }
            RuntimeUnhealthyStatus::Invalid => {
                return Some(marker_state_from_snapshot(
                    &dir,
                    record,
                    &runtime.launch_id,
                    runtime.generation,
                    &runtime.started_at,
                ));
            }
            RuntimeUnhealthyStatus::Absent => {}
        }
    }
    let path = dir.join(ACTIVITY_FILE);
    if !path.is_file() {
        return None;
    }
    match read_document(&path) {
        Ok(document)
            if activity_matches_runtime(&document, record)
                && replay_matches_document(&dir, &document) =>
        {
            Some(document.state)
        }
        Ok(_) | Err(_) => Some(unknown_state(record)),
    }
}

pub(crate) fn runtime_is_unhealthy(context: &CliContext, record: &SessionRecord) -> bool {
    let Some(runtime) = record.runtime.as_ref() else {
        return false;
    };
    !matches!(
        runtime_unhealthy_marker(
            &session_dir(context, &record.id),
            &runtime.launch_id,
            runtime.generation,
        ),
        RuntimeUnhealthyStatus::Absent
    )
}

pub(crate) fn capture_snapshot(
    context: &CliContext,
    id: &str,
) -> Result<ActivitySnapshot, CliError> {
    let dir = session_dir(context, id);
    let _lock = acquire_lock(&dir)?;
    Ok(ActivitySnapshot {
        document: read_optional_activity_file(&dir.join(ACTIVITY_FILE))?,
        replay: read_optional_activity_file(&dir.join(ACTIVITY_REPLAY_FILE))?,
        unhealthy: read_optional_activity_file(&dir.join(ACTIVITY_UNHEALTHY_FILE))?,
    })
}

fn read_optional_activity_file(path: &Path) -> Result<Option<Vec<u8>>, CliError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(activity_io_error("activity-read-failed", path, err)),
    }
}

pub(crate) fn restore_snapshot(
    context: &CliContext,
    id: &str,
    snapshot: &ActivitySnapshot,
) -> Result<(), CliError> {
    let dir = session_dir(context, id);
    let _lock = acquire_lock(&dir)?;
    restore_activity_file(&dir.join(ACTIVITY_FILE), snapshot.document.as_deref())?;
    restore_activity_file(&dir.join(ACTIVITY_REPLAY_FILE), snapshot.replay.as_deref())?;
    restore_activity_file(
        &dir.join(ACTIVITY_UNHEALTHY_FILE),
        snapshot.unhealthy.as_deref(),
    )
}

fn restore_activity_file(path: &Path, snapshot: Option<&[u8]>) -> Result<(), CliError> {
    if let Some(bytes) = snapshot {
        write_atomic(path, bytes, SECRET_FILE_MODE).map_err(|err| {
            CliError::runtime(
                "activity-write-failed",
                format!("activity storage failed at {}: {err}", path.display()),
                Some(json!({ "path": display_path(path) })),
            )
        })
    } else {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(activity_io_error("activity-remove-failed", path, err)),
        }
    }
}

pub(crate) fn read_event_from_stdin() -> Result<TurnEvent, CliError> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(MAX_EVENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| {
            CliError::runtime(
                "activity-stdin-read-failed",
                format!("failed to read event stdin: {err}"),
                None,
            )
        })?;
    if bytes.len() as u64 > MAX_EVENT_BYTES {
        return Err(CliError::data(
            "activity-event-too-large",
            "activity event exceeds 65536 bytes",
            Some(json!({ "max_bytes": MAX_EVENT_BYTES })),
        ));
    }
    serde_json::from_slice(&bytes).map_err(|err| {
        CliError::data(
            "activity-event-invalid",
            format!("activity event is not valid allowlisted JSON: {err}"),
            None,
        )
    })
}

pub(crate) fn ingest_event(
    context: &CliContext,
    id: &str,
    event: TurnEvent,
) -> Result<ActivityResult, CliError> {
    ingest_event_with_lock(context, id, event, ActivityLockMode::Blocking)
}

fn ingest_event_nonblocking(
    context: &CliContext,
    id: &str,
    event: TurnEvent,
) -> Result<ActivityResult, CliError> {
    ingest_event_with_lock(context, id, event, ActivityLockMode::NonBlocking)
}

pub(crate) fn ingest_event_retry(
    context: &CliContext,
    id: &str,
    event: TurnEvent,
) -> Result<ActivityResult, CliError> {
    ingest_event_with_lock(
        context,
        id,
        event,
        ActivityLockMode::Timed(CODEX_COMPLETION_RETRY_TIMEOUT),
    )
}

fn ingest_event_with_lock(
    context: &CliContext,
    id: &str,
    mut event: TurnEvent,
    lock_mode: ActivityLockMode,
) -> Result<ActivityResult, CliError> {
    validate_event(&event)?;
    let observed = load_session_record(context, id)?;
    let record_lock = match lock_mode {
        ActivityLockMode::Blocking => crate::acquire_session_record_lock(context, &observed.id)?,
        ActivityLockMode::NonBlocking => {
            crate::try_acquire_session_record_lock(context, &observed.id)?.ok_or_else(|| {
                CliError::runtime(
                    "activity-lock-busy",
                    "activity state is busy",
                    Some(json!({ "id": observed.id })),
                )
            })?
        }
        ActivityLockMode::Timed(timeout) => {
            crate::acquire_session_record_lock_timed(context, &observed.id, timeout)?
        }
    };
    let record = load_session_record(context, &observed.id)?;
    crate::ensure_same_session_identity(&observed, &record)?;
    if event.provider != record.agent {
        return Err(CliError::data(
            "activity-provider-mismatch",
            "activity event provider does not match the session provider",
            Some(json!({ "id": record.id, "provider": event.provider })),
        ));
    }
    let active_runtime = record.runtime.as_ref();
    let active_runtime_id = active_runtime
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    let active_runtime_generation = active_runtime.map_or(0, |runtime| runtime.generation);
    if active_runtime_id.is_empty() || event.runtime_id != active_runtime_id {
        return Err(CliError::data(
            "runtime-id-mismatch",
            "activity event does not belong to the active runtime generation",
            Some(json!({ "id": record.id })),
        ));
    }
    let agent = AgentKind::from_name(&record.agent).expect("validated session agent");
    event.provider_session_id = event
        .provider_session_id
        .as_deref()
        .map(|value| normalize_provider_identifier(active_runtime_id, agent, "session", value))
        .transpose()?;
    event.provider_turn_id = event
        .provider_turn_id
        .as_deref()
        .map(|value| normalize_provider_identifier(active_runtime_id, agent, "turn", value))
        .transpose()?;
    let expected_provider_session_id = record
        .provider_resume
        .as_ref()
        .filter(|resume| resume.provider == record.agent)
        .map(|resume| {
            projected_provider_identifier(active_runtime_id, agent, "session", &resume.session_id)
        })
        .transpose()?;
    if let (Some(expected), Some(observed)) = (
        expected_provider_session_id.as_ref(),
        event.provider_session_id.as_ref(),
    ) && expected != observed
    {
        return Err(CliError::data(
            "provider-session-id-mismatch",
            "activity event does not belong to the provider session bound to this runtime",
            Some(json!({ "id": record.id })),
        ));
    }
    if expected_provider_session_id.is_some() && event.provider_session_id.is_none() {
        return Err(CliError::data(
            "provider-session-id-missing",
            "activity event omitted the provider session identity required by this runtime",
            Some(json!({ "id": record.id })),
        ));
    }

    let dir = session_dir(context, &record.id);
    if !matches!(
        runtime_unhealthy_marker(&dir, active_runtime_id, active_runtime_generation),
        RuntimeUnhealthyStatus::Absent
    ) {
        return Err(CliError::data(
            "activity-runtime-unhealthy",
            "activity evidence is unavailable until the session starts a new runtime generation",
            Some(json!({ "id": record.id })),
        ));
    }
    let _lock = match lock_mode {
        ActivityLockMode::Blocking => acquire_lock(&dir)?,
        ActivityLockMode::NonBlocking => acquire_lock_nonblocking(&dir)?,
        ActivityLockMode::Timed(timeout) => acquire_lock_with_timeout(&dir, timeout)?,
    };
    let _health_fence = acquire_health_fence(&dir)?;
    if !matches!(
        runtime_unhealthy_marker(&dir, active_runtime_id, active_runtime_generation),
        RuntimeUnhealthyStatus::Absent
    ) {
        return Err(CliError::data(
            "activity-runtime-unhealthy",
            "activity evidence is unavailable until the session starts a new runtime generation",
            Some(json!({ "id": record.id })),
        ));
    }
    let path = dir.join(ACTIVITY_FILE);
    let journal_path = dir.join(ACTIVITY_JOURNAL_FILE);
    let replay_path = dir.join(ACTIVITY_REPLAY_FILE);
    let mut document = read_document(&path)?;
    if document.runtime_id != event.runtime_id
        || document.runtime_generation != active_runtime_generation
    {
        return Err(CliError::data(
            "runtime-id-mismatch",
            "activity snapshot does not match the active runtime generation",
            Some(json!({ "id": record.id })),
        ));
    }
    repair_pending_transaction(&path, &journal_path, &replay_path, &mut document)?;
    if let Some(reason) = document.runtime_unhealthy_reason.as_deref() {
        return Err(CliError::data(
            "activity-runtime-unhealthy",
            "activity evidence is unavailable until the session starts a new runtime generation",
            Some(json!({ "id": record.id, "reason": reason })),
        ));
    }
    if event.kind == TurnEventKind::AttentionRequested
        && let (Some(expected), Some(observed)) = (
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.provider_turn_id.as_ref()),
            event.provider_turn_id.as_ref(),
        )
        && expected != observed
    {
        return Err(CliError::data(
            "provider-turn-id-mismatch",
            "attention request does not belong to the active provider turn",
            Some(json!({ "id": record.id })),
        ));
    }
    if let (Some(bound), Some(observed)) = (
        document.provider_session_id.as_ref(),
        event.provider_session_id.as_ref(),
    ) && bound != observed
    {
        return Err(CliError::data(
            "provider-session-id-mismatch",
            "activity event changed provider session identity within one runtime",
            Some(json!({ "id": record.id })),
        ));
    }
    if document.provider_session_id.is_some() && event.provider_session_id.is_none() {
        return Err(CliError::data(
            "provider-session-id-missing",
            "activity event omitted the provider session identity already bound to this runtime",
            Some(json!({ "id": record.id })),
        ));
    }
    let uses_exact_replay_horizon = event_uses_exact_replay_horizon(&event);
    let dedupe_key = event_dedupe_key(&event.runtime_id, &event.event_id);
    if uses_exact_replay_horizon
        && replay_contains(
            &replay_path,
            &document.runtime_id,
            document.runtime_generation,
            document.seen_event_count == 0,
            &dedupe_key,
        )?
    {
        let state = document.state.clone();
        drop(_health_fence);
        drop(_lock);
        drop(record_lock);
        arm_auto_resume_from_event(context, &record.id, &event, &state, &now())?;
        return Ok(ActivityResult {
            id: record.id,
            turn_state: state,
            duplicate: true,
        });
    }

    let received_at = now();
    let semantic_key = semantic_event_key(&event);
    if semantic_event_is_duplicate(&document, &event, &semantic_key, &received_at) {
        let state = document.state.clone();
        drop(_health_fence);
        drop(_lock);
        drop(record_lock);
        arm_auto_resume_from_event(context, &record.id, &event, &state, &received_at)?;
        return Ok(ActivityResult {
            id: record.id,
            turn_state: state,
            duplicate: true,
        });
    }
    if uses_exact_replay_horizon && document.seen_event_count >= MAX_DEDUPE_EVENTS {
        return Err(CliError::data(
            "activity-dedupe-capacity-reached",
            "activity event replay horizon is full for this runtime; resume the session to start a new runtime generation",
            Some(json!({ "id": record.id, "max_events": MAX_DEDUPE_EVENTS })),
        ));
    }

    reduce(&mut document, &event, &received_at);
    document.last_event_at = Some(received_at.clone());
    if matches!(event.source_kind, SourceKind::ProviderHook) {
        document.last_semantic_event = Some(semantic_key);
        document.last_semantic_event_at = Some(received_at.clone());
    }
    if document.provider_session_id.is_none() {
        document.provider_session_id = event.provider_session_id.clone();
    }
    if uses_exact_replay_horizon {
        document.seen_event_count = document.seen_event_count.saturating_add(1);
    }
    let journal_entry = JournalEntry {
        received_at: received_at.clone(),
        event: event.clone(),
    };
    document.pending_journal = Some(journal_entry.clone());
    write_document(&path, &document)?;
    if uses_exact_replay_horizon {
        replay_insert(
            &replay_path,
            &document.runtime_id,
            document.runtime_generation,
            false,
            &dedupe_key,
        )?;
    }
    append_journal_entry(&journal_path, journal_entry)?;
    document.pending_journal = None;
    write_document(&path, &document)?;
    let state = document.state;
    drop(_health_fence);
    drop(_lock);
    drop(record_lock);
    arm_auto_resume_from_event(context, &record.id, &event, &state, &received_at)?;
    Ok(ActivityResult {
        id: record.id,
        turn_state: state,
        duplicate: false,
    })
}

fn arm_auto_resume_from_event(
    context: &CliContext,
    id: &str,
    event: &TurnEvent,
    state: &TurnState,
    received_at: &str,
) -> Result<(), CliError> {
    if event.kind != TurnEventKind::TurnFailed
        || event.failure_reason.as_deref() != Some("usage_exhausted")
        || event.confidence != Confidence::Authoritative
        || !matches!(event.source_kind, SourceKind::ProviderHook)
    {
        return Ok(());
    }
    let blocked_turn_id = event
        .provider_turn_id
        .clone()
        .unwrap_or_else(|| event.event_id.clone());
    crate::auto_resume::arm_usage_exhaustion(
        context,
        id,
        blocked_turn_id,
        state.revision,
        received_at,
    )?;
    Ok(())
}

pub(crate) fn activity_status(context: &CliContext, id: &str) -> Result<ActivityResult, CliError> {
    let record = load_session_record(context, id)?;
    let state = state_for_view(context, &record).unwrap_or_else(|| TurnState {
        schema_version: TURN_STATE_VERSION.to_string(),
        phase: TurnPhase::Unknown,
        phase_changed_at: record.updated_at,
        revision: 0,
        source: runtime_source(),
        current_turn: None,
        last_turn: None,
        extra: Map::new(),
    });
    Ok(ActivityResult {
        id: record.id,
        turn_state: state,
        duplicate: false,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodexHookAttentionDisposition {
    Accept,
    Suppress,
    Breach,
}

fn codex_hook_attention_disposition(
    record: &SessionRecord,
    injected_authority: Option<&str>,
) -> CodexHookAttentionDisposition {
    match (
        crate::codex_app_server::attention_authority(record),
        injected_authority,
    ) {
        ("protocol", Some("protocol")) => CodexHookAttentionDisposition::Suppress,
        ("hook", None | Some("hook")) => CodexHookAttentionDisposition::Accept,
        _ => CodexHookAttentionDisposition::Breach,
    }
}

pub(crate) fn ingest_provider_hook(
    context: &CliContext,
    agent: AgentKind,
    event_override: Option<&str>,
) -> Result<bool, CliError> {
    let Some(id) = std::env::var("AGENT_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(false);
    };
    let Some(runtime_id) = std::env::var("AGENT_SESSION_RUNTIME_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(false);
    };
    let mut bytes = Vec::new();
    io::stdin()
        .take(MAX_EVENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| {
            CliError::runtime(
                "provider-hook-read-failed",
                format!("failed to read provider hook metadata: {err}"),
                None,
            )
        })?;
    if bytes.len() as u64 > MAX_EVENT_BYTES {
        return Err(CliError::data(
            "provider-hook-too-large",
            "provider hook payload exceeds the local metadata parser limit",
            None,
        ));
    }
    let raw: Value = serde_json::from_slice(&bytes).map_err(|_| {
        CliError::data(
            "provider-hook-invalid",
            "provider hook payload is not valid JSON",
            None,
        )
    })?;
    let raw_event_name = event_override
        .or_else(|| raw.get("hook_event_name").and_then(Value::as_str))
        .or_else(|| raw.get("event").and_then(Value::as_str));
    if agent == AgentKind::Codex && raw_event_name == Some("PermissionRequest") {
        let record = load_session_record(context, &id)?;
        let injected_authority =
            std::env::var(crate::codex_app_server::ATTENTION_AUTHORITY_ENV).ok();
        match codex_hook_attention_disposition(&record, injected_authority.as_deref()) {
            CodexHookAttentionDisposition::Accept => {}
            CodexHookAttentionDisposition::Suppress => {
                // The managed app-server adapter is the sole attention
                // authority. Suppress the generic reporter before it can
                // normalize or mutate durable activity state.
                return Ok(false);
            }
            CodexHookAttentionDisposition::Breach => {
                mark_runtime_unhealthy(
                    context,
                    &id,
                    &runtime_id,
                    "codex_attention_authority_mismatch",
                )?;
                return Err(CliError::data(
                    "codex-attention-authority-breach",
                    "Codex permission hook authority did not match the immutable runtime selection",
                    Some(json!({ "id": record.id })),
                ));
            }
        }
    }
    let Some(event) = normalize_provider_hook(agent, event_override, &runtime_id, &raw)? else {
        return Ok(false);
    };
    let provider_resume = provider_resume_from_user_prompt_hook(
        context,
        &id,
        agent,
        &runtime_id,
        event_override,
        &raw,
    );
    let _ = ingest_event(context, &id, event)?;
    provider_resume?;
    Ok(true)
}

fn provider_resume_from_user_prompt_hook(
    context: &CliContext,
    id: &str,
    agent: AgentKind,
    runtime_id: &str,
    event_override: Option<&str>,
    raw: &Value,
) -> Result<(), CliError> {
    let event_name = event_override
        .or_else(|| raw.get("hook_event_name").and_then(Value::as_str))
        .or_else(|| raw.get("event").and_then(Value::as_str));
    if agent != AgentKind::Codex || event_name != Some("UserPromptSubmit") {
        return Ok(());
    }
    let Some(session_id) = raw
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    mutate_session_record(context, id, |record| {
        let runtime_matches = record
            .runtime
            .as_ref()
            .is_some_and(|runtime| runtime.launch_id == runtime_id);
        if record.agent != agent.as_str() || !runtime_matches {
            return Err(CliError::data(
                "provider-hook-runtime-mismatch",
                "provider hook runtime does not match the active session runtime",
                None,
            ));
        }
        if let Some(existing) = record.provider_resume.as_ref() {
            if existing.provider == agent.as_str() && existing.session_id == session_id {
                let resume_args = canonical_provider_resume_args(agent, &record.cwd, session_id)
                    .ok_or_else(|| {
                        CliError::data(
                            "provider-hook-session-invalid",
                            "provider hook session identity cannot be resumed",
                            None,
                        )
                    })?;
                if existing.capture_method == "codex-user-prompt-submit-hook"
                    && existing.resume_args == resume_args
                {
                    return Ok(());
                }
                let mut promoted = existing.clone();
                promoted.captured_at = Zoned::now().timestamp().to_string();
                promoted.capture_method = "codex-user-prompt-submit-hook".to_string();
                promoted.resume_args = resume_args;
                record.provider_resume = Some(promoted);
                return Ok(());
            }
            return Err(CliError::data(
                "provider-hook-session-mismatch",
                "provider hook session identity conflicts with the durable session identity",
                None,
            ));
        }
        let resume_args = canonical_provider_resume_args(agent, &record.cwd, session_id)
            .ok_or_else(|| {
                CliError::data(
                    "provider-hook-session-invalid",
                    "provider hook session identity cannot be resumed",
                    None,
                )
            })?;
        record.provider_resume = Some(ProviderResume {
            provider: agent.as_str().to_string(),
            session_id: session_id.to_string(),
            captured_at: Zoned::now().timestamp().to_string(),
            capture_method: "codex-user-prompt-submit-hook".to_string(),
            resume_args,
            extra: BTreeMap::new(),
        });
        Ok(())
    })
}

pub(crate) fn ingest_provider_hook_fail_open(
    context: &CliContext,
    agent: AgentKind,
    event_override: Option<&str>,
) {
    match ingest_provider_hook(context, agent, event_override) {
        Ok(true) => clear_hook_diagnostic(context, agent),
        Ok(false) => {}
        Err(err) => record_hook_diagnostic(context, agent, err.code()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderNotificationIngest {
    Ignored,
    Ingested,
    Deferred,
}

fn ingest_provider_notification(
    context: &CliContext,
    agent: AgentKind,
    payload: &str,
) -> Result<ProviderNotificationIngest, CliError> {
    let Some(id) = std::env::var("AGENT_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(ProviderNotificationIngest::Ignored);
    };
    let Some(runtime_id) = std::env::var("AGENT_SESSION_RUNTIME_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(ProviderNotificationIngest::Ignored);
    };
    if payload.len() as u64 > MAX_EVENT_BYTES {
        return Err(CliError::data(
            "provider-notification-too-large",
            "provider notification payload exceeds the local metadata parser limit",
            None,
        ));
    }
    let raw: Value = serde_json::from_str(payload).map_err(|_| {
        CliError::data(
            "provider-notification-invalid",
            "provider notification payload is not valid JSON",
            None,
        )
    })?;
    let Some(event) = normalize_provider_notification(agent, &runtime_id, &raw)? else {
        return Ok(ProviderNotificationIngest::Ignored);
    };
    if let Err(error) = ingest_event_nonblocking(context, &id, event.clone()) {
        if error.code() != "activity-lock-busy" {
            return Err(error);
        }
        spawn_activity_event_retry(context, &id, &event)?;
        return Ok(ProviderNotificationIngest::Deferred);
    }
    Ok(ProviderNotificationIngest::Ingested)
}

fn spawn_activity_event_retry(
    context: &CliContext,
    id: &str,
    event: &TurnEvent,
) -> Result<(), CliError> {
    let executable = std::env::current_exe().map_err(|err| {
        CliError::runtime(
            "provider-notification-retry-executable-failed",
            format!("failed to resolve the activity retry executable: {err}"),
            None,
        )
    })?;
    let bytes = serde_json::to_vec(event).map_err(|_| {
        CliError::data(
            "provider-notification-retry-event-invalid",
            "normalized provider notification could not be encoded for activity retry",
            None,
        )
    })?;
    let mut child = ProcessCommand::new(executable)
        .arg("--state-dir")
        .arg(&context.state_dir)
        .args(["activity", "event", id, "--stdin", "--format", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env(ACTIVITY_RETRY_PROVIDER_ENV, event.provider.as_str())
        .process_group(0)
        .spawn()
        .map_err(|err| {
            CliError::runtime(
                "provider-notification-retry-spawn-failed",
                format!("failed to spawn the metadata-only activity retry: {err}"),
                None,
            )
        })?;
    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| {
            CliError::runtime(
                "provider-notification-retry-stdin-failed",
                "metadata-only activity retry stdin was unavailable",
                None,
            )
        })?
        .write_all(&bytes);
    if let Err(err) = write_result {
        unsafe {
            let pgid = -(child.id() as libc::pid_t);
            let _ = libc::kill(pgid, libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
        return Err(CliError::runtime(
            "provider-notification-retry-write-failed",
            format!("failed to write metadata-only activity retry input: {err}"),
            None,
        ));
    }
    Ok(())
}

pub(crate) fn ingest_provider_notification_fail_open(
    context: &CliContext,
    agent: AgentKind,
    payload: &str,
) {
    match ingest_provider_notification(context, agent, payload) {
        Ok(ProviderNotificationIngest::Ingested) => clear_hook_diagnostic(context, agent),
        Ok(ProviderNotificationIngest::Ignored | ProviderNotificationIngest::Deferred) => {}
        Err(err) => record_hook_diagnostic(context, agent, err.code()),
    }
}

pub(crate) fn forward_provider_notification_fail_open(
    agent: AgentKind,
    encoded_argv: Option<&str>,
    payload: &str,
) {
    if agent != AgentKind::Codex {
        return;
    }
    if std::env::var_os(CODEX_NOTIFY_FORWARD_ACTIVE_ENV).is_some() {
        return;
    }
    let Some(encoded_argv) = encoded_argv else {
        return;
    };
    if encoded_argv.len() > MAX_CODEX_FORWARD_ARGV_BYTES {
        return;
    }
    let Ok(argv) = serde_json::from_str::<Vec<String>>(encoded_argv) else {
        return;
    };
    if !codex_forward_argv_is_safe(&argv) {
        return;
    }

    let mut command = ProcessCommand::new(&argv[0]);
    command
        .args(&argv[1..])
        .arg(payload)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env(CODEX_NOTIFY_FORWARD_ACTIVE_ENV, "1")
        .process_group(0);
    let Ok(mut child) = command.spawn() else {
        return;
    };
    let deadline = Instant::now() + CODEX_FORWARD_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                unsafe {
                    let pgid = -(child.id() as libc::pid_t);
                    let _ = libc::kill(pgid, libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

pub(crate) fn record_hook_diagnostic(context: &CliContext, agent: AgentKind, code: &str) {
    let Some(id) = std::env::var("AGENT_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    let Ok(record) = load_session_record(context, &id) else {
        return;
    };
    let Some(runtime) = record.runtime.as_ref() else {
        return;
    };
    let runtime_matches = std::env::var("AGENT_SESSION_RUNTIME_ID")
        .ok()
        .is_some_and(|runtime_id| runtime.launch_id == runtime_id);
    if record.agent != agent.as_str() || !runtime_matches {
        return;
    }
    let diagnostic = ActivityDiagnostic {
        schema_version: "agent-session.activity-diagnostic.v1".to_string(),
        provider: agent.as_str().to_string(),
        runtime_id: runtime.launch_id.clone(),
        runtime_generation: runtime.generation,
        code: code.to_string(),
        observed_at: now(),
    };
    let Ok(bytes) = serde_json::to_vec_pretty(&diagnostic) else {
        return;
    };
    let _ = write_atomic(
        &session_dir(context, &id).join(ACTIVITY_DIAGNOSTIC_FILE),
        &bytes,
        SECRET_FILE_MODE,
    );
}

pub(crate) fn clear_hook_diagnostic(context: &CliContext, agent: AgentKind) {
    let Some(id) = std::env::var("AGENT_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    if load_session_record(context, &id).is_ok_and(|record| {
        record.agent == agent.as_str()
            && std::env::var("AGENT_SESSION_RUNTIME_ID")
                .ok()
                .is_some_and(|runtime_id| {
                    record
                        .runtime
                        .as_ref()
                        .is_some_and(|runtime| runtime.launch_id == runtime_id)
                })
    }) {
        let _ = fs::remove_file(session_dir(context, &id).join(ACTIVITY_DIAGNOSTIC_FILE));
    }
}

fn normalize_provider_hook(
    agent: AgentKind,
    event_override: Option<&str>,
    runtime_id: &str,
    raw: &Value,
) -> Result<Option<TurnEvent>, CliError> {
    let event_name = event_override
        .or_else(|| raw.get("hook_event_name").and_then(Value::as_str))
        .or_else(|| raw.get("event").and_then(Value::as_str));
    let Some(event_name) = event_name else {
        return Ok(None);
    };
    let notification = raw.get("notification_type").and_then(Value::as_str);
    let tool_name = raw.get("tool_name").and_then(Value::as_str);
    if agent == AgentKind::Claude
        && event_name == "PermissionRequest"
        && tool_name == Some("AskUserQuestion")
    {
        // AskUserQuestion is already owned by the exact PreToolUse/PostToolUse
        // correlation below. Claude may also emit an uncorrelated permission
        // request for the same UI; admitting both would strand a conservative
        // approval after the exact question resolves.
        return Ok(None);
    }
    let exact_clarification = agent == AgentKind::Claude
        && tool_name == Some("AskUserQuestion")
        && matches!(
            event_name,
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure"
        );
    let claude_elicitation =
        agent == AgentKind::Claude && matches!(event_name, "Elicitation" | "ElicitationResult");
    let elicitation_id = claude_elicitation
        .then(|| optional_hook_string(raw, "elicitation_id"))
        .transpose()?
        .flatten();
    if event_name == "ElicitationResult" && elicitation_id.is_none() {
        // Claude documents this identifier as optional. An identifier-less
        // result cannot safely clear a conservative request latch.
        return Ok(None);
    }
    let exact_elicitation = claude_elicitation && elicitation_id.is_some();
    let exact_hermes_approval = agent == AgentKind::Hermes
        && matches!(
            event_name,
            "pre_approval_request" | "post_approval_response"
        );
    let hermes_approval_metadata = exact_hermes_approval
        .then(|| hermes_approval_metadata(raw))
        .transpose()?;
    if agent == AgentKind::Hermes
        && event_name == "post_approval_response"
        && !matches!(
            hermes_approval_metadata
                .and_then(|metadata| metadata.get("choice"))
                .and_then(Value::as_str),
            Some("once" | "session" | "always" | "deny" | "timeout")
        )
    {
        return Err(CliError::data(
            "provider-hook-response-invalid",
            "recognized Hermes approval response has an invalid or missing choice",
            None,
        ));
    }
    let (kind, attention_kind, confidence) = match (agent, event_name, notification) {
        (AgentKind::Codex, "UserPromptSubmit", _) => {
            (TurnEventKind::TurnStarted, None, Confidence::Observed)
        }
        (AgentKind::Codex, "PermissionRequest", _) => (
            TurnEventKind::AttentionRequested,
            Some("approval"),
            Confidence::Observed,
        ),
        (AgentKind::Codex, "PostToolUse", _) => {
            (TurnEventKind::Progress, None, Confidence::Observed)
        }
        (AgentKind::Codex, "Stop", _) => (TurnEventKind::StopObserved, None, Confidence::Observed),
        (AgentKind::Claude, "UserPromptSubmit", _) => {
            (TurnEventKind::TurnStarted, None, Confidence::Observed)
        }
        (AgentKind::Claude, "PreToolUse", _) if exact_clarification => (
            TurnEventKind::AttentionRequested,
            Some("clarification"),
            Confidence::Observed,
        ),
        (AgentKind::Claude, "PreToolUse", _) => {
            (TurnEventKind::Progress, None, Confidence::Observed)
        }
        (AgentKind::Claude, "PostToolUse", _) if exact_clarification => {
            (TurnEventKind::AttentionCleared, None, Confidence::Observed)
        }
        (AgentKind::Claude, "PostToolUseFailure", _) if exact_clarification => {
            (TurnEventKind::AttentionCleared, None, Confidence::Observed)
        }
        (AgentKind::Claude, "Elicitation", _) => (
            TurnEventKind::AttentionRequested,
            Some(if raw.get("mode").and_then(Value::as_str) == Some("url") {
                "authentication"
            } else {
                "clarification"
            }),
            Confidence::Observed,
        ),
        (AgentKind::Claude, "ElicitationResult", _) if exact_elicitation => {
            (TurnEventKind::AttentionCleared, None, Confidence::Observed)
        }
        (AgentKind::Claude, "PermissionRequest", _)
        | (AgentKind::Claude, "Notification", Some("permission_prompt")) => (
            TurnEventKind::AttentionRequested,
            Some("approval"),
            Confidence::Observed,
        ),
        (AgentKind::Claude, "Notification", Some("agent_needs_input")) => (
            TurnEventKind::AttentionRequested,
            Some("other"),
            Confidence::Observed,
        ),
        (AgentKind::Claude, "PostToolUse", _) => {
            (TurnEventKind::Progress, None, Confidence::Observed)
        }
        (AgentKind::Claude, "Stop", _) => (TurnEventKind::StopObserved, None, Confidence::Observed),
        (AgentKind::Claude, "StopFailure", _) => {
            (TurnEventKind::TurnFailed, None, Confidence::Authoritative)
        }
        (AgentKind::Claude, "Notification", Some("idle_prompt")) => {
            (TurnEventKind::TurnCompleted, None, Confidence::Observed)
        }
        (AgentKind::Hermes, "pre_llm_call", _) => {
            (TurnEventKind::TurnStarted, None, Confidence::Observed)
        }
        (AgentKind::Hermes, "post_llm_call", _) => (
            TurnEventKind::TurnCompleted,
            None,
            Confidence::Authoritative,
        ),
        (AgentKind::Hermes, "pre_approval_request", _) => (
            TurnEventKind::AttentionRequested,
            Some("approval"),
            Confidence::Observed,
        ),
        (AgentKind::Hermes, "post_approval_response", _) => {
            (TurnEventKind::AttentionCleared, None, Confidence::Observed)
        }
        _ => return Ok(None),
    };
    let failure_reason = (agent == AgentKind::Claude && event_name == "StopFailure")
        .then(|| {
            raw.get("error")
                .and_then(Value::as_str)
                .map(normalize_claude_failure_reason)
        })
        .flatten();
    let mut provider_session = optional_hook_string(raw, "session_id")?;
    if provider_session.is_none() {
        provider_session = optional_hook_string(raw, "session_key")?;
    }
    if provider_session.is_none()
        && let Some(metadata) = hermes_approval_metadata
    {
        provider_session = optional_hook_string(metadata, "session_key")?;
    }
    let provider_session_id = provider_session
        .map(|value| projected_provider_identifier(runtime_id, agent, "session", value))
        .transpose()?;
    let mut provider_turn = optional_hook_string(raw, "turn_id")?;
    if provider_turn.is_none()
        && let Some(metadata) = hermes_approval_metadata
    {
        provider_turn = optional_hook_string(metadata, "turn_id")?;
    }
    let provider_turn_id = provider_turn
        .map(|value| projected_provider_identifier(runtime_id, agent, "turn", value))
        .transpose()?;
    let (exact_attention_id, attention_correlation_ambiguous) = if exact_clarification {
        (
            raw.get("tool_use_id")
                .and_then(Value::as_str)
                .map(|value| projected_provider_identifier(runtime_id, agent, "attention", value))
                .transpose()?,
            false,
        )
    } else if exact_elicitation {
        (
            elicitation_id
                .map(|value| projected_provider_identifier(runtime_id, agent, "attention", value))
                .transpose()?,
            false,
        )
    } else if let Some(metadata) = hermes_approval_metadata {
        let (id, ambiguous) = hermes_approval_correlation(runtime_id, metadata)?;
        (Some(id), ambiguous)
    } else {
        (None, false)
    };
    if exact_clarification && exact_attention_id.is_none() {
        return Err(CliError::data(
            "provider-hook-correlation-missing",
            "recognized AskUserQuestion hook event is missing tool_use_id",
            None,
        ));
    }
    let attention_id = match kind {
        TurnEventKind::AttentionRequested
            if exact_clarification || exact_elicitation || exact_hermes_approval =>
        {
            exact_attention_id
        }
        TurnEventKind::AttentionRequested => Some(uuid::Uuid::new_v4().to_string()),
        TurnEventKind::AttentionCleared => exact_attention_id,
        _ => None,
    };
    let event_id = if exact_hermes_approval && !attention_correlation_ambiguous {
        stable_hermes_approval_event_id(
            runtime_id,
            &kind,
            attention_id
                .as_deref()
                .expect("exact Hermes approval correlation"),
        )
    } else {
        uuid::Uuid::new_v4().to_string()
    };
    Ok(Some(TurnEvent {
        schema_version: TURN_EVENT_VERSION.to_string(),
        event_id,
        runtime_id: runtime_id.to_string(),
        provider: agent.as_str().to_string(),
        provider_session_id,
        provider_turn_id,
        kind,
        failure_reason,
        attention_id,
        attention_kind: attention_kind.map(str::to_string),
        attention_correlation_ambiguous,
        confidence,
        source_kind: SourceKind::ProviderHook,
        provider_time: None,
    }))
}

fn normalize_claude_failure_reason(reason: &str) -> String {
    match reason {
        "rate_limit" => "usage_exhausted",
        "authentication_failed" => "authentication",
        "oauth_org_not_allowed" => "organization",
        "billing_error" => "billing",
        "invalid_request" => "invalid_request",
        "server_error" => "service",
        "max_output_tokens" => "max_output_tokens",
        _ => "unknown",
    }
    .to_string()
}

fn normalize_provider_notification(
    agent: AgentKind,
    runtime_id: &str,
    raw: &Value,
) -> Result<Option<TurnEvent>, CliError> {
    if agent != AgentKind::Codex
        || raw.get("type").and_then(Value::as_str) != Some("agent-turn-complete")
    {
        return Ok(None);
    }
    let provider_session_id = raw
        .get("thread-id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CliError::data(
                "provider-notification-session-id-missing",
                "Codex completion notification is missing thread-id",
                None,
            )
        })
        .and_then(|value| projected_provider_identifier(runtime_id, agent, "session", value))?;
    let provider_turn_id = raw
        .get("turn-id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CliError::data(
                "provider-notification-turn-id-missing",
                "Codex completion notification is missing turn-id",
                None,
            )
        })
        .and_then(|value| projected_provider_identifier(runtime_id, agent, "turn", value))?;
    Ok(Some(TurnEvent {
        schema_version: TURN_EVENT_VERSION.to_string(),
        event_id: uuid::Uuid::new_v4().to_string(),
        runtime_id: runtime_id.to_string(),
        provider: agent.as_str().to_string(),
        provider_session_id: Some(provider_session_id),
        provider_turn_id: Some(provider_turn_id),
        kind: TurnEventKind::TurnCompleted,
        failure_reason: None,
        attention_id: None,
        attention_kind: None,
        attention_correlation_ambiguous: false,
        confidence: Confidence::Authoritative,
        source_kind: SourceKind::ProviderHook,
        provider_time: None,
    }))
}

pub(crate) fn doctor(
    context: &CliContext,
    agent: Option<AgentKind>,
) -> Result<DoctorResult, CliError> {
    let agents = agent
        .map(|agent| vec![agent])
        .unwrap_or_else(|| vec![AgentKind::Codex, AgentKind::Claude, AgentKind::Hermes]);
    let activity_by_provider = latest_provider_activity(context);
    let version_probes = thread::scope(|scope| {
        let handles = agents
            .iter()
            .copied()
            .map(|agent| (agent, scope.spawn(move || provider_version(agent))))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|(agent, handle)| {
                (
                    agent.as_str().to_string(),
                    handle.join().unwrap_or_else(|_| VersionProbe {
                        version: None,
                        error: Some("probe-panicked".to_string()),
                    }),
                )
            })
            .collect::<BTreeMap<_, _>>()
    });
    let helper_executable =
        command_resolves_on_path("agent-session", std::env::var_os("PATH").as_deref());
    let mut providers = Vec::new();
    for agent in agents {
        let path = provider_config_path(agent)?;
        let notification_path = (agent == AgentKind::Codex)
            .then(codex_notification_config_path)
            .transpose()?;
        let (
            configured,
            configuration_error,
            notification_mode,
            hook_representation,
            hook_migration_required,
            representation_conflict,
            reported_config_path,
        ) = match agent {
            AgentKind::Codex => {
                let config_path = notification_path
                    .as_deref()
                    .expect("Codex notification path is resolved above");
                let hooks = codex_hook_status(&path, config_path);
                let notification = codex_notification_status(config_path);
                let configured = hooks.as_ref().is_ok_and(|status| status.configured)
                    && notification.as_ref().is_ok_and(|status| status.configured);
                let configuration_error = hooks
                    .as_ref()
                    .err()
                    .or_else(|| notification.as_ref().err())
                    .map(|error| error.code().to_string());
                let notification_mode = Some(
                    notification
                        .map(|status| status.mode)
                        .unwrap_or_else(|_| "invalid".to_string()),
                );
                let hook_representation = hooks
                    .as_ref()
                    .ok()
                    .map(|status| status.representation.as_str().to_string());
                let hook_migration_required =
                    hooks.as_ref().ok().map(|status| status.migration_required);
                let representation_conflict = hooks.as_ref().ok().map(|status| status.conflict);
                let reported_config_path = hooks
                    .as_ref()
                    .ok()
                    .filter(|status| status.representation == CodexHookRepresentation::InlineToml)
                    .map(|_| config_path.to_path_buf())
                    .unwrap_or_else(|| path.clone());
                (
                    configured,
                    configuration_error,
                    notification_mode,
                    hook_representation,
                    hook_migration_required,
                    representation_conflict,
                    reported_config_path,
                )
            }
            AgentKind::Claude | AgentKind::Hermes => match provider_configured(agent, &path) {
                Ok(configured) => (configured, None, None, None, None, None, path.clone()),
                Err(error) => (
                    false,
                    Some(error.code().to_string()),
                    None,
                    None,
                    None,
                    None,
                    path.clone(),
                ),
            },
        };
        let activity_summary = activity_by_provider
            .get(agent.as_str())
            .cloned()
            .unwrap_or_default();
        let version_probe = version_probes
            .get(agent.as_str())
            .cloned()
            .unwrap_or(VersionProbe {
                version: None,
                error: Some("probe-unavailable".to_string()),
            });
        let version = version_probe.version;
        let (supported_classification, completion, attention_correlation, trust, base_guidance) =
            match agent {
                AgentKind::Codex => (
                    "supported",
                    "agent-turn-complete is authoritative; raw Stop remains non-final observation because matching hooks may continue the turn",
                    "audited managed app-server runtimes correlate typed blocking request ids with serverRequest/resolved; raw or unmanaged PermissionRequest hooks remain conservative latches",
                    "Codex requires review/trust for each non-managed hook definition; representation migration changes hook source identities and requires one fresh review; a safe singular user notify argv is composed through bounded direct execution without a shell",
                    "Run activity setup --agent codex --dry-run before initial apply; for drift, review activity setup --agent codex --repair --dry-run before repair. After migration, review /hooks and verify a fresh session has no dual-representation warning. Unsafe, recursive, or non-reversible user-owned notify commands are preserved and reported without argv content",
                ),
                AgentKind::Claude => (
                    "partial",
                    "idle_prompt is observed completion; general PreToolUse reactivates continued work, while uncorrelated SubagentStop is ignored and raw Stop remains non-final because other hooks may continue",
                    "AskUserQuestion uses exact runtime-scoped tool_use_id correlation; Elicitation uses exact elicitation_id when both callbacks provide it and otherwise latches conservatively; other PermissionRequest and configured notification signals remain conservative latches",
                    "Claude settings hooks compose additively and execute with the user's permissions",
                    "Run activity setup --agent claude --dry-run and then --apply",
                ),
                AgentKind::Hermes => (
                    "supported",
                    "post_llm_call is authoritative for successful non-interrupted turns on the supported version",
                    "Hermes 0.18.2 shell approval hooks use projected non-empty tool_call_id for exact correlation; missing/empty-id tuple fallback retains conservative multiplicity until completion, a new turn, or a runtime boundary",
                    "Hermes shell hooks require first-use consent unless explicitly accepted",
                    "Run activity setup --agent hermes --dry-run, apply it, then approve and verify with hermes hooks doctor",
                ),
            };
        let audited = version
            .as_deref()
            .and_then(parse_version_triplet)
            .is_some_and(|version| version >= audited_floor(agent));
        let classification = if version.is_none() {
            "unavailable"
        } else if audited {
            supported_classification
        } else {
            "unverified"
        };
        let (exact_attention, attention_authority) = match agent {
            AgentKind::Codex => {
                let exact_attention = if version
                    .as_deref()
                    .and_then(parse_version_triplet)
                    .is_some_and(crate::codex_app_server::exact_attention_version_is_audited)
                {
                    "supported"
                } else {
                    "unverified"
                };
                (
                    exact_attention,
                    "protocol for audited managed app-server runtimes; hook for raw or unmanaged runtimes",
                )
            }
            AgentKind::Claude => (
                if audited {
                    "conditional: exact for AskUserQuestion and Elicitation callbacks with a non-empty shared id; conservative otherwise"
                } else {
                    "unverified"
                },
                "hook",
            ),
            AgentKind::Hermes => (if audited { "supported" } else { "unverified" }, "hook"),
        };
        let mut guidance = if matches!(classification, "unavailable" | "unverified") {
            format!(
                "Provider version is outside the audited floor {}; upgrade or validate it before relying on lifecycle state. {base_guidance}",
                format_version(audited_floor(agent))
            )
        } else {
            base_guidance.to_string()
        };
        if representation_conflict == Some(true) {
            guidance.push_str(
                " User-owned lifecycle hooks exist in both Codex representations; converge them manually onto one source before reviewing a fresh repair preview.",
            );
        } else if let Some(error) = configuration_error.as_deref() {
            guidance.push_str(&format!(
                " Provider lifecycle configuration could not be validated ({error}); fix the provider configuration before running repair."
            ));
        } else if !configured {
            if agent == AgentKind::Codex {
                guidance.push_str(
                    " The installed lifecycle specification is missing or drifted; run activity setup --agent codex --repair --dry-run and proceed with repair only when apply_allowed is true.",
                );
            } else {
                guidance.push_str(
                    " The installed hook specification is missing or drifted; run activity setup --repair after reviewing the dry-run.",
                );
            }
        }
        if !helper_executable {
            guidance.push_str(
                " The configured agent-session helper does not resolve to an executable on PATH; install it on the provider hook PATH before relying on activity state.",
            );
        }
        providers.push(ProviderDoctor {
            provider: agent.as_str().to_string(),
            classification: classification.to_string(),
            version,
            version_error: version_probe.error,
            configured,
            configuration_error,
            config_path: display_path(&reported_config_path),
            hook_representation,
            hook_migration_required,
            representation_conflict,
            notification_config_path: notification_path.as_deref().map(display_path),
            notification_mode,
            completion: completion.to_string(),
            attention_correlation: attention_correlation.to_string(),
            exact_attention: exact_attention.to_string(),
            attention_authority: attention_authority.to_string(),
            trust: trust.to_string(),
            guidance,
            last_event_at: activity_summary.last_event_at,
            last_error: activity_summary.last_error.map(|(_, code)| code),
            helper_executable,
        });
    }
    Ok(DoctorResult { providers })
}

fn command_resolves_on_path(command: &str, path: Option<&std::ffi::OsStr>) -> bool {
    let Some(path) = path else {
        return false;
    };
    std::env::split_paths(path).any(|directory| {
        fs::metadata(directory.join(command))
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    })
}

#[derive(Clone, Debug, Default)]
struct ProviderActivitySummary {
    last_event_at: Option<String>,
    last_error: Option<(String, String)>,
}

fn update_latest_error(summary: &mut ProviderActivitySummary, observed_at: String, code: String) {
    let replace = summary
        .last_error
        .as_ref()
        .is_none_or(|(current, _)| timestamp_is_later(&observed_at, current));
    if replace {
        summary.last_error = Some((observed_at, code));
    }
}

fn timestamp_is_later(candidate: &str, current: &str) -> bool {
    match (
        candidate.parse::<jiff::Timestamp>(),
        current.parse::<jiff::Timestamp>(),
    ) {
        (Ok(candidate), Ok(current)) => candidate > current,
        _ => candidate > current,
    }
}

fn latest_provider_activity(context: &CliContext) -> BTreeMap<String, ProviderActivitySummary> {
    let root = context.state_dir.join("sessions");
    let Ok(entries) = fs::read_dir(root) else {
        return BTreeMap::new();
    };
    let mut activity = BTreeMap::<String, ProviderActivitySummary>::new();
    for entry in entries.flatten() {
        let record_path = entry.path().join("session.json");
        let Ok(record_bytes) = fs::read(&record_path) else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<SessionRecord>(&record_bytes) else {
            continue;
        };
        if !matches!(record.agent.as_str(), "codex" | "claude" | "hermes") {
            continue;
        }
        let summary = activity.entry(record.agent.clone()).or_default();
        let diagnostic_path = entry.path().join(ACTIVITY_DIAGNOSTIC_FILE);
        if let Ok(bytes) = fs::read(&diagnostic_path)
            && let Ok(diagnostic) = serde_json::from_slice::<ActivityDiagnostic>(&bytes)
            && diagnostic.schema_version == "agent-session.activity-diagnostic.v1"
            && diagnostic.provider == record.agent
            && record.runtime.as_ref().is_some_and(|runtime| {
                diagnostic.runtime_id == runtime.launch_id
                    && diagnostic.runtime_generation == runtime.generation
            })
        {
            update_latest_error(summary, diagnostic.observed_at, diagnostic.code);
        }
        let activity_path = entry.path().join(ACTIVITY_FILE);
        if !activity_path.is_file() {
            continue;
        }
        match read_document(&activity_path) {
            Ok(document) if activity_matches_runtime(&document, &record) => {
                if let Some(observed) = document.last_event_at
                    && summary
                        .last_event_at
                        .as_ref()
                        .is_none_or(|current| timestamp_is_later(&observed, current))
                {
                    summary.last_event_at = Some(observed);
                }
            }
            Ok(_) | Err(_) => {}
        }
    }
    activity
}

pub(crate) fn setup(
    agent: AgentKind,
    action: SetupAction,
    expected_preview_digest: Option<&str>,
) -> Result<SetupResult, CliError> {
    if action == SetupAction::RepairPreview && agent != AgentKind::Codex {
        return Err(CliError::usage(
            "provider-repair-preview-unsupported",
            "--repair --dry-run is a Codex-only reviewed-plan workflow; use --dry-run or --repair separately for this provider",
            Some(json!({ "agent": agent.as_str() })),
        ));
    }
    if agent == AgentKind::Codex && action == SetupAction::Repair {
        let Some(expected) = expected_preview_digest else {
            return Err(CliError::data(
                "provider-config-preview-digest-required",
                "Codex repair requires the digest from a reviewed --repair --dry-run preview",
                None,
            ));
        };
        validate_preview_digest(expected)?;
    } else if expected_preview_digest.is_some() {
        return Err(CliError::data(
            "provider-config-preview-digest-unsupported",
            "an expected preview digest is accepted only when applying Codex repair",
            None,
        ));
    }
    let path = provider_config_path(agent)?;
    let notification_path = (agent == AgentKind::Codex)
        .then(codex_notification_config_path)
        .transpose()?;
    let configured_before = provider_configured(agent, &path)?;
    let outcome = match agent {
        AgentKind::Codex => {
            let notify_path = notification_path.as_deref().expect("Codex notify path");
            setup_codex_provider(&path, notify_path, action, expected_preview_digest)?
        }
        AgentKind::Claude => {
            let (would_change, would_configure) = setup_json_provider(agent, &path, action)?;
            ProviderSetupOutcome {
                would_change,
                would_configure,
                apply_allowed: true,
                notification_preview: None,
                preview_digest: None,
                hook_config_path: None,
                hook_representation: None,
                hook_migration: None,
                representation_conflict: None,
            }
        }
        AgentKind::Hermes => {
            let (would_change, would_configure) = setup_hermes(&path, action)?;
            ProviderSetupOutcome {
                would_change,
                would_configure,
                apply_allowed: true,
                notification_preview: None,
                preview_digest: None,
                hook_config_path: None,
                hook_representation: None,
                hook_migration: None,
                representation_conflict: None,
            }
        }
    };
    let action_name = match action {
        SetupAction::DryRun => "dry-run",
        SetupAction::RepairPreview => "repair-preview",
        SetupAction::Apply => "apply",
        SetupAction::Remove => "remove",
        SetupAction::Repair => "repair",
    };
    Ok(SetupResult {
        provider: agent.as_str().to_string(),
        action: action_name.to_string(),
        changed: !matches!(action, SetupAction::DryRun | SetupAction::RepairPreview)
            && outcome.would_change,
        would_change: outcome.would_change,
        configured: if matches!(action, SetupAction::DryRun | SetupAction::RepairPreview) {
            configured_before
        } else {
            outcome.would_configure
        },
        would_configure: outcome.would_configure,
        apply_allowed: outcome.apply_allowed,
        preview_digest: outcome.preview_digest,
        config_path: display_path(outcome.hook_config_path.as_deref().unwrap_or(&path)),
        hook_representation: outcome.hook_representation,
        hook_migration: outcome.hook_migration,
        representation_conflict: outcome.representation_conflict,
        notification_config_path: notification_path.as_deref().map(display_path),
        notification_preview: outcome.notification_preview,
        owned_events: provider_specs(agent)
            .into_iter()
            .map(|spec| spec.event.to_string())
            .chain((agent == AgentKind::Codex).then(|| "agent-turn-complete".to_string()))
            .collect(),
        trust: match agent {
            AgentKind::Codex => {
                "approve the exact new Codex hook definitions; a safe singular user notify argv is preserved through bounded direct-argv fan-out without a shell"
            }
            AgentKind::Claude => "review the additive settings entries before apply",
            AgentKind::Hermes => {
                "approve each new (event, command) pair or use Hermes' explicit hook-consent flow"
            }
        }
        .to_string(),
    })
}

fn provider_version(agent: AgentKind) -> VersionProbe {
    probe_version_command(
        crate::resolve_agent_bin(agent, None),
        Duration::from_secs(2),
    )
}

fn probe_version_command(binary: impl AsRef<std::ffi::OsStr>, timeout: Duration) -> VersionProbe {
    let mut command = ProcessCommand::new(binary);
    command
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            return VersionProbe {
                version: None,
                error: Some("unavailable".to_string()),
            };
        }
    };
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return VersionProbe {
                    version: None,
                    error: Some("timeout".to_string()),
                };
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return VersionProbe {
                    version: None,
                    error: Some("probe-failed".to_string()),
                };
            }
        }
    };
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(_) => {
            return VersionProbe {
                version: None,
                error: Some("probe-output-failed".to_string()),
            };
        }
    };
    if !output.status.success() {
        return VersionProbe {
            version: None,
            error: Some(format!("exit-{}", status.code().unwrap_or(-1))),
        };
    }
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr)
    } else {
        String::from_utf8_lossy(&output.stdout)
    };
    let version = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(160).collect());
    VersionProbe {
        error: version.is_none().then(|| "empty-output".to_string()),
        version,
    }
}

fn audited_floor(agent: AgentKind) -> (u64, u64, u64) {
    match agent {
        AgentKind::Codex => (0, 144, 1),
        AgentKind::Claude => (2, 1, 206),
        AgentKind::Hermes => (0, 18, 2),
    }
}

fn parse_version_triplet(text: &str) -> Option<(u64, u64, u64)> {
    text.split(|character: char| !(character.is_ascii_digit() || character == '.'))
        .filter(|candidate| candidate.matches('.').count() >= 2)
        .find_map(|candidate| {
            let mut parts = candidate.split('.');
            let major = parts.next()?.parse().ok()?;
            let minor = parts.next()?.parse().ok()?;
            let patch = parts.next()?.parse().ok()?;
            Some((major, minor, patch))
        })
}

fn format_version(version: (u64, u64, u64)) -> String {
    format!("{}.{}.{}", version.0, version.1, version.2)
}

fn provider_config_path(agent: AgentKind) -> Result<std::path::PathBuf, CliError> {
    let home = home_dir().ok_or_else(|| {
        CliError::runtime(
            "home-unavailable",
            "HOME is required for provider activity setup",
            None,
        )
    })?;
    Ok(match agent {
        AgentKind::Codex => home.join(".codex/hooks.json"),
        AgentKind::Claude => home.join(".claude/settings.json"),
        AgentKind::Hermes => home.join(".hermes/config.yaml"),
    })
}

fn codex_notification_config_path() -> Result<std::path::PathBuf, CliError> {
    let home = home_dir().ok_or_else(|| {
        CliError::runtime(
            "home-unavailable",
            "HOME is required for provider activity setup",
            None,
        )
    })?;
    Ok(home.join(".codex/config.toml"))
}

#[derive(Clone, Copy)]
struct ProviderSpec {
    event: &'static str,
    matcher: Option<&'static str>,
}

#[derive(Debug)]
struct ProviderConfigPlan {
    path: PathBuf,
    original_bytes: Option<Vec<u8>>,
    updated_bytes: Option<Vec<u8>>,
    changed: bool,
    configured: bool,
}

#[derive(Debug)]
struct CodexNotificationPlan {
    config: ProviderConfigPlan,
    preview: CodexNotificationPreview,
    apply_allowed: bool,
}

#[derive(Debug)]
struct ProviderSetupOutcome {
    would_change: bool,
    would_configure: bool,
    apply_allowed: bool,
    notification_preview: Option<CodexNotificationPreview>,
    preview_digest: Option<String>,
    hook_config_path: Option<PathBuf>,
    hook_representation: Option<String>,
    hook_migration: Option<String>,
    representation_conflict: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodexHookRepresentation {
    Json,
    InlineToml,
}

impl CodexHookRepresentation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::InlineToml => "inline_toml",
        }
    }
}

#[derive(Clone, Debug)]
struct CodexHookStatus {
    representation: CodexHookRepresentation,
    configured: bool,
    migration_required: bool,
    conflict: bool,
}

fn provider_specs(agent: AgentKind) -> Vec<ProviderSpec> {
    match agent {
        AgentKind::Codex => vec![
            ProviderSpec {
                event: "UserPromptSubmit",
                matcher: None,
            },
            ProviderSpec {
                event: "PermissionRequest",
                matcher: None,
            },
            ProviderSpec {
                event: "PostToolUse",
                matcher: None,
            },
            ProviderSpec {
                event: "Stop",
                matcher: None,
            },
        ],
        AgentKind::Claude => vec![
            ProviderSpec {
                event: "UserPromptSubmit",
                matcher: None,
            },
            ProviderSpec {
                event: "PermissionRequest",
                matcher: None,
            },
            ProviderSpec {
                event: "PreToolUse",
                matcher: None,
            },
            ProviderSpec {
                event: "PreToolUse",
                matcher: Some("AskUserQuestion"),
            },
            ProviderSpec {
                event: "PostToolUse",
                matcher: None,
            },
            ProviderSpec {
                event: "PostToolUse",
                matcher: Some("AskUserQuestion"),
            },
            ProviderSpec {
                event: "PostToolUseFailure",
                matcher: Some("AskUserQuestion"),
            },
            ProviderSpec {
                event: "Elicitation",
                matcher: None,
            },
            ProviderSpec {
                event: "ElicitationResult",
                matcher: None,
            },
            ProviderSpec {
                event: "Stop",
                matcher: None,
            },
            ProviderSpec {
                event: "StopFailure",
                matcher: None,
            },
            ProviderSpec {
                event: "Notification",
                matcher: Some("idle_prompt"),
            },
            ProviderSpec {
                event: "Notification",
                matcher: Some("agent_needs_input"),
            },
        ],
        AgentKind::Hermes => vec![
            ProviderSpec {
                event: "pre_llm_call",
                matcher: None,
            },
            ProviderSpec {
                event: "post_llm_call",
                matcher: None,
            },
            ProviderSpec {
                event: "pre_approval_request",
                matcher: None,
            },
            ProviderSpec {
                event: "post_approval_response",
                matcher: None,
            },
        ],
    }
}

fn retired_provider_specs(agent: AgentKind) -> Vec<ProviderSpec> {
    match agent {
        AgentKind::Claude => vec![
            ProviderSpec {
                event: "Notification",
                matcher: Some("permission_prompt"),
            },
            ProviderSpec {
                event: "SubagentStop",
                matcher: None,
            },
        ],
        AgentKind::Codex | AgentKind::Hermes => Vec::new(),
    }
}

fn owned_command(agent: AgentKind, event: Option<&str>) -> String {
    match event {
        Some("PermissionRequest") if agent == AgentKind::Codex => concat!(
            "sh -c '",
            "if [ \"${AGENT_SESSION_ATTENTION_AUTHORITY:-hook}\" = protocol ]; ",
            "then exit 0; fi; exec agent-session activity hook --agent codex'"
        )
        .to_string(),
        Some(event) if agent == AgentKind::Hermes => {
            format!("agent-session activity hook --agent hermes --event {event}")
        }
        _ => format!("agent-session activity hook --agent {}", agent.as_str()),
    }
}

fn provider_configured(agent: AgentKind, path: &Path) -> Result<bool, CliError> {
    match agent {
        AgentKind::Codex => {
            let config_path = codex_notification_config_path()?;
            let hooks = codex_hook_status(path, &config_path)?;
            let notification = codex_notification_status(&config_path)?;
            Ok(hooks.configured && notification.configured)
        }
        AgentKind::Claude => json_provider_configured(agent, path),
        AgentKind::Hermes => hermes_configured(path),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CodexNotificationStatus {
    configured: bool,
    mode: String,
}

fn codex_notification_status(path: &Path) -> Result<CodexNotificationStatus, CliError> {
    if !path.is_file() {
        return Ok(CodexNotificationStatus {
            configured: false,
            mode: "absent".to_string(),
        });
    }
    let document = parse_codex_notification_config(
        path,
        &fs::read_to_string(path)
            .map_err(|err| activity_io_error("provider-config-read-failed", path, err))?,
    )?;
    let mode = codex_notify_mode(&document);
    Ok(CodexNotificationStatus {
        configured: matches!(mode, CodexNotifyMode::Owned | CodexNotifyMode::Composed(_)),
        mode: codex_notify_mode_name(&mode).to_string(),
    })
}

fn parse_codex_notification_config(path: &Path, raw: &str) -> Result<TomlDocument, CliError> {
    raw.parse::<TomlDocument>().map_err(|err| {
        CliError::data(
            "provider-config-invalid",
            format!("failed to parse {}: {err}", path.display()),
            None,
        )
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CodexNotifyMode {
    Absent,
    Owned,
    Composed(Vec<String>),
    Foreign(Vec<String>),
    Invalid,
}

fn codex_notify_mode(document: &TomlDocument) -> CodexNotifyMode {
    let Some(item) = document.get("notify") else {
        return CodexNotifyMode::Absent;
    };
    let Some(array) = item.as_array() else {
        return CodexNotifyMode::Invalid;
    };
    let Some(argv) = array
        .iter()
        .map(|value| value.as_str().map(ToOwned::to_owned))
        .collect::<Option<Vec<_>>>()
    else {
        return CodexNotifyMode::Invalid;
    };
    if argv_matches(&argv, &CODEX_NOTIFY_ARGV) {
        return CodexNotifyMode::Owned;
    }
    if argv.len() == CODEX_NOTIFY_ARGV.len() + 2
        && argv_matches(&argv[..CODEX_NOTIFY_ARGV.len()], &CODEX_NOTIFY_ARGV)
        && argv[CODEX_NOTIFY_ARGV.len()] == CODEX_NOTIFY_FORWARD_FLAG
        && let Ok(forwarded) =
            serde_json::from_str::<Vec<String>>(&argv[CODEX_NOTIFY_ARGV.len() + 1])
        && codex_forward_argv_is_safe(&forwarded)
    {
        return CodexNotifyMode::Composed(forwarded);
    }
    CodexNotifyMode::Foreign(argv)
}

fn argv_matches(argv: &[String], expected: &[&str]) -> bool {
    argv.len() == expected.len()
        && argv
            .iter()
            .zip(expected)
            .all(|(value, expected)| value == expected)
}

fn codex_forward_argv_is_safe(argv: &[String]) -> bool {
    !argv.is_empty()
        && argv.len() <= MAX_CODEX_FORWARD_ARGS
        && !argv[0].trim().is_empty()
        && argv.iter().map(String::len).sum::<usize>() <= MAX_CODEX_FORWARD_ARGV_BYTES
        && !argv.iter().any(|value| value == CODEX_NOTIFY_FORWARD_FLAG)
        && (argv.len() < CODEX_NOTIFY_ARGV.len()
            || !argv_matches(&argv[..CODEX_NOTIFY_ARGV.len()], &CODEX_NOTIFY_ARGV))
}

fn codex_notify_mode_name(mode: &CodexNotifyMode) -> &'static str {
    match mode {
        CodexNotifyMode::Absent => "absent",
        CodexNotifyMode::Owned => "owned",
        CodexNotifyMode::Composed(_) => "composed",
        CodexNotifyMode::Foreign(_) => "conflict",
        CodexNotifyMode::Invalid => "invalid",
    }
}

fn codex_forward_argv_sha256(argv: &[String]) -> Result<String, CliError> {
    let mut encoded = serde_json::to_vec(argv).map_err(|_| {
        CliError::data(
            "provider-notification-config-invalid",
            "Codex user-owned notify argv could not be encoded for safe preview",
            None,
        )
    })?;
    // Match the operational comparison contract: compact JSON argv followed by
    // one LF, so an operator can compare this content-free digest with a
    // separately decoded composed notifier without exposing argv values.
    encoded.push(b'\n');
    let mut digest = Sha256::new();
    digest.update(encoded);
    Ok(format!("sha256:{}", hex_digest(digest.finalize())))
}

fn validate_preview_digest(value: &str) -> Result<(), CliError> {
    let valid = value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if valid {
        Ok(())
    } else {
        Err(CliError::data(
            "provider-config-preview-digest-invalid",
            "expected preview digest must use sha256 followed by 64 lowercase hexadecimal characters",
            None,
        ))
    }
}

fn update_plan_digest(digest: &mut Sha256, role: &[u8], plan: &ProviderConfigPlan) {
    digest.update(role);
    digest.update(b"\0");
    match plan.original_bytes.as_deref() {
        Some(bytes) => {
            digest.update([1]);
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
        }
        None => digest.update([0]),
    }
    match plan.updated_bytes.as_deref() {
        Some(bytes) => {
            digest.update([1]);
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
        }
        None => digest.update([0]),
    }
}

fn codex_provider_preview_digest(
    hooks: &ProviderConfigPlan,
    notification: &ProviderConfigPlan,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"agent-session.codex-provider-repair-plan.v1\0");
    update_plan_digest(&mut digest, b"hooks", hooks);
    update_plan_digest(&mut digest, b"notification", notification);
    format!("sha256:{}", hex_digest(digest.finalize()))
}

fn plan_codex_notification(
    path: &Path,
    action: SetupAction,
) -> Result<CodexNotificationPlan, CliError> {
    let original_bytes = if path.is_file() {
        Some(
            fs::read(path)
                .map_err(|err| activity_io_error("provider-config-read-failed", path, err))?,
        )
    } else {
        None
    };
    let raw = original_bytes
        .as_ref()
        .map(|bytes| {
            String::from_utf8(bytes.clone()).map_err(|_| {
                CliError::data(
                    "provider-config-invalid",
                    format!("{} is not valid UTF-8 TOML", path.display()),
                    None,
                )
            })
        })
        .transpose()?
        .unwrap_or_default();
    let mut document = if raw.is_empty() {
        TomlDocument::new()
    } else {
        parse_codex_notification_config(path, &raw)?
    };
    let mode = codex_notify_mode(&document);
    let current_mode = codex_notify_mode_name(&mode).to_string();
    let forwarded_preview = match &mode {
        CodexNotifyMode::Composed(forwarded) | CodexNotifyMode::Foreign(forwarded)
            if codex_forward_argv_is_safe(forwarded) =>
        {
            Some((forwarded.len(), codex_forward_argv_sha256(forwarded)?))
        }
        _ => None,
    };
    let mut reversible = true;
    let mut blocker_code = None;
    let mut apply_allowed = true;
    let remove = action == SetupAction::Remove;
    if remove {
        match mode {
            CodexNotifyMode::Owned => {
                document.remove("notify");
            }
            CodexNotifyMode::Composed(forwarded) => {
                let mut argv = TomlArray::new();
                argv.extend(forwarded);
                document["notify"] = toml_value(argv);
            }
            CodexNotifyMode::Absent | CodexNotifyMode::Foreign(_) | CodexNotifyMode::Invalid => {}
        }
    } else {
        match mode {
            CodexNotifyMode::Absent => {
                let mut argv = TomlArray::new();
                argv.extend(CODEX_NOTIFY_ARGV);
                document["notify"] = toml_value(argv);
            }
            CodexNotifyMode::Owned | CodexNotifyMode::Composed(_) => {}
            CodexNotifyMode::Foreign(forwarded) if codex_forward_argv_is_safe(&forwarded) => {
                let encoded = serde_json::to_string(&forwarded).map_err(|_| {
                    CliError::data(
                        "provider-notification-config-invalid",
                        "Codex user-owned notify argv could not be encoded for safe composition",
                        Some(json!({ "path": display_path(path) })),
                    )
                })?;
                if encoded.len() > MAX_CODEX_FORWARD_ARGV_BYTES {
                    return Err(CliError::data(
                        "provider-notification-config-conflict",
                        "Codex user-owned notify argv expands beyond the safe composition limit; it was preserved and activity setup made no changes",
                        Some(json!({ "path": display_path(path) })),
                    ));
                }
                let mut argv = TomlArray::new();
                argv.extend(CODEX_NOTIFY_ARGV);
                argv.push(CODEX_NOTIFY_FORWARD_FLAG);
                argv.push(encoded);
                document["notify"] = toml_value(argv);

                let mut restored = document.clone();
                let mut restored_argv = TomlArray::new();
                restored_argv.extend(forwarded);
                restored["notify"] = toml_value(restored_argv);
                if Some(restored.to_string().as_bytes()) != original_bytes.as_deref() {
                    if action == SetupAction::RepairPreview {
                        reversible = false;
                        apply_allowed = false;
                        blocker_code =
                            Some("provider-notification-config-nonreversible".to_string());
                    } else {
                        return Err(CliError::data(
                            "provider-notification-config-conflict",
                            "Codex user-owned notify argv cannot be composed with byte-exact removal; it was preserved and activity setup made no changes",
                            Some(json!({ "path": display_path(path) })),
                        ));
                    }
                }
            }
            CodexNotifyMode::Foreign(_) | CodexNotifyMode::Invalid => {
                return Err(CliError::data(
                    "provider-notification-config-conflict",
                    "Codex config has a notify command that cannot be composed safely; it was preserved and activity setup made no changes",
                    Some(json!({ "path": display_path(path) })),
                ));
            }
        }
    }
    let rendered = document.to_string().into_bytes();
    let original_rendered = original_bytes.as_deref().unwrap_or_default();
    let changed = rendered != original_rendered;
    let candidate_mode = codex_notify_mode(&document);
    let candidate_configured = matches!(
        candidate_mode,
        CodexNotifyMode::Owned | CodexNotifyMode::Composed(_)
    );
    let (forwarded_argc, forwarded_argv_sha256) = forwarded_preview
        .map(|(argc, hash)| (Some(argc), Some(hash)))
        .unwrap_or((None, None));
    Ok(CodexNotificationPlan {
        config: ProviderConfigPlan {
            path: path.to_path_buf(),
            original_bytes,
            updated_bytes: Some(rendered),
            changed,
            configured: candidate_configured,
        },
        preview: CodexNotificationPreview {
            current_mode,
            candidate_mode: codex_notify_mode_name(&candidate_mode).to_string(),
            forwarded_argc,
            forwarded_argv_sha256,
            reversible,
            blocker_code,
        },
        apply_allowed,
    })
}

const CODEX_HOOK_BLOCK_START: &str = "# >>> agent-session:codex-hooks >>>";
const CODEX_HOOK_BLOCK_END: &str = "# <<< agent-session:codex-hooks <<<";

fn toml_hook_matcher_matches(group: &toml_edit::Table, spec: ProviderSpec) -> bool {
    let matcher = group.get("matcher").and_then(TomlItem::as_str);
    match spec.matcher {
        Some(expected) => matcher == Some(expected),
        None => matcher.is_none_or(str::is_empty),
    }
}

fn toml_handler_command_is_owned(event: &str, handler: &toml_edit::Table) -> bool {
    handler.get("type").and_then(TomlItem::as_str) == Some("command")
        && handler
            .get("command")
            .and_then(TomlItem::as_str)
            .is_some_and(|command| {
                command == owned_command(AgentKind::Codex, Some(event))
                    || command == owned_command(AgentKind::Codex, None)
            })
}

fn toml_inline_has_lifecycle_hooks(document: &TomlDocument) -> bool {
    document
        .get("hooks")
        .and_then(TomlItem::as_table)
        .is_some_and(|hooks| {
            hooks.iter().any(|(_, item)| {
                item.as_array_of_tables()
                    .is_some_and(|groups| !groups.is_empty())
            })
        })
}

fn toml_has_spec(document: &TomlDocument, spec: ProviderSpec) -> bool {
    document
        .get("hooks")
        .and_then(TomlItem::as_table)
        .and_then(|hooks| hooks.get(spec.event))
        .and_then(TomlItem::as_array_of_tables)
        .is_some_and(|groups| {
            groups.iter().any(|group| {
                toml_hook_matcher_matches(group, spec)
                    && group
                        .get("hooks")
                        .and_then(TomlItem::as_array_of_tables)
                        .is_some_and(|handlers| {
                            handlers.iter().any(|handler| {
                                handler.get("type").and_then(TomlItem::as_str) == Some("command")
                                    && handler.get("command").and_then(TomlItem::as_str)
                                        == Some(
                                            owned_command(AgentKind::Codex, Some(spec.event))
                                                .as_str(),
                                        )
                                    && handler.get("timeout").and_then(TomlItem::as_integer)
                                        == Some(5)
                            })
                        })
            })
        })
}

fn toml_codex_hooks_configured(document: &TomlDocument) -> bool {
    provider_specs(AgentKind::Codex)
        .into_iter()
        .all(|spec| toml_has_spec(document, spec))
}

fn remove_owned_toml_hooks(document: &mut TomlDocument) {
    let Some(hooks) = document.get_mut("hooks").and_then(TomlItem::as_table_mut) else {
        return;
    };
    for spec in provider_specs(AgentKind::Codex) {
        let mut remove_event = false;
        if let Some(groups) = hooks
            .get_mut(spec.event)
            .and_then(TomlItem::as_array_of_tables_mut)
        {
            for group in groups.iter_mut() {
                if !toml_hook_matcher_matches(group, spec) {
                    continue;
                }
                if let Some(handlers) = group
                    .get_mut("hooks")
                    .and_then(TomlItem::as_array_of_tables_mut)
                {
                    handlers.retain(|handler| !toml_handler_command_is_owned(spec.event, handler));
                }
            }
            groups.retain(|group| {
                group
                    .get("hooks")
                    .and_then(TomlItem::as_array_of_tables)
                    .is_none_or(|handlers| !handlers.is_empty())
            });
            remove_event = groups.is_empty();
        }
        if remove_event {
            hooks.remove(spec.event);
        }
    }
    if hooks.is_empty() {
        document.remove("hooks");
    }
}

fn render_owned_codex_toml_hook_block() -> String {
    let mut block = String::from(CODEX_HOOK_BLOCK_START);
    block.push('\n');
    for spec in provider_specs(AgentKind::Codex) {
        let command = TomlValue::from(owned_command(AgentKind::Codex, Some(spec.event)));
        block.push_str(&format!(
            "[[hooks.{event}]]\n\n[[hooks.{event}.hooks]]\ntype = \"command\"\ncommand = {command}\ntimeout = 5\n\n",
            event = spec.event,
        ));
    }
    block.push_str(CODEX_HOOK_BLOCK_END);
    block.push('\n');
    block
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TomlMultilineString {
    None,
    Basic,
    Literal,
}

fn toml_multiline_value_line_starts(raw: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut state = TomlMultilineString::None;
    let mut offset = 0_usize;
    for line in raw.split_inclusive('\n') {
        if state != TomlMultilineString::None {
            starts.push(offset);
        }
        let bytes = line.as_bytes();
        let mut index = 0_usize;
        while index < bytes.len() {
            match state {
                TomlMultilineString::None => match bytes[index] {
                    b'#' => break,
                    b'"' if bytes[index..].starts_with(b"\"\"\"") => {
                        state = TomlMultilineString::Basic;
                        index += 3;
                    }
                    b'\'' if bytes[index..].starts_with(b"'''") => {
                        state = TomlMultilineString::Literal;
                        index += 3;
                    }
                    b'"' => {
                        index += 1;
                        while index < bytes.len() {
                            match bytes[index] {
                                b'\\' => index = (index + 2).min(bytes.len()),
                                b'"' => {
                                    index += 1;
                                    break;
                                }
                                _ => index += 1,
                            }
                        }
                    }
                    b'\'' => {
                        index += 1;
                        while index < bytes.len() && bytes[index] != b'\'' {
                            index += 1;
                        }
                        index = (index + 1).min(bytes.len());
                    }
                    _ => index += 1,
                },
                TomlMultilineString::Basic => {
                    if bytes[index..].starts_with(b"\"\"\"") {
                        state = TomlMultilineString::None;
                        index += 3;
                    } else if bytes[index] == b'\\' {
                        index = (index + 2).min(bytes.len());
                    } else {
                        index += 1;
                    }
                }
                TomlMultilineString::Literal => {
                    if bytes[index..].starts_with(b"'''") {
                        state = TomlMultilineString::None;
                        index += 3;
                    } else {
                        index += 1;
                    }
                }
            }
        }
        offset += line.len();
    }
    starts
}

fn strip_owned_codex_toml_hook_block(path: &Path, raw: &str) -> Result<String, CliError> {
    parse_codex_notification_config(path, raw)?;
    let multiline_value_lines = toml_multiline_value_line_starts(raw);
    let marker_lines = |marker: &str| {
        let mut offset = 0_usize;
        raw.split_inclusive('\n')
            .filter_map(|line| {
                let start = offset;
                offset += line.len();
                let without_newline = line.strip_suffix('\n').unwrap_or(line);
                let content = without_newline
                    .strip_suffix('\r')
                    .unwrap_or(without_newline);
                (content == marker && multiline_value_lines.binary_search(&start).is_err())
                    .then_some((start, offset))
            })
            .collect::<Vec<_>>()
    };
    let starts = marker_lines(CODEX_HOOK_BLOCK_START);
    let ends = marker_lines(CODEX_HOOK_BLOCK_END);
    if starts.is_empty() && ends.is_empty() {
        return Ok(raw.to_string());
    }
    if starts.len() != 1 || ends.len() != 1 || ends[0].0 < starts[0].0 {
        return Err(CliError::data(
            "provider-config-invalid",
            "Codex config has an incomplete or duplicate agent-session hook marker block",
            Some(json!({ "path": display_path(path) })),
        ));
    }
    let start = starts[0].0;
    let end = ends[0].1;
    let mut stripped = String::with_capacity(raw.len() - (end - start));
    stripped.push_str(&raw[..start]);
    stripped.push_str(&raw[end..]);
    Ok(stripped)
}

fn plan_inline_codex_hooks(
    notification: &mut CodexNotificationPlan,
    action: SetupAction,
) -> Result<bool, CliError> {
    let candidate = notification
        .config
        .updated_bytes
        .as_deref()
        .expect("notification config always has a rendered candidate");
    let raw = String::from_utf8(candidate.to_vec()).map_err(|_| {
        CliError::data(
            "provider-config-invalid",
            format!(
                "{} is not valid UTF-8 TOML",
                notification.config.path.display()
            ),
            None,
        )
    })?;
    let stripped = strip_owned_codex_toml_hook_block(&notification.config.path, &raw)?;
    let mut document = parse_codex_notification_config(&notification.config.path, &stripped)?;
    remove_owned_toml_hooks(&mut document);
    let mut rendered = document.to_string();
    if action != SetupAction::Remove {
        if !rendered.is_empty() && !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        if !rendered.is_empty() && !rendered.ends_with("\n\n") {
            rendered.push('\n');
        }
        rendered.push_str(&render_owned_codex_toml_hook_block());
    }
    let rendered_bytes = rendered.into_bytes();
    notification.config.changed =
        notification.config.original_bytes.as_deref() != Some(rendered_bytes.as_slice());
    notification.config.updated_bytes = Some(rendered_bytes.clone());
    let candidate_document = parse_codex_notification_config(
        &notification.config.path,
        std::str::from_utf8(&rendered_bytes).expect("rendered TOML is UTF-8"),
    )?;
    Ok(action != SetupAction::Remove && toml_codex_hooks_configured(&candidate_document))
}

fn json_handler_is_owned_for_group(event: &str, group: &Value, handler: &Value) -> bool {
    let Some(spec) = provider_specs(AgentKind::Codex).into_iter().find(|spec| {
        spec.event == event && group.get("matcher").and_then(Value::as_str) == spec.matcher
    }) else {
        return false;
    };
    if handler.get("type").and_then(Value::as_str) != Some("command") {
        return false;
    }
    let Some(command) = handler.get("command").and_then(Value::as_str) else {
        return false;
    };
    command == owned_command(AgentKind::Codex, Some(spec.event))
        || command == owned_command(AgentKind::Codex, None)
}

fn json_has_owned_codex_hooks(value: &Value) -> bool {
    value
        .get("hooks")
        .and_then(Value::as_object)
        .is_some_and(|hooks| {
            hooks.iter().any(|(event, groups)| {
                groups.as_array().is_some_and(|groups| {
                    groups.iter().any(|group| {
                        group
                            .get("hooks")
                            .and_then(Value::as_array)
                            .is_some_and(|handlers| {
                                handlers.iter().any(|handler| {
                                    json_handler_is_owned_for_group(event, group, handler)
                                })
                            })
                    })
                })
            })
        })
}

fn json_has_non_owned_lifecycle_hooks(value: &Value) -> bool {
    value
        .get("hooks")
        .and_then(Value::as_object)
        .is_some_and(|hooks| {
            hooks.iter().any(|(event, groups)| {
                groups.as_array().is_none_or(|groups| {
                    groups.iter().any(|group| {
                        group
                            .get("hooks")
                            .and_then(Value::as_array)
                            .is_none_or(|handlers| {
                                handlers.is_empty()
                                    || handlers.iter().any(|handler| {
                                        !json_handler_is_owned_for_group(event, group, handler)
                                    })
                            })
                    })
                })
            })
        })
}

fn plan_codex_json_cleanup(
    path: &Path,
    original_bytes: Option<Vec<u8>>,
) -> Result<ProviderConfigPlan, CliError> {
    let mut plan = plan_json_provider_from_original(
        AgentKind::Codex,
        path,
        SetupAction::Remove,
        original_bytes,
    )?;
    let candidate = plan
        .updated_bytes
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        .expect("JSON provider plan always renders an object");
    if candidate.as_object().is_some_and(Map::is_empty) {
        plan.changed = plan.original_bytes.is_some();
        plan.updated_bytes = None;
    }
    plan.configured = true;
    Ok(plan)
}

fn codex_hook_status_from_documents(json: &Value, config: &TomlDocument) -> CodexHookStatus {
    let inline_active = toml_inline_has_lifecycle_hooks(config);
    let representation = if inline_active {
        CodexHookRepresentation::InlineToml
    } else {
        CodexHookRepresentation::Json
    };
    let conflict = inline_active && json_has_non_owned_lifecycle_hooks(json);
    let migration_required = inline_active && json_has_owned_codex_hooks(json);
    let configured = !conflict
        && match representation {
            CodexHookRepresentation::Json => provider_specs(AgentKind::Codex)
                .into_iter()
                .all(|spec| json_has_spec(json, AgentKind::Codex, spec)),
            CodexHookRepresentation::InlineToml => toml_codex_hooks_configured(config),
        };
    CodexHookStatus {
        representation,
        configured,
        migration_required,
        conflict,
    }
}

fn codex_hook_status(json_path: &Path, config_path: &Path) -> Result<CodexHookStatus, CliError> {
    let json_bytes = read_optional_provider_config(json_path)?;
    let json = parse_json_provider_config(json_path, json_bytes.as_deref())?;
    let config_raw = read_optional_provider_config(config_path)?
        .map(|bytes| {
            String::from_utf8(bytes).map_err(|_| {
                CliError::data(
                    "provider-config-invalid",
                    format!("{} is not valid UTF-8 TOML", config_path.display()),
                    None,
                )
            })
        })
        .transpose()?
        .unwrap_or_default();
    let config = if config_raw.is_empty() {
        TomlDocument::new()
    } else {
        parse_codex_notification_config(config_path, &config_raw)?
    };
    Ok(codex_hook_status_from_documents(&json, &config))
}

fn setup_codex_provider(
    hooks_path: &Path,
    notification_path: &Path,
    action: SetupAction,
    expected_preview_digest: Option<&str>,
) -> Result<ProviderSetupOutcome, CliError> {
    // Parse and render both files before either can change. This makes invalid
    // or conflicting notification configuration a preflight failure for apply,
    // repair, and remove alike.
    let mut notification = plan_codex_notification(notification_path, action)?;
    let json_original = read_optional_provider_config(hooks_path)?;
    let json = parse_json_provider_config(hooks_path, json_original.as_deref())?;
    let config_raw = notification
        .config
        .original_bytes
        .as_deref()
        .map(|bytes| {
            std::str::from_utf8(bytes).map_err(|_| {
                CliError::data(
                    "provider-config-invalid",
                    format!("{} is not valid UTF-8 TOML", notification_path.display()),
                    None,
                )
            })
        })
        .transpose()?
        .unwrap_or_default();
    let config = if config_raw.is_empty() {
        TomlDocument::new()
    } else {
        parse_codex_notification_config(notification_path, config_raw)?
    };
    let status = codex_hook_status_from_documents(&json, &config);
    if status.conflict && action != SetupAction::Remove {
        return Err(CliError::data(
            "provider-hook-representation-conflict",
            "Codex has non-agent-session lifecycle hooks in both hooks.json and config.toml; no files were changed because moving user-owned hooks requires a manual representation decision",
            Some(json!({
                "hooks_path": display_path(hooks_path),
                "config_path": display_path(notification_path),
                "next": "converge user-owned lifecycle hooks onto one Codex representation, then review a fresh repair preview"
            })),
        ));
    }
    let migration = if status.migration_required && action != SetupAction::Remove {
        "json_to_inline_toml"
    } else {
        "not_needed"
    };
    let (hooks, hooks_configured, hook_config_path) = match status.representation {
        CodexHookRepresentation::Json => {
            let hooks = plan_json_provider_from_original(
                AgentKind::Codex,
                hooks_path,
                action,
                json_original,
            )?;
            let configured = hooks.configured;
            (hooks, configured, hooks_path.to_path_buf())
        }
        CodexHookRepresentation::InlineToml => {
            let hooks = plan_codex_json_cleanup(hooks_path, json_original)?;
            let configured = plan_inline_codex_hooks(&mut notification, action)?;
            (hooks, configured, notification_path.to_path_buf())
        }
    };
    let plan_digest = codex_provider_preview_digest(&hooks, &notification.config);
    let changed = hooks.changed || notification.config.changed;
    let configured =
        hooks_configured && notification.config.configured && notification.apply_allowed;
    let preview = (action == SetupAction::RepairPreview).then(|| notification.preview.clone());
    if matches!(action, SetupAction::DryRun | SetupAction::RepairPreview) {
        return Ok(ProviderSetupOutcome {
            would_change: changed,
            would_configure: configured,
            apply_allowed: notification.apply_allowed,
            notification_preview: preview,
            preview_digest: (action == SetupAction::RepairPreview).then_some(plan_digest),
            hook_config_path: Some(hook_config_path),
            hook_representation: Some(status.representation.as_str().to_string()),
            hook_migration: Some(migration.to_string()),
            representation_conflict: Some(status.conflict),
        });
    }
    if action == SetupAction::Repair && expected_preview_digest != Some(plan_digest.as_str()) {
        return Err(CliError::data(
            "provider-config-preview-digest-mismatch",
            "provider configuration changed after preview; review a fresh repair preview before retrying",
            None,
        ));
    }

    apply_codex_provider_plans(&hooks, &notification.config)?;
    Ok(ProviderSetupOutcome {
        would_change: changed,
        would_configure: configured,
        apply_allowed: true,
        notification_preview: None,
        preview_digest: None,
        hook_config_path: Some(hook_config_path),
        hook_representation: Some(status.representation.as_str().to_string()),
        hook_migration: Some(migration.to_string()),
        representation_conflict: Some(status.conflict),
    })
}

fn apply_codex_provider_plans(
    hooks: &ProviderConfigPlan,
    notification: &ProviderConfigPlan,
) -> Result<(), CliError> {
    apply_codex_provider_plans_with_rollback(hooks, notification, rollback_provider_config_plan)
}

fn apply_codex_provider_plans_with_rollback(
    hooks: &ProviderConfigPlan,
    notification: &ProviderConfigPlan,
    rollback: impl FnOnce(&ProviderConfigPlan) -> Result<(), CliError>,
) -> Result<(), CliError> {
    apply_provider_config_plan(hooks)?;
    if let Err(apply_error) = apply_provider_config_plan(notification) {
        if hooks.changed
            && let Err(rollback_error) = rollback(hooks)
        {
            return Err(CliError::runtime(
                "provider-config-rollback-failed",
                "Codex lifecycle setup could not apply both configuration files and could not restore the first file; inspect both paths before retrying",
                Some(json!({
                    "apply_error": apply_error.code(),
                    "rollback_error": rollback_error.code(),
                    "rollback_error_details": rollback_error.0.details.clone(),
                    "config_path": display_path(&hooks.path),
                    "notification_config_path": display_path(&notification.path)
                })),
            ));
        }
        return Err(apply_error);
    }
    Ok(())
}

fn apply_provider_config_plan(plan: &ProviderConfigPlan) -> Result<(), CliError> {
    if !plan.changed {
        let current = match fs::read(&plan.path) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(activity_io_error(
                    "provider-config-read-failed",
                    &plan.path,
                    err,
                ));
            }
        };
        if current != plan.original_bytes {
            return Err(CliError::runtime(
                "provider-config-concurrent-modification",
                "provider config changed while activity setup was preparing an update; retry after reviewing the newer file",
                Some(json!({ "path": display_path(&plan.path) })),
            ));
        }
        return Ok(());
    }
    match plan.updated_bytes.as_deref() {
        Some(updated) => {
            write_provider_config_if_unchanged(&plan.path, updated, plan.original_bytes.as_deref())
        }
        None => remove_provider_config_if_unchanged(&plan.path, plan.original_bytes.as_deref()),
    }
}

fn rollback_provider_config_plan(plan: &ProviderConfigPlan) -> Result<(), CliError> {
    rollback_provider_config_plan_after_capture(plan, || {})
}

fn rollback_provider_config_plan_after_capture(
    plan: &ProviderConfigPlan,
    after_capture: impl FnOnce(),
) -> Result<(), CliError> {
    rollback_provider_config_plan_after_capture_with_restore(
        plan,
        after_capture,
        restore_provider_config_if_absent,
    )
}

fn rollback_provider_config_plan_after_capture_with_restore(
    plan: &ProviderConfigPlan,
    after_capture: impl FnOnce(),
    restore: impl FnOnce(&Path, &[u8], &str) -> Result<(), CliError>,
) -> Result<(), CliError> {
    if !plan.changed {
        return Ok(());
    }
    let mut after_capture = Some(after_capture);
    let mut restore = Some(restore);
    match plan.updated_bytes.as_deref() {
        Some(updated) => {
            let (quarantine, captured) =
                quarantine_provider_config(&plan.path, "rollback-capture")?;
            after_capture.take().expect("single rollback hook")();
            if captured != updated {
                restore_quarantined_provider_config(&plan.path, &quarantine)?;
                return Err(provider_config_concurrent_error(
                    "provider-config-rollback-concurrent-modification",
                    "provider config changed after activity setup wrote it; refusing to overwrite the newer file during rollback",
                    &plan.path,
                ));
            }
            let restore_result = match plan.original_bytes.as_deref() {
                Some(original) => restore.take().expect("single restore hook")(
                    &plan.path,
                    original,
                    "rollback-restore",
                ),
                None if provider_config_path_exists(&plan.path)? => {
                    Err(provider_config_concurrent_error(
                        "provider-config-rollback-concurrent-modification",
                        "provider config changed while activity setup was rolling back; the newer file was preserved",
                        &plan.path,
                    ))
                }
                None => Ok(()),
            };
            match restore_result {
                Ok(()) => fs::remove_file(&quarantine).map_err(|err| {
                    activity_io_error("provider-config-rollback-remove-failed", &quarantine, err)
                }),
                Err(error) => Err(provider_config_recovery_error(
                    error,
                    &plan.path,
                    &quarantine,
                )),
            }
        }
        None => {
            after_capture.take().expect("single rollback hook")();
            let Some(original) = plan.original_bytes.as_deref() else {
                return Ok(());
            };
            restore.take().expect("single restore hook")(&plan.path, original, "rollback-restore")
        }
    }
}

fn provider_config_recovery_error(
    mut error: CliError,
    path: &Path,
    recovery_path: &Path,
) -> CliError {
    let details = error.0.details.get_or_insert_with(|| json!({}));
    if let Some(details) = details.as_object_mut() {
        details.insert("path".to_string(), json!(display_path(path)));
        details.insert(
            "recovery_path".to_string(),
            json!(display_path(recovery_path)),
        );
    }
    error.0.message.push_str(&format!(
        "; recovery bytes were preserved at {}",
        recovery_path.display()
    ));
    error
}

fn remove_provider_config_if_unchanged(
    path: &Path,
    expected: Option<&[u8]>,
) -> Result<(), CliError> {
    remove_provider_config_if_unchanged_after_capture(path, expected, || {})
}

fn remove_provider_config_if_unchanged_after_capture(
    path: &Path,
    expected: Option<&[u8]>,
    after_capture: impl FnOnce(),
) -> Result<(), CliError> {
    let Some(expected) = expected else {
        return if provider_config_path_exists(path)? {
            Err(provider_config_concurrent_error(
                "provider-config-concurrent-modification",
                "provider config appeared while activity setup was preparing its deletion; the newer file was preserved",
                path,
            ))
        } else {
            Ok(())
        };
    };
    let (quarantine, captured) = quarantine_provider_config(path, "delete-capture")?;
    after_capture();
    if captured != expected {
        restore_quarantined_provider_config(path, &quarantine)?;
        return Err(provider_config_concurrent_error(
            "provider-config-concurrent-modification",
            "provider config changed while activity setup was preparing an update; retry after reviewing the newer file",
            path,
        ));
    }
    if provider_config_path_exists(path)? {
        fs::remove_file(&quarantine)
            .map_err(|err| activity_io_error("provider-config-remove-failed", &quarantine, err))?;
        return Err(provider_config_concurrent_error(
            "provider-config-concurrent-modification",
            "provider config was replaced while activity setup was deleting the reviewed file; the replacement was preserved",
            path,
        ));
    }
    fs::remove_file(&quarantine)
        .map_err(|err| activity_io_error("provider-config-remove-failed", &quarantine, err))
}

fn provider_config_concurrent_error(code: &str, message: &str, path: &Path) -> CliError {
    CliError::runtime(code, message, Some(json!({ "path": display_path(path) })))
}

fn provider_config_path_exists(path: &Path) -> Result<bool, CliError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(activity_io_error("provider-config-read-failed", path, err)),
    }
}

fn reserve_provider_config_temp(
    path: &Path,
    purpose: &str,
) -> Result<(PathBuf, fs::File), CliError> {
    let parent = path.parent().ok_or_else(|| {
        CliError::runtime(
            "provider-config-temp-failed",
            "provider config path has no parent directory for a transactional update",
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("provider-config");
    for _ in 0..8 {
        let candidate = parent.join(format!(
            ".{name}.agent-session-{purpose}-{}",
            uuid::Uuid::new_v4()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(SECRET_FILE_MODE)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(activity_io_error(
                    "provider-config-temp-failed",
                    &candidate,
                    err,
                ));
            }
        }
    }
    Err(CliError::runtime(
        "provider-config-temp-failed",
        "could not reserve a unique same-directory provider config transaction file",
        Some(json!({ "path": display_path(path) })),
    ))
}

fn quarantine_provider_config(path: &Path, purpose: &str) -> Result<(PathBuf, Vec<u8>), CliError> {
    let (quarantine, reservation) = reserve_provider_config_temp(path, purpose)?;
    drop(reservation);
    if let Err(err) = fs::rename(path, &quarantine) {
        let _ = fs::remove_file(&quarantine);
        return Err(if err.kind() == io::ErrorKind::NotFound {
            provider_config_concurrent_error(
                "provider-config-concurrent-modification",
                "provider config disappeared during a transactional update",
                path,
            )
        } else {
            activity_io_error("provider-config-quarantine-failed", path, err)
        });
    }
    match fs::read(&quarantine) {
        Ok(bytes) => Ok((quarantine, bytes)),
        Err(err) => {
            let _ = restore_quarantined_provider_config(path, &quarantine);
            Err(activity_io_error(
                "provider-config-quarantine-read-failed",
                &quarantine,
                err,
            ))
        }
    }
}

fn restore_quarantined_provider_config(path: &Path, quarantine: &Path) -> Result<(), CliError> {
    match fs::hard_link(quarantine, path) {
        Ok(()) => fs::remove_file(quarantine).map_err(|err| {
            activity_io_error("provider-config-quarantine-remove-failed", quarantine, err)
        }),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Err(CliError::runtime(
            "provider-config-rollback-concurrent-modification",
            "provider config changed during transactional recovery; the newer path and captured file were both preserved",
            Some(json!({
                "path": display_path(path),
                "captured_path": display_path(quarantine)
            })),
        )),
        Err(err) => Err(activity_io_error(
            "provider-config-quarantine-restore-failed",
            path,
            err,
        )),
    }
}

fn restore_provider_config_if_absent(
    path: &Path,
    bytes: &[u8],
    purpose: &str,
) -> Result<(), CliError> {
    restore_provider_config_if_absent_with_link(path, bytes, purpose, |source, target| {
        fs::hard_link(source, target)
    })
}

fn restore_provider_config_if_absent_with_link(
    path: &Path,
    bytes: &[u8],
    purpose: &str,
    link: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> Result<(), CliError> {
    let (temporary, mut file) = reserve_provider_config_temp(path, purpose)?;
    let staged = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|err| activity_io_error("provider-config-rollback-write-failed", &temporary, err));
    drop(file);
    if let Err(err) = staged {
        let _ = fs::remove_file(&temporary);
        return Err(err);
    }
    match link(&temporary, path) {
        Ok(()) => fs::remove_file(&temporary).map_err(|err| {
            activity_io_error("provider-config-rollback-remove-failed", &temporary, err)
        }),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            Err(provider_config_recovery_error(
                provider_config_concurrent_error(
                    "provider-config-rollback-concurrent-modification",
                    "provider config changed during rollback; the newer file was preserved",
                    path,
                ),
                path,
                &temporary,
            ))
        }
        Err(err) => Err(provider_config_recovery_error(
            activity_io_error("provider-config-rollback-write-failed", path, err),
            path,
            &temporary,
        )),
    }
}

fn json_provider_configured(agent: AgentKind, path: &Path) -> Result<bool, CliError> {
    if !path.is_file() {
        return Ok(false);
    }
    let value: Value = serde_json::from_slice(
        &fs::read(path)
            .map_err(|err| activity_io_error("provider-config-read-failed", path, err))?,
    )
    .map_err(|err| {
        CliError::data(
            "provider-config-invalid",
            format!("failed to parse {}: {err}", path.display()),
            None,
        )
    })?;
    Ok(provider_specs(agent)
        .iter()
        .all(|spec| json_has_spec(&value, agent, *spec))
        && retired_provider_specs(agent)
            .iter()
            .all(|spec| !json_has_spec(&value, agent, *spec)))
}

pub(crate) fn codex_protocol_attention_source_guard_configured() -> bool {
    let Ok(json_path) = provider_config_path(AgentKind::Codex) else {
        return false;
    };
    let Ok(config_path) = codex_notification_config_path() else {
        return false;
    };
    let Ok(status) = codex_hook_status(&json_path, &config_path) else {
        return false;
    };
    if status.conflict {
        return false;
    }
    match status.representation {
        CodexHookRepresentation::Json => codex_json_permission_source_guard(&json_path),
        CodexHookRepresentation::InlineToml => codex_toml_permission_source_guard(&config_path),
    }
}

fn codex_json_permission_source_guard(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let expected = owned_command(AgentKind::Codex, Some("PermissionRequest"));
    let Some(groups) = value
        .get("hooks")
        .and_then(|hooks| hooks.get("PermissionRequest"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    let mut guarded = 0_usize;
    for group in groups {
        let matcher = group.get("matcher").and_then(Value::as_str);
        let Some(handlers) = group.get("hooks").and_then(Value::as_array) else {
            return false;
        };
        for handler in handlers {
            if handler.get("type").and_then(Value::as_str) != Some("command") {
                continue;
            }
            let Some(command) = handler.get("command").and_then(Value::as_str) else {
                return false;
            };
            if !codex_permission_reporter_command(command) {
                continue;
            }
            if matcher.is_none()
                && command == expected
                && handler.get("timeout").and_then(Value::as_u64) == Some(5)
            {
                guarded += 1;
            } else {
                // A second direct reporter can bypass the runtime authority
                // guard even when the owned handler is also installed.
                return false;
            }
        }
    }
    guarded == 1
}

fn codex_toml_permission_source_guard(path: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(document) = parse_codex_notification_config(path, &raw) else {
        return false;
    };
    let expected = owned_command(AgentKind::Codex, Some("PermissionRequest"));
    let Some(groups) = document
        .get("hooks")
        .and_then(TomlItem::as_table)
        .and_then(|hooks| hooks.get("PermissionRequest"))
        .and_then(TomlItem::as_array_of_tables)
    else {
        return false;
    };
    let mut guarded = 0_usize;
    for group in groups.iter() {
        let matcher = group.get("matcher").and_then(TomlItem::as_str);
        let Some(handlers) = group.get("hooks").and_then(TomlItem::as_array_of_tables) else {
            return false;
        };
        for handler in handlers.iter() {
            if handler.get("type").and_then(TomlItem::as_str) != Some("command") {
                continue;
            }
            let Some(command) = handler.get("command").and_then(TomlItem::as_str) else {
                return false;
            };
            if !codex_permission_reporter_command(command) {
                continue;
            }
            if matcher.is_none_or(str::is_empty)
                && command == expected
                && handler.get("timeout").and_then(TomlItem::as_integer) == Some(5)
            {
                guarded += 1;
            } else {
                return false;
            }
        }
    }
    guarded == 1
}

fn codex_permission_reporter_command(command: &str) -> bool {
    let words = command
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    words
        .windows(3)
        .any(|words| words == ["agent-session", "activity", "hook"])
}

fn setup_json_provider(
    agent: AgentKind,
    path: &Path,
    action: SetupAction,
) -> Result<(bool, bool), CliError> {
    let plan = plan_json_provider(agent, path, action)?;
    if !matches!(action, SetupAction::DryRun | SetupAction::RepairPreview) {
        apply_provider_config_plan(&plan)?;
    }
    Ok((plan.changed, plan.configured))
}

fn plan_json_provider(
    agent: AgentKind,
    path: &Path,
    action: SetupAction,
) -> Result<ProviderConfigPlan, CliError> {
    let original_bytes = read_optional_provider_config(path)?;
    plan_json_provider_from_original(agent, path, action, original_bytes)
}

fn read_optional_provider_config(path: &Path) -> Result<Option<Vec<u8>>, CliError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(activity_io_error("provider-config-read-failed", path, err)),
    }
}

fn parse_json_provider_config(
    path: &Path,
    original_bytes: Option<&[u8]>,
) -> Result<Value, CliError> {
    let original = if let Some(bytes) = original_bytes {
        serde_json::from_slice::<Value>(bytes).map_err(|err| {
            CliError::data(
                "provider-config-invalid",
                format!("failed to parse {}: {err}", path.display()),
                None,
            )
        })?
    } else {
        Value::Object(Map::new())
    };
    if !original.is_object() {
        return Err(CliError::data(
            "provider-config-invalid",
            "provider config root must be an object",
            Some(json!({ "path": display_path(path) })),
        ));
    }
    Ok(original)
}

fn plan_json_provider_from_original(
    agent: AgentKind,
    path: &Path,
    action: SetupAction,
    original_bytes: Option<Vec<u8>>,
) -> Result<ProviderConfigPlan, CliError> {
    let original = parse_json_provider_config(path, original_bytes.as_deref())?;
    let mut updated = original.clone();
    let remove = action == SetupAction::Remove;
    for spec in retired_provider_specs(agent) {
        remove_retired_json_spec(&mut updated, agent, spec)?;
    }
    for spec in provider_specs(agent) {
        mutate_json_spec(&mut updated, agent, spec, remove)?;
    }
    let changed = updated != original;
    let updated_bytes = serde_json::to_vec_pretty(&updated).map_err(|err| {
        CliError::runtime(
            "provider-config-render-failed",
            format!("failed to render provider config: {err}"),
            None,
        )
    })?;
    let configured = provider_specs(agent)
        .iter()
        .all(|spec| json_has_spec(&updated, agent, *spec))
        && retired_provider_specs(agent)
            .iter()
            .all(|spec| !json_has_spec(&updated, agent, *spec));
    Ok(ProviderConfigPlan {
        path: path.to_path_buf(),
        original_bytes,
        updated_bytes: Some(updated_bytes),
        changed,
        configured,
    })
}

fn json_has_spec(value: &Value, agent: AgentKind, spec: ProviderSpec) -> bool {
    value
        .get("hooks")
        .and_then(|hooks| hooks.get(spec.event))
        .and_then(Value::as_array)
        .is_some_and(|groups| {
            groups.iter().any(|group| {
                group.get("matcher").and_then(Value::as_str) == spec.matcher
                    && group
                        .get("hooks")
                        .and_then(Value::as_array)
                        .is_some_and(|handlers| {
                            handlers.iter().any(|handler| {
                                handler.get("type").and_then(Value::as_str) == Some("command")
                                    && handler.get("command").and_then(Value::as_str)
                                        == Some(owned_command(agent, Some(spec.event)).as_str())
                                    && handler.get("timeout").and_then(Value::as_u64) == Some(5)
                            })
                        })
            })
        })
}

fn mutate_json_spec(
    root: &mut Value,
    agent: AgentKind,
    spec: ProviderSpec,
    remove: bool,
) -> Result<(), CliError> {
    let root = root.as_object_mut().expect("validated object");
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(hooks) = hooks.as_object_mut() else {
        return Err(CliError::data(
            "provider-config-invalid",
            "provider hooks must be an object",
            None,
        ));
    };
    let groups = hooks
        .entry(spec.event)
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(groups) = groups.as_array_mut() else {
        return Err(CliError::data(
            "provider-config-invalid",
            format!("provider hook event {} must be an array", spec.event),
            None,
        ));
    };
    let command = owned_command(agent, Some(spec.event));
    let legacy_command = owned_command(agent, None);
    for group in groups.iter_mut() {
        if group.get("matcher").and_then(Value::as_str) != spec.matcher {
            continue;
        }
        if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
            handlers.retain(|handler| {
                !(handler.get("type").and_then(Value::as_str) == Some("command")
                    && handler
                        .get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|candidate| {
                            candidate == command || candidate == legacy_command
                        }))
            });
        }
    }
    groups.retain(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .is_none_or(|handlers| !handlers.is_empty())
    });
    if !remove {
        let mut group = Map::new();
        if let Some(matcher) = spec.matcher {
            group.insert("matcher".to_string(), Value::String(matcher.to_string()));
        }
        group.insert(
            "hooks".to_string(),
            json!([{ "type": "command", "command": command, "timeout": 5 }]),
        );
        groups.push(Value::Object(group));
    }
    if groups.is_empty() {
        hooks.remove(spec.event);
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
    Ok(())
}

fn remove_retired_json_spec(
    root: &mut Value,
    agent: AgentKind,
    spec: ProviderSpec,
) -> Result<(), CliError> {
    let root = root.as_object_mut().expect("validated object");
    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(());
    };
    let Some(hooks) = hooks.as_object_mut() else {
        return Err(CliError::data(
            "provider-config-invalid",
            "provider hooks must be an object",
            None,
        ));
    };
    let Some(groups) = hooks.get_mut(spec.event) else {
        return Ok(());
    };
    let Some(groups) = groups.as_array_mut() else {
        return Err(CliError::data(
            "provider-config-invalid",
            format!("provider hook event {} must be an array", spec.event),
            None,
        ));
    };
    let command = owned_command(agent, Some(spec.event));
    for group in groups.iter_mut() {
        if group.get("matcher").and_then(Value::as_str) != spec.matcher {
            continue;
        }
        if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
            handlers.retain(|handler| {
                !(handler.get("type").and_then(Value::as_str) == Some("command")
                    && handler.get("command").and_then(Value::as_str) == Some(command.as_str())
                    && handler.get("timeout").and_then(Value::as_u64) == Some(5))
            });
        }
    }
    groups.retain(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .is_none_or(|handlers| !handlers.is_empty())
    });
    if groups.is_empty() {
        hooks.remove(spec.event);
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
    Ok(())
}

fn hermes_configured(path: &Path) -> Result<bool, CliError> {
    if !path.is_file() {
        return Ok(false);
    }
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_slice(
        &fs::read(path)
            .map_err(|err| activity_io_error("provider-config-read-failed", path, err))?,
    )
    .map_err(|err| {
        CliError::data(
            "provider-config-invalid",
            format!("failed to parse {}: {err}", path.display()),
            None,
        )
    })?;
    Ok(provider_specs(AgentKind::Hermes)
        .iter()
        .all(|spec| yaml_has_spec(&value, *spec)))
}

fn yaml_has_spec(value: &serde_yaml_ng::Value, spec: ProviderSpec) -> bool {
    let key = serde_yaml_ng::Value::String(spec.event.to_string());
    value
        .get("hooks")
        .and_then(|hooks| hooks.get(&key))
        .and_then(serde_yaml_ng::Value::as_sequence)
        .is_some_and(|handlers| {
            handlers.iter().any(|handler| {
                handler
                    .get("command")
                    .and_then(serde_yaml_ng::Value::as_str)
                    == Some(owned_command(AgentKind::Hermes, Some(spec.event)).as_str())
                    && handler
                        .get("timeout")
                        .and_then(serde_yaml_ng::Value::as_i64)
                        == Some(5)
            })
        })
}

fn setup_hermes(path: &Path, action: SetupAction) -> Result<(bool, bool), CliError> {
    let original_bytes = if path.is_file() {
        Some(
            fs::read(path)
                .map_err(|err| activity_io_error("provider-config-read-failed", path, err))?,
        )
    } else {
        None
    };
    let original = if let Some(bytes) = original_bytes.as_deref() {
        serde_yaml_ng::from_slice::<serde_yaml_ng::Value>(bytes).map_err(|err| {
            CliError::data(
                "provider-config-invalid",
                format!("failed to parse {}: {err}", path.display()),
                None,
            )
        })?
    } else {
        serde_yaml_ng::Value::Mapping(Default::default())
    };
    let mut updated = original.clone();
    let Some(root) = updated.as_mapping_mut() else {
        return Err(CliError::data(
            "provider-config-invalid",
            "Hermes config root must be a mapping",
            None,
        ));
    };
    let hooks_key = serde_yaml_ng::Value::String("hooks".to_string());
    if !root.contains_key(&hooks_key) {
        root.insert(
            hooks_key.clone(),
            serde_yaml_ng::Value::Mapping(Default::default()),
        );
    }
    let hooks = root
        .get_mut(&hooks_key)
        .and_then(serde_yaml_ng::Value::as_mapping_mut)
        .ok_or_else(|| {
            CliError::data(
                "provider-config-invalid",
                "Hermes hooks must be a mapping",
                None,
            )
        })?;
    let remove = action == SetupAction::Remove;
    for spec in provider_specs(AgentKind::Hermes) {
        let key = serde_yaml_ng::Value::String(spec.event.to_string());
        if !hooks.contains_key(&key) {
            hooks.insert(key.clone(), serde_yaml_ng::Value::Sequence(Vec::new()));
        }
        let handlers = hooks
            .get_mut(&key)
            .and_then(serde_yaml_ng::Value::as_sequence_mut)
            .ok_or_else(|| {
                CliError::data(
                    "provider-config-invalid",
                    format!("Hermes hook {} must be a sequence", spec.event),
                    None,
                )
            })?;
        let command = owned_command(AgentKind::Hermes, Some(spec.event));
        handlers.retain(|handler| {
            handler
                .get("command")
                .and_then(serde_yaml_ng::Value::as_str)
                != Some(command.as_str())
        });
        if !remove {
            let mut handler = serde_yaml_ng::Mapping::new();
            handler.insert(
                serde_yaml_ng::Value::String("command".to_string()),
                serde_yaml_ng::Value::String(command),
            );
            handler.insert(
                serde_yaml_ng::Value::String("timeout".to_string()),
                serde_yaml_ng::Value::Number(5.into()),
            );
            handlers.push(serde_yaml_ng::Value::Mapping(handler));
        }
        if handlers.is_empty() {
            hooks.remove(&key);
        }
    }
    if hooks.is_empty() {
        root.remove(&hooks_key);
    }
    let changed = updated != original;
    if changed && !matches!(action, SetupAction::DryRun | SetupAction::RepairPreview) {
        let rendered = serde_yaml_ng::to_string(&updated).map_err(|err| {
            CliError::runtime(
                "provider-config-render-failed",
                format!("failed to render Hermes config: {err}"),
                None,
            )
        })?;
        write_provider_config_if_unchanged(path, rendered.as_bytes(), original_bytes.as_deref())?;
    }
    let configured = provider_specs(AgentKind::Hermes)
        .iter()
        .all(|spec| yaml_has_spec(&updated, *spec));
    Ok((changed, configured))
}

fn write_provider_config_if_unchanged(
    path: &Path,
    bytes: &[u8],
    expected_original: Option<&[u8]>,
) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| activity_io_error("provider-config-dir-failed", parent, err))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|err| {
            activity_io_error("provider-config-dir-permission-failed", parent, err)
        })?;
    }
    let current = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(activity_io_error("provider-config-read-failed", path, err)),
    };
    if current.as_deref() != expected_original {
        return Err(CliError::runtime(
            "provider-config-concurrent-modification",
            "provider config changed while activity setup was preparing an update; retry after reviewing the newer file",
            Some(json!({ "path": display_path(path) })),
        ));
    }
    write_atomic(path, bytes, SECRET_FILE_MODE).map_err(|err| {
        CliError::runtime(
            "provider-config-write-failed",
            format!("failed to write provider config {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CliContext, ProviderResume, RecordRequest, create_record, write_session_record};
    use pretty_assertions::assert_eq;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Barrier};

    fn event(kind: TurnEventKind, event_id: &str) -> TurnEvent {
        TurnEvent {
            schema_version: TURN_EVENT_VERSION.to_string(),
            event_id: event_id.to_string(),
            runtime_id: "runtime-1".to_string(),
            provider: "codex".to_string(),
            provider_session_id: Some("session-1".to_string()),
            provider_turn_id: Some("turn-1".to_string()),
            kind,
            failure_reason: None,
            attention_id: None,
            attention_kind: None,
            attention_correlation_ambiguous: false,
            confidence: Confidence::Observed,
            source_kind: SourceKind::ProviderHook,
            provider_time: None,
        }
    }

    fn document() -> ActivityDocument {
        ActivityDocument {
            schema_version: ACTIVITY_DOCUMENT_VERSION.to_string(),
            runtime_id: "runtime-1".to_string(),
            runtime_generation: 1,
            state: starting_state("2026-07-10T00:00:00Z".to_string(), 1, None),
            pending_attention: Vec::new(),
            overflow_attention: None,
            seen_event_count: 0,
            last_semantic_event: None,
            last_semantic_event_at: None,
            provider_session_id: None,
            last_event_at: None,
            pending_journal: None,
            runtime_unhealthy_reason: None,
            extra: Map::new(),
        }
    }

    #[test]
    fn claude_rate_limit_failure_is_authoritative_and_content_free() {
        let raw = json!({
            "hook_event_name": "StopFailure",
            "session_id": "claude-session",
            "error": "rate_limit",
            "error_details": "sensitive provider detail",
            "last_assistant_message": "sensitive rendered error",
            "transcript_path": "/private/transcript.jsonl"
        });

        let event = normalize_provider_hook(AgentKind::Claude, None, "runtime-1", &raw)
            .expect("recognized failure")
            .expect("normalized event");

        assert_eq!(event.kind, TurnEventKind::TurnFailed);
        assert_eq!(event.confidence, Confidence::Authoritative);
        assert_eq!(event.failure_reason.as_deref(), Some("usage_exhausted"));

        let serialized = serde_json::to_string(&event).expect("serialize event");
        for forbidden in [
            "sensitive provider detail",
            "sensitive rendered error",
            "/private/transcript.jsonl",
            "error_details",
            "last_assistant_message",
            "transcript_path",
        ] {
            assert!(!serialized.contains(forbidden), "leaked {forbidden}");
        }
    }

    #[test]
    fn auto_resume_failure_fixture_arms_only_authoritative_usage_exhaustion() {
        for line in include_str!("../tests/fixtures/activity/auto-resume-failures.jsonl").lines() {
            let mut raw: Value = serde_json::from_str(line).expect("failure fixture");
            let provider = raw
                .get("provider")
                .and_then(Value::as_str)
                .and_then(AgentKind::from_name)
                .expect("fixture provider");
            let expected = raw
                .get("expected")
                .and_then(Value::as_str)
                .map(str::to_string);
            let arms = raw.get("arms").and_then(Value::as_bool).unwrap_or(false);
            raw.as_object_mut().unwrap().remove("provider");
            raw.as_object_mut().unwrap().remove("expected");
            raw.as_object_mut().unwrap().remove("arms");
            let event = normalize_provider_hook(provider, None, "runtime-1", &raw)
                .expect("fixture normalization")
                .expect("recognized fixture");
            assert_eq!(event.failure_reason, expected);
            assert_eq!(
                event.failure_reason.as_deref() == Some("usage_exhausted")
                    && event.confidence == Confidence::Authoritative,
                arms
            );
        }
    }

    #[test]
    fn timed_activity_lock_wait_is_bounded() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let _held = acquire_lock(tmp.path()).expect("held lock");
        let started = Instant::now();
        let error = acquire_lock_with_timeout(tmp.path(), Duration::from_millis(50))
            .expect_err("timed lock must not wait forever");
        assert_eq!(error.code(), "activity-lock-timeout");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    fn test_session_for_agent(
        tmp: &tempfile::TempDir,
        agent: AgentKind,
    ) -> (CliContext, crate::CreatedRecord) {
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let cwd = tmp.path().join("repo");
        fs::create_dir_all(&cwd).expect("repo dir");
        let mut created = create_record(RecordRequest {
            context: &context,
            agent,
            mode: "interactive",
            title: None,
            title_state: None,
            explicit_id: Some("activity-test"),
            cwd: &cwd,
            prompt: None,
            log_file_name: None,
            provider_resume: Some(ProviderResume {
                provider: agent.as_str().to_string(),
                session_id: "session-1".to_string(),
                captured_at: "2026-07-10T00:00:00Z".to_string(),
                capture_method: "test".to_string(),
                resume_args: vec!["resume".to_string(), "session-1".to_string()],
                extra: BTreeMap::new(),
            }),
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .expect("test session");
        created.release_lifecycle_lock();
        (context, created)
    }

    fn test_session(tmp: &tempfile::TempDir) -> (CliContext, crate::CreatedRecord) {
        test_session_for_agent(tmp, AgentKind::Codex)
    }

    #[test]
    fn codex_permission_hook_obeys_the_immutable_runtime_authority() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (_, created) = test_session(&tmp);
        assert_eq!(
            codex_hook_attention_disposition(&created.record, Some("hook")),
            CodexHookAttentionDisposition::Accept
        );
        assert_eq!(
            codex_hook_attention_disposition(&created.record, None),
            CodexHookAttentionDisposition::Accept
        );
        assert_eq!(
            codex_hook_attention_disposition(&created.record, Some("protocol")),
            CodexHookAttentionDisposition::Breach
        );

        let mut protocol = created.record.clone();
        let runtime = protocol.runtime.as_mut().unwrap();
        runtime.kind = crate::codex_app_server::RUNTIME_KIND.to_string();
        runtime.extra.insert(
            crate::codex_app_server::ATTENTION_AUTHORITY_KEY.to_string(),
            json!("protocol"),
        );
        assert_eq!(
            codex_hook_attention_disposition(&protocol, Some("protocol")),
            CodexHookAttentionDisposition::Suppress
        );
        for injected in [None, Some("hook"), Some("future-mode")] {
            assert_eq!(
                codex_hook_attention_disposition(&protocol, injected),
                CodexHookAttentionDisposition::Breach
            );
        }
    }

    #[test]
    fn unhealthy_runtime_cannot_recover_until_a_new_generation() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, mut created) = test_session(&tmp);
        created.record.updated_at = "2000-01-01T00:00:00Z".to_string();
        write_session_record(&context, &created.record).unwrap();
        let runtime_id = created.record.runtime.as_ref().unwrap().launch_id.clone();
        let mut started = event(TurnEventKind::TurnStarted, "start");
        started.runtime_id = runtime_id.clone();
        let working = ingest_event(&context, &created.record.id, started)
            .unwrap()
            .turn_state;
        mark_runtime_unhealthy(
            &context,
            &created.record.id,
            &runtime_id,
            "fixture_projection_loss",
        )
        .unwrap();
        let unknown = activity_status(&context, &created.record.id)
            .unwrap()
            .turn_state;
        assert_eq!(unknown.phase, TurnPhase::Unknown);
        assert!(unknown.revision > working.revision);
        assert_ne!(unknown.phase_changed_at, created.record.updated_at);
        let revision = unknown.revision;
        let changed_at = unknown.phase_changed_at.clone();

        let mut progress = event(TurnEventKind::Progress, "late-progress");
        progress.runtime_id = runtime_id.clone();
        let error = ingest_event(&context, &created.record.id, progress)
            .expect_err("same-runtime evidence cannot recover degraded activity");
        assert_eq!(error.code(), "activity-runtime-unhealthy");
        assert_eq!(
            activate_runtime(&context, &created.record).unwrap().phase,
            TurnPhase::Unknown
        );
        assert_eq!(
            (
                activity_status(&context, &created.record.id)
                    .unwrap()
                    .turn_state
                    .revision,
                activity_status(&context, &created.record.id)
                    .unwrap()
                    .turn_state
                    .phase_changed_at,
            ),
            (revision, changed_at)
        );

        fs::write(
            session_dir(&context, &created.record.id).join(ACTIVITY_UNHEALTHY_FILE),
            b"{malformed",
        )
        .unwrap();
        assert!(runtime_is_unhealthy(&context, &created.record));
        assert_eq!(
            activity_status(&context, &created.record.id)
                .unwrap()
                .turn_state
                .phase,
            TurnPhase::Unknown
        );
        let auto_resume = crate::auto_resume::view_for_record(&context, &created.record);
        assert_eq!(auto_resume.state, "terminal_failure");
        assert_eq!(
            auto_resume.failure_reason.as_deref(),
            Some("state_unavailable")
        );

        let mut false_healthy = unknown.clone();
        false_healthy.phase = TurnPhase::Working;
        false_healthy.source = provider_source(&event(TurnEventKind::Progress, "false-healthy"));
        fs::write(
            session_dir(&context, &created.record.id).join(ACTIVITY_UNHEALTHY_FILE),
            serde_json::to_vec_pretty(&RuntimeUnhealthyMarker {
                schema_version: "agent-session.activity-unhealthy.v1".to_string(),
                runtime_id: runtime_id.clone(),
                runtime_generation: created.record.runtime.as_ref().unwrap().generation,
                reason: "invalid_state_fixture".to_string(),
                marked_at: "2030-01-01T00:00:00Z".to_string(),
                state: Some(false_healthy),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            activity_status(&context, &created.record.id)
                .unwrap()
                .turn_state
                .phase,
            TurnPhase::Unknown,
            "a parseable marker cannot publish a non-degraded state"
        );

        let runtime = created.record.runtime.as_mut().unwrap();
        runtime.generation += 1;
        runtime.launch_id = "runtime-next".to_string();
        runtime.started_at = "2030-01-01T00:00:00Z".to_string();
        write_session_record(&context, &created.record).unwrap();
        assert_eq!(
            activate_runtime(&context, &created.record).unwrap().phase,
            TurnPhase::Starting
        );
    }

    #[test]
    fn exact_attention_binds_an_unidentified_open_turn_before_rejecting_mismatch() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created) = test_session(&tmp);
        let runtime_id = created.record.runtime.as_ref().unwrap().launch_id.clone();

        let mut started = event(TurnEventKind::TurnStarted, "start-without-turn");
        started.runtime_id = runtime_id.clone();
        started.provider_turn_id = None;
        ingest_event(&context, &created.record.id, started).unwrap();

        let mut first = event(TurnEventKind::AttentionRequested, "attention-turn-a");
        first.runtime_id = runtime_id.clone();
        first.provider_turn_id = Some("turn-a".to_string());
        first.attention_id = Some("local:v1:attention-a".to_string());
        first.attention_kind = Some("approval".to_string());
        let bound = ingest_event(&context, &created.record.id, first)
            .unwrap()
            .turn_state;
        let bound_turn = bound
            .current_turn
            .as_ref()
            .and_then(|turn| turn.provider_turn_id.as_deref())
            .expect("first exact request must bind the open turn")
            .to_string();

        let mut second = event(TurnEventKind::AttentionRequested, "attention-turn-b");
        second.runtime_id = runtime_id;
        second.provider_turn_id = Some("turn-b".to_string());
        second.attention_id = Some("local:v1:attention-b".to_string());
        second.attention_kind = Some("approval".to_string());
        let error = ingest_event(&context, &created.record.id, second)
            .expect_err("a later exact request cannot change the bound turn");
        assert_eq!(error.code(), "provider-turn-id-mismatch");
        assert_eq!(
            activity_status(&context, &created.record.id)
                .unwrap()
                .turn_state
                .current_turn
                .and_then(|turn| turn.provider_turn_id),
            Some(bound_turn)
        );
    }

    #[test]
    fn codex_user_prompt_hook_persists_exact_runtime_provider_identity() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let cwd = tmp.path().join("repo");
        fs::create_dir_all(&cwd).expect("repo dir");
        let mut created = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            title: None,
            title_state: None,
            explicit_id: Some("hook-identity"),
            cwd: &cwd,
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .expect("test session");
        created.release_lifecycle_lock();
        let runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        let raw = json!({
            "hook_event_name":"UserPromptSubmit",
            "session_id":"exact-codex-session",
            "turn_id":"turn-1"
        });

        provider_resume_from_user_prompt_hook(
            &context,
            &created.record.id,
            AgentKind::Codex,
            &runtime_id,
            None,
            &raw,
        )
        .expect("capture identity");
        let record = load_session_record(&context, &created.record.id).expect("session record");
        let resume = record.provider_resume.expect("provider resume");
        assert_eq!(resume.provider, "codex");
        assert_eq!(resume.session_id, "exact-codex-session");
        assert_eq!(resume.capture_method, "codex-user-prompt-submit-hook");
        assert_eq!(
            &resume.resume_args[..2],
            &["resume".to_string(), "exact-codex-session".to_string()]
        );
        assert!(resume.resume_args.iter().any(|arg| arg == "--cd"));

        let error = provider_resume_from_user_prompt_hook(
            &context,
            &created.record.id,
            AgentKind::Codex,
            "different-runtime",
            None,
            &json!({
                "hook_event_name":"UserPromptSubmit",
                "session_id":"other-session"
            }),
        )
        .expect_err("wrong runtime must fail closed");
        assert_eq!(error.code(), "provider-hook-runtime-mismatch");
        let record = load_session_record(&context, &created.record.id).expect("session record");
        assert_eq!(
            record.provider_resume.expect("provider resume").session_id,
            "exact-codex-session"
        );
    }

    #[test]
    fn codex_user_prompt_hook_promotes_matching_heuristic_identity() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let cwd = tmp.path().join("repo");
        fs::create_dir_all(&cwd).expect("repo dir");
        let mut created = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            title: None,
            title_state: None,
            explicit_id: Some("hook-promotes-heuristic"),
            cwd: &cwd,
            prompt: None,
            log_file_name: None,
            provider_resume: Some(ProviderResume {
                provider: "codex".to_string(),
                session_id: "exact-codex-session".to_string(),
                captured_at: "2026-07-10T00:00:00Z".to_string(),
                capture_method: "codex-session-meta".to_string(),
                resume_args: Vec::new(),
                extra: BTreeMap::new(),
            }),
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .expect("test session");
        created.release_lifecycle_lock();
        let runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();

        provider_resume_from_user_prompt_hook(
            &context,
            &created.record.id,
            AgentKind::Codex,
            &runtime_id,
            None,
            &json!({
                "hook_event_name":"UserPromptSubmit",
                "session_id":"exact-codex-session"
            }),
        )
        .expect("promote matching identity");

        let record = load_session_record(&context, &created.record.id).expect("session record");
        let resume = record.provider_resume.expect("provider resume");
        assert_eq!(resume.capture_method, "codex-user-prompt-submit-hook");
        assert_eq!(
            &resume.resume_args[..2],
            &["resume".to_string(), "exact-codex-session".to_string()]
        );
    }

    #[test]
    fn provider_identity_and_title_updates_are_serialized_without_lost_fields() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let cwd = tmp.path().join("repo");
        fs::create_dir_all(&cwd).expect("repo dir");
        let mut created = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Codex,
            mode: "interactive",
            title: None,
            title_state: None,
            explicit_id: Some("concurrent-session-mutations"),
            cwd: &cwd,
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .expect("test session");
        created.release_lifecycle_lock();
        let runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        let barrier = Arc::new(Barrier::new(3));
        let hook = {
            let context = context.clone();
            let id = created.record.id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                provider_resume_from_user_prompt_hook(
                    &context,
                    &id,
                    AgentKind::Codex,
                    &runtime_id,
                    None,
                    &json!({
                        "hook_event_name":"UserPromptSubmit",
                        "session_id":"concurrent-provider-session"
                    }),
                )
            })
        };
        let title = {
            let context = context.clone();
            let id = created.record.id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                crate::update_session_title(
                    &context,
                    &id,
                    Some("Concurrent title".to_string()),
                    Path::new("/bin/false"),
                )
            })
        };
        barrier.wait();
        hook.join().expect("hook thread").expect("hook update");
        title.join().expect("title thread").expect("title update");

        let record = load_session_record(&context, &created.record.id).expect("session record");
        assert_eq!(record.title.as_deref(), Some("Concurrent title"));
        assert_eq!(
            record
                .provider_resume
                .as_ref()
                .expect("provider resume")
                .session_id,
            "concurrent-provider-session"
        );
        assert!(
            session_dir(&context, &created.record.id)
                .join(crate::SESSION_RESUME_FILE)
                .is_file()
        );
    }

    #[test]
    fn reducer_preserves_parallel_attention_until_each_correlated_request_clears() {
        let mut document = document();
        reduce(
            &mut document,
            &event(TurnEventKind::TurnStarted, "start"),
            "2026-07-10T00:00:01Z",
        );
        for (event_id, attention_id) in [("ask-1", "request-1"), ("ask-2", "request-2")] {
            let mut request = event(TurnEventKind::AttentionRequested, event_id);
            request.attention_id = Some(attention_id.to_string());
            request.attention_kind = Some("approval".to_string());
            reduce(&mut document, &request, "2026-07-10T00:00:02Z");
        }
        assert_eq!(document.state.phase, TurnPhase::NeedsInput);
        assert_eq!(
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .map(|attention| attention.pending_count),
            Some(2)
        );

        reduce(
            &mut document,
            &event(TurnEventKind::Progress, "unrelated-progress"),
            "2026-07-10T00:00:03Z",
        );
        assert_eq!(
            serde_json::to_value(&document.state).expect("state json")["current_turn"]["last_progress_at"],
            "2026-07-10T00:00:03Z",
            "accepted provider progress should expose a safe monotonic timestamp"
        );
        reduce(
            &mut document,
            &event(TurnEventKind::StopObserved, "raw-stop"),
            "2026-07-10T00:00:04Z",
        );
        assert_eq!(document.state.phase, TurnPhase::NeedsInput);

        let mut clear = event(TurnEventKind::AttentionCleared, "clear-1");
        clear.attention_id = Some("request-1".to_string());
        reduce(&mut document, &clear, "2026-07-10T00:00:05Z");
        assert_eq!(document.state.phase, TurnPhase::NeedsInput);
        assert_eq!(
            serde_json::to_value(&document.state).expect("state json")["current_turn"]["last_progress_at"],
            "2026-07-10T00:00:05Z",
            "an exact response is both a correlated clear and safe provider progress"
        );
        assert_eq!(
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .map(|attention| attention.pending_count),
            Some(1)
        );

        clear.event_id = "clear-2".to_string();
        clear.attention_id = Some("request-2".to_string());
        reduce(&mut document, &clear, "2026-07-10T00:00:06Z");
        assert_eq!(document.state.phase, TurnPhase::Working);
        assert_eq!(
            document
                .state
                .current_turn
                .as_ref()
                .map(|turn| turn.started_at.as_str()),
            Some("2026-07-10T00:00:01Z"),
            "clearing attention must not reset the original turn timer"
        );

        reduce(
            &mut document,
            &event(TurnEventKind::Progress, "late-progress"),
            "2026-07-10T00:00:04Z",
        );
        assert_eq!(
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.last_progress_at.as_deref()),
            Some("2026-07-10T00:00:06Z"),
            "out-of-order evidence must not regress the monotonic progress timestamp"
        );
    }

    #[test]
    fn reducer_progress_metadata_requires_provider_evidence_and_matching_clear() {
        let mut document = document();
        reduce(
            &mut document,
            &event(TurnEventKind::TurnStarted, "start"),
            "2026-07-10T00:00:01Z",
        );

        for (index, source_kind) in [
            SourceKind::ConsoleObservation,
            SourceKind::TerminalHeuristic,
            SourceKind::Runtime,
        ]
        .into_iter()
        .enumerate()
        {
            let mut progress = event(TurnEventKind::Progress, &format!("progress-{index}"));
            progress.source_kind = source_kind;
            reduce(
                &mut document,
                &progress,
                &format!("2026-07-10T00:00:0{}Z", index + 2),
            );
        }
        assert_eq!(
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.last_progress_at.as_deref()),
            None,
            "non-provider observations must not create safe progress metadata"
        );

        let mut request = event(TurnEventKind::AttentionRequested, "ask-1");
        request.attention_id = Some("request-1".to_string());
        request.attention_kind = Some("clarification".to_string());
        reduce(&mut document, &request, "2026-07-10T00:00:05Z");

        let mut non_provider_clear = event(TurnEventKind::AttentionCleared, "clear-local");
        non_provider_clear.attention_id = Some("request-1".to_string());
        non_provider_clear.source_kind = SourceKind::TerminalHeuristic;
        reduce(&mut document, &non_provider_clear, "2026-07-10T00:00:06Z");
        assert_eq!(
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.last_progress_at.as_deref()),
            None,
            "a non-provider clear must not count as safe provider progress"
        );

        request.event_id = "ask-2".to_string();
        request.attention_id = Some("request-2".to_string());
        reduce(&mut document, &request, "2026-07-10T00:00:07Z");

        let mut unmatched_clear = event(TurnEventKind::AttentionCleared, "clear-unmatched");
        unmatched_clear.attention_id = Some("different-request".to_string());
        reduce(&mut document, &unmatched_clear, "2026-07-10T00:00:08Z");
        assert_eq!(
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.last_progress_at.as_deref()),
            None,
            "an unmatched clear must not count as correlated provider progress"
        );

        unmatched_clear.event_id = "clear-matched".to_string();
        unmatched_clear.attention_id = Some("request-2".to_string());
        reduce(&mut document, &unmatched_clear, "2026-07-10T00:00:09Z");
        assert_eq!(
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.last_progress_at.as_deref()),
            Some("2026-07-10T00:00:09Z")
        );
    }

    #[test]
    fn reducer_bounds_attention_and_keeps_overflow_conservatively_latched() {
        let mut document = document();
        reduce(
            &mut document,
            &event(TurnEventKind::TurnStarted, "start"),
            "2026-07-10T00:00:01Z",
        );
        for index in 0..(MAX_PENDING_ATTENTION + 20) {
            let mut request = event(
                TurnEventKind::AttentionRequested,
                &format!("request-event-{index}"),
            );
            request.attention_id = Some(format!("request-{index}"));
            request.attention_kind = Some("approval".to_string());
            reduce(&mut document, &request, "2026-07-10T00:00:02Z");
        }
        assert_eq!(document.pending_attention.len(), MAX_PENDING_ATTENTION);
        assert_eq!(
            document
                .overflow_attention
                .as_ref()
                .map(|overflow| overflow.count),
            Some(20)
        );
        assert_eq!(document.state.phase, TurnPhase::NeedsInput);
        assert_eq!(
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .map(|attention| attention.pending_count),
            Some(MAX_PENDING_ATTENTION + 20)
        );
    }

    #[test]
    fn reducer_ignores_late_completion_after_a_newer_turn_completed() {
        let mut document = document();
        let mut turn_a = event(TurnEventKind::TurnStarted, "a-start");
        turn_a.provider_turn_id = Some("turn-a".to_string());
        reduce(&mut document, &turn_a, "2026-07-10T00:00:01Z");
        let mut turn_b = event(TurnEventKind::TurnStarted, "b-start");
        turn_b.provider_turn_id = Some("turn-b".to_string());
        reduce(&mut document, &turn_b, "2026-07-10T00:00:02Z");
        let mut complete_b = event(TurnEventKind::TurnCompleted, "b-complete");
        complete_b.provider_turn_id = Some("turn-b".to_string());
        reduce(&mut document, &complete_b, "2026-07-10T00:00:03Z");
        let last_b = document.state.last_turn.clone();

        let mut complete_a = event(TurnEventKind::TurnFailed, "a-late");
        complete_a.provider_turn_id = Some("turn-a".to_string());
        reduce(&mut document, &complete_a, "2026-07-10T00:00:04Z");

        assert_eq!(document.state.phase, TurnPhase::Waiting);
        assert_eq!(document.state.last_turn, last_b);
    }

    #[test]
    fn codex_authoritative_completion_requires_an_exact_open_turn() {
        let mut completion = event(TurnEventKind::TurnCompleted, "completion");
        completion.confidence = Confidence::Authoritative;
        completion.provider_turn_id = Some("turn-a".to_string());

        let mut no_open_turn = document();
        reduce(&mut no_open_turn, &completion, "2026-07-10T00:00:01Z");
        assert_eq!(no_open_turn.state.phase, TurnPhase::Starting);
        assert!(no_open_turn.state.last_turn.is_none());

        let mut id_less_turn = document();
        let mut start_without_id = event(TurnEventKind::TurnStarted, "start-without-id");
        start_without_id.provider_turn_id = None;
        reduce(&mut id_less_turn, &start_without_id, "2026-07-10T00:00:01Z");
        reduce(&mut id_less_turn, &completion, "2026-07-10T00:00:02Z");
        assert_eq!(id_less_turn.state.phase, TurnPhase::Working);
        assert!(id_less_turn.state.current_turn.is_some());

        let mut exact_turn = document();
        let mut start = event(TurnEventKind::TurnStarted, "start");
        start.provider_turn_id = Some("turn-a".to_string());
        reduce(&mut exact_turn, &start, "2026-07-10T00:00:01Z");
        reduce(&mut exact_turn, &completion, "2026-07-10T00:00:02Z");
        assert_eq!(exact_turn.state.phase, TurnPhase::Waiting);
        assert!(exact_turn.state.current_turn.is_none());
    }

    #[test]
    fn provider_mapping_uses_only_metadata_and_conservative_finality() {
        let codex = normalize_provider_hook(
            AgentKind::Codex,
            None,
            "runtime-1",
            &json!({
                "hook_event_name": "Stop",
                "session_id": "provider-session",
                "turn_id": "provider-turn",
                "last_assistant_message": "secret output",
                "transcript_path": "/secret/transcript"
            }),
        )
        .expect("codex mapping")
        .expect("codex event");
        assert_eq!(codex.kind, TurnEventKind::StopObserved);
        let serialized = serde_json::to_string(&codex).expect("event json");
        assert!(!serialized.contains("secret output"));
        assert!(!serialized.contains("transcript"));
        assert!(!serialized.contains("provider-session"));
        assert!(!serialized.contains("provider-turn"));

        let codex_completion = normalize_provider_notification(
            AgentKind::Codex,
            "runtime-1",
            &json!({
                "type": "agent-turn-complete",
                "thread-id": "provider-session",
                "turn-id": "provider-turn",
                "cwd": "/secret/cwd",
                "input-messages": ["secret prompt"],
                "last-assistant-message": "secret output"
            }),
        )
        .expect("Codex notification mapping")
        .expect("recognized completion");
        assert_eq!(codex_completion.kind, TurnEventKind::TurnCompleted);
        assert_eq!(codex_completion.confidence, Confidence::Authoritative);
        let serialized = serde_json::to_string(&codex_completion).expect("completion event json");
        for forbidden in [
            "provider-session",
            "provider-turn",
            "/secret/cwd",
            "secret prompt",
            "secret output",
            "input-messages",
            "last-assistant-message",
        ] {
            assert!(!serialized.contains(forbidden), "forbidden {forbidden}");
        }
        assert!(
            normalize_provider_notification(
                AgentKind::Codex,
                "runtime-1",
                &json!({"type": "future-notification"}),
            )
            .expect("future notification")
            .is_none()
        );
        let missing_turn = normalize_provider_notification(
            AgentKind::Codex,
            "runtime-1",
            &json!({
                "type": "agent-turn-complete",
                "thread-id": "provider-session"
            }),
        )
        .expect_err("completion requires turn-id correlation");
        assert_eq!(missing_turn.code(), "provider-notification-turn-id-missing");

        let claude = normalize_provider_hook(
            AgentKind::Claude,
            None,
            "runtime-1",
            &json!({
                "hook_event_name": "Notification",
                "notification_type": "idle_prompt",
                "message": "content is discarded"
            }),
        )
        .expect("claude mapping")
        .expect("claude event");
        assert_eq!(claude.kind, TurnEventKind::TurnCompleted);
        assert_eq!(claude.confidence, Confidence::Observed);

        let hermes = normalize_provider_hook(
            AgentKind::Hermes,
            Some("post_llm_call"),
            "runtime-1",
            &json!({
                "session_id": "provider-session",
                "assistant_response": "discarded"
            }),
        )
        .expect("Hermes mapping")
        .expect("Hermes event");
        assert_eq!(hermes.kind, TurnEventKind::TurnCompleted);
        assert_eq!(hermes.confidence, Confidence::Authoritative);
    }

    #[test]
    fn claude_pre_tool_use_reactivates_working_without_admitting_stale_subagent_stop() {
        let pre_tool_use = normalize_provider_hook(
            AgentKind::Claude,
            None,
            "runtime-1",
            &json!({
                "hook_event_name": "PreToolUse",
                "session_id": "claude-session",
                "tool_name": "Task",
                "tool_use_id": "tool-secret",
                "tool_input": {"prompt": "discarded"}
            }),
        )
        .expect("pre-tool normalization")
        .expect("recognized pre-tool signal");
        assert_eq!(pre_tool_use.kind, TurnEventKind::Progress);
        assert_eq!(pre_tool_use.confidence, Confidence::Observed);
        assert_eq!(pre_tool_use.source_kind, SourceKind::ProviderHook);
        assert_eq!(pre_tool_use.provider, "claude");

        let serialized = serde_json::to_string(&pre_tool_use).expect("continuation event json");
        for forbidden in ["tool-secret", "discarded"] {
            assert!(!serialized.contains(forbidden), "forbidden {forbidden}");
        }

        let mut document = document();
        reduce(
            &mut document,
            &event(TurnEventKind::TurnStarted, "turn-started"),
            "2026-07-18T00:00:01Z",
        );
        reduce(
            &mut document,
            &event(TurnEventKind::TurnCompleted, "idle-prompt"),
            "2026-07-18T00:00:02Z",
        );
        assert_eq!(document.state.phase, TurnPhase::Waiting);
        assert!(document.state.current_turn.is_none());

        let stale_subagent_stop = normalize_provider_hook(
            AgentKind::Claude,
            None,
            "runtime-1",
            &json!({
                "hook_event_name": "SubagentStop",
                "session_id": "claude-session",
                "agent_id": "subagent-secret",
                "agent_transcript_path": "/secret/transcript"
            }),
        )
        .expect("stale subagent-stop normalization");
        assert!(
            stale_subagent_stop.is_none(),
            "an uncorrelated completed subagent must not resurrect a waiting parent turn"
        );
        assert_eq!(document.state.phase, TurnPhase::Waiting);
        assert!(document.state.current_turn.is_none());

        reduce(&mut document, &pre_tool_use, "2026-07-18T00:00:03Z");
        assert_eq!(document.state.phase, TurnPhase::Working);
        assert!(document.state.current_turn.is_some());
        assert_eq!(document.state.source.kind, SourceKind::ProviderHook);
        assert_eq!(document.state.source.provider.as_deref(), Some("claude"));
        assert_eq!(document.state.source.confidence, Confidence::Observed);
    }

    #[test]
    fn claude_bypass_permissions_prompt_is_latched() {
        // PermissionRequest and permission_prompt hooks mean Claude is showing a
        // real dialog, including the root/home deletion circuit breaker that remains
        // active in bypass mode. Preserve that attention signal.
        for payload in [
            json!({
                "hook_event_name": "PermissionRequest",
                "session_id": "claude-session",
                "tool_name": "Bash",
                "tool_input": {"command": "rm -rf /"},
                "permission_mode": "bypassPermissions"
            }),
            json!({
                "hook_event_name": "Notification",
                "notification_type": "permission_prompt",
                "permission_mode": "bypassPermissions"
            }),
        ] {
            let mapped = normalize_provider_hook(AgentKind::Claude, None, "runtime-1", &payload)
                .expect("bypass mapping")
                .expect("bypass prompt");
            assert_eq!(mapped.kind, TurnEventKind::AttentionRequested);
            assert_eq!(mapped.attention_kind.as_deref(), Some("approval"));
        }

        // Non-bypass modes (and a missing mode) keep the conservative approval latch.
        for mode in [Some("default"), Some("acceptEdits"), Some("plan"), None] {
            let mut payload = json!({
                "hook_event_name": "PermissionRequest",
                "session_id": "claude-session",
                "tool_name": "Bash"
            });
            if let Some(mode) = mode {
                payload["permission_mode"] = json!(mode);
            }
            let mapped = normalize_provider_hook(AgentKind::Claude, None, "runtime-1", &payload)
                .expect("approval mapping")
                .expect("recognized approval");
            assert_eq!(mapped.kind, TurnEventKind::AttentionRequested);
            assert_eq!(mapped.attention_kind.as_deref(), Some("approval"));
        }
    }

    #[test]
    fn claude_ask_user_question_uses_exact_runtime_scoped_correlation() {
        let request = normalize_provider_hook(
            AgentKind::Claude,
            None,
            "runtime-1",
            &json!({
                "hook_event_name": "PreToolUse",
                "session_id": "claude-session",
                "tool_name": "AskUserQuestion",
                "tool_use_id": "tool-use-secret-1",
                "tool_input": {"questions": [{"question": "discarded"}]}
            }),
        )
        .expect("request mapping")
        .expect("recognized request");
        assert_eq!(request.kind, TurnEventKind::AttentionRequested);
        assert_eq!(request.attention_kind.as_deref(), Some("clarification"));
        let correlation = request.attention_id.as_deref().expect("correlation id");
        assert!(correlation.starts_with("local:v1:"));
        assert!(!correlation.contains("tool-use-secret-1"));

        for event_name in ["PostToolUse", "PostToolUseFailure"] {
            let response = normalize_provider_hook(
                AgentKind::Claude,
                None,
                "runtime-1",
                &json!({
                    "hook_event_name": event_name,
                    "session_id": "claude-session",
                    "tool_name": "AskUserQuestion",
                    "tool_use_id": "tool-use-secret-1",
                    "tool_response": {"answers": "discarded"},
                    "error": "discarded"
                }),
            )
            .expect("response mapping")
            .expect("recognized response");
            assert_eq!(response.kind, TurnEventKind::AttentionCleared);
            assert_eq!(response.attention_id.as_deref(), Some(correlation));
            let serialized = serde_json::to_string(&response).expect("response json");
            assert!(!serialized.contains("tool-use-secret-1"));
            assert!(!serialized.contains("answers"));
            assert!(!serialized.contains("discarded"));
        }

        let permission = normalize_provider_hook(
            AgentKind::Claude,
            None,
            "runtime-1",
            &json!({
                "hook_event_name": "PermissionRequest",
                "session_id": "claude-session",
                "tool_name": "Bash"
            }),
        )
        .expect("permission mapping")
        .expect("recognized permission");
        assert_eq!(permission.kind, TurnEventKind::AttentionRequested);
        assert_ne!(permission.attention_id.as_deref(), Some(correlation));

        let unrelated = normalize_provider_hook(
            AgentKind::Claude,
            None,
            "runtime-1",
            &json!({
                "hook_event_name": "PostToolUse",
                "session_id": "claude-session",
                "tool_name": "Bash",
                "tool_use_id": "other-tool"
            }),
        )
        .expect("progress mapping")
        .expect("recognized progress");
        assert_eq!(unrelated.kind, TurnEventKind::Progress);
        assert!(unrelated.attention_id.is_none());

        let missing_correlation = normalize_provider_hook(
            AgentKind::Claude,
            None,
            "runtime-1",
            &json!({
                "hook_event_name": "PreToolUse",
                "session_id": "claude-session",
                "tool_name": "AskUserQuestion"
            }),
        )
        .expect_err("missing correlation should surface safe drift diagnostics");
        assert_eq!(
            missing_correlation.code(),
            "provider-hook-correlation-missing"
        );
    }

    #[test]
    fn claude_ask_user_question_shadows_do_not_pin_attention() {
        let mapped = normalize_provider_hook(
            AgentKind::Claude,
            None,
            "runtime-1",
            &json!({
                "hook_event_name": "PermissionRequest",
                "session_id": "claude-session",
                "tool_name": "AskUserQuestion",
                "tool_input": {"questions": [{"question": "discarded"}]}
            }),
        )
        .expect("shadow signal should be safely recognized");
        assert!(
            mapped.is_none(),
            "AskUserQuestion exact PreToolUse/PostToolUse correlation must be the sole attention occurrence"
        );
        assert!(!provider_specs(AgentKind::Claude).iter().any(|spec| {
            spec.event == "Notification" && spec.matcher == Some("permission_prompt")
        }));
    }

    #[test]
    fn claude_elicitation_uses_exact_id_when_present_and_never_manufactures_a_clear() {
        for (mode, expected_kind) in [("form", "clarification"), ("url", "authentication")] {
            let request = normalize_provider_hook(
                AgentKind::Claude,
                None,
                "runtime-1",
                &json!({
                    "hook_event_name": "Elicitation",
                    "session_id": "claude-session",
                    "mcp_server_name": "fixture-server",
                    "mode": mode,
                    "elicitation_id": "elicit-secret-1",
                    "message": "must-not-leave-normalizer",
                    "url": "https://must-not-leave.invalid",
                    "requested_schema": {"secret": true}
                }),
            )
            .expect("request mapping")
            .expect("recognized elicitation request");
            assert_eq!(request.kind, TurnEventKind::AttentionRequested);
            assert_eq!(request.attention_kind.as_deref(), Some(expected_kind));
            let correlation = request.attention_id.as_deref().expect("exact id");
            assert!(correlation.starts_with("local:v1:"));

            let response = normalize_provider_hook(
                AgentKind::Claude,
                None,
                "runtime-1",
                &json!({
                    "hook_event_name": "ElicitationResult",
                    "session_id": "claude-session",
                    "mcp_server_name": "fixture-server",
                    "mode": mode,
                    "elicitation_id": "elicit-secret-1",
                    "action": "accept",
                    "content": {"answer": "must-not-leave-normalizer"}
                }),
            )
            .expect("response mapping")
            .expect("recognized elicitation response");
            assert_eq!(response.kind, TurnEventKind::AttentionCleared);
            assert_eq!(response.attention_id.as_deref(), Some(correlation));

            for event in [request, response] {
                let wire = serde_json::to_string(&event).unwrap();
                for forbidden in [
                    "elicit-secret-1",
                    "fixture-server",
                    "must-not-leave-normalizer",
                    "must-not-leave.invalid",
                    "answer",
                ] {
                    assert!(!wire.contains(forbidden), "forbidden {forbidden}");
                }
            }
        }

        let conservative = normalize_provider_hook(
            AgentKind::Claude,
            None,
            "runtime-1",
            &json!({
                "hook_event_name": "Elicitation",
                "session_id": "claude-session",
                "mode": "form",
                "message": "identifier omitted"
            }),
        )
        .expect("missing-id request remains safe")
        .expect("missing-id request is conservatively latched");
        assert_eq!(conservative.kind, TurnEventKind::AttentionRequested);
        assert_eq!(
            conservative.attention_kind.as_deref(),
            Some("clarification")
        );
        assert!(conservative.attention_id.is_some());

        assert!(
            normalize_provider_hook(
                AgentKind::Claude,
                None,
                "runtime-1",
                &json!({
                    "hook_event_name": "ElicitationResult",
                    "session_id": "claude-session",
                    "mode": "form",
                    "action": "decline"
                }),
            )
            .expect("identifier-less result is safely ignored")
            .is_none(),
            "identifier-less results must never clear a conservative latch"
        );
    }

    #[test]
    fn exact_codex_approval_ids_survive_semantic_deduplication() {
        let mut first = event(TurnEventKind::AttentionRequested, "first");
        first.attention_kind = Some("approval".to_string());
        first.attention_id = Some(
            "local:v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        );
        let mut second = first.clone();
        second.event_id = "second".to_string();
        second.attention_id = Some(
            "local:v1:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
        );

        assert_ne!(
            semantic_event_key(&first),
            semantic_event_key(&second),
            "independent exact approvals must not collapse inside the semantic dedupe window"
        );
    }

    #[test]
    fn hermes_approval_hooks_use_runtime_scoped_metadata_correlation() {
        let fixture = include_str!("../tests/fixtures/activity/hermes-approval-events.jsonl")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("Hermes approval fixture"))
            .collect::<Vec<_>>();
        let approval = fixture[0].clone();
        let request = normalize_provider_hook(
            AgentKind::Hermes,
            Some("pre_approval_request"),
            "runtime-1",
            &approval,
        )
        .expect("request mapping")
        .expect("recognized request");
        assert_eq!(request.kind, TurnEventKind::AttentionRequested);
        assert_eq!(request.attention_kind.as_deref(), Some("approval"));
        assert_eq!(request.confidence, Confidence::Observed);
        let correlation = request.attention_id.as_deref().expect("correlation id");
        assert!(correlation.starts_with("local:v1:"));

        let response_payload = fixture[1].clone();
        let response = normalize_provider_hook(
            AgentKind::Hermes,
            Some("post_approval_response"),
            "runtime-1",
            &response_payload,
        )
        .expect("response mapping")
        .expect("recognized response");
        assert_eq!(response.kind, TurnEventKind::AttentionCleared);
        assert_eq!(response.attention_id.as_deref(), Some(correlation));
        assert_eq!(response.confidence, Confidence::Observed);

        let mut reordered = response_payload.clone();
        reordered["pattern_keys"] = json!(["shell_escalation", "sudo", "sudo"]);
        let reordered = normalize_provider_hook(
            AgentKind::Hermes,
            Some("post_approval_response"),
            "runtime-1",
            &reordered,
        )
        .expect("reordered mapping")
        .expect("recognized response");
        assert_eq!(reordered.attention_id.as_deref(), Some(correlation));

        let same_other_runtime = normalize_provider_hook(
            AgentKind::Hermes,
            Some("pre_approval_request"),
            "runtime-2",
            &approval,
        )
        .expect("other runtime mapping")
        .expect("recognized request");
        assert_ne!(same_other_runtime.attention_id, request.attention_id);

        let mut different = approval.clone();
        different["pattern_keys"] = json!(["sudo"]);
        let different = normalize_provider_hook(
            AgentKind::Hermes,
            Some("pre_approval_request"),
            "runtime-1",
            &different,
        )
        .expect("different mapping")
        .expect("recognized request");
        assert_ne!(different.attention_id, request.attention_id);
        assert_ne!(semantic_event_key(&different), semantic_event_key(&request));
        assert_eq!(
            semantic_event_key(&reordered),
            semantic_event_key(&response)
        );

        let missing = normalize_provider_hook(
            AgentKind::Hermes,
            Some("pre_approval_request"),
            "runtime-1",
            &json!({
                "session_key": "hermes-session",
                "surface": "cli",
                "command": "must-not-appear-in-diagnostic"
            }),
        )
        .expect_err("the complete matching tuple is required");
        assert_eq!(missing.code(), "provider-hook-correlation-missing");
        assert!(!format!("{missing:?}").contains("must-not-appear-in-diagnostic"));

        let mut invalid_response = response_payload.clone();
        invalid_response["choice"] = json!("future-choice");
        let invalid_response = normalize_provider_hook(
            AgentKind::Hermes,
            Some("post_approval_response"),
            "runtime-1",
            &invalid_response,
        )
        .expect_err("unknown response choices must not clear attention");
        assert_eq!(invalid_response.code(), "provider-hook-response-invalid");

        let mut document = document();
        reduce(
            &mut document,
            &event(TurnEventKind::TurnStarted, "start"),
            "2026-07-10T00:00:01Z",
        );
        reduce(&mut document, &request, "2026-07-10T00:00:02Z");
        reduce(&mut document, &different, "2026-07-10T00:00:03Z");
        assert_eq!(
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .map(|attention| attention.pending_count),
            Some(2)
        );
        reduce(&mut document, &response, "2026-07-10T00:00:04Z");
        assert_eq!(document.state.phase, TurnPhase::NeedsInput);
        assert_eq!(
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .map(|attention| attention.pending_count),
            Some(1)
        );

        for event in [&request, &response] {
            let serialized = serde_json::to_string(event).expect("event json");
            for forbidden in [
                "discarded-command",
                "discarded-description",
                "shell_escalation",
                "hermes-session",
                "choice",
            ] {
                assert!(!serialized.contains(forbidden), "forbidden {forbidden}");
            }
        }
    }

    #[test]
    fn identical_concurrent_hermes_approvals_remain_latched_after_one_response() {
        let fixture = include_str!("../tests/fixtures/activity/hermes-approval-events.jsonl")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("Hermes approval fixture"))
            .collect::<Vec<_>>();
        let first = normalize_provider_hook(AgentKind::Hermes, None, "runtime-1", &fixture[0])
            .expect("first mapping")
            .expect("first request");
        let second = normalize_provider_hook(AgentKind::Hermes, None, "runtime-1", &fixture[0])
            .expect("second mapping")
            .expect("second request");
        assert_ne!(first.event_id, second.event_id);
        assert_eq!(first.attention_id, second.attention_id);

        let mut document = document();
        reduce(&mut document, &first, "2026-07-10T00:00:01Z");
        let first_key = semantic_event_key(&first);
        document.last_semantic_event = Some(first_key);
        document.last_semantic_event_at = Some("2026-07-10T00:00:01Z".to_string());
        let second_key = semantic_event_key(&second);
        assert!(
            !semantic_event_is_duplicate(&document, &second, &second_key, "2026-07-10T00:00:01Z"),
            "distinct hook deliveries must retain ambiguous request multiplicity"
        );
        reduce(&mut document, &second, "2026-07-10T00:00:01Z");
        assert_eq!(
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .map(|attention| attention.pending_count),
            Some(2)
        );

        let response = normalize_provider_hook(AgentKind::Hermes, None, "runtime-1", &fixture[1])
            .expect("response mapping")
            .expect("response");
        reduce(&mut document, &response, "2026-07-10T00:00:02Z");
        assert_eq!(document.state.phase, TurnPhase::NeedsInput);
        assert_eq!(
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .map(|attention| attention.pending_count),
            Some(1)
        );

        let completion = normalize_provider_hook(
            AgentKind::Hermes,
            Some("post_llm_call"),
            "runtime-1",
            &json!({"session_id": "hermes-session"}),
        )
        .expect("completion mapping")
        .expect("completion");
        reduce(&mut document, &completion, "2026-07-10T00:00:03Z");
        assert_eq!(document.state.phase, TurnPhase::Waiting);
        assert!(
            document
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .is_none()
        );
    }

    #[test]
    fn frozen_provider_fixtures_replay_through_each_adapter() {
        let cases = [
            (
                AgentKind::Codex,
                include_str!("../tests/fixtures/activity/codex-events.jsonl"),
                vec![
                    TurnEventKind::TurnStarted,
                    TurnEventKind::AttentionRequested,
                    TurnEventKind::Progress,
                    TurnEventKind::StopObserved,
                ],
            ),
            (
                AgentKind::Claude,
                include_str!("../tests/fixtures/activity/claude-events.jsonl"),
                vec![
                    TurnEventKind::TurnStarted,
                    TurnEventKind::Progress,
                    TurnEventKind::AttentionRequested,
                    TurnEventKind::AttentionCleared,
                    TurnEventKind::AttentionRequested,
                    TurnEventKind::AttentionCleared,
                    TurnEventKind::AttentionRequested,
                    TurnEventKind::Progress,
                    TurnEventKind::StopObserved,
                    TurnEventKind::TurnCompleted,
                    TurnEventKind::TurnFailed,
                ],
            ),
            (
                AgentKind::Hermes,
                include_str!("../tests/fixtures/activity/hermes-events.jsonl"),
                vec![TurnEventKind::TurnStarted, TurnEventKind::TurnCompleted],
            ),
        ];
        for (agent, fixture, expected) in cases {
            let normalized = fixture
                .lines()
                .map(|line| {
                    let raw: Value = serde_json::from_str(line).expect("provider fixture");
                    normalize_provider_hook(agent, None, "runtime-1", &raw)
                        .expect("provider fixture mapping")
                        .expect("recognized provider fixture")
                })
                .collect::<Vec<_>>();
            assert_eq!(
                normalized
                    .iter()
                    .map(|event| event.kind.clone())
                    .collect::<Vec<_>>(),
                expected
            );
            for event in normalized {
                validate_event(&event).expect("normalized fixture event");
                let serialized = serde_json::to_string(&event).expect("normalized event json");
                assert!(!serialized.contains("codex-session"));
                assert!(!serialized.contains("claude-session"));
                assert!(!serialized.contains("hermes-session"));
                assert!(!serialized.contains("tool-1"));
                assert!(!serialized.contains("subagent-secret"));
                assert!(!serialized.contains("rate_limit"));
            }
        }
    }

    #[test]
    fn frozen_codex_notification_fixture_projects_only_matching_completion_metadata() {
        let normalized = include_str!("../tests/fixtures/activity/codex-notifications.jsonl")
            .lines()
            .map(|line| {
                let raw: Value = serde_json::from_str(line).expect("Codex notification fixture");
                normalize_provider_notification(AgentKind::Codex, "runtime-1", &raw)
                    .expect("Codex notification mapping")
            })
            .collect::<Vec<_>>();
        let completion = normalized[0].as_ref().expect("recognized completion");
        assert_eq!(completion.kind, TurnEventKind::TurnCompleted);
        assert_eq!(completion.confidence, Confidence::Authoritative);
        assert!(normalized[1].is_none());
        let serialized = serde_json::to_string(completion).expect("normalized completion");
        for forbidden in [
            "codex-session",
            "codex-turn",
            "<redacted>",
            "input-messages",
            "last-assistant-message",
        ] {
            assert!(!serialized.contains(forbidden), "forbidden {forbidden}");
        }
    }

    #[test]
    fn provider_config_write_rejects_a_changed_source() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("settings.json");
        fs::write(&path, b"newer").expect("concurrent provider config");
        let error = write_provider_config_if_unchanged(&path, b"owned", Some(b"older"))
            .expect_err("stale update must fail");
        assert_eq!(error.code(), "provider-config-concurrent-modification");
        assert_eq!(fs::read(&path).expect("preserved config"), b"newer");
    }

    #[test]
    fn provider_version_parser_handles_audited_cli_formats() {
        assert_eq!(
            parse_version_triplet("codex-cli 0.144.1"),
            Some((0, 144, 1))
        );
        assert_eq!(
            parse_version_triplet("2.1.206 (Claude Code)"),
            Some((2, 1, 206))
        );
        assert_eq!(parse_version_triplet("Hermes 0.18.2"), Some((0, 18, 2)));
        assert_eq!(parse_version_triplet("development build"), None);
        assert_eq!(audited_floor(AgentKind::Claude), (2, 1, 206));
        assert_eq!(audited_floor(AgentKind::Hermes), (0, 18, 2));
    }

    #[test]
    fn event_confidence_is_required_by_the_v1_wire_contract() {
        let missing = json!({
            "schema_version": TURN_EVENT_VERSION,
            "event_id": "event-1",
            "runtime_id": "runtime-1",
            "provider": "codex",
            "kind": "progress"
        });
        assert!(serde_json::from_value::<TurnEvent>(missing).is_err());
    }

    #[test]
    fn claude_progress_preserves_exact_replay_capacity_for_lifecycle_events() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created) = test_session_for_agent(&tmp, AgentKind::Claude);
        let runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        let dir = session_dir(&context, &created.record.id);
        let path = dir.join(ACTIVITY_FILE);
        let mut snapshot = read_document(&path).expect("activity snapshot");
        snapshot.seen_event_count = MAX_DEDUPE_EVENTS - 1;
        write_document(&path, &snapshot).expect("near-capacity snapshot");

        let progress = normalize_provider_hook(
            AgentKind::Claude,
            None,
            &runtime_id,
            &json!({
                "hook_event_name": "PreToolUse",
                "session_id": "session-1",
                "tool_name": "Bash",
                "tool_use_id": "tool-secret"
            }),
        )
        .expect("progress normalization")
        .expect("recognized progress");
        let working = ingest_event(&context, &created.record.id, progress)
            .expect("Claude progress remains ingestible near replay capacity");
        assert_eq!(working.turn_state.phase, TurnPhase::Working);
        assert_eq!(
            read_document(&path)
                .expect("progress snapshot")
                .seen_event_count,
            MAX_DEDUPE_EVENTS - 1,
            "uncorrelated Claude progress must not consume exact replay capacity"
        );

        let completed = normalize_provider_hook(
            AgentKind::Claude,
            None,
            &runtime_id,
            &json!({
                "hook_event_name": "Notification",
                "notification_type": "idle_prompt",
                "session_id": "session-1"
            }),
        )
        .expect("completion normalization")
        .expect("recognized completion");
        let waiting = ingest_event(&context, &created.record.id, completed)
            .expect("lifecycle event retains the final exact replay slot");
        assert_eq!(waiting.turn_state.phase, TurnPhase::Waiting);
        assert_eq!(
            read_document(&path)
                .expect("completion snapshot")
                .seen_event_count,
            MAX_DEDUPE_EVENTS
        );
    }

    #[test]
    fn claude_progress_pending_journal_repairs_without_exact_replay_slot() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created) = test_session_for_agent(&tmp, AgentKind::Claude);
        let runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        let dir = session_dir(&context, &created.record.id);
        let journal_path = dir.join(ACTIVITY_JOURNAL_FILE);
        fs::create_dir(&journal_path).expect("block journal target");
        let progress = normalize_provider_hook(
            AgentKind::Claude,
            None,
            &runtime_id,
            &json!({
                "hook_event_name": "PreToolUse",
                "session_id": "session-1",
                "tool_name": "Task",
                "tool_use_id": "tool-secret"
            }),
        )
        .expect("progress normalization")
        .expect("recognized progress");

        assert!(
            ingest_event(&context, &created.record.id, progress.clone()).is_err(),
            "blocked journal target must interrupt the split write"
        );
        let pending = read_document(&dir.join(ACTIVITY_FILE)).expect("pending snapshot");
        assert!(pending.pending_journal.is_some());
        assert_eq!(pending.seen_event_count, 0);

        fs::remove_dir(&journal_path).expect("restore journal target");
        let repaired = ingest_event(&context, &created.record.id, progress)
            .expect("repair replay-exempt progress");
        assert!(repaired.duplicate);
        let repaired_snapshot = read_document(&dir.join(ACTIVITY_FILE)).expect("repaired snapshot");
        assert!(repaired_snapshot.pending_journal.is_none());
        assert_eq!(repaired_snapshot.seen_event_count, 0);
        let journal = fs::read_to_string(journal_path).expect("repaired journal");
        assert_eq!(journal.matches("\"kind\":\"progress\"").count(), 1);
    }

    #[test]
    fn dedupe_horizon_is_independent_from_the_bounded_journal() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created) = test_session(&tmp);
        let runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        let first = event(TurnEventKind::Progress, "event-0");
        for index in 0..=MAX_JOURNAL_EVENTS {
            let mut current = if index == 0 {
                first.clone()
            } else {
                event(TurnEventKind::Progress, &format!("event-{index}"))
            };
            current.runtime_id.clone_from(&runtime_id);
            ingest_event(&context, &created.record.id, current).expect("accepted event");
        }
        let before = activity_status(&context, &created.record.id)
            .expect("status")
            .turn_state
            .revision;
        let mut replay = first;
        replay.runtime_id = runtime_id;
        let result = ingest_event(&context, &created.record.id, replay).expect("duplicate replay");
        assert!(result.duplicate);
        assert_eq!(result.turn_state.revision, before);
        let dir = session_dir(&context, &created.record.id);
        let snapshot = fs::read_to_string(dir.join(ACTIVITY_FILE)).expect("snapshot");
        let journal = fs::read_to_string(dir.join(ACTIVITY_JOURNAL_FILE)).expect("journal");
        assert!(!snapshot.contains("session-1"));
        assert!(!snapshot.contains("turn-1"));
        assert!(!journal.contains("session-1"));
        assert!(!journal.contains("turn-1"));
    }

    #[test]
    fn exact_hermes_replay_survives_restart_and_bounded_journal_eviction() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let context = CliContext {
            state_dir: tmp.path().join("state"),
            host: None,
        };
        let cwd = tmp.path().join("repo");
        fs::create_dir_all(&cwd).expect("repo dir");
        let mut created = create_record(RecordRequest {
            context: &context,
            agent: AgentKind::Hermes,
            mode: "interactive",
            title: None,
            title_state: None,
            explicit_id: Some("hermes-replay-horizon"),
            cwd: &cwd,
            prompt: None,
            log_file_name: None,
            provider_resume: None,
            agent_args: Vec::new(),
            agent_bin: None,
        })
        .expect("Hermes session");
        created.release_lifecycle_lock();
        let runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        let raw = json!({
            "hook_event_name": "pre_approval_request",
            "session_id": "",
            "extra": {
                "command": "discarded-command",
                "description": "discarded-description",
                "pattern_key": "discarded-pattern",
                "pattern_keys": ["discarded-pattern"],
                "session_key": "provider-session",
                "surface": "shell",
                "turn_id": "provider-turn",
                "tool_call_id": "exact-tool-call"
            }
        });
        let exact = normalize_provider_hook(AgentKind::Hermes, None, &runtime_id, &raw)
            .expect("normalize exact approval")
            .expect("exact approval event");
        let first = ingest_event(&context, &created.record.id, exact.clone())
            .expect("first exact approval");
        assert_eq!(first.turn_state.phase, TurnPhase::NeedsInput);

        for index in 0..=(MAX_JOURNAL_EVENTS + 20) {
            let mut progress = exact.clone();
            progress.event_id = format!("eviction-progress-{index}");
            progress.kind = TurnEventKind::Progress;
            progress.attention_id = None;
            progress.attention_kind = None;
            progress.provider_turn_id = Some(format!("eviction-turn-{index}"));
            ingest_event(&context, &created.record.id, progress).expect("accepted progress");
        }

        let dir = session_dir(&context, &created.record.id);
        let journal_path = dir.join(ACTIVITY_JOURNAL_FILE);
        let journal_before = fs::read_to_string(&journal_path).expect("bounded journal");
        assert!(!journal_before.contains("attention_requested"));
        let before = activity_status(&context, &created.record.id)
            .expect("status before replay")
            .turn_state;
        let restarted_replay = normalize_provider_hook(AgentKind::Hermes, None, &runtime_id, &raw)
            .expect("normalize replay after restart")
            .expect("replayed exact approval event");
        assert_eq!(restarted_replay.event_id, exact.event_id);
        let replay = ingest_event(&context, &created.record.id, restarted_replay)
            .expect("replay after bounded eviction");
        assert!(replay.duplicate);
        assert_eq!(replay.turn_state.revision, before.revision);
        assert_eq!(replay.turn_state.phase, before.phase);
        assert_eq!(
            fs::read_to_string(journal_path).expect("journal after replay"),
            journal_before
        );
    }

    #[test]
    fn pending_journal_is_repaired_idempotently_after_a_split_write() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created) = test_session(&tmp);
        let runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        let dir = session_dir(&context, &created.record.id);
        let journal_path = dir.join(ACTIVITY_JOURNAL_FILE);
        fs::create_dir(&journal_path).expect("block journal target");
        let mut progress = event(TurnEventKind::Progress, "split-write-event");
        progress.runtime_id = runtime_id;
        assert!(ingest_event(&context, &created.record.id, progress.clone()).is_err());
        let pending = read_document(&dir.join(ACTIVITY_FILE)).expect("pending snapshot");
        assert!(pending.pending_journal.is_some());

        fs::remove_dir(&journal_path).expect("restore journal target");
        let repaired =
            ingest_event(&context, &created.record.id, progress).expect("repaired duplicate");
        assert!(repaired.duplicate);
        let document = read_document(&dir.join(ACTIVITY_FILE)).expect("repaired snapshot");
        assert!(document.pending_journal.is_none());
        let journal = fs::read_to_string(journal_path).expect("journal");
        assert_eq!(journal.matches("split-write-event").count(), 1);
    }

    #[test]
    fn runtime_generation_mismatch_never_exposes_or_accepts_stale_activity() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created) = test_session(&tmp);
        let mut progress = event(TurnEventKind::Progress, "generation-one-event");
        progress.runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        ingest_event(&context, &created.record.id, progress.clone()).expect("generation one event");

        let mut downgraded_resume = created.record.clone();
        downgraded_resume
            .runtime
            .as_mut()
            .expect("runtime")
            .generation += 1;
        write_session_record(&context, &downgraded_resume).expect("downgraded resume record");
        let status = activity_status(&context, &created.record.id).expect("safe status");
        assert_eq!(status.turn_state.phase, TurnPhase::Unknown);
        progress.event_id = "generation-two-event".to_string();
        assert_eq!(
            ingest_event(&context, &created.record.id, progress)
                .expect_err("stale activity generation")
                .code(),
            "runtime-id-mismatch"
        );
    }

    #[test]
    fn runtime_activation_repairs_pending_journal_before_transition() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created) = test_session(&tmp);
        let dir = session_dir(&context, &created.record.id);
        let journal_path = dir.join(ACTIVITY_JOURNAL_FILE);
        fs::create_dir(&journal_path).expect("block journal target");
        let mut progress = event(TurnEventKind::Progress, "pre-resume-pending");
        progress.runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        assert!(ingest_event(&context, &created.record.id, progress).is_err());
        fs::remove_dir(&journal_path).expect("restore journal target");

        let mut resumed = created.record.clone();
        let runtime = resumed.runtime.as_mut().expect("runtime");
        runtime.generation += 1;
        runtime.launch_id = "runtime-2".to_string();
        runtime.started_at = "2026-07-10T00:02:00Z".to_string();
        write_session_record(&context, &resumed).expect("resumed record");
        activate_runtime(&context, &resumed).expect("activate next generation");

        let journal = fs::read_to_string(journal_path).expect("repaired journal");
        assert_eq!(journal.matches("pre-resume-pending").count(), 1);
        let document = read_document(&dir.join(ACTIVITY_FILE)).expect("new activity");
        assert_eq!(document.runtime_generation, 2);
        assert!(document.pending_journal.is_none());
    }

    #[test]
    fn runtime_activation_preserves_additive_fields_and_quarantines_future_schema() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created) = test_session(&tmp);
        let dir = session_dir(&context, &created.record.id);
        let path = dir.join(ACTIVITY_FILE);
        let mut value: Value =
            serde_json::from_slice(&fs::read(&path).expect("activity")).expect("activity json");
        value["future_top"] = json!({ "enabled": true });
        value["state"]["future_state"] = json!("preserve-me");
        value["state"]["source"]["future_source"] = json!("preserve-source");
        value["state"]["current_turn"] = json!({
            "provider_turn_id": null,
            "started_at": "2026-07-10T00:01:00Z",
            "future_turn": "preserve-turn"
        });
        fs::write(
            &path,
            serde_json::to_vec_pretty(&value).expect("activity bytes"),
        )
        .expect("extended activity");

        let mut next = created.record.clone();
        let runtime = next.runtime.as_mut().expect("runtime");
        runtime.generation += 1;
        runtime.launch_id = "runtime-2".to_string();
        runtime.started_at = "2026-07-10T00:02:00Z".to_string();
        write_session_record(&context, &next).expect("next record");
        activate_runtime(&context, &next).expect("activate with additive fields");
        let preserved: Value =
            serde_json::from_slice(&fs::read(&path).expect("preserved activity"))
                .expect("preserved json");
        assert_eq!(preserved["future_top"]["enabled"], true);
        assert_eq!(preserved["state"]["future_state"], "preserve-me");
        assert_eq!(
            preserved["state"]["source"]["future_source"],
            "preserve-source"
        );
        assert_eq!(
            preserved["state"]["last_turn"]["future_turn"],
            "preserve-turn"
        );

        let mut future = preserved;
        future["schema_version"] = json!("agent-session.activity.v99");
        let future_bytes = serde_json::to_vec_pretty(&future).expect("future bytes");
        fs::write(&path, &future_bytes).expect("future activity");
        let runtime = next.runtime.as_mut().expect("runtime");
        runtime.generation += 1;
        runtime.launch_id = "runtime-3".to_string();
        runtime.started_at = "2026-07-10T00:03:00Z".to_string();
        write_session_record(&context, &next).expect("third record");
        activate_runtime(&context, &next).expect("quarantine future activity");

        let quarantine = fs::read_dir(&dir)
            .expect("session files")
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("activity.quarantine.activity-version-unsupported")
                    })
            })
            .expect("future activity quarantine");
        assert_eq!(
            fs::read(quarantine).expect("quarantine bytes"),
            future_bytes
        );
        let current = read_document(&path).expect("current activity");
        assert_eq!(current.runtime_generation, 3);
    }

    #[test]
    fn provider_version_probe_times_out_without_blocking_diagnostics() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let binary = tmp.path().join("slow-provider");
        fs::write(&binary, "#!/usr/bin/env sh\nsleep 5\n").expect("slow provider");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("provider mode");
        let started = Instant::now();
        let probe = probe_version_command(
            binary.to_str().expect("provider path"),
            Duration::from_millis(50),
        );
        assert_eq!(probe.error.as_deref(), Some("timeout"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn repeated_provider_hook_semantics_are_idempotent() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created) = test_session(&tmp);
        let runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        let mut start = event(TurnEventKind::TurnStarted, "hook-start-1");
        start.runtime_id = runtime_id.clone();
        let first = ingest_event(&context, &created.record.id, start.clone()).expect("first start");
        start.event_id = "hook-start-2".to_string();
        let repeated = ingest_event(&context, &created.record.id, start).expect("repeated start");
        assert!(repeated.duplicate);
        assert_eq!(repeated.turn_state.revision, first.turn_state.revision);

        let mut attention = event(TurnEventKind::AttentionRequested, "hook-attention-1");
        attention.runtime_id = runtime_id.clone();
        attention.attention_id = Some("generated-attention-1".to_string());
        attention.attention_kind = Some("approval".to_string());
        let first =
            ingest_event(&context, &created.record.id, attention.clone()).expect("first attention");
        attention.event_id = "hook-attention-2".to_string();
        attention.attention_id = Some("generated-attention-2".to_string());
        let repeated =
            ingest_event(&context, &created.record.id, attention).expect("repeated attention");
        assert!(repeated.duplicate);
        assert_eq!(repeated.turn_state.revision, first.turn_state.revision);
        assert_eq!(
            repeated
                .turn_state
                .current_turn
                .and_then(|turn| turn.attention)
                .map(|attention| attention.pending_count),
            Some(1)
        );

        let mut complete = event(TurnEventKind::TurnCompleted, "hook-complete-1");
        complete.runtime_id = runtime_id;
        let first =
            ingest_event(&context, &created.record.id, complete.clone()).expect("first completion");
        let mut stop = event(TurnEventKind::StopObserved, "hook-stop-after-completion");
        stop.runtime_id = complete.runtime_id.clone();
        let after_stop =
            ingest_event(&context, &created.record.id, stop).expect("interleaved raw stop");
        complete.event_id = "hook-complete-2".to_string();
        let repeated =
            ingest_event(&context, &created.record.id, complete).expect("repeated completion");
        assert!(repeated.duplicate);
        assert_eq!(
            repeated.turn_state.revision, after_stop.turn_state.revision,
            "intervening non-final observations must not reopen completion dedupe"
        );
        assert!(after_stop.turn_state.revision > first.turn_state.revision);
    }

    #[test]
    fn missing_replay_index_never_reopens_a_nonempty_dedupe_horizon() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created) = test_session(&tmp);
        let mut progress = event(TurnEventKind::Progress, "durable-event");
        progress.runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        ingest_event(&context, &created.record.id, progress.clone()).expect("first event");
        fs::remove_file(session_dir(&context, &created.record.id).join(ACTIVITY_REPLAY_FILE))
            .expect("remove replay index");

        assert!(ingest_event(&context, &created.record.id, progress).is_err());
        assert_eq!(
            state_for_view(&context, &created.record)
                .expect("safe state")
                .phase,
            TurnPhase::Unknown
        );
    }

    #[test]
    fn replay_index_from_another_runtime_is_rejected() {
        let first_tmp = tempfile::TempDir::new().expect("first tempdir");
        let (first_context, first) = test_session(&first_tmp);
        let mut first_event = event(TurnEventKind::Progress, "first-event");
        first_event.runtime_id = first
            .record
            .runtime
            .as_ref()
            .expect("first runtime")
            .launch_id
            .clone();
        ingest_event(&first_context, &first.record.id, first_event.clone()).expect("first event");

        let second_tmp = tempfile::TempDir::new().expect("second tempdir");
        let (second_context, second) = test_session(&second_tmp);
        let mut second_event = event(TurnEventKind::Progress, "second-event");
        second_event.runtime_id = second
            .record
            .runtime
            .as_ref()
            .expect("second runtime")
            .launch_id
            .clone();
        ingest_event(&second_context, &second.record.id, second_event).expect("second event");

        fs::copy(
            session_dir(&second_context, &second.record.id).join(ACTIVITY_REPLAY_FILE),
            session_dir(&first_context, &first.record.id).join(ACTIVITY_REPLAY_FILE),
        )
        .expect("swap same-size replay index");
        assert!(ingest_event(&first_context, &first.record.id, first_event).is_err());
        assert_eq!(
            state_for_view(&first_context, &first.record)
                .expect("safe state")
                .phase,
            TurnPhase::Unknown
        );
    }

    #[test]
    fn configured_provider_specs_require_the_owned_timeout() {
        let codex = json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{
                        "type": "command",
                        "command": owned_command(AgentKind::Codex, None),
                        "timeout": 1
                    }]
                }]
            }
        });
        assert!(!json_has_spec(
            &codex,
            AgentKind::Codex,
            provider_specs(AgentKind::Codex)[0]
        ));
        let permission_command = owned_command(AgentKind::Codex, Some("PermissionRequest"));
        assert!(permission_command.contains("AGENT_SESSION_ATTENTION_AUTHORITY"));
        assert!(permission_command.contains("= protocol"));
        assert!(permission_command.contains("exec agent-session activity hook --agent codex"));

        let hermes: serde_yaml_ng::Value = serde_yaml_ng::from_str(&format!(
            "hooks:\n  pre_llm_call:\n    - command: {}\n      timeout: 1\n",
            owned_command(AgentKind::Hermes, Some("pre_llm_call"))
        ))
        .expect("Hermes config");
        assert!(!yaml_has_spec(
            &hermes,
            provider_specs(AgentKind::Hermes)[0]
        ));

        let claude_specs = provider_specs(AgentKind::Claude);
        assert!(
            claude_specs
                .iter()
                .any(|spec| spec.event == "PreToolUse" && spec.matcher.is_none())
        );
        assert!(
            claude_specs.iter().any(|spec| {
                spec.event == "PreToolUse" && spec.matcher == Some("AskUserQuestion")
            })
        );
        assert!(claude_specs.iter().any(|spec| {
            spec.event == "PostToolUse" && spec.matcher == Some("AskUserQuestion")
        }));
        assert!(claude_specs.iter().any(|spec| {
            spec.event == "PostToolUseFailure" && spec.matcher == Some("AskUserQuestion")
        }));
        assert!(claude_specs.iter().any(|spec| spec.event == "Elicitation"));
        assert!(
            claude_specs
                .iter()
                .any(|spec| spec.event == "ElicitationResult")
        );
        assert!(
            !claude_specs
                .iter()
                .any(|spec| spec.event == "SubagentStop" && spec.matcher.is_none())
        );
        assert!(
            retired_provider_specs(AgentKind::Claude)
                .iter()
                .any(|spec| spec.event == "SubagentStop" && spec.matcher.is_none())
        );
    }

    #[test]
    fn helper_resolution_requires_an_executable_on_the_hook_path() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let helper = tmp.path().join("agent-session");
        fs::write(&helper, "fixture").expect("helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o600)).expect("non-executable");
        assert!(!command_resolves_on_path(
            "agent-session",
            Some(tmp.path().as_os_str())
        ));
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("executable");
        assert!(command_resolves_on_path(
            "agent-session",
            Some(tmp.path().as_os_str())
        ));
        assert!(!command_resolves_on_path("agent-session", None));
    }

    #[test]
    fn codex_provider_plans_rollback_when_the_second_file_changes() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let hooks_path = tmp.path().join("hooks.json");
        let notification_path = tmp.path().join("config.toml");
        let hooks = plan_json_provider(AgentKind::Codex, &hooks_path, SetupAction::Apply)
            .expect("hooks plan");
        let notification = plan_codex_notification(&notification_path, SetupAction::Apply)
            .expect("notification plan");

        let concurrent = b"notify = [\"user-notifier\"]\n";
        fs::write(&notification_path, concurrent).expect("concurrent notification config");
        let error = apply_codex_provider_plans(&hooks, &notification.config)
            .expect_err("second-file change must fail the transaction");

        assert_eq!(error.code(), "provider-config-concurrent-modification");
        assert!(!hooks_path.exists(), "first-file write must be rolled back");
        assert_eq!(
            fs::read(&notification_path).expect("concurrent config retained"),
            concurrent
        );
    }

    #[test]
    fn codex_provider_plans_guard_an_unchanged_second_file() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let hooks_path = tmp.path().join("hooks.json");
        let notification_path = tmp.path().join("config.toml");
        fs::write(
            &notification_path,
            "notify = [\"agent-session\", \"activity\", \"notify\", \"--agent\", \"codex\"]\n",
        )
        .expect("owned notification config");
        let hooks = plan_json_provider(AgentKind::Codex, &hooks_path, SetupAction::Apply)
            .expect("hooks plan");
        let notification = plan_codex_notification(&notification_path, SetupAction::Apply)
            .expect("notification plan");
        assert!(!notification.config.changed);

        let concurrent = b"notify = [\"user-notifier\"]\n";
        fs::write(&notification_path, concurrent).expect("concurrent notification config");
        let error = apply_codex_provider_plans(&hooks, &notification.config)
            .expect_err("unchanged second-file plan must still verify its snapshot");

        assert_eq!(error.code(), "provider-config-concurrent-modification");
        assert!(!hooks_path.exists(), "first-file write must be rolled back");
        assert_eq!(
            fs::read(&notification_path).expect("concurrent config retained"),
            concurrent
        );
    }

    #[test]
    fn codex_provider_plan_failure_reports_the_retained_recovery_path() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let hooks_path = tmp.path().join("hooks.json");
        let notification_path = tmp.path().join("config.toml");
        fs::write(&hooks_path, "{}\n").expect("original hooks config");
        let hooks = plan_json_provider(AgentKind::Codex, &hooks_path, SetupAction::Apply)
            .expect("hooks plan");
        let notification = plan_codex_notification(&notification_path, SetupAction::Apply)
            .expect("notification plan");
        fs::write(&notification_path, "notify = [\"concurrent\"]\n")
            .expect("concurrent notification config");

        let error =
            apply_codex_provider_plans_with_rollback(&hooks, &notification.config, |plan| {
                rollback_provider_config_plan_after_capture_with_restore(
                    plan,
                    || {},
                    |_, _, _| {
                        Err(CliError::runtime(
                            "provider-config-rollback-write-failed",
                            "injected restore failure",
                            None,
                        ))
                    },
                )
            })
            .expect_err("two-file failure must surface rollback recovery metadata");

        assert_eq!(error.code(), "provider-config-rollback-failed");
        let recovery_path = PathBuf::from(
            error.0.details.as_ref().expect("error details")["rollback_error_details"]
                ["recovery_path"]
                .as_str()
                .expect("recovery path"),
        );
        assert!(recovery_path.is_file());
        assert_eq!(
            fs::read(recovery_path).expect("captured candidate"),
            hooks.updated_bytes.clone().expect("candidate bytes")
        );
    }

    #[test]
    fn codex_inline_migration_restores_deleted_json_when_config_changes() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let hooks_path = tmp.path().join("hooks.json");
        let config_path = tmp.path().join("config.toml");
        let initial = plan_json_provider(AgentKind::Codex, &hooks_path, SetupAction::Apply)
            .expect("initial JSON plan");
        apply_provider_config_plan(&initial).expect("initial JSON config");
        let hooks_before = fs::read(&hooks_path).expect("hooks before migration");
        fs::write(
            &config_path,
            "[[hooks.Stop]]\n\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"user-stop\"\ntimeout = 9\n",
        )
        .expect("inline config");

        let hooks = plan_codex_json_cleanup(
            &hooks_path,
            read_optional_provider_config(&hooks_path).expect("hooks snapshot"),
        )
        .expect("JSON cleanup plan");
        assert!(hooks.changed);
        assert!(hooks.updated_bytes.is_none());
        let mut notification =
            plan_codex_notification(&config_path, SetupAction::Apply).expect("config plan");
        assert!(
            plan_inline_codex_hooks(&mut notification, SetupAction::Apply)
                .expect("inline hook plan")
        );

        let concurrent = b"model = \"concurrent\"\n";
        fs::write(&config_path, concurrent).expect("concurrent config");
        let error = apply_codex_provider_plans(&hooks, &notification.config)
            .expect_err("second-file change must roll back the JSON deletion");

        assert_eq!(error.code(), "provider-config-concurrent-modification");
        assert_eq!(fs::read(&hooks_path).expect("restored hooks"), hooks_before);
        assert_eq!(
            fs::read(&config_path).expect("concurrent config retained"),
            concurrent
        );
    }

    #[test]
    fn provider_config_delete_preserves_a_replacement_created_after_capture() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("hooks.json");
        let reviewed = b"reviewed-owned-config";
        let replacement = b"concurrent-user-config";
        fs::write(&path, reviewed).expect("reviewed config");

        let error =
            remove_provider_config_if_unchanged_after_capture(&path, Some(reviewed), || {
                fs::write(&path, replacement).expect("concurrent replacement")
            })
            .expect_err("replacement must make deletion fail closed");

        assert_eq!(error.code(), "provider-config-concurrent-modification");
        assert_eq!(fs::read(&path).expect("replacement retained"), replacement);
    }

    #[test]
    fn provider_config_rollback_preserves_a_replacement_created_after_capture() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("hooks.json");
        let plan =
            plan_json_provider(AgentKind::Codex, &path, SetupAction::Apply).expect("create plan");
        apply_provider_config_plan(&plan).expect("created config");
        let replacement = b"concurrent-user-config";

        let error = rollback_provider_config_plan_after_capture(&plan, || {
            fs::write(&path, replacement).expect("concurrent replacement");
        })
        .expect_err("replacement must make rollback fail closed");

        assert_eq!(
            error.code(),
            "provider-config-rollback-concurrent-modification"
        );
        assert_eq!(fs::read(&path).expect("replacement retained"), replacement);
    }

    #[test]
    fn codex_marker_lines_inside_multiline_strings_are_not_owned_blocks() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        for quotes in ["\"\"\"", "'''"] {
            let raw = format!(
                "note = {quotes}\n{CODEX_HOOK_BLOCK_START}\nprivate\n{CODEX_HOOK_BLOCK_END}\n{quotes}\n"
            );

            assert_eq!(
                strip_owned_codex_toml_hook_block(&path, &raw).expect("marker-shaped value"),
                raw
            );
        }
    }

    #[test]
    fn provider_config_restore_failure_preserves_the_staged_original() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("hooks.json");
        let original = b"original-user-config";

        let error = restore_provider_config_if_absent_with_link(
            &path,
            original,
            "rollback-restore",
            |_, _| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected hard-link failure",
                ))
            },
        )
        .expect_err("unexpected hard-link failure must retain recovery bytes");

        assert_eq!(error.code(), "provider-config-rollback-write-failed");
        let recovery_path = PathBuf::from(
            error.0.details.as_ref().expect("error details")["recovery_path"]
                .as_str()
                .expect("recovery path"),
        );
        assert_eq!(fs::read(recovery_path).expect("staged original"), original);
        assert!(!path.exists());
    }

    #[test]
    fn provider_config_rollback_failure_preserves_the_captured_candidate() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("hooks.json");
        let original = b"original-user-config".to_vec();
        let candidate = b"applied-agent-config".to_vec();
        fs::write(&path, &candidate).expect("candidate config");
        let plan = ProviderConfigPlan {
            path: path.clone(),
            original_bytes: Some(original),
            updated_bytes: Some(candidate.clone()),
            changed: true,
            configured: true,
        };

        let error = rollback_provider_config_plan_after_capture_with_restore(
            &plan,
            || {},
            |_, _, _| {
                Err(CliError::runtime(
                    "provider-config-rollback-write-failed",
                    "injected restore failure",
                    None,
                ))
            },
        )
        .expect_err("restore failure must retain the captured candidate");

        assert_eq!(error.code(), "provider-config-rollback-write-failed");
        assert!(!path.exists());
        let recoveries = fs::read_dir(tmp.path())
            .expect("recovery directory")
            .map(|entry| entry.expect("recovery entry").path())
            .filter(|entry| {
                entry
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains("rollback-capture"))
            })
            .collect::<Vec<_>>();
        assert_eq!(recoveries.len(), 1);
        assert_eq!(
            fs::read(&recoveries[0]).expect("captured candidate"),
            candidate
        );
    }

    #[test]
    fn codex_inline_permission_source_guard_rejects_duplicate_reporters() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, render_owned_codex_toml_hook_block()).expect("owned inline hooks");
        assert!(codex_toml_permission_source_guard(&config_path));

        for command in [
            owned_command(AgentKind::Codex, Some("PermissionRequest")),
            "agent-session activity hook --agent=codex".to_string(),
        ] {
            let mut duplicate = render_owned_codex_toml_hook_block();
            duplicate.push_str(&format!(
                "\n[[hooks.PermissionRequest]]\n\n[[hooks.PermissionRequest.hooks]]\ntype = \"command\"\ncommand = {}\ntimeout = 5\n",
                TomlValue::from(command)
            ));
            fs::write(&config_path, duplicate).expect("duplicate reporter");
            assert!(!codex_toml_permission_source_guard(&config_path));
        }
    }

    #[test]
    fn codex_json_permission_source_guard_rejects_agent_equals_reporter() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("hooks.json");
        let plan =
            plan_json_provider(AgentKind::Codex, &path, SetupAction::Apply).expect("JSON plan");
        apply_provider_config_plan(&plan).expect("JSON hooks");
        assert!(codex_json_permission_source_guard(&path));

        let mut value: Value =
            serde_json::from_slice(&fs::read(&path).expect("JSON hooks")).expect("JSON document");
        value["hooks"]["PermissionRequest"][0]["hooks"]
            .as_array_mut()
            .expect("PermissionRequest handlers")
            .push(json!({
                "type": "command",
                "command": "agent-session activity hook --agent=codex",
                "timeout": 5
            }));
        fs::write(
            &path,
            serde_json::to_vec_pretty(&value).expect("JSON bytes"),
        )
        .expect("duplicate reporter");

        assert!(!codex_json_permission_source_guard(&path));
    }

    #[test]
    fn doctor_selects_the_newest_active_runtime_diagnostic() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created) = test_session(&tmp);
        let write_diagnostic =
            |record: &SessionRecord, observed_at: &str, code: &str, runtime_id: &str| {
                let diagnostic = ActivityDiagnostic {
                    schema_version: "agent-session.activity-diagnostic.v1".to_string(),
                    provider: record.agent.clone(),
                    runtime_id: runtime_id.to_string(),
                    runtime_generation: record.runtime.as_ref().expect("runtime").generation,
                    code: code.to_string(),
                    observed_at: observed_at.to_string(),
                };
                write_atomic(
                    &session_dir(&context, &record.id).join(ACTIVITY_DIAGNOSTIC_FILE),
                    &serde_json::to_vec_pretty(&diagnostic).expect("diagnostic json"),
                    SECRET_FILE_MODE,
                )
                .expect("diagnostic");
            };
        let first_runtime = created
            .record
            .runtime
            .as_ref()
            .expect("first runtime")
            .launch_id
            .clone();
        write_diagnostic(
            &created.record,
            "2026-07-10T00:01:00Z",
            "older-error",
            &first_runtime,
        );

        let mut second = created.record.clone();
        second.id = "activity-test-2".to_string();
        second.tmux_session = "activity-test-2".to_string();
        let runtime = second.runtime.as_mut().expect("second runtime");
        runtime.launch_id = "runtime-second".to_string();
        runtime.generation = 2;
        fs::create_dir_all(session_dir(&context, &second.id)).expect("second session dir");
        write_session_record(&context, &second).expect("second record");
        write_diagnostic(
            &second,
            "2026-07-10T00:02:00Z",
            "newer-error",
            "runtime-second",
        );

        let mut stale = second.clone();
        stale.id = "activity-test-3".to_string();
        stale.tmux_session = "activity-test-3".to_string();
        stale.runtime.as_mut().expect("stale runtime").launch_id = "runtime-third".to_string();
        fs::create_dir_all(session_dir(&context, &stale.id)).expect("third session dir");
        write_session_record(&context, &stale).expect("third record");
        write_diagnostic(
            &stale,
            "2026-07-10T00:03:00Z",
            "stale-error",
            "wrong-runtime",
        );

        let summary = latest_provider_activity(&context)
            .remove("codex")
            .expect("Codex summary");
        assert_eq!(
            summary.last_error,
            Some((
                "2026-07-10T00:02:00Z".to_string(),
                "newer-error".to_string()
            ))
        );
    }

    #[test]
    fn journal_is_bounded_and_metadata_only() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join(ACTIVITY_JOURNAL_FILE);
        for index in 0..(MAX_JOURNAL_EVENTS + 20) {
            let event = event(TurnEventKind::Progress, &format!("event-{index}"));
            append_journal(&path, &event, "2026-07-10T00:00:00Z").expect("append");
        }
        let contents = fs::read_to_string(&path).expect("journal");
        assert!(contents.lines().count() <= MAX_JOURNAL_EVENTS);
        assert!(contents.len() <= MAX_JOURNAL_BYTES);
        assert!(!contents.contains("prompt"));
        assert!(!contents.contains("tool_input"));
    }

    #[test]
    fn journal_idempotency_is_scoped_to_the_runtime_generation() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join(ACTIVITY_JOURNAL_FILE);
        let first = event(TurnEventKind::Progress, "reused-event-id");
        append_journal(&path, &first, "2026-07-10T00:00:00Z").expect("first runtime");
        let mut second = first;
        second.runtime_id = "runtime-2".to_string();
        append_journal(&path, &second, "2026-07-10T00:01:00Z").expect("second runtime");
        let entries = fs::read_to_string(path).expect("journal");
        assert_eq!(entries.matches("reused-event-id").count(), 2);
        assert!(entries.contains("runtime-1"));
        assert!(entries.contains("runtime-2"));
    }

    #[test]
    fn frozen_normalized_fixtures_parse_and_contain_no_content_keys() {
        let events = include_str!("../tests/fixtures/activity/turn-events.jsonl");
        for line in events.lines() {
            let event: TurnEvent = serde_json::from_str(line).expect("turn event fixture");
            validate_event(&event).expect("valid turn event fixture");
        }
        let states: Vec<TurnState> =
            serde_json::from_str(include_str!("../tests/fixtures/activity/turn-states.json"))
                .expect("turn state fixtures");
        assert_eq!(states.len(), 3);
        assert_eq!(
            states[1]
                .current_turn
                .as_ref()
                .and_then(|turn| turn.last_progress_at.as_deref()),
            Some("2026-07-10T00:00:03Z")
        );

        for fixture in [
            include_str!("../tests/fixtures/activity/codex-events.jsonl"),
            include_str!("../tests/fixtures/activity/claude-events.jsonl"),
            include_str!("../tests/fixtures/activity/hermes-events.jsonl"),
            events,
        ] {
            for forbidden in [
                "\"prompt\":",
                "assistant_response",
                "last_assistant_message",
                "tool_input",
                "tool_response",
                "transcript_path",
                "\"command\":",
                "\"questions\":",
                "\"options\":",
                "\"answers\":",
                "token",
            ] {
                assert!(!fixture.contains(forbidden), "forbidden key {forbidden}");
            }
        }
    }
}

fn validate_event(event: &TurnEvent) -> Result<(), CliError> {
    if event.schema_version != TURN_EVENT_VERSION {
        return Err(CliError::data(
            "unsupported-turn-event-version",
            format!(
                "unsupported turn event schema {}; expected {TURN_EVENT_VERSION}",
                event.schema_version
            ),
            None,
        ));
    }
    for (name, value) in [
        ("event_id", Some(event.event_id.as_str())),
        ("runtime_id", Some(event.runtime_id.as_str())),
        ("provider", Some(event.provider.as_str())),
        ("provider_session_id", event.provider_session_id.as_deref()),
        ("provider_turn_id", event.provider_turn_id.as_deref()),
        ("attention_id", event.attention_id.as_deref()),
    ] {
        if let Some(value) = value
            && (value.is_empty()
                || value.chars().count() > MAX_ID_CHARS
                || value.chars().any(char::is_control))
        {
            return Err(CliError::data(
                "activity-event-field-invalid",
                format!("activity event field {name} is empty, too long, or contains controls"),
                Some(json!({ "field": name })),
            ));
        }
    }
    if !matches!(event.provider.as_str(), "codex" | "claude" | "hermes") {
        return Err(CliError::data(
            "activity-provider-unsupported",
            "activity event provider must be codex, claude, or hermes",
            None,
        ));
    }
    match event.kind {
        TurnEventKind::AttentionRequested => {
            if event.attention_id.is_none() || event.attention_kind.is_none() {
                return Err(CliError::data(
                    "activity-attention-invalid",
                    "attention_requested requires attention_id and attention_kind",
                    None,
                ));
            }
        }
        TurnEventKind::AttentionCleared if event.attention_id.is_none() => {
            return Err(CliError::data(
                "activity-attention-invalid",
                "attention_cleared requires a correlated attention_id",
                None,
            ));
        }
        _ => {}
    }
    if let Some(kind) = event.attention_kind.as_deref()
        && !matches!(
            kind,
            "approval" | "clarification" | "authentication" | "other"
        )
    {
        return Err(CliError::data(
            "activity-attention-kind-invalid",
            "attention kind must be approval, clarification, authentication, or other",
            None,
        ));
    }
    if let Some(reason) = event.failure_reason.as_deref()
        && (event.kind != TurnEventKind::TurnFailed
            || event.confidence != Confidence::Authoritative
            || !matches!(
                reason,
                "usage_exhausted"
                    | "authentication"
                    | "organization"
                    | "billing"
                    | "invalid_request"
                    | "service"
                    | "max_output_tokens"
                    | "unknown"
            ))
    {
        return Err(CliError::data(
            "activity-failure-reason-invalid",
            "failure_reason requires an authoritative turn_failed event and an allowlisted value",
            None,
        ));
    }
    if let Some(provider_time) = event.provider_time.as_deref() {
        let parsed = provider_time.parse::<jiff::Timestamp>().map_err(|_| {
            CliError::data(
                "activity-provider-time-invalid",
                "provider_time must be a valid RFC 3339 timestamp",
                None,
            )
        })?;
        let skew = parsed
            .as_second()
            .saturating_sub(Zoned::now().timestamp().as_second())
            .unsigned_abs();
        if skew > 24 * 60 * 60 {
            return Err(CliError::data(
                "activity-provider-time-skewed",
                "provider_time is outside the accepted 24-hour skew window",
                None,
            ));
        }
    }
    Ok(())
}

fn reduce(document: &mut ActivityDocument, event: &TurnEvent, at: &str) {
    let previous_phase = document.state.phase.clone();
    let mut source = provider_source(event);
    source.extra = document.state.source.extra.clone();
    match event.kind {
        TurnEventKind::TurnStarted => {
            if let Some(current) = document.state.current_turn.take() {
                document.state.last_turn = Some(LastTurn {
                    provider_turn_id: current.provider_turn_id,
                    started_at: Some(current.started_at),
                    completed_at: at.to_string(),
                    outcome: "interrupted".to_string(),
                    extra: current.extra,
                });
            }
            document.pending_attention.clear();
            document.overflow_attention = None;
            document.state.current_turn = Some(CurrentTurn {
                provider_turn_id: event.provider_turn_id.clone(),
                started_at: at.to_string(),
                last_progress_at: None,
                attention: None,
                extra: Map::new(),
            });
            document.state.phase = TurnPhase::Working;
        }
        TurnEventKind::AttentionRequested => {
            if document.state.current_turn.is_none() {
                document.state.current_turn = Some(CurrentTurn {
                    provider_turn_id: event.provider_turn_id.clone(),
                    started_at: at.to_string(),
                    last_progress_at: None,
                    attention: None,
                    extra: Map::new(),
                });
            } else if let Some(current) = document.state.current_turn.as_mut()
                && current.provider_turn_id.is_none()
                && event.provider_turn_id.is_some()
            {
                current.provider_turn_id = event.provider_turn_id.clone();
            }
            let attention_id = event.attention_id.as_deref().unwrap_or_default();
            let duplicate_hermes_approval = event.attention_correlation_ambiguous
                && event.provider == AgentKind::Hermes.as_str()
                && event.attention_kind.as_deref() == Some("approval")
                && document
                    .pending_attention
                    .iter()
                    .any(|pending| pending.id == attention_id);
            if duplicate_hermes_approval {
                let kind = event
                    .attention_kind
                    .clone()
                    .unwrap_or_else(|| "other".to_string());
                if let Some(overflow) = document.overflow_attention.as_mut() {
                    overflow.count = overflow.count.saturating_add(1);
                } else {
                    document.overflow_attention = Some(OverflowAttention {
                        kind,
                        requested_at: at.to_string(),
                        count: 1,
                        extra: Map::new(),
                    });
                }
            } else if !document
                .pending_attention
                .iter()
                .any(|pending| pending.id == attention_id)
            {
                let kind = event
                    .attention_kind
                    .clone()
                    .unwrap_or_else(|| "other".to_string());
                if document.pending_attention.len() < MAX_PENDING_ATTENTION {
                    document.pending_attention.push(PendingAttention {
                        id: attention_id.to_string(),
                        kind,
                        requested_at: at.to_string(),
                        extra: Map::new(),
                    });
                } else if let Some(overflow) = document.overflow_attention.as_mut() {
                    overflow.count = overflow.count.saturating_add(1);
                } else {
                    document.overflow_attention = Some(OverflowAttention {
                        kind,
                        requested_at: at.to_string(),
                        count: 1,
                        extra: Map::new(),
                    });
                }
            }
            refresh_attention(document);
            document.state.phase = TurnPhase::NeedsInput;
        }
        TurnEventKind::AttentionCleared => {
            let attention_id = event.attention_id.as_deref().unwrap_or_default();
            let pending_before = document.pending_attention.len();
            document
                .pending_attention
                .retain(|pending| pending.id != attention_id);
            let matched = document.pending_attention.len() < pending_before;
            if matched
                && is_provider_progress_evidence(event)
                && let Some(current) = document.state.current_turn.as_mut()
            {
                advance_last_progress_at(current, at);
            }
            refresh_attention(document);
            document.state.phase =
                if document.pending_attention.is_empty() && document.overflow_attention.is_none() {
                    TurnPhase::Working
                } else {
                    TurnPhase::NeedsInput
                };
        }
        TurnEventKind::Progress => {
            if document.state.current_turn.is_none() {
                document.state.current_turn = Some(CurrentTurn {
                    provider_turn_id: event.provider_turn_id.clone(),
                    started_at: at.to_string(),
                    last_progress_at: None,
                    attention: None,
                    extra: Map::new(),
                });
            }
            if is_provider_progress_evidence(event)
                && let Some(current) = document.state.current_turn.as_mut()
            {
                advance_last_progress_at(current, at);
            }
            if document.pending_attention.is_empty() && document.overflow_attention.is_none() {
                document.state.phase = TurnPhase::Working;
            }
        }
        TurnEventKind::StopObserved => {
            // A raw Stop can race another matching hook that continues the turn.
            // Retain it in the journal/revision, but never fabricate Waiting.
        }
        TurnEventKind::TurnCompleted | TurnEventKind::TurnFailed => {
            let requires_exact_open_turn = event.provider == AgentKind::Codex.as_str()
                && event.kind == TurnEventKind::TurnCompleted
                && event.confidence == Confidence::Authoritative
                && event.source_kind == SourceKind::ProviderHook
                && event.provider_turn_id.is_some();
            let matches_current = if requires_exact_open_turn {
                document
                    .state
                    .current_turn
                    .as_ref()
                    .and_then(|turn| turn.provider_turn_id.as_ref())
                    == event.provider_turn_id.as_ref()
            } else if let Some(current) = document.state.current_turn.as_ref() {
                event.provider_turn_id.is_none()
                    || current.provider_turn_id.is_none()
                    || event.provider_turn_id == current.provider_turn_id
            } else if let (Some(event_turn_id), Some(last_turn_id)) = (
                event.provider_turn_id.as_ref(),
                document
                    .state
                    .last_turn
                    .as_ref()
                    .and_then(|turn| turn.provider_turn_id.as_ref()),
            ) {
                event_turn_id == last_turn_id
            } else {
                true
            };
            if matches_current {
                let current = document.state.current_turn.take();
                let extra = current
                    .as_ref()
                    .map(|turn| turn.extra.clone())
                    .or_else(|| {
                        document
                            .state
                            .last_turn
                            .as_ref()
                            .map(|turn| turn.extra.clone())
                    })
                    .unwrap_or_default();
                document.state.last_turn = Some(LastTurn {
                    provider_turn_id: event.provider_turn_id.clone().or_else(|| {
                        current
                            .as_ref()
                            .and_then(|turn| turn.provider_turn_id.clone())
                    }),
                    started_at: current.map(|turn| turn.started_at),
                    completed_at: at.to_string(),
                    outcome: if event.kind == TurnEventKind::TurnFailed {
                        "failed".to_string()
                    } else {
                        "completed".to_string()
                    },
                    extra,
                });
                document.pending_attention.clear();
                document.overflow_attention = None;
                document.state.phase = TurnPhase::Waiting;
            }
        }
    }
    document.state.revision = document.state.revision.saturating_add(1);
    document.state.source = source;
    if document.state.phase != previous_phase {
        document.state.phase_changed_at = at.to_string();
    }
}

fn is_provider_progress_evidence(event: &TurnEvent) -> bool {
    event.source_kind == SourceKind::ProviderHook
}

fn advance_last_progress_at(current: &mut CurrentTurn, at: &str) {
    let Ok(candidate) = at.parse::<jiff::Timestamp>() else {
        return;
    };
    let should_advance = current
        .last_progress_at
        .as_deref()
        .and_then(|value| value.parse::<jiff::Timestamp>().ok())
        .is_none_or(|previous| candidate > previous);
    if should_advance {
        current.last_progress_at = Some(at.to_string());
    }
}

fn refresh_attention(document: &mut ActivityDocument) {
    let prior_extra = document
        .state
        .current_turn
        .as_ref()
        .and_then(|current| current.attention.as_ref())
        .map(|attention| attention.extra.clone())
        .unwrap_or_default();
    let attention = document
        .pending_attention
        .first()
        .map(|pending| AttentionView {
            kind: pending.kind.clone(),
            requested_at: pending.requested_at.clone(),
            pending_count: document.pending_attention.len().saturating_add(
                document
                    .overflow_attention
                    .as_ref()
                    .map_or(0, |overflow| overflow.count),
            ),
            extra: if pending.extra.is_empty() {
                prior_extra.clone()
            } else {
                pending.extra.clone()
            },
        })
        .or_else(|| {
            document
                .overflow_attention
                .as_ref()
                .map(|overflow| AttentionView {
                    kind: overflow.kind.clone(),
                    requested_at: overflow.requested_at.clone(),
                    pending_count: overflow.count,
                    extra: if overflow.extra.is_empty() {
                        prior_extra.clone()
                    } else {
                        overflow.extra.clone()
                    },
                })
        });
    if let Some(current) = document.state.current_turn.as_mut() {
        current.attention = attention;
    }
}

fn read_document(path: &Path) -> Result<ActivityDocument, CliError> {
    let contents =
        fs::read(path).map_err(|err| activity_io_error("activity-read-failed", path, err))?;
    let document: ActivityDocument = serde_json::from_slice(&contents).map_err(|err| {
        CliError::data(
            "activity-json-invalid",
            format!("failed to parse {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    if document.schema_version != ACTIVITY_DOCUMENT_VERSION
        || document.state.schema_version != TURN_STATE_VERSION
    {
        return Err(CliError::data(
            "activity-version-unsupported",
            "activity snapshot uses an unsupported schema version",
            Some(json!({ "path": display_path(path) })),
        ));
    }
    Ok(document)
}

fn write_document(path: &Path, document: &ActivityDocument) -> Result<(), CliError> {
    let bytes = serde_json::to_vec_pretty(document).map_err(|err| {
        CliError::runtime(
            "activity-render-failed",
            format!("failed to render activity snapshot: {err}"),
            None,
        )
    })?;
    write_atomic(path, &bytes, SECRET_FILE_MODE).map_err(|err| {
        CliError::runtime(
            "activity-write-failed",
            format!("activity storage failed at {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JournalEntry {
    received_at: String,
    event: TurnEvent,
}

#[cfg(test)]
fn append_journal(path: &Path, event: &TurnEvent, received_at: &str) -> Result<(), CliError> {
    append_journal_entry(
        path,
        JournalEntry {
            received_at: received_at.to_string(),
            event: event.clone(),
        },
    )
}

fn append_journal_entry(path: &Path, entry: JournalEntry) -> Result<(), CliError> {
    let mut entries = if path.is_file() {
        fs::read_to_string(path)
            .map_err(|err| activity_io_error("activity-journal-read-failed", path, err))?
            .lines()
            .filter_map(|line| serde_json::from_str::<JournalEntry>(line).ok())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if entries.iter().any(|existing| {
        existing.event.runtime_id == entry.event.runtime_id
            && existing.event.event_id == entry.event.event_id
    }) {
        return Ok(());
    }
    entries.push(entry);
    if entries.len() > MAX_JOURNAL_EVENTS {
        let drop_count = entries.len() - MAX_JOURNAL_EVENTS;
        entries.drain(0..drop_count);
    }
    loop {
        let mut bytes = Vec::new();
        for entry in &entries {
            serde_json::to_writer(&mut bytes, entry).map_err(|err| {
                CliError::runtime(
                    "activity-journal-render-failed",
                    format!("failed to render activity journal: {err}"),
                    None,
                )
            })?;
            bytes.push(b'\n');
        }
        if bytes.len() <= MAX_JOURNAL_BYTES || entries.len() <= 1 {
            return write_atomic(path, &bytes, SECRET_FILE_MODE).map_err(|err| {
                CliError::runtime(
                    "activity-journal-write-failed",
                    format!("activity journal write failed at {}: {err}", path.display()),
                    Some(json!({ "path": display_path(path) })),
                )
            });
        }
        entries.remove(0);
    }
}

fn repair_pending_transaction(
    document_path: &Path,
    journal_path: &Path,
    replay_path: &Path,
    document: &mut ActivityDocument,
) -> Result<(), CliError> {
    let Some(entry) = document.pending_journal.clone() else {
        return Ok(());
    };
    if event_uses_exact_replay_horizon(&entry.event) {
        let key = event_dedupe_key(&entry.event.runtime_id, &entry.event.event_id);
        replay_insert(
            replay_path,
            &document.runtime_id,
            document.runtime_generation,
            false,
            &key,
        )?;
    }
    append_journal_entry(journal_path, entry)?;
    document.pending_journal = None;
    write_document(document_path, document)
}

fn initialize_replay_index(
    path: &Path,
    runtime_id: &str,
    runtime_generation: u64,
) -> Result<(), CliError> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(activity_io_error("activity-replay-reset-failed", path, err)),
    }
    let _ = open_replay_index(path, runtime_id, runtime_generation, true)?;
    sync_parent_directory(path)
}

fn replay_header(runtime_id: &str, runtime_generation: u64) -> [u8; REPLAY_HEADER_BYTES] {
    let mut header = [0_u8; REPLAY_HEADER_BYTES];
    header[..REPLAY_MAGIC.len()].copy_from_slice(REPLAY_MAGIC);
    let mut digest = Sha256::new();
    digest.update(b"agent-session.replay-runtime.v1\0");
    digest.update(runtime_id.as_bytes());
    header[16..48].copy_from_slice(&digest.finalize());
    header[48..56].copy_from_slice(&runtime_generation.to_be_bytes());
    header
}

fn sync_parent_directory(path: &Path) -> Result<(), CliError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|err| activity_io_error("activity-replay-directory-sync-failed", parent, err))
}

fn open_replay_index(
    path: &Path,
    runtime_id: &str,
    runtime_generation: u64,
    allow_create: bool,
) -> Result<fs::File, CliError> {
    let existed = path.is_file();
    if !existed && !allow_create {
        return Err(CliError::data(
            "activity-replay-missing",
            "activity replay index is missing for a nonempty durable activity snapshot",
            Some(json!({ "path": display_path(path) })),
        ));
    }
    let file = OpenOptions::new()
        .create(allow_create)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(SECRET_FILE_MODE)
        .open(path)
        .map_err(|err| activity_io_error("activity-replay-open-failed", path, err))?;
    fs::set_permissions(path, fs::Permissions::from_mode(SECRET_FILE_MODE))
        .map_err(|err| activity_io_error("activity-replay-permission-failed", path, err))?;
    let expected_len = (REPLAY_HEADER_BYTES + REPLAY_SLOT_COUNT * REPLAY_SLOT_BYTES) as u64;
    let actual_len = file
        .metadata()
        .map_err(|err| activity_io_error("activity-replay-metadata-failed", path, err))?
        .len();
    if actual_len == 0 && allow_create {
        file.set_len(expected_len)
            .map_err(|err| activity_io_error("activity-replay-size-failed", path, err))?;
        let mut writer = &file;
        writer
            .seek(SeekFrom::Start(0))
            .and_then(|_| writer.write_all(&replay_header(runtime_id, runtime_generation)))
            .and_then(|()| writer.sync_data())
            .map_err(|err| activity_io_error("activity-replay-header-write-failed", path, err))?;
        if !existed {
            sync_parent_directory(path)?;
        }
    } else if actual_len != expected_len {
        return Err(CliError::data(
            "activity-replay-size-invalid",
            "activity replay index has an unexpected size",
            Some(json!({ "path": display_path(path), "expected_bytes": expected_len })),
        ));
    } else {
        let mut reader = &file;
        let mut observed = [0_u8; REPLAY_HEADER_BYTES];
        reader
            .seek(SeekFrom::Start(0))
            .and_then(|_| reader.read_exact(&mut observed))
            .map_err(|err| activity_io_error("activity-replay-header-read-failed", path, err))?;
        if observed != replay_header(runtime_id, runtime_generation) {
            return Err(CliError::data(
                "activity-replay-runtime-mismatch",
                "activity replay index does not match the durable runtime generation",
                Some(json!({ "path": display_path(path) })),
            ));
        }
    }
    Ok(file)
}

fn replay_slot(key: &[u8; REPLAY_SLOT_BYTES], probe: usize) -> u64 {
    let start = u64::from_be_bytes(key[..8].try_into().expect("eight-byte replay prefix"));
    (REPLAY_HEADER_BYTES + (start as usize + probe) % REPLAY_SLOT_COUNT * REPLAY_SLOT_BYTES) as u64
}

fn replay_contains(
    path: &Path,
    runtime_id: &str,
    runtime_generation: u64,
    allow_create: bool,
    key: &[u8; REPLAY_SLOT_BYTES],
) -> Result<bool, CliError> {
    let mut file = open_replay_index(path, runtime_id, runtime_generation, allow_create)?;
    let mut slot = [0_u8; REPLAY_SLOT_BYTES];
    for probe in 0..REPLAY_SLOT_COUNT {
        file.seek(SeekFrom::Start(replay_slot(key, probe)))
            .map_err(|err| activity_io_error("activity-replay-seek-failed", path, err))?;
        file.read_exact(&mut slot)
            .map_err(|err| activity_io_error("activity-replay-read-failed", path, err))?;
        if slot.iter().all(|byte| *byte == 0) {
            return Ok(false);
        }
        if &slot == key {
            return Ok(true);
        }
    }
    Ok(false)
}

fn replay_insert(
    path: &Path,
    runtime_id: &str,
    runtime_generation: u64,
    allow_create: bool,
    key: &[u8; REPLAY_SLOT_BYTES],
) -> Result<(), CliError> {
    let mut file = open_replay_index(path, runtime_id, runtime_generation, allow_create)?;
    let mut slot = [0_u8; REPLAY_SLOT_BYTES];
    for probe in 0..REPLAY_SLOT_COUNT {
        let offset = replay_slot(key, probe);
        file.seek(SeekFrom::Start(offset))
            .map_err(|err| activity_io_error("activity-replay-seek-failed", path, err))?;
        file.read_exact(&mut slot)
            .map_err(|err| activity_io_error("activity-replay-read-failed", path, err))?;
        if &slot == key {
            return Ok(());
        }
        if slot.iter().all(|byte| *byte == 0) {
            file.seek(SeekFrom::Start(offset))
                .map_err(|err| activity_io_error("activity-replay-seek-failed", path, err))?;
            file.write_all(key)
                .map_err(|err| activity_io_error("activity-replay-write-failed", path, err))?;
            file.sync_data()
                .map_err(|err| activity_io_error("activity-replay-sync-failed", path, err))?;
            return Ok(());
        }
    }
    Err(CliError::data(
        "activity-replay-index-full",
        "activity replay index is full for this runtime generation",
        Some(json!({ "path": display_path(path) })),
    ))
}

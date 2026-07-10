use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::Command as ProcessCommand;

use jiff::Zoned;
use nils_common::fs::{SECRET_FILE_MODE, display_path, home_dir, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::cli::AgentKind;
use crate::{CliContext, CliError, SessionRecord, load_session_record, session_dir};

pub(crate) const TURN_EVENT_VERSION: &str = "agent-session.turn-event.v1";
pub(crate) const TURN_STATE_VERSION: &str = "agent-session.turn-state.v1";
const ACTIVITY_DOCUMENT_VERSION: &str = "agent-session.activity.v1";
const ACTIVITY_FILE: &str = "activity.json";
const ACTIVITY_JOURNAL_FILE: &str = "activity.journal.jsonl";
const ACTIVITY_DIAGNOSTIC_FILE: &str = "activity.diagnostic.json";
const ACTIVITY_LOCK_FILE: &str = ".activity.lock";
const MAX_EVENT_BYTES: u64 = 64 * 1024;
const MAX_JOURNAL_EVENTS: usize = 256;
const MAX_JOURNAL_BYTES: usize = 64 * 1024;
const MAX_ID_CHARS: usize = 256;

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
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct AttentionView {
    pub(crate) kind: String,
    pub(crate) requested_at: String,
    pub(crate) pending_count: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CurrentTurn {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider_turn_id: Option<String>,
    pub(crate) started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) attention: Option<AttentionView>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LastTurn {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider_turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) started_at: Option<String>,
    pub(crate) completed_at: String,
    pub(crate) outcome: String,
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
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct PendingAttention {
    id: String,
    kind: String,
    requested_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ActivityDocument {
    schema_version: String,
    runtime_id: String,
    state: TurnState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_attention: Vec<PendingAttention>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    seen_event_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_event_at: Option<String>,
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
    pub(crate) attention_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) attention_kind: Option<String>,
    #[serde(default)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetupAction {
    DryRun,
    Apply,
    Remove,
    Repair,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ProviderDoctor {
    pub(crate) provider: String,
    pub(crate) classification: String,
    pub(crate) version: Option<String>,
    pub(crate) configured: bool,
    pub(crate) config_path: String,
    pub(crate) completion: String,
    pub(crate) attention_correlation: String,
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
    pub(crate) configured: bool,
    pub(crate) config_path: String,
    pub(crate) owned_events: Vec<String>,
    pub(crate) trust: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ActivityDiagnostic {
    schema_version: String,
    provider: String,
    code: String,
    observed_at: String,
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

fn acquire_lock(dir: &Path) -> Result<ActivityLock, CliError> {
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
    let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if status != 0 {
        return Err(activity_io_error(
            "activity-lock-failed",
            &path,
            io::Error::last_os_error(),
        ));
    }
    Ok(ActivityLock(file))
}

fn activity_io_error(code: &str, path: &Path, err: io::Error) -> CliError {
    CliError::runtime(
        code,
        format!("activity storage failed at {}: {err}", path.display()),
        Some(json!({ "path": display_path(path) })),
    )
}

fn now() -> String {
    Zoned::now().timestamp().to_string()
}

fn runtime_source() -> TurnSource {
    TurnSource {
        kind: SourceKind::Runtime,
        provider: None,
        confidence: Confidence::Authoritative,
    }
}

fn provider_source(event: &TurnEvent) -> TurnSource {
    TurnSource {
        kind: event.source_kind.clone(),
        provider: Some(event.provider.clone()),
        confidence: event.confidence.clone(),
    }
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
    }
}

pub(crate) fn activate_runtime(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<TurnState, CliError> {
    let runtime_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            CliError::data(
                "runtime-id-missing",
                "session runtime is missing its launch id",
                Some(json!({ "id": record.id })),
            )
        })?;
    let dir = session_dir(context, &record.id);
    let _lock = acquire_lock(&dir)?;
    let path = dir.join(ACTIVITY_FILE);
    let existing = read_document(&path).ok();
    if let Some(existing) = existing.as_ref()
        && existing.runtime_id == runtime_id
    {
        return Ok(existing.state.clone());
    }
    let at = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.started_at.clone())
        .unwrap_or_else(now);
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
        });
    }
    let revision = existing
        .as_ref()
        .map(|document| document.state.revision.saturating_add(1))
        .unwrap_or(1);
    let document = ActivityDocument {
        schema_version: ACTIVITY_DOCUMENT_VERSION.to_string(),
        runtime_id: runtime_id.to_string(),
        state: starting_state(at, revision, last_turn),
        pending_attention: Vec::new(),
        seen_event_ids: Vec::new(),
        provider_session_id: existing.and_then(|document| document.provider_session_id),
        last_event_at: None,
    };
    write_document(&path, &document)?;
    Ok(document.state)
}

pub(crate) fn state_for_view(context: &CliContext, record: &SessionRecord) -> Option<TurnState> {
    let path = session_dir(context, &record.id).join(ACTIVITY_FILE);
    if !path.is_file() {
        return None;
    }
    match read_document(&path) {
        Ok(document) => Some(document.state),
        Err(_) => Some(TurnState {
            schema_version: TURN_STATE_VERSION.to_string(),
            phase: TurnPhase::Unknown,
            phase_changed_at: record.updated_at.clone(),
            revision: 0,
            source: runtime_source(),
            current_turn: None,
            last_turn: None,
        }),
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
    validate_event(&event)?;
    let record = load_session_record(context, id)?;
    if event.provider != record.agent {
        return Err(CliError::data(
            "activity-provider-mismatch",
            "activity event provider does not match the session provider",
            Some(json!({ "id": record.id, "provider": event.provider })),
        ));
    }
    let active_runtime_id = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if active_runtime_id.is_empty() || event.runtime_id != active_runtime_id {
        return Err(CliError::data(
            "runtime-id-mismatch",
            "activity event does not belong to the active runtime generation",
            Some(json!({ "id": record.id })),
        ));
    }

    let dir = session_dir(context, &record.id);
    let _lock = acquire_lock(&dir)?;
    let path = dir.join(ACTIVITY_FILE);
    let mut document = read_document(&path)?;
    if document.runtime_id != event.runtime_id {
        return Err(CliError::data(
            "runtime-id-mismatch",
            "activity snapshot does not match the active runtime generation",
            Some(json!({ "id": record.id })),
        ));
    }
    if document
        .seen_event_ids
        .iter()
        .any(|id| id == &event.event_id)
    {
        return Ok(ActivityResult {
            id: record.id,
            turn_state: document.state,
            duplicate: true,
        });
    }

    let received_at = now();
    reduce(&mut document, &event, &received_at);
    document.last_event_at = Some(received_at.clone());
    document.provider_session_id = event
        .provider_session_id
        .clone()
        .or(document.provider_session_id);
    document.seen_event_ids.push(event.event_id.clone());
    if document.seen_event_ids.len() > MAX_JOURNAL_EVENTS {
        let drop_count = document.seen_event_ids.len() - MAX_JOURNAL_EVENTS;
        document.seen_event_ids.drain(0..drop_count);
    }
    write_document(&path, &document)?;
    append_journal(&dir.join(ACTIVITY_JOURNAL_FILE), &event, &received_at)?;
    Ok(ActivityResult {
        id: record.id,
        turn_state: document.state,
        duplicate: false,
    })
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
    });
    Ok(ActivityResult {
        id: record.id,
        turn_state: state,
        duplicate: false,
    })
}

pub(crate) fn ingest_provider_hook(
    context: &CliContext,
    agent: AgentKind,
    event_override: Option<&str>,
) -> Result<(), CliError> {
    let Some(id) = std::env::var("AGENT_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };
    let Some(runtime_id) = std::env::var("AGENT_SESSION_RUNTIME_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
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
    let Some(event) = normalize_provider_hook(agent, event_override, &runtime_id, &raw)? else {
        return Ok(());
    };
    let _ = ingest_event(context, &id, event)?;
    Ok(())
}

pub(crate) fn ingest_provider_hook_fail_open(
    context: &CliContext,
    agent: AgentKind,
    event_override: Option<&str>,
) {
    match ingest_provider_hook(context, agent, event_override) {
        Ok(()) => clear_hook_diagnostic(context),
        Err(err) => record_hook_diagnostic(context, agent, err.code()),
    }
}

fn record_hook_diagnostic(context: &CliContext, agent: AgentKind, code: &str) {
    let Some(id) = std::env::var("AGENT_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    let Ok(record) = load_session_record(context, &id) else {
        return;
    };
    if record.agent != agent.as_str() {
        return;
    }
    let diagnostic = ActivityDiagnostic {
        schema_version: "agent-session.activity-diagnostic.v1".to_string(),
        provider: agent.as_str().to_string(),
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

fn clear_hook_diagnostic(context: &CliContext) {
    let Some(id) = std::env::var("AGENT_SESSION_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    if load_session_record(context, &id).is_ok() {
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
            (TurnEventKind::TurnFailed, None, Confidence::Observed)
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
            // Hermes does not expose a stable approval request id. Preserve the
            // conservative latch until completion or a new turn.
            (TurnEventKind::Progress, None, Confidence::Observed)
        }
        _ => return Ok(None),
    };
    let provider_session_id = raw
        .get("session_id")
        .or_else(|| raw.get("session_key"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let provider_turn_id = raw
        .get("turn_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let attention_id =
        (kind == TurnEventKind::AttentionRequested).then(|| uuid::Uuid::new_v4().to_string());
    Ok(Some(TurnEvent {
        schema_version: TURN_EVENT_VERSION.to_string(),
        event_id: uuid::Uuid::new_v4().to_string(),
        runtime_id: runtime_id.to_string(),
        provider: agent.as_str().to_string(),
        provider_session_id,
        provider_turn_id,
        kind,
        attention_id,
        attention_kind: attention_kind.map(str::to_string),
        confidence,
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
    let mut providers = Vec::new();
    for agent in agents {
        let path = provider_config_path(agent)?;
        let configured = provider_configured(agent, &path).unwrap_or(false);
        let (last_event_at, last_error) = latest_provider_activity(context, agent);
        let (classification, completion, attention_correlation, trust, guidance) = match agent {
            AgentKind::Codex => (
                "partial",
                "raw Stop is observed only because concurrent matching hooks may continue the turn",
                "PermissionRequest has no request id shared with PostToolUse; attention is conservatively latched",
                "Codex requires review/trust for each non-managed hook definition",
                "Run activity setup --agent codex --dry-run, apply it, then approve the hook definitions in Codex",
            ),
            AgentKind::Claude => (
                "partial",
                "idle_prompt is observed completion; raw Stop remains non-final because other hooks may continue",
                "PermissionRequest omits tool_use_id; attention is conservatively latched",
                "Claude settings hooks compose additively and execute with the user's permissions",
                "Run activity setup --agent claude --dry-run and then --apply",
            ),
            AgentKind::Hermes => (
                "supported",
                "post_llm_call is authoritative for successful non-interrupted turns on the supported version",
                "approval hooks expose no stable request id; attention clears on completion or a new turn",
                "Hermes shell hooks require first-use consent unless explicitly accepted",
                "Run activity setup --agent hermes --dry-run, apply it, then approve and verify with hermes hooks doctor",
            ),
        };
        providers.push(ProviderDoctor {
            provider: agent.as_str().to_string(),
            classification: classification.to_string(),
            version: provider_version(agent),
            configured,
            config_path: display_path(&path),
            completion: completion.to_string(),
            attention_correlation: attention_correlation.to_string(),
            trust: trust.to_string(),
            guidance: guidance.to_string(),
            last_event_at,
            last_error,
            helper_executable: std::env::current_exe()
                .ok()
                .and_then(|path| fs::metadata(path).ok())
                .is_some_and(|metadata| metadata.is_file()),
        });
    }
    Ok(DoctorResult { providers })
}

fn latest_provider_activity(
    context: &CliContext,
    agent: AgentKind,
) -> (Option<String>, Option<String>) {
    let root = context.state_dir.join("sessions");
    let Ok(entries) = fs::read_dir(root) else {
        return (None, None);
    };
    let mut latest = None;
    let mut error = None;
    for entry in entries.flatten() {
        let record_path = entry.path().join("session.json");
        let Ok(record_bytes) = fs::read(&record_path) else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<SessionRecord>(&record_bytes) else {
            continue;
        };
        if record.agent != agent.as_str() {
            continue;
        }
        let diagnostic_path = entry.path().join(ACTIVITY_DIAGNOSTIC_FILE);
        if let Ok(bytes) = fs::read(&diagnostic_path) {
            match serde_json::from_slice::<ActivityDiagnostic>(&bytes) {
                Ok(diagnostic)
                    if diagnostic.schema_version == "agent-session.activity-diagnostic.v1"
                        && diagnostic.provider == agent.as_str() =>
                {
                    error = Some(diagnostic.code);
                }
                _ => error = Some("activity diagnostic unreadable".to_string()),
            }
        }
        let activity_path = entry.path().join(ACTIVITY_FILE);
        if !activity_path.is_file() {
            continue;
        }
        match read_document(&activity_path) {
            Ok(document) => {
                if let Some(observed) = document.last_event_at
                    && latest.as_ref().is_none_or(|current| &observed > current)
                {
                    latest = Some(observed);
                }
            }
            Err(_) => error = Some("activity snapshot unreadable".to_string()),
        }
    }
    (latest, error)
}

pub(crate) fn setup(agent: AgentKind, action: SetupAction) -> Result<SetupResult, CliError> {
    let path = provider_config_path(agent)?;
    let (changed, configured) = match agent {
        AgentKind::Codex | AgentKind::Claude => setup_json_provider(agent, &path, action)?,
        AgentKind::Hermes => setup_hermes(&path, action)?,
    };
    let action_name = match action {
        SetupAction::DryRun => "dry-run",
        SetupAction::Apply => "apply",
        SetupAction::Remove => "remove",
        SetupAction::Repair => "repair",
    };
    Ok(SetupResult {
        provider: agent.as_str().to_string(),
        action: action_name.to_string(),
        changed,
        configured,
        config_path: display_path(&path),
        owned_events: provider_specs(agent)
            .into_iter()
            .map(|spec| spec.event.to_string())
            .collect(),
        trust: match agent {
            AgentKind::Codex => "approve the exact new Codex hook definitions before they run",
            AgentKind::Claude => "review the additive settings entries before apply",
            AgentKind::Hermes => {
                "approve each new (event, command) pair or use Hermes' explicit hook-consent flow"
            }
        }
        .to_string(),
    })
}

fn provider_version(agent: AgentKind) -> Option<String> {
    let mut command = ProcessCommand::new(match agent {
        AgentKind::Codex => "codex",
        AgentKind::Claude => "claude",
        AgentKind::Hermes => "hermes",
    });
    command.arg("--version");
    let output = command.output().ok()?;
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr)
    } else {
        String::from_utf8_lossy(&output.stdout)
    };
    text.lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(160).collect())
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

#[derive(Clone, Copy)]
struct ProviderSpec {
    event: &'static str,
    matcher: Option<&'static str>,
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
                event: "PostToolUse",
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
                matcher: Some("permission_prompt"),
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

fn owned_command(agent: AgentKind, event: Option<&str>) -> String {
    match event {
        Some(event) if agent == AgentKind::Hermes => {
            format!("agent-session activity hook --agent hermes --event {event}")
        }
        _ => format!("agent-session activity hook --agent {}", agent.as_str()),
    }
}

fn provider_configured(agent: AgentKind, path: &Path) -> Result<bool, CliError> {
    match agent {
        AgentKind::Codex | AgentKind::Claude => json_provider_configured(agent, path),
        AgentKind::Hermes => hermes_configured(path),
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
        .all(|spec| json_has_spec(&value, agent, *spec)))
}

fn setup_json_provider(
    agent: AgentKind,
    path: &Path,
    action: SetupAction,
) -> Result<(bool, bool), CliError> {
    let original = if path.is_file() {
        serde_json::from_slice::<Value>(
            &fs::read(path)
                .map_err(|err| activity_io_error("provider-config-read-failed", path, err))?,
        )
        .map_err(|err| {
            CliError::data(
                "provider-config-invalid",
                format!("failed to parse {}: {err}", path.display()),
                None,
            )
        })?
    } else {
        Value::Object(Map::new())
    };
    let mut updated = original.clone();
    if !updated.is_object() {
        return Err(CliError::data(
            "provider-config-invalid",
            "provider config root must be an object",
            Some(json!({ "path": display_path(path) })),
        ));
    }
    let remove = action == SetupAction::Remove;
    for spec in provider_specs(agent) {
        mutate_json_spec(&mut updated, agent, spec, remove)?;
    }
    let changed = updated != original;
    if changed && action != SetupAction::DryRun {
        write_provider_config(
            path,
            &serde_json::to_vec_pretty(&updated).map_err(|err| {
                CliError::runtime(
                    "provider-config-render-failed",
                    format!("failed to render provider config: {err}"),
                    None,
                )
            })?,
        )?;
    }
    let configured = provider_specs(agent)
        .iter()
        .all(|spec| json_has_spec(&updated, agent, *spec));
    Ok((changed, configured))
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
                                        == Some(owned_command(agent, None).as_str())
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
    let command = owned_command(agent, None);
    for group in groups.iter_mut() {
        if group.get("matcher").and_then(Value::as_str) != spec.matcher {
            continue;
        }
        if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
            handlers.retain(|handler| {
                !(handler.get("type").and_then(Value::as_str) == Some("command")
                    && handler.get("command").and_then(Value::as_str) == Some(command.as_str()))
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
            })
        })
}

fn setup_hermes(path: &Path, action: SetupAction) -> Result<(bool, bool), CliError> {
    let original = if path.is_file() {
        serde_yaml_ng::from_slice::<serde_yaml_ng::Value>(
            &fs::read(path)
                .map_err(|err| activity_io_error("provider-config-read-failed", path, err))?,
        )
        .map_err(|err| {
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
    if changed && action != SetupAction::DryRun {
        let rendered = serde_yaml_ng::to_string(&updated).map_err(|err| {
            CliError::runtime(
                "provider-config-render-failed",
                format!("failed to render Hermes config: {err}"),
                None,
            )
        })?;
        write_provider_config(path, rendered.as_bytes())?;
    }
    let configured = provider_specs(AgentKind::Hermes)
        .iter()
        .all(|spec| yaml_has_spec(&updated, *spec));
    Ok((changed, configured))
}

fn write_provider_config(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| activity_io_error("provider-config-dir-failed", parent, err))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|err| {
            activity_io_error("provider-config-dir-permission-failed", parent, err)
        })?;
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
    use pretty_assertions::assert_eq;

    fn event(kind: TurnEventKind, event_id: &str) -> TurnEvent {
        TurnEvent {
            schema_version: TURN_EVENT_VERSION.to_string(),
            event_id: event_id.to_string(),
            runtime_id: "runtime-1".to_string(),
            provider: "codex".to_string(),
            provider_session_id: Some("session-1".to_string()),
            provider_turn_id: Some("turn-1".to_string()),
            kind,
            attention_id: None,
            attention_kind: None,
            confidence: Confidence::Observed,
            source_kind: SourceKind::ProviderHook,
            provider_time: None,
        }
    }

    fn document() -> ActivityDocument {
        ActivityDocument {
            schema_version: ACTIVITY_DOCUMENT_VERSION.to_string(),
            runtime_id: "runtime-1".to_string(),
            state: starting_state("2026-07-10T00:00:00Z".to_string(), 1, None),
            pending_attention: Vec::new(),
            seen_event_ids: Vec::new(),
            provider_session_id: None,
            last_event_at: None,
        }
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
    let source = provider_source(event);
    match event.kind {
        TurnEventKind::TurnStarted => {
            if let Some(current) = document.state.current_turn.take() {
                document.state.last_turn = Some(LastTurn {
                    provider_turn_id: current.provider_turn_id,
                    started_at: Some(current.started_at),
                    completed_at: at.to_string(),
                    outcome: "interrupted".to_string(),
                });
            }
            document.pending_attention.clear();
            document.state.current_turn = Some(CurrentTurn {
                provider_turn_id: event.provider_turn_id.clone(),
                started_at: at.to_string(),
                attention: None,
            });
            document.state.phase = TurnPhase::Working;
        }
        TurnEventKind::AttentionRequested => {
            if document.state.current_turn.is_none() {
                document.state.current_turn = Some(CurrentTurn {
                    provider_turn_id: event.provider_turn_id.clone(),
                    started_at: at.to_string(),
                    attention: None,
                });
            }
            let attention_id = event.attention_id.as_deref().unwrap_or_default();
            if !document
                .pending_attention
                .iter()
                .any(|pending| pending.id == attention_id)
            {
                document.pending_attention.push(PendingAttention {
                    id: attention_id.to_string(),
                    kind: event
                        .attention_kind
                        .clone()
                        .unwrap_or_else(|| "other".to_string()),
                    requested_at: at.to_string(),
                });
            }
            refresh_attention(document);
            document.state.phase = TurnPhase::NeedsInput;
        }
        TurnEventKind::AttentionCleared => {
            let attention_id = event.attention_id.as_deref().unwrap_or_default();
            document
                .pending_attention
                .retain(|pending| pending.id != attention_id);
            refresh_attention(document);
            document.state.phase = if document.pending_attention.is_empty() {
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
                    attention: None,
                });
            }
            if document.pending_attention.is_empty() {
                document.state.phase = TurnPhase::Working;
            }
        }
        TurnEventKind::StopObserved => {
            // A raw Stop can race another matching hook that continues the turn.
            // Retain it in the journal/revision, but never fabricate Waiting.
        }
        TurnEventKind::TurnCompleted | TurnEventKind::TurnFailed => {
            let matches_current = document.state.current_turn.as_ref().is_none_or(|current| {
                event.provider_turn_id.is_none()
                    || current.provider_turn_id.is_none()
                    || event.provider_turn_id == current.provider_turn_id
            });
            if matches_current {
                let current = document.state.current_turn.take();
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
                });
                document.pending_attention.clear();
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

fn refresh_attention(document: &mut ActivityDocument) {
    let attention = document
        .pending_attention
        .first()
        .map(|pending| AttentionView {
            kind: pending.kind.clone(),
            requested_at: pending.requested_at.clone(),
            pending_count: document.pending_attention.len(),
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

#[derive(Serialize, Deserialize)]
struct JournalEntry {
    received_at: String,
    event: TurnEvent,
}

fn append_journal(path: &Path, event: &TurnEvent, received_at: &str) -> Result<(), CliError> {
    let mut entries = if path.is_file() {
        fs::read_to_string(path)
            .map_err(|err| activity_io_error("activity-journal-read-failed", path, err))?
            .lines()
            .filter_map(|line| serde_json::from_str::<JournalEntry>(line).ok())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    entries.push(JournalEntry {
        received_at: received_at.to_string(),
        event: event.clone(),
    });
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

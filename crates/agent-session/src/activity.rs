use std::collections::{BTreeMap, BTreeSet};
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
use toml_edit::{DocumentMut as TomlDocument, Item as TomlItem};

use crate::cli::AgentKind;
use crate::{
    CliContext, CliError, Envelope, ProviderResume, SessionRecord, canonical_provider_resume_args,
    load_session_record, mutate_session_record, run_output_with_timeout_and_strict_cap,
    session_dir, valid_sha256,
};

mod provider;
pub(crate) mod shadow;

use provider::{normalize_provider_hook, normalize_provider_notification};

pub(crate) const TURN_EVENT_VERSION: &str = "agent-session.turn-event.v1";
const CODEX_PROTOCOL_TURN_EVENT_VERSION: &str = "agent-session.turn-event.v2";
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
const OPERATOR_PROVIDER_TURN_RECEIPT_TTL_SECS: i64 = 24 * 60 * 60;
const MAX_OPERATOR_PROVIDER_TURN_RECEIPTS: usize = 64;
const CODEX_NOTIFY_FORWARD_ACTIVE_ENV: &str = "AGENT_SESSION_CODEX_NOTIFY_FANOUT_ACTIVE";
pub(crate) const ACTIVITY_RETRY_PROVIDER_ENV: &str = "AGENT_SESSION_ACTIVITY_RETRY_PROVIDER";
const CODEX_FORWARD_TIMEOUT: Duration = Duration::from_secs(2);
const CODEX_COMPLETION_RETRY_TIMEOUT: Duration = Duration::from_secs(5);
const RUNTIME_UNHEALTHY_LOCK_TIMEOUT: Duration = Duration::from_millis(250);
const AGENT_HOOK_DOCTOR_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const AGENT_CONSOLE_DSH_PROFILE: &str = "dsh-tui";
const AGENT_CONSOLE_DSH_LAUNCHER: &str = "run-agent-console-dsh";

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

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttentionCertainty {
    Exact,
    #[default]
    Conservative,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SemanticEventView {
    pub(crate) kind: String,
    pub(crate) observed_at: String,
    #[serde(default, flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ActivityDiagnosticView {
    pub(crate) reason: String,
    #[serde(default, flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ShadowObservationView {
    pub(crate) observer_version: String,
    pub(crate) rule_id: String,
    pub(crate) observed_at: String,
    pub(crate) projection: String,
    pub(crate) disagrees: bool,
    #[serde(default, flatten)]
    extra: Map<String, Value>,
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
    #[serde(default)]
    pub(crate) certainty: AttentionCertainty,
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

impl LastTurn {
    pub(crate) fn provider_failure_kind(&self) -> Option<&str> {
        self.extra
            .get("provider_failure_kind")
            .and_then(Value::as_str)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct TurnState {
    pub(crate) schema_version: String,
    pub(crate) phase: TurnPhase,
    pub(crate) phase_changed_at: String,
    pub(crate) revision: u64,
    pub(crate) source: TurnSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) semantic_event: Option<SemanticEventView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) diagnostic: Option<ActivityDiagnosticView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) shadow_observation: Option<ShadowObservationView>,
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
pub(crate) struct StreamSemanticEventView {
    pub(crate) kind: String,
    pub(crate) observed_at: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StreamActivityDiagnosticView {
    pub(crate) reason: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StreamShadowObservationView {
    pub(crate) observer_version: String,
    pub(crate) rule_id: String,
    pub(crate) observed_at: String,
    pub(crate) projection: String,
    pub(crate) disagrees: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct StreamAttentionView {
    pub(crate) kind: String,
    pub(crate) requested_at: String,
    pub(crate) pending_count: usize,
    pub(crate) certainty: AttentionCertainty,
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
    pub(crate) semantic_event: Option<StreamSemanticEventView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) diagnostic: Option<StreamActivityDiagnosticView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) shadow_observation: Option<StreamShadowObservationView>,
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
    #[serde(default)]
    certainty: AttentionCertainty,
    #[serde(default, flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct OverflowAttention {
    kind: String,
    requested_at: String,
    count: usize,
    #[serde(default)]
    certainty: AttentionCertainty,
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
    last_provider_event_kind: Option<TurnEventKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_provider_event_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_provider_event_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_provider_event_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_event_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_journal: Option<JournalEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_unhealthy_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    operator_provider_turn_receipts: Vec<OperatorProviderTurnReceipt>,
    #[serde(default, flatten)]
    extra: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct OperatorProviderTurnReceipt {
    idempotency_key: String,
    request_digest: String,
    reason: String,
    reconciliation: OperatorProviderTurnReconciliation,
    expires_at_epoch: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct OperatorProviderTurnReconciliation {
    pub(crate) schema_version: String,
    pub(crate) provider_turn_id: String,
    pub(crate) state: String,
    pub(crate) activity_revision_before: u64,
    pub(crate) activity_revision_after: u64,
    pub(crate) reconciled_at: String,
    pub(crate) reason: String,
    pub(crate) provenance: String,
}

pub(crate) struct OperatorProviderTurnReconcileInput<'a> {
    pub(crate) session_incarnation: &'a str,
    pub(crate) runtime_launch_id: &'a str,
    pub(crate) runtime_generation: u64,
    pub(crate) activity_revision: u64,
    pub(crate) provider: &'a str,
    pub(crate) provider_turn_id: &'a str,
    pub(crate) reason: &'a str,
    pub(crate) idempotency_key: &'a str,
    pub(crate) request_digest: &'a str,
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
    #[serde(skip)]
    attention_correlation_exact: bool,
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

#[cfg(test)]
pub(crate) fn ingest_codex_app_server_failure(
    context: &CliContext,
    id: &str,
    runtime_id: &str,
    thread_id: &str,
    turn_id: &str,
) -> Result<ActivityResult, CliError> {
    ingest_codex_app_server_failure_with_kind(
        context,
        id,
        runtime_id,
        thread_id,
        turn_id,
        crate::codex_app_server::StructuredFailureKind::UsageExhausted,
    )
}

pub(crate) fn ingest_codex_app_server_failure_with_kind(
    context: &CliContext,
    id: &str,
    runtime_id: &str,
    thread_id: &str,
    turn_id: &str,
    failure_kind: crate::codex_app_server::StructuredFailureKind,
) -> Result<ActivityResult, CliError> {
    let provider_session_id =
        projected_provider_identifier(runtime_id, AgentKind::Codex, "session", thread_id)?;
    let provider_turn_id =
        projected_provider_identifier(runtime_id, AgentKind::Codex, "turn", turn_id)?;
    ingest_event_retry_with_admission(
        context,
        id,
        TurnEvent {
            schema_version: if failure_kind
                == crate::codex_app_server::StructuredFailureKind::ProviderCapacity
            {
                CODEX_PROTOCOL_TURN_EVENT_VERSION.to_string()
            } else {
                TURN_EVENT_VERSION.to_string()
            },
            event_id: format!("codex-app-server-failure:{provider_turn_id}"),
            runtime_id: runtime_id.to_string(),
            provider: AgentKind::Codex.as_str().to_string(),
            provider_session_id: Some(provider_session_id),
            provider_turn_id: Some(provider_turn_id),
            kind: TurnEventKind::TurnFailed,
            failure_reason: Some(failure_kind.activity_reason().to_string()),
            attention_id: None,
            attention_kind: None,
            attention_correlation_ambiguous: false,
            attention_correlation_exact: false,
            confidence: Confidence::Authoritative,
            // `provider_hook` is the stable v1 wire value for authoritative,
            // provider-structured evidence, including the app-server protocol.
            source_kind: SourceKind::ProviderHook,
            provider_time: None,
        },
        EventAdmission::CodexProtocol,
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
    ingest_event_retry_with_admission(
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
            attention_correlation_exact: true,
            confidence: Confidence::Authoritative,
            // `provider_hook` is the stable v1 wire value for all structured
            // provider evidence, including the app-server protocol.
            source_kind: SourceKind::ProviderHook,
            provider_time: None,
        },
        EventAdmission::CodexProtocol,
    )
}

/// Project durable activity state onto the explicitly allowlisted stream
/// contract. Durable snapshots preserve additive fields for forward
/// compatibility; those unknown fields must not cross the daemon stream's
/// metadata-only privacy boundary.
fn stream_identifier(value: &str) -> bool {
    value.len() <= 64
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn stream_timestamp(value: &str) -> bool {
    value.len() <= 64 && value.parse::<jiff::Timestamp>().is_ok()
}

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
        semantic_event: state
            .semantic_event
            .as_ref()
            .filter(|event| {
                matches!(
                    event.kind.as_str(),
                    "turn_started"
                        | "attention_requested"
                        | "attention_cleared"
                        | "progress"
                        | "stop_observed"
                        | "turn_completed"
                        | "turn_failed"
                ) && stream_timestamp(&event.observed_at)
            })
            .map(|event| StreamSemanticEventView {
                kind: event.kind.clone(),
                observed_at: event.observed_at.clone(),
            }),
        diagnostic: state
            .diagnostic
            .as_ref()
            .filter(|diagnostic| {
                matches!(
                    diagnostic.reason.as_str(),
                    "completion_evidence_pending"
                        | "attention_authority_mismatch"
                        | "provider_projection_unavailable"
                        | "runtime_activity_unhealthy"
                        | "activity_state_unavailable"
                )
            })
            .map(|diagnostic| StreamActivityDiagnosticView {
                reason: diagnostic.reason.clone(),
            }),
        shadow_observation: state
            .shadow_observation
            .as_ref()
            .filter(|observation| {
                stream_identifier(&observation.observer_version)
                    && stream_identifier(&observation.rule_id)
                    && stream_timestamp(&observation.observed_at)
                    && matches!(
                        observation.projection.as_str(),
                        "working" | "needs_input" | "waiting" | "unknown"
                    )
            })
            .map(|observation| StreamShadowObservationView {
                observer_version: observation.observer_version.clone(),
                rule_id: observation.rule_id.clone(),
                observed_at: observation.observed_at.clone(),
                projection: observation.projection.clone(),
                disagrees: observation.disagrees,
            }),
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
                    certainty: attention.certainty.clone(),
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

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ProviderDoctor {
    pub(crate) provider: String,
    pub(crate) classification: String,
    pub(crate) version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) version_error: Option<String>,
    /// The provider's agent-session lifecycle hook/notification spec is present
    /// and not drifted. Config presence only — NOT version adequacy (see
    /// `classification`) and NOT launch-readiness (see `can_launch_worker`).
    pub(crate) configured: bool,
    /// True only when the provider is both `classification == "supported"` and
    /// `configured` — i.e. Main Agent Mode may launch a worker for it. Makes the
    /// launch gate explicit instead of leaving callers to AND the two axes.
    pub(crate) can_launch_worker: bool,
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
    /// The running agent-session binary's own version (git-describe form), so a
    /// stale or split install is diagnosable against the source it should match.
    pub(crate) binary_version: String,
    pub(crate) providers: Vec<ProviderDoctor>,
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
pub(crate) struct ActivityLock {
    file: fs::File,
    directory: PathBuf,
}

impl Drop for ActivityLock {
    fn drop(&mut self) {
        // SAFETY: flock only observes the valid file descriptor owned by self.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
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

pub(crate) fn acquire_coordination_activity_lock(
    context: &CliContext,
    session_id: &str,
) -> Result<ActivityLock, CliError> {
    acquire_lock_with_timeout(&session_dir(context, session_id), Duration::from_secs(2))
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
        return Ok(ActivityLock {
            file,
            directory: dir.to_path_buf(),
        });
    }

    let deadline = match mode {
        ActivityLockMode::Timed(timeout) => Some(Instant::now() + timeout),
        ActivityLockMode::NonBlocking => None,
        ActivityLockMode::Blocking => unreachable!("blocking mode returned above"),
    };
    loop {
        // SAFETY: flock only observes the valid file descriptor owned by file.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(ActivityLock {
                file,
                directory: dir.to_path_buf(),
            });
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
    state.diagnostic = Some(ActivityDiagnosticView {
        reason: "runtime_activity_unhealthy".to_string(),
        extra: Map::new(),
    });
    state.shadow_observation = None;
    state
}

fn runtime_diagnostic(reason: &str) -> ActivityDiagnosticView {
    let reason = if reason.contains("attention_authority") {
        "attention_authority_mismatch"
    } else if reason.contains("projection") {
        "provider_projection_unavailable"
    } else {
        "runtime_activity_unhealthy"
    };
    ActivityDiagnosticView {
        reason: reason.to_string(),
        extra: Map::new(),
    }
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
        semantic_event: None,
        diagnostic: None,
        shadow_observation: None,
        current_turn: None,
        last_turn,
        extra: Map::new(),
    }
}

pub(crate) fn activate_runtime(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<TurnState, CliError> {
    activate_runtime_in_dir(&session_dir(context, &record.id), record)
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn activate_new_runtime_with(
    record: &SessionRecord,
    mut write: impl FnMut(&str, &[u8]) -> Result<(), CliError>,
) -> Result<TurnState, CliError> {
    let runtime = record.runtime.as_ref().ok_or_else(|| {
        CliError::data(
            "runtime-id-missing",
            "session runtime is missing its launch id",
            Some(json!({ "id": record.id })),
        )
    })?;
    if runtime.launch_id.is_empty() {
        return Err(CliError::data(
            "runtime-id-missing",
            "session runtime is missing its launch id",
            Some(json!({ "id": record.id })),
        ));
    }

    let state = starting_state(runtime.started_at.clone(), 1, None);
    let document = activity_document_for_runtime(
        record,
        &runtime.launch_id,
        runtime.generation,
        state.clone(),
        Map::new(),
    )?;

    let expected_len = REPLAY_HEADER_BYTES + REPLAY_SLOT_COUNT * REPLAY_SLOT_BYTES;
    let mut replay = vec![0_u8; expected_len];
    replay[..REPLAY_HEADER_BYTES]
        .copy_from_slice(&replay_header(&runtime.launch_id, runtime.generation));
    write(ACTIVITY_REPLAY_FILE, &replay)?;
    let bytes = serde_json::to_vec_pretty(&document).map_err(|err| {
        CliError::runtime(
            "activity-render-failed",
            format!("failed to render activity snapshot: {err}"),
            None,
        )
    })?;
    write(ACTIVITY_FILE, &bytes)?;
    Ok(state)
}

fn activity_document_for_runtime(
    record: &SessionRecord,
    runtime_id: &str,
    runtime_generation: u64,
    state: TurnState,
    extra: Map<String, Value>,
) -> Result<ActivityDocument, CliError> {
    Ok(ActivityDocument {
        schema_version: ACTIVITY_DOCUMENT_VERSION.to_string(),
        runtime_id: runtime_id.to_string(),
        runtime_generation,
        state,
        pending_attention: Vec::new(),
        overflow_attention: None,
        seen_event_count: 0,
        last_semantic_event: None,
        last_semantic_event_at: None,
        last_provider_event_kind: None,
        last_provider_event_provider: None,
        last_provider_event_at: None,
        last_provider_event_turn_id: None,
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
        operator_provider_turn_receipts: Vec::new(),
        extra,
    })
}

pub(crate) fn activate_runtime_in_dir(
    dir: &Path,
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
    let _lock = acquire_lock(dir)?;
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
            match runtime_unhealthy_marker(dir, runtime_id, runtime_generation) {
                RuntimeUnhealthyStatus::Matching(state) => *state,
                RuntimeUnhealthyStatus::Pending(marked_at) => marker_state_from_snapshot(
                    dir,
                    record,
                    runtime_id,
                    runtime_generation,
                    &marked_at,
                ),
                RuntimeUnhealthyStatus::Invalid => marker_state_from_snapshot(
                    dir,
                    record,
                    runtime_id,
                    runtime_generation,
                    &existing.state.phase_changed_at,
                ),
                RuntimeUnhealthyStatus::Absent if replay_matches_document(dir, existing) => {
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
    let extra = existing
        .take()
        .map_or_else(Map::new, |document| document.extra);
    let mut document =
        activity_document_for_runtime(record, runtime_id, runtime_generation, state, extra)?;
    write_document(&path, &mut document)?;
    remove_runtime_unhealthy_marker(dir)?;
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
    let mut marker_state =
        marker_state_from_snapshot(&dir, &record, runtime_id, runtime.generation, &now());
    marker_state.diagnostic = Some(runtime_diagnostic(reason));
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
    document.state.diagnostic = Some(runtime_diagnostic(reason));
    write_runtime_unhealthy_marker(
        &dir,
        runtime_id,
        runtime.generation,
        reason,
        &document.state,
    )?;
    write_document(&path, &mut document)
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
        semantic_event: None,
        diagnostic: None,
        shadow_observation: None,
        current_turn: None,
        last_turn: None,
        extra: Map::new(),
    }
}

fn unknown_state_with_reason(record: &SessionRecord, reason: &str) -> TurnState {
    let mut state = unknown_state(record);
    state.diagnostic = Some(ActivityDiagnosticView {
        reason: reason.to_string(),
        extra: Map::new(),
    });
    state
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
    // A plugin-owned dsh lane reports its turn through the liveness sidecar
    // instead of this store's activity document, and its unhealthy markers
    // belong to a tmux runtime it never had. Projecting it here keeps every
    // activity consumer — claim gates, views, prompt baselines — reading the
    // same evidence as `main-agent worker diagnose`.
    if crate::dsh_external::is_external_record(record) {
        return crate::dsh_external::external_turn_state(record);
    }
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
        Ok(_) | Err(_) => Some(unknown_state_with_reason(
            record,
            "activity_state_unavailable",
        )),
    }
}

/// The turn identity observed immediately before a terminal-delivered prompt.
/// Acknowledging that prompt means seeing a turn this snapshot did not already
/// contain, so progress on a turn that was already running can never be
/// mistaken for the provider accepting new input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PromptTurnBaseline {
    revision: u64,
    turn_ids: BTreeSet<String>,
    turn_in_flight: bool,
}

pub(crate) fn prompt_turn_baseline(
    context: &CliContext,
    record: &SessionRecord,
) -> PromptTurnBaseline {
    let Some(state) = state_for_view(context, record) else {
        return PromptTurnBaseline::default();
    };
    PromptTurnBaseline {
        revision: state.revision,
        turn_ids: observed_turn_ids(&state)
            .into_iter()
            .flatten()
            .map(str::to_string)
            .collect(),
        turn_in_flight: state.current_turn.is_some(),
    }
}

/// True once the provider's own hook has reported a turn the baseline did not
/// contain. A provider that omits turn ids is accepted only when the baseline
/// had nothing in flight, so an unidentified turn cannot be double-counted.
pub(crate) fn prompt_turn_started_since(
    context: &CliContext,
    record: &SessionRecord,
    baseline: &PromptTurnBaseline,
) -> bool {
    let Some(state) = state_for_view(context, record) else {
        return false;
    };
    if state.revision <= baseline.revision || state.source.kind != SourceKind::ProviderHook {
        return false;
    }
    let observed = observed_turn_ids(&state);
    if observed
        .iter()
        .flatten()
        .any(|turn_id| !baseline.turn_ids.contains(*turn_id))
    {
        return true;
    }
    !baseline.turn_in_flight && state.current_turn.is_some() && observed.iter().all(Option::is_none)
}

/// The turn a provider is running plus the one it just finished. Both matter
/// because a short turn can start and complete inside one acknowledgement wait.
fn observed_turn_ids(state: &TurnState) -> [Option<&str>; 2] {
    [
        state
            .current_turn
            .as_ref()
            .and_then(|turn| turn.provider_turn_id.as_deref()),
        state
            .last_turn
            .as_ref()
            .and_then(|turn| turn.provider_turn_id.as_deref()),
    ]
}

pub(crate) struct OperatorProviderTurnReplaySelector<'a> {
    pub(crate) session_incarnation: &'a str,
    pub(crate) runtime_launch_id: &'a str,
    pub(crate) runtime_generation: u64,
}

pub(crate) fn operator_provider_turn_replay_locked(
    context: &CliContext,
    id: &str,
    activity_lock: &ActivityLock,
    selector: OperatorProviderTurnReplaySelector<'_>,
    idempotency_key: &str,
    request_digest: &str,
) -> Result<Option<Value>, CliError> {
    operator_provider_turn_replay_locked_at(
        context,
        id,
        activity_lock,
        selector,
        idempotency_key,
        request_digest,
        crate::coordination::now_epoch(),
    )
}

fn operator_provider_turn_replay_locked_at(
    context: &CliContext,
    id: &str,
    activity_lock: &ActivityLock,
    selector: OperatorProviderTurnReplaySelector<'_>,
    idempotency_key: &str,
    request_digest: &str,
    now_epoch: i64,
) -> Result<Option<Value>, CliError> {
    let dir = session_dir(context, id);
    ensure_activity_lock_owns(activity_lock, &dir, id)?;
    let path = dir.join(ACTIVITY_FILE);
    let document = read_document(&path)?;
    if document.runtime_id != selector.runtime_launch_id
        || document.runtime_generation != selector.runtime_generation
    {
        return Err(CliError::data(
            "session-incarnation-conflict",
            "provider turn reconciliation selectors do not match the activity runtime",
            Some(json!({ "id": id })),
        ));
    }
    let Some(receipt) = document
        .operator_provider_turn_receipts
        .iter()
        .filter(|receipt| operator_provider_turn_receipt_is_live(receipt, now_epoch))
        .find(|receipt| receipt.idempotency_key == idempotency_key)
    else {
        return Ok(None);
    };
    if receipt.request_digest != request_digest {
        return Err(CliError::data(
            "idempotency-key-reused",
            "idempotency key is already bound to another request",
            None,
        ));
    }
    Ok(Some(operator_provider_turn_result(
        id,
        selector.session_incarnation,
        selector.runtime_launch_id,
        selector.runtime_generation,
        &receipt.reason,
        &receipt.reconciliation,
    )))
}

pub(crate) fn operator_reconcile_provider_turn_locked(
    context: &CliContext,
    id: &str,
    activity_lock: &ActivityLock,
    _health_fence: &RuntimeHealthFence,
    input: OperatorProviderTurnReconcileInput<'_>,
) -> Result<Value, CliError> {
    let dir = session_dir(context, id);
    ensure_activity_lock_owns(activity_lock, &dir, id)?;
    if !matches!(
        runtime_unhealthy_marker(&dir, input.runtime_launch_id, input.runtime_generation),
        RuntimeUnhealthyStatus::Absent
    ) {
        return Err(CliError::data(
            "activity-runtime-unhealthy",
            "provider turn reconciliation requires healthy activity evidence",
            Some(json!({ "id": id })),
        ));
    }
    let path = dir.join(ACTIVITY_FILE);
    let mut document = read_document(&path)?;
    if document.runtime_id != input.runtime_launch_id
        || document.runtime_generation != input.runtime_generation
        || document.runtime_unhealthy_reason.is_some()
    {
        return Err(CliError::data(
            "session-incarnation-conflict",
            "provider turn reconciliation selectors do not match the healthy activity runtime",
            Some(json!({ "id": id })),
        ));
    }
    if document.pending_journal.is_some() {
        return Err(CliError::data(
            "operator-provider-turn-reconcile-not-admissible",
            "provider turn reconciliation refuses queued provider journal evidence",
            Some(json!({ "id": id })),
        ));
    }
    let now_epoch = crate::coordination::now_epoch();
    document
        .operator_provider_turn_receipts
        .retain(|receipt| receipt.expires_at_epoch > now_epoch);
    if let Some(receipt) = document
        .operator_provider_turn_receipts
        .iter()
        .find(|receipt| receipt.idempotency_key == input.idempotency_key)
    {
        if receipt.request_digest == input.request_digest {
            return Ok(operator_provider_turn_result(
                id,
                input.session_incarnation,
                input.runtime_launch_id,
                input.runtime_generation,
                &receipt.reason,
                &receipt.reconciliation,
            ));
        }
        return Err(CliError::data(
            "idempotency-key-reused",
            "idempotency key is already bound to another request",
            None,
        ));
    }
    if document.operator_provider_turn_receipts.len() >= MAX_OPERATOR_PROVIDER_TURN_RECEIPTS {
        return Err(CliError::data(
            "quota-exceeded",
            "provider turn reconciliation idempotency receipt quota exceeded",
            None,
        ));
    }
    if document.state.revision != input.activity_revision {
        return Err(CliError::data(
            "activity-revision-conflict",
            "provider turn reconciliation activity revision is stale",
            Some(json!({
                "id": id,
                "expected_revision": input.activity_revision,
                "actual_revision": document.state.revision,
            })),
        ));
    }
    let current_turn = document.state.current_turn.as_ref().ok_or_else(|| {
        CliError::data(
            "operator-provider-turn-reconcile-not-admissible",
            "provider turn reconciliation requires an exact open provider turn",
            Some(json!({ "id": id })),
        )
    })?;
    if current_turn.provider_turn_id.as_deref() != Some(input.provider_turn_id) {
        return Err(CliError::data(
            "provider-turn-id-mismatch",
            "provider turn reconciliation selector does not match the open provider turn",
            Some(json!({ "id": id })),
        ));
    }
    let semantic_event_matches = document.state.semantic_event.as_ref().is_some_and(|event| {
        event.kind == "stop_observed"
            && document.last_provider_event_at.as_deref() == Some(event.observed_at.as_str())
    });
    let diagnostic_matches = document
        .state
        .diagnostic
        .as_ref()
        .is_some_and(|diagnostic| diagnostic.reason == "completion_evidence_pending");
    let provider_turn_matches = document.last_provider_event_turn_id.as_deref()
        == Some(input.provider_turn_id)
        || (document.last_provider_event_turn_id.is_none()
            && pre_selector_journal_tail_matches_provider_turn(
                &dir.join(ACTIVITY_JOURNAL_FILE),
                &document,
                input.provider,
                input.provider_turn_id,
            ));
    let exact_stop_is_latest = document.last_provider_event_kind
        == Some(TurnEventKind::StopObserved)
        && document.last_provider_event_provider.as_deref() == Some(input.provider)
        && provider_turn_matches
        && document.last_semantic_event_at == document.last_provider_event_at
        && document.pending_journal.is_none();
    let attention_is_empty = document.pending_attention.is_empty()
        && document.overflow_attention.is_none()
        && current_turn.attention.is_none();
    if document.state.phase != TurnPhase::Working
        || !semantic_event_matches
        || !diagnostic_matches
        || !exact_stop_is_latest
        || !attention_is_empty
    {
        return Err(CliError::data(
            "operator-provider-turn-reconcile-not-admissible",
            "provider turn reconciliation requires the exact inactive stop-observed state with no pending attention or newer provider evidence",
            Some(json!({ "id": id })),
        ));
    }

    let reconciled_at = now();
    let revision_before = document.state.revision;
    let revision_after = revision_before.saturating_add(1);
    let reconciliation = OperatorProviderTurnReconciliation {
        schema_version: "agent-session.operator-provider-turn-reconciliation.v1".to_string(),
        provider_turn_id: input.provider_turn_id.to_string(),
        state: "operator_reconciled".to_string(),
        activity_revision_before: revision_before,
        activity_revision_after: revision_after,
        reconciled_at: reconciled_at.clone(),
        reason: input.reason.to_string(),
        provenance: "server_operator".to_string(),
    };
    let started_at = current_turn.started_at.clone();
    document.state.phase = TurnPhase::Waiting;
    document.state.phase_changed_at = reconciled_at.clone();
    document.state.revision = revision_after;
    document.state.source = TurnSource {
        kind: SourceKind::Runtime,
        provider: None,
        confidence: Confidence::Authoritative,
        extra: Map::new(),
    };
    document.state.semantic_event = Some(SemanticEventView {
        kind: "operator_reconciled".to_string(),
        observed_at: reconciled_at.clone(),
        extra: Map::new(),
    });
    document.state.diagnostic = None;
    document.state.shadow_observation = None;
    document.state.current_turn = None;
    let mut last_turn_extra = Map::new();
    last_turn_extra.insert(
        "operator_reconciliation".to_string(),
        serde_json::to_value(&reconciliation).map_err(|_| {
            CliError::runtime(
                "activity-write-failed",
                "failed to encode operator provider turn reconciliation",
                None,
            )
        })?,
    );
    document.state.last_turn = Some(LastTurn {
        provider_turn_id: Some(input.provider_turn_id.to_string()),
        started_at: Some(started_at),
        completed_at: reconciled_at.clone(),
        outcome: "operator_reconciled".to_string(),
        extra: last_turn_extra,
    });
    document.last_event_at = Some(reconciled_at);
    let result = operator_provider_turn_result(
        id,
        input.session_incarnation,
        input.runtime_launch_id,
        input.runtime_generation,
        input.reason,
        &reconciliation,
    );
    document
        .operator_provider_turn_receipts
        .push(OperatorProviderTurnReceipt {
            idempotency_key: input.idempotency_key.to_string(),
            request_digest: input.request_digest.to_string(),
            reason: input.reason.to_string(),
            reconciliation,
            expires_at_epoch: now_epoch.saturating_add(OPERATOR_PROVIDER_TURN_RECEIPT_TTL_SECS),
        });
    write_document(&path, &mut document)?;
    Ok(result)
}

fn pre_selector_journal_tail_matches_provider_turn(
    path: &Path,
    document: &ActivityDocument,
    provider: &str,
    provider_turn_id: &str,
) -> bool {
    let Ok(mut file) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_JOURNAL_BYTES as u64 {
        return false;
    }
    let mut bytes = Vec::new();
    if (&mut file)
        .take(MAX_JOURNAL_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.is_empty()
        || bytes.len() > MAX_JOURNAL_BYTES
        || bytes.last() != Some(&b'\n')
    {
        return false;
    }
    let Ok(contents) = std::str::from_utf8(&bytes) else {
        return false;
    };
    let mut last = None;
    let mut count = 0_usize;
    for line in contents.lines() {
        if line.is_empty() || count >= MAX_JOURNAL_EVENTS {
            return false;
        }
        let Ok(entry) = serde_json::from_str::<JournalEntry>(line) else {
            return false;
        };
        count = count.saturating_add(1);
        last = Some(entry);
    }
    let Some(last) = last else {
        return false;
    };
    last.received_at
        == document
            .last_provider_event_at
            .as_deref()
            .unwrap_or_default()
        && document.last_event_at.as_deref() == Some(last.received_at.as_str())
        && document.last_semantic_event_at.as_deref() == Some(last.received_at.as_str())
        && last.event.runtime_id == document.runtime_id
        && last.event.provider == provider
        && last.event.provider_turn_id.as_deref() == Some(provider_turn_id)
        && last.event.kind == TurnEventKind::StopObserved
        && last.event.source_kind == SourceKind::ProviderHook
        && document.provider_session_id == last.event.provider_session_id
        && document.last_semantic_event.as_deref() == Some(semantic_event_key(&last.event).as_str())
}

fn operator_provider_turn_result(
    id: &str,
    session_incarnation: &str,
    runtime_launch_id: &str,
    runtime_generation: u64,
    reason: &str,
    reconciliation: &OperatorProviderTurnReconciliation,
) -> Value {
    json!({
        "schema_version": "agent-session.operator-provider-turn-reconcile-result.v1",
        "session_id": id,
        "session_incarnation": session_incarnation,
        "runtime_launch_id": runtime_launch_id,
        "runtime_generation": runtime_generation,
        "reason": reason,
        "provider_turn_reconciliation": reconciliation,
    })
}

fn operator_provider_turn_receipt_is_live(
    receipt: &OperatorProviderTurnReceipt,
    now_epoch: i64,
) -> bool {
    receipt.expires_at_epoch > now_epoch
}

fn ensure_activity_lock_owns(
    activity_lock: &ActivityLock,
    expected_directory: &Path,
    id: &str,
) -> Result<(), CliError> {
    if activity_lock.directory == expected_directory {
        return Ok(());
    }
    Err(CliError::data(
        "activity-lock-session-mismatch",
        "activity lock does not own the target session",
        Some(json!({ "id": id })),
    ))
}

pub(crate) fn claude_notification_waiting(
    context: &CliContext,
    record: &SessionRecord,
    debounce: std::time::Duration,
) -> bool {
    if record.agent != AgentKind::Claude.as_str() {
        return false;
    }
    let Some(runtime) = record.runtime.as_ref() else {
        return false;
    };
    let path = session_dir(context, &record.id).join(ACTIVITY_FILE);
    let Ok(document) = read_document(&path) else {
        return false;
    };
    if document.runtime_id != runtime.launch_id
        || document.runtime_generation != runtime.generation
        || !document.pending_attention.is_empty()
        || document.overflow_attention.is_some()
    {
        return false;
    }
    if document.state.phase == TurnPhase::Waiting
        && document.state.source.confidence == Confidence::Authoritative
    {
        return true;
    }
    if document.last_provider_event_kind != Some(TurnEventKind::StopObserved)
        || document.last_provider_event_provider.as_deref() != Some(AgentKind::Claude.as_str())
    {
        return false;
    }
    let Some(observed_at) = document
        .last_provider_event_at
        .as_deref()
        .and_then(|value| value.parse::<jiff::Timestamp>().ok())
    else {
        return false;
    };
    let elapsed = jiff::Timestamp::now()
        .as_second()
        .saturating_sub(observed_at.as_second());
    elapsed >= i64::try_from(debounce.as_secs()).unwrap_or(i64::MAX)
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
    ingest_event_with_lock(
        context,
        id,
        event,
        ActivityLockMode::Blocking,
        EventAdmission::Generic,
    )
}

fn ingest_event_nonblocking(
    context: &CliContext,
    id: &str,
    event: TurnEvent,
) -> Result<ActivityResult, CliError> {
    ingest_event_with_lock(
        context,
        id,
        event,
        ActivityLockMode::NonBlocking,
        EventAdmission::Generic,
    )
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
        EventAdmission::Generic,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EventAdmission {
    Generic,
    CodexProtocol,
}

fn ingest_event_retry_with_admission(
    context: &CliContext,
    id: &str,
    event: TurnEvent,
    admission: EventAdmission,
) -> Result<ActivityResult, CliError> {
    ingest_event_with_lock(
        context,
        id,
        event,
        ActivityLockMode::Timed(CODEX_COMPLETION_RETRY_TIMEOUT),
        admission,
    )
}

fn ingest_event_with_lock(
    context: &CliContext,
    id: &str,
    mut event: TurnEvent,
    lock_mode: ActivityLockMode,
    admission: EventAdmission,
) -> Result<ActivityResult, CliError> {
    validate_event(&event, admission)?;
    let observed = load_session_record(context, id)?;
    if let Some(result) = external_dsh_provider_hook_activity(context, &observed, &event)? {
        return Ok(result);
    }
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
    if !session_accepts_activity_provider(&record, &event.provider) {
        return Err(CliError::data(
            "activity-provider-mismatch",
            "activity event provider does not match the session provider",
            Some(json!({ "id": record.id, "provider": event.provider })),
        ));
    }
    if admission == EventAdmission::CodexProtocol
        && !crate::codex_app_server::runtime_is_supported(&record)
    {
        return Err(CliError::data(
            "activity-provider-protocol-mismatch",
            "provider-protocol activity requires the bound Codex app-server runtime",
            Some(json!({ "id": record.id })),
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
    let agent = AgentKind::from_name(&event.provider).expect("validated activity provider");
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
        .filter(|resume| resume.provider == event.provider)
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
        document.state.semantic_event = Some(SemanticEventView {
            kind: turn_event_kind_name(&event.kind).to_string(),
            observed_at: received_at.clone(),
            extra: Map::new(),
        });
        document.state.diagnostic = (event.kind == TurnEventKind::StopObserved
            && document.state.current_turn.is_some())
        .then(|| ActivityDiagnosticView {
            reason: "completion_evidence_pending".to_string(),
            extra: Map::new(),
        });
        document.state.shadow_observation = None;
        document.last_semantic_event = Some(semantic_key);
        document.last_semantic_event_at = Some(received_at.clone());
        document.last_provider_event_kind = Some(event.kind.clone());
        document.last_provider_event_provider = Some(event.provider.clone());
        document.last_provider_event_at = Some(received_at.clone());
        document.last_provider_event_turn_id = event.provider_turn_id.clone();
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
    write_document(&path, &mut document)?;
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
    write_document(&path, &mut document)?;
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

fn external_dsh_provider_hook_activity(
    context: &CliContext,
    observed: &SessionRecord,
    event: &TurnEvent,
) -> Result<Option<ActivityResult>, CliError> {
    if !crate::dsh_external::is_external_record(observed)
        || event.provider != AgentKind::Dsh.as_str()
        || event.source_kind != SourceKind::ProviderHook
    {
        return Ok(None);
    }
    let active_runtime_id = observed
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if active_runtime_id.is_empty() || event.runtime_id != active_runtime_id {
        return Err(CliError::data(
            "runtime-id-mismatch",
            "activity event does not belong to the active runtime generation",
            Some(json!({ "id": observed.id })),
        ));
    }
    let observed_turn = live_external_dsh_turn(observed)?;

    // This path projects the plugin-owned sidecar and never mutates the
    // activity document, so waiting behind the mutable session-record lock can
    // only consume the hook admission budget. Re-read after the sidecar read
    // and require the exact session/runtime identity and validated liveness
    // binding to remain stable instead.
    let current = load_session_record(context, &observed.id)?;
    crate::ensure_same_session_identity(observed, &current)?;
    if !crate::dsh_external::is_external_record(&current)
        || !crate::dsh_external::same_liveness_binding(observed, &current)
    {
        return Err(CliError::runtime(
            "session-runtime-changed",
            "external DSH liveness authority changed during activity admission",
            Some(json!({ "id": observed.id })),
        ));
    }
    let current_runtime_id = current
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if event.runtime_id != current_runtime_id {
        return Err(CliError::data(
            "runtime-id-mismatch",
            "activity event does not belong to the active runtime generation",
            Some(json!({ "id": current.id })),
        ));
    }
    let turn_state = live_external_dsh_turn(&current)?;
    if turn_state != observed_turn {
        return Err(CliError::runtime(
            "external-dsh-activity-unavailable",
            "external DSH activity changed during sidecar admission",
            Some(json!({ "id": current.id })),
        ));
    }
    let final_record = load_session_record(context, &current.id)?;
    crate::ensure_same_session_identity(&current, &final_record)?;
    if !crate::dsh_external::is_external_record(&final_record)
        || !crate::dsh_external::same_liveness_binding(&current, &final_record)
    {
        return Err(CliError::runtime(
            "session-runtime-changed",
            "external DSH liveness authority changed after sidecar admission",
            Some(json!({ "id": current.id })),
        ));
    }
    let final_runtime_id = final_record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.as_str())
        .unwrap_or_default();
    if event.runtime_id != final_runtime_id {
        return Err(CliError::data(
            "runtime-id-mismatch",
            "activity event does not belong to the active runtime generation",
            Some(json!({ "id": final_record.id })),
        ));
    }
    Ok(Some(ActivityResult {
        id: final_record.id,
        turn_state,
        duplicate: true,
    }))
}

fn live_external_dsh_turn(record: &SessionRecord) -> Result<TurnState, CliError> {
    let turn_state = crate::dsh_external::external_turn_state(record).ok_or_else(|| {
        CliError::runtime(
            "external-dsh-activity-unavailable",
            "external DSH activity could not be proven by the plugin liveness sidecar",
            Some(json!({ "id": record.id })),
        )
    })?;
    if turn_state.phase != TurnPhase::Working || turn_state.current_turn.is_none() {
        return Err(CliError::runtime(
            "external-dsh-activity-unavailable",
            "external DSH activity does not prove a live plugin-owned turn",
            Some(json!({ "id": record.id })),
        ));
    }
    Ok(turn_state)
}

fn session_accepts_activity_provider(record: &SessionRecord, provider: &str) -> bool {
    if provider == AgentKind::Dsh.as_str() {
        return agent_console_dsh_transport(record);
    }
    !agent_console_dsh_transport(record) && provider == record.agent
}

fn agent_console_dsh_transport(record: &SessionRecord) -> bool {
    if record.agent != AgentKind::Hermes.as_str()
        || crate::session_agent_profile(record) != Some(AGENT_CONSOLE_DSH_PROFILE)
    {
        return false;
    }
    record.agent_bin.as_deref().is_some_and(|agent_bin| {
        let path = Path::new(agent_bin);
        path.is_absolute()
            && path.file_name().and_then(|name| name.to_str()) == Some(AGENT_CONSOLE_DSH_LAUNCHER)
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
    activity_status_for_record(context, &record)
}

pub(crate) fn activity_status_for_record(
    context: &CliContext,
    record: &SessionRecord,
) -> Result<ActivityResult, CliError> {
    let state = state_for_view(context, record).unwrap_or_else(|| TurnState {
        schema_version: TURN_STATE_VERSION.to_string(),
        phase: TurnPhase::Unknown,
        phase_changed_at: record.updated_at.clone(),
        revision: 0,
        source: runtime_source(),
        semantic_event: None,
        diagnostic: None,
        shadow_observation: None,
        current_turn: None,
        last_turn: None,
        extra: Map::new(),
    });
    Ok(ActivityResult {
        id: record.id.clone(),
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
    let Some(argv) = agent_hook::setup::decode_codex_forward_notify_argv(encoded_argv) else {
        return;
    };

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
                let hook_control = agent_hook_codex_configured();
                let notification = codex_notification_status(config_path);
                let configured = hook_control.as_ref().is_ok_and(|configured| *configured)
                    && notification.as_ref().is_ok_and(|status| status.configured);
                let configuration_error = hooks
                    .as_ref()
                    .err()
                    .map(|error| error.code().to_string())
                    .or_else(|| {
                        notification
                            .as_ref()
                            .err()
                            .map(|error| error.code().to_string())
                    })
                    .or_else(|| {
                        hook_control
                            .as_ref()
                            .err()
                            .map(|error| (*error).to_string())
                    });
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
            AgentKind::Claude | AgentKind::Hermes | AgentKind::Dsh => {
                match provider_configured(agent, &path) {
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
                }
            }
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
                AgentKind::Dsh => (
                    "unverified",
                    "dsh lifecycle state is owned by the external dsh-runtime-kit runtime; no provider hook completion signal exists",
                    "no provider attention correlation exists; the external runtime's liveness sidecar is the only runtime evidence",
                    "the external dsh-runtime-kit bundle owns Cordis registration; no files are managed here",
                    "Use main-agent capabilities --provider dsh for the external-runtime readiness contract",
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
            AgentKind::Dsh => ("unverified", "external-runtime"),
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
        let can_launch_worker = classification == "supported" && configured;
        providers.push(ProviderDoctor {
            provider: agent.as_str().to_string(),
            classification: classification.to_string(),
            version,
            version_error: version_probe.error,
            configured,
            can_launch_worker,
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
    Ok(DoctorResult {
        binary_version: nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
        providers,
    })
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

fn agent_hook_codex_configured() -> Result<bool, &'static str> {
    let binary = std::env::var_os("AGENT_HOOK_BIN").unwrap_or_else(|| "agent-hook".into());
    let mut command = ProcessCommand::new(binary);
    command.args(["doctor", "--product", "codex", "--format", "json"]);
    let output = run_output_with_timeout_and_strict_cap(
        command,
        Duration::from_secs(10),
        AGENT_HOOK_DOCTOR_MAX_OUTPUT_BYTES,
    )
    .map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidData {
            "agent-hook-doctor-output-invalid"
        } else {
            "agent-hook-doctor-unavailable"
        }
    })?;
    let envelope: Envelope<Vec<agent_hook::setup::DoctorResult>> =
        serde_json::from_slice(&output.stdout).map_err(|_| "agent-hook-doctor-output-invalid")?;
    if !output.status.success()
        || !envelope.ok
        || envelope.error.is_some()
        || envelope.schema_version != "cli.agent-hook.doctor.v1"
    {
        return Err("agent-hook-doctor-output-invalid");
    }
    let mut results = envelope
        .data
        .ok_or("agent-hook-doctor-output-invalid")?
        .into_iter();
    let result = results.next().ok_or("agent-hook-doctor-output-invalid")?;
    if results.next().is_some()
        || result.schema_version != "agent-hook.doctor.v1"
        || result.product != "codex"
        || !valid_sha256(&result.config_digest)
        || !valid_sha256(&result.policy_digest)
    {
        return Err("agent-hook-doctor-output-invalid");
    }
    Ok(result.supported
        && result.status == agent_hook::setup::ProviderStatus::Converged
        && result.owned_count == result.expected_owned_count
        && result.legacy_residue_count == 0)
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
        // No provider activity pipeline exists for dsh; the config-path gate
        // refuses `--agent dsh` before any floor comparison can run.
        AgentKind::Dsh => (u64::MAX, 0, 0),
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
        AgentKind::Dsh => {
            return Err(CliError::usage(
                "unsupported-activity-agent",
                "dsh lifecycle state is owned by the external dsh-runtime-kit runtime; there is no provider activity configuration to manage",
                Some(json!({ "agent": agent.as_str() })),
            ));
        }
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
    ownership_conflict: bool,
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
        // No provider activity hooks exist for dsh; lifecycle evidence is
        // owned by the external dsh-runtime-kit runtime.
        AgentKind::Dsh => Vec::new(),
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
        AgentKind::Codex | AgentKind::Hermes | AgentKind::Dsh => Vec::new(),
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
        // Unreachable in practice: provider_config_path refuses dsh first.
        AgentKind::Dsh => Ok(false),
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
    let mode = codex_notify_mode(&document, path);
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

fn codex_notify_mode(document: &TomlDocument, config_path: &Path) -> CodexNotifyMode {
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
    match agent_hook::setup::codex_notification_ownership(config_path, &argv) {
        Some(agent_hook::setup::CodexNotificationOwnership::Owned) => CodexNotifyMode::Owned,
        Some(agent_hook::setup::CodexNotificationOwnership::Composed) => {
            CodexNotifyMode::Composed(argv)
        }
        None => CodexNotifyMode::Foreign(argv),
    }
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

const CODEX_HOOK_BLOCK_START: &str = "# >>> agent-session:codex-hooks >>>";
const CODEX_HOOK_BLOCK_END: &str = "# <<< agent-session:codex-hooks <<<";

fn toml_hook_matcher_matches(group: &toml_edit::Table, spec: ProviderSpec) -> bool {
    let matcher = group.get("matcher").and_then(TomlItem::as_str);
    match spec.matcher {
        Some(expected) => matcher == Some(expected),
        None => matcher.is_none_or(str::is_empty),
    }
}

fn toml_hook_matcher_is_exact(group: &toml_edit::Table, spec: ProviderSpec) -> bool {
    match (group.get("matcher"), spec.matcher) {
        (None, None) => true,
        (Some(matcher), None) => matcher.as_str() == Some(""),
        (Some(matcher), Some(expected)) => matcher.as_str() == Some(expected),
        (None, Some(_)) => false,
    }
}

fn toml_handler_command_matches_owned(event: &str, handler: &toml_edit::Table) -> bool {
    handler.get("type").and_then(TomlItem::as_str) == Some("command")
        && handler
            .get("command")
            .and_then(TomlItem::as_str)
            .is_some_and(|command| {
                command == owned_command(AgentKind::Codex, Some(event))
                    || command == owned_command(AgentKind::Codex, None)
            })
}

fn toml_handler_command_is_owned(event: &str, handler: &toml_edit::Table) -> bool {
    toml_handler_command_matches_owned(event, handler)
        && handler.len() == 3
        && handler.get("timeout").and_then(TomlItem::as_integer) == Some(5)
}

fn toml_has_owned_handler_metadata_conflict(document: &TomlDocument) -> bool {
    let Some(hooks) = document.get("hooks").and_then(TomlItem::as_table) else {
        return false;
    };
    provider_specs(AgentKind::Codex).into_iter().any(|spec| {
        hooks
            .get(spec.event)
            .and_then(TomlItem::as_array_of_tables)
            .is_some_and(|groups| {
                groups.iter().any(|group| {
                    toml_hook_matcher_matches(group, spec)
                        && group
                            .get("hooks")
                            .and_then(TomlItem::as_array_of_tables)
                            .is_some_and(|handlers| {
                                handlers.iter().any(|handler| {
                                    toml_handler_command_matches_owned(spec.event, handler)
                                        && !toml_handler_command_is_owned(spec.event, handler)
                                })
                            })
                })
            })
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
                                toml_handler_command_is_owned(spec.event, handler)
                                    && handler.get("command").and_then(TomlItem::as_str)
                                        == Some(
                                            owned_command(AgentKind::Codex, Some(spec.event))
                                                .as_str(),
                                        )
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodexTomlHookMarkerLayout {
    Absent,
    OrphanStart,
    OrphanEnd,
    Complete,
}

impl CodexTomlHookMarkerLayout {
    fn has_evidence(self) -> bool {
        self != Self::Absent
    }
}

#[derive(Debug)]
struct CodexTomlHookAnalysis {
    document: TomlDocument,
    marker_layout: CodexTomlHookMarkerLayout,
}

fn analyze_codex_toml_hooks(path: &Path, raw: &str) -> Result<CodexTomlHookAnalysis, CliError> {
    let document = parse_codex_notification_config(path, raw)?;
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
        return Ok(CodexTomlHookAnalysis {
            document,
            marker_layout: CodexTomlHookMarkerLayout::Absent,
        });
    }
    if starts.len() > 1 || ends.len() > 1 {
        return Err(CliError::data(
            "provider-config-invalid",
            "Codex config has a duplicate agent-session hook marker",
            Some(json!({ "path": display_path(path) })),
        ));
    }
    let (Some(&(start_begin, _)), Some(&(end_begin, _))) = (starts.first(), ends.first()) else {
        let marker_layout = if starts.is_empty() {
            CodexTomlHookMarkerLayout::OrphanEnd
        } else {
            CodexTomlHookMarkerLayout::OrphanStart
        };
        return Ok(CodexTomlHookAnalysis {
            document,
            marker_layout,
        });
    };
    if end_begin < start_begin {
        return Err(CliError::data(
            "provider-config-invalid",
            "Codex config has a reversed agent-session hook marker block",
            Some(json!({ "path": display_path(path) })),
        ));
    }
    Ok(CodexTomlHookAnalysis {
        document,
        marker_layout: CodexTomlHookMarkerLayout::Complete,
    })
}

fn json_handler_is_owned_for_group(event: &str, group: &Value, handler: &Value) -> bool {
    let Some(spec) = provider_specs(AgentKind::Codex).into_iter().find(|spec| {
        spec.event == event && group.get("matcher").and_then(Value::as_str) == spec.matcher
    }) else {
        return false;
    };
    !json_hook_group_has_user_metadata(group) && json_codex_handler_is_owned(spec.event, handler)
}

fn json_codex_handler_command_matches_owned(event: &str, handler: &Value) -> bool {
    handler.get("type").and_then(Value::as_str) == Some("command")
        && handler
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| {
                command == owned_command(AgentKind::Codex, Some(event))
                    || command == owned_command(AgentKind::Codex, None)
            })
}

fn json_codex_handler_is_owned(event: &str, handler: &Value) -> bool {
    json_codex_handler_command_matches_owned(event, handler)
        && handler
            .as_object()
            .is_some_and(|handler| handler.len() == 3)
        && handler.get("timeout").and_then(Value::as_u64) == Some(5)
}

fn json_hook_group_has_user_metadata(group: &Value) -> bool {
    group.as_object().is_some_and(|group| {
        group
            .keys()
            .any(|key| !matches!(key.as_str(), "matcher" | "hooks"))
    })
}

fn json_has_owned_handler_metadata_conflict(value: &Value) -> bool {
    let Some(hooks) = value.get("hooks").and_then(Value::as_object) else {
        return false;
    };
    provider_specs(AgentKind::Codex).into_iter().any(|spec| {
        hooks
            .get(spec.event)
            .and_then(Value::as_array)
            .is_some_and(|groups| {
                groups.iter().any(|group| {
                    group.get("matcher").and_then(Value::as_str) == spec.matcher
                        && group
                            .get("hooks")
                            .and_then(Value::as_array)
                            .is_some_and(|handlers| {
                                handlers.iter().any(|handler| {
                                    json_codex_handler_command_matches_owned(spec.event, handler)
                                        && !json_codex_handler_is_owned(spec.event, handler)
                                })
                            })
                })
            })
    })
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

fn codex_hook_status_from_analysis(
    json: &Value,
    config: &CodexTomlHookAnalysis,
) -> CodexHookStatus {
    let inline_active =
        config.marker_layout.has_evidence() || toml_inline_has_lifecycle_hooks(&config.document);
    let representation = if inline_active {
        CodexHookRepresentation::InlineToml
    } else {
        CodexHookRepresentation::Json
    };
    let conflict = inline_active && json_has_non_owned_lifecycle_hooks(json);
    let ownership_conflict = json_has_owned_handler_metadata_conflict(json)
        || toml_has_owned_handler_metadata_conflict(&config.document);
    let migration_required = inline_active && json_has_owned_codex_hooks(json);
    let configured = !conflict
        && !ownership_conflict
        && match representation {
            CodexHookRepresentation::Json => provider_specs(AgentKind::Codex)
                .into_iter()
                .all(|spec| json_has_spec(json, AgentKind::Codex, spec)),
            CodexHookRepresentation::InlineToml => toml_codex_hooks_configured(&config.document),
        };
    CodexHookStatus {
        representation,
        configured,
        migration_required,
        conflict,
        ownership_conflict,
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
    let config = analyze_codex_toml_hooks(config_path, &config_raw)?;
    Ok(codex_hook_status_from_analysis(&json, &config))
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
    if status.conflict || status.ownership_conflict {
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
        let matcher_is_absent = group.get("matcher").is_none();
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
            if matcher_is_absent
                && command == expected
                && json_codex_handler_is_owned("PermissionRequest", handler)
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
            if toml_hook_matcher_is_exact(
                group,
                ProviderSpec {
                    event: "PermissionRequest",
                    matcher: None,
                },
            ) && command == expected
                && toml_handler_command_is_owned("PermissionRequest", handler)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CliContext, ProviderResume, RecordRequest, SessionRecord, create_record,
        write_session_record,
    };
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
            attention_correlation_exact: false,
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
            last_provider_event_kind: None,
            last_provider_event_provider: None,
            last_provider_event_at: None,
            last_provider_event_turn_id: None,
            provider_session_id: None,
            last_event_at: None,
            pending_journal: None,
            runtime_unhealthy_reason: None,
            operator_provider_turn_receipts: Vec::new(),
            extra: Map::new(),
        }
    }

    #[test]
    fn activity_document_without_operator_receipts_remains_compatible() {
        let mut value = serde_json::to_value(document()).expect("activity document");
        value
            .as_object_mut()
            .expect("activity object")
            .remove("operator_provider_turn_receipts");

        let restored: ActivityDocument =
            serde_json::from_value(value).expect("pre-receipt activity document");
        assert!(restored.operator_provider_turn_receipts.is_empty());
    }

    #[test]
    fn idless_completion_does_not_inherit_unrelated_operator_reconciliation() {
        let mut document = document();
        let mut reconciliation = Map::new();
        reconciliation.insert(
            "operator_reconciliation".to_string(),
            json!({"canary": true}),
        );
        document.state.last_turn = Some(LastTurn {
            provider_turn_id: Some("prior-provider-turn".to_string()),
            started_at: Some("2026-07-10T00:00:00Z".to_string()),
            completed_at: "2026-07-10T00:00:01Z".to_string(),
            outcome: "operator_reconciled".to_string(),
            extra: reconciliation,
        });
        let mut completion = event(TurnEventKind::TurnCompleted, "unrelated-idless-completion");
        completion.provider_turn_id = None;

        reduce(&mut document, &completion, "2026-07-10T00:00:02Z");

        assert!(
            document
                .state
                .last_turn
                .as_ref()
                .is_some_and(|turn| !turn.extra.contains_key("operator_reconciliation"))
        );
    }

    #[test]
    fn replacement_runtime_idless_completion_does_not_migrate_reconciliation_provenance() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created, runtime_id, revision) = prepare_operator_turn(&tmp);
        reconcile_operator_turn(
            &context,
            &created.record,
            &runtime_id,
            revision,
            "operator-provider-turn-provenance",
        )
        .expect("reconcile prior runtime");
        let mut replacement = created.record.clone();
        let replacement_runtime_id = {
            let replacement_runtime = replacement.runtime.as_mut().expect("runtime");
            replacement_runtime.launch_id = "replacement-runtime".to_string();
            replacement_runtime.generation = replacement_runtime.generation.saturating_add(1);
            replacement_runtime.started_at = "2026-07-10T00:00:03Z".to_string();
            replacement_runtime.launch_id.clone()
        };
        write_session_record(&context, &replacement).expect("replacement session");
        activate_runtime(&context, &replacement).expect("replacement activity");
        let mut completion = event(
            TurnEventKind::TurnCompleted,
            "replacement-runtime-idless-completion",
        );
        completion.runtime_id = replacement_runtime_id;
        completion.provider_turn_id = None;

        let completed =
            ingest_event(&context, &replacement.id, completion).expect("idless completion");
        let completed = serde_json::to_value(completed.turn_state).expect("completed state");

        assert_eq!(completed["last_turn"]["outcome"], "completed");
        assert!(
            completed["last_turn"]
                .get("operator_reconciliation")
                .is_none()
        );
    }

    fn prepare_operator_turn(
        tmp: &tempfile::TempDir,
    ) -> (CliContext, crate::CreatedRecord, String, u64) {
        let (context, created) = test_session(tmp);
        activate_runtime(&context, &created.record).expect("activate runtime");
        let runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        for (event_id, kind) in [
            ("operator-start", TurnEventKind::TurnStarted),
            ("operator-stop", TurnEventKind::StopObserved),
        ] {
            let mut provider_event = event(kind, event_id);
            provider_event.runtime_id = runtime_id.clone();
            ingest_event(&context, &created.record.id, provider_event).expect("provider event");
        }
        let revision = activity_status(&context, &created.record.id)
            .expect("activity")
            .turn_state
            .revision;
        (context, created, runtime_id, revision)
    }

    fn reconcile_operator_turn(
        context: &CliContext,
        record: &SessionRecord,
        runtime_id: &str,
        revision: u64,
        idempotency_key: &str,
    ) -> Result<Value, CliError> {
        let provider_turn_id = activity_status(context, &record.id)?
            .turn_state
            .current_turn
            .as_ref()
            .and_then(|turn| turn.provider_turn_id.as_deref())
            .ok_or_else(|| {
                CliError::data(
                    "provider-turn-id-mismatch",
                    "test fixture requires a canonical current provider turn",
                    None,
                )
            })?
            .to_string();
        let dir = session_dir(context, &record.id);
        let activity_lock = acquire_lock(&dir)?;
        let health_fence = acquire_runtime_health_fence(context, record)?;
        operator_reconcile_provider_turn_locked(
            context,
            &record.id,
            &activity_lock,
            &health_fence,
            OperatorProviderTurnReconcileInput {
                session_incarnation: runtime_id,
                runtime_launch_id: runtime_id,
                runtime_generation: record.runtime.as_ref().expect("runtime").generation,
                activity_revision: revision,
                provider: &record.agent,
                provider_turn_id: &provider_turn_id,
                reason: "authoritative-completion-signal-missing",
                idempotency_key,
                request_digest: idempotency_key,
            },
        )
    }

    fn test_operator_receipt(
        idempotency_key: String,
        request_digest: String,
        expires_at_epoch: i64,
    ) -> OperatorProviderTurnReceipt {
        OperatorProviderTurnReceipt {
            idempotency_key,
            request_digest,
            reason: "authoritative-completion-signal-missing".to_string(),
            reconciliation: OperatorProviderTurnReconciliation {
                schema_version: "agent-session.operator-provider-turn-reconciliation.v1"
                    .to_string(),
                provider_turn_id: "local:v1:test-provider-turn".to_string(),
                state: "operator_reconciled".to_string(),
                activity_revision_before: 2,
                activity_revision_after: 3,
                reconciled_at: "2026-07-10T00:00:02Z".to_string(),
                reason: "authoritative-completion-signal-missing".to_string(),
                provenance: "server_operator".to_string(),
            },
            expires_at_epoch,
        }
    }

    #[test]
    fn operator_reconcile_recovers_pre_selector_snapshot_from_exact_journal_tail() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created, runtime_id, revision) = prepare_operator_turn(&tmp);
        let path = session_dir(&context, &created.record.id).join(ACTIVITY_FILE);
        let mut pre_selector_document: Value =
            serde_json::from_slice(&fs::read(&path).expect("activity bytes"))
                .expect("activity document");
        pre_selector_document
            .as_object_mut()
            .expect("activity object")
            .remove("last_provider_event_turn_id");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&pre_selector_document)
                .expect("pre-selector activity document"),
        )
        .expect("seed pre-selector activity");

        let result = reconcile_operator_turn(
            &context,
            &created.record,
            &runtime_id,
            revision,
            "operator-provider-turn-pre-selector",
        )
        .expect("exact journal tail should recover the absent selector");

        assert_eq!(
            result["provider_turn_reconciliation"]["state"],
            json!("operator_reconciled")
        );
    }

    #[test]
    fn operator_reconcile_pre_selector_rejections_are_independently_fail_closed() {
        for case in [
            "present-mismatch",
            "tail-timestamp-mismatch",
            "malformed-middle",
            "missing-final-newline",
        ] {
            let tmp = tempfile::TempDir::new().expect("tempdir");
            let (context, created, runtime_id, revision) = prepare_operator_turn(&tmp);
            let dir = session_dir(&context, &created.record.id);
            let path = dir.join(ACTIVITY_FILE);
            let journal_path = dir.join(ACTIVITY_JOURNAL_FILE);
            let mut activity: Value =
                serde_json::from_slice(&fs::read(&path).expect("activity bytes"))
                    .expect("activity document");
            let activity_object = activity.as_object_mut().expect("activity object");
            if case == "present-mismatch" {
                activity_object.insert(
                    "last_provider_event_turn_id".to_string(),
                    json!("local:v1:present-but-different"),
                );
            } else {
                activity_object.remove("last_provider_event_turn_id");
            }
            fs::write(
                &path,
                serde_json::to_vec_pretty(&activity).expect("pre-selector activity document"),
            )
            .expect("seed activity");

            match case {
                "tail-timestamp-mismatch" => {
                    let journal = fs::read_to_string(&journal_path).expect("journal");
                    let mut entries = journal
                        .lines()
                        .map(|line| serde_json::from_str::<JournalEntry>(line).expect("entry"))
                        .collect::<Vec<_>>();
                    entries.last_mut().expect("exact journal tail").received_at =
                        "2030-01-01T00:00:00Z".to_string();
                    let mut changed = Vec::new();
                    for entry in entries {
                        serde_json::to_writer(&mut changed, &entry).expect("render entry");
                        changed.push(b'\n');
                    }
                    fs::write(&journal_path, changed).expect("seed timestamp-mismatched tail");
                }
                "malformed-middle" => {
                    let journal = fs::read_to_string(&journal_path).expect("journal");
                    let mut lines = journal.lines().collect::<Vec<_>>();
                    let exact_tail = lines.pop().expect("exact journal tail");
                    let mut malformed = lines.join("\n");
                    malformed.push_str("\n{not-valid-json}\n");
                    malformed.push_str(exact_tail);
                    malformed.push('\n');
                    fs::write(&journal_path, malformed).expect("seed malformed journal");
                }
                "missing-final-newline" => {
                    let mut journal = fs::read(&journal_path).expect("journal");
                    assert_eq!(journal.pop(), Some(b'\n'));
                    fs::write(&journal_path, journal).expect("seed truncated journal");
                }
                "present-mismatch" => {}
                _ => unreachable!("bounded rejection case"),
            }
            let activity_before = fs::read(&path).expect("activity before rejection");
            let journal_before = fs::read(&journal_path).expect("journal before rejection");

            let error = reconcile_operator_turn(
                &context,
                &created.record,
                &runtime_id,
                revision,
                &format!("operator-provider-turn-pre-selector-{case}"),
            )
            .expect_err("pre-selector boundary must reject");

            assert_eq!(
                error.code(),
                "operator-provider-turn-reconcile-not-admissible",
                "{case}"
            );
            assert_eq!(
                fs::read(&path).expect("activity after rejection"),
                activity_before,
                "{case}"
            );
            assert_eq!(
                fs::read(&journal_path).expect("journal after rejection"),
                journal_before,
                "{case}"
            );
        }
    }

    #[test]
    fn operator_receipt_quota_admits_64_and_preserves_state_on_65th() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created, runtime_id, revision) = prepare_operator_turn(&tmp);
        let path = session_dir(&context, &created.record.id).join(ACTIVITY_FILE);
        let mut document = read_document(&path).expect("activity");
        document.operator_provider_turn_receipts = (0..63)
            .map(|index| {
                test_operator_receipt(
                    format!("quota-key-{index:02}"),
                    format!("quota-digest-{index:02}"),
                    i64::MAX,
                )
            })
            .collect();
        write_document(&path, &mut document).expect("seed receipts");

        reconcile_operator_turn(
            &context,
            &created.record,
            &runtime_id,
            revision,
            "quota-key-63",
        )
        .expect("64th receipt");
        assert_eq!(
            read_document(&path)
                .expect("activity after 64th")
                .operator_provider_turn_receipts
                .len(),
            64
        );

        for (event_id, kind) in [
            ("operator-start-second", TurnEventKind::TurnStarted),
            ("operator-stop-second", TurnEventKind::StopObserved),
        ] {
            let mut provider_event = event(kind, event_id);
            provider_event.runtime_id = runtime_id.clone();
            ingest_event(&context, &created.record.id, provider_event).expect("second event");
        }
        let second_revision = activity_status(&context, &created.record.id)
            .expect("second activity")
            .turn_state
            .revision;
        let before = fs::read(&path).expect("activity before quota rejection");
        let error = reconcile_operator_turn(
            &context,
            &created.record,
            &runtime_id,
            second_revision,
            "quota-key-64",
        )
        .expect_err("65th receipt must reject");
        assert_eq!(error.code(), "quota-exceeded");
        assert_eq!(
            fs::read(&path).expect("activity after quota rejection"),
            before
        );
    }

    #[test]
    fn expired_operator_receipts_prune_on_hot_path_and_key_is_reusable() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created, runtime_id, revision) = prepare_operator_turn(&tmp);
        let path = session_dir(&context, &created.record.id).join(ACTIVITY_FILE);
        let mut document = read_document(&path).expect("activity");
        document.operator_provider_turn_receipts = (0..64)
            .map(|index| {
                test_operator_receipt(
                    if index == 0 {
                        "expired-reusable-key".to_string()
                    } else {
                        format!("expired-key-{index:02}")
                    },
                    "expired-digest".to_string(),
                    0,
                )
            })
            .collect();
        let seeded = serde_json::to_vec_pretty(&document).expect("expired receipt document");
        fs::write(&path, seeded).expect("seed expired receipts");

        reconcile_operator_turn(
            &context,
            &created.record,
            &runtime_id,
            revision,
            "expired-reusable-key",
        )
        .expect("expired key reuse");
        let persisted = read_document(&path).expect("activity after expired key reuse");
        assert_eq!(persisted.operator_provider_turn_receipts.len(), 1);
        assert_eq!(
            persisted.operator_provider_turn_receipts[0].idempotency_key,
            "expired-reusable-key"
        );
        let live_receipt = serde_json::to_value(&persisted.operator_provider_turn_receipts[0])
            .expect("live receipt");

        let mut with_expired_hot_path_receipt = persisted;
        with_expired_hot_path_receipt
            .operator_provider_turn_receipts
            .push(test_operator_receipt(
                "expired-hot-path-key".to_string(),
                "expired-hot-path-digest".to_string(),
                0,
            ));
        fs::write(
            &path,
            serde_json::to_vec_pretty(&with_expired_hot_path_receipt)
                .expect("hot-path receipt document"),
        )
        .expect("seed hot-path expired receipt");
        let mut progress = event(TurnEventKind::Progress, "post-reconcile-progress");
        progress.runtime_id = runtime_id;
        ingest_event(&context, &created.record.id, progress).expect("ordinary provider event");
        let persisted = read_document(&path).expect("activity after hot-path prune");
        assert_eq!(
            persisted.operator_provider_turn_receipts.len(),
            1,
            "the current live receipt remains while expired payloads stay pruned"
        );
        assert_eq!(
            serde_json::to_value(&persisted.operator_provider_turn_receipts[0])
                .expect("persisted live receipt"),
            live_receipt,
            "ordinary persistence must not mutate the retained live receipt"
        );
    }

    #[test]
    fn operator_receipt_ttl_boundary_and_pending_journal_replay_are_exact_and_read_only() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created, runtime_id, revision) = prepare_operator_turn(&tmp);
        let runtime_generation = created.record.runtime.as_ref().expect("runtime").generation;
        let admission_before = crate::coordination::now_epoch();
        let result = reconcile_operator_turn(
            &context,
            &created.record,
            &runtime_id,
            revision,
            "operator-provider-turn-ttl-boundary",
        )
        .expect("reconcile");
        let admission_after = crate::coordination::now_epoch();
        let path = session_dir(&context, &created.record.id).join(ACTIVITY_FILE);
        let mut document = read_document(&path).expect("reconciled activity");
        let receipt = document
            .operator_provider_turn_receipts
            .first()
            .expect("receipt");
        let expires_at_epoch = receipt.expires_at_epoch;
        assert!(
            admission_before + 24 * 60 * 60 <= expires_at_epoch
                && expires_at_epoch <= admission_after + 24 * 60 * 60
        );
        let encoded = serde_json::to_vec(receipt).expect("encoded receipt");
        assert!(encoded.len() < 1_024, "receipt must remain compact");
        let encoded = String::from_utf8(encoded).expect("receipt utf8");
        assert!(!encoded.contains("\"result\""));
        assert!(!encoded.contains("operator-provider-turn-reconcile-result"));
        let lock = acquire_coordination_activity_lock(&context, &created.record.id).expect("lock");
        let mut replay_result = result.clone();
        replay_result["session_incarnation"] = json!("distinct-session-incarnation");
        assert_eq!(
            operator_provider_turn_replay_locked_at(
                &context,
                &created.record.id,
                &lock,
                OperatorProviderTurnReplaySelector {
                    session_incarnation: "distinct-session-incarnation",
                    runtime_launch_id: &runtime_id,
                    runtime_generation,
                },
                "operator-provider-turn-ttl-boundary",
                "operator-provider-turn-ttl-boundary",
                expires_at_epoch - 1,
            )
            .expect("replay before expiry"),
            Some(replay_result)
        );
        assert_eq!(
            operator_provider_turn_replay_locked_at(
                &context,
                &created.record.id,
                &lock,
                OperatorProviderTurnReplaySelector {
                    session_incarnation: &runtime_id,
                    runtime_launch_id: &runtime_id,
                    runtime_generation,
                },
                "operator-provider-turn-ttl-boundary",
                "operator-provider-turn-ttl-boundary",
                expires_at_epoch,
            )
            .expect("lookup at expiry"),
            None
        );

        document.pending_journal = Some(JournalEntry {
            received_at: "2030-01-01T00:00:00Z".to_string(),
            event: event(TurnEventKind::StopObserved, "pending-after-reconciliation"),
        });
        write_document(&path, &mut document).expect("pending activity");
        let pending_bytes = fs::read(&path).expect("pending bytes");
        assert_eq!(
            operator_provider_turn_replay_locked(
                &context,
                &created.record.id,
                &lock,
                OperatorProviderTurnReplaySelector {
                    session_incarnation: &runtime_id,
                    runtime_launch_id: &runtime_id,
                    runtime_generation,
                },
                "operator-provider-turn-ttl-boundary",
                "operator-provider-turn-ttl-boundary",
            )
            .expect("pending replay"),
            Some(result)
        );
        let reused = operator_provider_turn_replay_locked(
            &context,
            &created.record.id,
            &lock,
            OperatorProviderTurnReplaySelector {
                session_incarnation: &runtime_id,
                runtime_launch_id: &runtime_id,
                runtime_generation,
            },
            "operator-provider-turn-ttl-boundary",
            "changed-digest",
        )
        .expect_err("changed digest must reject");
        assert_eq!(reused.code(), "idempotency-key-reused");
        assert_eq!(
            operator_provider_turn_replay_locked(
                &context,
                &created.record.id,
                &lock,
                OperatorProviderTurnReplaySelector {
                    session_incarnation: &runtime_id,
                    runtime_launch_id: &runtime_id,
                    runtime_generation,
                },
                "operator-provider-turn-new-key",
                "operator-provider-turn-new-key",
            )
            .expect("new key lookup"),
            None
        );
        let health_fence =
            acquire_runtime_health_fence(&context, &created.record).expect("health fence");
        let rejected = operator_reconcile_provider_turn_locked(
            &context,
            &created.record.id,
            &lock,
            &health_fence,
            OperatorProviderTurnReconcileInput {
                session_incarnation: &runtime_id,
                runtime_launch_id: &runtime_id,
                runtime_generation,
                activity_revision: document.state.revision,
                provider: &created.record.agent,
                provider_turn_id: "local:v1:unused-pending-selector",
                reason: "authoritative-completion-signal-missing",
                idempotency_key: "operator-provider-turn-new-key",
                request_digest: "operator-provider-turn-new-key",
            },
        )
        .expect_err("pending journal must reject a new key");
        assert_eq!(
            rejected.code(),
            "operator-provider-turn-reconcile-not-admissible"
        );
        assert_eq!(
            fs::read(&path).expect("activity after lookups"),
            pending_bytes
        );
    }

    #[test]
    fn coordination_activity_lock_cannot_authorize_another_session() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, alpha) = test_session(&tmp);
        let mut beta = alpha.record.clone();
        beta.id = "activity-test-beta".to_string();
        beta.tmux_session = "hs-codex-activity-test-beta".to_string();
        fs::create_dir_all(session_dir(&context, &beta.id)).expect("beta session dir");
        write_session_record(&context, &beta).expect("beta session");
        activate_runtime(&context, &alpha.record).expect("alpha activity");
        activate_runtime(&context, &beta).expect("beta activity");
        let alpha_lock =
            acquire_coordination_activity_lock(&context, &alpha.record.id).expect("alpha lock");
        let beta_path = session_dir(&context, &beta.id).join(ACTIVITY_FILE);
        let before = fs::read(&beta_path).expect("beta activity before mismatch");
        let runtime = beta.runtime.as_ref().expect("beta runtime");

        let error = operator_provider_turn_replay_locked(
            &context,
            &beta.id,
            &alpha_lock,
            OperatorProviderTurnReplaySelector {
                session_incarnation: &runtime.launch_id,
                runtime_launch_id: &runtime.launch_id,
                runtime_generation: runtime.generation,
            },
            "operator-provider-turn-lock-mismatch",
            "operator-provider-turn-lock-mismatch",
        )
        .expect_err("another session's activity lock must reject");

        assert_eq!(error.code(), "activity-lock-session-mismatch");
        let health_fence =
            acquire_runtime_health_fence(&context, &beta).expect("beta health fence");
        let error = operator_reconcile_provider_turn_locked(
            &context,
            &beta.id,
            &alpha_lock,
            &health_fence,
            OperatorProviderTurnReconcileInput {
                session_incarnation: &runtime.launch_id,
                runtime_launch_id: &runtime.launch_id,
                runtime_generation: runtime.generation,
                activity_revision: 1,
                provider: &beta.agent,
                provider_turn_id: "local:v1:mismatched-lock-provider-turn",
                reason: "authoritative-completion-signal-missing",
                idempotency_key: "operator-provider-turn-lock-mismatch-fresh",
                request_digest: "operator-provider-turn-lock-mismatch-fresh",
            },
        )
        .expect_err("another session's activity lock must reject a fresh reconciliation");
        assert_eq!(error.code(), "activity-lock-session-mismatch");
        assert_eq!(
            fs::read(&beta_path).expect("beta activity after mismatch"),
            before
        );
    }

    #[test]
    fn stream_projection_exposes_only_bounded_activity_evidence_metadata() {
        let state: TurnState = serde_json::from_value(json!({
            "schema_version": TURN_STATE_VERSION,
            "phase": "needs_input",
            "phase_changed_at": "2026-07-29T00:00:00Z",
            "revision": 9,
            "source": {
                "kind": "provider_hook",
                "provider": "claude",
                "confidence": "observed"
            },
            "semantic_event": {
                "kind": "progress",
                "observed_at": "2026-07-29T00:00:04Z"
            },
            "diagnostic": {
                "reason": "completion_evidence_pending"
            },
            "shadow_observation": {
                "observer_version": "terminal-shadow.v1",
                "rule_id": "claude-working-indicator",
                "observed_at": "2026-07-29T00:00:05Z",
                "projection": "working",
                "disagrees": false,
                "terminal": "must-not-stream",
                "prompt": "must-not-stream"
            },
            "current_turn": {
                "started_at": "2026-07-29T00:00:00Z",
                "attention": {
                    "kind": "approval",
                    "requested_at": "2026-07-29T00:00:02Z",
                    "pending_count": 1,
                    "certainty": "conservative",
                    "response": "must-not-stream"
                }
            },
            "provider_payload": "must-not-stream"
        }))
        .expect("forward-compatible state");

        let projection = serde_json::to_value(stream_projection(&state)).expect("projection");

        assert_eq!(projection["semantic_event"]["kind"], "progress");
        assert_eq!(
            projection["diagnostic"]["reason"],
            "completion_evidence_pending"
        );
        assert_eq!(
            projection["current_turn"]["attention"]["certainty"],
            "conservative"
        );
        assert_eq!(
            projection["shadow_observation"]["observer_version"],
            "terminal-shadow.v1"
        );
        let encoded = projection.to_string();
        for forbidden in [
            "must-not-stream",
            "\"provider_payload\":",
            "\"terminal\":",
            "\"prompt\":",
            "\"response\":",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "leaked forbidden field: {forbidden}"
            );
        }
    }

    #[test]
    fn stream_projection_omits_unallowlisted_activity_evidence_metadata() {
        let mut state = starting_state("2026-07-29T00:00:00Z".to_string(), 1, None);
        state.semantic_event = Some(SemanticEventView {
            kind: "provider payload".to_string(),
            observed_at: "2026-07-29T00:00:01Z".to_string(),
            extra: Map::new(),
        });
        state.diagnostic = Some(ActivityDiagnosticView {
            reason: "provider secret".to_string(),
            extra: Map::new(),
        });
        state.shadow_observation = Some(ShadowObservationView {
            observer_version: "terminal-shadow.v1".to_string(),
            rule_id: "prompt content".to_string(),
            observed_at: "2026-07-29T00:00:02Z".to_string(),
            projection: "provider response".to_string(),
            disagrees: true,
            extra: Map::new(),
        });

        let projection = serde_json::to_value(stream_projection(&state)).expect("projection");

        assert!(projection.get("semantic_event").is_none());
        assert!(projection.get("diagnostic").is_none());
        assert!(projection.get("shadow_observation").is_none());
        let encoded = projection.to_string();
        assert!(!encoded.contains("provider payload"));
        assert!(!encoded.contains("provider secret"));
        assert!(!encoded.contains("prompt content"));
        assert!(!encoded.contains("provider response"));
    }

    #[test]
    fn exact_and_uncorrelated_attention_project_distinct_certainty() {
        let exact_raw = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "AskUserQuestion",
            "tool_use_id": "exact-question"
        });
        let exact = normalize_provider_hook(AgentKind::Claude, None, "runtime-1", &exact_raw)
            .expect("exact attention")
            .expect("recognized exact attention");
        let mut exact_document = document();
        reduce(&mut exact_document, &exact, "2026-07-29T00:00:01Z");

        let conservative_raw = json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash"
        });
        let conservative =
            normalize_provider_hook(AgentKind::Claude, None, "runtime-1", &conservative_raw)
                .expect("conservative attention")
                .expect("recognized conservative attention");
        let mut conservative_document = document();
        reduce(
            &mut conservative_document,
            &conservative,
            "2026-07-29T00:00:01Z",
        );

        let exact_state =
            serde_json::to_value(stream_projection(&exact_document.state)).expect("exact state");
        let conservative_state =
            serde_json::to_value(stream_projection(&conservative_document.state))
                .expect("conservative state");
        assert_eq!(
            exact_state["current_turn"]["attention"]["certainty"],
            "exact"
        );
        assert_eq!(
            conservative_state["current_turn"]["attention"]["certainty"],
            "conservative"
        );
    }

    #[derive(Deserialize)]
    struct ActivityScenarioCorpus {
        schema_version: String,
        scenarios: Vec<ActivityScenario>,
    }

    #[derive(Deserialize)]
    struct ActivityScenario {
        id: String,
        provider: String,
        truth: String,
        events: Vec<String>,
        shadow: Option<String>,
        attention_certainty: Option<AttentionCertainty>,
        expected: ActivityScenarioExpected,
    }

    #[derive(Deserialize)]
    struct ActivityScenarioExpected {
        phase: TurnPhase,
        diagnostic: Option<String>,
        shadow_disagrees: Option<bool>,
        false_idle: Option<bool>,
        false_working: Option<bool>,
        false_blocked: Option<bool>,
    }

    fn execute_activity_scenario(scenario: &ActivityScenario) -> ActivityDocument {
        let mut result = document();
        result.state = TurnState {
            schema_version: TURN_STATE_VERSION.to_string(),
            phase: TurnPhase::Unknown,
            phase_changed_at: "2026-07-10T00:00:00Z".to_string(),
            revision: 0,
            source: runtime_source(),
            semantic_event: None,
            diagnostic: None,
            shadow_observation: None,
            current_turn: None,
            last_turn: None,
            extra: Map::new(),
        };
        let mut last_progress = None;
        for (index, action) in scenario.events.iter().enumerate() {
            if action == "runtime_changed" {
                result.state =
                    starting_state(format!("2026-07-10T00:00:{:02}Z", index + 1), 1, None);
                continue;
            }
            let kind = match action.as_str() {
                "turn_started" => TurnEventKind::TurnStarted,
                "attention_requested" => TurnEventKind::AttentionRequested,
                "progress" | "duplicate_progress" => TurnEventKind::Progress,
                "stop_observed" => TurnEventKind::StopObserved,
                "turn_completed" => TurnEventKind::TurnCompleted,
                "late_completion" => TurnEventKind::TurnCompleted,
                unknown => panic!("{} has unknown corpus event {unknown}", scenario.id),
            };
            let event_id = if action == "duplicate_progress" {
                last_progress
                    .clone()
                    .unwrap_or_else(|| format!("{}-progress", scenario.id))
            } else {
                format!("{}-{index}", scenario.id)
            };
            let mut input = event(kind.clone(), &event_id);
            input.provider = scenario.provider.clone();
            if kind == TurnEventKind::AttentionRequested {
                input.attention_id = Some(format!("{}-attention", scenario.id));
                input.attention_kind = Some("approval".to_string());
                input.attention_correlation_exact =
                    scenario.attention_certainty == Some(AttentionCertainty::Exact);
            }
            if action == "late_completion" {
                input.provider_turn_id = Some("older-turn".to_string());
            }
            let at = format!("2026-07-10T00:00:{:02}Z", index + 1);
            reduce(&mut result, &input, &at);
            if action == "progress" {
                last_progress = Some(event_id);
            }
            result.state.diagnostic = (kind == TurnEventKind::StopObserved
                && result.state.current_turn.is_some())
            .then(|| ActivityDiagnosticView {
                reason: "completion_evidence_pending".to_string(),
                extra: Map::new(),
            });
        }
        result
    }

    #[test]
    fn provider_activity_scenario_corpus_is_executable_content_free_and_covers_drift_risks() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/activity/provider-activity-scenarios.json"
        ))
        .expect("scenario corpus");
        assert_eq!(
            fixture["schema_version"],
            "agent-session.activity-scenarios.v1"
        );
        let scenarios = fixture["scenarios"].as_array().expect("scenario array");
        assert!(scenarios.len() >= 12);
        let ids = scenarios
            .iter()
            .filter_map(|scenario| scenario["id"].as_str())
            .collect::<Vec<_>>();
        for required in [
            "raw-stop-without-completion",
            "dropped-codex-completion",
            "generic-permission-later-progress",
            "exact-correlated-prompt",
            "nested-agent-completion",
            "background-helper-output",
            "transcript-view",
            "osc-disabled",
            "stale-osc-title",
            "runtime-reconnect",
            "duplicate-out-of-order-events",
            "provider-ui-drift",
        ] {
            assert!(ids.contains(&required), "missing scenario {required}");
        }
        let encoded = fixture.to_string();
        for forbidden in [
            "\"prompt\"",
            "\"command\"",
            "\"response\"",
            "\"terminal\"",
            "\"transcript_path\"",
            "\"credential\"",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "scenario corpus leaked a content-bearing field: {forbidden}"
            );
        }

        let corpus: ActivityScenarioCorpus =
            serde_json::from_value(fixture).expect("typed scenario corpus");
        assert_eq!(corpus.schema_version, "agent-session.activity-scenarios.v1");
        for scenario in corpus.scenarios {
            let result = execute_activity_scenario(&scenario);
            assert_eq!(
                result.state.phase, scenario.expected.phase,
                "{} phase",
                scenario.id
            );
            if let Some(expected) = scenario.expected.diagnostic.as_deref() {
                assert_eq!(
                    result
                        .state
                        .diagnostic
                        .as_ref()
                        .map(|value| value.reason.as_str()),
                    Some(expected),
                    "{} diagnostic",
                    scenario.id
                );
            }
            if let Some(expected) = scenario.expected.shadow_disagrees {
                assert_eq!(
                    shadow::disagrees(
                        &result.state.phase,
                        scenario.shadow.as_deref().unwrap_or("unknown")
                    ),
                    expected,
                    "{} shadow disagreement",
                    scenario.id
                );
            }
            let false_idle =
                result.state.phase == TurnPhase::Waiting && scenario.truth != "waiting";
            let false_working = result.state.phase == TurnPhase::Working
                && !matches!(
                    scenario.truth.as_str(),
                    "working" | "working_or_unknown" | "working_with_unconfirmed_attention"
                );
            let exact_attention = result
                .state
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .is_some_and(|attention| attention.certainty == AttentionCertainty::Exact);
            let false_blocked = result.state.phase == TurnPhase::NeedsInput
                && exact_attention
                && scenario.truth != "needs_input";
            if let Some(expected) = scenario.expected.false_idle {
                assert_eq!(false_idle, expected, "{} false idle", scenario.id);
            }
            if let Some(expected) = scenario.expected.false_working {
                assert_eq!(false_working, expected, "{} false working", scenario.id);
            }
            if let Some(expected) = scenario.expected.false_blocked {
                assert_eq!(false_blocked, expected, "{} false blocked", scenario.id);
            }
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
            coordination_mode: crate::cli::CoordinationMode::Advisory,
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
    fn raw_stop_keeps_working_and_projects_pending_completion_evidence() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created) = test_session(&tmp);
        activate_runtime(&context, &created.record).expect("activate runtime");
        let runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        let mut started = event(TurnEventKind::TurnStarted, "started");
        started.runtime_id = runtime_id.clone();
        ingest_event(&context, &created.record.id, started).expect("turn start");
        let mut stop = event(TurnEventKind::StopObserved, "raw-stop");
        stop.runtime_id = runtime_id;
        let state = ingest_event(&context, &created.record.id, stop)
            .expect("raw stop")
            .turn_state;

        assert_eq!(state.phase, TurnPhase::Working);
        assert_eq!(
            state
                .semantic_event
                .as_ref()
                .map(|event| event.kind.as_str()),
            Some("stop_observed")
        );
        assert_eq!(
            state
                .diagnostic
                .as_ref()
                .map(|diagnostic| diagnostic.reason.as_str()),
            Some("completion_evidence_pending")
        );
    }

    #[test]
    fn claude_notification_waiting_requires_stop_and_no_reactivation() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let (context, created) = test_session_for_agent(&tmp, AgentKind::Claude);
        activate_runtime(&context, &created.record).expect("activate runtime");
        let runtime_id = created
            .record
            .runtime
            .as_ref()
            .expect("runtime")
            .launch_id
            .clone();
        let mut stop = event(TurnEventKind::StopObserved, "claude-stop");
        stop.runtime_id = runtime_id.clone();
        stop.provider = AgentKind::Claude.as_str().to_string();
        stop.provider_turn_id = None;
        ingest_event(&context, &created.record.id, stop).expect("ingest stop");

        assert!(claude_notification_waiting(
            &context,
            &created.record,
            Duration::ZERO
        ));
        assert!(!claude_notification_waiting(
            &context,
            &created.record,
            Duration::from_secs(1)
        ));

        let mut reactivated = event(TurnEventKind::Progress, "claude-reactivated");
        reactivated.runtime_id = runtime_id;
        reactivated.provider = AgentKind::Claude.as_str().to_string();
        reactivated.provider_turn_id = None;
        ingest_event(&context, &created.record.id, reactivated).expect("ingest reactivation");
        assert!(!claude_notification_waiting(
            &context,
            &created.record,
            Duration::ZERO
        ));
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
            coordination_mode: crate::cli::CoordinationMode::Advisory,
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
            coordination_mode: crate::cli::CoordinationMode::Advisory,
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
            coordination_mode: crate::cli::CoordinationMode::Advisory,
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
                validate_event(&event, EventAdmission::Generic).expect("normalized fixture event");
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
        write_document(&path, &mut snapshot).expect("near-capacity snapshot");

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
            coordination_mode: crate::cli::CoordinationMode::Advisory,
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
    fn codex_marker_lines_inside_multiline_strings_are_not_owned_blocks() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        for quotes in ["\"\"\"", "'''"] {
            let raw = format!(
                "note = {quotes}\n{CODEX_HOOK_BLOCK_START}\nprivate\n{CODEX_HOOK_BLOCK_END}\n{quotes}\n"
            );

            let analysis = analyze_codex_toml_hooks(&path, &raw).expect("marker-shaped value");
            assert_eq!(analysis.marker_layout, CodexTomlHookMarkerLayout::Absent);
        }
    }

    #[test]
    fn codex_single_orphan_marker_is_an_owned_repair_fragment() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        for (marker, expected_layout) in [
            (
                CODEX_HOOK_BLOCK_START,
                CodexTomlHookMarkerLayout::OrphanStart,
            ),
            (CODEX_HOOK_BLOCK_END, CodexTomlHookMarkerLayout::OrphanEnd),
        ] {
            let raw = format!("keep = true\n{marker}\n");
            let analysis = analyze_codex_toml_hooks(&path, &raw).expect("owned orphan marker");
            assert_eq!(analysis.marker_layout, expected_layout);
        }
    }

    #[test]
    fn codex_duplicate_or_reversed_markers_remain_ambiguous() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        for raw in [
            format!("{CODEX_HOOK_BLOCK_START}\n{CODEX_HOOK_BLOCK_START}\n"),
            format!("{CODEX_HOOK_BLOCK_END}\n{CODEX_HOOK_BLOCK_END}\n"),
            format!("{CODEX_HOOK_BLOCK_END}\n{CODEX_HOOK_BLOCK_START}\n"),
        ] {
            assert_eq!(
                analyze_codex_toml_hooks(&path, &raw)
                    .expect_err("ambiguous marker layout")
                    .code(),
                "provider-config-invalid"
            );
        }
    }

    #[test]
    fn codex_inline_permission_source_guard_rejects_noncanonical_reporters() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        let canonical = format!(
            "[[hooks.PermissionRequest]]\n\n[[hooks.PermissionRequest.hooks]]\ntype = \"command\"\ncommand = {}\ntimeout = 5\n",
            toml_edit::Value::from(owned_command(AgentKind::Codex, Some("PermissionRequest")))
        );
        fs::write(&config_path, &canonical).expect("owned inline hooks");
        assert!(codex_toml_permission_source_guard(&config_path));

        for command in [
            owned_command(AgentKind::Codex, Some("PermissionRequest")),
            "agent-session activity hook --agent=codex".to_string(),
        ] {
            let mut duplicate = canonical.clone();
            duplicate.push_str(&format!(
                "\n[[hooks.PermissionRequest]]\n\n[[hooks.PermissionRequest.hooks]]\ntype = \"command\"\ncommand = {}\ntimeout = 5\n",
                toml_edit::Value::from(command)
            ));
            fs::write(&config_path, duplicate).expect("duplicate reporter");
            assert!(!codex_toml_permission_source_guard(&config_path));
        }

        for matcher in ["5", "false", "[]", "{}"] {
            let malformed = canonical.replacen(
                "[[hooks.PermissionRequest]]",
                &format!("[[hooks.PermissionRequest]]\nmatcher = {matcher}"),
                1,
            );
            fs::write(&config_path, malformed).expect("malformed matcher");
            assert!(
                !codex_toml_permission_source_guard(&config_path),
                "matcher {matcher} must fail closed"
            );
        }
    }

    #[test]
    fn codex_json_permission_source_guard_rejects_noncanonical_reporters() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("hooks.json");
        let canonical = json!({
            "hooks": {
                "PermissionRequest": [{
                    "hooks": [{
                        "type": "command",
                        "command": owned_command(AgentKind::Codex, Some("PermissionRequest")),
                        "timeout": 5
                    }]
                }]
            }
        });
        fs::write(
            &path,
            serde_json::to_vec_pretty(&canonical).expect("JSON bytes"),
        )
        .expect("JSON hooks");
        assert!(codex_json_permission_source_guard(&path));

        for matcher in [Value::Null, json!(5), json!(false), json!([]), json!({})] {
            let mut malformed = canonical.clone();
            malformed["hooks"]["PermissionRequest"][0]["matcher"] = matcher;
            fs::write(
                &path,
                serde_json::to_vec_pretty(&malformed).expect("JSON bytes"),
            )
            .expect("malformed matcher");
            assert!(!codex_json_permission_source_guard(&path));
        }

        let mut value = canonical;
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
            validate_event(&event, EventAdmission::Generic).expect("valid turn event fixture");
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

    #[test]
    fn provider_capacity_is_reserved_for_internal_protocol_v2_admission() {
        let mut capacity = event(TurnEventKind::TurnFailed, "provider-capacity");
        capacity.confidence = Confidence::Authoritative;
        capacity.source_kind = SourceKind::ProviderHook;
        capacity.failure_reason = Some("provider_capacity".to_string());

        let v1_error = validate_event(&capacity, EventAdmission::Generic)
            .expect_err("the closed public v1 failure union must reject provider capacity");
        assert_eq!(v1_error.code(), "activity-failure-reason-invalid");

        capacity.schema_version = CODEX_PROTOCOL_TURN_EVENT_VERSION.to_string();
        let generic_v2_error = validate_event(&capacity, EventAdmission::Generic)
            .expect_err("generic activity ingress must not impersonate provider protocol v2");
        assert_eq!(generic_v2_error.code(), "unsupported-turn-event-version");
        validate_event(&capacity, EventAdmission::CodexProtocol)
            .expect("internal protocol admission accepts the exact v2 capacity reason");
    }
}

fn validate_event(event: &TurnEvent, admission: EventAdmission) -> Result<(), CliError> {
    let schema_supported = event.schema_version == TURN_EVENT_VERSION
        || (admission == EventAdmission::CodexProtocol
            && event.schema_version == CODEX_PROTOCOL_TURN_EVENT_VERSION);
    if !schema_supported {
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
    if !matches!(
        event.provider.as_str(),
        "codex" | "claude" | "hermes" | "dsh"
    ) {
        return Err(CliError::data(
            "activity-provider-unsupported",
            "activity event provider must be codex, claude, hermes, or dsh",
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
    if let Some(reason) = event.failure_reason.as_deref() {
        let reason_allowed = matches!(
            reason,
            "usage_exhausted"
                | "authentication"
                | "organization"
                | "billing"
                | "invalid_request"
                | "service"
                | "max_output_tokens"
                | "unknown"
        ) || (reason == "provider_capacity"
            && admission == EventAdmission::CodexProtocol
            && event.schema_version == CODEX_PROTOCOL_TURN_EVENT_VERSION);
        if event.kind != TurnEventKind::TurnFailed
            || event.confidence != Confidence::Authoritative
            || !reason_allowed
        {
            return Err(CliError::data(
                "activity-failure-reason-invalid",
                "failure_reason requires an authoritative turn_failed event and an allowlisted value",
                None,
            ));
        }
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
                    overflow.certainty = AttentionCertainty::Conservative;
                } else {
                    document.overflow_attention = Some(OverflowAttention {
                        kind,
                        requested_at: at.to_string(),
                        count: 1,
                        certainty: AttentionCertainty::Conservative,
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
                        certainty: if event.attention_correlation_exact {
                            AttentionCertainty::Exact
                        } else {
                            AttentionCertainty::Conservative
                        },
                        extra: Map::new(),
                    });
                } else if let Some(overflow) = document.overflow_attention.as_mut() {
                    overflow.count = overflow.count.saturating_add(1);
                    if !event.attention_correlation_exact {
                        overflow.certainty = AttentionCertainty::Conservative;
                    }
                } else {
                    document.overflow_attention = Some(OverflowAttention {
                        kind,
                        requested_at: at.to_string(),
                        count: 1,
                        certainty: if event.attention_correlation_exact {
                            AttentionCertainty::Exact
                        } else {
                            AttentionCertainty::Conservative
                        },
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
                let extra = if let Some(current) = current.as_ref() {
                    current.extra.clone()
                } else if let (Some(event_turn_id), Some(last_turn)) = (
                    event.provider_turn_id.as_ref(),
                    document.state.last_turn.as_ref(),
                ) {
                    if last_turn.provider_turn_id.as_ref() == Some(event_turn_id) {
                        last_turn.extra.clone()
                    } else {
                        Map::new()
                    }
                } else {
                    Map::new()
                };
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
                    extra: if event.failure_reason.as_deref() == Some("provider_capacity") {
                        let mut extra = extra;
                        extra.insert(
                            "provider_failure_kind".to_string(),
                            json!("provider_capacity"),
                        );
                        extra
                    } else {
                        extra
                    },
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

fn turn_event_kind_name(kind: &TurnEventKind) -> &'static str {
    match kind {
        TurnEventKind::TurnStarted => "turn_started",
        TurnEventKind::AttentionRequested => "attention_requested",
        TurnEventKind::AttentionCleared => "attention_cleared",
        TurnEventKind::Progress => "progress",
        TurnEventKind::StopObserved => "stop_observed",
        TurnEventKind::TurnCompleted => "turn_completed",
        TurnEventKind::TurnFailed => "turn_failed",
    }
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
            certainty: if document
                .pending_attention
                .iter()
                .all(|pending| pending.certainty == AttentionCertainty::Exact)
                && document
                    .overflow_attention
                    .as_ref()
                    .is_none_or(|overflow| overflow.certainty == AttentionCertainty::Exact)
            {
                AttentionCertainty::Exact
            } else {
                AttentionCertainty::Conservative
            },
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
                    certainty: overflow.certainty.clone(),
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

fn write_document(path: &Path, document: &mut ActivityDocument) -> Result<(), CliError> {
    let now_epoch = crate::coordination::now_epoch();
    document
        .operator_provider_turn_receipts
        .retain(|receipt| receipt.expires_at_epoch > now_epoch);
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

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use jiff::Timestamp;
use nils_common::fs::{SECRET_FILE_MODE, write_atomic};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    CliContext, CliError, SessionRecord, acquire_session_record_lock,
    acquire_session_record_lock_timed, load_session_record, session_dir,
    try_acquire_session_record_lock,
};

const AUTO_RESUME_SCHEMA_VERSION: &str = "agent-session.auto-resume.v1";
const AUTO_RESUME_FILE: &str = "auto-resume.json";
const MAX_TRANSIENT_ATTEMPTS: u32 = 5;
const RETRY_DELAYS_SECONDS: [i64; 5] = [30, 60, 120, 300, 600];
const PROTOCOL_STATE_LOCK_TIMEOUT: Duration = Duration::from_secs(1);

pub(crate) const CONTINUATION_MESSAGE: &str = "Continue the interrupted task from where you stopped. First inspect the current session and repository state, then continue toward the existing objective. Do not repeat completed work.";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DurableAutoResume {
    schema_version: String,
    enabled: bool,
    state: String,
    updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheduled_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_check_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blocked_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blocked_revision: Option<u64>,
    #[serde(default)]
    attempt: u32,
    #[serde(default)]
    ever_scheduled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct AutoResumeView {
    pub(crate) schema_version: &'static str,
    pub(crate) supported: bool,
    pub(crate) enabled: bool,
    pub(crate) state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scheduled_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) failure_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageSnapshot {
    pub(crate) authoritative: bool,
    pub(crate) has_exhausted_windows: bool,
    pub(crate) exhausted_reset_epochs: Vec<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PendingSessions {
    pub(crate) recovery_ids: Vec<String>,
    pub(crate) usage_ids: Vec<String>,
    pub(crate) error_codes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TickOutcome {
    Unchanged,
    Scheduled,
    Resumed,
    Retrying,
    TerminalFailure,
}

fn supported(record: &SessionRecord) -> bool {
    // Claude Code StopFailure exposes an official structured `error=rate_limit`
    // signal. Codex is supported only when this exact runtime was launched
    // through the app-server v2 protocol; the standalone TUI notification
    // surface remains fail-closed because it has no structured failure reason.
    crate::session_profile_auto_resume_supported(record)
        && (record.agent == "claude" || crate::codex_app_server::runtime_is_supported(record))
}

fn default_state(now: &str) -> DurableAutoResume {
    DurableAutoResume {
        schema_version: AUTO_RESUME_SCHEMA_VERSION.to_string(),
        enabled: false,
        state: "disabled".to_string(),
        updated_at: now.to_string(),
        scheduled_at: None,
        next_check_at: None,
        failure_reason: None,
        blocked_turn_id: None,
        blocked_revision: None,
        attempt: 0,
        ever_scheduled: false,
    }
}

fn path(context: &CliContext, id: &str) -> PathBuf {
    session_dir(context, id).join(AUTO_RESUME_FILE)
}

fn read_state(context: &CliContext, id: &str, now: &str) -> Result<DurableAutoResume, CliError> {
    let path = path(context, id);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(default_state(now)),
        Err(err) => {
            return Err(CliError::runtime(
                "auto-resume-read-failed",
                format!("failed to read durable auto-resume state: {err}"),
                Some(json!({ "id": id })),
            ));
        }
    };
    let state: DurableAutoResume = serde_json::from_slice(&bytes).map_err(|_| {
        CliError::data(
            "auto-resume-state-invalid",
            "durable auto-resume state is malformed",
            Some(json!({ "id": id })),
        )
    })?;
    if state.schema_version != AUTO_RESUME_SCHEMA_VERSION {
        return Err(CliError::data(
            "auto-resume-version-unsupported",
            "durable auto-resume state has an unsupported schema version",
            Some(json!({ "id": id })),
        ));
    }
    Ok(state)
}

fn write_state(context: &CliContext, id: &str, state: &DurableAutoResume) -> Result<(), CliError> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|err| {
        CliError::runtime(
            "auto-resume-render-failed",
            format!("failed to render durable auto-resume state: {err}"),
            Some(json!({ "id": id })),
        )
    })?;
    write_atomic(&path(context, id), &bytes, SECRET_FILE_MODE).map_err(|err| {
        CliError::runtime(
            "auto-resume-write-failed",
            format!("failed to write durable auto-resume state: {err}"),
            Some(json!({ "id": id })),
        )
    })
}

fn view(record: &SessionRecord, state: DurableAutoResume) -> AutoResumeView {
    AutoResumeView {
        schema_version: AUTO_RESUME_SCHEMA_VERSION,
        supported: supported(record),
        enabled: state.enabled,
        state: state.state,
        scheduled_at: state.scheduled_at,
        failure_reason: state.failure_reason,
    }
}

fn projection_unavailable(state: &DurableAutoResume) -> bool {
    state.state == "terminal_failure"
        && state.failure_reason.as_deref() == Some("state_unavailable")
}

pub(crate) fn view_for_record(context: &CliContext, record: &SessionRecord) -> AutoResumeView {
    if crate::activity::runtime_is_unhealthy(context, record) {
        return AutoResumeView {
            schema_version: AUTO_RESUME_SCHEMA_VERSION,
            supported: supported(record),
            enabled: false,
            state: "terminal_failure".to_string(),
            scheduled_at: None,
            failure_reason: Some("state_unavailable".to_string()),
        };
    }
    let now = Timestamp::now().to_string();
    match read_state(context, &record.id, &now) {
        Ok(state) => view(record, state),
        Err(_) => AutoResumeView {
            schema_version: AUTO_RESUME_SCHEMA_VERSION,
            supported: supported(record),
            enabled: false,
            state: "terminal_failure".to_string(),
            scheduled_at: None,
            failure_reason: Some("state_unavailable".to_string()),
        },
    }
}

pub(crate) fn set_enabled(
    context: &CliContext,
    id: &str,
    enabled: bool,
    now: &str,
) -> Result<AutoResumeView, CliError> {
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _lock = acquire_session_record_lock(context, &canonical_id)?;
    let record = load_session_record(context, &canonical_id)?;
    crate::ensure_same_session_identity(&observed, &record)?;
    if enabled && crate::activity::runtime_is_unhealthy(context, &record) {
        return Err(CliError::data(
            "auto-resume-state-unavailable",
            "auto-resume projection is unavailable until this session runtime is restarted",
            Some(json!({ "id": record.id })),
        ));
    }
    if enabled && !supported(&record) {
        return Err(CliError::data(
            "auto-resume-unsupported",
            "this provider does not expose an authoritative structured usage-exhaustion signal",
            Some(json!({ "id": record.id, "provider": record.agent })),
        ));
    }
    let mut state = read_state(context, &record.id, now)?;
    if projection_unavailable(&state) {
        if enabled {
            return Err(CliError::data(
                "auto-resume-state-unavailable",
                "auto-resume projection is unavailable until this session runtime is restarted",
                Some(json!({ "id": record.id })),
            ));
        }
        return Ok(view(&record, state));
    }
    state.enabled = enabled;
    state.state = if enabled { "enabled" } else { "disabled" }.to_string();
    state.updated_at = now.to_string();
    state.scheduled_at = None;
    state.next_check_at = None;
    state.failure_reason = None;
    state.blocked_turn_id = None;
    state.blocked_revision = None;
    state.attempt = 0;
    state.ever_scheduled = false;
    write_state(context, &record.id, &state)?;
    Ok(view(&record, state))
}

pub(crate) fn cancel(
    context: &CliContext,
    id: &str,
    now: &str,
) -> Result<AutoResumeView, CliError> {
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _lock = acquire_session_record_lock(context, &canonical_id)?;
    let record = load_session_record(context, &canonical_id)?;
    crate::ensure_same_session_identity(&observed, &record)?;
    let mut state = read_state(context, &record.id, now)?;
    if projection_unavailable(&state) {
        return Ok(view(&record, state));
    }
    if state.state == "resumed" {
        return Err(CliError::data(
            "auto-resume-already-submitted",
            "the continuation was already submitted",
            Some(json!({ "id": record.id })),
        ));
    }
    state.enabled = false;
    state.state = "cancelled".to_string();
    state.updated_at = now.to_string();
    state.scheduled_at = None;
    state.next_check_at = None;
    state.failure_reason = None;
    write_state(context, &record.id, &state)?;
    Ok(view(&record, state))
}

/// Cancel an armed continuation before a human or another control-plane caller
/// writes to the pane. The caller must already hold the session record lock so
/// cancellation and input remain one serialized operation.
pub(crate) fn cancel_for_manual_input_locked(
    context: &CliContext,
    id: &str,
    now: &str,
) -> Result<(), CliError> {
    cancel_active_locked(context, id, now, "manual_input")
}

pub(crate) fn cancel_for_account_switch_locked(
    context: &CliContext,
    id: &str,
    now: &str,
) -> Result<(), CliError> {
    cancel_active_locked(context, id, now, "account_switch")
}

fn cancel_active_locked(
    context: &CliContext,
    id: &str,
    now: &str,
    reason: &str,
) -> Result<(), CliError> {
    let mut state = read_state(context, id, now)?;
    if !state.enabled
        || !matches!(
            state.state.as_str(),
            "armed" | "scheduled" | "checking" | "transient_failure"
        )
    {
        return Ok(());
    }
    state.enabled = false;
    state.state = "cancelled".to_string();
    state.updated_at = now.to_string();
    state.scheduled_at = None;
    state.next_check_at = None;
    state.failure_reason = Some(reason.to_string());
    write_state(context, id, &state)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManualInputCancelOutcome {
    Ready,
    Busy,
    RuntimeChanged,
}

pub(crate) fn try_cancel_for_manual_input_for_runtime(
    context: &CliContext,
    id: &str,
    expected_launch_id: &str,
    now: &str,
) -> Result<ManualInputCancelOutcome, CliError> {
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let Some(_lock) = try_acquire_session_record_lock(context, &canonical_id)? else {
        return Ok(ManualInputCancelOutcome::Busy);
    };
    let mut record = load_session_record(context, &canonical_id)?;
    crate::ensure_same_session_identity(&observed, &record)?;
    if !runtime_matches(&record, Some(expected_launch_id)) {
        return Ok(ManualInputCancelOutcome::RuntimeChanged);
    }
    crate::codex_account::authorize_input_locked(context, &mut record)?;
    cancel_for_manual_input_locked(context, &record.id, now)?;
    Ok(ManualInputCancelOutcome::Ready)
}

pub(crate) fn fail_closed_projection_for_runtime(
    context: &CliContext,
    id: &str,
    expected_launch_id: &str,
    now: &str,
) -> Result<(), CliError> {
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _lock =
        acquire_session_record_lock_timed(context, &canonical_id, PROTOCOL_STATE_LOCK_TIMEOUT)?;
    let record = load_session_record(context, &canonical_id)?;
    crate::ensure_same_session_identity(&observed, &record)?;
    if !runtime_matches(&record, Some(expected_launch_id)) {
        return Ok(());
    }
    let mut state = read_state(context, &record.id, now)?;
    if !state.enabled
        || !matches!(
            state.state.as_str(),
            "enabled" | "armed" | "scheduled" | "checking" | "transient_failure"
        )
    {
        return Ok(());
    }
    state.enabled = false;
    state.state = "terminal_failure".to_string();
    state.updated_at = now.to_string();
    state.scheduled_at = None;
    state.next_check_at = None;
    state.failure_reason = Some("state_unavailable".to_string());
    write_state(context, &record.id, &state)
}

pub(crate) fn arm_usage_exhaustion(
    context: &CliContext,
    id: &str,
    blocked_turn_id: String,
    blocked_revision: u64,
    now: &str,
) -> Result<bool, CliError> {
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _lock =
        acquire_session_record_lock_timed(context, &canonical_id, PROTOCOL_STATE_LOCK_TIMEOUT)?;
    let record = load_session_record(context, &canonical_id)?;
    crate::ensure_same_session_identity(&observed, &record)?;
    if !supported(&record) {
        return Ok(false);
    }
    let mut state = read_state(context, &record.id, now)?;
    if !state.enabled {
        return Ok(false);
    }
    if state.blocked_turn_id.as_deref() == Some(blocked_turn_id.as_str())
        && matches!(
            state.state.as_str(),
            "armed" | "scheduled" | "checking" | "resumed"
        )
    {
        return Ok(false);
    }
    state.state = "armed".to_string();
    state.updated_at = now.to_string();
    state.scheduled_at = None;
    state.next_check_at = None;
    state.failure_reason = None;
    state.blocked_turn_id = Some(blocked_turn_id);
    state.blocked_revision = Some(blocked_revision);
    state.attempt = 0;
    state.ever_scheduled = false;
    write_state(context, &record.id, &state)?;
    Ok(true)
}

pub(crate) fn pending_sessions(
    context: &CliContext,
    now_epoch: i64,
) -> Result<PendingSessions, CliError> {
    let root = context.state_dir.join("sessions");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PendingSessions::default());
        }
        Err(err) => {
            return Err(CliError::runtime(
                "auto-resume-discovery-failed",
                format!("failed to inspect session state: {err}"),
                None,
            ));
        }
    };
    let mut pending = PendingSessions::default();
    for entry in entries {
        let entry = entry.map_err(|err| {
            CliError::runtime(
                "auto-resume-discovery-failed",
                format!("failed to inspect session state: {err}"),
                None,
            )
        })?;
        let Ok(id) = entry.file_name().into_string() else {
            continue;
        };
        let now = Timestamp::now().to_string();
        let state = match read_state(context, &id, &now) {
            Ok(state) => state,
            Err(err) => {
                pending.error_codes.push(err.code().to_string());
                continue;
            }
        };
        if !state.enabled {
            continue;
        }
        match state.state.as_str() {
            "checking" => pending.recovery_ids.push(id),
            "armed" => pending.usage_ids.push(id),
            "scheduled"
                if state
                    .scheduled_at
                    .as_deref()
                    .and_then(epoch_from_string)
                    .is_none_or(|due| due <= now_epoch) =>
            {
                pending.usage_ids.push(id);
            }
            "transient_failure"
                if state
                    .next_check_at
                    .as_deref()
                    .and_then(epoch_from_string)
                    .is_none_or(|due| due <= now_epoch) =>
            {
                pending.usage_ids.push(id);
            }
            _ => {}
        }
    }
    pending.recovery_ids.sort();
    pending.usage_ids.sort();
    pending.error_codes.sort();
    Ok(pending)
}

pub(crate) fn record_scheduler_error(
    context: &CliContext,
    id: &str,
    now_epoch: i64,
    reason: &str,
) -> Result<TickOutcome, CliError> {
    record_scheduler_error_inner(context, id, None, now_epoch, reason)
}

pub(crate) fn record_scheduler_error_for_runtime(
    context: &CliContext,
    id: &str,
    expected_launch_id: &str,
    now_epoch: i64,
    reason: &str,
) -> Result<TickOutcome, CliError> {
    record_scheduler_error_inner(context, id, Some(expected_launch_id), now_epoch, reason)
}

fn record_scheduler_error_inner(
    context: &CliContext,
    id: &str,
    expected_launch_id: Option<&str>,
    now_epoch: i64,
    reason: &str,
) -> Result<TickOutcome, CliError> {
    let now = epoch_string(now_epoch)?;
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _lock =
        acquire_session_record_lock_timed(context, &canonical_id, PROTOCOL_STATE_LOCK_TIMEOUT)?;
    let record = load_session_record(context, &canonical_id)?;
    crate::ensure_same_session_identity(&observed, &record)?;
    if !runtime_matches(&record, expected_launch_id) {
        return Ok(TickOutcome::Unchanged);
    }
    if crate::activity::runtime_is_unhealthy(context, &record) {
        let mut state = read_state(context, &record.id, &now)?;
        state.enabled = false;
        state.state = "terminal_failure".to_string();
        state.updated_at = now;
        state.failure_reason = Some("state_unavailable".to_string());
        write_state(context, &record.id, &state)?;
        return Ok(TickOutcome::TerminalFailure);
    }
    let state = read_state(context, &record.id, &now)?;
    if !state.enabled
        || !matches!(
            state.state.as_str(),
            "armed" | "scheduled" | "transient_failure"
        )
    {
        return Ok(TickOutcome::Unchanged);
    }
    record_retry(context, &record, state, now_epoch, reason)
}

/// Advance an already-scheduled claim when the bound provider reports that
/// usage is available before the previously advertised reset epoch. This is
/// intentionally narrower than arming: reconnects and rate-limit updates may
/// wake an existing claim, but can never create one or revive a cancellation.
#[cfg(test)]
pub(crate) fn wake_scheduled_if_usage_open(
    context: &CliContext,
    id: &str,
    now_epoch: i64,
) -> Result<bool, CliError> {
    wake_scheduled_if_usage_open_inner(context, id, None, now_epoch)
}

pub(crate) fn wake_scheduled_if_usage_open_for_runtime(
    context: &CliContext,
    id: &str,
    expected_launch_id: &str,
    now_epoch: i64,
) -> Result<bool, CliError> {
    wake_scheduled_if_usage_open_inner(context, id, Some(expected_launch_id), now_epoch)
}

fn wake_scheduled_if_usage_open_inner(
    context: &CliContext,
    id: &str,
    expected_launch_id: Option<&str>,
    now_epoch: i64,
) -> Result<bool, CliError> {
    let now = epoch_string(now_epoch)?;
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let Some(_lock) = try_acquire_session_record_lock(context, &canonical_id)? else {
        return Ok(false);
    };
    let record = load_session_record(context, &canonical_id)?;
    crate::ensure_same_session_identity(&observed, &record)?;
    if !runtime_matches(&record, expected_launch_id) {
        return Ok(false);
    }
    let mut state = read_state(context, &record.id, &now)?;
    if !state.enabled || state.state != "scheduled" || !state.ever_scheduled {
        return Ok(false);
    }
    let due = state
        .scheduled_at
        .as_deref()
        .and_then(epoch_from_string)
        .is_none_or(|due| due <= now_epoch);
    if due {
        return Ok(false);
    }
    state.updated_at = now.clone();
    state.scheduled_at = Some(now);
    write_state(context, &record.id, &state)?;
    Ok(true)
}

pub(crate) fn tick<F>(
    context: &CliContext,
    id: &str,
    now_epoch: i64,
    usage: &UsageSnapshot,
    submit: F,
) -> Result<TickOutcome, CliError>
where
    F: FnMut(&SessionRecord) -> Result<(), CliError>,
{
    tick_inner(context, id, None, None, now_epoch, usage, submit)
}

#[cfg(test)]
pub(crate) fn tick_for_runtime<F>(
    context: &CliContext,
    id: &str,
    expected_launch_id: &str,
    now_epoch: i64,
    usage: &UsageSnapshot,
    submit: F,
) -> Result<TickOutcome, CliError>
where
    F: FnMut(&SessionRecord) -> Result<(), CliError>,
{
    tick_inner(
        context,
        id,
        Some(expected_launch_id),
        None,
        now_epoch,
        usage,
        submit,
    )
}

pub(crate) fn tick_for_runtime_and_binding<F>(
    context: &CliContext,
    id: &str,
    expected_launch_id: &str,
    expected_binding: &crate::codex_account::BindingSnapshot,
    now_epoch: i64,
    usage: &UsageSnapshot,
    submit: F,
) -> Result<TickOutcome, CliError>
where
    F: FnMut(&SessionRecord) -> Result<(), CliError>,
{
    tick_inner(
        context,
        id,
        Some(expected_launch_id),
        Some(expected_binding),
        now_epoch,
        usage,
        submit,
    )
}

fn tick_inner<F>(
    context: &CliContext,
    id: &str,
    expected_launch_id: Option<&str>,
    expected_binding: Option<&crate::codex_account::BindingSnapshot>,
    now_epoch: i64,
    usage: &UsageSnapshot,
    mut submit: F,
) -> Result<TickOutcome, CliError>
where
    F: FnMut(&SessionRecord) -> Result<(), CliError>,
{
    let now = epoch_string(now_epoch)?;
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _lock = acquire_session_record_lock(context, &canonical_id)?;
    let mut record = load_session_record(context, &canonical_id)?;
    crate::ensure_same_session_identity(&observed, &record)?;
    if !runtime_matches(&record, expected_launch_id) {
        return Ok(TickOutcome::Unchanged);
    }
    if expected_binding
        .is_some_and(|expected| crate::codex_account::binding_snapshot(&record) != *expected)
    {
        return Ok(TickOutcome::Unchanged);
    }
    let mut state = read_state(context, &record.id, &now)?;
    if !supported(&record) {
        state.enabled = false;
        state.state = "terminal_failure".to_string();
        state.updated_at = now;
        state.failure_reason = Some("provider_unsupported".to_string());
        write_state(context, &record.id, &state)?;
        return Ok(TickOutcome::TerminalFailure);
    }
    if state.state == "checking" {
        let activity = crate::activity::state_for_view(context, &record);
        if activity
            .as_ref()
            .is_some_and(|activity| activity.revision > state.blocked_revision.unwrap_or_default())
        {
            state.state = "resumed".to_string();
            state.updated_at = now;
            state.failure_reason = None;
            write_state(context, &record.id, &state)?;
            return Ok(TickOutcome::Resumed);
        }
        state.enabled = false;
        state.state = "terminal_failure".to_string();
        state.updated_at = now;
        state.failure_reason = Some("submission_outcome_unknown".to_string());
        write_state(context, &record.id, &state)?;
        return Ok(TickOutcome::TerminalFailure);
    }
    if !state.enabled
        || !matches!(
            state.state.as_str(),
            "armed" | "scheduled" | "transient_failure"
        )
    {
        return Ok(TickOutcome::Unchanged);
    }
    if let Some(next) = state.next_check_at.as_deref().and_then(epoch_from_string)
        && next > now_epoch
    {
        return Ok(TickOutcome::Unchanged);
    }
    if let Some(scheduled) = state.scheduled_at.as_deref().and_then(epoch_from_string)
        && scheduled > now_epoch
    {
        return Ok(TickOutcome::Unchanged);
    }

    if !usage.authoritative {
        return record_retry(context, &record, state, now_epoch, "usage_unavailable");
    }

    if usage.has_exhausted_windows {
        let Some(latest_reset) = usage.exhausted_reset_epochs.iter().copied().max() else {
            return record_retry(
                context,
                &record,
                state,
                now_epoch,
                "exhausted_reset_unavailable",
            );
        };
        let wake_epoch = latest_reset.max(now_epoch.saturating_add(1))
            + bounded_jitter_seconds(&record.id, state.blocked_turn_id.as_deref().unwrap_or(""));
        state.state = "scheduled".to_string();
        state.updated_at = now;
        state.scheduled_at = Some(epoch_string(wake_epoch)?);
        state.next_check_at = None;
        state.failure_reason = None;
        state.ever_scheduled = true;
        write_state(context, &record.id, &state)?;
        return Ok(TickOutcome::Scheduled);
    }

    if !state.ever_scheduled {
        return record_retry(
            context,
            &record,
            state,
            now_epoch,
            "usage_window_not_exhausted",
        );
    }

    let activity = crate::activity::state_for_view(context, &record);
    let eligible = activity.as_ref().is_some_and(|activity| {
        activity.phase == crate::activity::TurnPhase::Waiting
            && activity.revision == state.blocked_revision.unwrap_or_default()
            && activity
                .current_turn
                .as_ref()
                .and_then(|turn| turn.attention.as_ref())
                .is_none()
    });
    if !eligible {
        state.enabled = false;
        state.state = "terminal_failure".to_string();
        state.updated_at = now;
        state.failure_reason = Some("session_state_changed".to_string());
        write_state(context, &record.id, &state)?;
        return Ok(TickOutcome::TerminalFailure);
    }

    // Claim before submitting. A crash after this durable write is never
    // retried automatically, which preserves the no-duplicate guarantee.
    let health_fence = crate::activity::acquire_runtime_health_fence(context, &record)?;
    if crate::activity::runtime_is_unhealthy(context, &record) {
        state.enabled = false;
        state.state = "terminal_failure".to_string();
        state.updated_at = now;
        state.failure_reason = Some("state_unavailable".to_string());
        write_state(context, &record.id, &state)?;
        return Ok(TickOutcome::TerminalFailure);
    }
    if record.agent == "codex" {
        crate::codex_account::authorize_input_locked(context, &mut record)?;
    }
    state.state = "checking".to_string();
    state.updated_at = now.clone();
    state.scheduled_at = None;
    state.next_check_at = None;
    write_state(context, &record.id, &state)?;
    drop(health_fence);
    match submit(&record) {
        Ok(()) => {
            state.state = "resumed".to_string();
            state.updated_at = now;
            state.failure_reason = None;
            write_state(context, &record.id, &state)?;
            Ok(TickOutcome::Resumed)
        }
        Err(_) => {
            state.enabled = false;
            state.state = "terminal_failure".to_string();
            state.updated_at = now;
            state.failure_reason = Some("submission_outcome_unknown".to_string());
            write_state(context, &record.id, &state)?;
            Ok(TickOutcome::TerminalFailure)
        }
    }
}

fn runtime_matches(record: &SessionRecord, expected_launch_id: Option<&str>) -> bool {
    expected_launch_id.is_none_or(|expected| {
        record
            .runtime
            .as_ref()
            .is_some_and(|runtime| runtime.launch_id == expected)
    })
}

fn record_retry(
    context: &CliContext,
    record: &SessionRecord,
    mut state: DurableAutoResume,
    now_epoch: i64,
    reason: &str,
) -> Result<TickOutcome, CliError> {
    state.attempt = state.attempt.saturating_add(1);
    state.updated_at = epoch_string(now_epoch)?;
    state.failure_reason = Some(reason.to_string());
    state.scheduled_at = None;
    if state.attempt >= MAX_TRANSIENT_ATTEMPTS {
        state.enabled = false;
        state.state = "terminal_failure".to_string();
        state.next_check_at = None;
        write_state(context, &record.id, &state)?;
        return Ok(TickOutcome::TerminalFailure);
    }
    let delay = RETRY_DELAYS_SECONDS[state.attempt.saturating_sub(1) as usize];
    state.state = "transient_failure".to_string();
    state.next_check_at = Some(epoch_string(now_epoch.saturating_add(delay))?);
    write_state(context, &record.id, &state)?;
    Ok(TickOutcome::Retrying)
}

fn bounded_jitter_seconds(id: &str, blocked_turn_id: &str) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(id.as_bytes());
    hasher.update([0]);
    hasher.update(blocked_turn_id.as_bytes());
    i64::from(hasher.finalize()[0] % 31)
}

fn epoch_string(epoch: i64) -> Result<String, CliError> {
    Timestamp::from_second(epoch)
        .map(|timestamp| timestamp.to_string())
        .map_err(|_| {
            CliError::data(
                "auto-resume-time-invalid",
                "auto-resume timestamp is outside the supported range",
                None,
            )
        })
}

fn epoch_from_string(value: &str) -> Option<i64> {
    value
        .parse::<Timestamp>()
        .ok()
        .map(|value| value.as_second())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RuntimeInfo, activity};
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn context(tmp: &tempfile::TempDir) -> CliContext {
        CliContext {
            state_dir: tmp.path().to_path_buf(),
            host: None,
        }
    }

    fn seed_session(tmp: &tempfile::TempDir) -> (CliContext, SessionRecord) {
        let context = context(tmp);
        let id = "claude-reset";
        let dir = session_dir(&context, id);
        fs::create_dir_all(&dir).unwrap();
        let record = SessionRecord {
            schema_version: crate::SESSION_DOCUMENT_VERSION.to_string(),
            id: id.to_string(),
            agent: "claude".to_string(),
            mode: "interactive".to_string(),
            title: None,
            title_state: None,
            title_revision: 0,
            cwd: "/repo".to_string(),
            tmux_session: "hs-claude-reset".to_string(),
            prompt_file: None,
            log_file: None,
            created_at: "2030-01-01T00:00:00Z".to_string(),
            updated_at: "2030-01-01T00:00:00Z".to_string(),
            provider_resume: None,
            runtime: Some(RuntimeInfo {
                kind: "tmux".to_string(),
                tmux_session: "hs-claude-reset".to_string(),
                generation: 1,
                started_at: "2030-01-01T00:00:00Z".to_string(),
                launch_id: "runtime-1".to_string(),
                extra: BTreeMap::new(),
            }),
            agent_args: Vec::new(),
            agent_bin: None,
            extra: BTreeMap::new(),
            resume_sidecar_extra: BTreeMap::new(),
        };
        crate::write_session_record(&context, &record).unwrap();
        activity::activate_runtime(&context, &record).unwrap();
        (context, record)
    }

    #[test]
    fn launch_profile_can_fail_closed_auto_resume_support() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_, mut record) = seed_session(&tmp);
        record.runtime.as_mut().unwrap().extra.insert(
            "agent_profile_auto_resume_supported".to_string(),
            json!(false),
        );

        assert!(!supported(&record));
    }

    fn waiting_revision(context: &CliContext, record: &SessionRecord) -> u64 {
        let started = json!({
            "schema_version": "agent-session.turn-event.v1",
            "event_id": "start",
            "runtime_id": "runtime-1",
            "provider": "claude",
            "kind": "turn_started",
            "confidence": "observed"
        });
        activity::ingest_event(
            context,
            &record.id,
            serde_json::from_value(started).unwrap(),
        )
        .unwrap();
        let failed = json!({
            "schema_version": "agent-session.turn-event.v1",
            "event_id": "failed",
            "runtime_id": "runtime-1",
            "provider": "claude",
            "kind": "turn_failed",
            "failure_reason": "usage_exhausted",
            "confidence": "authoritative"
        });
        activity::ingest_event(context, &record.id, serde_json::from_value(failed).unwrap())
            .unwrap()
            .turn_state
            .revision
    }

    #[test]
    fn latest_exhausted_window_controls_wake_and_duplicate_ticks_submit_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, record) = seed_session(&tmp);
        set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();
        let revision = waiting_revision(&context, &record);
        arm_usage_exhaustion(
            &context,
            &record.id,
            "turn-1".to_string(),
            revision,
            "2030-01-01T00:00:01Z",
        )
        .unwrap();

        let base = 1_893_456_000;
        let first = tick(
            &context,
            &record.id,
            base,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: true,
                exhausted_reset_epochs: vec![base + 300, base + 900],
            },
            |_| panic!("must not submit while blocked"),
        )
        .unwrap();
        assert_eq!(first, TickOutcome::Scheduled);

        let state = read_state(&context, &record.id, "ignored").unwrap();
        let wake = epoch_from_string(state.scheduled_at.as_deref().unwrap()).unwrap();
        assert!(wake >= base + 900);

        let mut submissions = 0;
        let resumed = tick(
            &context,
            &record.id,
            wake,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
            },
            |_| {
                submissions += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(resumed, TickOutcome::Resumed);
        let duplicate = tick(
            &context,
            &record.id,
            wake + 1,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
            },
            |_| {
                submissions += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(duplicate, TickOutcome::Unchanged);
        assert_eq!(submissions, 1);
    }

    #[test]
    fn authoritative_open_usage_advances_only_an_existing_scheduled_claim() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, record) = seed_session(&tmp);
        set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();
        let revision = waiting_revision(&context, &record);
        arm_usage_exhaustion(
            &context,
            &record.id,
            "turn-1".to_string(),
            revision,
            "2030-01-01T00:00:01Z",
        )
        .unwrap();
        let base = 1_893_456_000;
        tick(
            &context,
            &record.id,
            base,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: true,
                exhausted_reset_epochs: vec![base + 900],
            },
            |_| panic!("must not submit while blocked"),
        )
        .unwrap();

        assert!(wake_scheduled_if_usage_open(&context, &record.id, base + 1).unwrap());
        assert_eq!(
            read_state(&context, &record.id, "ignored")
                .unwrap()
                .scheduled_at
                .as_deref()
                .and_then(epoch_from_string),
            Some(base + 1)
        );

        cancel_for_manual_input_locked(&context, &record.id, "2030-01-01T00:00:02Z").unwrap();
        assert!(!wake_scheduled_if_usage_open(&context, &record.id, base + 2).unwrap());
        let cancelled = read_state(&context, &record.id, "ignored").unwrap();
        assert_eq!(cancelled.state, "cancelled");
        assert!(!cancelled.enabled);
    }

    #[test]
    fn stale_runtime_cannot_wake_or_submit_for_a_same_id_replacement() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, record) = seed_session(&tmp);
        set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();
        let revision = waiting_revision(&context, &record);
        arm_usage_exhaustion(
            &context,
            &record.id,
            "turn-1".to_string(),
            revision,
            "2030-01-01T00:00:01Z",
        )
        .unwrap();
        let base = 1_893_456_000;
        assert_eq!(
            tick(
                &context,
                &record.id,
                base,
                &UsageSnapshot {
                    authoritative: true,
                    has_exhausted_windows: true,
                    exhausted_reset_epochs: vec![base + 900],
                },
                |_| panic!("must not submit while exhausted"),
            )
            .unwrap(),
            TickOutcome::Scheduled
        );
        let scheduled_before = read_state(&context, &record.id, "ignored")
            .unwrap()
            .scheduled_at;
        let mut replacement = record.clone();
        replacement.runtime.as_mut().unwrap().launch_id = "runtime-2".to_string();
        crate::write_session_record(&context, &replacement).unwrap();

        assert!(
            !wake_scheduled_if_usage_open_for_runtime(&context, &record.id, "runtime-1", base + 1,)
                .unwrap()
        );
        let mut submissions = 0;
        assert_eq!(
            tick_for_runtime(
                &context,
                &record.id,
                "runtime-1",
                base + 901,
                &UsageSnapshot {
                    authoritative: true,
                    has_exhausted_windows: false,
                    exhausted_reset_epochs: Vec::new(),
                },
                |_| {
                    submissions += 1;
                    Ok(())
                },
            )
            .unwrap(),
            TickOutcome::Unchanged
        );
        assert_eq!(submissions, 0);
        assert_eq!(
            read_state(&context, &record.id, "ignored")
                .unwrap()
                .scheduled_at,
            scheduled_before
        );
    }

    #[test]
    fn public_v1_view_serializes_the_documented_state_and_reason_allowlists() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_, record) = seed_session(&tmp);
        let states = [
            "disabled",
            "enabled",
            "armed",
            "scheduled",
            "checking",
            "resumed",
            "cancelled",
            "transient_failure",
            "terminal_failure",
        ];
        let reasons = [
            "state_unavailable",
            "manual_input",
            "usage_unavailable",
            "usage_window_not_exhausted",
            "exhausted_reset_unavailable",
            "session_state_changed",
            "submission_outcome_unknown",
            "provider_unsupported",
            "scheduler_error",
        ];
        for state in states {
            let mut durable = default_state("2030-01-01T00:00:00Z");
            durable.state = state.to_string();
            durable.enabled = state != "disabled";
            durable.scheduled_at =
                (state == "scheduled").then(|| "2030-01-01T00:05:00Z".to_string());
            let value = serde_json::to_value(view(&record, durable)).unwrap();
            assert_eq!(value["schema_version"], AUTO_RESUME_SCHEMA_VERSION);
            assert_eq!(value["state"], state);
            assert_eq!(value.get("scheduled_at").is_some(), state == "scheduled");
        }
        for reason in reasons {
            let mut durable = default_state("2030-01-01T00:00:00Z");
            durable.state = "terminal_failure".to_string();
            durable.failure_reason = Some(reason.to_string());
            let value = serde_json::to_value(view(&record, durable)).unwrap();
            assert_eq!(value["failure_reason"], reason);
        }
    }

    #[test]
    fn cancellation_wins_before_wake_and_restart_discovers_pending_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, record) = seed_session(&tmp);
        set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();
        arm_usage_exhaustion(
            &context,
            &record.id,
            "turn-1".to_string(),
            2,
            "2030-01-01T00:00:01Z",
        )
        .unwrap();
        assert_eq!(
            pending_sessions(&context, 1_893_456_000).unwrap().usage_ids,
            vec![record.id.clone()]
        );
        cancel(&context, &record.id, "2030-01-01T00:00:02Z").unwrap();
        assert_eq!(
            pending_sessions(&context, 1_893_456_000).unwrap(),
            PendingSessions::default()
        );
        let outcome = tick(
            &context,
            &record.id,
            1_893_456_000,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
            },
            |_| panic!("cancelled state must not submit"),
        )
        .unwrap();
        assert_eq!(outcome, TickOutcome::Unchanged);
    }

    #[test]
    fn malformed_state_is_isolated_from_healthy_pending_sessions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, record) = seed_session(&tmp);
        set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();
        arm_usage_exhaustion(
            &context,
            &record.id,
            "turn-1".to_string(),
            1,
            "2030-01-01T00:00:01Z",
        )
        .unwrap();

        let mut corrupt = record.clone();
        corrupt.id = "claude-corrupt".to_string();
        corrupt.tmux_session = "hs-claude-corrupt".to_string();
        corrupt.created_at = "2030-01-01T00:00:02Z".to_string();
        corrupt.runtime.as_mut().unwrap().tmux_session = corrupt.tmux_session.clone();
        corrupt.runtime.as_mut().unwrap().launch_id = "runtime-corrupt".to_string();
        fs::create_dir_all(session_dir(&context, &corrupt.id)).unwrap();
        crate::write_session_record(&context, &corrupt).unwrap();
        fs::write(path(&context, &corrupt.id), b"not-json").unwrap();

        let pending = pending_sessions(&context, 1_893_456_000).unwrap();
        assert_eq!(pending.usage_ids, vec![record.id]);
        assert_eq!(pending.error_codes, vec!["auto-resume-state-invalid"]);
    }

    #[test]
    fn future_schedule_is_not_due_and_non_authoritative_usage_never_submits() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, record) = seed_session(&tmp);
        set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();
        let revision = waiting_revision(&context, &record);
        arm_usage_exhaustion(
            &context,
            &record.id,
            "turn-1".to_string(),
            revision,
            "2030-01-01T00:00:01Z",
        )
        .unwrap();
        let base = 1_893_456_000;
        tick(
            &context,
            &record.id,
            base,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: true,
                exhausted_reset_epochs: vec![base + 300],
            },
            |_| panic!("must not submit while blocked"),
        )
        .unwrap();
        let wake = read_state(&context, &record.id, "ignored")
            .unwrap()
            .scheduled_at
            .as_deref()
            .and_then(epoch_from_string)
            .unwrap();
        assert!(
            pending_sessions(&context, wake - 1)
                .unwrap()
                .usage_ids
                .is_empty()
        );
        assert_eq!(
            pending_sessions(&context, wake).unwrap().usage_ids,
            vec![record.id.clone()]
        );

        let outcome = tick(
            &context,
            &record.id,
            wake,
            &UsageSnapshot {
                authoritative: false,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
            },
            |_| panic!("stale or policy-blocked usage must never submit"),
        )
        .unwrap();
        assert_eq!(outcome, TickOutcome::Retrying);
        assert_eq!(
            read_state(&context, &record.id, "ignored")
                .unwrap()
                .failure_reason
                .as_deref(),
            Some("usage_unavailable")
        );
    }

    #[test]
    fn exhausted_window_without_reset_never_authorizes_submission() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, record) = seed_session(&tmp);
        set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();
        let revision = waiting_revision(&context, &record);
        arm_usage_exhaustion(
            &context,
            &record.id,
            "turn-1".to_string(),
            revision,
            "2030-01-01T00:00:01Z",
        )
        .unwrap();
        let base = 1_893_456_000;
        tick(
            &context,
            &record.id,
            base,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: true,
                exhausted_reset_epochs: vec![base + 60],
            },
            |_| panic!("must not submit while blocked"),
        )
        .unwrap();
        let wake = read_state(&context, &record.id, "ignored")
            .unwrap()
            .scheduled_at
            .as_deref()
            .and_then(epoch_from_string)
            .unwrap();
        let outcome = tick(
            &context,
            &record.id,
            wake,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: true,
                exhausted_reset_epochs: Vec::new(),
            },
            |_| panic!("an exhausted window without reset must never submit"),
        )
        .unwrap();
        assert_eq!(outcome, TickOutcome::Retrying);
        assert_eq!(
            read_state(&context, &record.id, "ignored")
                .unwrap()
                .failure_reason
                .as_deref(),
            Some("exhausted_reset_unavailable")
        );
    }

    #[test]
    fn restart_never_replays_an_unconfirmed_submission_claim() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, record) = seed_session(&tmp);
        set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();
        let revision = waiting_revision(&context, &record);
        arm_usage_exhaustion(
            &context,
            &record.id,
            "turn-1".to_string(),
            revision,
            "2030-01-01T00:00:01Z",
        )
        .unwrap();
        let mut state = read_state(&context, &record.id, "ignored").unwrap();
        state.state = "checking".to_string();
        state.ever_scheduled = true;
        write_state(&context, &record.id, &state).unwrap();
        assert_eq!(
            pending_sessions(&context, 1_893_456_000)
                .unwrap()
                .recovery_ids,
            vec![record.id.clone()]
        );

        let outcome = tick(
            &context,
            &record.id,
            1_893_456_000,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
            },
            |_| panic!("an unconfirmed durable claim must never be replayed"),
        )
        .unwrap();
        assert_eq!(outcome, TickOutcome::TerminalFailure);
        let state = read_state(&context, &record.id, "ignored").unwrap();
        assert_eq!(
            state.failure_reason.as_deref(),
            Some("submission_outcome_unknown")
        );
    }

    #[test]
    fn submission_error_after_claim_is_terminal_and_never_replayed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, record) = seed_session(&tmp);
        set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();
        let revision = waiting_revision(&context, &record);
        arm_usage_exhaustion(
            &context,
            &record.id,
            "turn-1".to_string(),
            revision,
            "2030-01-01T00:00:01Z",
        )
        .unwrap();
        let base = 1_893_456_000;
        tick(
            &context,
            &record.id,
            base,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: true,
                exhausted_reset_epochs: vec![base + 60],
            },
            |_| panic!("must not submit while blocked"),
        )
        .unwrap();
        let wake = read_state(&context, &record.id, "ignored")
            .unwrap()
            .scheduled_at
            .as_deref()
            .and_then(epoch_from_string)
            .unwrap();

        let mut submissions = 0;
        let failed = tick(
            &context,
            &record.id,
            wake,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
            },
            |_| {
                submissions += 1;
                Err(CliError::runtime("injected-partial-send", "injected", None))
            },
        )
        .unwrap();
        assert_eq!(failed, TickOutcome::TerminalFailure);
        let replay = tick(
            &context,
            &record.id,
            wake + 600,
            &UsageSnapshot {
                authoritative: true,
                has_exhausted_windows: false,
                exhausted_reset_epochs: Vec::new(),
            },
            |_| {
                submissions += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(replay, TickOutcome::Unchanged);
        assert_eq!(submissions, 1);
        let state = read_state(&context, &record.id, "ignored").unwrap();
        assert_eq!(state.state, "terminal_failure");
        assert_eq!(
            state.failure_reason.as_deref(),
            Some("submission_outcome_unknown")
        );
    }

    #[test]
    fn app_server_backed_codex_can_enable_auto_resume() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, mut record) = seed_session(&tmp);
        record.agent = "codex".to_string();
        let runtime = record.runtime.as_mut().unwrap();
        runtime.kind = "codex_app_server".to_string();
        runtime
            .extra
            .insert("codex_app_server_protocol".to_string(), json!("v2"));
        runtime.extra.insert(
            "codex_app_server_socket".to_string(),
            json!("/run/user/1000/agent-session/codex-test.sock"),
        );
        runtime.extra.insert(
            "codex_app_server_proxy".to_string(),
            json!("/run/user/1000/agent-session/codex-test.proxy"),
        );
        runtime.extra.insert(
            "codex_app_server_thread_handoff".to_string(),
            json!("/run/user/1000/agent-session/codex-test.thread"),
        );
        runtime.extra.insert(
            "codex_app_server_thread_attached".to_string(),
            json!("/run/user/1000/agent-session/codex-test.attached"),
        );
        crate::write_session_record(&context, &record).unwrap();

        let view = set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z")
            .expect("a capability-probed app-server Codex runtime should be supported");
        assert!(view.supported);
        assert!(view.enabled);
        assert_eq!(view.state, "enabled");
    }

    #[test]
    fn profiled_app_server_codex_requires_explicit_auto_resume_support() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_, mut record) = seed_session(&tmp);
        record.agent = "codex".to_string();
        let runtime = record.runtime.as_mut().unwrap();
        runtime.kind = "codex_app_server".to_string();
        runtime
            .extra
            .insert("agent_profile".to_string(), json!("codex-custom"));
        runtime.extra.insert(
            "agent_profile_auto_resume_supported".to_string(),
            json!(false),
        );
        runtime
            .extra
            .insert("codex_app_server_protocol".to_string(), json!("v2"));
        for (key, suffix) in [
            ("codex_app_server_socket", "sock"),
            ("codex_app_server_proxy", "proxy"),
            ("codex_app_server_thread_handoff", "thread"),
            ("codex_app_server_thread_attached", "attached"),
        ] {
            runtime.extra.insert(
                key.to_string(),
                json!(format!(
                    "/run/user/1000/agent-session/codex-profile.{suffix}"
                )),
            );
        }

        assert!(!supported(&record));
        record.runtime.as_mut().unwrap().extra.insert(
            "agent_profile_auto_resume_supported".to_string(),
            json!(true),
        );
        assert!(supported(&record));
    }

    #[test]
    fn projection_loss_disables_enabled_runtime_and_blocks_reenable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (context, mut record) = seed_session(&tmp);
        record.agent = "codex".to_string();
        let runtime = record.runtime.as_mut().unwrap();
        runtime.kind = "codex_app_server".to_string();
        runtime
            .extra
            .insert("codex_app_server_protocol".to_string(), json!("v2"));
        for (key, suffix) in [
            ("codex_app_server_socket", "sock"),
            ("codex_app_server_proxy", "proxy"),
            ("codex_app_server_thread_handoff", "thread"),
            ("codex_app_server_thread_attached", "attached"),
        ] {
            runtime.extra.insert(
                key.to_string(),
                json!(format!("/run/user/1000/agent-session/codex-test.{suffix}")),
            );
        }
        crate::write_session_record(&context, &record).unwrap();
        set_enabled(&context, &record.id, true, "2030-01-01T00:00:00Z").unwrap();

        fail_closed_projection_for_runtime(
            &context,
            &record.id,
            "runtime-1",
            "2030-01-01T00:00:01Z",
        )
        .unwrap();
        let view = view_for_record(&context, &record);
        assert!(!view.enabled);
        assert_eq!(view.state, "terminal_failure");
        assert_eq!(view.failure_reason.as_deref(), Some("state_unavailable"));
        let error = set_enabled(&context, &record.id, true, "2030-01-01T00:00:02Z").unwrap_err();
        assert_eq!(error.code(), "auto-resume-state-unavailable");
        let disabled = set_enabled(&context, &record.id, false, "2030-01-01T00:00:03Z").unwrap();
        assert!(!disabled.enabled);
        let error = set_enabled(&context, &record.id, true, "2030-01-01T00:00:04Z").unwrap_err();
        assert_eq!(error.code(), "auto-resume-state-unavailable");
        let cancelled = cancel(&context, &record.id, "2030-01-01T00:00:05Z").unwrap();
        assert!(!cancelled.enabled);
        let error = set_enabled(&context, &record.id, true, "2030-01-01T00:00:06Z").unwrap_err();
        assert_eq!(error.code(), "auto-resume-state-unavailable");
    }
}

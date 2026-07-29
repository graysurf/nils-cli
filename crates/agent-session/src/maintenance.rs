use super::*;
use sha2::{Digest, Sha256};

pub(crate) const SCHEMA_VERSION: &str = "agent-session.session-maintenance.v1";
const MAINTENANCE_LOCK_TIMEOUT: Duration = Duration::from_millis(500);
const MAINTENANCE_TMUX_STATUS_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MaintenanceOperation {
    Resume,
    Delete,
    Attach,
    Inspect,
}

impl MaintenanceOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Resume => "resume",
            Self::Delete => "delete",
            Self::Attach => "attach",
            Self::Inspect => "inspect",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MaintenanceActionId {
    RetryResume,
    RetryDelete,
    RetryAttach,
    Inspect,
    TerminateRuntimeThenResume,
    TerminateRuntimeThenDelete,
}

impl MaintenanceActionId {
    fn as_str(self) -> &'static str {
        match self {
            Self::RetryResume => "retry_resume",
            Self::RetryDelete => "retry_delete",
            Self::RetryAttach => "retry_attach",
            Self::Inspect => "inspect",
            Self::TerminateRuntimeThenResume => "terminate_runtime_then_resume",
            Self::TerminateRuntimeThenDelete => "terminate_runtime_then_delete",
        }
    }

    fn destructive(self) -> bool {
        matches!(
            self,
            Self::RetryDelete | Self::TerminateRuntimeThenResume | Self::TerminateRuntimeThenDelete
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MaintenanceIssue {
    kind: &'static str,
    operation: &'static str,
    retryable: bool,
    preserves_session_metadata: bool,
    message: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MaintenanceBoundary {
    kind: &'static str,
    safe_process_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MaintenancePreservation {
    session_metadata_retained_until_success: bool,
    provider_conversation_retained: bool,
    destructive_scope: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MaintenanceActionView {
    id: MaintenanceActionId,
    label: &'static str,
    destructive: bool,
    requires_confirmation: bool,
    preserves_session_metadata: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionMaintenancePreview {
    schema_version: &'static str,
    session_id: String,
    operation: MaintenanceOperation,
    state: &'static str,
    pub(crate) session_incarnation: Option<String>,
    pub(crate) session_generation: Option<u64>,
    issue: Option<MaintenanceIssue>,
    boundary: MaintenanceBoundary,
    preservation: MaintenancePreservation,
    actions: Vec<MaintenanceActionView>,
    pub(crate) preview_digest: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaintenanceActionRequest {
    pub(crate) operation: MaintenanceOperation,
    pub(crate) action: MaintenanceActionId,
    pub(crate) expected_session_incarnation: Option<String>,
    pub(crate) expected_session_generation: Option<u64>,
    pub(crate) expected_preview_digest: String,
    #[serde(default)]
    pub(crate) confirmed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MaintenanceActionResult {
    schema_version: &'static str,
    session_id: String,
    operation: MaintenanceOperation,
    action: MaintenanceActionId,
    outcome: &'static str,
    session_incarnation: Option<String>,
    session_generation: Option<u64>,
    status: &'static str,
    cleanup_pending: bool,
    #[serde(skip)]
    deleted_registry_fence: Option<SessionRegistryFence>,
}

impl MaintenanceActionResult {
    pub(crate) fn deleted_registry_fence(&self) -> Option<&SessionRegistryFence> {
        self.deleted_registry_fence.as_ref()
    }
}

struct PreviewAssessment {
    state: &'static str,
    runtime_status: &'static str,
    issue_kind: Option<&'static str>,
    issue_message: Option<&'static str>,
    retryable: bool,
    boundary_kind: &'static str,
    safe_process_count: usize,
    repairable_identity: Option<TmuxRuntimeIdentity>,
    digest_identities: Vec<TmuxRuntimeIdentity>,
}

struct PreparedPreview {
    view: SessionMaintenancePreview,
    assessment: PreviewAssessment,
}

pub(crate) fn preview(
    context: &CliContext,
    id: &str,
    tmux_bin: &Path,
    operation: MaintenanceOperation,
) -> Result<SessionMaintenancePreview, CliError> {
    preview_inner(context, id, tmux_bin, operation)
        .map_err(|error| sanitize_maintenance_error(id, operation, error))
}

fn preview_inner(
    context: &CliContext,
    id: &str,
    tmux_bin: &Path,
    operation: MaintenanceOperation,
) -> Result<SessionMaintenancePreview, CliError> {
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _record_lock = acquire_maintenance_lock(context, &canonical_id, operation)?;
    let record = load_session_record(context, &canonical_id)?;
    ensure_same_session_identity(&observed, &record)?;
    preview_locked(context, &record, tmux_bin, operation).map(|prepared| prepared.view)
}

fn preview_locked(
    context: &CliContext,
    record: &SessionRecord,
    tmux_bin: &Path,
    operation: MaintenanceOperation,
) -> Result<PreparedPreview, CliError> {
    let runtime_stop_fenced = operation == MaintenanceOperation::Delete
        && crate::orchestration::session_runtime_stop_fenced(context, record)?;
    let assessment = if runtime_stop_fenced {
        blocked_assessment(
            "runtime_stop_fenced",
            "assignment-bound worker delete is required for this runtime-stopped session",
            "stopped",
            "none",
            0,
            Vec::new(),
        )
    } else {
        assess(record, tmux_bin, operation)?
    };
    let actions = if runtime_stop_fenced {
        Vec::new()
    } else {
        actions_for(operation, assessment.state == "repairable")
    };
    let session_incarnation = record
        .runtime
        .as_ref()
        .map(|runtime| runtime.launch_id.clone())
        .filter(|launch_id| !launch_id.is_empty());
    let session_generation = record.runtime.as_ref().map(|runtime| runtime.generation);
    let issue = assessment.issue_kind.map(|kind| MaintenanceIssue {
        kind,
        operation: operation.as_str(),
        retryable: assessment.retryable,
        preserves_session_metadata: true,
        message: assessment
            .issue_message
            .unwrap_or("maintenance is required"),
    });
    let preview_digest = preview_digest(
        record,
        operation,
        assessment.state,
        assessment.issue_kind,
        assessment.boundary_kind,
        assessment.safe_process_count,
        &assessment.digest_identities,
    )?;
    let view = SessionMaintenancePreview {
        schema_version: SCHEMA_VERSION,
        session_id: record.id.clone(),
        operation,
        state: assessment.state,
        session_incarnation,
        session_generation,
        issue,
        boundary: MaintenanceBoundary {
            kind: assessment.boundary_kind,
            safe_process_count: assessment.safe_process_count,
        },
        preservation: MaintenancePreservation {
            session_metadata_retained_until_success: true,
            provider_conversation_retained: true,
            destructive_scope: "verified_recorded_runtime_only",
        },
        actions,
        preview_digest,
    };
    Ok(PreparedPreview { view, assessment })
}

fn assess(
    record: &SessionRecord,
    tmux_bin: &Path,
    operation: MaintenanceOperation,
) -> Result<PreviewAssessment, CliError> {
    match live_status_with_timeout(
        tmux_bin,
        &record.tmux_session,
        MAINTENANCE_TMUX_STATUS_TIMEOUT,
    )
    .as_str()
    {
        "running" => {
            return Ok(PreviewAssessment {
                state: "healthy",
                runtime_status: "running",
                issue_kind: None,
                issue_message: None,
                retryable: true,
                boundary_kind: "tmux_session",
                safe_process_count: 0,
                repairable_identity: None,
                digest_identities: Vec::new(),
            });
        }
        "unknown" => {
            return Ok(blocked_assessment(
                "unknown",
                "session status could not be verified",
                "unknown",
                "unknown",
                0,
                Vec::new(),
            ));
        }
        _ => {}
    }

    let identity = persisted_tmux_runtime_identity(record)
        .map_err(|reason| maintenance_failure_error(record, reason, operation))?;
    let prior_identities = persisted_prior_tmux_runtime_identities(record)
        .map_err(|reason| maintenance_failure_error(record, reason, operation))?;
    if identity.is_none() && runtime_is_proven_never_launched(record) && prior_identities.is_empty()
    {
        if startup_projection(record).is_some_and(|startup| startup.state == "failed") {
            return Ok(startup_failed_assessment(Vec::new()));
        }
        return Ok(PreviewAssessment {
            state: "healthy",
            runtime_status: "stopped",
            issue_kind: None,
            issue_message: None,
            retryable: true,
            boundary_kind: "none",
            safe_process_count: 0,
            repairable_identity: None,
            digest_identities: Vec::new(),
        });
    }
    let Some(identity) = identity else {
        return Ok(blocked_assessment(
            "runtime_identity_unavailable",
            "the recorded runtime identity is unavailable",
            "stopped",
            "unknown",
            0,
            prior_identities,
        ));
    };
    if prior_identities
        .iter()
        .any(|prior| !prior.same_runtime_target(&identity))
    {
        let mut identities = vec![identity];
        identities.extend(prior_identities);
        return Ok(blocked_assessment(
            "runtime_identity_changed",
            "the recorded runtime identity changed",
            "stopped",
            "unknown",
            0,
            identities,
        ));
    }

    let mut identities = vec![identity.clone()];
    identities.extend(prior_identities.iter().cloned());
    let statuses = identities
        .iter()
        .map(process_runtime_status)
        .collect::<Vec<_>>();
    let mut running = identities
        .iter()
        .zip(&statuses)
        .filter(|(_, status)| **status == ProcessGroupStatus::Running)
        .map(|(identity, _)| identity.clone())
        .collect::<Vec<_>>();
    if statuses.contains(&ProcessGroupStatus::Unknown) {
        return Ok(blocked_assessment(
            "runtime_identity_unavailable",
            "the recorded runtime boundary could not be verified",
            "stopped",
            "unknown",
            0,
            identities,
        ));
    }
    if running.is_empty() {
        if startup_projection(record).is_some_and(|startup| startup.state == "failed") {
            return Ok(startup_failed_assessment(identities));
        }
        return Ok(PreviewAssessment {
            state: "healthy",
            runtime_status: "stopped",
            issue_kind: None,
            issue_message: None,
            retryable: true,
            boundary_kind: "none",
            safe_process_count: 0,
            repairable_identity: None,
            digest_identities: identities,
        });
    }
    if running.len() != 1 || running[0] != identity {
        return Ok(blocked_assessment(
            "runtime_identity_changed",
            "more than one recorded runtime boundary remains live",
            "stopped",
            "unknown",
            running.len(),
            identities,
        ));
    }

    let live_identity = running.pop().expect("one live identity");
    let (boundary_kind, safe_process_count, repairable_identity) =
        assess_repairable_boundary(&live_identity);
    if let Some(repairable) = repairable_identity.as_ref() {
        identities[0] = repairable.clone();
    }
    let repairable = repairable_identity.is_some()
        && matches!(
            operation,
            MaintenanceOperation::Resume | MaintenanceOperation::Delete
        );
    Ok(PreviewAssessment {
        state: if repairable { "repairable" } else { "blocked" },
        runtime_status: "stopped",
        issue_kind: Some("process_boundary_live"),
        issue_message: Some("the recorded runtime process boundary is still live"),
        retryable: true,
        boundary_kind,
        safe_process_count,
        repairable_identity,
        digest_identities: identities,
    })
}

fn startup_failed_assessment(digest_identities: Vec<TmuxRuntimeIdentity>) -> PreviewAssessment {
    PreviewAssessment {
        state: "blocked",
        runtime_status: "stopped",
        issue_kind: Some("startup_failed"),
        issue_message: Some("the last runtime startup failed"),
        retryable: true,
        boundary_kind: "none",
        safe_process_count: 0,
        repairable_identity: None,
        digest_identities,
    }
}

fn assess_repairable_boundary(
    _identity: &TmuxRuntimeIdentity,
) -> (&'static str, usize, Option<TmuxRuntimeIdentity>) {
    #[cfg(target_os = "linux")]
    if _identity.control_group.is_some()
        && let Ok(pinned_runtime) = prepare_process_runtime(_identity)
    {
        let current_members = process_runtime_identities(&pinned_runtime);
        let count = current_members.len().max(1);
        let mut repairable = _identity.clone();
        repairable.control_group_members = current_members;
        release_process_runtime(Some(pinned_runtime));
        return ("managed_scope", count, Some(repairable));
    }
    ("process_group", 1, None)
}

fn blocked_assessment(
    issue_kind: &'static str,
    issue_message: &'static str,
    runtime_status: &'static str,
    boundary_kind: &'static str,
    safe_process_count: usize,
    digest_identities: Vec<TmuxRuntimeIdentity>,
) -> PreviewAssessment {
    PreviewAssessment {
        state: "blocked",
        runtime_status,
        issue_kind: Some(issue_kind),
        issue_message: Some(issue_message),
        retryable: matches!(
            issue_kind,
            "process_boundary_live" | "session_still_running"
        ),
        boundary_kind,
        safe_process_count,
        repairable_identity: None,
        digest_identities,
    }
}

fn actions_for(operation: MaintenanceOperation, repairable: bool) -> Vec<MaintenanceActionView> {
    let retry = match operation {
        MaintenanceOperation::Resume => MaintenanceActionId::RetryResume,
        MaintenanceOperation::Delete => MaintenanceActionId::RetryDelete,
        MaintenanceOperation::Attach => MaintenanceActionId::RetryAttach,
        MaintenanceOperation::Inspect => MaintenanceActionId::Inspect,
    };
    let mut actions = vec![action_view(retry)];
    if repairable {
        actions.push(action_view(match operation {
            MaintenanceOperation::Resume => MaintenanceActionId::TerminateRuntimeThenResume,
            MaintenanceOperation::Delete => MaintenanceActionId::TerminateRuntimeThenDelete,
            MaintenanceOperation::Attach | MaintenanceOperation::Inspect => unreachable!(),
        }));
    }
    actions
}

fn action_view(id: MaintenanceActionId) -> MaintenanceActionView {
    let (label, preserves_session_metadata) = match id {
        MaintenanceActionId::RetryResume => ("Retry resume", true),
        MaintenanceActionId::RetryDelete => ("Retry delete", false),
        MaintenanceActionId::RetryAttach => ("Retry attach", true),
        MaintenanceActionId::Inspect => ("Inspect", true),
        MaintenanceActionId::TerminateRuntimeThenResume => {
            ("Terminate recorded runtime, then resume", true)
        }
        MaintenanceActionId::TerminateRuntimeThenDelete => {
            ("Terminate recorded runtime, then delete", false)
        }
    };
    MaintenanceActionView {
        id,
        label,
        destructive: id.destructive(),
        requires_confirmation: id.destructive(),
        preserves_session_metadata,
    }
}

fn preview_digest(
    record: &SessionRecord,
    operation: MaintenanceOperation,
    state: &str,
    issue_kind: Option<&str>,
    boundary_kind: &str,
    safe_process_count: usize,
    identities: &[TmuxRuntimeIdentity],
) -> Result<String, CliError> {
    let digest_subject = json!({
        "domain": SCHEMA_VERSION,
        "session_id": record.id,
        "session_created_at": record.created_at,
        "session_incarnation": record.runtime.as_ref().map(|runtime| runtime.launch_id.as_str()),
        "session_generation": record.runtime.as_ref().map(|runtime| runtime.generation),
        "operation": operation.as_str(),
        "state": state,
        "issue_kind": issue_kind,
        "boundary_kind": boundary_kind,
        "safe_process_count": safe_process_count,
        "identities": identities,
    });
    let encoded = serde_json::to_vec(&digest_subject).map_err(|_| {
        CliError::runtime(
            "maintenance-preview-failed",
            "maintenance preview could not be encoded",
            Some(json!({ "id": record.id })),
        )
    })?;
    let mut digest = Sha256::new();
    digest.update(encoded);
    Ok(format!("sha256:{}", hex_digest(digest.finalize())))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

pub(crate) fn execute_with_resume_guard(
    context: &CliContext,
    id: &str,
    tmux_bin: &Path,
    request: MaintenanceActionRequest,
    resume_guard: impl Fn(&SessionRecord) -> Result<(), CliError>,
) -> Result<MaintenanceActionResult, CliError> {
    let operation = request.operation;
    execute_inner(context, id, tmux_bin, request, &resume_guard)
        .map_err(|error| sanitize_maintenance_error(id, operation, error))
}

fn execute_inner(
    context: &CliContext,
    id: &str,
    tmux_bin: &Path,
    request: MaintenanceActionRequest,
    resume_guard: &impl Fn(&SessionRecord) -> Result<(), CliError>,
) -> Result<MaintenanceActionResult, CliError> {
    let requested_operation = request.operation;
    validate_action_pair(requested_operation, request.action)?;
    validate_digest(&request.expected_preview_digest)?;
    if request.action.destructive() && !request.confirmed {
        return Err(CliError::data(
            "maintenance-confirmation-required",
            "destructive maintenance action requires explicit confirmation",
            Some(json!({
                "operation": requested_operation.as_str(),
                "action": request.action.as_str(),
            })),
        ));
    }
    let observed = load_session_record(context, id)?;
    let canonical_id = observed.id.clone();
    let _record_lock = acquire_maintenance_lock(context, &canonical_id, requested_operation)?;
    let mut record = load_session_record(context, &canonical_id)?;
    ensure_same_session_identity(&observed, &record)?;
    let prepared = preview_locked(context, &record, tmux_bin, request.operation)?;
    let prepared_runtime_status = prepared.assessment.runtime_status;
    let preview = &prepared.view;
    if preview.session_incarnation != request.expected_session_incarnation
        || preview.session_generation != request.expected_session_generation
        || preview.preview_digest != request.expected_preview_digest
    {
        return Err(stale_preview_error(&record));
    }

    if matches!(
        request.action,
        MaintenanceActionId::RetryResume | MaintenanceActionId::TerminateRuntimeThenResume
    ) {
        crate::orchestration::ensure_session_not_quarantined(context, &record)?;
        resume_guard(&record)?;
    }

    if matches!(
        request.action,
        MaintenanceActionId::RetryDelete | MaintenanceActionId::TerminateRuntimeThenDelete
    ) {
        crate::orchestration::ensure_terminal_assignment_may_delete_runtime_stopped_session(
            context, &record, None,
        )?;
    }

    if matches!(
        request.action,
        MaintenanceActionId::TerminateRuntimeThenResume
            | MaintenanceActionId::TerminateRuntimeThenDelete
    ) {
        if preview.state != "repairable" {
            return Err(stale_preview_error(&record));
        }
        let expected_identity = prepared
            .assessment
            .repairable_identity
            .ok_or_else(|| stale_preview_error(&record))?;
        terminate_orphaned_runtime_locked(
            context,
            &mut record,
            tmux_bin,
            &expected_identity,
            request.operation,
        )?;
    }

    match request.action {
        MaintenanceActionId::RetryResume | MaintenanceActionId::TerminateRuntimeThenResume => {
            let resumed = resume_session_locked(context, record, tmux_bin)?;
            Ok(MaintenanceActionResult {
                schema_version: SCHEMA_VERSION,
                session_id: resumed.session.id,
                operation: request.operation,
                action: request.action,
                outcome: "resumed",
                session_incarnation: resumed.session_incarnation,
                session_generation: resumed.session_generation,
                status: "running",
                cleanup_pending: false,
                deleted_registry_fence: None,
            })
        }
        MaintenanceActionId::RetryDelete | MaintenanceActionId::TerminateRuntimeThenDelete => {
            let resolved = resolve_session_record_path(context, &canonical_id)?;
            validate_record_id(&record, &resolved.expected_id, &resolved.record_path)?;
            let deleted = delete_session_locked_with_timeouts(
                context,
                record,
                resolved.session_dir,
                tmux_bin,
                PANE_INPUT_COMMAND_TIMEOUT,
                DELETE_TERMINATION_VERIFY_TIMEOUT,
            )?;
            let deleted_registry_fence = deleted.registry_fence.clone();
            Ok(MaintenanceActionResult {
                schema_version: SCHEMA_VERSION,
                session_id: deleted.id,
                operation: request.operation,
                action: request.action,
                outcome: "deleted",
                session_incarnation: None,
                session_generation: None,
                status: "deleted",
                cleanup_pending: deleted.cleanup_pending,
                deleted_registry_fence: Some(deleted_registry_fence),
            })
        }
        MaintenanceActionId::RetryAttach | MaintenanceActionId::Inspect => {
            Ok(MaintenanceActionResult {
                schema_version: SCHEMA_VERSION,
                session_id: record.id,
                operation: request.operation,
                action: request.action,
                outcome: "inspected",
                session_incarnation: record
                    .runtime
                    .as_ref()
                    .map(|runtime| runtime.launch_id.clone())
                    .filter(|value| !value.is_empty()),
                session_generation: record.runtime.as_ref().map(|runtime| runtime.generation),
                status: prepared_runtime_status,
                cleanup_pending: false,
                deleted_registry_fence: None,
            })
        }
    }
}

fn validate_action_pair(
    operation: MaintenanceOperation,
    action: MaintenanceActionId,
) -> Result<(), CliError> {
    let valid = matches!(
        (operation, action),
        (
            MaintenanceOperation::Resume,
            MaintenanceActionId::RetryResume
        ) | (
            MaintenanceOperation::Resume,
            MaintenanceActionId::TerminateRuntimeThenResume
        ) | (
            MaintenanceOperation::Delete,
            MaintenanceActionId::RetryDelete
        ) | (
            MaintenanceOperation::Delete,
            MaintenanceActionId::TerminateRuntimeThenDelete
        ) | (
            MaintenanceOperation::Attach,
            MaintenanceActionId::RetryAttach
        ) | (MaintenanceOperation::Inspect, MaintenanceActionId::Inspect)
    );
    if valid {
        Ok(())
    } else {
        Err(CliError::usage(
            "invalid-maintenance-action",
            "maintenance action does not match the requested operation",
            Some(json!({
                "operation": operation.as_str(),
                "action": action.as_str(),
            })),
        ))
    }
}

fn validate_digest(value: &str) -> Result<(), CliError> {
    let valid = value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if valid {
        Ok(())
    } else {
        Err(CliError::usage(
            "invalid-maintenance-preview-digest",
            "expected_preview_digest must be sha256 followed by 64 lowercase hexadecimal characters",
            Some(json!({ "field": "expected_preview_digest" })),
        ))
    }
}

fn stale_preview_error(record: &SessionRecord) -> CliError {
    CliError::runtime(
        "maintenance-preview-stale",
        "session maintenance state changed; inspect it again before retrying",
        Some(json!({ "id": record.id })),
    )
}

fn acquire_maintenance_lock(
    context: &CliContext,
    id: &str,
    operation: MaintenanceOperation,
) -> Result<SessionRecordLock, CliError> {
    acquire_session_record_lock_timed(context, id, MAINTENANCE_LOCK_TIMEOUT).map_err(|error| {
        if error.code() == "session-record-lock-timeout" {
            CliError::runtime(
                "maintenance-session-busy",
                "session maintenance is busy; retry after the current operation finishes",
                Some(json!({
                    "id": id,
                    "operation": operation.as_str(),
                    "retryable": true,
                })),
            )
        } else {
            error
        }
    })
}

fn maintenance_failure_error(
    record: &SessionRecord,
    reason: SessionTerminationFailure,
    operation: MaintenanceOperation,
) -> CliError {
    let kind = match reason {
        SessionTerminationFailure::StillRunning => "session_still_running",
        SessionTerminationFailure::ProcessStillRunning => "process_boundary_live",
        SessionTerminationFailure::RuntimeIdentityChanged
        | SessionTerminationFailure::RuntimeIdentityMismatch => "runtime_identity_changed",
        SessionTerminationFailure::RuntimeIdentityUnavailable => "runtime_identity_unavailable",
        SessionTerminationFailure::KillFailed
        | SessionTerminationFailure::KillTimeout
        | SessionTerminationFailure::KillError
        | SessionTerminationFailure::VerificationFailed => "unknown",
    };
    safe_maintenance_failure(&record.id, operation, kind, reason.retryable())
}

fn safe_maintenance_failure(
    id: &str,
    operation: MaintenanceOperation,
    kind: &'static str,
    retryable: bool,
) -> CliError {
    CliError::runtime(
        "session-maintenance-failed",
        "session maintenance could not safely complete; session metadata was retained",
        Some(json!({
            "id": id,
            "operation": operation.as_str(),
            "kind": kind,
            "retryable": retryable,
            "session_metadata_retained": true,
        })),
    )
}

fn sanitize_maintenance_error(
    id: &str,
    operation: MaintenanceOperation,
    error: CliError,
) -> CliError {
    let code = error.code().to_string();
    if matches!(
        code.as_str(),
        "maintenance-preview-stale"
            | "maintenance-session-busy"
            | "invalid-maintenance-action"
            | "invalid-maintenance-preview-digest"
            | "maintenance-confirmation-required"
            | "session-maintenance-failed"
            | "agent-profile-unavailable"
            | "worker-quarantined"
    ) {
        return error;
    }
    if code == "session-not-found" {
        return CliError::data(
            "session-not-found",
            "session was not found",
            Some(json!({ "id": id })),
        );
    }
    if code == "session-termination-failed" {
        let reason = error
            .0
            .details
            .as_ref()
            .and_then(|details| details.get("reason"))
            .and_then(Value::as_str);
        let (kind, retryable) = match reason {
            Some("session-still-running") => ("session_still_running", true),
            Some("process-still-running") => ("process_boundary_live", true),
            Some("runtime-identity-changed" | "runtime-identity-mismatch") => {
                ("runtime_identity_changed", false)
            }
            Some("runtime-identity-unavailable") => ("runtime_identity_unavailable", false),
            _ => ("unknown", false),
        };
        return safe_maintenance_failure(id, operation, kind, retryable);
    }
    safe_maintenance_failure(id, operation, "unknown", false)
}

fn terminate_orphaned_runtime_locked(
    context: &CliContext,
    record: &mut SessionRecord,
    tmux_bin: &Path,
    expected_identity: &TmuxRuntimeIdentity,
    operation: MaintenanceOperation,
) -> Result<(), CliError> {
    recover_interrupted_tmux_termination_locked(context, record)
        .map_err(|reason| maintenance_failure_error(record, reason, operation))?;
    match capture_tmux_runtime_identity(context, record, tmux_bin, DELETE_TERMINATION_PROBE_TIMEOUT)
        .map_err(|reason| maintenance_failure_error(record, reason, operation))?
    {
        TmuxRuntimeProbe::Running(_) => return Err(stale_preview_error(record)),
        TmuxRuntimeProbe::Stopped => {}
    }
    let mut identity = persisted_tmux_runtime_identity(record)
        .map_err(|reason| maintenance_failure_error(record, reason, operation))?
        .filter(|identity| {
            identity.same_runtime_target(expected_identity)
                && identity.same_process_identity(expected_identity)
        })
        .ok_or_else(|| stale_preview_error(record))?;
    identity.merge_process_evidence_from(expected_identity);
    let prior_identities = persisted_prior_tmux_runtime_identities(record)
        .map_err(|reason| maintenance_failure_error(record, reason, operation))?;
    verify_stopped_process_runtimes(&prior_identities, DELETE_TERMINATION_VERIFY_TIMEOUT)
        .map_err(|reason| maintenance_failure_error(record, reason, operation))?;
    if process_runtime_status(&identity) != ProcessGroupStatus::Running {
        return Err(stale_preview_error(record));
    }
    match terminate_verified_process_runtime_transaction(
        context,
        record,
        &mut identity,
        VerifiedRuntimeTerminationMode::AlreadyStopped { tmux_bin },
        DELETE_TERMINATION_VERIFY_TIMEOUT,
    )
    .map_err(|reason| maintenance_failure_error(record, reason, operation))?
    {
        VerifiedRuntimeTerminationOutcome::Complete => Ok(()),
        VerifiedRuntimeTerminationOutcome::TmuxIdentityChanged => Err(stale_preview_error(record)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn repairable_preview_fixture_matches_the_producer_projection() {
        let projected = SessionMaintenancePreview {
            schema_version: SCHEMA_VERSION,
            session_id: "fixture-codex".to_string(),
            operation: MaintenanceOperation::Resume,
            state: "repairable",
            session_incarnation: Some("fixture-runtime-incarnation".to_string()),
            session_generation: Some(1),
            issue: Some(MaintenanceIssue {
                kind: "process_boundary_live",
                operation: "resume",
                retryable: true,
                preserves_session_metadata: true,
                message: "the recorded runtime process boundary is still live",
            }),
            boundary: MaintenanceBoundary {
                kind: "managed_scope",
                safe_process_count: 2,
            },
            preservation: MaintenancePreservation {
                session_metadata_retained_until_success: true,
                provider_conversation_retained: true,
                destructive_scope: "verified_recorded_runtime_only",
            },
            actions: vec![
                action_view(MaintenanceActionId::RetryResume),
                action_view(MaintenanceActionId::TerminateRuntimeThenResume),
            ],
            preview_digest:
                "sha256:8d5dece08afdf22b39fcbba1d1bc33dd6d967e81a4728a715be06995d9db7f24"
                    .to_string(),
        };
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/maintenance/session-maintenance-v1-repairable.json"
        ))
        .expect("fixture must contain valid JSON");

        assert_eq!(serde_json::to_value(projected).unwrap(), fixture);
    }
}

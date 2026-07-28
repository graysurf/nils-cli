use super::*;

pub(super) fn normalize_provider_hook(
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
        attention_correlation_exact: exact_clarification
            || exact_elicitation
            || (exact_hermes_approval && !attention_correlation_ambiguous),
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

pub(super) fn normalize_provider_notification(
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
        attention_correlation_exact: false,
        confidence: Confidence::Authoritative,
        source_kind: SourceKind::ProviderHook,
        provider_time: None,
    }))
}

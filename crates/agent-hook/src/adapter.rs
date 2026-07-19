use std::io::Read;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::contract::{digest, supported_event};
use crate::error::HookError;
use crate::model::{
    DecisionAction, NormalizedDecision, NormalizedRequest, Product, REQUEST_VERSION,
};

pub const MAX_PROVIDER_BYTES: usize = 1024 * 1024;
const MAX_PROVIDER_ID_CHARS: usize = 256;

#[derive(Debug, Serialize)]
struct ActivityEvent {
    schema_version: &'static str,
    event_id: String,
    runtime_id: String,
    provider: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_turn_id: Option<String>,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attention_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attention_kind: Option<&'static str>,
    confidence: &'static str,
    source_kind: &'static str,
}

pub fn read_stdin() -> Result<Vec<u8>, HookError> {
    let mut input = Vec::new();
    std::io::stdin()
        .take((MAX_PROVIDER_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .map_err(|_| {
            HookError::runtime("provider-input-read-failed", "failed to read hook input")
        })?;
    if input.len() > MAX_PROVIDER_BYTES {
        return Err(HookError::data(
            "provider-input-too-large",
            "provider hook input exceeds 1 MiB",
        ));
    }
    Ok(input)
}

pub fn normalize(
    product: Product,
    event_arg: Option<&str>,
    input: &[u8],
) -> Result<NormalizedRequest, HookError> {
    if input.is_empty() {
        return Err(HookError::data(
            "provider-input-empty",
            "provider hook input must be JSON",
        ));
    }
    let raw: Value = serde_json::from_slice(input).map_err(|_| {
        HookError::data(
            "provider-input-invalid",
            "provider hook input is not valid bounded JSON",
        )
    })?;
    let object = raw.as_object().ok_or_else(|| {
        HookError::data(
            "provider-input-invalid",
            "provider hook input root must be an object",
        )
    })?;
    let payload_event = string_at(object, &["hook_event_name", "event"]);
    let event = match (event_arg, payload_event) {
        (Some(argument), Some(payload)) if argument != payload => {
            return Err(HookError::data(
                "provider-event-mismatch",
                "--event does not match the provider payload event",
            ));
        }
        (Some(argument), _) => argument.to_string(),
        (None, Some(payload)) => payload.to_string(),
        (None, None) => {
            return Err(HookError::data(
                "provider-event-missing",
                "provider event is missing",
            ));
        }
    };
    if !supported_event(product, &event) {
        return Err(HookError::data(
            "provider-event-unsupported",
            "provider event is not supported by agent-hook v1",
        ));
    }

    let matcher_keys: &[&str] = if event == "SessionStart" {
        &["matcher", "source"]
    } else {
        &["matcher", "tool_name", "notification_type"]
    };
    let matcher = string_at(object, matcher_keys)
        .map(str::to_string)
        .filter(|value| value.len() <= 128);
    let target_path = target_path(object);
    let target_material = target_path
        .as_ref()
        .map(|path| path.as_os_str().as_encoded_bytes())
        .unwrap_or(b"target-unavailable");
    let command_material = command_text(object)
        .unwrap_or("command-unavailable")
        .as_bytes();
    let snapshot_digest = digest(input);
    let request_id = format!("request:{}", &snapshot_digest[7..39]);
    Ok(NormalizedRequest {
        schema_version: REQUEST_VERSION.to_string(),
        request_id,
        product,
        event,
        matcher,
        target_digest: digest(target_material),
        command_digest: digest(command_material),
        snapshot_digest,
        worktree_fingerprint: None,
        // Provider payload fields are untrusted and deliberately ignored. The
        // dispatcher replaces this with a #676 registry-derived projection.
        semantic_conflict: None,
        target_path,
    })
}

pub fn render_provider(decision: &NormalizedDecision) -> Result<String, HookError> {
    if let Some(output) = decision.provider_output.as_ref() {
        return serde_json::to_string(output).map_err(|_| {
            HookError::runtime(
                "provider-output-render-failed",
                "provider output could not be rendered",
            )
        });
    }
    let reason = decision
        .reasons
        .iter()
        .map(|reason| reason.code.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let output = match decision.action {
        DecisionAction::Allow => json!({}),
        DecisionAction::Block => json!({
            "continue": false,
            "stopReason": format!("agent-hook:{reason}"),
        }),
        DecisionAction::Transform => json!({
            "hookSpecificOutput": {
                "hookEventName": decision.event,
                "updatedInput": decision.replacement,
            }
        }),
        DecisionAction::Context | DecisionAction::Warn => json!({
            "hookSpecificOutput": {
                "hookEventName": decision.event,
                "additionalContext": decision.context.as_deref().unwrap_or(&reason),
            }
        }),
    };
    serde_json::to_string(&output).map_err(|_| {
        HookError::runtime(
            "provider-output-render-failed",
            "provider output could not be rendered",
        )
    })
}

pub fn normalize_activity_event(
    request: &NormalizedRequest,
    input: &[u8],
    runtime_id: &str,
) -> Result<Option<Vec<u8>>, HookError> {
    validate_provider_id("runtime_id", runtime_id)?;
    let raw: Value = serde_json::from_slice(input).map_err(|_| {
        HookError::data(
            "provider-input-invalid",
            "provider hook input is not valid bounded JSON",
        )
    })?;
    let object = raw.as_object().ok_or_else(|| {
        HookError::data(
            "provider-input-invalid",
            "provider hook input root must be an object",
        )
    })?;
    let notification = object.get("notification_type").and_then(Value::as_str);
    let tool_name = object.get("tool_name").and_then(Value::as_str);
    if request.product == Product::Claude
        && request.event == "PermissionRequest"
        && tool_name == Some("AskUserQuestion")
    {
        return Ok(None);
    }
    let exact_clarification = request.product == Product::Claude
        && tool_name == Some("AskUserQuestion")
        && matches!(
            request.event.as_str(),
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure"
        );
    let exact_elicitation = request.product == Product::Claude
        && matches!(request.event.as_str(), "Elicitation" | "ElicitationResult");
    let elicitation_id = if exact_elicitation {
        optional_provider_id(object, "elicitation_id")?
    } else {
        None
    };
    if request.event == "ElicitationResult" && elicitation_id.is_none() {
        return Ok(None);
    }

    let (kind, attention_kind, confidence) = match (
        request.product,
        request.event.as_str(),
        notification,
        exact_clarification,
    ) {
        (Product::Codex | Product::Claude, "UserPromptSubmit", _, _) => {
            ("turn_started", None, "observed")
        }
        (Product::Codex, "PermissionRequest", _, _) => {
            ("attention_requested", Some("approval"), "observed")
        }
        (Product::Codex, "PreToolUse" | "PostToolUse" | "PostToolUseFailure", _, _) => {
            ("progress", None, "observed")
        }
        (Product::Codex, "Stop", _, _) => ("stop_observed", None, "observed"),
        (Product::Codex, "StopFailure", _, _) => ("turn_failed", None, "authoritative"),
        (Product::Claude, "PreToolUse", _, true) => {
            ("attention_requested", Some("clarification"), "observed")
        }
        (Product::Claude, "PostToolUse" | "PostToolUseFailure", _, true) => {
            ("attention_cleared", None, "observed")
        }
        (Product::Claude, "PreToolUse" | "PostToolUse" | "PostToolUseFailure", _, false) => {
            ("progress", None, "observed")
        }
        (Product::Claude, "Elicitation", _, _) => (
            "attention_requested",
            Some(
                if object.get("mode").and_then(Value::as_str) == Some("url") {
                    "authentication"
                } else {
                    "clarification"
                },
            ),
            "observed",
        ),
        (Product::Claude, "ElicitationResult", _, _) => ("attention_cleared", None, "observed"),
        (Product::Claude, "PermissionRequest", _, _) => {
            ("attention_requested", Some("approval"), "observed")
        }
        (Product::Claude, "Notification", Some("permission_prompt"), _) => {
            ("attention_requested", Some("approval"), "observed")
        }
        (Product::Claude, "Notification", Some("agent_needs_input"), _) => {
            ("attention_requested", Some("other"), "observed")
        }
        (Product::Claude, "Notification", Some("idle_prompt"), _) => {
            ("turn_completed", None, "observed")
        }
        (Product::Claude, "Stop", _, _) => ("stop_observed", None, "observed"),
        (Product::Claude, "StopFailure", _, _) => ("turn_failed", None, "authoritative"),
        _ => return Ok(None),
    };

    let provider_session_id = optional_provider_id(object, "session_id")?
        .or(optional_provider_id(object, "session_key")?)
        .map(|value| projected_provider_id(runtime_id, request.product, "session", value));
    let provider_turn_id = optional_provider_id(object, "turn_id")?
        .map(|value| projected_provider_id(runtime_id, request.product, "turn", value));
    let exact_attention = if exact_clarification {
        optional_provider_id(object, "tool_use_id")?
    } else if exact_elicitation {
        elicitation_id
    } else {
        None
    };
    if exact_clarification && exact_attention.is_none() {
        return Err(HookError::data(
            "provider-hook-correlation-missing",
            "AskUserQuestion lifecycle event is missing tool_use_id",
        ));
    }
    let attention_id = match kind {
        "attention_requested" | "attention_cleared" if exact_attention.is_some() => exact_attention
            .map(|value| projected_provider_id(runtime_id, request.product, "attention", value)),
        "attention_requested" => Some(format!(
            "local:v1:{}",
            request
                .snapshot_digest
                .strip_prefix("sha256:")
                .unwrap_or_default()
        )),
        _ => None,
    };
    let failure_reason = (kind == "turn_failed").then(|| {
        if request.product == Product::Claude {
            normalized_failure_reason(object.get("error").and_then(Value::as_str))
        } else {
            "unknown"
        }
    });
    let event = ActivityEvent {
        schema_version: "agent-session.turn-event.v1",
        event_id: format!(
            "agent-hook:v1:{}",
            request
                .snapshot_digest
                .strip_prefix("sha256:")
                .unwrap_or_default()
        ),
        runtime_id: runtime_id.to_string(),
        provider: request.product.as_str(),
        provider_session_id,
        provider_turn_id,
        kind,
        failure_reason,
        attention_id,
        attention_kind,
        confidence,
        source_kind: "provider_hook",
    };
    serde_json::to_vec(&event).map(Some).map_err(|_| {
        HookError::runtime(
            "session-activity-render-failed",
            "metadata-only activity event could not be rendered",
        )
    })
}

fn string_at<'a>(object: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
}

fn target_path(object: &Map<String, Value>) -> Option<PathBuf> {
    string_at(object, &["cwd", "working_directory"])
        .or_else(|| nested_string(object, "tool_input", &["path", "file_path", "cwd"]))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn command_text(object: &Map<String, Value>) -> Option<&str> {
    string_at(object, &["command"])
        .or_else(|| nested_string(object, "tool_input", &["command", "cmd"]))
}

fn optional_provider_id<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a str>, HookError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) => {
            validate_provider_id(field, value)?;
            Ok(Some(value))
        }
        Some(_) => Err(HookError::data(
            "provider-hook-identifier-invalid",
            "provider hook identifier must be a bounded string",
        )),
    }
}

fn validate_provider_id(field: &str, value: &str) -> Result<(), HookError> {
    if value.is_empty()
        || value.chars().count() > MAX_PROVIDER_ID_CHARS
        || value.chars().any(char::is_control)
    {
        return Err(HookError::data(
            "provider-hook-identifier-invalid",
            format!("provider hook identifier {field} is invalid"),
        ));
    }
    Ok(())
}

fn projected_provider_id(runtime_id: &str, product: Product, field: &str, value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"agent-session.provider-identifier.v1\0");
    digest.update(runtime_id.as_bytes());
    digest.update(b"\0");
    digest.update(product.as_str().as_bytes());
    digest.update(b"\0");
    digest.update(field.as_bytes());
    digest.update(b"\0");
    digest.update(value.as_bytes());
    format!(
        "local:v1:{}",
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn normalized_failure_reason(raw: Option<&str>) -> &'static str {
    match raw {
        Some("rate_limit") => "usage_exhausted",
        Some("authentication_failed") => "authentication",
        Some("oauth_org_not_allowed") => "organization",
        Some("billing_error") => "billing",
        Some("invalid_request") => "invalid_request",
        Some("server_error") => "service",
        Some("max_output_tokens") => "max_output_tokens",
        _ => "unknown",
    }
}

fn nested_string<'a>(
    object: &'a Map<String, Value>,
    parent: &str,
    keys: &[&str],
) -> Option<&'a str> {
    let nested = object.get(parent)?.as_object()?;
    string_at(nested, keys)
}

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::contract::{digest, matcher_input_field, supported_event};
use crate::error::HookError;
use crate::model::{
    DecisionAction, DshSubject, NormalizedDecision, NormalizedRequest, Product, REQUEST_VERSION,
};
use crate::path_binding::{TargetBinding, resolve_target_bindings};
use crate::strict_json;

pub const MAX_PROVIDER_BYTES: usize = 1024 * 1024;
const DSH_INGRESS_V1: &str = "agent-hook.dsh-ingress.v1";
const DSH_INGRESS_V2: &str = "agent-hook.dsh-ingress.v2";
const DSH_INGRESS_V3: &str = "agent-hook.dsh-ingress.v3";
const DSH_INGRESS_V4: &str = "agent-hook.dsh-ingress.v4";
const MAX_PROVIDER_ID_CHARS: usize = 256;
const MAX_MUTATION_TARGETS: usize = 256;
const MAX_MUTATION_TARGET_BYTES: usize = 4096;
const MAX_DSH_PROMPT_BYTES: usize = 64 * 1024;

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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DshIngress {
    schema_version: String,
    event: String,
    #[serde(default)]
    call_id: Option<String>,
    cwd: PathBuf,
    #[serde(default)]
    subject: Option<DshIngressSubject>,
    #[serde(default)]
    tool: Option<DshToolCall>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    result: Option<DshToolResult>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DshIngressSubject {
    session_id: String,
    turn: u64,
    #[serde(default)]
    step: Option<u64>,
    #[serde(default)]
    session_start_source: Option<String>,
    agent_docs_state_home: PathBuf,
    #[serde(default)]
    agent_docs_home: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DshToolCall {
    name: String,
    arguments: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DshToolResult {
    is_error: bool,
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
    let raw = parse_provider_json(input)?;
    if product == Product::Dsh {
        return normalize_dsh(event_arg, input, raw);
    }
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

    let matcher = matcher_input_field(product, &event)
        .and_then(|field| object.get(field))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| value.len() <= 128);
    finalize_normalized_request(product, event, matcher, input, object, stop_reentry(object))
}

fn normalize_dsh(
    event_arg: Option<&str>,
    input: &[u8],
    raw: Value,
) -> Result<NormalizedRequest, HookError> {
    let ingress: DshIngress = serde_json::from_value(raw).map_err(|_| {
        HookError::data(
            "dsh-ingress-invalid",
            "DSH ingress must match a strict supported agent-hook.dsh-ingress object",
        )
    })?;
    if !matches!(
        ingress.schema_version.as_str(),
        DSH_INGRESS_V1 | DSH_INGRESS_V2 | DSH_INGRESS_V3 | DSH_INGRESS_V4
    ) {
        return Err(HookError::data(
            "dsh-ingress-version-invalid",
            "DSH ingress schema_version is unsupported",
        ));
    }
    let dsh_subject = match (ingress.schema_version.as_str(), ingress.subject) {
        (DSH_INGRESS_V1, None) => None,
        (DSH_INGRESS_V2 | DSH_INGRESS_V4, Some(subject))
            if !subject.session_id.is_empty()
                && subject.session_id.chars().count() <= MAX_PROVIDER_ID_CHARS
                && subject.turn > 0
                && subject.step.is_some_and(|step| step > 0)
                && subject.session_start_source.is_none()
                && subject.agent_docs_state_home.is_absolute()
                && subject
                    .agent_docs_home
                    .as_ref()
                    .is_none_or(|path| path.is_absolute()) =>
        {
            Some(DshSubject {
                session_id: subject.session_id,
                call_id: ingress.call_id.clone(),
                turn: subject.turn,
                step: subject.step,
                session_start_source: None,
                agent_docs_state_home: subject.agent_docs_state_home,
                agent_docs_home: subject.agent_docs_home,
            })
        }
        (DSH_INGRESS_V3, Some(subject))
            if !subject.session_id.is_empty()
                && subject.session_id.chars().count() <= MAX_PROVIDER_ID_CHARS
                && subject.turn > 0
                && subject.step.is_none_or(|step| step > 0)
                && subject
                    .session_start_source
                    .as_deref()
                    .is_none_or(|source| {
                        matches!(
                            source,
                            "startup" | "resume" | "clear" | "compact" | "observed"
                        )
                    })
                && subject.agent_docs_state_home.is_absolute()
                && subject
                    .agent_docs_home
                    .as_ref()
                    .is_none_or(|path| path.is_absolute()) =>
        {
            Some(DshSubject {
                session_id: subject.session_id,
                call_id: None,
                turn: subject.turn,
                step: subject.step,
                session_start_source: subject.session_start_source,
                agent_docs_state_home: subject.agent_docs_state_home,
                agent_docs_home: subject.agent_docs_home,
            })
        }
        _ => {
            return Err(HookError::data(
                "dsh-ingress-invalid",
                "DSH ingress subject does not match its schema version",
            ));
        }
    };
    if let Some(argument) = event_arg
        && argument != ingress.event
    {
        return Err(HookError::data(
            "provider-event-mismatch",
            "--event does not match the DSH ingress event",
        ));
    }
    let event = match ingress.event.as_str() {
        "tools/pre-execute" if ingress.schema_version != DSH_INGRESS_V4 => "PreToolUse",
        "tools/post-execute" if ingress.schema_version == DSH_INGRESS_V4 => {
            if ingress
                .result
                .as_ref()
                .is_some_and(|result| result.is_error)
            {
                "PostToolUseFailure"
            } else {
                "PostToolUse"
            }
        }
        "agent/pre-step" if ingress.schema_version == DSH_INGRESS_V3 => "UserPromptSubmit",
        "agent/turn-stopping" if ingress.schema_version == DSH_INGRESS_V3 => "Stop",
        _ => {
            return Err(HookError::data(
                "provider-event-unsupported",
                "DSH ingress event is not supported by agent-hook v1",
            ));
        }
    };
    if !ingress.cwd.is_absolute() {
        return Err(HookError::data(
            "dsh-ingress-invalid",
            "DSH ingress requires an absolute cwd",
        ));
    }

    let tool_shape = event == "PreToolUse"
        && ingress.call_id.as_deref().is_some_and(|call_id| {
            !call_id.is_empty() && call_id.chars().count() <= MAX_PROVIDER_ID_CHARS
        })
        && ingress.tool.as_ref().is_some_and(|tool| {
            !tool.name.is_empty()
                && tool.name.chars().count() <= MAX_PROVIDER_ID_CHARS
                && tool.arguments.is_object()
        })
        && ingress.prompt.is_none()
        && ingress.result.is_none()
        && dsh_subject
            .as_ref()
            .is_none_or(|subject| subject.step.is_some() && subject.session_start_source.is_none());
    let prompt_shape = event == "UserPromptSubmit"
        && ingress.schema_version == DSH_INGRESS_V3
        && ingress.call_id.is_none()
        && ingress.tool.is_none()
        && ingress
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.len() <= MAX_DSH_PROMPT_BYTES)
        && dsh_subject
            .as_ref()
            .is_some_and(|subject| subject.step.is_some())
        && ingress.result.is_none();
    let stop_shape = event == "Stop"
        && ingress.schema_version == DSH_INGRESS_V3
        && ingress.call_id.is_none()
        && ingress.tool.is_none()
        && ingress.prompt.is_none()
        && ingress.result.is_none()
        && dsh_subject.as_ref().is_some_and(|subject| {
            subject.step.is_none() && subject.session_start_source.is_none()
        });
    let post_tool_shape = matches!(event, "PostToolUse" | "PostToolUseFailure")
        && ingress.schema_version == DSH_INGRESS_V4
        && ingress.call_id.as_deref().is_some_and(|call_id| {
            !call_id.is_empty() && call_id.chars().count() <= MAX_PROVIDER_ID_CHARS
        })
        && ingress.tool.as_ref().is_some_and(|tool| {
            !tool.name.is_empty()
                && tool.name.chars().count() <= MAX_PROVIDER_ID_CHARS
                && tool.arguments.is_object()
        })
        && ingress.prompt.is_none()
        && ingress.result.is_some()
        && dsh_subject.as_ref().is_some_and(|subject| {
            subject.step.is_some() && subject.session_start_source.is_none()
        });
    if !(tool_shape || post_tool_shape || prompt_shape || stop_shape) {
        return Err(HookError::data(
            "dsh-ingress-invalid",
            "DSH ingress fields do not match the selected lifecycle event",
        ));
    }

    let mut canonical = Map::new();
    canonical.insert(
        "cwd".to_string(),
        Value::String(ingress.cwd.to_string_lossy().into_owned()),
    );
    let matcher = if let Some(tool) = ingress.tool {
        canonical.insert("tool_name".to_string(), Value::String(tool.name.clone()));
        canonical.insert("tool_input".to_string(), tool.arguments);
        Some(tool.name)
    } else {
        if let Some(prompt) = ingress.prompt {
            canonical.insert("prompt".to_string(), Value::String(prompt));
        }
        None
    };
    let mut request = finalize_normalized_request(
        Product::Dsh,
        event.to_string(),
        matcher,
        input,
        &canonical,
        None,
    )?;
    request.dsh_subject = dsh_subject;
    Ok(request)
}

fn finalize_normalized_request(
    product: Product,
    event: String,
    matcher: Option<String>,
    input: &[u8],
    object: &Map<String, Value>,
    stop_reentry: Option<bool>,
) -> Result<NormalizedRequest, HookError> {
    let (target_paths, execution_path) = target_paths(product, object, matcher.as_deref())?;
    let target_count = target_paths.len();
    let mut binding_paths = target_paths;
    if let Some(execution_path) = execution_path.as_ref() {
        binding_paths.push(execution_path.clone());
    }
    let mut resolved_bindings = resolve_target_bindings(&binding_paths)?;
    let execution_binding = execution_path
        .as_ref()
        .and_then(|_| resolved_bindings.pop());
    debug_assert_eq!(resolved_bindings.len(), target_count);
    let mut target_bindings = resolved_bindings;
    deduplicate_target_bindings(&mut target_bindings);
    let target_material = target_set_binding_material(&target_bindings)?;
    let mut binding_roots = target_bindings
        .iter()
        .map(|binding| binding.binding_root.clone())
        .collect::<Vec<_>>();
    if let Some(binding) = execution_binding {
        binding_roots.push(binding.binding_root);
    }
    deduplicate_paths(&mut binding_roots);
    let target_paths = target_bindings
        .into_iter()
        .map(|binding| binding.effective_path)
        .collect();
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
        target_digest: digest(&target_material),
        command_digest: digest(command_material),
        snapshot_digest,
        worktree_fingerprint: None,
        semantic_conflict: None,
        // Provider payload fields are untrusted and deliberately ignored. The
        // dispatcher replaces semantic conflict state with a registry-derived
        // projection. Only the separately validated Stop re-entry fact remains.
        stop_reentry,
        target_paths,
        execution_path,
        binding_roots,
        dsh_subject: None,
    })
}

pub fn render_provider(decision: &NormalizedDecision) -> Result<String, HookError> {
    if let Some(output) = provider_output_with_aggregate_context(decision) {
        return serde_json::to_string(&output).map_err(|_| {
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
        DecisionAction::Block if matches!(decision.product, Product::Codex | Product::Claude) => {
            provider_denial(decision.product, &decision.event, &reason)
        }
        DecisionAction::Block => json!({
            "continue": false,
            "stopReason": format!("agent-hook:{reason}"),
        }),
        DecisionAction::Transform => provider_transform(
            decision.product,
            &decision.event,
            decision.replacement.as_ref(),
        ),
        DecisionAction::Warn if decision.product == Product::Codex && decision.event == "Stop" => {
            json!({})
        }
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

fn provider_output_with_aggregate_context(decision: &NormalizedDecision) -> Option<Value> {
    let mut output = decision.provider_output.clone()?;
    if !matches!(
        decision.action,
        DecisionAction::Context | DecisionAction::Warn
    ) {
        return Some(output);
    }
    let Some(context) = decision.context.as_deref() else {
        return Some(output);
    };

    let Some(root) = output.as_object_mut() else {
        return Some(json!({
            "hookSpecificOutput": {
                "hookEventName": decision.event,
                "additionalContext": context,
            }
        }));
    };
    let has_top_level_context = root.contains_key("additionalContext");
    if let Some(hook_output) = root
        .get_mut("hookSpecificOutput")
        .and_then(Value::as_object_mut)
    {
        hook_output.insert("additionalContext".to_string(), json!(context));
        hook_output
            .entry("hookEventName".to_string())
            .or_insert_with(|| json!(decision.event));
        if has_top_level_context {
            root.insert("additionalContext".to_string(), json!(context));
        }
    } else if has_top_level_context {
        root.insert("additionalContext".to_string(), json!(context));
    } else {
        return Some(json!({
            "hookSpecificOutput": {
                "hookEventName": decision.event,
                "additionalContext": context,
            }
        }));
    }
    Some(output)
}

pub fn render_provider_error(
    product: Product,
    event: &str,
    reason: &str,
) -> Result<String, HookError> {
    serde_json::to_string(&provider_denial(product, event, reason)).map_err(|_| {
        HookError::runtime(
            "provider-output-render-failed",
            "provider error output could not be rendered",
        )
    })
}

fn provider_denial(product: Product, event: &str, reason: &str) -> Value {
    match (product, event) {
        (Product::Codex | Product::Claude, "PreToolUse") => json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "permissionDecision": "deny",
                "permissionDecisionReason": format!("agent-hook:{reason}"),
            }
        }),
        (Product::Codex | Product::Claude, "PermissionRequest") => json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "decision": {
                    "behavior": "deny",
                    "message": format!("agent-hook:{reason}"),
                }
            }
        }),
        (Product::Claude, "Elicitation" | "ElicitationResult") => json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "action": "decline",
            }
        }),
        (
            Product::Codex | Product::Claude,
            "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" | "SubagentStop" | "Stop",
        )
        | (Product::Claude, "PreCompact") => json!({
            "decision": "block",
            "reason": format!("agent-hook:{reason}"),
        }),
        _ => json!({
            "continue": false,
            "stopReason": format!("agent-hook:{reason}"),
        }),
    }
}

fn provider_transform(product: Product, event: &str, replacement: Option<&Value>) -> Value {
    match (product, event) {
        (Product::Codex | Product::Claude, "PreToolUse") => json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "permissionDecision": "allow",
                "updatedInput": replacement,
            }
        }),
        (Product::Codex | Product::Claude, "PermissionRequest") => json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "decision": {
                    "behavior": "allow",
                    "updatedInput": replacement,
                }
            }
        }),
        (Product::Claude, "PostToolUse") => json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "updatedToolOutput": replacement,
            }
        }),
        _ => json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "updatedInput": replacement,
            }
        }),
    }
}

pub fn normalize_activity_event(
    request: &NormalizedRequest,
    input: &[u8],
    runtime_id: &str,
) -> Result<Option<Vec<u8>>, HookError> {
    validate_provider_id("runtime_id", runtime_id)?;
    if request.product == Product::Dsh {
        let subject = request.dsh_subject.as_ref().ok_or_else(|| {
            HookError::data(
                "provider-hook-correlation-missing",
                "DSH activity event is missing its normalized subject",
            )
        })?;
        let kind = match request.event.as_str() {
            "UserPromptSubmit" => "turn_started",
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => "progress",
            "Stop" => "stop_observed",
            _ => return Ok(None),
        };
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
            provider: "dsh",
            provider_session_id: Some(subject.session_id.clone()),
            provider_turn_id: Some(subject.turn.to_string()),
            kind,
            failure_reason: None,
            attention_id: None,
            attention_kind: None,
            confidence: "observed",
            source_kind: "provider_hook",
        };
        return serde_json::to_vec(&event).map(Some).map_err(|_| {
            HookError::runtime(
                "session-activity-render-failed",
                "metadata-only DSH activity event could not be rendered",
            )
        });
    }
    let raw = parse_provider_json(input)?;
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
    let mut turn_id = optional_provider_id(object, "turn_id")?;
    if turn_id.is_none() && request.product == Product::Claude {
        // Claude names its per-turn identifier `prompt_id`, not `turn_id`. It is
        // stable across the turn's own events — the `Stop` and the idle
        // `Notification` that closes the same turn carry the same value — which
        // is exactly what turn correlation needs. Without it every Claude event
        // is uncorrelated and completion has to fall back to loose matching.
        turn_id = optional_provider_id(object, "prompt_id")?;
    }
    let provider_turn_id =
        turn_id.map(|value| projected_provider_id(runtime_id, request.product, "turn", value));
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

/// Project the provider's own Stop re-entry marker into a public boolean fact.
///
/// This is the only Stop field that is trusted from the payload, and it is
/// trusted in exactly one direction: `true` can end a turn that is already
/// looping, and it can never grant authority or downgrade a proven owner. A
/// non-boolean or absent value stays `None` so the ordinary posture applies.
fn stop_reentry(object: &Map<String, Value>) -> Option<bool> {
    object
        .get("stop_hook_active")
        .or_else(|| object.get("stop_hook_reentry"))
        .and_then(Value::as_bool)
}

fn string_at<'a>(object: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
}

fn target_paths(
    product: Product,
    object: &Map<String, Value>,
    matcher: Option<&str>,
) -> Result<(Vec<PathBuf>, Option<PathBuf>), HookError> {
    let mut execution = string_at(object, &["cwd", "working_directory"])
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let nested = object.get("tool_input").and_then(Value::as_object);
    let mut targets = if product == Product::Dsh {
        let Some(input) = nested else {
            return if matcher.is_none() {
                Ok((Vec::new(), execution))
            } else {
                Err(untrusted_target("DSH tool is missing object arguments"))
            };
        };
        match matcher {
            Some("bash") => {
                if let Some(value) = input.get("workdir") {
                    let raw = required_string(Some(value))?;
                    execution = Some(resolve_mutation_target(raw, execution.as_deref())?);
                }
                execution.iter().cloned().collect()
            }
            Some("write" | "edit") => {
                let path = required_string(input.get("file_path"))?;
                vec![resolve_mutation_target(path, execution.as_deref())?]
            }
            Some("str_replace_editor") => match input.get("command").and_then(Value::as_str) {
                Some("create" | "str_replace" | "insert") => {
                    let path = required_string(input.get("path"))?;
                    vec![resolve_mutation_target(path, execution.as_deref())?]
                }
                Some("view") => execution.iter().cloned().collect(),
                _ => {
                    return Err(untrusted_target(
                        "DSH str_replace_editor command is unsupported",
                    ));
                }
            },
            _ => execution.iter().cloned().collect(),
        }
    } else {
        match matcher {
            Some("Write" | "Edit" | "MultiEdit") => {
                let input = mutation_input(nested)?;
                let path = exactly_one_path(input, &["path", "file_path"])?;
                vec![resolve_mutation_target(path, execution.as_deref())?]
            }
            Some("NotebookEdit") => {
                let input = mutation_input(nested)?;
                let path = required_string(input.get("notebook_path"))?;
                vec![resolve_mutation_target(path, execution.as_deref())?]
            }
            Some("apply_patch") if product == Product::Codex => {
                let input = mutation_input(nested)?;
                let patch = match input.get("command") {
                    Some(Value::String(patch)) if !patch.is_empty() => patch.as_str(),
                    _ => {
                        return Err(untrusted_target(
                            "Codex apply_patch mutation must contain a non-empty command string",
                        ));
                    }
                };
                parse_apply_patch_targets(patch, execution.as_deref())?
            }
            Some("apply_patch") => {
                return Err(untrusted_target(
                    "apply_patch mutation has no documented native mapping for this provider",
                ));
            }
            _ => execution.iter().cloned().collect(),
        }
    };
    deduplicate_targets(&mut targets);
    Ok((targets, execution))
}

fn mutation_input(nested: Option<&Map<String, Value>>) -> Result<&Map<String, Value>, HookError> {
    nested.ok_or_else(|| untrusted_target("path-bearing mutation is missing tool_input"))
}

fn exactly_one_path<'a>(
    input: &'a Map<String, Value>,
    keys: &[&str],
) -> Result<&'a str, HookError> {
    let mut values = keys.iter().filter_map(|key| input.get(*key));
    let Some(value) = values.next() else {
        return Err(untrusted_target(
            "path-bearing mutation is missing its provider path field",
        ));
    };
    if values.next().is_some() {
        return Err(untrusted_target(
            "path-bearing mutation has ambiguous provider path fields",
        ));
    }
    required_string(Some(value))
}

fn required_string(value: Option<&Value>) -> Result<&str, HookError> {
    match value {
        Some(Value::String(value))
            if !value.is_empty()
                && !value.contains('\0')
                && value.len() <= MAX_MUTATION_TARGET_BYTES =>
        {
            Ok(value)
        }
        _ => Err(untrusted_target(
            "path-bearing mutation target must be a bounded non-empty string",
        )),
    }
}

fn resolve_mutation_target(value: &str, execution: Option<&Path>) -> Result<PathBuf, HookError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Ok(path);
    }
    execution.map(|cwd| cwd.join(path)).ok_or_else(|| {
        untrusted_target("relative mutation target requires an absolute execution directory")
    })
}

fn parse_apply_patch_targets(
    patch: &str,
    execution: Option<&Path>,
) -> Result<Vec<PathBuf>, HookError> {
    let lines = patch.lines().collect::<Vec<_>>();
    if lines.first() != Some(&"*** Begin Patch") || lines.last() != Some(&"*** End Patch") {
        return Err(untrusted_target(
            "apply_patch input must have exact begin and end markers",
        ));
    }

    let mut targets = Vec::new();
    for line in &lines[1..lines.len() - 1] {
        let path = [
            "*** Add File: ",
            "*** Update File: ",
            "*** Delete File: ",
            "*** Move to: ",
        ]
        .iter()
        .find_map(|prefix| line.strip_prefix(prefix));
        if let Some(path) = path {
            if path.is_empty() || path.contains('\0') || path.len() > MAX_MUTATION_TARGET_BYTES {
                return Err(untrusted_target(
                    "apply_patch target must be a bounded non-empty path",
                ));
            }
            targets.push(resolve_mutation_target(path, execution)?);
            if targets.len() > MAX_MUTATION_TARGETS {
                return Err(untrusted_target("apply_patch input has too many targets"));
            }
        } else if line.starts_with("*** Add File:")
            || line.starts_with("*** Update File:")
            || line.starts_with("*** Delete File:")
            || line.starts_with("*** Move to:")
            || matches!(*line, "*** Begin Patch" | "*** End Patch")
            || (line.starts_with("*** ") && *line != "*** End of File")
        {
            return Err(untrusted_target(
                "apply_patch input contains an incomplete target directive",
            ));
        }
    }
    if targets.is_empty() {
        return Err(untrusted_target(
            "apply_patch input does not contain a mapped mutation target",
        ));
    }
    deduplicate_targets(&mut targets);
    Ok(targets)
}

fn deduplicate_targets(targets: &mut Vec<PathBuf>) {
    let mut index = 0;
    while index < targets.len() {
        if targets[..index].contains(&targets[index]) {
            targets.remove(index);
        } else {
            index += 1;
        }
    }
}

fn deduplicate_target_bindings(bindings: &mut Vec<TargetBinding>) {
    let mut index = 0;
    while index < bindings.len() {
        if bindings[..index]
            .iter()
            .any(|binding| binding.effective_path == bindings[index].effective_path)
        {
            bindings.remove(index);
        } else {
            index += 1;
        }
    }
}

fn deduplicate_paths(paths: &mut Vec<PathBuf>) {
    let mut index = 0;
    while index < paths.len() {
        if paths[..index].contains(&paths[index]) {
            paths.remove(index);
        } else {
            index += 1;
        }
    }
}

fn untrusted_target(message: &str) -> HookError {
    HookError::data("provider-target-untrusted", message)
}

fn target_set_binding_material(bindings: &[TargetBinding]) -> Result<Vec<u8>, HookError> {
    match bindings {
        [] => Ok(b"target-unavailable".to_vec()),
        [binding] => target_binding_material(binding),
        bindings => {
            let mut material = b"agent-hook.target-set-binding.v1\0".to_vec();
            material.extend_from_slice(&(bindings.len() as u64).to_le_bytes());
            for binding in bindings {
                let item = target_binding_material(binding)?;
                material.extend_from_slice(&(item.len() as u64).to_le_bytes());
                material.extend_from_slice(&item);
            }
            Ok(material)
        }
    }
}

fn target_binding_material(binding: &TargetBinding) -> Result<Vec<u8>, HookError> {
    let metadata = std::fs::metadata(&binding.binding_root)
        .map_err(|_| untrusted_target("mutation target binding root is unavailable"))?;
    let mut material = b"agent-hook.target-binding.v2\0".to_vec();
    material.extend_from_slice(binding.effective_path.as_os_str().as_encoded_bytes());
    material.push(0);
    material.extend_from_slice(binding.binding_root.as_os_str().as_encoded_bytes());
    material.extend_from_slice(&metadata.dev().to_le_bytes());
    material.extend_from_slice(&metadata.ino().to_le_bytes());
    Ok(material)
}

pub(crate) fn command_text(object: &Map<String, Value>) -> Option<&str> {
    string_at(object, &["command"])
        .or_else(|| nested_string(object, "tool_input", &["command", "cmd"]))
}

pub(crate) fn exact_main_agent_bootstrap_command(input: &[u8]) -> bool {
    let Some(command) = strict_json::from_slice(input)
        .ok()
        .and_then(|value| value.as_object().and_then(command_text).map(str::to_string))
    else {
        return false;
    };
    if command.len() > MAX_MUTATION_TARGET_BYTES {
        return false;
    }
    let Ok(words) = shell_words::split(&command) else {
        return false;
    };
    if words.len() != 6 || command != shell_words::join(words.iter()) {
        return false;
    }
    let executable = Path::new(&words[0]);
    let trusted_shape = words[0] == "main-agent"
        || (executable.is_absolute() && executable.file_name() == Some(OsStr::new("main-agent")));
    trusted_shape
        && words[1] == "bootstrap"
        && words[2] == "--idempotency-key"
        && lifecycle_idempotency_key(&words[3])
        && words[4..] == ["--format", "json"]
}

fn lifecycle_idempotency_key(value: &str) -> bool {
    (8..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
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

fn parse_provider_json(input: &[u8]) -> Result<Value, HookError> {
    strict_json::from_slice(input).map_err(|error| {
        if error.to_string().contains("duplicate object key") {
            HookError::data(
                "provider-input-duplicate-key",
                "provider hook input contains a duplicate object key",
            )
        } else {
            HookError::data(
                "provider-input-invalid",
                "provider hook input is not valid bounded JSON",
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DecisionReason, ShadowObservation};

    #[test]
    fn provider_render_uses_the_full_aggregated_context() {
        for product in [Product::Codex, Product::Claude] {
            let decision = NormalizedDecision {
                schema_version: "agent-hook.decision.v1".to_string(),
                request_id: "request:test".to_string(),
                product,
                event: "UserPromptSubmit".to_string(),
                action: DecisionAction::Context,
                reasons: vec![DecisionReason {
                    rule_id: "fixture.context".to_string(),
                    code: "fixture-context".to_string(),
                    disposition: "context".to_string(),
                }],
                context: Some("first context\nsecond context".to_string()),
                replacement: None,
                shadow: Vec::<ShadowObservation>::new(),
                config_digest: "sha256:config".to_string(),
                policy_digest: "sha256:policy".to_string(),
                recovery_applied: false,
                provider_output: Some(json!({
                    "hookSpecificOutput": {
                        "hookEventName": "UserPromptSubmit",
                        "additionalContext": "first context",
                    },
                    "suppressOutput": true,
                })),
            };

            let rendered: Value =
                serde_json::from_str(&render_provider(&decision).expect("provider output"))
                    .expect("provider JSON");
            assert_eq!(
                rendered["hookSpecificOutput"]["additionalContext"],
                "first context\nsecond context"
            );
            assert_eq!(rendered["suppressOutput"], true);
        }
    }

    #[test]
    fn provider_render_preserves_native_envelopes_without_aggregate_context() {
        for product in [Product::Codex, Product::Claude] {
            for action in [
                DecisionAction::Allow,
                DecisionAction::Block,
                DecisionAction::Transform,
            ] {
                let provider_output = json!({
                    "decision": "provider-native",
                    "reason": "preserve me",
                    "suppressOutput": true,
                    "providerExtension": {"product": product.as_str()},
                });
                let decision = NormalizedDecision {
                    schema_version: "agent-hook.decision.v1".to_string(),
                    request_id: "request:provider-preservation".to_string(),
                    product,
                    event: "PreToolUse".to_string(),
                    action,
                    reasons: Vec::new(),
                    context: None,
                    replacement: None,
                    shadow: Vec::<ShadowObservation>::new(),
                    config_digest: "sha256:config".to_string(),
                    policy_digest: "sha256:policy".to_string(),
                    recovery_applied: false,
                    provider_output: Some(provider_output.clone()),
                };

                let rendered: Value =
                    serde_json::from_str(&render_provider(&decision).expect("provider output"))
                        .expect("provider JSON");
                assert_eq!(rendered, provider_output);
            }
        }
    }

    #[test]
    fn provider_render_synchronizes_mixed_accepted_context_locations() {
        for product in [Product::Codex, Product::Claude] {
            let decision = NormalizedDecision {
                schema_version: "agent-hook.decision.v1".to_string(),
                request_id: "request:mixed-context".to_string(),
                product,
                event: "UserPromptSubmit".to_string(),
                action: DecisionAction::Context,
                reasons: Vec::new(),
                context: Some("first context\nsecond context".to_string()),
                replacement: None,
                shadow: Vec::<ShadowObservation>::new(),
                config_digest: "sha256:config".to_string(),
                policy_digest: "sha256:policy".to_string(),
                recovery_applied: false,
                provider_output: Some(json!({
                    "additionalContext": "first context",
                    "hookSpecificOutput": {
                        "hookEventName": "UserPromptSubmit",
                    },
                    "suppressOutput": true,
                })),
            };

            let rendered: Value =
                serde_json::from_str(&render_provider(&decision).expect("provider output"))
                    .expect("provider JSON");
            assert_eq!(
                rendered["hookSpecificOutput"]["additionalContext"],
                "first context\nsecond context"
            );
            assert_eq!(
                rendered["additionalContext"],
                "first context\nsecond context"
            );
            assert_eq!(rendered["suppressOutput"], true);
        }
    }
}

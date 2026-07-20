use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::contract::{
    effective_mode_for_product, matcher_expression_matches, runtime_handler_filename,
};
use crate::error::HookError;
use crate::liveness;
use crate::model::{
    Capability, DECISION_VERSION, DecisionAction, DecisionReason, FailurePosture, LoadedPolicy,
    NormalizedDecision, NormalizedRequest, Product, RuleMode, ShadowObservation,
};

const MAX_AGGREGATE_CONTEXT: usize = 16 * 1024;
const MAX_REASONS: usize = 64;
const MAX_HANDLER_OUTPUT: usize = 256 * 1024;
const HANDLER_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_EXECUTABLE_CAPABILITIES: usize = 16;
const MAX_DISPATCH_CHILD_OUTPUT: usize = 512 * 1024;
const DISPATCH_CHILD_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Debug)]
struct RuleOutcome {
    action: DecisionAction,
    code: String,
    context: Option<String>,
    replacement: Option<Value>,
    provider_output: Option<Value>,
}

#[derive(Debug)]
struct ExecutionBudget {
    started: Instant,
    children: usize,
    retained_output: usize,
}

impl ExecutionBudget {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            children: 0,
            retained_output: 0,
        }
    }

    fn reserve_child(&mut self) -> Result<(Duration, usize), HookError> {
        let elapsed = self.started.elapsed();
        if elapsed >= DISPATCH_CHILD_DEADLINE {
            return Err(HookError::data(
                "dispatch-deadline-exceeded",
                "dispatch executable capability deadline exceeded",
            ));
        }
        if self.children >= MAX_EXECUTABLE_CAPABILITIES {
            return Err(HookError::data(
                "dispatch-child-budget-exceeded",
                "dispatch executable capability count exceeds 16",
            ));
        }
        self.children += 1;
        Ok((
            HANDLER_TIMEOUT.min(DISPATCH_CHILD_DEADLINE - elapsed),
            MAX_DISPATCH_CHILD_OUTPUT.saturating_sub(self.retained_output),
        ))
    }

    fn retain_output(&mut self, bytes: usize) -> Result<(), HookError> {
        self.retained_output = self.retained_output.saturating_add(bytes);
        if self.retained_output > MAX_DISPATCH_CHILD_OUTPUT {
            return Err(HookError::data(
                "dispatch-output-budget-exceeded",
                "dispatch executable capability output exceeds 512 KiB",
            ));
        }
        Ok(())
    }
}

pub fn evaluate(
    loaded: &LoadedPolicy,
    request: &NormalizedRequest,
    raw: &[u8],
    all_shadow: bool,
    recovery_rules: &BTreeSet<String>,
    coordination: Option<&liveness::Snapshot>,
) -> Result<NormalizedDecision, HookError> {
    let mut selected = loaded
        .bundle
        .rules
        .iter()
        .filter(|rule| {
            rule.products.contains(&request.product)
                && rule.events.iter().any(|event| event == &request.event)
                && rule.matcher.as_deref().is_none_or(|matcher| {
                    request
                        .matcher
                        .as_deref()
                        .is_some_and(|candidate| matcher_expression_matches(matcher, candidate))
                })
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.id.cmp(&right.id))
    });

    let executable_count = selected
        .iter()
        .filter(|rule| !recovery_rules.contains(&rule.id))
        .filter(|rule| {
            let mode = effective_mode_for_product(loaded, request.product, rule);
            !all_shadow && mode == RuleMode::Enforce
        })
        .filter(|rule| {
            matches!(
                rule.capability,
                Capability::SessionActivity { .. } | Capability::RuntimeKitHandler { .. }
            )
        })
        .count();
    if executable_count > MAX_EXECUTABLE_CAPABILITIES {
        return Err(HookError::data(
            "dispatch-child-budget-exceeded",
            "dispatch executable capability count exceeds 16",
        ));
    }

    let mut enforced = Vec::new();
    let mut shadow = Vec::new();
    let mut recovery_applied = false;
    let mut execution_budget = ExecutionBudget::new();
    for rule in selected {
        let mode = if all_shadow {
            if effective_mode_for_product(loaded, request.product, rule) == RuleMode::Disabled {
                RuleMode::Disabled
            } else {
                RuleMode::Shadow
            }
        } else {
            effective_mode_for_product(loaded, request.product, rule)
        };
        if mode == RuleMode::Disabled {
            continue;
        }
        if mode == RuleMode::Shadow {
            let outcome = evaluate_shadow(&rule.capability);
            shadow.push(ShadowObservation {
                rule_id: rule.id.clone(),
                action: outcome.action,
                code: outcome.code,
            });
            continue;
        }
        if recovery_rules.contains(&rule.id) {
            recovery_applied = true;
            enforced.push((
                rule.id.clone(),
                RuleOutcome {
                    action: DecisionAction::Allow,
                    code: "recovery-exact-bypass".to_string(),
                    context: None,
                    replacement: None,
                    provider_output: None,
                },
            ));
            continue;
        }
        let outcome = match evaluate_capability(
            &rule.capability,
            request,
            raw,
            &mut execution_budget,
            coordination,
        ) {
            Ok(outcome) => outcome,
            Err(error) if error.code.starts_with("dispatch-") => return Err(error),
            Err(_) => failure_outcome(rule.failure_posture, &rule.id),
        };
        enforced.push((rule.id.clone(), outcome));
    }

    aggregate(loaded, request, enforced, shadow, recovery_applied)
}

pub fn needs_coordination(
    loaded: &LoadedPolicy,
    request: &NormalizedRequest,
    all_shadow: bool,
    recovery_rules: &BTreeSet<String>,
) -> bool {
    !all_shadow
        && loaded.bundle.rules.iter().any(|rule| {
            rule.products.contains(&request.product)
                && rule.events.iter().any(|event| event == &request.event)
                && rule.matcher.as_deref().is_none_or(|matcher| {
                    request
                        .matcher
                        .as_deref()
                        .is_some_and(|candidate| matcher_expression_matches(matcher, candidate))
                })
                && !recovery_rules.contains(&rule.id)
                && effective_mode_for_product(loaded, request.product, rule) == RuleMode::Enforce
                && matches!(
                    rule.capability,
                    Capability::SemanticConflict { .. } | Capability::OwnerLiveness { .. }
                )
        })
}

fn evaluate_shadow(capability: &Capability) -> RuleOutcome {
    match capability {
        Capability::Allow { reason_code } => simple(DecisionAction::Allow, reason_code),
        Capability::Warn { reason_code, .. } => simple(DecisionAction::Warn, reason_code),
        Capability::Block { reason_code, .. } => simple(DecisionAction::Block, reason_code),
        Capability::Context { reason_code, .. } => simple(DecisionAction::Context, reason_code),
        Capability::Transform { reason_code, .. } => simple(DecisionAction::Transform, reason_code),
        Capability::SemanticConflict { reason_code } => simple(DecisionAction::Warn, reason_code),
        Capability::OwnerLiveness { reason_code, .. } => simple(DecisionAction::Warn, reason_code),
        Capability::SessionActivity { .. } | Capability::RuntimeKitHandler { .. } => {
            simple(DecisionAction::Allow, "shadow-side-effect-skipped")
        }
    }
}

fn evaluate_capability(
    capability: &Capability,
    request: &NormalizedRequest,
    raw: &[u8],
    execution_budget: &mut ExecutionBudget,
    coordination: Option<&liveness::Snapshot>,
) -> Result<RuleOutcome, HookError> {
    Ok(match capability {
        Capability::Allow { reason_code } => simple(DecisionAction::Allow, reason_code),
        Capability::Warn {
            reason_code,
            message,
        } => RuleOutcome {
            action: DecisionAction::Warn,
            code: reason_code.clone(),
            context: Some(message.clone()),
            replacement: None,
            provider_output: None,
        },
        Capability::Block {
            reason_code,
            message,
        } => RuleOutcome {
            action: DecisionAction::Block,
            code: reason_code.clone(),
            context: Some(message.clone()),
            replacement: None,
            provider_output: None,
        },
        Capability::Context { reason_code, text } => RuleOutcome {
            action: DecisionAction::Context,
            code: reason_code.clone(),
            context: Some(text.clone()),
            replacement: None,
            provider_output: None,
        },
        Capability::Transform {
            reason_code,
            replacement,
        } => RuleOutcome {
            action: DecisionAction::Transform,
            code: reason_code.clone(),
            context: None,
            replacement: Some(replacement.clone()),
            provider_output: None,
        },
        Capability::SemanticConflict { reason_code } => simple(
            liveness::semantic_conflict_action(request.semantic_conflict, coordination),
            reason_code,
        ),
        Capability::OwnerLiveness {
            reason_code: _,
            legacy_ttl_seconds,
        } => {
            let outcome = liveness::classify(request, *legacy_ttl_seconds, coordination);
            simple(outcome.action, &outcome.reason_code)
        }
        Capability::SessionActivity { reason_code } => {
            run_session_activity(request, raw, execution_budget)?;
            simple(DecisionAction::Allow, reason_code)
        }
        Capability::RuntimeKitHandler { handler_id } => {
            run_runtime_handler(request.product, handler_id, raw, execution_budget)?
        }
    })
}

fn failure_outcome(posture: FailurePosture, rule_id: &str) -> RuleOutcome {
    match posture {
        FailurePosture::Open => simple(
            DecisionAction::Allow,
            &format!("{rule_id}:capability-failure-open"),
        ),
        FailurePosture::Warn => simple(
            DecisionAction::Warn,
            &format!("{rule_id}:capability-failure-warn"),
        ),
        FailurePosture::Closed => simple(
            DecisionAction::Block,
            &format!("{rule_id}:capability-failure-closed"),
        ),
    }
}

fn simple(action: DecisionAction, code: &str) -> RuleOutcome {
    RuleOutcome {
        action,
        code: code.to_string(),
        context: None,
        replacement: None,
        provider_output: None,
    }
}

fn aggregate(
    loaded: &LoadedPolicy,
    request: &NormalizedRequest,
    outcomes: Vec<(String, RuleOutcome)>,
    shadow: Vec<ShadowObservation>,
    recovery_applied: bool,
) -> Result<NormalizedDecision, HookError> {
    let mut action = DecisionAction::Allow;
    let mut reasons = Vec::new();
    let mut contexts = Vec::new();
    let mut replacement: Option<Value> = None;
    let mut provider_output = None;
    let mut transform_conflicted = false;
    for (rule_id, outcome) in outcomes {
        if reasons.len() >= MAX_REASONS {
            return Err(HookError::data(
                "decision-reason-limit",
                "aggregate decision exceeds the reason limit",
            ));
        }
        if outcome.action == DecisionAction::Transform {
            if let (Some(existing), Some(candidate)) = (&replacement, &outcome.replacement)
                && existing != candidate
            {
                action = DecisionAction::Block;
                reasons.push(DecisionReason {
                    rule_id,
                    code: "transform-conflict".to_string(),
                    disposition: "block".to_string(),
                });
                replacement = None;
                provider_output = None;
                transform_conflicted = true;
                continue;
            }
            if !transform_conflicted && replacement.is_none() {
                replacement = outcome.replacement.clone();
            }
        }
        if let Some(context) = outcome.context {
            contexts.push(context);
        }
        if rank(outcome.action) > rank(action) {
            action = outcome.action;
            provider_output = outcome.provider_output;
        } else if rank(outcome.action) == rank(action)
            && provider_output.is_none()
            && outcome.provider_output.is_some()
        {
            provider_output = outcome.provider_output;
        }
        reasons.push(DecisionReason {
            rule_id,
            code: outcome.code,
            disposition: disposition(outcome.action).to_string(),
        });
    }
    let context = if contexts.is_empty() {
        None
    } else {
        let joined = contexts.join("\n");
        if joined.len() > MAX_AGGREGATE_CONTEXT {
            return Err(HookError::data(
                "decision-context-too-large",
                "aggregate decision context exceeds 16 KiB",
            ));
        }
        Some(joined)
    };
    Ok(NormalizedDecision {
        schema_version: DECISION_VERSION.to_string(),
        request_id: request.request_id.clone(),
        product: request.product,
        event: request.event.clone(),
        action,
        reasons,
        context,
        replacement,
        shadow,
        config_digest: loaded.config_digest.clone(),
        policy_digest: loaded.policy_digest.clone(),
        recovery_applied,
        provider_output,
    })
}

fn rank(action: DecisionAction) -> u8 {
    match action {
        DecisionAction::Allow => 0,
        DecisionAction::Warn => 1,
        DecisionAction::Context => 2,
        DecisionAction::Transform => 3,
        DecisionAction::Block => 4,
    }
}

fn disposition(action: DecisionAction) -> &'static str {
    match action {
        DecisionAction::Allow => "allow",
        DecisionAction::Warn => "warn",
        DecisionAction::Context => "context",
        DecisionAction::Transform => "transform",
        DecisionAction::Block => "block",
    }
}

fn run_session_activity(
    request: &NormalizedRequest,
    raw: &[u8],
    execution_budget: &mut ExecutionBudget,
) -> Result<(), HookError> {
    let Some(session_id) = std::env::var("AGENT_SESSION_ID")
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
    let Some(event) = crate::adapter::normalize_activity_event(request, raw, &runtime_id)? else {
        return Ok(());
    };
    let binary = std::env::var_os("AGENT_SESSION_BIN").unwrap_or_else(|| "agent-session".into());
    let mut command = Command::new(binary);
    command
        .args(["activity", "event", "--stdin", &session_id])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let output = run_with_budget(command, &event, execution_budget)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(HookError::runtime(
            "session-activity-failed",
            "agent-session activity capability failed",
        ))
    }
}

fn run_runtime_handler(
    product: Product,
    handler_id: &str,
    raw: &[u8],
    execution_budget: &mut ExecutionBudget,
) -> Result<RuleOutcome, HookError> {
    let filename = runtime_handler_filename(handler_id).ok_or_else(|| {
        HookError::data(
            "handler-id-unsupported",
            "runtime-kit handler is not in the compiled v1 allowlist",
        )
    })?;
    let path = runtime_hook_root(product)?.join(filename);
    validate_handler(&path)?;
    let mut command = Command::new(&path);
    command
        .env("AGENT_RUNTIME_PRODUCT", product.as_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let output = run_with_budget(command, raw, execution_budget)?;
    if output.stdout.len() > MAX_HANDLER_OUTPUT {
        return Err(HookError::data(
            "handler-output-too-large",
            "runtime-kit handler output exceeds 256 KiB",
        ));
    }
    if !output.status.success() {
        return Err(HookError::runtime(
            "handler-failed",
            "runtime-kit handler returned failure",
        ));
    }
    if output.stdout.is_empty() {
        return Ok(simple(DecisionAction::Allow, handler_id));
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        HookError::data(
            "handler-output-invalid",
            "runtime-kit handler output is not JSON",
        )
    })?;
    let action = infer_provider_action(&value);
    let context = value
        .pointer("/hookSpecificOutput/additionalContext")
        .or_else(|| value.get("additionalContext"))
        .and_then(Value::as_str)
        .map(|value| value.chars().take(16 * 1024).collect());
    let replacement = value
        .pointer("/hookSpecificOutput/updatedInput")
        .or_else(|| value.get("updatedInput"))
        .filter(|value| value.is_object())
        .cloned();
    Ok(RuleOutcome {
        action,
        code: handler_id.to_string(),
        context,
        replacement,
        provider_output: Some(value),
    })
}

fn infer_provider_action(value: &Value) -> DecisionAction {
    let decision = value
        .get("decision")
        .or_else(|| value.pointer("/hookSpecificOutput/permissionDecision"))
        .and_then(Value::as_str);
    if value.get("continue").and_then(Value::as_bool) == Some(false)
        || matches!(decision, Some("block" | "deny" | "denied"))
    {
        return DecisionAction::Block;
    }
    if value.pointer("/hookSpecificOutput/updatedInput").is_some()
        || value.get("updatedInput").is_some()
    {
        return DecisionAction::Transform;
    }
    if value
        .pointer("/hookSpecificOutput/additionalContext")
        .is_some()
        || value.get("additionalContext").is_some()
    {
        return DecisionAction::Context;
    }
    DecisionAction::Allow
}

fn runtime_hook_root(product: Product) -> Result<PathBuf, HookError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| HookError::runtime("home-unavailable", "HOME is required"))?;
    Ok(match product {
        Product::Codex => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".codex"))
            .join("hooks"),
        Product::Claude => home.join(".claude/hooks"),
        Product::Hermes => home.join(".hermes/hooks"),
    })
}

fn validate_handler(path: &Path) -> Result<(), HookError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| {
        HookError::runtime("handler-unavailable", "runtime-kit handler is unavailable")
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(HookError::data(
            "handler-untrusted",
            "runtime-kit handler type, owner, or mode is untrusted",
        ));
    }
    Ok(())
}

fn run_with_budget(
    command: Command,
    input: &[u8],
    budget: &mut ExecutionBudget,
) -> Result<std::process::Output, HookError> {
    let (timeout, output_limit) = budget.reserve_child()?;
    let output = run_bounded(command, input, timeout, output_limit, true)?;
    budget.retain_output(output.stdout.len().saturating_add(output.stderr.len()))?;
    Ok(output)
}

fn run_bounded(
    mut command: Command,
    input: &[u8],
    timeout: Duration,
    output_limit: usize,
    dispatch_deadline: bool,
) -> Result<std::process::Output, HookError> {
    command.process_group(0);
    let deadline = Instant::now() + timeout;
    let mut child = command
        .spawn()
        .map_err(|_| HookError::runtime("capability-unavailable", "capability could not start"))?;
    let input = input.to_vec();
    let input_handle = child.stdin.take().map(|mut stdin| {
        thread::spawn(move || {
            stdin.write_all(&input).map_err(|_| {
                HookError::runtime("capability-input-failed", "capability input failed")
            })
        })
    });
    let stdout_handle = child
        .stdout
        .take()
        .map(|stdout| thread::spawn(move || read_capped(stdout, output_limit + 1)));
    let stderr_handle = child
        .stderr
        .take()
        .map(|stderr| thread::spawn(move || read_capped(stderr, output_limit + 1)));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                terminate_process_group(&mut child);
                return Err(if dispatch_deadline {
                    HookError::data(
                        "dispatch-deadline-exceeded",
                        "dispatch executable capability deadline exceeded",
                    )
                } else {
                    HookError::runtime(
                        "capability-timeout",
                        "capability exceeded its fixed timeout",
                    )
                });
            }
            Err(_) => {
                terminate_process_group(&mut child);
                return Err(HookError::runtime(
                    "capability-wait-failed",
                    "capability state could not be read",
                ));
            }
        }
    };
    terminate_descendants(child.id());
    if let Some(handle) = input_handle {
        wait_for_thread(&handle, deadline)?;
        handle.join().map_err(|_| {
            HookError::runtime("capability-input-failed", "capability input failed")
        })??;
    }
    let stdout = join_capped(stdout_handle, deadline)?;
    let stderr = join_capped(stderr_handle, deadline)?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn read_capped(mut pipe: impl Read, limit: usize) -> Result<Vec<u8>, HookError> {
    let mut retained = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = pipe.read(&mut buffer).map_err(|_| {
            HookError::runtime("capability-output-failed", "capability output failed")
        })?;
        if read == 0 {
            return Ok(retained);
        }
        let remaining = limit.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

fn join_capped(
    handle: Option<thread::JoinHandle<Result<Vec<u8>, HookError>>>,
    deadline: Instant,
) -> Result<Vec<u8>, HookError> {
    match handle {
        Some(handle) => {
            wait_for_thread(&handle, deadline)?;
            handle.join().map_err(|_| {
                HookError::runtime("capability-output-failed", "capability output failed")
            })?
        }
        None => Ok(Vec::new()),
    }
}

fn wait_for_thread<T>(handle: &thread::JoinHandle<T>, deadline: Instant) -> Result<(), HookError> {
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return Err(HookError::data(
                "dispatch-deadline-exceeded",
                "dispatch executable capability deadline exceeded while draining pipes",
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn terminate_descendants(process_group: u32) {
    let process_group = process_group as libc::pid_t;
    unsafe {
        libc::kill(-process_group, libc::SIGKILL);
    }
}

fn terminate_process_group(child: &mut std::process::Child) {
    terminate_descendants(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_runner_drains_overflowing_stdout_and_stderr_without_deadlock() {
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "head -c 307200 /dev/zero; head -c 307200 /dev/zero >&2",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let started = Instant::now();
        let output = run_bounded(command, &[], HANDLER_TIMEOUT, MAX_HANDLER_OUTPUT, false)
            .expect("bounded output");

        assert!(output.status.success());
        assert_eq!(output.stdout.len(), MAX_HANDLER_OUTPUT + 1);
        assert_eq!(output.stderr.len(), MAX_HANDLER_OUTPUT + 1);
        assert!(started.elapsed() < HANDLER_TIMEOUT);
    }

    #[test]
    fn bounded_runner_handles_child_exit_before_stdin_is_consumed() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "exit 0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let started = Instant::now();
        let error = run_bounded(
            command,
            &vec![b'x'; 1024 * 1024],
            HANDLER_TIMEOUT,
            MAX_HANDLER_OUTPUT,
            false,
        )
        .expect_err("closed child stdin must be reported");

        assert_eq!(error.code, "capability-input-failed");
        assert!(started.elapsed() < HANDLER_TIMEOUT);
    }
}

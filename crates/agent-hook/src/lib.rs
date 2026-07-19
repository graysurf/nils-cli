mod adapter;
mod cli;
mod completion;
mod contract;
mod error;
mod evaluator;
mod liveness;
mod model;
mod paths;
pub mod recovery;
pub mod setup;
mod trace;

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::time::Instant;

use clap::Parser;
use clap::error::ErrorKind;
use nils_common::cli_contract::OutputFormat;
use serde::Serialize;
use serde_json::json;

use cli::{Cli, Command, DispatchFormat, RecoveryCommand};
use error::{ErrorBody, ErrorEnvelope, HookError, SuccessEnvelope, schema};
use model::{
    Capability, DECISION_VERSION, DecisionAction, DecisionReason, NormalizedDecision, Product,
    SetupAction,
};
use paths::Layout;

pub use model::{Product as HookProduct, SetupAction as HookSetupAction};

pub fn run() -> i32 {
    run_with_args(env::args_os())
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let raw = args.into_iter().map(Into::into).collect::<Vec<_>>();
    let json_requested = raw
        .windows(2)
        .any(|pair| pair[0] == "--format" && pair[1] == "json");
    let cli = match Cli::try_parse_from(raw) {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                let code = error.exit_code();
                let _ = error.print();
                return code;
            }
            if json_requested {
                return emit_error(
                    "agent-hook",
                    &HookError::usage("invalid-arguments", "command arguments are invalid"),
                    true,
                );
            }
            let _ = error.print();
            return 64;
        }
    };
    let layout = match Layout::resolve(cli.config, cli.state_dir) {
        Ok(layout) => layout,
        Err(error) => return emit_error("agent-hook", &error, json_requested),
    };
    dispatch(layout, cli.policy.as_deref(), cli.command)
}

fn dispatch(layout: Layout, policy_override: Option<&std::path::Path>, command: Command) -> i32 {
    match command {
        Command::Completion(args) => completion::run(args.shell),
        Command::Dispatch(args) => run_dispatch(&layout, policy_override, args),
        Command::Validate(args) => {
            let loaded = match contract::load(&layout, policy_override) {
                Ok(loaded) => loaded,
                Err(error) => {
                    return emit_error(
                        "agent-hook validate",
                        &error,
                        args.format == OutputFormat::Json,
                    );
                }
            };
            let result = json!({
                "schema_version": "agent-hook.validation.v1",
                "bundle_id": loaded.bundle.bundle_id,
                "bundle_version": loaded.bundle.version,
                "rule_count": loaded.bundle.rules.len(),
                "config_digest": loaded.config_digest,
                "policy_digest": loaded.policy_digest,
            });
            emit_success("agent-hook validate", args.format, &result, || {
                format!(
                    "agent-hook config and policy are valid ({} rules)\n",
                    loaded.bundle.rules.len()
                )
            })
        }
        Command::Inventory(args) => {
            let loaded = match contract::load(&layout, policy_override) {
                Ok(loaded) => loaded,
                Err(error) => {
                    return emit_error(
                        "agent-hook inventory",
                        &error,
                        args.format == OutputFormat::Json,
                    );
                }
            };
            let rules = loaded
                .bundle
                .rules
                .iter()
                .map(|rule| {
                    json!({
                        "id": rule.id,
                        "products": rule.products,
                        "events": rule.events,
                        "matcher": rule.matcher,
                        "priority": rule.priority,
                        "mode": contract::effective_mode(&loaded, &rule.id, rule.mode),
                        "failure_posture": rule.failure_posture,
                        "override_class": rule.override_class,
                        "capability_id": capability_id(&rule.capability),
                    })
                })
                .collect::<Vec<_>>();
            let result = json!({
                "schema_version": "agent-hook.inventory.v1",
                "bundle_id": loaded.bundle.bundle_id,
                "bundle_version": loaded.bundle.version,
                "config_digest": loaded.config_digest,
                "policy_digest": loaded.policy_digest,
                "rules": rules,
            });
            emit_success("agent-hook inventory", args.format, &result, || {
                format!("agent-hook inventory: {} rules\n", rules.len())
            })
        }
        Command::Doctor(args) => {
            let loaded = match contract::load(&layout, policy_override) {
                Ok(loaded) => loaded,
                Err(error) => {
                    return emit_error(
                        "agent-hook doctor",
                        &error,
                        args.format == OutputFormat::Json,
                    );
                }
            };
            let products = if let Some(product) = args.product {
                vec![product]
            } else if args.all {
                vec![Product::Codex, Product::Claude, Product::Hermes]
            } else {
                vec![Product::Codex, Product::Claude]
            };
            let results = products
                .into_iter()
                .map(|product| setup::doctor(&loaded, product))
                .collect::<Result<Vec<_>, _>>();
            match results {
                Ok(results) => emit_success("agent-hook doctor", args.format, &results, || {
                    let summary = results
                        .iter()
                        .map(|result| format!("{}={}", result.product, result.status))
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!("agent-hook doctor: {summary}\n")
                }),
                Err(error) => emit_error(
                    "agent-hook doctor",
                    &error,
                    args.format == OutputFormat::Json,
                ),
            }
        }
        Command::Setup(args) => {
            let loaded = match contract::load(&layout, policy_override) {
                Ok(loaded) => loaded,
                Err(error) => {
                    return emit_error(
                        "agent-hook setup",
                        &error,
                        args.format == OutputFormat::Json,
                    );
                }
            };
            let action = if args.apply {
                SetupAction::Apply
            } else if args.repair {
                SetupAction::Repair
            } else if args.remove {
                SetupAction::Remove
            } else {
                SetupAction::DryRun
            };
            match setup::run(
                &layout,
                &loaded,
                args.product,
                action,
                args.expected_plan_digest.as_deref(),
            ) {
                Ok(result) => emit_success("agent-hook setup", args.format, &result, || {
                    format!(
                        "agent-hook setup {} {}: {} (owned: {}/{})\n",
                        result.product,
                        result.action,
                        result.status,
                        result.owned_count,
                        result.owned_groups.len()
                    )
                }),
                Err(error) => emit_error(
                    "agent-hook setup",
                    &error,
                    args.format == OutputFormat::Json,
                ),
            }
        }
        Command::Recovery(args) => run_recovery(&layout, args.command),
    }
}

fn run_dispatch(
    layout: &Layout,
    policy_override: Option<&std::path::Path>,
    args: cli::DispatchArgs,
) -> i32 {
    let started = Instant::now();
    let raw = match adapter::read_stdin() {
        Ok(raw) => raw,
        Err(error) => return emit_dispatch_error(args.format, &error),
    };
    let mut request = match adapter::normalize(args.product, args.event.as_deref(), &raw) {
        Ok(request) => request,
        Err(error) => return emit_dispatch_error(args.format, &error),
    };
    request.semantic_conflict = Some(liveness::derive_semantic_conflict(&request));
    let grant = match recovery::consume_for_dispatch(
        &layout.state_root,
        args.capability_file.as_deref(),
        &request,
    ) {
        Ok(grant) => grant,
        Err(error) => return emit_dispatch_error(args.format, &error),
    };
    let loaded = match contract::load(layout, policy_override) {
        Ok(loaded) => loaded,
        Err(_error) if !grant.rules.is_empty() => {
            let decision = emergency_decision(&request, grant.rules);
            return emit_decision(args.format, &decision);
        }
        Err(error) => return emit_dispatch_error(args.format, &error),
    };
    let decision = match evaluator::evaluate(&loaded, &request, &raw, args.shadow, &grant.rules) {
        Ok(decision) => decision,
        Err(error) => return emit_dispatch_error(args.format, &error),
    };
    if args.trace
        && let Err(error) = trace::append(
            &layout.state_root,
            &request,
            &decision,
            started.elapsed().as_micros(),
        )
    {
        return emit_dispatch_error(args.format, &error);
    }
    emit_decision(args.format, &decision)
}

fn run_recovery(layout: &Layout, command: RecoveryCommand) -> i32 {
    match command {
        RecoveryCommand::Challenge(args) => {
            if !contract::supported_event(args.product, &args.event) {
                return emit_error(
                    "agent-hook recovery challenge",
                    &HookError::usage(
                        "provider-event-unsupported",
                        "recovery event is unsupported",
                    ),
                    args.format == OutputFormat::Json,
                );
            }
            let input = recovery::ChallengeInput {
                product: args.product,
                event: &args.event,
                target_digest: &args.target_digest,
                command_digest: &args.command_digest,
                snapshot_digest: &args.snapshot_digest,
                rules: &args.rules,
                scope: args.scope,
                ttl_seconds: args.ttl_seconds,
                out: &args.out,
            };
            match recovery::create_challenge(&layout.state_root, input) {
                Ok(result) => emit_success(
                    "agent-hook recovery challenge",
                    args.format,
                    &result,
                    || {
                        format!(
                            "recovery challenge {} created; review digest {}\n",
                            result.challenge_id, result.challenge_digest
                        )
                    },
                ),
                Err(error) => emit_error(
                    "agent-hook recovery challenge",
                    &error,
                    args.format == OutputFormat::Json,
                ),
            }
        }
        RecoveryCommand::Authorize(args) => match recovery::authorize(
            &layout.state_root,
            &args.challenge_file,
            &args.expected_challenge_digest,
            &args.out,
        ) {
            Ok(result) => emit_success(
                "agent-hook recovery authorize",
                args.format,
                &result,
                || format!("recovery capability {} authorized\n", result.capability_id),
            ),
            Err(error) => emit_error(
                "agent-hook recovery authorize",
                &error,
                args.format == OutputFormat::Json,
            ),
        },
        RecoveryCommand::Consume(args) => match recovery::consume_exact(
            &layout.state_root,
            &args.capability_file,
            args.product,
            &args.event,
            &args.target_digest,
            &args.command_digest,
            &args.snapshot_digest,
        ) {
            Ok((result, _)) => {
                emit_success("agent-hook recovery consume", args.format, &result, || {
                    format!(
                        "recovery capability {} {}\n",
                        result.capability_id, result.status
                    )
                })
            }
            Err(error) => emit_error(
                "agent-hook recovery consume",
                &error,
                args.format == OutputFormat::Json,
            ),
        },
        RecoveryCommand::Status(args) => {
            let Some(capability_id) = args.capability_id.as_deref() else {
                return emit_error(
                    "agent-hook recovery status",
                    &HookError::usage(
                        "capability-id-required",
                        "--capability-id is required for redacted status",
                    ),
                    args.format == OutputFormat::Json,
                );
            };
            match recovery::status(&layout.state_root, capability_id) {
                Ok(result) => {
                    emit_success("agent-hook recovery status", args.format, &result, || {
                        format!(
                            "recovery capability {}: {}\n",
                            result.capability_id, result.status
                        )
                    })
                }
                Err(error) => emit_error(
                    "agent-hook recovery status",
                    &error,
                    args.format == OutputFormat::Json,
                ),
            }
        }
        RecoveryCommand::Revoke(args) => {
            match recovery::revoke(&layout.state_root, &args.capability_id) {
                Ok(result) => {
                    emit_success("agent-hook recovery revoke", args.format, &result, || {
                        format!("recovery capability {} revoked\n", result.capability_id)
                    })
                }
                Err(error) => emit_error(
                    "agent-hook recovery revoke",
                    &error,
                    args.format == OutputFormat::Json,
                ),
            }
        }
    }
}

fn emergency_decision(
    request: &model::NormalizedRequest,
    rules: BTreeSet<String>,
) -> NormalizedDecision {
    NormalizedDecision {
        schema_version: DECISION_VERSION.to_string(),
        request_id: request.request_id.clone(),
        product: request.product,
        event: request.event.clone(),
        action: DecisionAction::Allow,
        reasons: rules
            .into_iter()
            .map(|rule_id| DecisionReason {
                rule_id,
                code: "recovery-config-independent".to_string(),
                disposition: "allow".to_string(),
            })
            .collect(),
        context: None,
        replacement: None,
        shadow: Vec::new(),
        config_digest: "unavailable".to_string(),
        policy_digest: "unavailable".to_string(),
        recovery_applied: true,
        provider_output: None,
    }
}

fn emit_decision(format: DispatchFormat, decision: &NormalizedDecision) -> i32 {
    let code = if decision.action == DecisionAction::Block {
        1
    } else {
        0
    };
    match format {
        DispatchFormat::Provider => match adapter::render_provider(decision) {
            Ok(output) => println!("{output}"),
            Err(error) => return emit_dispatch_error(format, &error),
        },
        DispatchFormat::Json => print_json_success("agent-hook dispatch", decision),
        DispatchFormat::Text => {
            println!(
                "agent-hook {} {}: {} ({} reasons, {} shadow)",
                decision.product.as_str(),
                decision.event,
                action_name(decision.action),
                decision.reasons.len(),
                decision.shadow.len()
            );
        }
    }
    code
}

fn emit_dispatch_error(format: DispatchFormat, error: &HookError) -> i32 {
    match format {
        DispatchFormat::Json => emit_error("agent-hook dispatch", error, true),
        DispatchFormat::Provider => {
            let output = json!({
                "continue": false,
                "stopReason": format!("agent-hook:{}", error.code),
            });
            println!("{output}");
            error.exit_code
        }
        DispatchFormat::Text => emit_error("agent-hook dispatch", error, false),
    }
}

fn emit_success<T: Serialize>(
    command: &str,
    format: OutputFormat,
    result: &T,
    text: impl FnOnce() -> String,
) -> i32 {
    match format {
        OutputFormat::Json => print_json_success(command, result),
        OutputFormat::Text => print!("{}", text()),
    }
    0
}

fn print_json_success<T: Serialize>(command: &str, result: &T) {
    let envelope = SuccessEnvelope {
        schema_version: schema(command),
        command,
        ok: true,
        result,
    };
    println!(
        "{}",
        serde_json::to_string(&envelope).expect("serializable success envelope")
    );
}

fn emit_error(command: &str, error: &HookError, json: bool) -> i32 {
    if json {
        let envelope = ErrorEnvelope {
            schema_version: schema(command),
            command,
            ok: false,
            error: ErrorBody {
                code: &error.code,
                message: &error.message,
                details: error.details.as_deref(),
            },
        };
        println!(
            "{}",
            serde_json::to_string(&envelope).expect("serializable error envelope")
        );
    } else {
        eprintln!("agent-hook: {}: {}", error.code, error.message);
    }
    error.exit_code
}

fn capability_id(capability: &Capability) -> &'static str {
    match capability {
        Capability::Allow { .. } => "decision.allow.v1",
        Capability::Warn { .. } => "decision.warn.v1",
        Capability::Block { .. } => "decision.block.v1",
        Capability::Context { .. } => "decision.context.v1",
        Capability::Transform { .. } => "decision.transform.v1",
        Capability::SessionActivity { .. } => "agent-session.activity.v1",
        Capability::OwnerLiveness { .. } => "agent-session.owner-liveness.v1",
        Capability::SemanticConflict { .. } => "agent-session.semantic-conflict.v1",
        Capability::RuntimeKitHandler { .. } => "runtime-kit.handler.v1",
    }
}

fn action_name(action: DecisionAction) -> &'static str {
    match action {
        DecisionAction::Allow => "allow",
        DecisionAction::Warn => "warn",
        DecisionAction::Context => "context",
        DecisionAction::Transform => "transform",
        DecisionAction::Block => "block",
    }
}

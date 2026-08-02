mod adapter;
mod cli;
mod completion;
mod contract;
mod degraded;
mod effect;
mod error;
mod evaluator;
mod liveness;
mod model;
mod path_binding;
mod paths;
mod read_only;
pub mod recovery;
pub mod setup;
mod strict_json;
mod trace;

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::time::Instant;

use clap::Parser;
use clap::error::ErrorKind;
use nils_common::cli_contract::{
    Envelope, EnvelopeError, OutputFormat, emit_parse_error, schema_version_for,
};
use serde::Serialize;
use serde_json::json;

use cli::{Cli, Command, DispatchFormat, RecoveryCommand};
use error::HookError;
use model::{
    Capability, DECISION_VERSION, DecisionAction, DecisionReason, NormalizedDecision,
    OperationEffectClass, Product, RuleMode, SetupAction,
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
    let json_requested = detect_format_from_args(&raw) == OutputFormat::Json;
    let cli = match Cli::try_parse_from(raw) {
        Ok(cli) => cli,
        Err(error) => {
            let kind = error.kind();
            if matches!(
                kind,
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                let code = error.exit_code();
                let _ = error.print();
                return code;
            }
            let code = if kind == ErrorKind::InvalidSubcommand {
                "unknown-subcommand"
            } else {
                "parse-error"
            };
            return emit_parse_error(
                "agent-hook",
                if json_requested {
                    OutputFormat::Json
                } else {
                    OutputFormat::Text
                },
                code,
                &render_clap_message(&error),
            );
        }
    };
    let layout = match Layout::resolve(cli.config, cli.state_dir) {
        Ok(layout) => layout,
        Err(error) => return emit_error("agent-hook", &error, json_requested),
    };
    dispatch(layout, cli.policy.as_deref(), cli.command)
}

fn detect_format_from_args(args: &[OsString]) -> OutputFormat {
    let mut iter = args.iter().skip(1);
    while let Some(argument) = iter.next() {
        let Some(argument) = argument.to_str() else {
            continue;
        };
        if argument == "--format"
            && iter
                .next()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("json"))
        {
            return OutputFormat::Json;
        }
        if argument
            .strip_prefix("--format=")
            .is_some_and(|value| value.eq_ignore_ascii_case("json"))
        {
            return OutputFormat::Json;
        }
    }
    OutputFormat::Text
}

fn render_clap_message(error: &clap::Error) -> String {
    error
        .to_string()
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| {
            let line = line.trim();
            line.strip_prefix("error:")
                .map(str::trim)
                .unwrap_or(line)
                .to_string()
        })
        .unwrap_or_else(|| "command-line parse failed".to_string())
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
                    let effective_modes = rule
                        .products
                        .iter()
                        .map(|product| {
                            (
                                product.as_str(),
                                contract::effective_mode_for_product(&loaded, *product, rule),
                            )
                        })
                        .collect::<BTreeMap<_, _>>();
                    json!({
                        "id": rule.id,
                        "products": rule.products,
                        "events": rule.events,
                        "matcher": rule.matcher,
                        "priority": rule.priority,
                        "base_mode": rule.mode,
                        "effective_modes": effective_modes,
                        "failure_posture": rule.failure_posture,
                        "timeout_posture": rule.timeout_posture,
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
            let action = if args.remove && args.dry_run {
                SetupAction::RemoveDryRun
            } else if args.apply {
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
        Command::Recovery(args) => run_recovery(&layout, policy_override, args.command),
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
        Err(error) => return emit_dispatch_error(args.format, &error, None),
    };
    let mut request = match adapter::normalize(args.product, args.event.as_deref(), &raw) {
        Ok(request) => request,
        Err(error) => return emit_dispatch_error(args.format, &error, None),
    };
    let unmanaged = liveness::current_process_is_unmanaged();
    let grant = match recovery::consume_for_dispatch(
        &layout.state_root,
        args.capability_file.as_deref(),
        &request,
    ) {
        Ok(grant) => grant,
        Err(error) => return emit_dispatch_error(args.format, &error, Some(&request)),
    };
    let loaded = match contract::load(layout, policy_override) {
        Ok(loaded) => loaded,
        Err(_error) if !grant.rules.is_empty() => {
            let (decision, coordination_rule) = match emergency_decision(&request, &grant) {
                Ok(result) => result,
                Err(error) => return emit_dispatch_error(args.format, &error, Some(&request)),
            };
            let decision = match evaluator::apply_session_coordination(
                None,
                decision,
                &request,
                &raw,
                coordination_rule,
                evaluator::CoordinationExecution::new(
                    None,
                    OperationEffectClass::Unknown,
                    unmanaged,
                ),
            ) {
                Ok(decision) => decision,
                Err(error) => return emit_dispatch_error(args.format, &error, Some(&request)),
            };
            return emit_decision(args.format, &decision);
        }
        Err(error) => return emit_dispatch_error(args.format, &error, Some(&request)),
    };
    let prepared = match evaluator::prepare(&loaded, &request, args.shadow, &grant.rules) {
        Ok(prepared) => prepared,
        Err(error) => return emit_dispatch_error(args.format, &error, Some(&request)),
    };
    let mut coordination_mode_override = None;
    let coordination = if prepared.needs_coordination() {
        match liveness::load_snapshot(unmanaged) {
            Ok(snapshot) => snapshot,
            Err(error) => match liveness::coordination_failure_mode() {
                Some(mode) => {
                    coordination_mode_override = Some(mode);
                    None
                }
                None => return emit_dispatch_error(args.format, &error, Some(&request)),
            },
        }
    } else {
        None
    };
    let decision = match evaluator::evaluate(
        &loaded,
        &mut request,
        &raw,
        &prepared,
        coordination.as_ref(),
        coordination_mode_override,
        unmanaged,
    ) {
        Ok(decision) => decision,
        Err(error) => return emit_dispatch_error(args.format, &error, Some(&request)),
    };
    if args.trace
        && let Err(error) = trace::append(
            &layout.state_root,
            &request,
            &decision,
            started.elapsed().as_micros(),
        )
    {
        return emit_dispatch_error(args.format, &error, Some(&request));
    }
    emit_decision(args.format, &decision)
}

fn run_recovery(
    layout: &Layout,
    policy_override: Option<&std::path::Path>,
    command: RecoveryCommand,
) -> i32 {
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
            let loaded = match contract::load(layout, policy_override) {
                Ok(loaded) => loaded,
                Err(error) => {
                    return emit_error(
                        "agent-hook recovery challenge",
                        &error,
                        args.format == OutputFormat::Json,
                    );
                }
            };
            let manifest = recovery::RecoveryManifest {
                schema_version: "agent-hook.recovery-manifest.v1".to_string(),
                product: args.product,
                event: args.event.clone(),
                config_digest: loaded.config_digest.clone(),
                policy_digest: loaded.policy_digest.clone(),
                rules: loaded
                    .bundle
                    .rules
                    .iter()
                    .filter(|rule| {
                        rule.products.contains(&args.product)
                            && rule.events.contains(&args.event)
                            && contract::effective_mode_for_product(&loaded, args.product, rule)
                                == RuleMode::Enforce
                    })
                    .cloned()
                    .collect(),
            };
            let input = recovery::ChallengeInput {
                product: args.product,
                event: &args.event,
                target_digest: &args.target_digest,
                command_digest: &args.command_digest,
                snapshot_digest: &args.snapshot_digest,
                rules: &args.rules,
                manifest,
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

fn emergency_decision<'a>(
    request: &model::NormalizedRequest,
    grant: &'a recovery::RecoveryGrant,
) -> Result<(NormalizedDecision, Option<&'a model::PolicyRule>), HookError> {
    let manifest = grant.manifest.as_ref().ok_or_else(|| {
        HookError::data(
            "recovery-manifest-unavailable",
            "recovery capability has no independently verifiable rule manifest",
        )
    })?;
    let mut rules = manifest
        .rules
        .iter()
        .filter(|rule| {
            rule.matcher.as_deref().is_none_or(|matcher| {
                request.matcher.as_deref().is_some_and(|candidate| {
                    contract::matcher_expression_matches(matcher, candidate)
                })
            })
        })
        .collect::<Vec<_>>();
    rules.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut action = DecisionAction::Allow;
    let mut reasons = Vec::new();
    let mut coordination_rule = None;
    for rule in rules {
        if matches!(rule.capability, Capability::SessionCoordination { .. }) {
            if coordination_rule.replace(rule).is_some() {
                return Err(HookError::data(
                    "coordination-capability-ambiguous",
                    "one dispatch may select at most one session coordination rule",
                ));
            }
            continue;
        }
        let (candidate, code) = if grant.rules.contains(&rule.id) {
            (DecisionAction::Allow, "recovery-exact-bypass")
        } else if let Some(outcome) =
            evaluator::terminal_activity_failure_decision(&rule.capability, &request.event)
        {
            outcome
        } else {
            match &rule.capability {
                Capability::Allow { .. } => (DecisionAction::Allow, "recovery-manifest-allow"),
                Capability::Warn { .. } => (DecisionAction::Warn, "recovery-manifest-warn"),
                Capability::Block { .. } => (DecisionAction::Block, "recovery-manifest-block"),
                Capability::Context { .. } => {
                    (DecisionAction::Context, "recovery-manifest-context")
                }
                Capability::Transform { .. } => {
                    (DecisionAction::Transform, "recovery-manifest-transform")
                }
                Capability::SessionCoordination { .. } => {
                    unreachable!("session coordination is deferred above")
                }
                Capability::SessionActivity { .. }
                | Capability::OwnerLiveness { .. }
                | Capability::SemanticConflict { .. }
                | Capability::ExecutionReadOnly { .. }
                | Capability::RuntimeKitHandler { .. } => match rule.failure_posture {
                    model::FailurePosture::Open => {
                        (DecisionAction::Allow, "recovery-manifest-failure-open")
                    }
                    model::FailurePosture::Warn => {
                        (DecisionAction::Warn, "recovery-manifest-failure-warn")
                    }
                    model::FailurePosture::Closed => {
                        (DecisionAction::Block, "recovery-manifest-failure-closed")
                    }
                },
            }
        };
        if emergency_action_rank(candidate) > emergency_action_rank(action) {
            action = candidate;
        }
        reasons.push(DecisionReason {
            rule_id: rule.id.clone(),
            code: code.to_string(),
            disposition: action_name(candidate).to_string(),
        });
    }
    Ok((
        NormalizedDecision {
            schema_version: DECISION_VERSION.to_string(),
            request_id: request.request_id.clone(),
            product: request.product,
            event: request.event.clone(),
            action,
            reasons,
            context: None,
            replacement: None,
            shadow: Vec::new(),
            config_digest: manifest.config_digest.clone(),
            policy_digest: manifest.policy_digest.clone(),
            recovery_applied: true,
            provider_output: None,
        },
        coordination_rule,
    ))
}

fn emergency_action_rank(action: DecisionAction) -> u8 {
    match action {
        DecisionAction::Allow => 0,
        DecisionAction::Warn => 1,
        DecisionAction::Context => 2,
        DecisionAction::Transform => 3,
        DecisionAction::Block => 4,
    }
}

fn emit_decision(format: DispatchFormat, decision: &NormalizedDecision) -> i32 {
    let code = if decision.action == DecisionAction::Block
        && !matches!(format, DispatchFormat::Provider)
    {
        1
    } else {
        0
    };
    match format {
        DispatchFormat::Provider => match adapter::render_provider(decision) {
            Ok(output) => println!("{output}"),
            Err(error) => return emit_dispatch_error(format, &error, None),
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

fn emit_dispatch_error(
    format: DispatchFormat,
    error: &HookError,
    request: Option<&model::NormalizedRequest>,
) -> i32 {
    match format {
        DispatchFormat::Json => emit_error("agent-hook dispatch", error, true),
        DispatchFormat::Provider => {
            if let Some(request) = request {
                match adapter::render_provider_error(request.product, &request.event, &error.code) {
                    Ok(output) => println!("{output}"),
                    Err(_) => return error.exit_code,
                }
                0
            } else {
                eprintln!("agent-hook:{}: {}", error.code, error.message);
                2
            }
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
    let envelope = Envelope::success(command_schema(command), result);
    println!(
        "{}",
        serde_json::to_string(&envelope).expect("serializable success envelope")
    );
}

fn emit_error(command: &str, error: &HookError, json: bool) -> i32 {
    if json {
        let mut body = EnvelopeError::new(&error.code, &error.message);
        if let Some(details) = error.details.as_deref() {
            body = body.with_details(details.clone());
        }
        let envelope: Envelope<()> = Envelope::failure(command_schema(command), body);
        println!(
            "{}",
            serde_json::to_string(&envelope).expect("serializable error envelope")
        );
    } else {
        eprintln!("agent-hook: {}: {}", error.code, error.message);
    }
    error.exit_code
}

fn command_schema(command: &str) -> String {
    let command = command
        .strip_prefix("agent-hook")
        .unwrap_or(command)
        .trim()
        .replace(' ', "-");
    schema_version_for(
        "agent-hook",
        if command.is_empty() { "root" } else { &command },
        1,
    )
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
        Capability::SessionCoordination { .. } => "agent-session.coordination.v1",
        Capability::ExecutionReadOnly { .. } => "execution.read-only.v1",
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

use serde::Serialize;

use crate::backend;
use crate::cli::{BackendCommand, Cli, CommandGroup, JournalCommand, OutputFormat};
use crate::commands;
use crate::error::CliError;
use crate::journal;
use crate::lock::PeekabooLock;
use crate::model::SuccessEnvelope;

pub fn command_label(cli: &Cli) -> &'static str {
    match &cli.command {
        CommandGroup::Backend { command } => match command {
            BackendCommand::Install(_) => "backend.install",
            BackendCommand::Status(_) => "backend.status",
            BackendCommand::Verify(_) => "backend.verify",
            BackendCommand::Rollback(_) => "backend.rollback",
        },
        CommandGroup::Doctor(_) => "doctor",
        CommandGroup::Capabilities(_) => "capabilities",
        CommandGroup::Exec(_) => "exec",
        CommandGroup::Scenario(_) => "scenario",
        CommandGroup::Mcp(_) => "mcp",
        CommandGroup::Journal { command } => match command {
            JournalCommand::Summarize(_) => "journal.summarize",
            JournalCommand::Review(_) => "journal.review",
            JournalCommand::ReplayPlan(_) => "journal.replay-plan",
            JournalCommand::ReplayStep(_) => "journal.replay-step",
        },
        CommandGroup::Completion(_) => "completion",
        CommandGroup::Remote => "remote",
        CommandGroup::RemoteMcp(_) => "remote-mcp",
        CommandGroup::RemoteCleanup(_) => "remote-cleanup",
    }
}

pub(crate) fn capability_report(strict: bool) -> Result<serde_json::Value, CliError> {
    let lock = PeekabooLock::embedded()?;
    let live = strict
        .then(|| backend::doctor(true))
        .transpose()?
        .map(|value| {
            serde_json::to_value(value)
                .map_err(|_| CliError::upstream("failed to encode capability verification"))
        })
        .transpose()?;
    Ok(serde_json::json!({
        "backend": {"tag": lock.tag, "minimum_macos": lock.minimum_macos},
        "transport": ["local", "ssh"],
        "interfaces": ["exec", "scenario", "mcp_stdio"],
        "runtime": ["app", "daemon", "auto", "process"],
        "tool_profiles": ["observe", "interact", "extended"],
        "disabled": crate::policy::disabled_capabilities(),
        "live": live,
    }))
}

pub fn run(cli: Cli) -> Result<u8, CliError> {
    let command = command_label(&cli);
    match cli.command {
        CommandGroup::Backend { command } => match command {
            BackendCommand::Install(args) => {
                if let Some(host) = args.host.as_deref() {
                    return crate::transport::run_remote_control(
                        host,
                        crate::transport::RemoteCommand::Backend {
                            action: crate::transport::BackendAction::Install,
                            dry_run: args.dry_run,
                            strict: args.strict,
                        },
                        cli.format,
                        "backend.install",
                    );
                }
                emit(
                    cli.format,
                    command_label_for_backend("install"),
                    backend::install(args.dry_run, args.strict)?,
                )
            }
            BackendCommand::Status(args) => {
                if let Some(host) = args.host.as_deref() {
                    return crate::transport::run_remote_control(
                        host,
                        crate::transport::RemoteCommand::Backend {
                            action: crate::transport::BackendAction::Status,
                            dry_run: false,
                            strict: false,
                        },
                        cli.format,
                        "backend.status",
                    );
                }
                emit(
                    cli.format,
                    command_label_for_backend("status"),
                    backend::status(false)?,
                )
            }
            BackendCommand::Verify(args) => {
                if let Some(host) = args.host.as_deref() {
                    return crate::transport::run_remote_control(
                        host,
                        crate::transport::RemoteCommand::Backend {
                            action: crate::transport::BackendAction::Verify,
                            dry_run: false,
                            strict: args.strict,
                        },
                        cli.format,
                        "backend.verify",
                    );
                }
                emit(
                    cli.format,
                    command_label_for_backend("verify"),
                    backend::verify(args.strict)?,
                )
            }
            BackendCommand::Rollback(args) => {
                if let Some(host) = args.host.as_deref() {
                    return crate::transport::run_remote_control(
                        host,
                        crate::transport::RemoteCommand::Backend {
                            action: crate::transport::BackendAction::Rollback,
                            dry_run: args.dry_run,
                            strict: args.strict,
                        },
                        cli.format,
                        "backend.rollback",
                    );
                }
                emit(
                    cli.format,
                    command_label_for_backend("rollback"),
                    backend::rollback(args.dry_run, args.strict)?,
                )
            }
        },
        CommandGroup::Doctor(args) => {
            if let Some(host) = args.host.as_deref() {
                return crate::transport::run_remote_control(
                    host,
                    crate::transport::RemoteCommand::Doctor {
                        strict: args.strict,
                    },
                    cli.format,
                    "doctor",
                );
            }
            let report = backend::doctor(args.strict)?;
            let exit_code = if args.strict && !report.ready { 77 } else { 0 };
            emit_with_exit(cli.format, command, report, exit_code)
        }
        CommandGroup::Capabilities(args) => {
            if let Some(host) = args.host.as_deref() {
                return crate::transport::run_remote_control(
                    host,
                    crate::transport::RemoteCommand::Capabilities {
                        strict: args.strict,
                    },
                    cli.format,
                    "capabilities",
                );
            }
            emit(cli.format, command, capability_report(args.strict)?)
        }
        CommandGroup::Exec(args) => {
            if args.host.is_some() {
                return crate::transport::run_remote_exec(&args, cli.format);
            }
            let outcome = commands::exec::run_local(&args, None, "local")?;
            emit_with_exit(cli.format, command, outcome.result, outcome.exit_code)
        }
        CommandGroup::Scenario(args) => {
            if args.host.is_some() {
                return crate::transport::run_remote_scenario(&args, cli.format);
            }
            let outcome = commands::scenario::run_local(&args, "local")?;
            emit_with_exit(cli.format, command, outcome.result, outcome.exit_code)
        }
        CommandGroup::Mcp(args) => {
            if args.host.is_some() {
                crate::transport::run_remote_mcp(&args)
            } else {
                commands::mcp::run_local(&args, "local")
            }
        }
        CommandGroup::Journal { command } => match command {
            JournalCommand::Summarize(args) => emit(
                cli.format,
                "journal.summarize",
                journal::summarize(&args.out_dir)?,
            ),
            JournalCommand::Review(args) => emit(
                cli.format,
                "journal.review",
                journal::review(&args.out_dir)?,
            ),
            JournalCommand::ReplayPlan(args) => emit(
                cli.format,
                "journal.replay-plan",
                journal::replay_plan(&args.out_dir, args.step.as_deref())?,
            ),
            JournalCommand::ReplayStep(args) => {
                let request = journal::prepare_replay(
                    &args.out_dir,
                    &args.step,
                    args.confirm_conditional,
                    args.current_snapshot.as_deref(),
                    args.expected.as_deref(),
                )?;
                let exec = crate::cli::ExecArgs {
                    host: None,
                    out_dir: args.out_dir,
                    intent: request.intent,
                    expected: request.expected,
                    evidence_mode: request.evidence_mode,
                    runtime: request.runtime,
                    timeout_seconds: 60,
                    argv: request.argv,
                };
                let outcome = commands::exec::run_local(&exec, Some(request.parent_id), "local")?;
                emit_with_exit(
                    cli.format,
                    "journal.replay-step",
                    outcome.result,
                    outcome.exit_code,
                )
            }
        },
        CommandGroup::Completion(_) => Ok(0),
        CommandGroup::Remote => crate::transport::run_remote_endpoint(),
        CommandGroup::RemoteMcp(args) => crate::transport::run_remote_mcp_endpoint(&args.token),
        CommandGroup::RemoteCleanup(args) => {
            crate::transport::run_remote_cleanup_endpoint(&args.token)
        }
    }
}

fn command_label_for_backend(operation: &str) -> &'static str {
    match operation {
        "install" => "backend.install",
        "status" => "backend.status",
        "verify" => "backend.verify",
        "rollback" => "backend.rollback",
        _ => "backend",
    }
}

fn emit<T: Serialize>(
    format: OutputFormat,
    command: &'static str,
    result: T,
) -> Result<u8, CliError> {
    emit_with_exit(format, command, result, 0)
}

fn emit_with_exit<T: Serialize>(
    format: OutputFormat,
    command: &'static str,
    result: T,
    exit_code: u8,
) -> Result<u8, CliError> {
    let envelope = SuccessEnvelope::new(command, result);
    let body = match format {
        OutputFormat::Json => serde_json::to_string(&envelope),
        OutputFormat::Text => serde_json::to_string_pretty(&envelope),
    }
    .map_err(|_| CliError::upstream("failed to encode command response"))?;
    println!("{body}");
    Ok(exit_code)
}

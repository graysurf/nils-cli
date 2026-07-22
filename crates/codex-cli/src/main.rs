mod cli;
mod completion;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use codex_cli::{agent, auth, config};
use nils_common::cli_contract::exit;

fn main() {
    let exit_code = run();
    std::process::exit(exit_code);
}

fn run() -> i32 {
    if std::env::args_os().nth(1).is_none() {
        let mut cmd = cli::Cli::command();
        if cmd.print_help().is_ok() {
            println!();
            return exit::SUCCESS;
        }
        return exit::RUNTIME;
    }

    let cli = match cli::Cli::try_parse_from(std::env::args()) {
        Ok(cli) => cli,
        Err(err) => {
            let code = match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => exit::SUCCESS,
                _ => exit::USAGE,
            };
            let _ = err.print();
            return code;
        }
    };

    match cli.command {
        Some(command) => match command {
            cli::Command::Agent(args) => handle_agent(&args),
            cli::Command::Auth(args) => handle_auth(&args),
            cli::Command::Diag(args) => handle_diag(&args),
            cli::Command::Config(args) => handle_config(&args),
            cli::Command::PromptSegment(args) => handle_prompt_segment(&args),
            cli::Command::Completion(args) => handle_completion(&args),
        },
        None => {
            let mut cmd = cli::Cli::command();
            if cmd.print_help().is_ok() {
                println!();
                return 0;
            }
            1
        }
    }
}

fn handle_agent(args: &cli::AgentArgs) -> i32 {
    match &args.command {
        Some(cli::AgentCommand::Prompt {
            runtime,
            prompt,
            ephemeral,
        }) => agent::prompt_with_runtime_options(
            prompt,
            codex_cli::runtime::AgentRuntimeOptions {
                runtime: *runtime,
                ephemeral: *ephemeral,
            },
        ),
        Some(cli::AgentCommand::Advice {
            runtime,
            question,
            ephemeral,
        }) => agent::advice_with_runtime_options(
            question,
            codex_cli::runtime::AgentRuntimeOptions {
                runtime: *runtime,
                ephemeral: *ephemeral,
            },
        ),
        Some(cli::AgentCommand::Knowledge {
            runtime,
            concept,
            ephemeral,
        }) => agent::knowledge_with_runtime_options(
            concept,
            codex_cli::runtime::AgentRuntimeOptions {
                runtime: *runtime,
                ephemeral: *ephemeral,
            },
        ),
        Some(cli::AgentCommand::Commit {
            runtime,
            push,
            auto_stage,
            ephemeral,
            extra,
        }) => {
            let options = agent::commit::CommitOptions {
                runtime: *runtime,
                push: *push,
                auto_stage: *auto_stage,
                ephemeral: *ephemeral,
                extra: extra.clone(),
            };
            agent::commit::run(&options).unwrap_or(1)
        }
        Some(cli::AgentCommand::Resume { session_id, cd }) => {
            agent::resume::run(&agent::resume::ResumeOptions {
                session_id: session_id.clone(),
                cwd: cd.clone(),
            })
        }
        Some(cli::AgentCommand::Doctor { output }) => {
            codex_cli::runtime::doctor_isolated(output.is_json())
        }
        None => print_subcommand_help("agent"),
    }
}

fn handle_auth(args: &cli::AuthArgs) -> i32 {
    match &args.command {
        Some(cli::AuthCommand::Login {
            output,
            api_key,
            device_code,
        }) => auth::login::run_with_json(*api_key, *device_code, output.is_json()).unwrap_or(1),
        Some(cli::AuthCommand::Use { output, args }) => {
            if args.len() != 1 || args[0].is_empty() {
                eprintln!("codex-use: usage: codex-use <name|name.json|email>");
                return 64;
            }
            auth::use_secret::run_with_json(&args[0], output.is_json()).unwrap_or(1)
        }
        Some(cli::AuthCommand::Save { output, yes, args }) => {
            if args.len() != 1 || args[0].is_empty() {
                eprintln!("codex-save: usage: codex-save [--yes] <secret|secret.json>");
                return 64;
            }
            auth::save::run_with_json(&args[0], *yes, output.is_json()).unwrap_or(1)
        }
        Some(cli::AuthCommand::Remove { output, yes, args }) => {
            if args.len() != 1 || args[0].is_empty() {
                eprintln!("codex-remove: usage: codex-remove [--yes] <secret|secret.json>");
                return 64;
            }
            auth::remove::run_with_json(&args[0], *yes, output.is_json()).unwrap_or(1)
        }
        Some(cli::AuthCommand::Refresh { output, args }) => {
            if args.len() > 1 {
                eprintln!("codex-refresh: usage: codex-refresh-auth [secret.json]");
                return 64;
            }
            auth::refresh::run_with_json(args, output.is_json()).unwrap_or(1)
        }
        Some(cli::AuthCommand::AutoRefresh { output }) => {
            auth::auto_refresh::run_with_json(output.is_json()).unwrap_or(1)
        }
        Some(cli::AuthCommand::Status { output }) => {
            auth::status::run_with_json(output.is_json()).unwrap_or(1)
        }
        Some(cli::AuthCommand::Current { output }) => {
            auth::current::run_with_json(output.is_json()).unwrap_or(1)
        }
        Some(cli::AuthCommand::Sync { output }) => {
            auth::sync::run_with_json(output.is_json()).unwrap_or(1)
        }
        Some(cli::AuthCommand::Remote(remote_args)) => handle_auth_remote(remote_args),
        None => print_subcommand_help("auth"),
    }
}

fn handle_auth_remote(args: &cli::AuthRemoteArgs) -> i32 {
    match &args.command {
        Some(cli::AuthRemoteCommand::Pull {
            output,
            ssh,
            name,
            access_only,
            write_active,
            refresh,
        }) => auth::remote::pull_with_json(
            ssh.as_deref(),
            name.as_deref(),
            *access_only,
            *write_active,
            *refresh,
            output.is_json(),
        )
        .unwrap_or(1),
        Some(cli::AuthRemoteCommand::Export {
            name,
            access_only,
            refresh,
        }) => auth::remote::export(name, *access_only, *refresh).unwrap_or(1),
        None => print_subcommand_help("auth remote"),
    }
}

fn handle_diag(args: &cli::DiagArgs) -> i32 {
    match &args.command {
        Some(cli::DiagCommand::RateLimits(rate_args)) => {
            let output_json =
                rate_args.json || matches!(rate_args.format, Some(cli::OutputFormat::Json));
            let options = codex_cli::rate_limits::RateLimitsOptions {
                clear_cache: rate_args.clear_cache,
                debug: rate_args.debug,
                cached: rate_args.cached,
                no_refresh_auth: rate_args.no_refresh_auth,
                json: output_json,
                one_line: rate_args.one_line,
                all: rate_args.all,
                async_mode: rate_args.async_mode,
                watch: rate_args.watch,
                jobs: rate_args.jobs.clone(),
                secret: rate_args.secret.clone(),
            };
            codex_cli::rate_limits::run(&options).unwrap_or(1)
        }
        None => print_subcommand_help("diag"),
    }
}

fn handle_config(args: &cli::ConfigArgs) -> i32 {
    match &args.command {
        Some(cli::ConfigCommand::Show) => config::show(),
        Some(cli::ConfigCommand::Set { key, value }) => config::set(key, value),
        None => print_subcommand_help("config"),
    }
}

fn handle_prompt_segment(args: &cli::PromptSegmentArgs) -> i32 {
    if args.is_enabled {
        return codex_cli::prompt_segment::check();
    }

    match &args.command {
        Some(cli::PromptSegmentCommand::Check) => return codex_cli::prompt_segment::check(),
        Some(cli::PromptSegmentCommand::Status { output }) => {
            return codex_cli::prompt_segment::status(output.is_json());
        }
        None => {}
    }

    let options = codex_cli::prompt_segment::PromptSegmentOptions {
        no_5h: args.no_5h,
        ttl: args.ttl.clone(),
        time_format: args.time_format.clone(),
        show_timezone: args.show_timezone,
        refresh: args.refresh,
    };
    codex_cli::prompt_segment::run(&options)
}

fn handle_completion(args: &cli::CompletionArgs) -> i32 {
    completion::run(args.shell)
}

fn print_subcommand_help(name: &str) -> i32 {
    let mut cmd = cli::Cli::command();
    if let Some(subcommand) = cmd.find_subcommand_mut(name)
        && subcommand.print_help().is_ok()
    {
        println!();
        return 0;
    }
    1
}

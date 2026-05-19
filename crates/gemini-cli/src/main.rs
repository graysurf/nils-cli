mod cli;
mod completion;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use gemini_cli::{agent, auth, config};
use nils_common::cli_contract::exit;

fn main() {
    let exit_code = run();
    std::process::exit(exit_code);
}

fn run() -> i32 {
    if std::env::args_os().nth(1).is_none() {
        return print_root_help();
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
        None => print_root_help(),
    }
}

fn handle_agent(args: &cli::AgentArgs) -> i32 {
    match &args.command {
        Some(cli::AgentCommand::Prompt { prompt }) => agent::prompt(prompt),
        Some(cli::AgentCommand::Advice { question }) => agent::advice(question),
        Some(cli::AgentCommand::Knowledge { concept }) => agent::knowledge(concept),
        Some(cli::AgentCommand::Commit {
            push,
            auto_stage,
            extra,
        }) => {
            let options = agent::commit::CommitOptions {
                push: *push,
                auto_stage: *auto_stage,
                extra: extra.clone(),
            };
            agent::commit::run(&options)
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
        }) => auth::login::run_with_json(*api_key, *device_code, output.is_json()),
        Some(cli::AuthCommand::Use { output, args }) => {
            if args.len() != 1 || args[0].is_empty() {
                eprintln!("gemini-use: usage: gemini-use <name|name.json|email>");
                return 64;
            }
            auth::use_secret::run_with_json(&args[0], output.is_json())
        }
        Some(cli::AuthCommand::Save { output, yes, args }) => {
            if args.len() != 1 || args[0].is_empty() {
                eprintln!("gemini-save: usage: gemini-save [--yes] <secret.json>");
                return 64;
            }
            auth::save::run_with_json(&args[0], *yes, output.is_json())
        }
        Some(cli::AuthCommand::Remove { output, yes, args }) => {
            if args.len() != 1 || args[0].is_empty() {
                eprintln!("gemini-remove: usage: gemini-remove [--yes] <secret.json>");
                return 64;
            }
            auth::remove::run_with_json(&args[0], *yes, output.is_json())
        }
        Some(cli::AuthCommand::Refresh { output, args }) => {
            if args.len() > 1 {
                eprintln!("gemini-refresh: usage: gemini-refresh-auth [secret.json]");
                return 64;
            }
            auth::refresh::run_with_json(args, output.is_json())
        }
        Some(cli::AuthCommand::AutoRefresh { output }) => {
            auth::auto_refresh::run_with_json(output.is_json())
        }
        Some(cli::AuthCommand::Current { output }) => {
            auth::current::run_with_json(output.is_json())
        }
        Some(cli::AuthCommand::Sync { output }) => auth::sync::run_with_json(output.is_json()),
        None => print_subcommand_help("auth"),
    }
}

fn handle_diag(args: &cli::DiagArgs) -> i32 {
    match &args.command {
        Some(cli::DiagCommand::RateLimits(rate_args)) => {
            let output_json =
                rate_args.json || matches!(rate_args.format, Some(cli::OutputFormat::Json));
            let options = gemini_cli::rate_limits::RateLimitsOptions {
                clear_cache: rate_args.clear_cache,
                debug: rate_args.debug,
                cached: rate_args.cached,
                no_refresh_auth: rate_args.no_refresh_auth,
                json: output_json,
                one_line: rate_args.one_line,
                all: rate_args.all,
                async_mode: rate_args.async_mode,
                jobs: rate_args.jobs.clone(),
                secret: rate_args.secret.clone(),
            };
            gemini_cli::rate_limits::run(&options)
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
    let options = gemini_cli::prompt_segment::PromptSegmentOptions {
        no_5h: args.no_5h,
        ttl: args.ttl.clone(),
        time_format: args.time_format.clone(),
        show_timezone: args.show_timezone,
        refresh: args.refresh,
        is_enabled: args.is_enabled,
    };
    gemini_cli::prompt_segment::run(&options)
}

fn handle_completion(args: &cli::CompletionArgs) -> i32 {
    completion::run(args.shell)
}

fn print_root_help() -> i32 {
    let mut command = cli::Cli::command();
    if command.print_help().is_ok() {
        println!();
        return 0;
    }
    1
}

fn print_subcommand_help(name: &str) -> i32 {
    let mut command = cli::Cli::command();
    if let Some(subcommand) = command.find_subcommand_mut(name)
        && subcommand.print_help().is_ok()
    {
        println!();
        return 0;
    }
    1
}

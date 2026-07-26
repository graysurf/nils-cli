mod cli;
mod completion;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
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
        Some(cli::Command::Agent(args)) => handle_agent(&args),
        Some(cli::Command::Auth(args)) => handle_auth(&args),
        Some(cli::Command::Config(args)) => handle_config(&args),
        Some(cli::Command::PromptSegment(args)) => handle_prompt_segment(&args),
        Some(cli::Command::Usage(args)) => handle_usage(&args),
        Some(cli::Command::Completion(args)) => completion::run(args.shell),
        None => {
            let mut cmd = cli::Cli::command();
            if cmd.print_help().is_ok() {
                println!();
                return exit::SUCCESS;
            }
            exit::RUNTIME
        }
    }
}

fn handle_agent(args: &cli::AgentArgs) -> i32 {
    match &args.command {
        Some(cli::AgentCommand::Prompt(options)) => claude_cli::agent::oneshot::run(
            &oneshot_options(claude_cli::agent::oneshot::AgentTask::Prompt, options),
        ),
        Some(cli::AgentCommand::Advice(options)) => claude_cli::agent::oneshot::run(
            &oneshot_options(claude_cli::agent::oneshot::AgentTask::Advice, options),
        ),
        Some(cli::AgentCommand::Knowledge(options)) => claude_cli::agent::oneshot::run(
            &oneshot_options(claude_cli::agent::oneshot::AgentTask::Knowledge, options),
        ),
        Some(cli::AgentCommand::Commit(options)) => {
            claude_cli::agent::commit::run(&claude_cli::agent::commit::CommitOptions {
                push: options.push,
                auto_stage: options.auto_stage,
                model: options.model.clone(),
                effort: options
                    .effort
                    .map(cli::AgentEffort::as_str)
                    .map(str::to_string),
                extra: options.extra.clone(),
            })
        }
        Some(cli::AgentCommand::Doctor { output }) => {
            claude_cli::agent::doctor::run(output.is_json())
        }
        Some(cli::AgentCommand::Resume { session_id, cd }) => {
            claude_cli::agent::resume::run(&claude_cli::agent::resume::ResumeOptions {
                session_id: session_id.clone(),
                cwd: cd.clone(),
            })
        }
        None => {
            let mut cmd = cli::Cli::command();
            if let Some(subcommand) = cmd.find_subcommand_mut("agent")
                && subcommand.print_help().is_ok()
            {
                println!();
                return exit::SUCCESS;
            }
            exit::RUNTIME
        }
    }
}

fn oneshot_options(
    task: claude_cli::agent::oneshot::AgentTask,
    options: &cli::AgentOneShotArgs,
) -> claude_cli::agent::oneshot::OneShotOptions {
    claude_cli::agent::oneshot::OneShotOptions {
        task,
        runtime: options.runtime.map(|runtime| match runtime {
            cli::AgentRuntimeMode::Safe => claude_cli::agent::oneshot::RuntimeMode::Safe,
            cli::AgentRuntimeMode::Inherited => claude_cli::agent::oneshot::RuntimeMode::Inherited,
        }),
        model: options.model.clone(),
        effort: options
            .effort
            .map(cli::AgentEffort::as_str)
            .map(str::to_string),
        ephemeral: options.ephemeral,
        input: options.input.clone(),
    }
}

fn handle_auth(args: &cli::AuthArgs) -> i32 {
    match &args.command {
        Some(cli::AuthCommand::Login {
            console,
            claudeai,
            email,
            sso,
        }) => claude_cli::auth::login(&claude_cli::auth::LoginOptions {
            console: *console,
            claudeai: *claudeai,
            email: email.clone(),
            sso: *sso,
        }),
        Some(cli::AuthCommand::Status { output }) => claude_cli::auth::status(output.is_json()),
        Some(cli::AuthCommand::Logout) => claude_cli::auth::logout(),
        None => print_subcommand_help("auth"),
    }
}

fn handle_config(args: &cli::ConfigArgs) -> i32 {
    match &args.command {
        Some(cli::ConfigCommand::Show) => claude_cli::config::show(),
        Some(cli::ConfigCommand::Set { key, value }) => claude_cli::config::set(key, value),
        None => print_subcommand_help("config"),
    }
}

fn print_subcommand_help(name: &str) -> i32 {
    let mut cmd = cli::Cli::command();
    if let Some(subcommand) = cmd.find_subcommand_mut(name)
        && subcommand.print_help().is_ok()
    {
        println!();
        return exit::SUCCESS;
    }
    exit::RUNTIME
}

fn handle_usage(args: &cli::UsageArgs) -> i32 {
    claude_cli::prompt_segment::usage::run(&claude_cli::prompt_segment::usage::UsageOptions {
        source: match args.source {
            cli::UsageSource::Auto => claude_cli::prompt_segment::usage::UsageSource::Auto,
            cli::UsageSource::Oauth => claude_cli::prompt_segment::usage::UsageSource::Oauth,
            cli::UsageSource::Cli => claude_cli::prompt_segment::usage::UsageSource::Cli,
            cli::UsageSource::Cache => claude_cli::prompt_segment::usage::UsageSource::Cache,
        },
        output_json: args.output.is_json(),
    })
}

fn handle_prompt_segment(args: &cli::PromptSegmentArgs) -> i32 {
    if args.is_enabled {
        return claude_cli::prompt_segment::check();
    }

    match &args.command {
        Some(cli::PromptSegmentCommand::Check) => claude_cli::prompt_segment::check(),
        Some(cli::PromptSegmentCommand::Status { output }) => {
            claude_cli::prompt_segment::status(output.is_json())
        }
        None => {
            claude_cli::prompt_segment::run(&claude_cli::prompt_segment::PromptSegmentOptions {
                no_5h: args.no_5h,
                ttl: args.ttl.clone(),
                time_format: args.time_format.clone(),
                show_timezone: args.show_timezone,
                refresh: args.refresh,
            })
        }
    }
}

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
                ttl: args.ttl.clone(),
                time_format: args.time_format.clone(),
                refresh: args.refresh,
            })
        }
    }
}

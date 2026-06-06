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
        Some(cli::Command::PromptSegment(args)) => handle_prompt_segment(&args),
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

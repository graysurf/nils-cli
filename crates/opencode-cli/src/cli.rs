use std::ffi::OsString;

use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use nils_common::cli_contract::exit;

use crate::{agent, completion};

const ROOT_AFTER_HELP: &str = "\
EXAMPLES:
  opencode-cli agent prompt 'Summarize this diff'
  opencode-cli agent advice 'How should I structure this CLI?'
  opencode-cli agent commit --auto-stage 'Prefer a narrow commit'
  opencode-cli completion zsh

ENVIRONMENT:
  OPENCODE_CLI_MODEL, OPENCODE_CLI_VARIANT, ZDOTDIR, ZSH_SCRIPT_DIR

EXIT CODES:
  0   success
  1   runtime error
  64  command-line usage error";

#[derive(Debug, Parser)]
#[command(
    name = "opencode-cli",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "OpenCode agent helper CLI",
    long_about = "Run OpenCode-oriented prompt helpers migrated out of zsh-kit.",
    after_help = ROOT_AFTER_HELP
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Agent command group
    Agent(AgentArgs),
    /// Export shell completion script
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: Option<AgentCommand>,
}

#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// Run a raw prompt
    Prompt {
        #[arg(value_name = "prompt", num_args = 0..)]
        prompt: Vec<String>,
    },
    /// Get actionable engineering advice
    Advice {
        #[arg(value_name = "question", num_args = 0..)]
        question: Vec<String>,
    },
    /// Get an explanation for a concept
    Knowledge {
        #[arg(value_name = "concept", num_args = 0..)]
        concept: Vec<String>,
    },
    /// Run the semantic-commit workflow
    Commit {
        /// Push after committing
        #[arg(short = 'p', long = "push")]
        push: bool,
        /// Autostage changes before committing
        #[arg(short = 'a', long = "auto-stage")]
        auto_stage: bool,
        /// Extra prompt text
        #[arg(value_name = "extra", num_args = 0..)]
        extra: Vec<String>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    /// Shell to generate completion script for
    #[arg(value_enum, value_name = "shell")]
    pub shell: CompletionShell,
}

pub fn run_from<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => return print_parse_error(err),
    };

    match cli.command {
        None => print_root_help(),
        Some(Command::Agent(args)) => handle_agent(args),
        Some(Command::Completion(args)) => completion::run(args.shell),
    }
}

fn handle_agent(args: AgentArgs) -> i32 {
    match args.command {
        None => print_subcommand_help("agent"),
        Some(AgentCommand::Prompt { prompt }) => agent::prompt(&prompt),
        Some(AgentCommand::Advice { question }) => agent::advice(&question),
        Some(AgentCommand::Knowledge { concept }) => agent::knowledge(&concept),
        Some(AgentCommand::Commit {
            push,
            auto_stage,
            extra,
        }) => {
            let options = agent::commit::CommitOptions {
                push,
                auto_stage,
                extra,
            };
            agent::commit::run(&options)
        }
    }
}

fn print_parse_error(err: clap::Error) -> i32 {
    let kind = err.kind();
    if let Err(print_err) = err.print() {
        eprintln!("{print_err}");
        return exit::RUNTIME;
    }

    if matches!(kind, ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        exit::SUCCESS
    } else {
        exit::USAGE
    }
}

fn print_root_help() -> i32 {
    let mut command = Cli::command();
    if command.print_help().is_ok() {
        println!();
        return exit::SUCCESS;
    }
    exit::RUNTIME
}

fn print_subcommand_help(name: &str) -> i32 {
    let mut command = Cli::command();
    if let Some(subcommand) = command.find_subcommand_mut(name)
        && subcommand.print_help().is_ok()
    {
        println!();
        return exit::SUCCESS;
    }
    exit::RUNTIME
}

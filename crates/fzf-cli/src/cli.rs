use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand};
use nils_common::cli_contract::exit;
use std::ffi::OsString;

use crate::{
    completion, defs, directory, file, git_branch, git_checkout, git_commit, git_status, git_tag,
    history, port, process, util,
};

#[derive(Debug, Parser)]
#[command(
    name = "fzf-cli",
    version,
    about = "Fuzzy workflow helper CLI",
    disable_help_subcommand = true,
    override_usage = "fzf-cli <command> [args]"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Search and preview text files")]
    File(RawArgs),
    #[command(about = "Search directories and cd into selection")]
    Directory(RawArgs),
    #[command(name = "git-status", about = "Interactive git status viewer")]
    GitStatus(RawArgs),
    #[command(
        name = "git-commit",
        about = "Browse commits and open changed files in editor"
    )]
    GitCommit(RawArgs),
    #[command(name = "git-checkout", about = "Pick and checkout a previous commit")]
    GitCheckout(RawArgs),
    #[command(
        name = "git-branch",
        about = "Browse and checkout branches interactively"
    )]
    GitBranch(RawArgs),
    #[command(name = "git-tag", about = "Browse and checkout tags interactively")]
    GitTag(RawArgs),
    #[command(about = "Browse and kill running processes")]
    Process(RawArgs),
    #[command(about = "Browse listening ports and owners")]
    Port(RawArgs),
    #[command(about = "Search and execute command history")]
    History(RawArgs),
    #[command(about = "Browse environment variables")]
    Env(RawArgs),
    #[command(about = "Browse shell aliases")]
    Alias(RawArgs),
    #[command(about = "Browse defined shell functions")]
    Function(RawArgs),
    #[command(about = "Browse all definitions (env, alias, functions)")]
    Def(RawArgs),
    #[command(about = "Export shell completion script")]
    Completion(RawArgs),
    #[command(about = "Display help message for fzf-cli")]
    Help,
}

#[derive(Debug, Args)]
#[command(disable_help_flag = true)]
struct RawArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

pub fn run() -> i32 {
    run_from(std::env::args())
}

fn run_from<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => return print_parse_error(err),
    };

    match cli.command {
        Some(Command::File(raw)) => run_subcommand("file", raw, file::run),
        Some(Command::Directory(raw)) => run_subcommand("directory", raw, directory::run),
        Some(Command::GitStatus(raw)) => run_subcommand("git-status", raw, git_status::run),
        Some(Command::GitCommit(raw)) => run_subcommand("git-commit", raw, git_commit::run),
        Some(Command::GitCheckout(raw)) => run_subcommand("git-checkout", raw, git_checkout::run),
        Some(Command::GitBranch(raw)) => run_subcommand("git-branch", raw, git_branch::run),
        Some(Command::GitTag(raw)) => run_subcommand("git-tag", raw, git_tag::run),
        Some(Command::Process(raw)) => run_subcommand("process", raw, process::run),
        Some(Command::Port(raw)) => run_subcommand("port", raw, port::run),
        Some(Command::History(raw)) => run_subcommand("history", raw, history::run),
        Some(Command::Env(raw)) => run_subcommand("env", raw, defs::run_env),
        Some(Command::Alias(raw)) => run_subcommand("alias", raw, defs::run_alias),
        Some(Command::Function(raw)) => run_subcommand("function", raw, defs::run_function),
        Some(Command::Def(raw)) => run_subcommand("def", raw, defs::run_def),
        Some(Command::Completion(raw)) => completion::run(&raw.args),
        Some(Command::Help) | None => print_help_stdout(),
    }
}

fn run_subcommand(command_name: &str, raw: RawArgs, handler: impl FnOnce(&[String]) -> i32) -> i32 {
    if raw.args.first().is_some_and(|arg| util::is_help(arg))
        && completion::print_subcommand_help(command_name)
    {
        return exit::SUCCESS;
    }

    handler(&raw.args)
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

fn print_help_stdout() -> i32 {
    let mut command = Cli::command();
    if let Err(err) = command.print_help() {
        eprintln!("{err}");
        return exit::RUNTIME;
    }
    println!();
    exit::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_parses_help_when_no_command() {
        let code = run_from(["fzf-cli"]);
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn clap_parses_version_flag() {
        let code = run_from(["fzf-cli", "-V"]);
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn clap_unknown_subcommand_exits_usage() {
        let code = run_from(["fzf-cli", "nope"]);
        assert_eq!(code, exit::USAGE);
    }

    #[test]
    fn clap_dispatches_subcommand_raw_args() {
        let cli =
            Cli::try_parse_from(["fzf-cli", "file", "--vi", "needle"]).expect("parse file command");

        let Some(Command::File(raw)) = cli.command else {
            panic!("expected file command");
        };
        assert_eq!(raw.args, vec!["--vi".to_string(), "needle".to_string()]);
    }
}

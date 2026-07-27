use crate::{commit, completion, default_branch, staged_context};
use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand};
use nils_common::cli_contract::exit;

#[derive(Debug, Parser)]
#[command(
    name = "semantic-commit",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Commit workflow helper with semantic commit validation",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(
        name = "staged-context",
        about = "Print staged change context for commit message generation"
    )]
    StagedContext(RawArgs),
    #[command(about = "Commit staged changes with a prepared commit message")]
    Commit(RawArgs),
    #[command(
        name = "default-branch",
        about = "Create one governed signed commit on the primary checkout's default branch"
    )]
    DefaultBranch(RawArgs),
    #[command(about = "Create a fixup! commit for staged changes")]
    Fixup(RawArgs),
    #[command(about = "Create a squash! commit for staged changes")]
    Squash(RawArgs),
    #[command(about = "Export shell completion script")]
    Completion(RawArgs),
    #[command(about = "Display help message")]
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
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => return print_parse_error(err),
    };

    match cli.command {
        Some(Command::StagedContext(raw)) => staged_context::run(&raw.args),
        Some(Command::Commit(raw)) => commit::run(&raw.args),
        Some(Command::DefaultBranch(raw)) => default_branch::run(&raw.args),
        Some(Command::Fixup(raw)) => commit::run_fixup(&raw.args),
        Some(Command::Squash(raw)) => commit::run_squash(&raw.args),
        Some(Command::Completion(raw)) => completion::run(&raw.args),
        Some(Command::Help) | None => print_help_stdout(),
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
        let code = run_from(["semantic-commit"]);
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn clap_parses_version_flag() {
        let code = run_from(["semantic-commit", "-V"]);
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn clap_unknown_subcommand_exits_usage() {
        let code = run_from(["semantic-commit", "nope"]);
        assert_eq!(code, exit::USAGE);
    }

    #[test]
    fn clap_dispatches_subcommand_raw_args() {
        let cli = Cli::try_parse_from([
            "semantic-commit",
            "commit",
            "--message",
            "feat: test",
            "--validate-only",
        ])
        .expect("parse commit command");

        let Some(Command::Commit(raw)) = cli.command else {
            panic!("expected commit command");
        };
        assert_eq!(
            raw.args,
            ["--message", "feat: test", "--validate-only"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn clap_dispatches_fixup_subcommand_raw_args() {
        let cli = Cli::try_parse_from(["semantic-commit", "fixup", "--target", "HEAD"])
            .expect("parse fixup command");

        let Some(Command::Fixup(raw)) = cli.command else {
            panic!("expected fixup command");
        };
        assert_eq!(
            raw.args,
            ["--target", "HEAD"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }
}

use crate::{
    artifact_audit, batches, completion, ledger_sync, ledger_update, scaffold, spec, validate,
};
use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand};
use nils_common::cli_contract::exit;

#[derive(Debug, Parser)]
#[command(
    name = "plan-tooling",
    version,
    about = "Plan tooling CLI",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(
        name = "to-json",
        about = "Parse a plan markdown file into a stable JSON schema"
    )]
    ToJson(RawArgs),
    #[command(about = "Lint plan markdown files")]
    Validate(RawArgs),
    #[command(about = "Compute dependency layers (parallel batches) for a sprint")]
    Batches(RawArgs),
    #[command(
        name = "artifact-audit",
        about = "Classify durable coordination artifacts without side effects"
    )]
    ArtifactAudit(RawArgs),
    #[command(
        name = "split-prs",
        about = "Build task-to-PR split records (deterministic/auto)"
    )]
    SplitPrs(RawArgs),
    #[command(
        name = "ledger-update",
        about = "Patch one row in an execution-state `## Task Ledger` table"
    )]
    LedgerUpdate(RawArgs),
    #[command(
        name = "ledger-sync",
        about = "Reconcile a ledger Evidence column against tracking-issue evidence"
    )]
    LedgerSync(RawArgs),
    #[command(about = "Create a new plan from template")]
    Scaffold(RawArgs),
    #[command(about = "Dump the validate catalog (class, pattern, rule, example)")]
    Spec(RawArgs),
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

pub(crate) fn print_version_stdout() {
    println!("plan-tooling {}", env!("CARGO_PKG_VERSION"));
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
        Some(Command::ToJson(raw)) => crate::parse::to_json::run(&raw.args),
        Some(Command::Validate(raw)) => validate::run(&raw.args),
        Some(Command::Batches(raw)) => batches::run(&raw.args),
        Some(Command::ArtifactAudit(raw)) => artifact_audit::run(&raw.args),
        Some(Command::SplitPrs(raw)) => crate::split_prs::run(&raw.args),
        Some(Command::LedgerUpdate(raw)) => ledger_update::run(&raw.args),
        Some(Command::LedgerSync(raw)) => ledger_sync::run(&raw.args),
        Some(Command::Scaffold(raw)) => scaffold::run(&raw.args),
        Some(Command::Spec(raw)) => spec::run(&raw.args),
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
        let code = run_from(["plan-tooling"]);
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn clap_parses_version_flag() {
        let code = run_from(["plan-tooling", "-V"]);
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn clap_unknown_subcommand_exits_usage() {
        let code = run_from(["plan-tooling", "nope"]);
        assert_eq!(code, exit::USAGE);
    }

    #[test]
    fn clap_dispatches_subcommand_raw_args() {
        let cli = Cli::try_parse_from([
            "plan-tooling",
            "validate",
            "--file",
            "docs/plans/example.md",
            "--format",
            "json",
        ])
        .expect("parse validate command");

        let Some(Command::Validate(raw)) = cli.command else {
            panic!("expected validate command");
        };
        assert_eq!(
            raw.args,
            ["--file", "docs/plans/example.md", "--format", "json"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }
}

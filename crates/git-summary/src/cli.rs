use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand};
use nils_common::cli_contract::exit;
use std::ffi::OsString;

use crate::dates::{
    last_month_range, last_week_range, this_month_range, this_week_range, today_range,
    yesterday_range,
};
use crate::git::require_git;
use crate::summary::summary;

#[derive(Debug, Parser)]
#[command(
    name = "git-summary",
    version,
    about = "Git history summary CLI",
    disable_help_subcommand = true,
    override_usage = "git-summary <command> [args]",
    after_help = "Custom date range:\n  <from> <to>       Custom date range (YYYY-MM-DD)"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Entire history")]
    All,
    #[command(about = "Today only")]
    Today,
    #[command(about = "Yesterday only")]
    Yesterday,
    #[command(name = "this-month", about = "1st to today")]
    ThisMonth,
    #[command(name = "last-month", about = "1st to end of last month")]
    LastMonth,
    #[command(name = "this-week", about = "This Mon-Sun")]
    ThisWeek,
    #[command(name = "last-week", about = "Last Mon-Sun")]
    LastWeek,
    #[command(about = "Export shell completion script")]
    Completion(RawArgs),
    #[command(about = "Display help message for git-summary")]
    Help,
    #[command(external_subcommand)]
    Custom(Vec<OsString>),
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
        Some(Command::All) => run_git_summary_for_label("all commits", None, None),
        Some(Command::Today) => {
            let range = today_range();
            run_git_summary_for_label(&range.label, Some(&range.start), Some(&range.end))
        }
        Some(Command::Yesterday) => {
            let range = yesterday_range();
            run_git_summary_for_label(&range.label, Some(&range.start), Some(&range.end))
        }
        Some(Command::ThisMonth) => {
            let range = this_month_range();
            run_git_summary_for_label(&range.label, Some(&range.start), Some(&range.end))
        }
        Some(Command::LastMonth) => {
            let range = last_month_range();
            run_git_summary_for_label(&range.label, Some(&range.start), Some(&range.end))
        }
        Some(Command::ThisWeek) => {
            let range = this_week_range();
            run_git_summary_for_label(&range.label, Some(&range.start), Some(&range.end))
        }
        Some(Command::LastWeek) => {
            let range = last_week_range();
            run_git_summary_for_label(&range.label, Some(&range.start), Some(&range.end))
        }
        Some(Command::Completion(raw)) => crate::completion::run(&raw.args),
        Some(Command::Help) | None => print_help_stdout(),
        Some(Command::Custom(args)) => run_custom_range(args),
    }
}

fn run_git_summary_for_label(label: &str, from: Option<&str>, to: Option<&str>) -> i32 {
    if let Err(msg) = require_git() {
        println!("{msg}");
        return exit::RUNTIME;
    }
    print_header(label);
    summary(from, to)
}

fn run_custom_range(args: Vec<OsString>) -> i32 {
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    if args.len() >= 2 {
        if let Err(msg) = require_git() {
            println!("{msg}");
            return exit::RUNTIME;
        }
        return summary(Some(&args[0]), Some(&args[1]));
    }

    println!("❌ Invalid usage. Try: git-summary help");
    exit::USAGE
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

pub fn print_header(label: &str) {
    println!();
    println!("📅 Git summary for {label}");
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_parses_help_when_no_command() {
        let code = run_from(["git-summary"]);
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn clap_parses_version_flag() {
        let code = run_from(["git-summary", "-V"]);
        assert_eq!(code, exit::SUCCESS);
    }

    #[test]
    fn clap_dispatches_custom_date_range() {
        let cli = Cli::try_parse_from(["git-summary", "2024-01-01", "2024-01-31"])
            .expect("parse custom range");

        let Some(Command::Custom(args)) = cli.command else {
            panic!("expected custom range");
        };
        assert_eq!(
            args,
            vec![OsString::from("2024-01-01"), OsString::from("2024-01-31")]
        );
    }

    #[test]
    fn clap_unknown_single_token_keeps_usage_exit() {
        let code = run_from(["git-summary", "nope"]);
        assert_eq!(code, exit::USAGE);
    }
}

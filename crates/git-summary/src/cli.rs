use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand};
use nils_common::cli_contract::{
    Envelope, EnvelopeError, OutputFormat, emit_parse_error, exit, schema_version_for,
};
use std::ffi::OsString;

use crate::dates::{
    last_month_range, last_week_range, this_month_range, this_week_range, today_range,
    yesterday_range,
};
use crate::git::require_git;
use crate::summary::{SummaryFailure, SummaryPayload, collect_summary, render_text};

const BINARY: &str = "git-summary";

#[derive(Debug, Parser)]
#[command(
    name = "git-summary",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Git history summary CLI",
    disable_help_subcommand = true,
    override_usage = "git-summary <command> [args]",
    after_help = "Custom date range:\n  <from> <to>       Custom date range (YYYY-MM-DD)"
)]
struct Cli {
    /// Output format
    #[arg(long, value_enum, global = true, default_value_t = OutputFormat::Text)]
    format: OutputFormat,

    /// Show raw commit identities instead of canonical mailmap identities
    #[arg(long, global = true)]
    no_mailmap: bool,

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
    run_from(std::env::args_os())
}

pub fn run_embedded(args: &[String]) -> i32 {
    let argv = std::iter::once(OsString::from(BINARY))
        .chain(args.iter().map(OsString::from))
        .collect::<Vec<_>>();
    run_from(argv)
}

pub fn run_from<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let argv = args.into_iter().map(Into::into).collect::<Vec<OsString>>();
    let cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(err) => return print_parse_error(err, &argv),
    };
    let format = cli.format;
    let use_mailmap = !cli.no_mailmap;

    match cli.command {
        Some(Command::All) => {
            run_git_summary_for_label("all commits", None, None, format, use_mailmap, true)
        }
        Some(Command::Today) => {
            let range = today_range();
            run_git_summary_for_label(
                &range.label,
                Some(&range.start),
                Some(&range.end),
                format,
                use_mailmap,
                true,
            )
        }
        Some(Command::Yesterday) => {
            let range = yesterday_range();
            run_git_summary_for_label(
                &range.label,
                Some(&range.start),
                Some(&range.end),
                format,
                use_mailmap,
                true,
            )
        }
        Some(Command::ThisMonth) => {
            let range = this_month_range();
            run_git_summary_for_label(
                &range.label,
                Some(&range.start),
                Some(&range.end),
                format,
                use_mailmap,
                true,
            )
        }
        Some(Command::LastMonth) => {
            let range = last_month_range();
            run_git_summary_for_label(
                &range.label,
                Some(&range.start),
                Some(&range.end),
                format,
                use_mailmap,
                true,
            )
        }
        Some(Command::ThisWeek) => {
            let range = this_week_range();
            run_git_summary_for_label(
                &range.label,
                Some(&range.start),
                Some(&range.end),
                format,
                use_mailmap,
                true,
            )
        }
        Some(Command::LastWeek) => {
            let range = last_week_range();
            run_git_summary_for_label(
                &range.label,
                Some(&range.start),
                Some(&range.end),
                format,
                use_mailmap,
                true,
            )
        }
        Some(Command::Completion(raw)) => crate::completion::run(&raw.args),
        Some(Command::Help) | None => print_help_stdout(),
        Some(Command::Custom(args)) => run_custom_range(args, format, use_mailmap),
    }
}

fn run_git_summary_for_label(
    label: &str,
    from: Option<&str>,
    to: Option<&str>,
    format: OutputFormat,
    use_mailmap: bool,
    show_text_header: bool,
) -> i32 {
    if let Err(msg) = require_git() {
        return emit_error(format, "git-context-unavailable", msg, exit::RUNTIME);
    }
    if format.is_text() && show_text_header {
        print_header(label);
    }
    match collect_summary(label, from, to, use_mailmap) {
        Ok(payload) => emit_success(format, &payload),
        Err(failure) => emit_summary_failure(format, failure),
    }
}

fn run_custom_range(args: Vec<OsString>, format: OutputFormat, use_mailmap: bool) -> i32 {
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    if args.len() >= 2 {
        let label = format!("custom range: {} to {}", args[0], args[1]);
        return run_git_summary_for_label(
            &label,
            Some(&args[0]),
            Some(&args[1]),
            format,
            use_mailmap,
            false,
        );
    }

    emit_error(
        format,
        "invalid-arguments",
        "❌ Invalid usage. Try: git-summary help",
        exit::USAGE,
    )
}

fn print_parse_error(err: clap::Error, argv: &[OsString]) -> i32 {
    let kind = err.kind();
    if !matches!(kind, ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        let format = detect_format_from_argv(argv);
        if format.is_json() {
            let code = if matches!(kind, ErrorKind::InvalidSubcommand) {
                "unknown-subcommand"
            } else {
                "parse-error"
            };
            return emit_parse_error(BINARY, format, code, &render_clap_message(&err));
        }
    }
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

fn detect_format_from_argv(argv: &[OsString]) -> OutputFormat {
    let mut iter = argv.iter().map(|arg| arg.to_string_lossy());
    while let Some(arg) = iter.next() {
        if arg == "--format" {
            if let Some(value) = iter.next()
                && value.eq_ignore_ascii_case("json")
            {
                return OutputFormat::Json;
            }
        } else if let Some(value) = arg.strip_prefix("--format=")
            && value.eq_ignore_ascii_case("json")
        {
            return OutputFormat::Json;
        }
    }
    OutputFormat::Text
}

fn render_clap_message(err: &clap::Error) -> String {
    err.to_string()
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| {
            let line = line.trim();
            line.strip_prefix("error:")
                .map(str::trim)
                .unwrap_or(line)
                .to_string()
        })
        .unwrap_or_else(|| "command-line parse failed".to_string())
}

fn emit_success(format: OutputFormat, payload: &SummaryPayload) -> i32 {
    match format {
        OutputFormat::Text => {
            render_text(payload);
            exit::SUCCESS
        }
        OutputFormat::Json => {
            let envelope = Envelope::success(schema_version_for(BINARY, "summary", 1), payload);
            match serde_json::to_string(&envelope) {
                Ok(serialized) => {
                    println!("{serialized}");
                    exit::SUCCESS
                }
                Err(err) => {
                    eprintln!("git-summary: failed to serialize JSON output: {err}");
                    exit::SOFTWARE
                }
            }
        }
    }
}

fn emit_summary_failure(format: OutputFormat, failure: SummaryFailure) -> i32 {
    emit_error(format, failure.code, &failure.message, failure.exit_code)
}

fn emit_error(format: OutputFormat, code: &str, message: &str, exit_code: i32) -> i32 {
    match format {
        OutputFormat::Text => println!("{message}"),
        OutputFormat::Json => {
            let envelope: Envelope<()> = Envelope::failure(
                schema_version_for(BINARY, "summary", 1),
                EnvelopeError::new(code, message),
            );
            match serde_json::to_string(&envelope) {
                Ok(serialized) => println!("{serialized}"),
                Err(err) => {
                    eprintln!("git-summary: failed to serialize JSON error: {err}");
                    return exit::SOFTWARE;
                }
            }
        }
    }
    exit_code
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

use std::ffi::OsString;

use clap::{Parser, error::ErrorKind};
use nils_common::cli_contract::{OutputFormat, emit_parse_error, exit};

use crate::cli::{Cli, MemoCommand};
use crate::errors::AppError;

const BINARY: &str = "memo";

pub fn run() -> i32 {
    run_with_args(std::env::args_os())
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    // Materialize argv so we can both feed clap and peek for `--format json` /
    // `--json` when clap rejects parsing.
    let argv: Vec<OsString> = args.into_iter().map(Into::into).collect();

    let cli = match Cli::try_parse_from(argv.clone()) {
        Ok(cli) => cli,
        Err(err) => {
            let kind = err.kind();
            if matches!(
                kind,
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                err.exit();
            }

            let format = detect_format_from_argv(&argv);
            let code = match kind {
                ErrorKind::InvalidSubcommand => "unknown-subcommand",
                _ => "parse-error",
            };
            let message = render_clap_message(&err);
            return emit_parse_error(BINARY, format, code, &message);
        }
    };

    if let MemoCommand::Completion(args) = cli.command {
        return crate::completion::run(args.shell);
    }

    let format = cli.output_format();

    match crate::commands::run(&cli, format) {
        Ok(()) => exit::SUCCESS,
        Err(err) => report_error(&cli, format, &err),
    }
}

fn report_error(cli: &Cli, format: OutputFormat, err: &AppError) -> i32 {
    if format.is_json() {
        if let Err(output_err) = crate::output::emit_json_error(cli.schema_version(), err) {
            eprintln!("{}", output_err.message());
        }
    } else {
        eprintln!("{}", err.message());
    }

    err.exit_code()
}

fn detect_format_from_argv(argv: &[OsString]) -> OutputFormat {
    let mut iter = argv.iter().skip(1);
    while let Some(arg) = iter.next() {
        let arg = arg.to_string_lossy();
        if arg == "--json" {
            return OutputFormat::Json;
        }
        if arg == "--format"
            && let Some(next) = iter.next()
            && next.to_string_lossy().eq_ignore_ascii_case("json")
        {
            return OutputFormat::Json;
        }
        if let Some(rest) = arg.strip_prefix("--format=")
            && rest.eq_ignore_ascii_case("json")
        {
            return OutputFormat::Json;
        }
    }
    OutputFormat::Text
}

fn render_clap_message(err: &clap::Error) -> String {
    let rendered = err.to_string();
    rendered
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

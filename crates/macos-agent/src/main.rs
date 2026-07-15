use std::process::ExitCode;

use clap::{Parser, error::ErrorKind};
use macos_agent::cli::{Cli, CommandGroup, ErrorFormat};
use macos_agent::error::CliError;
use macos_agent::model::ErrorEnvelope;

fn main() -> ExitCode {
    let raw_args = std::env::args().collect::<Vec<_>>();
    let error_format = requested_error_format(&raw_args);
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                let _ = error.print();
                return ExitCode::SUCCESS;
            }
            let error = CliError::usage(error.to_string())
                .with_operation("cli.parse")
                .with_hint("Run `macos-agent --help` to inspect the supported syntax.");
            emit_error(&error, error_format);
            return ExitCode::from(error.exit_code());
        }
    };

    if let CommandGroup::Completion(args) = &cli.command {
        return ExitCode::from(macos_agent::completion::run(args.shell) as u8);
    }

    match macos_agent::run::run(cli.clone()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            emit_error(&error, cli.error_format);
            ExitCode::from(error.exit_code())
        }
    }
}

fn requested_error_format(args: &[String]) -> ErrorFormat {
    let mut arguments = args.iter().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--error-format" {
            if arguments.next().is_some_and(|value| value == "json") {
                return ErrorFormat::Json;
            }
        } else if argument.strip_prefix("--error-format=") == Some("json") {
            return ErrorFormat::Json;
        }
    }
    ErrorFormat::Text
}

fn emit_error(error: &CliError, format: ErrorFormat) {
    match format {
        ErrorFormat::Text => eprintln!("{error}"),
        ErrorFormat::Json => match serde_json::to_string(&ErrorEnvelope::from_error(error)) {
            Ok(body) => eprintln!("{body}"),
            Err(_) => eprintln!("{error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::requested_error_format;
    use macos_agent::cli::ErrorFormat;

    #[test]
    fn detects_json_parse_error_format() {
        assert_eq!(
            requested_error_format(&["macos-agent".into(), "--error-format=json".into()]),
            ErrorFormat::Json
        );
    }
}

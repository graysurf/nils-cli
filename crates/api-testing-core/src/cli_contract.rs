//! Shared CLI output contract glue for the five api-* binaries.
//!
//! All five `api-rest` / `api-gql` / `api-grpc` / `api-websocket` / `api-test`
//! binaries route their output through this module so the JSON envelope and
//! exit codes stay aligned with `nils_common::cli_contract`. Each binary's
//! tests still pin the literal `schema_version` per subcommand; this module
//! intentionally adds no new schema versions of its own.

use std::ffi::OsString;

use clap::error::ErrorKind;

pub use nils_common::cli_contract::{
    Envelope, EnvelopeError, OutputFormat, emit_parse_error, exit, schema_version_for,
};

/// Route a clap parse error through the shared output contract.
///
/// Help and version exits keep clap's native behavior; everything else lands
/// through `emit_parse_error` with raw-argv format detection so `--format json`
/// consumers see a JSON envelope on parse and unknown-subcommand errors
/// instead of clap's text-only error.
pub fn handle_parse_error<I>(binary: &str, argv: I, err: clap::Error) -> i32
where
    I: IntoIterator<Item = OsString>,
{
    let kind = err.kind();
    if matches!(
        kind,
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    ) {
        let _ = err.print();
        return err.exit_code();
    }

    let argv: Vec<OsString> = argv.into_iter().collect();
    let format = detect_format_from_argv(&argv);
    let code = match kind {
        ErrorKind::InvalidSubcommand => "unknown-subcommand",
        _ => "parse-error",
    };
    let message = render_clap_message(&err);
    emit_parse_error(binary, format, code, &message)
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

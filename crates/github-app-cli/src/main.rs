//! `github-app-cli` binary entry point.
//!
//! Implements the workspace output contract: `--format text|json`, a versioned
//! [`Envelope`], and BSD sysexits exit codes. Text mode for `token` prints the
//! raw token to stdout; JSON mode prints only non-secret metadata.

use std::process;

use clap::Parser;
use github_app_cli::cli::{AppAuthArgs, Cli, Command, InstallationsArgs, TokenArgs};
use github_app_cli::commands::installations::InstallationsPayload;
use github_app_cli::commands::token::TokenMetadata;
use github_app_cli::error::CommandError;
use github_app_cli::github::Client;
use github_app_cli::{BINARY, completion, jwt, now_unix};
use nils_common::cli_contract::{
    Envelope, EnvelopeError, OutputFormat, emit_parse_error, exit, schema_version_for,
};

fn main() {
    let cli = parse_or_exit();
    let format = cli.output_format();

    let code = match cli.command {
        Command::Completion(args) => completion::run(args.shell),
        Command::Token(args) => dispatch_token(format, &args),
        Command::Installations(args) => dispatch_installations(format, &args),
    };
    process::exit(code);
}

fn dispatch_token(format: OutputFormat, args: &TokenArgs) -> i32 {
    match mint_token(&args.auth, &args.installation_id) {
        Ok((raw_token, metadata)) => match format {
            OutputFormat::Text => {
                println!("{raw_token}");
                exit::SUCCESS
            }
            OutputFormat::Json => emit_success("token", metadata),
        },
        Err(err) => emit_failure(format, "token", &err),
    }
}

fn dispatch_installations(format: OutputFormat, args: &InstallationsArgs) -> i32 {
    match list_installations(&args.auth) {
        Ok(payload) => match format {
            OutputFormat::Text => {
                print_installations_text(&payload);
                exit::SUCCESS
            }
            OutputFormat::Json => emit_success("installations", payload),
        },
        Err(err) => emit_failure(format, "installations", &err),
    }
}

/// Mint a token, returning `(raw_token_for_text_mode, non_secret_metadata)`.
fn mint_token(
    auth: &AppAuthArgs,
    installation_id: &str,
) -> Result<(String, TokenMetadata), CommandError> {
    let jwt = make_jwt(auth)?;
    let client = Client::new(&auth.api_url)?;
    let token = client.mint_installation_token(&jwt, installation_id)?;
    let metadata = TokenMetadata::from_token(&token);
    Ok((token.token, metadata))
}

fn list_installations(auth: &AppAuthArgs) -> Result<InstallationsPayload, CommandError> {
    let jwt = make_jwt(auth)?;
    let client = Client::new(&auth.api_url)?;
    let items = client.list_installations(&jwt)?;
    Ok(InstallationsPayload::from_installations(&items))
}

fn make_jwt(auth: &AppAuthArgs) -> Result<String, CommandError> {
    let pem = load_private_key(auth)?;
    jwt::app_jwt(&auth.app_id, &pem, now_unix()).map_err(|e| {
        CommandError::data("invalid-key", format!("sign app JWT: {e}"))
            .with_hint("--key must point to the App's RSA private-key PEM")
    })
}

/// Resolve the private key from `GITHUB_APP_PRIVATE_KEY` (inline PEM) or, failing
/// that, the `--key` / `GITHUB_APP_PRIVATE_KEY_PATH` file.
fn load_private_key(auth: &AppAuthArgs) -> Result<Vec<u8>, CommandError> {
    if let Ok(content) = std::env::var("GITHUB_APP_PRIVATE_KEY")
        && !content.trim().is_empty()
    {
        return Ok(content.into_bytes());
    }
    let path = auth.key.as_ref().ok_or_else(|| {
        CommandError::usage(
            "missing-key",
            "no private key: pass --key <path> or set GITHUB_APP_PRIVATE_KEY[_PATH]",
        )
    })?;
    std::fs::read(path)
        .map_err(|e| CommandError::data("key-read", format!("read {}: {e}", path.display())))
}

fn print_installations_text(payload: &InstallationsPayload) {
    for row in &payload.installations {
        let account = row.account.as_deref().unwrap_or("-");
        let selection = row.repository_selection.as_deref().unwrap_or("-");
        println!("{}\t{}\t{}", row.installation_id, account, selection);
    }
}

fn emit_success<T: serde::Serialize>(command: &str, data: T) -> i32 {
    let envelope = Envelope::success(schema_version_for(BINARY, command, 1), data);
    match serde_json::to_string(&envelope) {
        Ok(serialized) => {
            println!("{serialized}");
            exit::SUCCESS
        }
        Err(_) => exit::SOFTWARE,
    }
}

fn emit_failure(format: OutputFormat, command: &str, err: &CommandError) -> i32 {
    match format {
        OutputFormat::Json => {
            let mut envelope_error = EnvelopeError::new(err.code.clone(), err.message.clone());
            if let Some(hint) = &err.hint {
                envelope_error = envelope_error.with_hint(hint.clone());
            }
            let envelope: Envelope<()> =
                Envelope::failure(schema_version_for(BINARY, command, 1), envelope_error);
            let serialized =
                serde_json::to_string(&envelope).unwrap_or_else(|_| String::from("{\"ok\":false}"));
            println!("{serialized}");
        }
        OutputFormat::Text => {
            eprintln!("error: {}", err.message);
            if let Some(hint) = &err.hint {
                eprintln!("hint: {hint}");
            }
        }
    }
    err.exit_code
}

// ---- parse handling (mirrors the cli-output-contract reference in cli-template) ----

fn parse_or_exit() -> Cli {
    match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            use clap::error::ErrorKind;
            let kind = err.kind();
            if matches!(
                kind,
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                err.exit();
            }

            let format = detect_format_from_argv();
            let code = match kind {
                ErrorKind::InvalidSubcommand => "unknown-subcommand",
                _ => "parse-error",
            };
            let message = render_clap_message(&err);
            let exit_code = emit_parse_error(BINARY, format, code, &message);
            process::exit(exit_code);
        }
    }
}

fn detect_format_from_argv() -> OutputFormat {
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--format"
            && let Some(next) = iter.next()
            && next.eq_ignore_ascii_case("json")
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

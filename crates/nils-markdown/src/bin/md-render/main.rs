mod cli;

use std::io::{self, Write};
use std::path::Path;

use clap::{CommandFactory, Parser};
use clap_complete::{Shell, generate};
use nils_common::cli_contract::{
    Envelope, EnvelopeError, OutputFormat, emit_parse_error, exit, schema_version_for,
};
use nils_markdown::Engine;
use serde::Serialize;

use crate::cli::{Cli, Command, CompletionShell, RenderArgs};

const BINARY: &str = "md-render";
const RENDER_OPERATION: &str = "render";

#[derive(Debug, Serialize)]
struct RenderPayload {
    template: String,
    body: String,
}

fn main() {
    let cli = parse_or_exit();
    let format = cli.format.unwrap_or_default();
    let exit_code = match cli.command {
        Some(Command::Completion { shell }) => run_completion(shell),
        Some(Command::Render(args)) => run_render(args, format),
        None => run_render(cli.default_render, format),
    };
    std::process::exit(exit_code);
}

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
            let message = err.to_string();
            let exit_code = emit_parse_error(BINARY, format, code, &message);
            std::process::exit(exit_code);
        }
    }
}

fn detect_format_from_argv() -> OutputFormat {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--format"
            && let Some(value) = args.next()
            && value.eq_ignore_ascii_case("json")
        {
            return OutputFormat::Json;
        } else if let Some(value) = arg.strip_prefix("--format=")
            && value.eq_ignore_ascii_case("json")
        {
            return OutputFormat::Json;
        }
    }
    OutputFormat::Text
}

fn run_render(args: RenderArgs, format: OutputFormat) -> i32 {
    let Some(template_path) = args.template else {
        return emit_render_error(format, "missing-argument", "--template is required");
    };
    let Some(data_path) = args.data else {
        return emit_render_error(format, "missing-argument", "--data is required");
    };

    let template_name = match template_stem(&template_path) {
        Some(name) => name,
        None => {
            return emit_render_error(
                format,
                "invalid-template-path",
                &format!(
                    "could not derive a template name from `{}`",
                    template_path.display()
                ),
            );
        }
    };

    let template_body = match std::fs::read_to_string(&template_path) {
        Ok(body) => body,
        Err(err) => {
            return emit_render_error(
                format,
                "template-read-failed",
                &format!("failed to read `{}`: {err}", template_path.display()),
            );
        }
    };

    let data_body = match std::fs::read_to_string(&data_path) {
        Ok(body) => body,
        Err(err) => {
            return emit_render_error(
                format,
                "data-read-failed",
                &format!("failed to read `{}`: {err}", data_path.display()),
            );
        }
    };

    let data: serde_json::Value = match serde_json::from_str(&data_body) {
        Ok(value) => value,
        Err(err) => {
            return emit_render_error(
                format,
                "data-parse-failed",
                &format!("failed to parse `{}`: {err}", data_path.display()),
            );
        }
    };

    let mut engine = Engine::builder().build();
    if let Err(err) = engine.register_template(&template_name, &template_body) {
        return emit_render_error(format, "template-register-failed", &err.to_string());
    }
    let rendered = match engine.render_value(&template_name, &data) {
        Ok(text) => text,
        Err(err) => {
            return emit_render_error(format, "template-render-failed", &err.to_string());
        }
    };

    match format {
        OutputFormat::Text => {
            if let Err(err) = io::stdout().write_all(rendered.as_bytes()) {
                return emit_render_error(
                    OutputFormat::Text,
                    "write-stdout-failed",
                    &err.to_string(),
                );
            }
            exit::SUCCESS
        }
        OutputFormat::Json => {
            let payload = RenderPayload {
                template: template_name,
                body: rendered,
            };
            let envelope =
                Envelope::success(schema_version_for(BINARY, RENDER_OPERATION, 1), payload);
            match serde_json::to_string(&envelope) {
                Ok(serialized) => {
                    println!("{serialized}");
                    exit::SUCCESS
                }
                Err(_) => exit::SOFTWARE,
            }
        }
    }
}

fn emit_render_error(format: OutputFormat, code: &str, message: &str) -> i32 {
    match format {
        OutputFormat::Text => {
            eprintln!("error: {message}");
            exit::USAGE
        }
        OutputFormat::Json => {
            let envelope = Envelope::<()>::failure(
                schema_version_for(BINARY, RENDER_OPERATION, 1),
                EnvelopeError::new(code, message),
            );
            match serde_json::to_string(&envelope) {
                Ok(serialized) => {
                    println!("{serialized}");
                    exit::USAGE
                }
                Err(_) => exit::SOFTWARE,
            }
        }
    }
}

fn template_stem(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(".md.tera")
        .or_else(|| name.strip_suffix(".tera"))
        .unwrap_or_else(|| {
            Path::new(name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(name)
        });
    if stem.is_empty() {
        None
    } else {
        Some(stem.to_string())
    }
}

fn run_completion(shell: CompletionShell) -> i32 {
    let mut command = Cli::command();
    let shell = match shell {
        CompletionShell::Bash => Shell::Bash,
        CompletionShell::Zsh => Shell::Zsh,
    };
    let mut buffer = Vec::new();
    generate(shell, &mut command, BINARY, &mut buffer);
    let rendered = match shell {
        Shell::Bash => {
            let raw = String::from_utf8(buffer).expect("bash completion is valid UTF-8");
            raw.replace("__subcmd__", "__")
                .replace("complete -F _", "complete -o nospace -F _")
                .into_bytes()
        }
        _ => buffer,
    };
    if let Err(err) = io::stdout().write_all(&rendered) {
        eprintln!("error: failed to write completion: {err}");
        return exit::SOFTWARE;
    }
    exit::SUCCESS
}

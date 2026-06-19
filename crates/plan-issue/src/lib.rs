pub mod adapter;
pub mod cli;
pub mod commands;
mod completion;
pub mod dispatch_record;
mod execute;
mod forge_cli_adapter;
pub mod issue_body;
pub mod lifecycle_lock;
pub mod lifecycle_record;
pub mod lifecycle_vnext;
pub mod output;
mod provider;
pub mod render;
pub mod runtime_layout;
pub mod state;
pub mod task_spec;
pub mod tracking;

use std::ffi::OsString;

use clap::{CommandFactory, FromArgMatches};
use nils_common::cli_contract::exit;
use serde_json::json;

use crate::cli::Cli;
use crate::commands::Command;

pub const EXIT_SUCCESS: i32 = exit::SUCCESS;
pub const EXIT_FAILURE: i32 = exit::RUNTIME;
pub const EXIT_USAGE: i32 = exit::USAGE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryFlavor {
    PlanIssue,
    PlanIssueLocal,
}

impl BinaryFlavor {
    pub fn binary_name(self) -> &'static str {
        match self {
            Self::PlanIssue => "plan-issue",
            Self::PlanIssueLocal => "plan-issue-local",
        }
    }

    pub fn execution_mode(self) -> &'static str {
        match self {
            Self::PlanIssue => "live",
            Self::PlanIssueLocal => "local",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub code: &'static str,
    pub message: String,
}

impl ValidationError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
    pub exit_code: i32,
}

impl CommandError {
    pub fn new(code: &'static str, message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            code,
            message: message.into(),
            exit_code,
        }
    }

    pub fn runtime(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(code, message, EXIT_FAILURE)
    }

    pub fn usage(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(code, message, EXIT_USAGE)
    }
}

pub fn run(binary: BinaryFlavor) -> i32 {
    run_with_args(binary, std::env::args_os())
}

pub fn run_with_args<I, T>(binary: BinaryFlavor, args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let command = Cli::command().name(binary.binary_name());
    let matches = match command.try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(err) => {
            let code = if err.use_stderr() {
                EXIT_USAGE
            } else {
                EXIT_SUCCESS
            };
            let _ = err.print();
            return code;
        }
    };
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return EXIT_USAGE;
        }
    };

    crate::state::set_state_dir_override(cli.state_dir.clone());

    if let Command::Completion(args) = &cli.command {
        return completion::run(binary, args.shell);
    }

    let output_format = match cli.resolve_output_format() {
        Ok(format) => format,
        Err(err) => {
            eprintln!("error: {}", err.message);
            return EXIT_USAGE;
        }
    };

    // Task 1.5: `resolve-approval` text mode prints just the URL (or fails
    // with a clear stderr message naming the count). JSON mode falls
    // through to the standard envelope so consumers can read the candidate
    // array.
    if let Command::ResolveApproval(args) = &cli.command
        && matches!(output_format, crate::cli::OutputFormat::Text)
    {
        return execute::run_resolve_approval_text(binary, cli.repo.as_deref(), args);
    }

    if let Err(err) = cli.validate() {
        let schema_version = cli.command.schema_version();
        if let Err(render_err) = output::emit_error(
            output_format,
            &schema_version,
            cli.command.command_id(),
            err.code,
            &err.message,
        ) {
            eprintln!("error: {render_err}");
        }
        return EXIT_FAILURE;
    }

    let execution_result = match execute::execute(binary, &cli) {
        Ok(result) => result,
        Err(err) => {
            let schema_version = cli.command.schema_version();
            if let Err(render_err) = output::emit_error(
                output_format,
                &schema_version,
                cli.command.command_id(),
                err.code,
                &err.message,
            ) {
                eprintln!("error: {render_err}");
            }
            return err.exit_code;
        }
    };

    let schema_version = cli.command.schema_version();
    let payload = json!({
        "binary": binary.binary_name(),
        "execution_mode": binary.execution_mode(),
        "dry_run": cli.dry_run,
        "repo": cli.repo,
        "arguments": cli.command.payload(),
        "result": execution_result,
    });

    if let Err(err) = output::emit_success(
        output_format,
        &schema_version,
        cli.command.command_id(),
        &payload,
    ) {
        eprintln!("error: {err}");
        return EXIT_FAILURE;
    }

    EXIT_SUCCESS
}

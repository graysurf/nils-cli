use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use clap::{Args, Parser, Subcommand, ValueHint};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::common::{
    CliError, OutputFormat, display_path, ensure_non_empty, preview_text, record_path, redact_text,
    render_error, render_success, write_json_pretty,
};
use crate::completion::{self, CompletionShell};

const RECORD_SCHEMA_VERSION: &str = "canary-check.record.v1";
const RECORD_FILE: &str = "canary-check.json";
const RUN_SCHEMA_VERSION: &str = "cli.canary-check.run.v1";
const VERIFY_SCHEMA_VERSION: &str = "cli.canary-check.verify.v1";
const SHOW_SCHEMA_VERSION: &str = "cli.canary-check.show.v1";
const RUN_COMMAND: &str = "canary-check run";
const VERIFY_COMMAND: &str = "canary-check verify";
const SHOW_COMMAND: &str = "canary-check show";

pub fn run() -> i32 {
    run_with_args(env::args_os())
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let argv: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let cli = match Cli::try_parse_from(argv.clone()) {
        Ok(cli) => cli,
        Err(err) => return crate::common::handle_parse_error("canary-check", argv, err),
    };

    match cli.command {
        Command::Run(args) => run_canary(args),
        Command::Verify(args) => verify(args),
        Command::Show(args) => show(args),
        Command::Completion(args) => completion::run::<Cli>(args.shell, "canary-check"),
    }
}

fn run_canary(args: RunArgs) -> i32 {
    let format = args.format;
    match execute(&args) {
        Ok(result) if result.record.last_run.status == "pass" => render_success(
            RUN_SCHEMA_VERSION,
            RUN_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Ok(result) => render_error(
            RUN_SCHEMA_VERSION,
            RUN_COMMAND,
            format,
            CliError::runtime(
                "canary-failed",
                "canary command did not meet expected exit code",
                Some(json!({ "result": result })),
            ),
        ),
        Err(err) => render_error(RUN_SCHEMA_VERSION, RUN_COMMAND, format, err),
    }
}

fn verify(args: CommonArgs) -> i32 {
    let format = args.format;
    match read_record(&args.out_dir) {
        Ok(record) if record.last_run.status == "pass" => render_success(
            VERIFY_SCHEMA_VERSION,
            VERIFY_COMMAND,
            format,
            || format!("canary-check complete: {}", record.last_run.name),
            &record,
        ),
        Ok(record) => render_error(
            VERIFY_SCHEMA_VERSION,
            VERIFY_COMMAND,
            format,
            CliError::runtime(
                "canary-not-passing",
                "latest canary run is not passing",
                Some(json!({ "last_run": record.last_run })),
            ),
        ),
        Err(err) => render_error(VERIFY_SCHEMA_VERSION, VERIFY_COMMAND, format, err),
    }
}

fn show(args: CommonArgs) -> i32 {
    let format = args.format;
    match read_record(&args.out_dir) {
        Ok(record) => render_success(
            SHOW_SCHEMA_VERSION,
            SHOW_COMMAND,
            format,
            || format!("canary-check: {}", record.last_run.status),
            &record,
        ),
        Err(err) => render_error(SHOW_SCHEMA_VERSION, SHOW_COMMAND, format, err),
    }
}

fn execute(args: &RunArgs) -> Result<RunResult, CliError> {
    ensure_non_empty("--name", &args.name)?;
    ensure_non_empty("--command", &args.command)?;

    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let output = ProcessCommand::new(shell)
        .arg("-lc")
        .arg(&args.command)
        .output()
        .map_err(|err| {
            CliError::runtime(
                "command-failed-to-start",
                format!("failed to start canary command: {err}"),
                Some(json!({ "command": redact_text(&args.command) })),
            )
        })?;
    let exit_code = output.status.code().unwrap_or(128);
    let status = if exit_code == args.expect_exit {
        "pass"
    } else {
        "fail"
    };
    let record_file = record_path(&args.out_dir, RECORD_FILE)?;
    let record = CanaryRecord {
        schema_version: RECORD_SCHEMA_VERSION.to_string(),
        last_run: CanaryRun {
            name: redact_text(&args.name),
            command: redact_text(&args.command),
            expect_exit: args.expect_exit,
            exit_code,
            status: status.to_string(),
            stdout_preview: preview_text(&output.stdout, args.preview_bytes),
            stderr_preview: preview_text(&output.stderr, args.preview_bytes),
        },
    };
    write_json_pretty(&record_file, &record)?;
    Ok(RunResult {
        record_file: display_path(&record_file),
        record,
    })
}

fn read_record(out_dir: &Path) -> Result<CanaryRecord, CliError> {
    let path = record_path(out_dir, RECORD_FILE)?;
    crate::common::read_json(&path)
}

#[derive(Debug, Parser)]
#[command(
    name = "canary-check",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Run and verify local canary checks for agent workflows.",
    long_about = "Run a local command as a canary, persist redacted evidence, and verify the latest run status.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  canary-check run --out /tmp/canary --name smoke --command 'cargo test smoke'\n  canary-check verify --out /tmp/canary --format json\n  canary-check completion zsh\n\nENVIRONMENT:\n  SHELL  Shell used to execute canary commands; defaults to /bin/sh.\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
enum Command {
    /// Run one local canary command and persist the redacted result.
    Run(RunArgs),
    /// Verify the latest canary result is passing.
    Verify(CommonArgs),
    /// Print the latest canary record.
    Show(CommonArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
struct CommonArgs {
    /// Artifact directory containing `canary-check.json`.
    #[arg(long = "out", value_name = "DIR", value_hint = ValueHint::DirPath)]
    out_dir: PathBuf,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// Artifact directory where `canary-check.json` is written.
    #[arg(long = "out", value_name = "DIR", value_hint = ValueHint::DirPath)]
    out_dir: PathBuf,

    /// Canary label.
    #[arg(long, value_name = "TEXT")]
    name: String,

    /// Command to execute through the local shell.
    #[arg(long, value_name = "TEXT")]
    command: String,

    /// Expected command exit code.
    #[arg(long = "expect-exit", default_value_t = 0)]
    expect_exit: i32,

    /// Maximum stdout/stderr preview characters to persist.
    #[arg(long = "preview-bytes", default_value_t = 4096)]
    preview_bytes: usize,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum)]
    shell: CompletionShell,
}

#[derive(Debug, Deserialize, Serialize)]
struct CanaryRecord {
    schema_version: String,
    last_run: CanaryRun,
}

#[derive(Debug, Deserialize, Serialize)]
struct CanaryRun {
    name: String,
    command: String,
    expect_exit: i32,
    exit_code: i32,
    status: String,
    stdout_preview: String,
    stderr_preview: String,
}

#[derive(Debug, Serialize)]
struct RunResult {
    record_file: String,
    record: CanaryRecord,
}

impl RunResult {
    fn text_summary(&self) -> String {
        format!(
            "canary-check: {} exit_code={} expected={}",
            self.record.last_run.status,
            self.record.last_run.exit_code,
            self.record.last_run.expect_exit
        )
    }
}

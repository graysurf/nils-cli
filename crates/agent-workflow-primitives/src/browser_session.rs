use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::common::{
    CliError, OutputFormat, display_path, ensure_non_empty, normalized_paths, record_path,
    redact_text, render_error, render_success, write_json_pretty,
};
use crate::completion::{self, CompletionShell};

const RECORD_SCHEMA_VERSION: &str = "browser-session.record.v1";
const RECORD_FILE: &str = "browser-session.json";
const INIT_SCHEMA_VERSION: &str = "cli.browser-session.init.v1";
const STEP_SCHEMA_VERSION: &str = "cli.browser-session.record-step.v1";
const VERIFY_SCHEMA_VERSION: &str = "cli.browser-session.verify.v1";
const SHOW_SCHEMA_VERSION: &str = "cli.browser-session.show.v1";
const INIT_COMMAND: &str = "browser-session init";
const STEP_COMMAND: &str = "browser-session record-step";
const VERIFY_COMMAND: &str = "browser-session verify";
const SHOW_COMMAND: &str = "browser-session show";

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
        Err(err) => return crate::common::handle_parse_error("browser-session", argv, err),
    };

    match cli.command {
        Command::Init(args) => init(args),
        Command::RecordStep(args) => record_step(args),
        Command::Verify(args) => verify(args),
        Command::Show(args) => show(args),
        Command::Completion(args) => completion::run::<Cli>(args.shell, "browser-session"),
    }
}

fn init(args: InitArgs) -> i32 {
    let format = args.common.format;
    match init_record(&args) {
        Ok(result) => render_success(
            INIT_SCHEMA_VERSION,
            INIT_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(INIT_SCHEMA_VERSION, INIT_COMMAND, format, err),
    }
}

fn record_step(args: StepArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        ensure_non_empty("--action", &args.action)?;
        record.steps.push(SessionStep {
            action: redact_text(&args.action),
            expectation: args.expectation.as_deref().map(redact_text),
            status: args.status.as_str().to_string(),
            artifacts: normalized_paths(&args.artifact),
        });
        Ok(())
    }) {
        Ok(result) => render_success(
            STEP_SCHEMA_VERSION,
            STEP_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(STEP_SCHEMA_VERSION, STEP_COMMAND, format, err),
    }
}

fn verify(args: CommonArgs) -> i32 {
    let format = args.format;
    match read_record_result(args.out_dir.as_path()) {
        Ok(result) => {
            let missing = missing_fields(&result.record);
            if missing.is_empty() {
                render_success(
                    VERIFY_SCHEMA_VERSION,
                    VERIFY_COMMAND,
                    format,
                    || "browser-session complete".to_string(),
                    &VerifyResult {
                        complete: true,
                        missing,
                        record_file: result.record_file,
                        record: result.record,
                    },
                )
            } else {
                render_error(
                    VERIFY_SCHEMA_VERSION,
                    VERIFY_COMMAND,
                    format,
                    CliError::runtime(
                        "incomplete-browser-session",
                        "browser session record is incomplete",
                        Some(json!({ "missing": missing, "record_file": result.record_file })),
                    ),
                )
            }
        }
        Err(err) => render_error(VERIFY_SCHEMA_VERSION, VERIFY_COMMAND, format, err),
    }
}

fn show(args: CommonArgs) -> i32 {
    let format = args.format;
    match read_record_result(args.out_dir.as_path()) {
        Ok(result) => render_success(
            SHOW_SCHEMA_VERSION,
            SHOW_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(SHOW_SCHEMA_VERSION, SHOW_COMMAND, format, err),
    }
}

fn init_record(args: &InitArgs) -> Result<RecordResult, CliError> {
    ensure_non_empty("--target", &args.target)?;
    ensure_non_empty("--goal", &args.goal)?;
    let path = record_path(&args.common.out_dir, RECORD_FILE)?;
    if path.exists() && !args.force {
        return Err(CliError::runtime(
            "record-exists",
            format!(
                "{} already exists; pass --force to overwrite",
                path.display()
            ),
            Some(json!({ "record_file": display_path(&path), "force_flag": "--force" })),
        ));
    }
    let record = BrowserRecord {
        schema_version: RECORD_SCHEMA_VERSION.to_string(),
        target: redact_text(&args.target),
        goal: redact_text(&args.goal),
        browser: args.browser.as_deref().map(redact_text),
        steps: Vec::new(),
    };
    write_json_pretty(&path, &record)?;
    Ok(RecordResult::new(path, record))
}

fn update_record<F>(out_dir: &Path, update: F) -> Result<RecordResult, CliError>
where
    F: FnOnce(&mut BrowserRecord) -> Result<(), CliError>,
{
    let path = record_path(out_dir, RECORD_FILE)?;
    let mut record: BrowserRecord = crate::common::read_json(&path)?;
    update(&mut record)?;
    write_json_pretty(&path, &record)?;
    Ok(RecordResult::new(path, record))
}

fn read_record_result(out_dir: &Path) -> Result<RecordResult, CliError> {
    let path = record_path(out_dir, RECORD_FILE)?;
    let record = crate::common::read_json(&path)?;
    Ok(RecordResult::new(path, record))
}

fn missing_fields(record: &BrowserRecord) -> Vec<String> {
    let mut missing = Vec::new();
    if record.steps.is_empty() {
        missing.push("steps".to_string());
    }
    if record.steps.iter().any(|step| step.status == "fail") {
        missing.push("no_failed_steps".to_string());
    }
    missing
}

#[derive(Debug, Parser)]
#[command(
    name = "browser-session",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Record browser-session evidence for agent workflows.",
    long_about = "Record browser QA goals, steps, artifacts, and verification status in a deterministic evidence file.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  browser-session init --out /tmp/browser --target http://localhost:3000 --goal 'verify checkout flow'\n  browser-session record-step --out /tmp/browser --action 'opened checkout page' --status pass --artifact screenshot.png\n  browser-session verify --out /tmp/browser --format json\n  browser-session completion zsh\n\nENVIRONMENT:\n  none\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
enum Command {
    /// Create a browser-session evidence record.
    Init(InitArgs),
    /// Append one browser-session step and optional artifacts.
    RecordStep(StepArgs),
    /// Verify the session has passing evidence.
    Verify(CommonArgs),
    /// Print the current session record.
    Show(CommonArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
struct CommonArgs {
    /// Artifact directory containing `browser-session.json`.
    #[arg(long = "out", value_name = "DIR", value_hint = ValueHint::DirPath)]
    out_dir: PathBuf,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct InitArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Target URL or browser surface.
    #[arg(long, value_name = "TEXT")]
    target: String,

    /// Session goal.
    #[arg(long, value_name = "TEXT")]
    goal: String,

    /// Optional browser/runtime label.
    #[arg(long, value_name = "TEXT")]
    browser: Option<String>,

    /// Overwrite an existing record.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct StepArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Browser action performed.
    #[arg(long, value_name = "TEXT")]
    action: String,

    /// Optional expectation or observed result.
    #[arg(long, value_name = "TEXT")]
    expectation: Option<String>,

    /// Step status.
    #[arg(long, value_enum)]
    status: StepStatus,

    /// Optional screenshot/log/evidence artifact path. Repeat for multiple artifacts.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    artifact: Vec<PathBuf>,
}

#[derive(Debug, Args)]
struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum)]
    shell: CompletionShell,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum StepStatus {
    Pass,
    Fail,
}

impl StepStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct BrowserRecord {
    schema_version: String,
    target: String,
    goal: String,
    browser: Option<String>,
    steps: Vec<SessionStep>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SessionStep {
    action: String,
    expectation: Option<String>,
    status: String,
    artifacts: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RecordResult {
    record_file: String,
    complete: bool,
    record: BrowserRecord,
}

impl RecordResult {
    fn new(path: PathBuf, record: BrowserRecord) -> Self {
        let complete = missing_fields(&record).is_empty();
        Self {
            record_file: display_path(&path),
            complete,
            record,
        }
    }

    fn text_summary(&self) -> String {
        format!(
            "browser-session: complete={} steps={}",
            self.complete,
            self.record.steps.len()
        )
    }
}

#[derive(Debug, Serialize)]
struct VerifyResult {
    complete: bool,
    missing: Vec<String>,
    record_file: String,
    record: BrowserRecord,
}

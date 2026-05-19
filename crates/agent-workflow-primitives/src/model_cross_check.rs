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

const RECORD_SCHEMA_VERSION: &str = "model-cross-check.record.v1";
const RECORD_FILE: &str = "model-cross-check.json";
const INIT_SCHEMA_VERSION: &str = "cli.model-cross-check.init.v1";
const OBSERVATION_SCHEMA_VERSION: &str = "cli.model-cross-check.record-observation.v1";
const VERIFY_SCHEMA_VERSION: &str = "cli.model-cross-check.verify.v1";
const SHOW_SCHEMA_VERSION: &str = "cli.model-cross-check.show.v1";
const INIT_COMMAND: &str = "model-cross-check init";
const OBSERVATION_COMMAND: &str = "model-cross-check record-observation";
const VERIFY_COMMAND: &str = "model-cross-check verify";
const SHOW_COMMAND: &str = "model-cross-check show";

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
        Err(err) => return crate::common::handle_parse_error("model-cross-check", argv, err),
    };

    match cli.command {
        Command::Init(args) => init(args),
        Command::RecordObservation(args) => record_observation(args),
        Command::Verify(args) => verify(args),
        Command::Show(args) => show(args),
        Command::Completion(args) => completion::run::<Cli>(args.shell, "model-cross-check"),
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

fn record_observation(args: ObservationArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        ensure_non_empty("--model", &args.model)?;
        ensure_non_empty("--summary", &args.summary)?;
        record.observations.push(ModelObservation {
            role: args.role.as_str().to_string(),
            model: redact_text(&args.model),
            verdict: args.verdict.as_str().to_string(),
            summary: redact_text(&args.summary),
            artifacts: normalized_paths(&args.artifact),
        });
        Ok(())
    }) {
        Ok(result) => render_success(
            OBSERVATION_SCHEMA_VERSION,
            OBSERVATION_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(OBSERVATION_SCHEMA_VERSION, OBSERVATION_COMMAND, format, err),
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
                    || "model-cross-check complete".to_string(),
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
                        "incomplete-model-cross-check",
                        "model cross-check record is incomplete",
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
    ensure_non_empty("--prompt", &args.prompt)?;
    ensure_non_empty("--primary-model", &args.primary_model)?;
    ensure_non_empty("--checker-model", &args.checker_model)?;
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
    let record = CrossCheckRecord {
        schema_version: RECORD_SCHEMA_VERSION.to_string(),
        prompt: redact_text(&args.prompt),
        primary_model: redact_text(&args.primary_model),
        checker_model: redact_text(&args.checker_model),
        criteria: crate::common::redact_strings(&args.criterion),
        observations: Vec::new(),
    };
    write_json_pretty(&path, &record)?;
    Ok(RecordResult::new(path, record))
}

fn update_record<F>(out_dir: &Path, update: F) -> Result<RecordResult, CliError>
where
    F: FnOnce(&mut CrossCheckRecord) -> Result<(), CliError>,
{
    let path = record_path(out_dir, RECORD_FILE)?;
    let mut record: CrossCheckRecord = crate::common::read_json(&path)?;
    update(&mut record)?;
    write_json_pretty(&path, &record)?;
    Ok(RecordResult::new(path, record))
}

fn read_record_result(out_dir: &Path) -> Result<RecordResult, CliError> {
    let path = record_path(out_dir, RECORD_FILE)?;
    let record = crate::common::read_json(&path)?;
    Ok(RecordResult::new(path, record))
}

fn missing_fields(record: &CrossCheckRecord) -> Vec<String> {
    let mut missing = Vec::new();
    if !record
        .observations
        .iter()
        .any(|item| item.role == "primary")
    {
        missing.push("primary_observation".to_string());
    }
    if !record
        .observations
        .iter()
        .any(|item| item.role == "checker")
    {
        missing.push("checker_observation".to_string());
    }
    if record
        .observations
        .iter()
        .any(|item| item.role == "checker" && item.verdict == "fail")
    {
        missing.push("checker_not_fail".to_string());
    }
    missing
}

#[derive(Debug, Parser)]
#[command(
    name = "model-cross-check",
    version,
    about = "Record cross-model review evidence without owning provider calls.",
    long_about = "Persist primary and checker model observations, linked artifacts, and verification status without invoking model providers.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  model-cross-check init --out /tmp/model-check --prompt 'review this patch' --primary-model gpt-5.5 --checker-model gemini-2.5-pro\n  model-cross-check record-observation --out /tmp/model-check --role primary --model gpt-5.5 --verdict pass --summary 'implementation is coherent'\n  model-cross-check record-observation --out /tmp/model-check --role checker --model gemini-2.5-pro --verdict pass --summary 'no blocker found'\n  model-cross-check verify --out /tmp/model-check --format json\n  model-cross-check completion zsh\n\nENVIRONMENT:\n  none\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
enum Command {
    /// Create a model cross-check record.
    Init(InitArgs),
    /// Append one manually captured model observation.
    RecordObservation(ObservationArgs),
    /// Verify both primary and checker observations are recorded.
    Verify(CommonArgs),
    /// Print the current record.
    Show(CommonArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
struct CommonArgs {
    /// Artifact directory containing `model-cross-check.json`.
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

    /// Prompt or task being cross-checked.
    #[arg(long, value_name = "TEXT")]
    prompt: String,

    /// Primary model label.
    #[arg(long = "primary-model", value_name = "TEXT")]
    primary_model: String,

    /// Checker model label.
    #[arg(long = "checker-model", value_name = "TEXT")]
    checker_model: String,

    /// Check criterion. Repeat for multiple criteria.
    #[arg(long, value_name = "TEXT")]
    criterion: Vec<String>,

    /// Overwrite an existing record.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct ObservationArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Observation role.
    #[arg(long, value_enum)]
    role: ObservationRole,

    /// Model label.
    #[arg(long, value_name = "TEXT")]
    model: String,

    /// Verdict for this observation.
    #[arg(long, value_enum)]
    verdict: Verdict,

    /// Concise observation summary.
    #[arg(long, value_name = "TEXT")]
    summary: String,

    /// Optional evidence artifact path. Repeat for multiple artifacts.
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
enum ObservationRole {
    Primary,
    Checker,
}

impl ObservationRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Checker => "checker",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum Verdict {
    Pass,
    Fail,
    Inconclusive,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Inconclusive => "inconclusive",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct CrossCheckRecord {
    schema_version: String,
    prompt: String,
    primary_model: String,
    checker_model: String,
    criteria: Vec<String>,
    observations: Vec<ModelObservation>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ModelObservation {
    role: String,
    model: String,
    verdict: String,
    summary: String,
    artifacts: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RecordResult {
    record_file: String,
    complete: bool,
    record: CrossCheckRecord,
}

impl RecordResult {
    fn new(path: PathBuf, record: CrossCheckRecord) -> Self {
        let complete = missing_fields(&record).is_empty();
        Self {
            record_file: display_path(&path),
            complete,
            record,
        }
    }

    fn text_summary(&self) -> String {
        format!(
            "model-cross-check: complete={} observations={}",
            self.complete,
            self.record.observations.len()
        )
    }
}

#[derive(Debug, Serialize)]
struct VerifyResult {
    complete: bool,
    missing: Vec<String>,
    record_file: String,
    record: CrossCheckRecord,
}

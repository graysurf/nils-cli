use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::common::{
    CliError, EXIT_USAGE, OutputFormat, display_path, ensure_non_empty, normalized_paths,
    record_path, redact_text, render_error, render_success, write_json_pretty,
};
use crate::completion::{self, CompletionShell};

const RECORD_SCHEMA_VERSION: &str = "review-evidence.record.v1";
const RECORD_FILE: &str = "review-evidence.json";
const INIT_SCHEMA_VERSION: &str = "cli.review-evidence.init.v1";
const FINDING_SCHEMA_VERSION: &str = "cli.review-evidence.record-finding.v1";
const VALIDATION_SCHEMA_VERSION: &str = "cli.review-evidence.record-validation.v1";
const VERIFY_SCHEMA_VERSION: &str = "cli.review-evidence.verify.v1";
const SHOW_SCHEMA_VERSION: &str = "cli.review-evidence.show.v1";
const INIT_COMMAND: &str = "review-evidence init";
const FINDING_COMMAND: &str = "review-evidence record-finding";
const VALIDATION_COMMAND: &str = "review-evidence record-validation";
const VERIFY_COMMAND: &str = "review-evidence verify";
const SHOW_COMMAND: &str = "review-evidence show";

pub fn run() -> i32 {
    run_with_args(env::args_os())
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            let code = match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => err.exit_code(),
                _ => EXIT_USAGE,
            };
            let _ = err.print();
            return code;
        }
    };

    match cli.command {
        Command::Init(args) => init(args),
        Command::RecordFinding(args) => record_finding(args),
        Command::RecordValidation(args) => record_validation(args),
        Command::Verify(args) => verify(args),
        Command::Show(args) => show(args),
        Command::Completion(args) => completion::run::<Cli>(args.shell, "review-evidence"),
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

fn record_finding(args: FindingArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        ensure_non_empty("--summary", &args.summary)?;
        record.findings.push(Finding {
            severity: args.severity.as_str().to_string(),
            path: display_path(&args.path),
            line: args.line,
            summary: redact_text(&args.summary),
            status: args.status.as_str().to_string(),
            artifacts: normalized_paths(&args.artifact),
        });
        Ok(())
    }) {
        Ok(result) => render_success(
            FINDING_SCHEMA_VERSION,
            FINDING_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(FINDING_SCHEMA_VERSION, FINDING_COMMAND, format, err),
    }
}

fn record_validation(args: ValidationArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        ensure_non_empty("--command", &args.command)?;
        record.validation = Some(Validation {
            command: redact_text(&args.command),
            status: args.status.as_str().to_string(),
            summary: args.summary.as_deref().map(redact_text),
        });
        Ok(())
    }) {
        Ok(result) => render_success(
            VALIDATION_SCHEMA_VERSION,
            VALIDATION_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(VALIDATION_SCHEMA_VERSION, VALIDATION_COMMAND, format, err),
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
                    || "review-evidence complete".to_string(),
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
                        "incomplete-review-evidence",
                        "review evidence record is incomplete",
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
    ensure_non_empty("--subject", &args.subject)?;
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
    let record = ReviewRecord {
        schema_version: RECORD_SCHEMA_VERSION.to_string(),
        subject: redact_text(&args.subject),
        reviewer: args.reviewer.as_deref().map(redact_text),
        findings: Vec::new(),
        validation: None,
    };
    write_json_pretty(&path, &record)?;
    Ok(RecordResult::new(path, record))
}

fn update_record<F>(out_dir: &Path, update: F) -> Result<RecordResult, CliError>
where
    F: FnOnce(&mut ReviewRecord) -> Result<(), CliError>,
{
    let path = record_path(out_dir, RECORD_FILE)?;
    let mut record: ReviewRecord = crate::common::read_json(&path)?;
    update(&mut record)?;
    write_json_pretty(&path, &record)?;
    Ok(RecordResult::new(path, record))
}

fn read_record_result(out_dir: &Path) -> Result<RecordResult, CliError> {
    let path = record_path(out_dir, RECORD_FILE)?;
    let record = crate::common::read_json(&path)?;
    Ok(RecordResult::new(path, record))
}

fn missing_fields(record: &ReviewRecord) -> Vec<String> {
    let mut missing = Vec::new();
    if record.findings.is_empty() {
        missing.push("findings".to_string());
    }
    match &record.validation {
        Some(validation) if validation.status == "pass" => {}
        Some(_) => missing.push("passing_validation".to_string()),
        None => missing.push("validation".to_string()),
    }
    if record.findings.iter().any(|finding| {
        finding.status == "open" && matches!(finding.severity.as_str(), "high" | "medium")
    }) {
        missing.push("no_open_high_or_medium_findings".to_string());
    }
    missing
}

#[derive(Debug, Parser)]
#[command(
    name = "review-evidence",
    version,
    about = "Record review findings and validation evidence.",
    disable_help_subcommand = true,
    after_help = "Examples:\n  review-evidence init --out /tmp/review --subject 'PR #12'\n  review-evidence record-finding --out /tmp/review --severity medium --path src/lib.rs --line 42 --summary 'missing error path'\n  review-evidence record-validation --out /tmp/review --command 'cargo test' --status pass\n  review-evidence verify --out /tmp/review --format json\n  review-evidence completion zsh"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
enum Command {
    /// Create a review evidence record.
    Init(InitArgs),
    /// Append one review finding.
    RecordFinding(FindingArgs),
    /// Record final review validation.
    RecordValidation(ValidationArgs),
    /// Verify the review record is complete enough for delivery.
    Verify(CommonArgs),
    /// Print the review record.
    Show(CommonArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
struct CommonArgs {
    /// Artifact directory containing `review-evidence.json`.
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

    /// Review subject, such as a PR, MR, issue, or patch label.
    #[arg(long, value_name = "TEXT")]
    subject: String,

    /// Optional reviewer or agent label.
    #[arg(long, value_name = "TEXT")]
    reviewer: Option<String>,

    /// Overwrite an existing record.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct FindingArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Finding severity.
    #[arg(long, value_enum)]
    severity: Severity,

    /// Finding file path.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    path: PathBuf,

    /// Optional line number.
    #[arg(long)]
    line: Option<u32>,

    /// Concise finding summary.
    #[arg(long, value_name = "TEXT")]
    summary: String,

    /// Finding resolution status.
    #[arg(long, value_enum, default_value_t = FindingStatus::Open)]
    status: FindingStatus,

    /// Optional evidence artifact path. Repeat for multiple artifacts.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    artifact: Vec<PathBuf>,
}

#[derive(Debug, Args)]
struct ValidationArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Validation command or manual validation step.
    #[arg(long, value_name = "TEXT")]
    command: String,

    /// Validation status.
    #[arg(long, value_enum)]
    status: ValidationStatus,

    /// Optional validation summary.
    #[arg(long, value_name = "TEXT")]
    summary: Option<String>,
}

#[derive(Debug, Args)]
struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum)]
    shell: CompletionShell,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum Severity {
    High,
    Medium,
    Low,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum FindingStatus {
    Open,
    Fixed,
    AcceptedRisk,
}

impl FindingStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Fixed => "fixed",
            Self::AcceptedRisk => "accepted-risk",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum ValidationStatus {
    Pass,
    Fail,
}

impl ValidationStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ReviewRecord {
    schema_version: String,
    subject: String,
    reviewer: Option<String>,
    findings: Vec<Finding>,
    validation: Option<Validation>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Finding {
    severity: String,
    path: String,
    line: Option<u32>,
    summary: String,
    status: String,
    artifacts: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Validation {
    command: String,
    status: String,
    summary: Option<String>,
}

#[derive(Debug, Serialize)]
struct RecordResult {
    record_file: String,
    complete: bool,
    record: ReviewRecord,
}

impl RecordResult {
    fn new(path: PathBuf, record: ReviewRecord) -> Self {
        let complete = missing_fields(&record).is_empty();
        Self {
            record_file: display_path(&path),
            complete,
            record,
        }
    }

    fn text_summary(&self) -> String {
        format!(
            "review-evidence: complete={} findings={}",
            self.complete,
            self.record.findings.len()
        )
    }
}

#[derive(Debug, Serialize)]
struct VerifyResult {
    complete: bool,
    missing: Vec<String>,
    record_file: String,
    record: ReviewRecord,
}

mod cli;

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use clap::Parser;
use clap::error::ErrorKind;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use cli::{
    Cli, Command, CommonArgs, InitArgs, OutputFormat, RecordFailingArgs, RecordFinalArgs,
    RecordWaiverArgs,
};
use nils_common::cli_contract::exit;

const EXIT_OK: i32 = exit::SUCCESS;
const EXIT_RUNTIME: i32 = exit::RUNTIME;
const EXIT_USAGE: i32 = exit::USAGE;

const RECORD_SCHEMA_VERSION: &str = "test-first-evidence.record.v1";
const RECORD_FILE_NAME: &str = "test-first-evidence.json";

const INIT_SCHEMA_VERSION: &str = "cli.test-first-evidence.init.v1";
const RECORD_FAILING_SCHEMA_VERSION: &str = "cli.test-first-evidence.record-failing.v1";
const RECORD_WAIVER_SCHEMA_VERSION: &str = "cli.test-first-evidence.record-waiver.v1";
const RECORD_FINAL_SCHEMA_VERSION: &str = "cli.test-first-evidence.record-final.v1";
const VERIFY_SCHEMA_VERSION: &str = "cli.test-first-evidence.verify.v1";
const SHOW_SCHEMA_VERSION: &str = "cli.test-first-evidence.show.v1";

const INIT_COMMAND: &str = "test-first-evidence init";
const RECORD_FAILING_COMMAND: &str = "test-first-evidence record-failing";
const RECORD_WAIVER_COMMAND: &str = "test-first-evidence record-waiver";
const RECORD_FINAL_COMMAND: &str = "test-first-evidence record-final";
const VERIFY_COMMAND: &str = "test-first-evidence verify";
const SHOW_COMMAND: &str = "test-first-evidence show";

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

    dispatch(cli)
}

fn dispatch(cli: Cli) -> i32 {
    match cli.command {
        Command::Init(args) => run_init(args),
        Command::RecordFailing(args) => run_record_failing(args),
        Command::RecordWaiver(args) => run_record_waiver(args),
        Command::RecordFinal(args) => run_record_final(args),
        Command::Verify(args) => run_verify(args),
        Command::Show(args) => run_show(args),
        Command::Completion(args) => {
            crate::completion::run::<Cli>(args.shell, "test-first-evidence")
        }
    }
}

fn run_init(args: InitArgs) -> i32 {
    let format = args.common.format;
    match init_record(&args) {
        Ok(result) => render_record_success(INIT_SCHEMA_VERSION, INIT_COMMAND, format, &result),
        Err(err) => render_error(INIT_SCHEMA_VERSION, INIT_COMMAND, format, err),
    }
}

fn run_record_failing(args: RecordFailingArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        record.failing_test = Some(FailingEvidence {
            command: redact_text(&args.command).value,
            exit_code: args.exit_code,
            summary: redact_text(&args.summary).value,
            test_name: args
                .test_name
                .as_deref()
                .map(|value| redact_text(value).value),
            artifacts: normalized_paths(&args.artifact),
        });
        Ok(())
    }) {
        Ok(result) => render_record_success(
            RECORD_FAILING_SCHEMA_VERSION,
            RECORD_FAILING_COMMAND,
            format,
            &result,
        ),
        Err(err) => render_error(
            RECORD_FAILING_SCHEMA_VERSION,
            RECORD_FAILING_COMMAND,
            format,
            err,
        ),
    }
}

fn run_record_waiver(args: RecordWaiverArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        record.waiver = Some(WaiverEvidence {
            reason: redact_text(&args.reason).value,
            substitute_validation: redacted_strings(&args.substitute_validation),
        });
        Ok(())
    }) {
        Ok(result) => render_record_success(
            RECORD_WAIVER_SCHEMA_VERSION,
            RECORD_WAIVER_COMMAND,
            format,
            &result,
        ),
        Err(err) => render_error(
            RECORD_WAIVER_SCHEMA_VERSION,
            RECORD_WAIVER_COMMAND,
            format,
            err,
        ),
    }
}

fn run_record_final(args: RecordFinalArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        record.final_validation = Some(FinalValidation {
            command: redact_text(&args.command).value,
            status: args.status.as_str().to_string(),
            summary: args
                .summary
                .as_deref()
                .map(|value| redact_text(value).value),
            artifacts: normalized_paths(&args.artifact),
        });
        Ok(())
    }) {
        Ok(result) => render_record_success(
            RECORD_FINAL_SCHEMA_VERSION,
            RECORD_FINAL_COMMAND,
            format,
            &result,
        ),
        Err(err) => render_error(
            RECORD_FINAL_SCHEMA_VERSION,
            RECORD_FINAL_COMMAND,
            format,
            err,
        ),
    }
}

fn run_verify(args: CommonArgs) -> i32 {
    match verify_record(&args) {
        Ok(result) if result.missing.is_empty() => render_verify_success(args.format, &result),
        Ok(result) => render_error(
            VERIFY_SCHEMA_VERSION,
            VERIFY_COMMAND,
            args.format,
            CliError::runtime(
                "incomplete-evidence",
                "test-first evidence record is incomplete",
                Some(json!({
                    "record_file": result.record_file,
                    "missing": result.missing,
                })),
            ),
        ),
        Err(err) => render_error(VERIFY_SCHEMA_VERSION, VERIFY_COMMAND, args.format, err),
    }
}

fn run_show(args: CommonArgs) -> i32 {
    match read_record_result(args.out_dir.as_path()) {
        Ok(result) => {
            render_record_success(SHOW_SCHEMA_VERSION, SHOW_COMMAND, args.format, &result)
        }
        Err(err) => render_error(SHOW_SCHEMA_VERSION, SHOW_COMMAND, args.format, err),
    }
}

fn init_record(args: &InitArgs) -> Result<RecordResult, CliError> {
    if args.classification.trim().is_empty() {
        return Err(CliError::usage(
            "missing-classification",
            "--classification must not be empty",
            Some(json!({ "flag": "--classification" })),
        ));
    }

    let out_dir = absolute_path(&args.common.out_dir)?;
    let record_file = out_dir.join(RECORD_FILE_NAME);
    if record_file.exists() && !args.force {
        return Err(CliError::runtime(
            "record-exists",
            format!(
                "{} already exists; pass --force to overwrite",
                record_file.display()
            ),
            Some(json!({ "record_file": display_path(&record_file), "force_flag": "--force" })),
        ));
    }

    let record = EvidenceRecord {
        schema_version: RECORD_SCHEMA_VERSION.to_string(),
        change_classification: redact_text(&args.classification).value,
        production_paths: normalized_paths(&args.production_paths),
        notes: redacted_strings(&args.notes),
        failing_test: None,
        waiver: None,
        final_validation: None,
    };
    write_record(&record_file, &record)?;
    Ok(record_result(record_file, record))
}

fn update_record<F>(out_dir: &Path, update: F) -> Result<RecordResult, CliError>
where
    F: FnOnce(&mut EvidenceRecord) -> Result<(), CliError>,
{
    let record_file = record_file_path(out_dir)?;
    let mut record = read_record(&record_file)?;
    update(&mut record)?;
    write_record(&record_file, &record)?;
    Ok(record_result(record_file, record))
}

fn verify_record(args: &CommonArgs) -> Result<VerifyResult, CliError> {
    let result = read_record_result(args.out_dir.as_path())?;
    let missing = missing_evidence_fields(&result.record);
    Ok(VerifyResult {
        record_file: result.record_file,
        complete: missing.is_empty(),
        missing,
        record: result.record,
    })
}

fn read_record_result(out_dir: &Path) -> Result<RecordResult, CliError> {
    let record_file = record_file_path(out_dir)?;
    let record = read_record(&record_file)?;
    Ok(record_result(record_file, record))
}

fn read_record(record_file: &Path) -> Result<EvidenceRecord, CliError> {
    let contents = fs::read_to_string(record_file).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            CliError::runtime(
                "missing-record",
                format!("evidence record not found: {}", record_file.display()),
                Some(json!({ "record_file": display_path(record_file) })),
            )
        } else {
            CliError::runtime(
                "record-read-failed",
                format!("failed to read {}: {err}", record_file.display()),
                Some(json!({ "record_file": display_path(record_file) })),
            )
        }
    })?;

    let record: EvidenceRecord = serde_json::from_str(&contents).map_err(|err| {
        CliError::runtime(
            "invalid-record-json",
            format!("failed to parse {}: {err}", record_file.display()),
            Some(json!({ "record_file": display_path(record_file) })),
        )
    })?;

    if record.schema_version != RECORD_SCHEMA_VERSION {
        return Err(CliError::runtime(
            "unsupported-record-version",
            format!(
                "unsupported record schema_version {}; expected {}",
                record.schema_version, RECORD_SCHEMA_VERSION
            ),
            Some(json!({
                "record_file": display_path(record_file),
                "schema_version": record.schema_version,
                "expected": RECORD_SCHEMA_VERSION
            })),
        ));
    }

    Ok(record)
}

fn write_record(record_file: &Path, record: &EvidenceRecord) -> Result<(), CliError> {
    if let Some(parent) = record_file.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            CliError::runtime(
                "record-dir-create-failed",
                format!("failed to create {}: {err}", parent.display()),
                Some(json!({ "out_dir": display_path(parent) })),
            )
        })?;
    }

    let mut contents = serde_json::to_string_pretty(record).map_err(|err| {
        CliError::runtime(
            "record-render-failed",
            format!("failed to render record JSON: {err}"),
            Some(json!({ "record_file": display_path(record_file) })),
        )
    })?;
    contents.push('\n');
    fs::write(record_file, contents).map_err(|err| {
        CliError::runtime(
            "record-write-failed",
            format!("failed to write {}: {err}", record_file.display()),
            Some(json!({ "record_file": display_path(record_file) })),
        )
    })
}

fn record_file_path(out_dir: &Path) -> Result<PathBuf, CliError> {
    Ok(absolute_path(out_dir)?.join(RECORD_FILE_NAME))
}

fn record_result(record_file: PathBuf, record: EvidenceRecord) -> RecordResult {
    let complete = missing_evidence_fields(&record).is_empty();
    RecordResult {
        record_file: display_path(&record_file),
        complete,
        record,
    }
}

fn missing_evidence_fields(record: &EvidenceRecord) -> Vec<String> {
    let mut missing = Vec::new();
    if record.failing_test.is_none() && record.waiver.is_none() {
        missing.push("failing_test_or_waiver".to_string());
    }
    match record.final_validation.as_ref() {
        None => missing.push("final_validation".to_string()),
        Some(final_validation) if final_validation.status != "pass" => {
            missing.push("final_validation_pass".to_string());
        }
        Some(_) => {}
    }
    missing
}

fn absolute_path(path: &Path) -> Result<PathBuf, CliError> {
    if path.is_absolute() {
        return Ok(normalize_path(path));
    }

    let current_dir = env::current_dir().map_err(|err| {
        CliError::runtime(
            "cwd-unavailable",
            format!("failed to read current directory: {err}"),
            None,
        )
    })?;
    Ok(normalize_path(&current_dir.join(path)))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn normalized_paths(paths: &[PathBuf]) -> Vec<String> {
    let mut normalized = BTreeSet::new();
    for path in paths {
        let value = path.to_string_lossy().replace('\\', "/");
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            normalized.insert(redact_text(trimmed).value);
        }
    }
    normalized.into_iter().collect()
}

fn redacted_strings(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| redact_text(value).value)
        .filter(|value| !value.trim().is_empty())
        .collect()
}

fn redact_text(input: &str) -> RedactedString {
    let mut value = input.to_string();
    let mut replacements = 0usize;

    let assignment_replacements = assignment_secret_regex().captures_iter(&value).count();
    if assignment_replacements > 0 {
        value = assignment_secret_regex()
            .replace_all(&value, "${key}${after_key}[REDACTED]")
            .to_string();
        replacements += assignment_replacements;
    }

    let token_replacements = token_secret_regex().find_iter(&value).count();
    if token_replacements > 0 {
        value = token_secret_regex()
            .replace_all(&value, "[REDACTED]")
            .to_string();
        replacements += token_replacements;
    }

    RedactedString {
        value,
        replacements,
    }
}

fn assignment_secret_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?ix)
            (?P<key>\b(?:access[_-]?token|refresh[_-]?token|api[_-]?key|apikey|authorization|cookie|password|secret|session[_-]?id|token)\b)
            (?P<after_key>"?\s*[:=]\s*)
            (?P<value>"[^"]*"|'[^']*'|[^\s,;&}\]]+)
            "#,
        )
        .expect("valid assignment secret regex")
    })
}

fn token_secret_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?ix)
            \b(
                sk-(?:proj-)?[A-Za-z0-9_-]{8,}
                | ghp_[A-Za-z0-9_]{8,}
                | github_pat_[A-Za-z0-9_]{8,}
                | xox[baprs]-[A-Za-z0-9-]{8,}
                | bearer\s+[A-Za-z0-9._~+/=-]{8,}
            )\b
            "#,
        )
        .expect("valid token secret regex")
    })
}

fn render_record_success(
    schema_version: &'static str,
    command: &'static str,
    format: OutputFormat,
    result: &RecordResult,
) -> i32 {
    match format {
        OutputFormat::Json => {
            print_json_success(schema_version, command, result).unwrap_or_else(render_json_failure)
        }
        OutputFormat::Text => {
            println!("test-first evidence record: {}", result.record_file);
            print_record_text(&result.record, result.complete);
            EXIT_OK
        }
    }
}

fn render_verify_success(format: OutputFormat, result: &VerifyResult) -> i32 {
    match format {
        OutputFormat::Json => print_json_success(VERIFY_SCHEMA_VERSION, VERIFY_COMMAND, result)
            .unwrap_or_else(render_json_failure),
        OutputFormat::Text => {
            println!("test-first evidence complete: {}", result.record_file);
            EXIT_OK
        }
    }
}

fn print_record_text(record: &EvidenceRecord, complete: bool) {
    println!("complete: {complete}");
    println!("change classification: {}", record.change_classification);
    if !record.production_paths.is_empty() {
        println!("production paths:");
        for path in &record.production_paths {
            println!("  - {path}");
        }
    }
    println!(
        "before-fix evidence: {}",
        if record.failing_test.is_some() {
            "failing-test"
        } else if record.waiver.is_some() {
            "waiver"
        } else {
            "missing"
        }
    );
    println!(
        "final validation: {}",
        record
            .final_validation
            .as_ref()
            .map(|value| value.status.as_str())
            .unwrap_or("missing")
    );
}

fn render_error(
    schema_version: &'static str,
    command: &'static str,
    format: OutputFormat,
    err: CliError,
) -> i32 {
    if format == OutputFormat::Json {
        return print_json_error(
            schema_version,
            command,
            err.code,
            &err.message,
            err.details,
            err.exit_code,
        )
        .unwrap_or_else(render_json_failure);
    }

    eprintln!("test-first-evidence: error: {}", err.message);
    err.exit_code
}

fn print_json_success<T: Serialize>(
    schema_version: &'static str,
    command: &'static str,
    result: &T,
) -> Result<i32, serde_json::Error> {
    let envelope = SuccessEnvelope {
        schema_version,
        command,
        ok: true,
        result,
    };
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(EXIT_OK)
}

fn print_json_error(
    schema_version: &'static str,
    command: &'static str,
    code: &'static str,
    message: &str,
    details: Option<Value>,
    exit_code: i32,
) -> Result<i32, serde_json::Error> {
    let envelope = ErrorEnvelope {
        schema_version,
        command,
        ok: false,
        error: ErrorBody {
            code,
            message,
            details,
        },
    };
    println!("{}", serde_json::to_string_pretty(&envelope)?);
    Ok(exit_code)
}

fn render_json_failure(err: serde_json::Error) -> i32 {
    eprintln!("test-first-evidence: error: failed to render json: {err}");
    EXIT_RUNTIME
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[derive(Debug)]
struct CliError {
    code: &'static str,
    message: String,
    details: Option<Value>,
    exit_code: i32,
}

impl CliError {
    fn usage(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            exit_code: EXIT_USAGE,
        }
    }

    fn runtime(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            exit_code: EXIT_RUNTIME,
        }
    }
}

struct RedactedString {
    value: String,
    #[allow(dead_code)]
    replacements: usize,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct EvidenceRecord {
    pub schema_version: String,
    pub change_classification: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub production_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failing_test: Option<FailingEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiver: Option<WaiverEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_validation: Option<FinalValidation>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FailingEvidence {
    pub command: String,
    pub exit_code: i32,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WaiverEvidence {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub substitute_validation: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FinalValidation {
    pub command: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RecordResult {
    pub record_file: String,
    pub complete: bool,
    pub record: EvidenceRecord,
}

#[derive(Debug, Serialize)]
pub struct VerifyResult {
    pub record_file: String,
    pub complete: bool,
    pub missing: Vec<String>,
    pub record: EvidenceRecord,
}

#[derive(Serialize)]
struct SuccessEnvelope<'a, T: Serialize> {
    schema_version: &'static str,
    command: &'static str,
    ok: bool,
    result: &'a T,
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    schema_version: &'static str,
    command: &'static str,
    ok: bool,
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'static str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::{missing_evidence_fields, redact_text};

    #[test]
    fn redacts_secret_like_tokens_and_assignments() {
        let redacted = redact_text("OPENAI_API_KEY=sk-proj-supersecret token: abcdefghijklmnop");
        assert!(redacted.value.contains("[REDACTED]"));
        assert!(!redacted.value.contains("sk-proj-supersecret"));
        assert!(!redacted.value.contains("abcdefghijklmnop"));
    }

    #[test]
    fn complete_record_needs_before_and_final_pass() {
        let record = super::EvidenceRecord {
            schema_version: super::RECORD_SCHEMA_VERSION.to_string(),
            change_classification: "bug-fix".to_string(),
            production_paths: Vec::new(),
            notes: Vec::new(),
            failing_test: None,
            waiver: None,
            final_validation: None,
        };
        assert_eq!(
            missing_evidence_fields(&record),
            vec![
                "failing_test_or_waiver".to_string(),
                "final_validation".to_string()
            ]
        );
    }
}

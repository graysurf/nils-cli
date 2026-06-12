use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::common::{
    CliError, OutputFormat, absolute_path, display_path, ensure_non_empty, normalized_paths,
    record_path, redact_strings, redact_text, render_error, render_success, write_json_pretty,
};
use crate::completion::{self, CompletionShell};

const RECORD_SCHEMA_VERSION: &str = "skill-usage.record.v1";
const RECORD_FILE: &str = "skill-usage.record.json";
const INIT_SCHEMA_VERSION: &str = "cli.skill-usage.init.v1";
const LINK_RECORD_SCHEMA_VERSION: &str = "cli.skill-usage.link-record.v1";
const FAILURE_SCHEMA_VERSION: &str = "cli.skill-usage.record-failure.v1";
const VALIDATION_SCHEMA_VERSION: &str = "cli.skill-usage.record-validation.v1";
const OUTCOME_SCHEMA_VERSION: &str = "cli.skill-usage.record-outcome.v1";
const VERIFY_SCHEMA_VERSION: &str = "cli.skill-usage.verify.v1";
const SHOW_SCHEMA_VERSION: &str = "cli.skill-usage.show.v1";
const INIT_COMMAND: &str = "skill-usage init";
const LINK_RECORD_COMMAND: &str = "skill-usage link-record";
const FAILURE_COMMAND: &str = "skill-usage record-failure";
const VALIDATION_COMMAND: &str = "skill-usage record-validation";
const OUTCOME_COMMAND: &str = "skill-usage record-outcome";
const VERIFY_COMMAND: &str = "skill-usage verify";
const SHOW_COMMAND: &str = "skill-usage show";

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
        Err(err) => return crate::common::handle_parse_error("skill-usage", argv, err),
    };

    match cli.command {
        Command::Init(args) => init(args),
        Command::LinkRecord(args) => link_record(args),
        Command::RecordFailure(args) => record_failure(args),
        Command::RecordValidation(args) => record_validation(args),
        Command::RecordOutcome(args) => record_outcome(args),
        Command::Verify(args) => verify(args),
        Command::Show(args) => show(args),
        Command::Completion(args) => completion::run::<Cli>(args.shell, "skill-usage"),
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

fn link_record(args: LinkRecordArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        ensure_non_empty("--type", &args.record_type)?;
        record.linked_records.push(LinkedRecord {
            record_type: redact_text(&args.record_type),
            path: redact_text(&display_path(&args.path)),
        });
        Ok(())
    }) {
        Ok(result) => render_success(
            LINK_RECORD_SCHEMA_VERSION,
            LINK_RECORD_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(LINK_RECORD_SCHEMA_VERSION, LINK_RECORD_COMMAND, format, err),
    }
}

fn record_failure(args: FailureArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        ensure_non_empty("--symptom", &args.symptom)?;
        ensure_non_empty("--diagnosis", &args.diagnosis)?;
        ensure_non_empty("--handling", &args.handling)?;
        record.failures.push(Failure {
            phase: args.phase.as_str().to_string(),
            command: args.command.as_deref().map(redact_text),
            exit_code: args.exit_code,
            symptom: redact_text(&args.symptom),
            classification: args.classification.as_str().to_string(),
            diagnosis: redact_text(&args.diagnosis),
            handling: redact_text(&args.handling),
            result: args.result.as_str().to_string(),
            artifacts: redact_strings(&normalized_paths(&args.artifact)),
        });
        Ok(())
    }) {
        Ok(result) => render_success(
            FAILURE_SCHEMA_VERSION,
            FAILURE_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(FAILURE_SCHEMA_VERSION, FAILURE_COMMAND, format, err),
    }
}

fn record_validation(args: ValidationArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        ensure_non_empty("--command", &args.command)?;
        ensure_non_empty("--summary", &args.summary)?;
        record.validation.push(Validation {
            command: redact_text(&args.command),
            status: args.status.as_str().to_string(),
            summary: redact_text(&args.summary),
            artifact: args
                .artifact
                .as_ref()
                .map(|path| redact_text(&display_path(path))),
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

fn record_outcome(args: OutcomeArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        ensure_non_empty("--summary", &args.summary)?;
        record.outcome = Outcome {
            status: args.status.as_str().to_string(),
            summary: redact_text(&args.summary),
        };
        record.ended_at = Some(match &args.ended_at {
            Some(value) => redact_text(value),
            None => now_rfc3339()?,
        });
        record
            .artifacts
            .extend(redact_strings(&normalized_paths(&args.artifact)));
        record.follow_up.extend(redact_strings(&args.follow_up));
        Ok(())
    }) {
        Ok(result) => render_success(
            OUTCOME_SCHEMA_VERSION,
            OUTCOME_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(OUTCOME_SCHEMA_VERSION, OUTCOME_COMMAND, format, err),
    }
}

fn verify(args: CommonArgs) -> i32 {
    let format = args.format;
    match read_record_result(args.out_dir.as_path()) {
        Ok(result) if result.complete => render_success(
            VERIFY_SCHEMA_VERSION,
            VERIFY_COMMAND,
            format,
            || "skill-usage complete".to_string(),
            &result,
        ),
        Ok(result) => render_error(
            VERIFY_SCHEMA_VERSION,
            VERIFY_COMMAND,
            format,
            CliError::runtime(
                "incomplete-skill-usage",
                "skill usage record is incomplete or invalid",
                Some(json!({ "violations": result.violations, "record_file": result.record_file })),
            ),
        ),
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
    ensure_non_empty("--skill", &args.skill)?;
    ensure_non_empty("--intent", &args.intent)?;
    ensure_non_empty("--user-request-summary", &args.user_request_summary)?;
    let path = record_path(&args.common.out_dir, RECORD_FILE)?;
    with_record_lock(&path, || {
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

        let cwd = match &args.cwd {
            Some(path) => absolute_path(path)?,
            None => env::current_dir().map_err(|err| {
                CliError::runtime(
                    "cwd-unavailable",
                    format!("failed to read current directory: {err}"),
                    None,
                )
            })?,
        };
        let record = SkillUsageRecord {
            schema: RECORD_SCHEMA_VERSION.to_string(),
            skill: redact_text(&args.skill),
            started_at: match &args.started_at {
                Some(value) => redact_text(value),
                None => now_rfc3339()?,
            },
            ended_at: None,
            cwd: redact_text(&display_path(&cwd)),
            trigger: args.trigger.as_str().to_string(),
            intent: redact_text(&args.intent),
            inputs: Inputs {
                user_request_summary: redact_text(&args.user_request_summary),
                referenced_files: redact_strings(&normalized_paths(&args.referenced_file)),
                external_sources: redact_strings(&args.external_source),
            },
            outcome: Outcome {
                status: "skipped".to_string(),
                summary: "initialized; final outcome not recorded".to_string(),
            },
            artifacts: Vec::new(),
            linked_records: Vec::new(),
            validation_required: args.validation_waiver.as_ref().map(|_| false),
            validation_waiver: args.validation_waiver.as_deref().map(redact_text),
            validation: Vec::new(),
            failures: Vec::new(),
            follow_up: Vec::new(),
        };
        write_record(&path, &record)
    })
}

fn update_record<F>(out_dir: &Path, update: F) -> Result<RecordResult, CliError>
where
    F: FnOnce(&mut SkillUsageRecord) -> Result<(), CliError>,
{
    let path = record_path(out_dir, RECORD_FILE)?;
    with_record_lock(&path, || {
        let mut record: SkillUsageRecord = crate::common::read_json(&path)?;
        update(&mut record)?;
        write_record(&path, &record)
    })
}

fn with_record_lock<T>(
    record_file: &Path,
    operation: impl FnOnce() -> Result<T, CliError>,
) -> Result<T, CliError> {
    let _guard = RecordLock::acquire(record_file)?;
    operation()
}

struct RecordLock {
    path: PathBuf,
}

impl RecordLock {
    fn acquire(record_file: &Path) -> Result<Self, CliError> {
        let lock_dir = record_lock_path(record_file);
        if let Some(parent) = lock_dir.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                CliError::runtime(
                    "record-lock-parent-create-failed",
                    format!(
                        "failed to create record lock parent {}: {err}",
                        parent.display()
                    ),
                    Some(json!({ "path": display_path(parent) })),
                )
            })?;
        }

        let started = Instant::now();
        let timeout = Duration::from_secs(30);
        loop {
            match fs::create_dir(&lock_dir) {
                Ok(()) => return Ok(Self { path: lock_dir }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if started.elapsed() >= timeout {
                        return Err(CliError::runtime(
                            "record-lock-timeout",
                            format!(
                                "timed out waiting for exclusive record lock {}",
                                lock_dir.display()
                            ),
                            Some(json!({
                                "lock_dir": display_path(&lock_dir),
                                "record_file": display_path(record_file),
                                "timeout_seconds": timeout.as_secs()
                            })),
                        ));
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => {
                    return Err(CliError::runtime(
                        "record-lock-create-failed",
                        format!("failed to create record lock {}: {err}", lock_dir.display()),
                        Some(json!({
                            "lock_dir": display_path(&lock_dir),
                            "record_file": display_path(record_file)
                        })),
                    ));
                }
            }
        }
    }
}

impl Drop for RecordLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

fn record_lock_path(record_file: &Path) -> PathBuf {
    let file_name = record_file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(RECORD_FILE);
    record_file.with_file_name(format!("{file_name}.lock"))
}

fn write_record(path: &Path, record: &SkillUsageRecord) -> Result<RecordResult, CliError> {
    write_json_pretty(path, record)?;
    let value = serde_json::to_value(record).expect("record should serialize");
    Ok(RecordResult::new(path.to_path_buf(), value))
}

fn read_record_result(out_dir: &Path) -> Result<RecordResult, CliError> {
    let path = record_path(out_dir, RECORD_FILE)?;
    with_record_lock(&path, || {
        let value: Value = crate::common::read_json(&path)?;
        Ok(RecordResult::new(path.clone(), value))
    })
}

fn now_rfc3339() -> Result<String, CliError> {
    OffsetDateTime::now_utc().format(&Rfc3339).map_err(|err| {
        CliError::runtime(
            "timestamp-format-failed",
            format!("failed to format current timestamp: {err}"),
            None,
        )
    })
}

fn validate_record(value: &Value) -> Vec<Violation> {
    let mut violations = Vec::new();
    let Some(data) = value.as_object() else {
        violations.push(Violation::new("root_not_object", "$", "must be an object"));
        return violations;
    };

    for key in [
        "schema",
        "skill",
        "started_at",
        "cwd",
        "trigger",
        "intent",
        "inputs",
        "outcome",
        "artifacts",
        "linked_records",
        "validation",
        "follow_up",
    ] {
        if !data.contains_key(key) {
            violations.push(Violation::new(
                format!("missing_{key}"),
                format!("$.{key}"),
                "required field is missing",
            ));
        }
    }

    if data.get("schema").and_then(Value::as_str) != Some(RECORD_SCHEMA_VERSION) {
        violations.push(Violation::new(
            "invalid_schema",
            "$.schema",
            format!("must equal {RECORD_SCHEMA_VERSION:?}"),
        ));
    }

    for key in ["skill", "started_at", "cwd", "intent"] {
        if let Some(value) = data.get(key) {
            expect_nonempty_string(
                &mut violations,
                value,
                format!("$.{key}"),
                format!("invalid_{key}"),
            );
        }
    }

    if let Some(trigger) = data.get("trigger").and_then(Value::as_str)
        && !["user_explicit", "agent_selected", "project_policy", "other"].contains(&trigger)
    {
        violations.push(Violation::new(
            "invalid_trigger",
            "$.trigger",
            "must be user_explicit, agent_selected, project_policy, or other",
        ));
    }

    validate_inputs(&mut violations, data.get("inputs"));
    let outcome_status = validate_outcome(&mut violations, data.get("outcome"));

    for key in ["artifacts", "linked_records", "validation", "follow_up"] {
        if let Some(value) = data.get(key) {
            expect_array(
                &mut violations,
                value,
                format!("$.{key}"),
                format!("invalid_{key}"),
            );
        }
    }
    validate_linked_records(&mut violations, data.get("linked_records"));
    let validation_required = validate_validation_required(&mut violations, data);
    let validation_count = validate_validation_items(&mut violations, data.get("validation"));
    if validation_required && validation_count == 0 {
        violations.push(Violation::new(
            "missing_final_validation",
            "$.validation",
            "validation is required unless validation_required is false with validation_waiver",
        ));
    }
    if !validation_required {
        expect_nonempty_string(
            &mut violations,
            data.get("validation_waiver").unwrap_or(&Value::Null),
            "$.validation_waiver",
            "missing_validation_waiver",
        );
    }

    let failure_count = validate_failures(&mut violations, data.get("failures"));
    if matches!(
        outcome_status.as_deref(),
        Some("fail" | "blocked" | "worked_around" | "accepted_risk")
    ) && failure_count == 0
    {
        violations.push(Violation::new(
            "missing_failure_record",
            "$.failures",
            "non-pass/non-skipped outcomes must include at least one failure record",
        ));
    }

    scan_secret_like_values(&mut violations, value, "$".to_string());
    violations
}

fn validate_inputs(violations: &mut Vec<Violation>, value: Option<&Value>) {
    let Some(inputs) = value else {
        return;
    };
    let Some(inputs) = inputs.as_object() else {
        violations.push(Violation::new(
            "invalid_inputs",
            "$.inputs",
            "must be an object",
        ));
        return;
    };
    for key in [
        "user_request_summary",
        "referenced_files",
        "external_sources",
    ] {
        if !inputs.contains_key(key) {
            violations.push(Violation::new(
                format!("missing_input_{key}"),
                format!("$.inputs.{key}"),
                "required field is missing",
            ));
        }
    }
    if let Some(summary) = inputs.get("user_request_summary") {
        expect_string(
            violations,
            summary,
            "$.inputs.user_request_summary",
            "invalid_user_request_summary",
        );
    }
    for key in ["referenced_files", "external_sources"] {
        if let Some(value) = inputs.get(key) {
            expect_array(
                violations,
                value,
                format!("$.inputs.{key}"),
                format!("invalid_{key}"),
            );
        }
    }
}

fn validate_outcome(violations: &mut Vec<Violation>, value: Option<&Value>) -> Option<String> {
    let outcome = value?;
    let Some(outcome) = outcome.as_object() else {
        violations.push(Violation::new(
            "invalid_outcome",
            "$.outcome",
            "must be an object",
        ));
        return None;
    };
    let status = outcome.get("status").and_then(Value::as_str);
    match status {
        Some(status)
            if [
                "pass",
                "fail",
                "blocked",
                "worked_around",
                "accepted_risk",
                "skipped",
            ]
            .contains(&status) => {}
        Some(_) => violations.push(Violation::new(
            "invalid_outcome_status",
            "$.outcome.status",
            "must be pass, fail, blocked, worked_around, accepted_risk, or skipped",
        )),
        None => violations.push(Violation::new(
            "missing_outcome_status",
            "$.outcome.status",
            "required field is missing",
        )),
    }
    expect_nonempty_string(
        violations,
        outcome.get("summary").unwrap_or(&Value::Null),
        "$.outcome.summary",
        "invalid_outcome_summary",
    );
    status.map(ToOwned::to_owned)
}

fn validate_linked_records(violations: &mut Vec<Violation>, value: Option<&Value>) {
    let Some(Value::Array(items)) = value else {
        return;
    };
    for (index, item) in items.iter().enumerate() {
        let path = format!("$.linked_records[{index}]");
        let Some(item) = item.as_object() else {
            violations.push(Violation::new(
                "invalid_linked_record",
                path,
                "must be an object",
            ));
            continue;
        };
        expect_nonempty_string(
            violations,
            item.get("type").unwrap_or(&Value::Null),
            format!("$.linked_records[{index}].type"),
            "invalid_linked_record_type",
        );
        expect_nonempty_string(
            violations,
            item.get("path").unwrap_or(&Value::Null),
            format!("$.linked_records[{index}].path"),
            "invalid_linked_record_path",
        );
    }
}

fn validate_validation_required(
    violations: &mut Vec<Violation>,
    data: &serde_json::Map<String, Value>,
) -> bool {
    match data.get("validation_required") {
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            violations.push(Violation::new(
                "invalid_validation_required",
                "$.validation_required",
                "must be boolean when present",
            ));
            true
        }
        None => true,
    }
}

fn validate_validation_items(violations: &mut Vec<Violation>, value: Option<&Value>) -> usize {
    let Some(Value::Array(items)) = value else {
        return 0;
    };
    for (index, item) in items.iter().enumerate() {
        let Some(item) = item.as_object() else {
            violations.push(Violation::new(
                "invalid_validation_item",
                format!("$.validation[{index}]"),
                "must be an object",
            ));
            continue;
        };
        expect_nonempty_string(
            violations,
            item.get("command").unwrap_or(&Value::Null),
            format!("$.validation[{index}].command"),
            "invalid_validation_command",
        );
        match item.get("status").and_then(Value::as_str) {
            Some("pass" | "fail" | "skipped") => {}
            _ => violations.push(Violation::new(
                "invalid_validation_status",
                format!("$.validation[{index}].status"),
                "must be pass, fail, or skipped",
            )),
        }
        expect_nonempty_string(
            violations,
            item.get("summary").unwrap_or(&Value::Null),
            format!("$.validation[{index}].summary"),
            "invalid_validation_summary",
        );
    }
    items.len()
}

fn validate_failures(violations: &mut Vec<Violation>, value: Option<&Value>) -> usize {
    let Some(value) = value else {
        return 0;
    };
    let Some(items) = value.as_array() else {
        violations.push(Violation::new(
            "invalid_failures",
            "$.failures",
            "must be an array",
        ));
        return 0;
    };
    for (index, item) in items.iter().enumerate() {
        let Some(item) = item.as_object() else {
            violations.push(Violation::new(
                "invalid_failure",
                format!("$.failures[{index}]"),
                "must be an object",
            ));
            continue;
        };
        for key in [
            "phase",
            "symptom",
            "classification",
            "diagnosis",
            "handling",
            "result",
        ] {
            if !item.contains_key(key) {
                violations.push(Violation::new(
                    format!("missing_failure_{key}"),
                    format!("$.failures[{index}].{key}"),
                    "required field is missing",
                ));
            }
        }
        match item.get("phase").and_then(Value::as_str) {
            Some("preflight" | "execution" | "validation" | "cleanup" | "delivery") | None => {}
            _ => violations.push(Violation::new(
                "invalid_failure_phase",
                format!("$.failures[{index}].phase"),
                "must be preflight, execution, validation, cleanup, or delivery",
            )),
        }
        match item.get("classification").and_then(Value::as_str) {
            Some(
                "skill_contract" | "script_bug" | "missing_dependency" | "external_service"
                | "project_state" | "user_scope" | "unknown",
            )
            | None => {}
            _ => violations.push(Violation::new(
                "invalid_failure_classification",
                format!("$.failures[{index}].classification"),
                "must be a known failure classification",
            )),
        }
        match item.get("result").and_then(Value::as_str) {
            Some("fixed" | "worked_around" | "blocked" | "accepted_risk") | None => {}
            _ => violations.push(Violation::new(
                "invalid_failure_result",
                format!("$.failures[{index}].result"),
                "must be fixed, worked_around, blocked, or accepted_risk",
            )),
        }
        for key in ["symptom", "diagnosis", "handling"] {
            if let Some(value) = item.get(key) {
                expect_nonempty_string(
                    violations,
                    value,
                    format!("$.failures[{index}].{key}"),
                    format!("invalid_failure_{key}"),
                );
            }
        }
    }
    items.len()
}

fn expect_array(
    violations: &mut Vec<Violation>,
    value: &Value,
    path: impl Into<String>,
    kind: impl Into<String>,
) {
    if !value.is_array() {
        violations.push(Violation::new(kind, path, "must be an array"));
    }
}

fn expect_string(
    violations: &mut Vec<Violation>,
    value: &Value,
    path: impl Into<String>,
    kind: impl Into<String>,
) {
    if !value.is_string() {
        violations.push(Violation::new(kind, path, "must be a string"));
    }
}

fn expect_nonempty_string(
    violations: &mut Vec<Violation>,
    value: &Value,
    path: impl Into<String>,
    kind: impl Into<String>,
) {
    if value.as_str().is_none_or(|value| value.trim().is_empty()) {
        violations.push(Violation::new(kind, path, "must be a non-empty string"));
    }
}

fn scan_secret_like_values(violations: &mut Vec<Violation>, value: &Value, path: String) {
    match value {
        Value::Object(items) => {
            for (key, child) in items {
                scan_secret_like_values(violations, child, format!("{path}.{key}"));
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                scan_secret_like_values(violations, child, format!("{path}[{index}]"));
            }
        }
        Value::String(text) if redact_text(text) != *text => {
            violations.push(Violation::new(
                "secret_like_value",
                path,
                "contains a token, credential, cookie, or private-key-like value",
            ));
        }
        _ => {}
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "skill-usage",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Record and verify skill-usage evidence for agent workflows.",
    long_about = "Record skill invocation intent, linked evidence, validation, failures, outcome, and verification status.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  skill-usage init --out /tmp/skill --skill skills/tools/devex/review-evidence --intent 'record review evidence' --user-request-summary 'Review PR #12'\n  skill-usage record-validation --out /tmp/skill --command 'scripts/check.sh --docs' --status pass --summary 'docs passed'\n  skill-usage record-outcome --out /tmp/skill --status pass --summary 'skill completed'\n  skill-usage verify --out /tmp/skill --format json\n  skill-usage completion zsh\n\nENVIRONMENT:\n  none\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
enum Command {
    /// Create a skill usage record.
    Init(InitArgs),
    /// Link a child evidence record.
    LinkRecord(LinkRecordArgs),
    /// Append one failure entry.
    RecordFailure(FailureArgs),
    /// Append one validation entry.
    RecordValidation(ValidationArgs),
    /// Record the final skill outcome.
    RecordOutcome(OutcomeArgs),
    /// Verify the skill usage record is complete.
    Verify(CommonArgs),
    /// Print the current skill usage record.
    Show(CommonArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
struct CommonArgs {
    /// Artifact directory containing `skill-usage.record.json`.
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

    /// Skill path or skill id.
    #[arg(long, value_name = "TEXT")]
    skill: String,

    /// Skill invocation intent.
    #[arg(long, value_name = "TEXT")]
    intent: String,

    /// Trigger source.
    #[arg(long, value_enum, default_value_t = Trigger::UserExplicit)]
    trigger: Trigger,

    /// User request summary.
    #[arg(long = "user-request-summary", value_name = "TEXT")]
    user_request_summary: String,

    /// Referenced local input file. Repeat for multiple files.
    #[arg(long = "referenced-file", value_name = "PATH", value_hint = ValueHint::AnyPath)]
    referenced_file: Vec<PathBuf>,

    /// External source URL or label. Repeat for multiple sources.
    #[arg(long = "external-source", value_name = "TEXT")]
    external_source: Vec<String>,

    /// Working directory to record. Defaults to the current directory.
    #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
    cwd: Option<PathBuf>,

    /// Start timestamp. Defaults to current UTC RFC3339 time.
    #[arg(long = "started-at", value_name = "TEXT")]
    started_at: Option<String>,

    /// Explicit waiver when validation is not required.
    #[arg(long = "validation-waiver", value_name = "TEXT")]
    validation_waiver: Option<String>,

    /// Overwrite an existing record.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct LinkRecordArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Linked record type, such as `test-first-evidence`.
    #[arg(long = "type", value_name = "TEXT")]
    record_type: String,

    /// Linked record path.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    path: PathBuf,
}

#[derive(Debug, Args)]
struct FailureArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Failure phase.
    #[arg(long, value_enum)]
    phase: FailurePhase,

    /// Optional failed command.
    #[arg(long, value_name = "TEXT")]
    command: Option<String>,

    /// Optional failed command exit code.
    #[arg(long = "exit-code")]
    exit_code: Option<i32>,

    /// Observable failure symptom.
    #[arg(long, value_name = "TEXT")]
    symptom: String,

    /// Failure classification.
    #[arg(long, value_enum)]
    classification: FailureClassification,

    /// Diagnosis for the failure.
    #[arg(long, value_name = "TEXT")]
    diagnosis: String,

    /// Handling performed by the workflow.
    #[arg(long, value_name = "TEXT")]
    handling: String,

    /// Result after handling.
    #[arg(long, value_enum)]
    result: FailureResult,

    /// Optional failure artifact path. Repeat for multiple artifacts.
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

    /// Validation summary.
    #[arg(long, value_name = "TEXT")]
    summary: String,

    /// Optional validation artifact path.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    artifact: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct OutcomeArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Final outcome status.
    #[arg(long, value_enum)]
    status: OutcomeStatus,

    /// Final outcome summary.
    #[arg(long, value_name = "TEXT")]
    summary: String,

    /// End timestamp. Defaults to current UTC RFC3339 time.
    #[arg(long = "ended-at", value_name = "TEXT")]
    ended_at: Option<String>,

    /// Top-level artifact path. Repeat for multiple artifacts.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    artifact: Vec<PathBuf>,

    /// Follow-up item. Repeat for multiple items.
    #[arg(long = "follow-up", value_name = "TEXT")]
    follow_up: Vec<String>,
}

#[derive(Debug, Args)]
struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum)]
    shell: CompletionShell,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Trigger {
    #[value(name = "user-explicit", alias = "user_explicit")]
    UserExplicit,
    #[value(name = "agent-selected", alias = "agent_selected")]
    AgentSelected,
    #[value(name = "project-policy", alias = "project_policy")]
    ProjectPolicy,
    Other,
}

impl Trigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::UserExplicit => "user_explicit",
            Self::AgentSelected => "agent_selected",
            Self::ProjectPolicy => "project_policy",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum ValidationStatus {
    Pass,
    Fail,
    Skipped,
}

impl ValidationStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum OutcomeStatus {
    Pass,
    Fail,
    Blocked,
    #[value(name = "worked-around", alias = "worked_around")]
    WorkedAround,
    #[value(name = "accepted-risk", alias = "accepted_risk")]
    AcceptedRisk,
    Skipped,
}

impl OutcomeStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Blocked => "blocked",
            Self::WorkedAround => "worked_around",
            Self::AcceptedRisk => "accepted_risk",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum FailurePhase {
    Preflight,
    Execution,
    Validation,
    Cleanup,
    Delivery,
}

impl FailurePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Execution => "execution",
            Self::Validation => "validation",
            Self::Cleanup => "cleanup",
            Self::Delivery => "delivery",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum FailureClassification {
    #[value(name = "skill-contract", alias = "skill_contract")]
    SkillContract,
    #[value(name = "script-bug", alias = "script_bug")]
    ScriptBug,
    #[value(name = "missing-dependency", alias = "missing_dependency")]
    MissingDependency,
    #[value(name = "external-service", alias = "external_service")]
    ExternalService,
    #[value(name = "project-state", alias = "project_state")]
    ProjectState,
    #[value(name = "user-scope", alias = "user_scope")]
    UserScope,
    Unknown,
}

impl FailureClassification {
    fn as_str(self) -> &'static str {
        match self {
            Self::SkillContract => "skill_contract",
            Self::ScriptBug => "script_bug",
            Self::MissingDependency => "missing_dependency",
            Self::ExternalService => "external_service",
            Self::ProjectState => "project_state",
            Self::UserScope => "user_scope",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum FailureResult {
    Fixed,
    #[value(name = "worked-around", alias = "worked_around")]
    WorkedAround,
    Blocked,
    #[value(name = "accepted-risk", alias = "accepted_risk")]
    AcceptedRisk,
}

impl FailureResult {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::WorkedAround => "worked_around",
            Self::Blocked => "blocked",
            Self::AcceptedRisk => "accepted_risk",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct SkillUsageRecord {
    schema: String,
    skill: String,
    started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ended_at: Option<String>,
    cwd: String,
    trigger: String,
    intent: String,
    inputs: Inputs,
    outcome: Outcome,
    artifacts: Vec<String>,
    linked_records: Vec<LinkedRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    validation_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    validation_waiver: Option<String>,
    validation: Vec<Validation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    failures: Vec<Failure>,
    follow_up: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Inputs {
    user_request_summary: String,
    referenced_files: Vec<String>,
    external_sources: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Outcome {
    status: String,
    summary: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct LinkedRecord {
    #[serde(rename = "type")]
    record_type: String,
    path: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Validation {
    command: String,
    status: String,
    summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Failure {
    phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    symptom: String,
    classification: String,
    diagnosis: String,
    handling: String,
    result: String,
    artifacts: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RecordResult {
    record_file: String,
    complete: bool,
    violations: Vec<Violation>,
    record: Value,
}

impl RecordResult {
    fn new(path: PathBuf, record: Value) -> Self {
        let violations = validate_record(&record);
        Self {
            record_file: display_path(&path),
            complete: violations.is_empty(),
            violations,
            record,
        }
    }

    fn text_summary(&self) -> String {
        let status = self
            .record
            .get("outcome")
            .and_then(|outcome| outcome.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let validation_count = self
            .record
            .get("validation")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let failure_count = self
            .record
            .get("failures")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        format!(
            "skill-usage: complete={} status={} validation={} failures={}",
            self.complete, status, validation_count, failure_count
        )
    }
}

#[derive(Debug, Serialize)]
struct Violation {
    kind: String,
    path: String,
    message: String,
}

impl Violation {
    fn new(kind: impl Into<String>, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            path: path.into(),
            message: message.into(),
        }
    }
}

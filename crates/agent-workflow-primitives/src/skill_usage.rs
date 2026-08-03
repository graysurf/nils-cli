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
const RECORD_SCHEMA_VERSION_V2: &str = "skill-usage.record.v2";
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
    ensure_non_empty("--intent", &args.intent)?;
    ensure_non_empty("--user-request-summary", &args.user_request_summary)?;
    let (schema, skill, owner) = match (&args.skill, args.owner_kind, &args.owner_id) {
        (Some(skill), None, None) => {
            ensure_non_empty("--skill", skill)?;
            (RECORD_SCHEMA_VERSION, Some(redact_text(skill)), None)
        }
        (None, Some(kind), Some(owner_id)) => {
            ensure_non_empty("--owner-id", owner_id)?;
            (
                RECORD_SCHEMA_VERSION_V2,
                None,
                Some(Owner {
                    kind: kind.as_str().to_string(),
                    id: redact_text(owner_id),
                }),
            )
        }
        _ => {
            return Err(CliError::usage(
                "invalid-owner",
                "pass either --skill <id> or both --owner-kind <kind> and --owner-id <id>",
                Some(json!({
                    "v1": "--skill <id>",
                    "v2": "--owner-kind <skill|workflow|intent> --owner-id <id>"
                })),
            ));
        }
    };
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
            schema: schema.to_string(),
            producer: Some(Producer {
                tool: "skill-usage".to_string(),
                nils_cli_version: env!("CARGO_PKG_VERSION").to_string(),
            }),
            skill,
            owner,
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

    let schema = data.get("schema").and_then(Value::as_str);
    if !matches!(
        schema,
        Some(RECORD_SCHEMA_VERSION | RECORD_SCHEMA_VERSION_V2)
    ) {
        violations.push(Violation::new(
            "invalid_schema",
            "$.schema",
            format!("must equal {RECORD_SCHEMA_VERSION:?} or {RECORD_SCHEMA_VERSION_V2:?}"),
        ));
    }

    for key in ["started_at", "cwd", "intent"] {
        if let Some(value) = data.get(key) {
            expect_nonempty_string(
                &mut violations,
                value,
                format!("$.{key}"),
                format!("invalid_{key}"),
            );
        }
    }
    validate_owner(&mut violations, data, schema);

    if let Some(trigger) = data.get("trigger").and_then(Value::as_str)
        && !["user_explicit", "agent_selected", "project_policy", "other"].contains(&trigger)
    {
        violations.push(Violation::new(
            "invalid_trigger",
            "$.trigger",
            "must be user_explicit, agent_selected, project_policy, or other",
        ));
    }

    validate_producer(&mut violations, data.get("producer"));
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

fn validate_owner(
    violations: &mut Vec<Violation>,
    data: &serde_json::Map<String, Value>,
    schema: Option<&str>,
) {
    match schema {
        Some(RECORD_SCHEMA_VERSION) => {
            expect_nonempty_string(
                violations,
                data.get("skill").unwrap_or(&Value::Null),
                "$.skill",
                "invalid_skill",
            );
            if data.contains_key("owner") {
                violations.push(Violation::new(
                    "unexpected_owner",
                    "$.owner",
                    "v1 records use `skill`, not `owner`",
                ));
            }
        }
        Some(RECORD_SCHEMA_VERSION_V2) => {
            if data.contains_key("skill") {
                violations.push(Violation::new(
                    "unexpected_skill",
                    "$.skill",
                    "v2 records use `owner`, not `skill`",
                ));
            }
            let Some(owner) = data.get("owner").and_then(Value::as_object) else {
                violations.push(Violation::new(
                    "invalid_owner",
                    "$.owner",
                    "v2 records require an owner object",
                ));
                return;
            };
            match owner.get("kind").and_then(Value::as_str) {
                Some("skill" | "workflow" | "intent") => {}
                _ => violations.push(Violation::new(
                    "invalid_owner_kind",
                    "$.owner.kind",
                    "must be skill, workflow, or intent",
                )),
            }
            expect_nonempty_string(
                violations,
                owner.get("id").unwrap_or(&Value::Null),
                "$.owner.id",
                "invalid_owner_id",
            );
        }
        _ => {}
    }
}

fn validate_producer(violations: &mut Vec<Violation>, value: Option<&Value>) {
    // Additive field: records produced before it existed omit it and stay valid.
    let Some(producer) = value else {
        return;
    };
    let Some(producer) = producer.as_object() else {
        violations.push(Violation::new(
            "invalid_producer",
            "$.producer",
            "must be an object",
        ));
        return;
    };
    for key in ["tool", "nils_cli_version"] {
        match producer.get(key) {
            Some(field) => expect_nonempty_string(
                violations,
                field,
                format!("$.producer.{key}"),
                format!("invalid_producer_{key}"),
            ),
            None => violations.push(Violation::new(
                format!("missing_producer_{key}"),
                format!("$.producer.{key}"),
                "required when producer is present",
            )),
        }
    }
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
    #[arg(long, value_name = "TEXT", conflicts_with = "owner_kind")]
    skill: Option<String>,

    /// V2 record owner kind. Requires --owner-id and is mutually exclusive with --skill.
    #[arg(long = "owner-kind", value_enum, requires = "owner_id")]
    owner_kind: Option<OwnerKind>,

    /// V2 record owner identifier. Requires --owner-kind.
    #[arg(long = "owner-id", value_name = "TEXT", requires = "owner_kind")]
    owner_id: Option<String>,

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum OwnerKind {
    Skill,
    Workflow,
    Intent,
}

impl OwnerKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Workflow => "workflow",
            Self::Intent => "intent",
        }
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    producer: Option<Producer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<Owner>,
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
struct Owner {
    kind: String,
    id: String,
}

/// Additive provenance block stamped at record creation so an archived record
/// always carries the producing tool and nils-cli version, even after the host
/// version-pin moves. Backward compatible: records created before this field
/// existed deserialize with `producer: None`.
#[derive(Debug, Deserialize, Serialize)]
struct Producer {
    tool: String,
    nils_cli_version: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;

    /// A record that satisfies every v1 requirement. Tests mutate one field at
    /// a time so a reported violation can only come from that mutation.
    fn valid_v1() -> Value {
        json!({
            "schema": RECORD_SCHEMA_VERSION,
            "skill": "review-evidence",
            "producer": {"tool": "skill-usage", "nils_cli_version": "1.25.12"},
            "started_at": "2026-08-03T00:00:00Z",
            "ended_at": "2026-08-03T00:10:00Z",
            "cwd": "/repo",
            "trigger": "user_explicit",
            "intent": "record review evidence",
            "inputs": {
                "user_request_summary": "review the diff",
                "referenced_files": [],
                "external_sources": []
            },
            "outcome": {"status": "pass", "summary": "completed"},
            "artifacts": [],
            "linked_records": [],
            "validation": [
                {"command": "cargo test", "status": "pass", "summary": "green"}
            ],
            "follow_up": [],
            "failures": []
        })
    }

    fn kinds(value: &Value) -> Vec<String> {
        validate_record(value)
            .into_iter()
            .map(|violation| violation.kind)
            .collect()
    }

    /// Assert the mutation produces exactly the expected violation kinds.
    fn assert_kinds(value: &Value, expected: &[&str]) {
        let mut actual = kinds(value);
        actual.sort();
        let mut expected = expected
            .iter()
            .map(|k| (*k).to_string())
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(actual, expected, "record: {value}");
    }

    #[test]
    fn a_complete_v1_record_has_no_violations() {
        assert_eq!(validate_record(&valid_v1()).len(), 0);
    }

    #[test]
    fn a_complete_v2_record_uses_owner_instead_of_skill() {
        let mut record = valid_v1();
        record["schema"] = json!(RECORD_SCHEMA_VERSION_V2);
        record.as_object_mut().unwrap().remove("skill");
        record["owner"] = json!({"kind": "workflow", "id": "deliver-pr"});

        assert_kinds(&record, &[]);

        for kind in ["skill", "intent"] {
            record["owner"]["kind"] = json!(kind);
            assert_kinds(&record, &[]);
        }
    }

    #[test]
    fn a_non_object_root_is_rejected_before_any_field_check() {
        let violations = validate_record(&json!([1, 2, 3]));

        assert_eq!(violations.len(), 1, "one root violation, not a field sweep");
        assert_eq!(violations[0].kind, "root_not_object");
        assert_eq!(violations[0].path, "$");
    }

    #[test]
    fn every_required_top_level_field_is_reported_when_absent() {
        let violations = validate_record(&json!({}));
        let kinds = violations
            .iter()
            .map(|v| v.kind.as_str())
            .collect::<Vec<_>>();

        for key in [
            "schema",
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
            assert!(
                kinds.contains(&format!("missing_{key}").as_str()),
                "missing_{key} not reported: {kinds:?}"
            );
        }
        assert!(kinds.contains(&"invalid_schema"));
        assert!(kinds.contains(&"missing_final_validation"));
    }

    #[test]
    fn the_schema_discriminator_is_closed() {
        let mut record = valid_v1();
        record["schema"] = json!("skill-usage.record.v99");

        // An unknown schema disables owner checks, so only the schema is flagged.
        assert_kinds(&record, &["invalid_schema"]);
    }

    #[test]
    fn ownership_fields_are_bound_to_the_schema_version() {
        // v1 must carry `skill` and must not carry `owner`.
        let mut record = valid_v1();
        record["owner"] = json!({"kind": "skill", "id": "x"});
        assert_kinds(&record, &["unexpected_owner"]);

        let mut record = valid_v1();
        record["skill"] = json!("   ");
        assert_kinds(&record, &["invalid_skill"]);

        // v2 must carry `owner` and must not carry `skill`.
        let mut record = valid_v1();
        record["schema"] = json!(RECORD_SCHEMA_VERSION_V2);
        assert_kinds(&record, &["unexpected_skill", "invalid_owner"]);

        let mut record = valid_v1();
        record["schema"] = json!(RECORD_SCHEMA_VERSION_V2);
        record.as_object_mut().unwrap().remove("skill");
        record["owner"] = json!({"kind": "team", "id": ""});
        assert_kinds(&record, &["invalid_owner_kind", "invalid_owner_id"]);
    }

    #[test]
    fn identity_strings_must_be_non_empty() {
        for key in ["started_at", "cwd", "intent"] {
            let mut record = valid_v1();
            record[key] = json!("  ");
            assert_kinds(&record, &[&format!("invalid_{key}")]);

            let mut record = valid_v1();
            record[key] = json!(42);
            assert_kinds(&record, &[&format!("invalid_{key}")]);
        }
    }

    #[test]
    fn the_trigger_vocabulary_is_closed() {
        for trigger in ["user_explicit", "agent_selected", "project_policy", "other"] {
            let mut record = valid_v1();
            record["trigger"] = json!(trigger);
            assert_kinds(&record, &[]);
        }

        let mut record = valid_v1();
        record["trigger"] = json!("vibes");
        assert_kinds(&record, &["invalid_trigger"]);
    }

    #[test]
    fn producer_is_additive_but_complete_when_present() {
        // Records written before the field existed stay valid.
        let mut record = valid_v1();
        record.as_object_mut().unwrap().remove("producer");
        assert_kinds(&record, &[]);

        let mut record = valid_v1();
        record["producer"] = json!("skill-usage");
        assert_kinds(&record, &["invalid_producer"]);

        let mut record = valid_v1();
        record["producer"] = json!({});
        assert_kinds(
            &record,
            &["missing_producer_tool", "missing_producer_nils_cli_version"],
        );

        let mut record = valid_v1();
        record["producer"] = json!({"tool": "", "nils_cli_version": 1});
        assert_kinds(
            &record,
            &["invalid_producer_tool", "invalid_producer_nils_cli_version"],
        );
    }

    #[test]
    fn inputs_require_the_full_provenance_triple() {
        let mut record = valid_v1();
        record["inputs"] = json!("summary");
        assert_kinds(&record, &["invalid_inputs"]);

        let mut record = valid_v1();
        record["inputs"] = json!({});
        assert_kinds(
            &record,
            &[
                "missing_input_user_request_summary",
                "missing_input_referenced_files",
                "missing_input_external_sources",
            ],
        );

        let mut record = valid_v1();
        record["inputs"] = json!({
            "user_request_summary": 1,
            "referenced_files": "a.rs",
            "external_sources": {}
        });
        assert_kinds(
            &record,
            &[
                "invalid_user_request_summary",
                "invalid_referenced_files",
                "invalid_external_sources",
            ],
        );

        // An empty summary is allowed; only a non-string is not.
        let mut record = valid_v1();
        record["inputs"]["user_request_summary"] = json!("");
        assert_kinds(&record, &[]);
    }

    #[test]
    fn the_outcome_status_vocabulary_is_closed() {
        for status in ["pass", "skipped"] {
            let mut record = valid_v1();
            record["outcome"]["status"] = json!(status);
            assert_kinds(&record, &[]);
        }

        let mut record = valid_v1();
        record["outcome"] = json!("pass");
        assert_kinds(&record, &["invalid_outcome"]);

        let mut record = valid_v1();
        record["outcome"] = json!({"summary": "done"});
        assert_kinds(&record, &["missing_outcome_status"]);

        let mut record = valid_v1();
        record["outcome"] = json!({"status": "partial", "summary": ""});
        assert_kinds(
            &record,
            &["invalid_outcome_status", "invalid_outcome_summary"],
        );
    }

    #[test]
    fn a_non_pass_outcome_must_carry_at_least_one_failure_record() {
        for status in ["fail", "blocked", "worked_around", "accepted_risk"] {
            let mut record = valid_v1();
            record["outcome"]["status"] = json!(status);
            assert_kinds(&record, &["missing_failure_record"]);

            record["failures"] = json!([{
                "phase": "execution",
                "symptom": "it broke",
                "classification": "script_bug",
                "diagnosis": "off-by-one",
                "handling": "fixed the loop",
                "result": "fixed"
            }]);
            assert_kinds(&record, &[]);
        }
    }

    #[test]
    fn failure_entries_are_shape_and_vocabulary_checked() {
        let mut record = valid_v1();
        record["failures"] = json!("boom");
        assert_kinds(&record, &["invalid_failures"]);

        let mut record = valid_v1();
        record["failures"] = json!(["boom"]);
        assert_kinds(&record, &["invalid_failure"]);

        let mut record = valid_v1();
        record["failures"] = json!([{}]);
        assert_kinds(
            &record,
            &[
                "missing_failure_phase",
                "missing_failure_symptom",
                "missing_failure_classification",
                "missing_failure_diagnosis",
                "missing_failure_handling",
                "missing_failure_result",
            ],
        );

        let mut record = valid_v1();
        record["failures"] = json!([{
            "phase": "teatime",
            "symptom": "  ",
            "classification": "cosmic_rays",
            "diagnosis": 5,
            "handling": "",
            "result": "ignored"
        }]);
        assert_kinds(
            &record,
            &[
                "invalid_failure_phase",
                "invalid_failure_classification",
                "invalid_failure_result",
                "invalid_failure_symptom",
                "invalid_failure_diagnosis",
                "invalid_failure_handling",
            ],
        );
    }

    #[test]
    fn linked_records_must_name_a_type_and_a_path() {
        let mut record = valid_v1();
        record["linked_records"] = json!(["child.json"]);
        assert_kinds(&record, &["invalid_linked_record"]);

        let mut record = valid_v1();
        record["linked_records"] = json!([{}]);
        assert_kinds(
            &record,
            &["invalid_linked_record_type", "invalid_linked_record_path"],
        );

        let mut record = valid_v1();
        record["linked_records"] = json!([{"type": "test-first-evidence", "path": "child.json"}]);
        assert_kinds(&record, &[]);
    }

    #[test]
    fn validation_is_required_unless_it_is_explicitly_waived() {
        let mut record = valid_v1();
        record["validation"] = json!([]);
        assert_kinds(&record, &["missing_final_validation"]);

        // Opting out requires a written waiver.
        let mut record = valid_v1();
        record["validation"] = json!([]);
        record["validation_required"] = json!(false);
        assert_kinds(&record, &["missing_validation_waiver"]);

        let mut record = valid_v1();
        record["validation"] = json!([]);
        record["validation_required"] = json!(false);
        record["validation_waiver"] = json!("no runnable suite in this repo");
        assert_kinds(&record, &[]);

        // A non-boolean flag fails closed to "required".
        let mut record = valid_v1();
        record["validation"] = json!([]);
        record["validation_required"] = json!("false");
        assert_kinds(
            &record,
            &["invalid_validation_required", "missing_final_validation"],
        );
    }

    #[test]
    fn validation_entries_are_shape_and_vocabulary_checked() {
        let mut record = valid_v1();
        record["validation"] = json!(["cargo test"]);
        assert_kinds(&record, &["invalid_validation_item"]);

        let mut record = valid_v1();
        record["validation"] = json!([{}]);
        assert_kinds(
            &record,
            &[
                "invalid_validation_command",
                "invalid_validation_status",
                "invalid_validation_summary",
            ],
        );

        for status in ["pass", "fail", "skipped"] {
            let mut record = valid_v1();
            record["validation"] = json!([{"command": "c", "status": status, "summary": "s"}]);
            assert_kinds(&record, &[]);
        }

        let mut record = valid_v1();
        record["validation"] = json!([{"command": "c", "status": "green", "summary": "s"}]);
        assert_kinds(&record, &["invalid_validation_status"]);

        // A non-array validation field is caught by the array check, and the
        // item scan then sees nothing to count.
        let mut record = valid_v1();
        record["validation"] = json!("cargo test");
        assert_kinds(&record, &["invalid_validation", "missing_final_validation"]);
    }

    #[test]
    fn list_shaped_fields_must_actually_be_arrays() {
        for key in ["artifacts", "linked_records", "follow_up"] {
            let mut record = valid_v1();
            record[key] = json!("not-a-list");
            assert_kinds(&record, &[&format!("invalid_{key}")]);
        }
    }

    #[test]
    fn a_secret_like_value_anywhere_in_the_record_is_flagged_with_its_path() {
        let mut record = valid_v1();
        record["inputs"]["referenced_files"] = json!(["ghp_abcdefghijklmnop"]);

        let violations = validate_record(&record);
        let secret = violations
            .iter()
            .find(|v| v.kind == "secret_like_value")
            .expect("secret must be reported");
        assert_eq!(secret.path, "$.inputs.referenced_files[0]");

        // Nested objects are scanned too, and the path names the exact key.
        let mut record = valid_v1();
        record["outcome"]["summary"] = json!("Authorization: Bearer abc123");
        let violations = validate_record(&record);
        assert_eq!(
            violations
                .iter()
                .find(|v| v.kind == "secret_like_value")
                .map(|v| v.path.as_str()),
            Some("$.outcome.summary")
        );

        // Ordinary prose is not a secret.
        assert_kinds(&valid_v1(), &[]);
    }

    #[test]
    fn the_text_summary_reports_completeness_and_counts() {
        let record = valid_v1();
        let result = RecordResult::new(PathBuf::from("/out/skill-usage.record.json"), record);

        assert!(result.complete);
        assert_eq!(
            result.text_summary(),
            "skill-usage: complete=true status=pass validation=1 failures=0"
        );

        let incomplete = RecordResult::new(PathBuf::from("/out/x.json"), json!({}));
        assert!(!incomplete.complete);
        assert_eq!(
            incomplete.text_summary(),
            "skill-usage: complete=false status=unknown validation=0 failures=0"
        );
    }
}

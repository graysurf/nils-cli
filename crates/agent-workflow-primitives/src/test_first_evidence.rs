mod cli;

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use clap::Parser;
use clap::error::ErrorKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use cli::{
    ChangeClassification, CheckArgs, CheckPhase, Cli, Command, CommonArgs, InitArgs, OutputFormat,
    RecordFailingArgs, RecordFinalArgs, RecordGapArgs, RecordImpactArgs, RecordWaiverArgs,
    SubjectArgs, TestDisposition, ValidationScope, ValidationStatus, VerifyArgs, WaiverKind,
};
use nils_common::cli_contract::exit;
use nils_common::fs::{display_path, normalize_path};
use nils_common::redact::redact_text;

const EXIT_OK: i32 = exit::SUCCESS;
const EXIT_RUNTIME: i32 = exit::RUNTIME;
const EXIT_USAGE: i32 = exit::USAGE;
const EXIT_DATA: i32 = exit::DATA;

const RECORD_SCHEMA_VERSION: &str = "test-first-evidence.record.v2";
const V1_RECORD_SCHEMA_VERSION: &str = "test-first-evidence.record.v1";
const RECORD_FILE_NAME: &str = "test-first-evidence.json";

const INIT_SCHEMA_VERSION: &str = "cli.test-first-evidence.init.v2";
const RECORD_FAILING_SCHEMA_VERSION: &str = "cli.test-first-evidence.record-failing.v2";
const RECORD_IMPACT_SCHEMA_VERSION: &str = "cli.test-first-evidence.record-impact.v2";
const RECORD_WAIVER_SCHEMA_VERSION: &str = "cli.test-first-evidence.record-waiver.v2";
const RECORD_FINAL_SCHEMA_VERSION: &str = "cli.test-first-evidence.record-final.v2";
const RECORD_GAP_SCHEMA_VERSION: &str = "cli.test-first-evidence.record-gap.v2";
const BIND_BASELINE_SCHEMA_VERSION: &str = "cli.test-first-evidence.bind-baseline.v2";
const BIND_DELIVERY_SCHEMA_VERSION: &str = "cli.test-first-evidence.bind-delivery.v2";
const VERIFY_SCHEMA_VERSION: &str = "cli.test-first-evidence.verify.v2";
const SHOW_SCHEMA_VERSION: &str = "cli.test-first-evidence.show.v2";
const CHECK_SCHEMA_VERSION: &str = "cli.test-first-evidence.check.v2";

const INIT_COMMAND: &str = "test-first-evidence init";
const RECORD_FAILING_COMMAND: &str = "test-first-evidence record-failing";
const RECORD_IMPACT_COMMAND: &str = "test-first-evidence record-impact";
const RECORD_WAIVER_COMMAND: &str = "test-first-evidence record-waiver";
const RECORD_FINAL_COMMAND: &str = "test-first-evidence record-final";
const RECORD_GAP_COMMAND: &str = "test-first-evidence record-gap";
const BIND_BASELINE_COMMAND: &str = "test-first-evidence bind-baseline";
const BIND_DELIVERY_COMMAND: &str = "test-first-evidence bind-delivery";
const VERIFY_COMMAND: &str = "test-first-evidence verify";
const SHOW_COMMAND: &str = "test-first-evidence show";
const CHECK_COMMAND: &str = "test-first-evidence check";

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
        Command::RecordImpact(args) => run_record_impact(args),
        Command::RecordWaiver(args) => run_record_waiver(args),
        Command::RecordFinal(args) => run_record_final(args),
        Command::RecordGap(args) => run_record_gap(args),
        Command::BindBaseline(args) => run_bind_baseline(args),
        Command::BindDelivery(args) => run_bind_delivery(args),
        Command::Check(args) => run_check(args),
        Command::Verify(args) => run_verify(args),
        Command::Show(args) => run_show(args),
        Command::Completion(args) => {
            crate::completion::run::<Cli>(args.shell, "test-first-evidence")
        }
    }
}

fn run_check(args: CheckArgs) -> i32 {
    let format = args.common.format;
    match check(&args) {
        Ok(result) if result.allowed => render_check_success(format, &result),
        Ok(result) => render_error(
            CHECK_SCHEMA_VERSION,
            CHECK_COMMAND,
            format,
            CliError::data(
                "pre-edit-blocked",
                "test-first readiness check blocked the requested phase",
                Some(json!({
                    "phase": result.phase,
                    "path_class": result.path_class,
                    "reason_code": result.reason_code,
                    "paths": result.paths,
                })),
            ),
        ),
        Err(err) => render_error(CHECK_SCHEMA_VERSION, CHECK_COMMAND, format, err),
    }
}

fn check(args: &CheckArgs) -> Result<CheckResult, CliError> {
    let record = read_record_result(args.common.out_dir.as_path())?.record;
    if record.schema_version == RECORD_SCHEMA_VERSION
        && ChangeClassification::parse(&record.change_classification).is_none()
    {
        return Err(CliError::data(
            "invalid-change-classification",
            "v2 evidence contains an unknown change classification",
            Some(json!({ "classification": record.change_classification })),
        ));
    }
    match args.phase {
        CheckPhase::Classified => Ok(CheckResult {
            phase: args.phase.as_str().to_string(),
            path_class: "not-applicable".to_string(),
            allowed: !record.change_classification.trim().is_empty(),
            reason_code: "classification-recorded".to_string(),
            paths: Vec::new(),
        }),
        CheckPhase::Delivery => {
            if record.schema_version == V1_RECORD_SCHEMA_VERSION {
                return Err(v1_record_error(None));
            }
            let missing = missing_evidence_fields(&record);
            Ok(CheckResult {
                phase: args.phase.as_str().to_string(),
                path_class: "not-applicable".to_string(),
                allowed: missing.is_empty(),
                reason_code: if missing.is_empty() {
                    "delivery-complete"
                } else {
                    "delivery-incomplete"
                }
                .to_string(),
                paths: Vec::new(),
            })
        }
        CheckPhase::PreEdit => check_pre_edit(args, &record),
    }
}

fn check_pre_edit(args: &CheckArgs, record: &EvidenceRecord) -> Result<CheckResult, CliError> {
    let project = args.project_path.as_deref().ok_or_else(|| {
        CliError::usage(
            "missing-project-path",
            "--project-path is required for --phase pre-edit",
            Some(json!({ "flag": "--project-path" })),
        )
    })?;
    if args.path.is_empty() {
        return Err(CliError::usage(
            "missing-path",
            "at least one --path is required for --phase pre-edit",
            Some(json!({ "flag": "--path" })),
        ));
    }
    let project = absolute_path(project)?;
    let catalog = agent_docs::config::load_catalog(&project, &project).map_err(|err| {
        CliError::data(
            "path-class-catalog-invalid",
            err.to_string(),
            Some(json!({ "project_path": display_path(&project) })),
        )
    })?;
    let Some(contract) = agent_docs::path_classes::project_contract(&catalog) else {
        return Ok(CheckResult {
            phase: args.phase.as_str().to_string(),
            path_class: "not-configured".to_string(),
            allowed: true,
            reason_code: "path-classes-not-configured".to_string(),
            paths: args
                .path
                .iter()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .collect(),
        });
    };
    let mut classes = BTreeSet::new();
    let mut paths = Vec::new();
    for path in &args.path {
        let classification = contract.classify(path).map_err(|message| {
            CliError::data(
                "invalid-pre-edit-path",
                message,
                Some(json!({ "path": path.to_string_lossy() })),
            )
        })?;
        classes.insert(classification.path_class);
        paths.push(classification.path);
    }
    let path_class = if classes.len() == 1 {
        classes.iter().next().cloned().unwrap_or_default()
    } else {
        "multiple".to_string()
    };
    let has_before_fix = has_durable_pre_edit_evidence(record);
    let (allowed, reason_code) = if classes.contains("ambiguous") {
        (false, "ambiguous-path-class")
    } else if classes.contains("unknown") {
        (false, "unknown-path-class")
    } else if classes.contains("production") && !has_before_fix {
        (false, "missing-durable-pre-edit-evidence")
    } else {
        (true, "pre-edit-ready")
    };
    Ok(CheckResult {
        phase: args.phase.as_str().to_string(),
        path_class,
        allowed,
        reason_code: reason_code.to_string(),
        paths,
    })
}

fn render_check_success(format: OutputFormat, result: &CheckResult) -> i32 {
    match format {
        OutputFormat::Json => print_json_success(CHECK_SCHEMA_VERSION, CHECK_COMMAND, result)
            .unwrap_or_else(render_json_failure),
        OutputFormat::Text => {
            println!(
                "test-first check: phase={} path_class={} allowed={} reason={}",
                result.phase, result.path_class, result.allowed, result.reason_code
            );
            EXIT_OK
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
        let command = required_text(Some(&args.command), "--command", "missing-failing-command")?;
        let summary = required_text(Some(&args.summary), "--summary", "missing-failing-summary")?;
        let expected_failure = required_text(
            Some(&args.expected_failure),
            "--expected-failure",
            "missing-expected-failure",
        )?;
        let observed_failure = required_text(
            Some(&args.observed_failure),
            "--observed-failure",
            "missing-observed-failure",
        )?;
        let evidence = FailingEvidence {
            command: redact_text(command).value,
            exit_code: args.exit_code,
            summary: redact_text(summary).value,
            expected_failure: redact_text(expected_failure).value,
            observed_failure: redact_text(observed_failure).value,
            test_name: args
                .test_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| redact_text(value).value),
            artifacts: normalized_paths(&args.artifact),
        };
        let duplicate = record.failing_tests.iter().any(|item| {
            item.command.trim() == evidence.command
                && item.test_name.as_deref().map(str::trim) == evidence.test_name.as_deref()
        });
        if duplicate {
            return Err(CliError::data(
                "duplicate-failing-evidence",
                "failing evidence with the same command and test name already exists",
                None,
            ));
        }
        record.failing_tests.push(evidence);
        record.failing_tests.sort_by(|left, right| {
            (&left.test_name, &left.command).cmp(&(&right.test_name, &right.command))
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

fn run_record_impact(args: RecordImpactArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        if args.none {
            if args.target.is_some()
                || args.disposition.is_some()
                || args.protected_behavior.is_some()
                || args.owner_test.is_some()
                || args.invariant_retired
                || !args.validation_scopes.is_empty()
            {
                return Err(CliError::usage(
                    "impact-none-conflict",
                    "--none cannot be combined with target impact fields",
                    None,
                ));
            }
            let reason =
                required_text(args.reason.as_deref(), "--reason", "missing-impact-reason")?;
            if !record.test_impacts.is_empty() {
                return Err(CliError::data(
                    "impact-none-after-targets",
                    "cannot declare no existing tests after recording affected targets",
                    None,
                ));
            }
            record.no_existing_tests_reason = Some(redact_text(reason).value);
            return Ok(());
        }

        if record.no_existing_tests_reason.is_some() {
            return Err(CliError::data(
                "impact-target-after-none",
                "cannot record an affected target after declaring no existing tests",
                None,
            ));
        }
        let target = required_text(args.target.as_deref(), "--target", "missing-impact-target")?;
        let disposition = args.disposition.ok_or_else(|| {
            CliError::usage(
                "missing-impact-disposition",
                "--disposition is required unless --none is used",
                Some(json!({ "flag": "--disposition" })),
            )
        })?;
        let protected_behavior = required_text(
            args.protected_behavior.as_deref(),
            "--protected-behavior",
            "missing-protected-behavior",
        )?;
        let reason = required_text(args.reason.as_deref(), "--reason", "missing-impact-reason")?;
        let owner_test = args
            .owner_test
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| redact_text(value).value);
        if disposition == TestDisposition::RemoveSuperseded
            && owner_test.is_none()
            && !args.invariant_retired
        {
            return Err(CliError::data(
                "remove-superseded-owner-required",
                "remove-superseded requires --owner-test or --invariant-retired",
                None,
            ));
        }
        let impact = TestImpact {
            target: redact_text(target).value,
            disposition,
            protected_behavior: redact_text(protected_behavior).value,
            reason: redact_text(reason).value,
            owner_test,
            invariant_retired: args.invariant_retired,
            validation_scopes: sorted_scopes(&args.validation_scopes),
        };
        let duplicate = record.test_impacts.iter().any(|item| {
            item.target.trim() == impact.target && item.disposition == impact.disposition
        });
        if duplicate {
            return Err(CliError::data(
                "duplicate-test-impact",
                "test impact with the same target and disposition already exists",
                None,
            ));
        }
        record.test_impacts.push(impact);
        record.test_impacts.sort_by(|left, right| {
            (&left.target, left.disposition.as_str())
                .cmp(&(&right.target, right.disposition.as_str()))
        });
        Ok(())
    }) {
        Ok(result) => render_record_success(
            RECORD_IMPACT_SCHEMA_VERSION,
            RECORD_IMPACT_COMMAND,
            format,
            &result,
        ),
        Err(err) => render_error(
            RECORD_IMPACT_SCHEMA_VERSION,
            RECORD_IMPACT_COMMAND,
            format,
            err,
        ),
    }
}

fn run_record_waiver(args: RecordWaiverArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        let reason = required_text(Some(&args.reason), "--reason", "missing-waiver-reason")?;
        let why_no_red =
            required_text(Some(&args.why_no_red), "--why-no-red", "missing-why-no-red")?;
        if args.substitute_validation.is_empty()
            || args
                .substitute_validation
                .iter()
                .any(|item| item.trim().is_empty())
        {
            return Err(CliError::usage(
                "missing-substitute-validation",
                "at least one non-empty --substitute-validation is required",
                Some(json!({ "flag": "--substitute-validation" })),
            ));
        }
        if args.waiver_kind == WaiverKind::DeferredDebt
            && (args
                .follow_up
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
                || args
                    .expires
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty()))
        {
            return Err(CliError::data(
                "deferred-waiver-follow-up-required",
                "deferred-debt waiver requires --follow-up and --expires",
                None,
            ));
        }
        record.waiver = Some(WaiverEvidence {
            reason: redact_text(reason).value,
            kind: Some(args.waiver_kind),
            why_no_red: redact_text(why_no_red).value,
            substitute_validation: redacted_strings(&args.substitute_validation),
            follow_up: args
                .follow_up
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| redact_text(value).value),
            expires: args
                .expires
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| redact_text(value).value),
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
        let command = required_text(Some(&args.command), "--command", "missing-final-command")?;
        let command = redact_text(command).value;
        let next_attempt = record
            .final_validations
            .iter()
            .filter(|item| item.command.trim() == command && item.scope == Some(args.scope))
            .map(|item| item.attempt)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| {
                CliError::data(
                    "final-validation-attempt-overflow",
                    "final validation attempt counter is exhausted",
                    None,
                )
            })?;
        let validation = FinalValidation {
            command,
            status: args.status,
            scope: Some(args.scope),
            attempt: next_attempt,
            summary: args
                .summary
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| redact_text(value).value),
            artifacts: normalized_paths(&args.artifact),
        };
        let latest = record
            .final_validations
            .iter()
            .filter(|item| {
                item.command.trim() == validation.command && item.scope == validation.scope
            })
            .max_by_key(|item| item.attempt);
        if latest.is_some_and(|item| item.status == validation.status) {
            return Err(CliError::data(
                "duplicate-final-validation",
                "latest final validation attempt already has this command, scope, and status",
                None,
            ));
        }
        record.final_validations.push(validation);
        record.final_validations.sort_by(|left, right| {
            (left.scope, left.command.trim(), left.attempt).cmp(&(
                right.scope,
                right.command.trim(),
                right.attempt,
            ))
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

fn run_record_gap(args: RecordGapArgs) -> i32 {
    let format = args.common.format;
    match update_record(args.common.out_dir.as_path(), |record| {
        if args.none {
            if args.gap.is_some() || args.reason.is_some() || args.follow_up.is_some() {
                return Err(CliError::usage(
                    "gap-none-conflict",
                    "--none cannot be combined with residual gap fields",
                    None,
                ));
            }
            if !record.residual_gaps.is_empty() {
                return Err(CliError::data(
                    "gap-none-after-gaps",
                    "cannot declare no residual gaps after recording a gap",
                    None,
                ));
            }
            record.no_residual_gaps = true;
            return Ok(());
        }
        if record.no_residual_gaps {
            return Err(CliError::data(
                "gap-after-none",
                "cannot record a residual gap after declaring none",
                None,
            ));
        }
        let gap = required_text(args.gap.as_deref(), "--gap", "missing-residual-gap")?;
        let reason = required_text(args.reason.as_deref(), "--reason", "missing-gap-reason")?;
        let follow_up = required_text(
            args.follow_up.as_deref(),
            "--follow-up",
            "missing-gap-follow-up",
        )?;
        let gap = ResidualGap {
            gap: redact_text(gap).value,
            reason: redact_text(reason).value,
            follow_up: redact_text(follow_up).value,
        };
        if record
            .residual_gaps
            .iter()
            .any(|item| item.gap.trim() == gap.gap)
        {
            return Err(CliError::data(
                "duplicate-residual-gap",
                "residual gap with the same identity already exists",
                None,
            ));
        }
        record.residual_gaps.push(gap);
        record
            .residual_gaps
            .sort_by(|left, right| left.gap.trim().cmp(right.gap.trim()));
        Ok(())
    }) {
        Ok(result) => render_record_success(
            RECORD_GAP_SCHEMA_VERSION,
            RECORD_GAP_COMMAND,
            format,
            &result,
        ),
        Err(err) => render_error(RECORD_GAP_SCHEMA_VERSION, RECORD_GAP_COMMAND, format, err),
    }
}

fn run_bind_baseline(args: SubjectArgs) -> i32 {
    let format = args.common.format;
    let result = update_record(args.common.out_dir.as_path(), |record| {
        if record.subject.is_some() {
            return Err(CliError::data(
                "baseline-subject-already-bound",
                "the immutable baseline subject is already bound",
                None,
            ));
        }
        record.subject = Some(capture_baseline_subject(
            &args.project_path,
            &args.remote,
            args.repository_id.as_deref(),
        )?);
        Ok(())
    });
    match result {
        Ok(result) => render_record_success(
            BIND_BASELINE_SCHEMA_VERSION,
            BIND_BASELINE_COMMAND,
            format,
            &result,
        ),
        Err(err) => render_error(
            BIND_BASELINE_SCHEMA_VERSION,
            BIND_BASELINE_COMMAND,
            format,
            err,
        ),
    }
}

fn run_bind_delivery(args: SubjectArgs) -> i32 {
    let format = args.common.format;
    let result = update_record(args.common.out_dir.as_path(), |record| {
        let subject = record.subject.as_mut().ok_or_else(|| {
            CliError::data(
                "unbound-subject",
                "bind the repository and pre-edit baseline before attesting delivery",
                None,
            )
        })?;
        let repository = repository_identity(
            &args.project_path,
            &args.remote,
            args.repository_id.as_deref(),
        )?;
        if !repository_matches(
            &repository,
            &subject.repository,
            args.repository_id.is_some(),
        ) {
            return Err(CliError::data(
                "subject-mismatch",
                "the current repository does not match the bound baseline repository",
                Some(json!({ "reason_code": "repository-mismatch" })),
            ));
        }
        let attempt = subject
            .deliveries
            .iter()
            .map(|delivery| delivery.attempt)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| {
                CliError::data(
                    "delivery-subject-attempt-overflow",
                    "delivery subject attempt counter is exhausted",
                    None,
                )
            })?;
        let delivery =
            capture_delivery_subject(&args.project_path, &subject.baseline.commit, attempt)?;
        if subject.deliveries.last().is_some_and(|latest| {
            latest.head == delivery.head
                && latest.tree == delivery.tree
                && latest.diff_digest == delivery.diff_digest
        }) {
            return Err(CliError::data(
                "duplicate-delivery-subject",
                "the latest delivery subject already attests this head and diff",
                None,
            ));
        }
        subject.deliveries.push(delivery);
        Ok(())
    });
    match result {
        Ok(result) => render_record_success(
            BIND_DELIVERY_SCHEMA_VERSION,
            BIND_DELIVERY_COMMAND,
            format,
            &result,
        ),
        Err(err) => render_error(
            BIND_DELIVERY_SCHEMA_VERSION,
            BIND_DELIVERY_COMMAND,
            format,
            err,
        ),
    }
}

fn run_verify(args: VerifyArgs) -> i32 {
    match verify_record(&args.common) {
        Ok(result) if !result.missing.is_empty() => render_error(
            VERIFY_SCHEMA_VERSION,
            VERIFY_COMMAND,
            args.common.format,
            CliError::runtime(
                "incomplete-evidence",
                "test-first evidence record is incomplete",
                Some(json!({
                    "record_file": result.record_file,
                    "missing": result.missing,
                })),
            ),
        ),
        Ok(result) if args.project_path.is_none() => {
            render_verify_success(args.common.format, &result)
        }
        Ok(result) => {
            let project_path = args.project_path.as_deref().expect("checked above");
            match match_delivery_subject(
                &result.record,
                project_path,
                &args.remote,
                args.repository_id.as_deref(),
            ) {
                Ok(subject) if subject.matches => {
                    render_verify_success(args.common.format, &result)
                }
                Ok(subject) => render_error(
                    VERIFY_SCHEMA_VERSION,
                    VERIFY_COMMAND,
                    args.common.format,
                    CliError::data(
                        if subject.reason_code == "unbound-subject"
                            || subject.reason_code == "delivery-subject-unbound"
                        {
                            "unbound-subject"
                        } else {
                            "subject-mismatch"
                        },
                        "test-first evidence subject does not match the current delivery",
                        Some(json!({ "reason_code": subject.reason_code })),
                    ),
                ),
                Err(err) => render_error(
                    VERIFY_SCHEMA_VERSION,
                    VERIFY_COMMAND,
                    args.common.format,
                    err,
                ),
            }
        }
        Err(err) => render_error(
            VERIFY_SCHEMA_VERSION,
            VERIFY_COMMAND,
            args.common.format,
            err,
        ),
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
        change_classification: args.classification.as_str().to_string(),
        production_paths: normalized_paths(&args.production_paths),
        notes: redacted_strings(&args.notes),
        contract_delta: ContractDelta {
            retained_behaviors: sorted_redacted_strings(&args.retained_behaviors),
            changed_behaviors: sorted_redacted_strings(&args.changed_behaviors),
            removed_behaviors: sorted_redacted_strings(&args.removed_behaviors),
            added_behaviors: sorted_redacted_strings(&args.added_behaviors),
            invariants: sorted_redacted_strings(&args.invariant),
        },
        test_impacts: Vec::new(),
        no_existing_tests_reason: None,
        failing_tests: Vec::new(),
        failing_test: None,
        waiver: None,
        final_validations: Vec::new(),
        final_validation: None,
        residual_gaps: Vec::new(),
        no_residual_gaps: false,
        subject: None,
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
    if record.schema_version == V1_RECORD_SCHEMA_VERSION {
        return Err(v1_record_error(None));
    }
    update(&mut record)?;
    write_record(&record_file, &record)?;
    Ok(record_result(record_file, record))
}

fn verify_record(args: &CommonArgs) -> Result<VerifyResult, CliError> {
    let result = read_record_result(args.out_dir.as_path())?;
    if result.record.schema_version == V1_RECORD_SCHEMA_VERSION {
        return Err(v1_record_error(Some(result.record_file)));
    }
    let missing = missing_evidence_fields(&result.record);
    Ok(VerifyResult {
        record_file: result.record_file,
        complete: missing.is_empty(),
        missing,
        record: result.record,
    })
}

fn v1_record_error(record_file: Option<String>) -> CliError {
    CliError::data(
        "v1-evidence-record",
        "v1 evidence is read-only and must be re-recorded as v2 for strict checks",
        Some(json!({
            "record_file": record_file,
            "schema_version": V1_RECORD_SCHEMA_VERSION,
            "expected": RECORD_SCHEMA_VERSION,
        })),
    )
}

/// Verify a test-first-evidence record directory for external callers such as
/// the `forge-cli` PR test-first gate. Returns the structured [`VerifyResult`]
/// (inspect `complete` / `missing`), or an error message when the record
/// directory is missing or unreadable. A record is `complete` when it carries
/// a failing test (non-zero exit) or an explicit waiver, plus a passing final
/// validation.
pub fn verify_dir(out_dir: &Path) -> Result<VerifyResult, String> {
    let result = read_record_result(out_dir).map_err(|err| err.message)?;
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

    if record.schema_version != RECORD_SCHEMA_VERSION
        && record.schema_version != V1_RECORD_SCHEMA_VERSION
    {
        return Err(CliError::runtime(
            "unsupported-record-version",
            format!(
                "unsupported record schema_version {}; expected {} or readable previous schema {}",
                record.schema_version, RECORD_SCHEMA_VERSION, V1_RECORD_SCHEMA_VERSION
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
    if record.schema_version == V1_RECORD_SCHEMA_VERSION {
        return vec!["record_v1_requires_rerecording".to_string()];
    }

    let mut missing = Vec::new();
    let classification = ChangeClassification::parse(&record.change_classification);
    if classification.is_none() {
        missing.push("invalid_change_classification".to_string());
    }
    let testable = classification.is_some_and(ChangeClassification::is_testable);

    if record.contract_delta.contains_blank_entry() {
        missing.push("contract_delta_blank_entry".to_string());
    }
    if testable && !record.contract_delta.has_behavior_change() {
        missing.push("changed_added_or_removed_behavior".to_string());
    }
    if testable {
        if record.test_impacts.is_empty()
            && record
                .no_existing_tests_reason
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            missing.push("test_impacts_or_none".to_string());
        }
        if !record.test_impacts.is_empty() && record.no_existing_tests_reason.is_some() {
            missing.push("test_impacts_none_conflict".to_string());
        }
        if record.test_impacts.iter().any(|impact| {
            impact.target.trim().is_empty()
                || impact.reason.trim().is_empty()
                || impact.protected_behavior.trim().is_empty()
        }) {
            missing.push("test_impact_identity_and_rationale".to_string());
        }
        if record.test_impacts.iter().any(|impact| {
            impact.disposition == TestDisposition::RemoveSuperseded
                && impact
                    .owner_test
                    .as_deref()
                    .is_none_or(|owner| owner.trim().is_empty())
                && !impact.invariant_retired
        }) {
            missing.push("remove_obsolete_owner_or_retired_invariant".to_string());
        }
        let mut identities = BTreeSet::new();
        if record
            .test_impacts
            .iter()
            .any(|impact| !identities.insert((impact.target.trim(), impact.disposition)))
        {
            missing.push("duplicate_test_impact".to_string());
        }
    }

    if let Some(waiver) = record.waiver.as_ref() {
        if waiver.reason.trim().is_empty()
            || waiver.kind.is_none()
            || waiver.why_no_red.trim().is_empty()
            || waiver.substitute_validation.is_empty()
            || waiver
                .substitute_validation
                .iter()
                .any(|item| item.trim().is_empty())
        {
            missing.push("complete_waiver".to_string());
        }
        if waiver.kind == Some(WaiverKind::DeferredDebt)
            && (waiver
                .follow_up
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
                || waiver
                    .expires
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty()))
        {
            missing.push("deferred_waiver_follow_up".to_string());
        }
    } else {
        if record.failing_tests.is_empty() {
            missing.push("failing_tests_or_waiver".to_string());
        } else {
            if record.failing_tests.iter().any(|item| item.exit_code == 0) {
                missing.push("failing_tests_nonzero_exit".to_string());
            }
            if record.failing_tests.iter().any(|item| {
                item.command.trim().is_empty()
                    || item.summary.trim().is_empty()
                    || item.expected_failure.trim().is_empty()
                    || item.observed_failure.trim().is_empty()
                    || item
                        .test_name
                        .as_deref()
                        .is_some_and(|name| name.trim().is_empty())
            }) {
                missing.push("meaningful_failing_evidence".to_string());
            }
            let mut identities = BTreeSet::new();
            if record.failing_tests.iter().any(|item| {
                !identities.insert((
                    item.test_name.as_deref().map(str::trim),
                    item.command.trim(),
                ))
            }) {
                missing.push("duplicate_failing_evidence".to_string());
            }
        }
    }

    if record
        .final_validations
        .iter()
        .any(|item| item.command.trim().is_empty() || item.scope.is_none() || item.attempt == 0)
    {
        missing.push("final_validation_identity".to_string());
    }
    let latest_validations = effective_final_validations(record);
    if latest_validations
        .values()
        .any(|item| item.status == ValidationStatus::Fail)
    {
        missing.push("failed_final_validation".to_string());
    }
    let passing_scopes: BTreeSet<ValidationScope> = latest_validations
        .values()
        .filter(|item| item.status == ValidationStatus::Pass)
        .filter_map(|item| item.scope)
        .collect();
    if testable && !passing_scopes.contains(&ValidationScope::Focused) {
        missing.push("focused_final_validation".to_string());
    } else if !testable && passing_scopes.is_empty() {
        missing.push("final_validation_pass".to_string());
    }
    let required_scopes: BTreeSet<ValidationScope> = record
        .test_impacts
        .iter()
        .flat_map(|impact| impact.validation_scopes.iter().copied())
        .filter(|scope| {
            matches!(
                scope,
                ValidationScope::AffectedSuite | ValidationScope::ContractConsumer
            )
        })
        .collect();
    for scope in required_scopes {
        if !passing_scopes.contains(&scope) {
            missing.push(format!("{}_final_validation", scope.as_str()));
        }
    }
    let mut validation_identities = BTreeSet::new();
    if record
        .final_validations
        .iter()
        .any(|item| !validation_identities.insert((item.scope, item.command.trim(), item.attempt)))
    {
        missing.push("duplicate_final_validation".to_string());
    }

    match (record.residual_gaps.is_empty(), record.no_residual_gaps) {
        (true, false) => missing.push("residual_gaps_declaration".to_string()),
        (false, true) => missing.push("residual_gaps_none_conflict".to_string()),
        _ => {}
    }
    if record.residual_gaps.iter().any(|gap| {
        gap.gap.trim().is_empty() || gap.reason.trim().is_empty() || gap.follow_up.trim().is_empty()
    }) {
        missing.push("residual_gap_reason_and_follow_up".to_string());
    }
    let mut gap_identities = BTreeSet::new();
    if record
        .residual_gaps
        .iter()
        .any(|gap| !gap_identities.insert(gap.gap.trim()))
    {
        missing.push("duplicate_residual_gap".to_string());
    }

    missing
}

pub fn is_testable_change_classification(classification: &str) -> bool {
    ChangeClassification::parse(classification).is_some_and(ChangeClassification::is_testable)
}

fn effective_final_validations(
    record: &EvidenceRecord,
) -> BTreeMap<(Option<ValidationScope>, &str), &FinalValidation> {
    let mut latest = BTreeMap::new();
    for validation in &record.final_validations {
        let identity = (validation.scope, validation.command.trim());
        if latest
            .get(&identity)
            .is_none_or(|current: &&FinalValidation| validation.attempt > current.attempt)
        {
            latest.insert(identity, validation);
        }
    }
    latest
}

fn has_durable_pre_edit_evidence(record: &EvidenceRecord) -> bool {
    if record.schema_version != RECORD_SCHEMA_VERSION {
        return false;
    }
    let Some(classification) = ChangeClassification::parse(&record.change_classification) else {
        return false;
    };
    if !classification.is_testable() {
        return false;
    }
    missing_evidence_fields(record)
        .iter()
        .all(|field| is_post_edit_evidence_field(field))
}

fn is_post_edit_evidence_field(field: &str) -> bool {
    matches!(
        field,
        "focused_final_validation"
            | "final_validation_pass"
            | "final_validation_identity"
            | "failed_final_validation"
            | "duplicate_final_validation"
            | "residual_gaps_declaration"
            | "residual_gaps_none_conflict"
            | "residual_gap_reason_and_follow_up"
            | "duplicate_residual_gap"
    ) || field.ends_with("_final_validation")
}

#[derive(Debug, Serialize)]
pub struct SubjectMatchResult {
    pub matches: bool,
    pub reason_code: String,
}

/// Compare a structurally valid evidence record's bound repository and latest
/// delivery attestation with the current Git checkout.
pub fn verify_delivery_subject(
    out_dir: &Path,
    project_path: &Path,
    remote: &str,
    repository_id: Option<&str>,
) -> Result<SubjectMatchResult, String> {
    let record = read_record_result(out_dir)
        .map_err(|err| err.message)?
        .record;
    match_delivery_subject(&record, project_path, remote, repository_id).map_err(|err| err.message)
}

fn capture_baseline_subject(
    project_path: &Path,
    remote: &str,
    repository_id: Option<&str>,
) -> Result<EvidenceSubject, CliError> {
    let repository = repository_identity(project_path, remote, repository_id)?;
    let commit = git_output(
        project_path,
        &["rev-parse", "HEAD^{commit}"],
        "baseline commit",
    )?;
    let tree = git_output(project_path, &["rev-parse", "HEAD^{tree}"], "baseline tree")?;
    Ok(EvidenceSubject {
        repository,
        baseline: BaselineSubject { commit, tree },
        deliveries: Vec::new(),
    })
}

fn capture_delivery_subject(
    project_path: &Path,
    baseline: &str,
    attempt: u32,
) -> Result<DeliverySubject, CliError> {
    let head = git_output(
        project_path,
        &["rev-parse", "HEAD^{commit}"],
        "delivery head",
    )?;
    let tree = git_output(project_path, &["rev-parse", "HEAD^{tree}"], "delivery tree")?;
    let output = ProcessCommand::new("git")
        .current_dir(project_path)
        .args([
            "diff",
            "--binary",
            "--full-index",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            baseline,
            &head,
            "--",
        ])
        .output()
        .map_err(|err| {
            CliError::runtime(
                "subject-git-failed",
                format!("failed to spawn git while computing delivery diff: {err}"),
                None,
            )
        })?;
    if !output.status.success() {
        return Err(CliError::data(
            "subject-baseline-unavailable",
            "the bound baseline commit is unavailable in the current repository",
            Some(json!({ "reason_code": "baseline-unavailable" })),
        ));
    }
    let diff_digest = format!("sha256:{}", hex(&Sha256::digest(&output.stdout)));
    Ok(DeliverySubject {
        head,
        tree,
        diff_digest,
        attempt,
    })
}

fn match_delivery_subject(
    record: &EvidenceRecord,
    project_path: &Path,
    remote: &str,
    repository_id: Option<&str>,
) -> Result<SubjectMatchResult, CliError> {
    let Some(subject) = record.subject.as_ref() else {
        return Ok(subject_result(false, "unbound-subject"));
    };
    let current_repository = repository_identity(project_path, remote, repository_id)?;
    if !repository_matches(
        &current_repository,
        &subject.repository,
        repository_id.is_some(),
    ) {
        return Ok(subject_result(false, "repository-mismatch"));
    }
    let baseline_tree = match git_output(
        project_path,
        &[
            "rev-parse",
            &format!("{}^{{tree}}", subject.baseline.commit),
        ],
        "bound baseline tree",
    ) {
        Ok(tree) => tree,
        Err(_) => return Ok(subject_result(false, "baseline-unavailable")),
    };
    if baseline_tree != subject.baseline.tree {
        return Ok(subject_result(false, "baseline-mismatch"));
    }
    let Some(latest) = subject.deliveries.iter().max_by_key(|item| item.attempt) else {
        return Ok(subject_result(false, "delivery-subject-unbound"));
    };
    let current =
        match capture_delivery_subject(project_path, &subject.baseline.commit, latest.attempt) {
            Ok(current) => current,
            Err(err) if err.code == "subject-baseline-unavailable" => {
                return Ok(subject_result(false, "baseline-unavailable"));
            }
            Err(err) => return Err(err),
        };
    if current.head != latest.head
        || current.tree != latest.tree
        || current.diff_digest != latest.diff_digest
    {
        return Ok(subject_result(false, "delivery-subject-mismatch"));
    }
    Ok(subject_result(true, "subject-match"))
}

fn subject_result(matches: bool, reason_code: &str) -> SubjectMatchResult {
    SubjectMatchResult {
        matches,
        reason_code: reason_code.to_string(),
    }
}

fn repository_matches(
    current: &RepositoryIdentity,
    bound: &RepositoryIdentity,
    explicit_id: bool,
) -> bool {
    current.id == bound.id && (explicit_id || current.kind == bound.kind)
}

fn repository_identity(
    project_path: &Path,
    remote: &str,
    repository_id: Option<&str>,
) -> Result<RepositoryIdentity, CliError> {
    git_output(
        project_path,
        &["rev-parse", "--show-toplevel"],
        "repository root",
    )?;
    if let Some(repository_id) = repository_id {
        return Ok(RepositoryIdentity {
            kind: RepositoryIdentityKind::Explicit,
            id: normalize_repository_id(repository_id)?,
        });
    }
    if let Some(url) = git_output_optional(project_path, &["remote", "get-url", remote])
        && let Some(parsed) = nils_common::git::parse_git_remote_url(&url)
    {
        return Ok(RepositoryIdentity {
            kind: RepositoryIdentityKind::Provider,
            id: format!("{}/{}", parsed.host, parsed.path).to_ascii_lowercase(),
        });
    }
    let roots = git_output(
        project_path,
        &["rev-list", "--max-parents=0", "HEAD"],
        "repository history roots",
    )?;
    let mut roots: Vec<&str> = roots
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    roots.sort_unstable();
    if roots.is_empty() {
        return Err(CliError::data(
            "subject-repository-unavailable",
            "repository identity requires at least one commit",
            None,
        ));
    }
    let digest = Sha256::digest(roots.join("\n").as_bytes());
    Ok(RepositoryIdentity {
        kind: RepositoryIdentityKind::LocalHistory,
        id: format!("sha256:{}", hex(&digest)),
    })
}

fn normalize_repository_id(value: &str) -> Result<String, CliError> {
    let value = value.trim();
    let unsafe_shape = value.is_empty()
        || value.contains("://")
        || value.contains('@')
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.contains("..")
        || value.chars().any(char::is_whitespace);
    let redacted = redact_text(value);
    if unsafe_shape || redacted.value != value {
        return Err(CliError::usage(
            "invalid-repository-id",
            "--repository-id must be a stable path-free identifier without credentials",
            Some(json!({ "flag": "--repository-id" })),
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn git_output(project_path: &Path, args: &[&str], operation: &str) -> Result<String, CliError> {
    let output = ProcessCommand::new("git")
        .current_dir(project_path)
        .args(args)
        .output()
        .map_err(|err| {
            CliError::runtime(
                "subject-git-failed",
                format!("failed to spawn git for {operation}: {err}"),
                None,
            )
        })?;
    if !output.status.success() {
        return Err(CliError::data(
            "subject-git-failed",
            format!("git could not resolve {operation}"),
            None,
        ));
    }
    let value = String::from_utf8(output.stdout).map_err(|_| {
        CliError::data(
            "subject-git-output-invalid",
            format!("git returned non-UTF-8 output for {operation}"),
            None,
        )
    })?;
    let value = value.trim();
    if value.is_empty() {
        return Err(CliError::data(
            "subject-git-output-empty",
            format!("git returned empty output for {operation}"),
            None,
        ));
    }
    Ok(value.to_string())
}

fn git_output_optional(project_path: &Path, args: &[&str]) -> Option<String> {
    let output = ProcessCommand::new("git")
        .current_dir(project_path)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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

fn required_text<'a>(
    value: Option<&'a str>,
    flag: &'static str,
    code: &'static str,
) -> Result<&'a str, CliError> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            CliError::usage(
                code,
                format!("{flag} is required and must not be empty"),
                Some(json!({ "flag": flag })),
            )
        })
}

fn sorted_redacted_strings(values: &[String]) -> Vec<String> {
    let mut values = redacted_strings(values);
    values.sort();
    values.dedup();
    values
}

fn sorted_scopes(scopes: &[ValidationScope]) -> Vec<ValidationScope> {
    let mut scopes = scopes.to_vec();
    scopes.sort();
    scopes.dedup();
    scopes
}

fn redacted_strings(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| redact_text(value).value)
        .collect()
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
        if !record.failing_tests.is_empty() || record.failing_test.is_some() {
            "failing-test"
        } else if record.waiver.is_some() {
            "waiver"
        } else {
            "missing"
        }
    );
    println!(
        "final validation: {}",
        effective_final_validation_status(record)
    );
}

fn effective_final_validation_status(record: &EvidenceRecord) -> &'static str {
    let latest = effective_final_validations(record);
    if latest
        .values()
        .any(|value| value.status == ValidationStatus::Fail)
        || record
            .final_validation
            .as_ref()
            .is_some_and(|value| value.status == ValidationStatus::Fail)
    {
        "fail"
    } else if latest
        .values()
        .any(|value| value.status == ValidationStatus::Pass)
        || record
            .final_validation
            .as_ref()
            .is_some_and(|value| value.status == ValidationStatus::Pass)
    {
        "pass"
    } else {
        "missing"
    }
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

    fn data(code: &'static str, message: impl Into<String>, details: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            exit_code: EXIT_DATA,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct EvidenceRecord {
    pub schema_version: String,
    pub change_classification: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub production_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "ContractDelta::is_empty")]
    pub contract_delta: ContractDelta,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_impacts: Vec<TestImpact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_existing_tests_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failing_tests: Vec<FailingEvidence>,
    // Read-only v1 compatibility field. New v2 writers never populate it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failing_test: Option<FailingEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiver: Option<WaiverEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub final_validations: Vec<FinalValidation>,
    // Read-only v1 compatibility field. New v2 writers never populate it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_validation: Option<FinalValidation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub residual_gaps: Vec<ResidualGap>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_residual_gaps: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<EvidenceSubject>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceSubject {
    pub repository: RepositoryIdentity,
    pub baseline: BaselineSubject,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliveries: Vec<DeliverySubject>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryIdentity {
    pub kind: RepositoryIdentityKind,
    pub id: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepositoryIdentityKind {
    Provider,
    LocalHistory,
    Explicit,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct BaselineSubject {
    pub commit: String,
    pub tree: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeliverySubject {
    pub head: String,
    pub tree: String,
    pub diff_digest: String,
    pub attempt: u32,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ContractDelta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retained_behaviors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_behaviors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_behaviors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_behaviors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invariants: Vec<String>,
}

impl ContractDelta {
    fn is_empty(&self) -> bool {
        self.retained_behaviors.is_empty()
            && self.changed_behaviors.is_empty()
            && self.removed_behaviors.is_empty()
            && self.added_behaviors.is_empty()
            && self.invariants.is_empty()
    }

    fn has_behavior_change(&self) -> bool {
        self.changed_behaviors
            .iter()
            .chain(&self.removed_behaviors)
            .chain(&self.added_behaviors)
            .any(|value| !value.trim().is_empty())
    }

    fn contains_blank_entry(&self) -> bool {
        self.retained_behaviors
            .iter()
            .chain(&self.changed_behaviors)
            .chain(&self.removed_behaviors)
            .chain(&self.added_behaviors)
            .chain(&self.invariants)
            .any(|value| value.trim().is_empty())
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TestImpact {
    pub target: String,
    pub disposition: TestDisposition,
    pub protected_behavior: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_test: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub invariant_retired: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation_scopes: Vec<ValidationScope>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FailingEvidence {
    pub command: String,
    pub exit_code: i32,
    pub summary: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub expected_failure: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub observed_failure: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WaiverEvidence {
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<WaiverKind>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why_no_red: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub substitute_validation: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FinalValidation {
    pub command: String,
    pub status: ValidationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ValidationScope>,
    #[serde(default = "one")]
    pub attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ResidualGap {
    pub gap: String,
    pub reason: String,
    pub follow_up: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn one() -> u32 {
    1
}

#[derive(Debug, Serialize)]
struct CheckResult {
    phase: String,
    path_class: String,
    allowed: bool,
    reason_code: String,
    paths: Vec<String>,
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
    fn durable_record_reports_every_missing_structural_layer() {
        let record = super::EvidenceRecord {
            schema_version: super::RECORD_SCHEMA_VERSION.to_string(),
            change_classification: "bug-fix".to_string(),
            production_paths: Vec::new(),
            notes: Vec::new(),
            contract_delta: super::ContractDelta::default(),
            test_impacts: Vec::new(),
            no_existing_tests_reason: None,
            failing_tests: Vec::new(),
            failing_test: None,
            waiver: None,
            final_validations: Vec::new(),
            final_validation: None,
            residual_gaps: Vec::new(),
            no_residual_gaps: false,
            subject: None,
        };
        assert_eq!(
            missing_evidence_fields(&record),
            vec![
                "changed_added_or_removed_behavior".to_string(),
                "test_impacts_or_none".to_string(),
                "failing_tests_or_waiver".to_string(),
                "focused_final_validation".to_string(),
                "residual_gaps_declaration".to_string(),
            ]
        );
    }

    #[test]
    fn complete_durable_record_needs_meaningful_red_and_scoped_green() {
        let mut record = super::EvidenceRecord {
            schema_version: super::RECORD_SCHEMA_VERSION.to_string(),
            change_classification: "bug-fix".to_string(),
            production_paths: Vec::new(),
            notes: Vec::new(),
            contract_delta: super::ContractDelta {
                changed_behaviors: vec!["durable verification".to_string()],
                ..super::ContractDelta::default()
            },
            test_impacts: vec![super::TestImpact {
                target: "tests::contract".to_string(),
                disposition: super::TestDisposition::AddMissing,
                protected_behavior: "durable verification".to_string(),
                reason: "no owner exists".to_string(),
                owner_test: None,
                invariant_retired: false,
                validation_scopes: vec![super::ValidationScope::AffectedSuite],
            }],
            no_existing_tests_reason: None,
            failing_tests: vec![super::FailingEvidence {
                command: "cargo test".to_string(),
                exit_code: 0,
                summary: "all green".to_string(),
                expected_failure: "missing v2".to_string(),
                observed_failure: "unexpected green".to_string(),
                test_name: None,
                artifacts: Vec::new(),
            }],
            failing_test: None,
            waiver: None,
            final_validations: vec![
                super::FinalValidation {
                    command: "cargo test contract".to_string(),
                    status: super::ValidationStatus::Pass,
                    scope: Some(super::ValidationScope::Focused),
                    attempt: 1,
                    summary: None,
                    artifacts: Vec::new(),
                },
                super::FinalValidation {
                    command: "cargo test suite".to_string(),
                    status: super::ValidationStatus::Pass,
                    scope: Some(super::ValidationScope::AffectedSuite),
                    attempt: 1,
                    summary: None,
                    artifacts: Vec::new(),
                },
            ],
            final_validation: None,
            residual_gaps: Vec::new(),
            no_residual_gaps: true,
            subject: None,
        };
        assert_eq!(
            missing_evidence_fields(&record),
            vec!["failing_tests_nonzero_exit".to_string()],
        );

        record.failing_tests[0].exit_code = 101;
        assert!(missing_evidence_fields(&record).is_empty());
    }
}

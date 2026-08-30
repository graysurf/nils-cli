use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command as ProcessCommand;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};
use nils_markdown::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::common::{
    CliError, OutputFormat, absolute_path, display_path, render_error, render_success,
    write_json_pretty,
};
use crate::completion::{self, CompletionShell};

const TERMINAL_TEMPLATE: &str = include_str!("../templates/review_specialists/terminal.md.tera");
const TERMINAL_TEMPLATE_NAME: &str = "review_specialists_terminal";

const REPORT_TEMPLATE: &str = include_str!("../templates/review_specialists/report.md.tera");
const REPORT_TEMPLATE_NAME: &str = "review_specialists_report";

const ISSUE_BODY_TEMPLATE: &str =
    include_str!("../templates/review_specialists/issue_body.md.tera");
const ISSUE_BODY_TEMPLATE_NAME: &str = "review_specialists_issue_body";

const PR_COMMENT_TEMPLATE: &str =
    include_str!("../templates/review_specialists/pr_comment.md.tera");
const PR_COMMENT_TEMPLATE_NAME: &str = "review_specialists_pr_comment";

#[derive(Debug, Serialize)]
struct TerminalView {
    displayed: usize,
    suppressed: usize,
    merged: usize,
    threshold: String,
    findings: Vec<TerminalFindingRow>,
}

#[derive(Debug, Serialize)]
struct TerminalFindingRow {
    severity: String,
    confidence: String,
    specialist: String,
    location: String,
    summary: String,
}

#[derive(Debug, Serialize)]
struct ReportView {
    displayed: usize,
    suppressed: usize,
    merged: usize,
    dispatch_rows: Vec<DispatchRow>,
    findings: Vec<ReportFindingRow>,
    red_team_status: &'static str,
    red_team_reason: String,
    input_rows: usize,
    input_files: String,
    residual_block: String,
}

#[derive(Debug, Serialize)]
struct DispatchRow {
    specialist: String,
    status: &'static str,
    reason: String,
}

#[derive(Debug, Serialize)]
struct ReportFindingRow {
    severity: String,
    confidence: String,
    specialist: String,
    location: String,
    summary: String,
    recommendation: String,
}

#[derive(Debug, Serialize)]
struct IssueBodyView {
    displayed: usize,
    suppressed: usize,
    findings_block: String,
    input_rows: usize,
    input_files: String,
}

#[derive(Debug, Serialize)]
struct PrCommentView {
    reviewable: String,
    lens: String,
    verdict: String,
    scope: String,
    evidence_reviewed: String,
    findings: Vec<ProviderReviewFindingRow>,
}

#[derive(Debug, Serialize)]
struct ProviderReviewFindingRow {
    severity: String,
    confidence: String,
    summary: String,
    evidence: String,
    recommendation: String,
}

#[derive(Clone, Debug, Default)]
struct ProviderReviewContext {
    reviewable: String,
    lens: String,
    verdict: String,
    scope: String,
    evidence_reviewed: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct ProviderReviewThread {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<u64>,
    subject_type: String,
    body: String,
}

#[derive(Debug)]
struct ProviderReviewArtifacts {
    body: String,
    threads: Vec<ProviderReviewThread>,
}

const FINDINGS_SCHEMA: &str = "review-specialists.findings.v1";
const MERGED_SCHEMA: &str = "review-specialists.merged.v1";
const DELIVERY_FINDINGS_SCHEMA: &str = "review-specialists.findings.v2";
const DELIVERY_MERGED_SCHEMA: &str = "review-specialists.merged.v2";
const SCOPE_SCHEMA: &str = "review-specialists.scope.v1";
const VALIDATE_SCHEMA_VERSION: &str = "cli.review-specialists.validate.v1";
const MERGE_SCHEMA_VERSION: &str = "cli.review-specialists.merge.v1";
const RENDER_SCHEMA_VERSION: &str = "cli.review-specialists.render.v1";
const BUNDLE_SCHEMA_VERSION: &str = "cli.review-specialists.bundle.v1";
const SCOPE_SCHEMA_VERSION: &str = "cli.review-specialists.scope.v1";
const VALIDATE_COMMAND: &str = "review-specialists validate";
const MERGE_COMMAND: &str = "review-specialists merge";
const RENDER_COMMAND: &str = "review-specialists render";
const BUNDLE_COMMAND: &str = "review-specialists bundle";
const SCOPE_COMMAND: &str = "review-specialists scope";
const DEFAULT_DISPLAY_THRESHOLD: f64 = 0.60;

const SPECIALISTS: &[&str] = &[
    "api-contract",
    "data-migration",
    "maintainability",
    "performance",
    "quick",
    "red-team",
    "security",
    "testing",
];

const INITIAL_SPECIALISTS: &[&str] = &[
    "api-contract",
    "data-migration",
    "maintainability",
    "performance",
    "security",
    "testing",
];

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
        Err(err) => return crate::common::handle_parse_error("review-specialists", argv, err),
    };

    match cli.command {
        Command::Validate(args) => command_validate(args),
        Command::Merge(args) => command_merge(args),
        Command::Render(args) => command_render(args),
        Command::Bundle(args) => command_bundle(args),
        Command::Scope(args) => command_scope(args),
        Command::Completion(args) => completion::run::<Cli>(args.shell, "review-specialists"),
    }
}

fn command_validate(args: ValidateArgs) -> i32 {
    let format = args.common.format;
    match validate_inputs(&args.inputs, path_policy(&args), args.mode) {
        Ok(result) => render_success(
            VALIDATE_SCHEMA_VERSION,
            VALIDATE_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(VALIDATE_SCHEMA_VERSION, VALIDATE_COMMAND, format, err),
    }
}

fn command_merge(args: MergeArgs) -> i32 {
    let format = args.common.format;
    match merge_from_inputs(&args.inputs, args.display_threshold, args.mode) {
        Ok(result) => {
            if let Some(summary_out) = &args.summary_out
                && let Err(err) = write_text(
                    summary_out,
                    &render_report(&result, RenderContext::default()),
                )
            {
                return render_error(MERGE_SCHEMA_VERSION, MERGE_COMMAND, format, err);
            }
            render_success(
                MERGE_SCHEMA_VERSION,
                MERGE_COMMAND,
                format,
                || result.text_summary(),
                &result,
            )
        }
        Err(err) => render_error(MERGE_SCHEMA_VERSION, MERGE_COMMAND, format, err),
    }
}

fn command_render(args: RenderArgs) -> i32 {
    let format = args.common.format;
    if args.thread_out.is_some()
        && !matches!(
            args.profile,
            RenderProfile::ProviderReview | RenderProfile::PrComment
        )
    {
        return render_error(
            RENDER_SCHEMA_VERSION,
            RENDER_COMMAND,
            format,
            CliError::usage(
                "thread-out-requires-provider-review",
                "--thread-out requires --profile provider-review or --profile pr-comment",
                None,
            ),
        );
    }
    match read_merged(&args.input).and_then(|merged| {
        let context = RenderContext::from_args(
            args.repo.as_deref(),
            args.git_ref.as_deref(),
            args.link_base.as_deref(),
        );
        let provider_context = provider_review_context(&args.provider_review, &merged);
        render_profile(&merged, args.profile, context, provider_context).and_then(|rendered| {
            if let Some(out) = &args.out {
                write_text(out, &rendered.body)?;
            }
            if let Some(thread_out) = &args.thread_out {
                write_json_pretty(thread_out, &rendered.threads)?;
            }
            Ok(rendered)
        })
    }) {
        Ok(rendered) => {
            if matches!(format, OutputFormat::Text) && args.out.is_none() {
                print!("{}", rendered.body);
                crate::common::EXIT_OK
            } else {
                render_success(
                    RENDER_SCHEMA_VERSION,
                    RENDER_COMMAND,
                    format,
                    || rendered.text_summary(),
                    &rendered,
                )
            }
        }
        Err(err) => render_error(RENDER_SCHEMA_VERSION, RENDER_COMMAND, format, err),
    }
}

fn command_bundle(args: BundleArgs) -> i32 {
    let format = args.common.format;
    match build_bundle(&args) {
        Ok(result) => render_success(
            BUNDLE_SCHEMA_VERSION,
            BUNDLE_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(BUNDLE_SCHEMA_VERSION, BUNDLE_COMMAND, format, err),
    }
}

fn command_scope(args: ScopeArgs) -> i32 {
    let format = args.common.format;
    match detect_scope(&args) {
        Ok(result) => render_success(
            SCOPE_SCHEMA_VERSION,
            SCOPE_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(SCOPE_SCHEMA_VERSION, SCOPE_COMMAND, format, err),
    }
}

fn path_policy(args: &ValidateArgs) -> PathPolicy {
    PathPolicy {
        repo: args.repo.clone(),
        validate_paths: args.validate_paths,
        validate_lines: args.validate_lines,
    }
}

fn validate_inputs(
    inputs: &[PathBuf],
    path_policy: PathPolicy,
    mode: ReviewMode,
) -> Result<ValidateResult, CliError> {
    if inputs.is_empty() {
        return Err(CliError::usage(
            "missing-input",
            "at least one --input JSONL file is required",
            Some(json!({ "flag": "--input" })),
        ));
    }

    let mut findings = Vec::new();
    let mut errors = Vec::new();
    for input in inputs {
        match parse_jsonl(input, &path_policy, mode) {
            Ok(mut parsed) => findings.append(&mut parsed),
            Err(mut parsed_errors) => errors.append(&mut parsed_errors),
        }
    }

    if !errors.is_empty() {
        return Err(validation_error(errors));
    }

    Ok(ValidateResult {
        schema: match mode {
            ReviewMode::Advisory => FINDINGS_SCHEMA,
            ReviewMode::Delivery => DELIVERY_FINDINGS_SCHEMA,
        }
        .to_string(),
        input_files: inputs.iter().map(|path| display_path(path)).collect(),
        findings_count: findings.len(),
        findings,
    })
}

fn merge_from_inputs(
    inputs: &[PathBuf],
    display_threshold: f64,
    mode: ReviewMode,
) -> Result<MergeResult, CliError> {
    validate_threshold(display_threshold)?;
    let validated = validate_inputs(inputs, PathPolicy::default(), mode)?;
    validate_delivery_collisions(&validated.findings, mode)?;
    Ok(merge_validated(validated, display_threshold, mode))
}

fn merge_validated(
    validated: ValidateResult,
    display_threshold: f64,
    mode: ReviewMode,
) -> MergeResult {
    let mut by_fingerprint: BTreeMap<String, MergedFindingBuilder> = BTreeMap::new();
    for finding in &validated.findings {
        let lifecycle_fingerprint = match mode {
            ReviewMode::Advisory => finding.fingerprint.clone(),
            ReviewMode::Delivery => finding
                .root_cause_fingerprint
                .as_ref()
                .unwrap_or(&finding.fingerprint)
                .clone(),
        };
        by_fingerprint
            .entry(lifecycle_fingerprint.clone())
            .or_insert_with(|| {
                MergedFindingBuilder::new(finding.clone(), lifecycle_fingerprint, mode)
            })
            .push(finding.clone());
    }

    let mut findings: Vec<MergedFinding> = by_fingerprint
        .into_values()
        .map(MergedFindingBuilder::finish)
        .collect();
    findings.sort_by(merged_sort_key);

    let displayed: Vec<MergedFinding> = findings
        .iter()
        .filter(|finding| finding.primary.confidence >= display_threshold)
        .cloned()
        .collect();
    let suppressed: Vec<MergedFinding> = findings
        .iter()
        .filter(|finding| finding.primary.confidence < display_threshold)
        .cloned()
        .collect();
    let red_team = red_team_from_findings(&findings);

    MergeResult {
        schema: match mode {
            ReviewMode::Advisory => MERGED_SCHEMA,
            ReviewMode::Delivery => DELIVERY_MERGED_SCHEMA,
        }
        .to_string(),
        input_files: validated.input_files,
        display_threshold,
        counts: FindingCounts {
            input_rows: validated.findings_count,
            merged: findings.len(),
            displayed: displayed.len(),
            suppressed: suppressed.len(),
        },
        specialist_stats: specialist_stats(&findings, &displayed),
        red_team,
        findings: displayed,
        suppressed_findings: suppressed,
    }
}

fn build_bundle(args: &BundleArgs) -> Result<BundleResult, CliError> {
    validate_threshold(args.display_threshold)?;
    let validated = validate_inputs(&args.inputs, PathPolicy::default(), args.mode)?;
    validate_delivery_collisions(&validated.findings, args.mode)?;
    let merged = merge_validated(validated.clone(), args.display_threshold, args.mode);
    let report = render_report(
        &merged,
        RenderContext::from_args(
            args.repo.as_deref(),
            args.git_ref.as_deref(),
            args.link_base.as_deref(),
        ),
    );

    let normalized_jsonl = args.out_dir.join("findings.normalized.jsonl");
    let merged_json = args.out_dir.join("findings.merged.json");
    let report_md = args.out_dir.join("specialist-review.md");
    let mut artifacts = Vec::new();

    let normalized_body = render_normalized_jsonl(&validated.findings)?;
    write_text(&normalized_jsonl, &normalized_body)?;
    artifacts.push(display_path(&normalized_jsonl));
    write_json_pretty(&merged_json, &merged)?;
    artifacts.push(display_path(&merged_json));
    write_text(&report_md, &report)?;
    artifacts.push(display_path(&report_md));

    let mut profile_artifact = None;
    if let Some(profile) = args.profile {
        let provider_context = provider_review_context(&args.provider_review, &merged);
        let rendered = render_profile(
            &merged,
            profile,
            RenderContext::from_args(
                args.repo.as_deref(),
                args.git_ref.as_deref(),
                args.link_base.as_deref(),
            ),
            provider_context,
        )?;
        let file_name = match profile {
            RenderProfile::Terminal => "terminal.txt",
            RenderProfile::Report => "specialist-review.md",
            RenderProfile::IssueBody => "issue-body.md",
            RenderProfile::PrComment => "pr-comment.md",
            RenderProfile::ProviderReview => "provider-review.md",
            RenderProfile::Evidence => "evidence.json",
        };
        let path = args.out_dir.join(file_name);
        write_text(&path, &rendered.body)?;
        profile_artifact = Some(display_path(&path));
        if !artifacts.iter().any(|item| item == &display_path(&path)) {
            artifacts.push(display_path(&path));
        }
        if matches!(
            profile,
            RenderProfile::ProviderReview | RenderProfile::PrComment
        ) {
            let threads_path = args.out_dir.join("review-threads.json");
            write_json_pretty(&threads_path, &rendered.threads)?;
            artifacts.push(display_path(&threads_path));
        }
    }

    Ok(BundleResult {
        out_dir: display_path(&args.out_dir),
        artifacts,
        profile: args.profile,
        profile_artifact,
        counts: merged.counts,
    })
}

fn parse_jsonl(
    input: &Path,
    path_policy: &PathPolicy,
    mode: ReviewMode,
) -> Result<Vec<NormalizedFinding>, Vec<RowError>> {
    let body = match fs::read_to_string(input) {
        Ok(body) => body,
        Err(err) => {
            return Err(vec![RowError::new(
                display_path(input),
                0,
                format!("failed to read file: {err}"),
            )]);
        }
    };

    let mut findings = Vec::new();
    let mut errors = Vec::new();
    for (index, line) in body.lines().enumerate() {
        let line_number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        match parse_finding_line(input, line_number, line, path_policy, mode) {
            Ok(finding) => findings.push(finding),
            Err(error) => errors.push(error),
        }
    }

    if errors.is_empty() {
        Ok(findings)
    } else {
        Err(errors)
    }
}

fn parse_finding_line(
    input: &Path,
    line_number: usize,
    line: &str,
    path_policy: &PathPolicy,
    mode: ReviewMode,
) -> Result<NormalizedFinding, RowError> {
    let source_file = display_path(input);
    let value: Value = serde_json::from_str(line).map_err(|err| {
        RowError::new(
            source_file.clone(),
            line_number,
            format!("malformed JSON: {}", err),
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        RowError::new(
            source_file.clone(),
            line_number,
            "finding must be a JSON object".to_string(),
        )
    })?;

    let allowed: BTreeSet<&str> = [
        "severity",
        "confidence",
        "path",
        "line",
        "category",
        "summary",
        "evidence",
        "recommendation",
        "fingerprint",
        "root_cause_fingerprint",
        "specialist",
        "test_suggestion",
        "actionable",
    ]
    .into_iter()
    .collect();
    let unknown: Vec<String> = object
        .keys()
        .filter(|key| !allowed.contains(key.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(RowError::new(
            source_file,
            line_number,
            format!("unknown field(s): {}", unknown.join(", ")),
        ));
    }

    let missing: Vec<&str> = [
        "severity",
        "confidence",
        "path",
        "summary",
        "evidence",
        "recommendation",
        "specialist",
    ]
    .into_iter()
    .filter(|key| !object.contains_key(*key) || object[*key].is_null())
    .collect();
    if !missing.is_empty() {
        return Err(RowError::new(
            display_path(input),
            line_number,
            format!("missing required field(s): {}", missing.join(", ")),
        ));
    }

    let severity = normalize_severity(
        value_string(object, "severity", input, line_number)?,
        input,
        line_number,
    )?;
    let confidence = normalize_confidence(
        object.get("confidence").expect("checked"),
        input,
        line_number,
    )?;
    let path = required_string(object, "path", input, line_number)?;
    let specialist = required_string(object, "specialist", input, line_number)?;
    if !SPECIALISTS.contains(&specialist.as_str()) {
        return Err(RowError::new(
            display_path(input),
            line_number,
            format!("unsupported specialist {specialist:?}"),
        ));
    }
    let line_value = optional_positive_u64(object.get("line"), input, line_number)?;
    let category = optional_string(object.get("category"), "category", input, line_number)?
        .unwrap_or_else(|| specialist.clone());
    let summary = required_string(object, "summary", input, line_number)?;
    let evidence = required_string(object, "evidence", input, line_number)?;
    let recommendation = required_string(object, "recommendation", input, line_number)?;
    let test_suggestion = optional_string(
        object.get("test_suggestion"),
        "test_suggestion",
        input,
        line_number,
    )?;
    let actionable =
        optional_bool(object.get("actionable"), "actionable", input, line_number)?.unwrap_or(false);
    let explicit_fingerprint =
        optional_string(object.get("fingerprint"), "fingerprint", input, line_number)?;
    let root_cause_fingerprint = optional_string(
        object.get("root_cause_fingerprint"),
        "root_cause_fingerprint",
        input,
        line_number,
    )?;
    if mode == ReviewMode::Delivery && explicit_fingerprint.is_none() {
        return Err(RowError::typed(
            display_path(input),
            line_number,
            "review_fingerprint_required",
            "delivery findings require an explicit stable fingerprint",
        ));
    }
    if mode == ReviewMode::Delivery
        && let Some(fingerprint) = explicit_fingerprint.as_deref()
    {
        validate_stable_fingerprint(fingerprint, "fingerprint", input, line_number)?;
        if fingerprint.split(':').next() != Some(category.as_str()) {
            return Err(RowError::typed(
                display_path(input),
                line_number,
                "review_fingerprint_collision",
                format!(
                    "fingerprint category must match finding category: fingerprint={fingerprint}; category={category}"
                ),
            ));
        }
    }
    if mode == ReviewMode::Delivery
        && let Some(root) = root_cause_fingerprint.as_deref()
    {
        validate_stable_fingerprint(root, "root_cause_fingerprint", input, line_number)?;
    }
    let fingerprint = explicit_fingerprint
        .unwrap_or_else(|| computed_fingerprint(&path, line_value, &category, &summary));

    validate_path_policy(&path, line_value, path_policy, input, line_number)?;

    Ok(NormalizedFinding {
        severity,
        confidence,
        path,
        line: line_value,
        category,
        summary,
        evidence,
        recommendation,
        fingerprint,
        root_cause_fingerprint,
        specialist,
        test_suggestion,
        actionable,
        source_file: display_path(input),
        source_line: line_number,
    })
}

fn value_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    input: &Path,
    line_number: usize,
) -> Result<&'a str, RowError> {
    object.get(key).and_then(Value::as_str).ok_or_else(|| {
        RowError::new(
            display_path(input),
            line_number,
            format!("{key} must be a string"),
        )
    })
}

fn required_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
    input: &Path,
    line_number: usize,
) -> Result<String, RowError> {
    let value = value_string(object, key, input, line_number)?;
    if value.trim().is_empty() {
        return Err(RowError::new(
            display_path(input),
            line_number,
            format!("{key} must not be empty"),
        ));
    }
    Ok(value.to_string())
}

fn optional_string(
    value: Option<&Value>,
    key: &str,
    input: &Path,
    line_number: usize,
) -> Result<Option<String>, RowError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let text = value.as_str().ok_or_else(|| {
                RowError::new(
                    display_path(input),
                    line_number,
                    format!("{key} must be a string"),
                )
            })?;
            if text.trim().is_empty() {
                Ok(None)
            } else {
                Ok(Some(text.to_string()))
            }
        }
    }
}

fn optional_positive_u64(
    value: Option<&Value>,
    input: &Path,
    line_number: usize,
) -> Result<Option<u64>, RowError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => match number.as_u64() {
            Some(value) if value > 0 => Ok(Some(value)),
            _ => Err(RowError::new(
                display_path(input),
                line_number,
                "line must be a positive integer when present".to_string(),
            )),
        },
        _ => Err(RowError::new(
            display_path(input),
            line_number,
            "line must be a positive integer when present".to_string(),
        )),
    }
}

fn optional_bool(
    value: Option<&Value>,
    key: &str,
    input: &Path,
    line_number: usize,
) -> Result<Option<bool>, RowError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(RowError::new(
            display_path(input),
            line_number,
            format!("{key} must be a boolean when present"),
        )),
    }
}

fn normalize_severity(value: &str, input: &Path, line_number: usize) -> Result<Severity, RowError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "critical" | "crit" => Ok(Severity::Critical),
        "high" => Ok(Severity::High),
        "medium" | "med" => Ok(Severity::Medium),
        "low" => Ok(Severity::Low),
        "info" | "informational" => Ok(Severity::Info),
        _ => Err(RowError::new(
            display_path(input),
            line_number,
            format!("unsupported severity {value:?}; expected critical|high|medium|low|info"),
        )),
    }
}

fn normalize_confidence(value: &Value, input: &Path, line_number: usize) -> Result<f64, RowError> {
    let Some(confidence) = value.as_f64() else {
        return Err(RowError::new(
            display_path(input),
            line_number,
            "confidence must be a number from 0.0 to 1.0".to_string(),
        ));
    };
    if !(0.0..=1.0).contains(&confidence) {
        return Err(RowError::new(
            display_path(input),
            line_number,
            "confidence must be from 0.0 to 1.0".to_string(),
        ));
    }
    Ok(confidence)
}

fn validate_path_policy(
    finding_path: &str,
    line: Option<u64>,
    path_policy: &PathPolicy,
    input: &Path,
    line_number: usize,
) -> Result<(), RowError> {
    if !path_policy.validate_paths && !path_policy.validate_lines {
        return Ok(());
    }
    let Some(repo) = &path_policy.repo else {
        return Err(RowError::new(
            display_path(input),
            line_number,
            "--repo is required when --validate-paths or --validate-lines is used".to_string(),
        ));
    };
    if !safe_relative_path(finding_path) {
        return Err(RowError::new(
            display_path(input),
            line_number,
            format!("path must be relative to the repo: {finding_path}"),
        ));
    }
    let full_path = repo.join(finding_path);
    if path_policy.validate_paths && !full_path.is_file() {
        return Err(RowError::new(
            display_path(input),
            line_number,
            format!("path does not exist: {}", display_path(&full_path)),
        ));
    }
    if path_policy.validate_lines
        && let Some(line_number_value) = line
    {
        let body = fs::read_to_string(&full_path).map_err(|err| {
            RowError::new(
                display_path(input),
                line_number,
                format!(
                    "failed to read {} for line validation: {err}",
                    display_path(&full_path)
                ),
            )
        })?;
        let line_count = body.lines().count() as u64;
        if line_number_value > line_count {
            return Err(RowError::new(
                display_path(input),
                line_number,
                format!(
                    "line {line_number_value} is past end of {}",
                    display_path(&full_path)
                ),
            ));
        }
    }
    Ok(())
}

fn validation_error(errors: Vec<RowError>) -> CliError {
    let typed_code = errors
        .iter()
        .find_map(|error| error.code.as_deref())
        .unwrap_or("invalid-findings");
    CliError::data(
        typed_code,
        format!("{} finding row(s) failed validation", errors.len()),
        Some(json!({ "errors": errors })),
    )
}

fn validate_stable_fingerprint(
    fingerprint: &str,
    field: &str,
    input: &Path,
    line_number: usize,
) -> Result<(), RowError> {
    let parts = fingerprint.split(':').collect::<Vec<_>>();
    let valid = parts.len() == 3 && parts.iter().all(|part| stable_fingerprint_part(part));
    if valid {
        return Ok(());
    }
    Err(RowError::typed(
        display_path(input),
        line_number,
        "review_fingerprint_collision",
        format!("{field} must have stable <category>:<component>:<invariant> form: {fingerprint}"),
    ))
}

fn stable_fingerprint_part(part: &str) -> bool {
    !part.is_empty()
        && !part.starts_with('-')
        && !part.ends_with('-')
        && part
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn validate_delivery_collisions(
    findings: &[NormalizedFinding],
    mode: ReviewMode,
) -> Result<(), CliError> {
    if mode != ReviewMode::Delivery {
        return Ok(());
    }
    let mut identities: BTreeMap<&str, (&str, Option<&str>)> = BTreeMap::new();
    for finding in findings {
        let identity = (
            finding.category.as_str(),
            finding.root_cause_fingerprint.as_deref(),
        );
        if let Some(previous) = identities.insert(finding.fingerprint.as_str(), identity)
            && previous != identity
        {
            return Err(CliError::data(
                "review_fingerprint_collision",
                "one lifecycle fingerprint describes incompatible category or root-cause identities",
                Some(json!({
                    "fingerprint": finding.fingerprint,
                    "previous_category": previous.0,
                    "observed_category": identity.0,
                    "previous_root_cause_fingerprint": previous.1,
                    "observed_root_cause_fingerprint": identity.1,
                })),
            ));
        }
    }
    Ok(())
}

fn validate_threshold(threshold: f64) -> Result<(), CliError> {
    if !(0.0..=1.0).contains(&threshold) {
        return Err(CliError::data(
            "invalid-threshold",
            "--display-threshold must be from 0.0 to 1.0",
            Some(json!({ "display_threshold": threshold })),
        ));
    }
    Ok(())
}

fn computed_fingerprint(path: &str, line: Option<u64>, category: &str, summary: &str) -> String {
    let source = format!(
        "{}|{}|{}|{}",
        path,
        line.map(|value| value.to_string()).unwrap_or_default(),
        category,
        summary.trim().to_ascii_lowercase()
    );
    format!("{:016x}", fnv1a64(source.as_bytes()))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn merged_sort_key(left: &MergedFinding, right: &MergedFinding) -> std::cmp::Ordering {
    severity_rank(left.primary.severity)
        .cmp(&severity_rank(right.primary.severity))
        .then_with(|| {
            right
                .primary
                .confidence
                .partial_cmp(&left.primary.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| left.primary.path.cmp(&right.primary.path))
        .then_with(|| left.primary.summary.cmp(&right.primary.summary))
        .then_with(|| left.fingerprint.cmp(&right.fingerprint))
}

fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Critical => 0,
        Severity::High => 1,
        Severity::Medium => 2,
        Severity::Low => 3,
        Severity::Info => 4,
    }
}

fn specialist_stats(
    merged: &[MergedFinding],
    displayed: &[MergedFinding],
) -> BTreeMap<String, SpecialistStat> {
    let displayed_fingerprints: BTreeSet<&str> = displayed
        .iter()
        .map(|item| item.fingerprint.as_str())
        .collect();
    let mut stats: BTreeMap<String, SpecialistStat> = BTreeMap::new();
    for item in merged {
        for specialist in &item.confirming_specialists {
            let stat = stats.entry(specialist.clone()).or_default();
            stat.input += 1;
            if displayed_fingerprints.contains(item.fingerprint.as_str()) {
                stat.displayed += 1;
            } else {
                stat.suppressed += 1;
            }
        }
    }
    stats
}

fn red_team_from_findings(findings: &[MergedFinding]) -> RedTeamTrigger {
    let critical_count = findings
        .iter()
        .filter(|finding| finding.primary.severity == Severity::Critical)
        .count();
    RedTeamTrigger {
        required: critical_count > 0,
        reasons: if critical_count > 0 {
            vec![format!("{critical_count} critical finding(s)")]
        } else {
            Vec::new()
        },
        diff_lines_gt_200: false,
        critical_finding: critical_count > 0,
        forced: false,
    }
}

fn render_profile(
    merged: &MergeResult,
    profile: RenderProfile,
    context: RenderContext,
    provider_context: ProviderReviewContext,
) -> Result<RenderResult, CliError> {
    let (body, threads) = match profile {
        RenderProfile::Terminal => (render_terminal(merged, context), Vec::new()),
        RenderProfile::Report => (render_report(merged, context), Vec::new()),
        RenderProfile::IssueBody => (render_issue_body(merged, context), Vec::new()),
        RenderProfile::PrComment | RenderProfile::ProviderReview => {
            let artifacts = render_provider_review_artifacts_with_render_context(
                merged,
                &provider_context,
                &context,
            )?;
            (artifacts.body, artifacts.threads)
        }
        RenderProfile::Evidence => (render_evidence_json(merged)?, Vec::new()),
    };
    Ok(RenderResult {
        profile,
        body,
        threads,
        counts: merged.counts.clone(),
    })
}

fn render_terminal(merged: &MergeResult, context: RenderContext) -> String {
    let view = TerminalView {
        displayed: merged.counts.displayed,
        suppressed: merged.counts.suppressed,
        merged: merged.counts.merged,
        threshold: format!("{:.2}", merged.display_threshold),
        findings: merged
            .findings
            .iter()
            .map(|finding| TerminalFindingRow {
                severity: finding.primary.severity.to_string(),
                confidence: format!("{:.2}", finding.primary.confidence),
                specialist: finding.primary.specialist.clone(),
                location: format_location(&finding.primary, &context),
                summary: finding.primary.summary.clone(),
            })
            .collect(),
    };
    let mut engine = Engine::builder().build();
    engine
        .register_template(TERMINAL_TEMPLATE_NAME, TERMINAL_TEMPLATE)
        .expect("terminal template registers");
    engine
        .render(TERMINAL_TEMPLATE_NAME, &view)
        .expect("terminal template renders")
}

fn render_report(merged: &MergeResult, context: RenderContext) -> String {
    let dispatch_rows: Vec<DispatchRow> = INITIAL_SPECIALISTS
        .iter()
        .map(|specialist| {
            let stat = merged.specialist_stats.get(*specialist);
            let status = if stat.map(|item| item.input).unwrap_or(0) > 0 {
                "selected"
            } else {
                "skipped"
            };
            let reason = stat
                .map(|item| format!("{} merged finding(s)", item.input))
                .unwrap_or_else(|| "no normalized findings".to_string());
            DispatchRow {
                specialist: (*specialist).to_string(),
                status,
                reason,
            }
        })
        .collect();

    let findings: Vec<ReportFindingRow> = merged
        .findings
        .iter()
        .map(|finding| ReportFindingRow {
            severity: finding.primary.severity.to_string(),
            confidence: format!("{:.2}", finding.primary.confidence),
            specialist: finding.primary.specialist.clone(),
            location: markdown_escape(&format_location(&finding.primary, &context)),
            summary: markdown_escape(&finding.primary.summary),
            recommendation: markdown_escape(&finding.primary.recommendation),
        })
        .collect();

    let red_team_status = if merged.red_team.required {
        "required"
    } else {
        "not required"
    };
    let red_team_reason = if merged.red_team.reasons.is_empty() {
        "none".to_string()
    } else {
        merged.red_team.reasons.join(", ")
    };

    let residual_block = if merged.suppressed_findings.is_empty() {
        "- Low-confidence concerns: none suppressed by threshold".to_string()
    } else {
        merged
            .suppressed_findings
            .iter()
            .map(|finding| {
                format!(
                    "- {} ({:.2}) {}: {}",
                    finding.primary.severity,
                    finding.primary.confidence,
                    format_location(&finding.primary, &context),
                    finding.primary.summary
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let view = ReportView {
        displayed: merged.counts.displayed,
        suppressed: merged.counts.suppressed,
        merged: merged.counts.merged,
        dispatch_rows,
        findings,
        red_team_status,
        red_team_reason,
        input_rows: merged.counts.input_rows,
        input_files: merged.input_files.join(", "),
        residual_block,
    };

    let mut engine = Engine::builder().build();
    engine
        .register_template(REPORT_TEMPLATE_NAME, REPORT_TEMPLATE)
        .expect("report template registers");
    engine
        .render(REPORT_TEMPLATE_NAME, &view)
        .expect("report template renders")
}

fn render_issue_body(merged: &MergeResult, context: RenderContext) -> String {
    let findings_block = if merged.findings.is_empty() {
        "No displayed specialist findings.".to_string()
    } else {
        merged
            .findings
            .iter()
            .map(|finding| {
                format!(
                    "- **{}** ({:.2}, {}) {}: {} Recommendation: {}",
                    finding.primary.severity,
                    finding.primary.confidence,
                    finding.primary.specialist,
                    format_location(&finding.primary, &context),
                    finding.primary.summary,
                    finding.primary.recommendation
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let view = IssueBodyView {
        displayed: merged.counts.displayed,
        suppressed: merged.counts.suppressed,
        findings_block,
        input_rows: merged.counts.input_rows,
        input_files: merged.input_files.join(", "),
    };
    let mut engine = Engine::builder().build();
    engine
        .register_template(ISSUE_BODY_TEMPLATE_NAME, ISSUE_BODY_TEMPLATE)
        .expect("issue_body template registers");
    engine
        .render(ISSUE_BODY_TEMPLATE_NAME, &view)
        .expect("issue_body template renders")
}

#[cfg(test)]
fn render_pr_comment(merged: &MergeResult, context: RenderContext) -> String {
    let provider_context = ProviderReviewContext {
        reviewable: "not provided".to_string(),
        lens: "unspecified".to_string(),
        verdict: if merged.findings.is_empty() {
            "pass".to_string()
        } else {
            "findings".to_string()
        },
        scope: "not provided".to_string(),
        evidence_reviewed: if merged.input_files.is_empty() {
            "no input files".to_string()
        } else {
            merged.input_files.join(", ")
        },
    };
    render_provider_review_artifacts_with_render_context(merged, &provider_context, &context)
        .expect("provider review template renders")
        .body
}

#[cfg(test)]
fn render_provider_review_artifacts(
    merged: &MergeResult,
    context: &ProviderReviewContext,
) -> Result<ProviderReviewArtifacts, CliError> {
    render_provider_review_artifacts_with_render_context(merged, context, &RenderContext::default())
}

fn render_provider_review_artifacts_with_render_context(
    merged: &MergeResult,
    context: &ProviderReviewContext,
    render_context: &RenderContext,
) -> Result<ProviderReviewArtifacts, CliError> {
    let findings = merged
        .findings
        .iter()
        .map(|finding| ProviderReviewFindingRow {
            severity: finding.primary.severity.to_string(),
            confidence: format!("{:.2}", finding.primary.confidence),
            summary: markdown_escape(&finding.primary.summary),
            evidence: markdown_escape(&format!(
                "{} — {}",
                format_location(&finding.primary, render_context),
                finding.primary.evidence
            )),
            recommendation: markdown_escape(&finding.primary.recommendation),
        })
        .collect();
    let view = PrCommentView {
        reviewable: markdown_escape(&context.reviewable),
        lens: markdown_escape(&context.lens),
        verdict: markdown_escape(&context.verdict),
        scope: markdown_escape(&context.scope),
        evidence_reviewed: markdown_escape(&context.evidence_reviewed),
        findings,
    };
    let mut engine = Engine::builder().build();
    engine
        .register_template(PR_COMMENT_TEMPLATE_NAME, PR_COMMENT_TEMPLATE)
        .expect("pr_comment template registers");
    let body = engine
        .render(PR_COMMENT_TEMPLATE_NAME, &view)
        .map_err(|err| {
            CliError::runtime(
                "provider-review-render-failed",
                format!("failed to render provider review: {err}"),
                None,
            )
        })?;
    let threads = merged
        .findings
        .iter()
        .filter_map(|finding| {
            let source = finding.actionable_source.as_ref().or_else(|| {
                finding
                    .primary
                    .actionable
                    .then_some(&finding.primary)
            })?;
            Some(ProviderReviewThread {
                path: source.path.clone(),
                line: source.line,
                subject_type: if source.line.is_some() {
                    "LINE".to_string()
                } else {
                    "FILE".to_string()
                },
                body: format!(
                    "**{}** ({}, {:.2})\n\n{}\n\nRecommendation: {}\n\n<!-- agent-kit:finding:{} -->",
                    source.summary,
                    source.severity,
                    source.confidence,
                    source.evidence,
                    source.recommendation,
                    finding.fingerprint,
                ),
            })
        })
        .collect();
    Ok(ProviderReviewArtifacts { body, threads })
}

fn render_evidence_json(merged: &MergeResult) -> Result<String, CliError> {
    serde_json::to_string_pretty(&json!({
        "schema": "review-specialists.evidence.v1",
        "counts": merged.counts,
        "red_team": merged.red_team,
        "artifacts": {
            "input_files": merged.input_files,
        },
        "findings": merged.findings.iter().map(|finding| {
            json!({
                "fingerprint": finding.fingerprint,
                "severity": finding.primary.severity,
                "confidence": finding.primary.confidence,
                "specialist": finding.primary.specialist,
                "path": finding.primary.path,
                "line": finding.primary.line,
                "summary": finding.primary.summary,
            })
        }).collect::<Vec<_>>(),
        "suppressed_findings": merged.suppressed_findings.iter().map(|finding| {
            json!({
                "fingerprint": finding.fingerprint,
                "severity": finding.primary.severity,
                "confidence": finding.primary.confidence,
                "specialist": finding.primary.specialist,
                "path": finding.primary.path,
                "line": finding.primary.line,
                "summary": finding.primary.summary,
            })
        }).collect::<Vec<_>>(),
    }))
    .map(|body| format!("{body}\n"))
    .map_err(|err| {
        CliError::runtime(
            "serialize-failed",
            format!("failed to serialize evidence profile: {err}"),
            None,
        )
    })
}

fn format_location(finding: &NormalizedFinding, context: &RenderContext) -> String {
    let plain = match finding.line {
        Some(line) => format!("{}:{line}", finding.path),
        None => finding.path.clone(),
    };
    let Some(base) = context.link_base.as_ref() else {
        return plain;
    };
    if !safe_relative_path(&finding.path) {
        return plain;
    }
    let url = match finding.line {
        Some(line) => format!(
            "{}/{}/{}#L{}",
            base.trim_end_matches('/'),
            context.git_ref.as_deref().unwrap_or("HEAD"),
            finding.path,
            line
        ),
        None => format!(
            "{}/{}/{}",
            base.trim_end_matches('/'),
            context.git_ref.as_deref().unwrap_or("HEAD"),
            finding.path
        ),
    };
    format!("[{plain}]({url})")
}

fn markdown_escape(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    let mut backslash_run = 0usize;
    for ch in input.chars() {
        match ch {
            '|' => {
                if backslash_run.is_multiple_of(2) {
                    escaped.push('\\');
                }
                escaped.push('|');
                backslash_run = 0;
            }
            '\n' | '\r' => {
                escaped.push(' ');
                backslash_run = 0;
            }
            '\\' => {
                escaped.push(ch);
                backslash_run += 1;
            }
            _ => {
                escaped.push(ch);
                backslash_run = 0;
            }
        }
    }
    escaped
}

fn read_merged(path: &Path) -> Result<MergeResult, CliError> {
    let body = fs::read_to_string(path).map_err(|err| {
        CliError::runtime(
            "read-failed",
            format!("failed to read {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    if let Ok(merged) = serde_json::from_str::<MergeResult>(&body) {
        return Ok(merged);
    }
    let value: Value = serde_json::from_str(&body).map_err(|err| {
        CliError::runtime(
            "invalid-json",
            format!("failed to parse {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })?;
    let Some(data) = value.get("data") else {
        return Err(CliError::data(
            "invalid-merged-input",
            "merged input must be a MergeResult or a success envelope with data",
            Some(json!({ "path": display_path(path) })),
        ));
    };
    serde_json::from_value(data.clone()).map_err(|err| {
        CliError::data(
            "invalid-merged-input",
            format!("failed to decode merged findings from envelope data: {err}"),
            Some(json!({ "path": display_path(path) })),
        )
    })
}

fn render_normalized_jsonl(findings: &[NormalizedFinding]) -> Result<String, CliError> {
    let mut lines = Vec::new();
    for finding in findings {
        lines.push(serde_json::to_string(finding).map_err(|err| {
            CliError::runtime(
                "serialize-failed",
                format!("failed to serialize normalized finding: {err}"),
                None,
            )
        })?);
    }
    Ok(lines.join("\n") + "\n")
}

fn write_text(path: &Path, body: &str) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            CliError::runtime(
                "create-dir-failed",
                format!("failed to create {}: {err}", parent.display()),
                Some(json!({ "path": display_path(parent) })),
            )
        })?;
    }
    fs::write(path, body).map_err(|err| {
        CliError::runtime(
            "write-failed",
            format!("failed to write {}: {err}", path.display()),
            Some(json!({ "path": display_path(path) })),
        )
    })
}

fn detect_scope(args: &ScopeArgs) -> Result<ScopeResult, CliError> {
    let repo = find_git_repo(&absolute_path(&args.repo)?)?;
    verify_ref(&repo, &args.base)?;
    let changed_files = git_lines(
        &repo,
        &[
            "diff",
            "--name-only",
            "--diff-filter=ACMRTUXB",
            &args.base,
            "--",
        ],
    )?;
    let diff_lines = git_numstat_lines(&repo, &args.base)?;
    let signals = ScopeSignals::from_files(&changed_files);
    let forced = forced_specialists(args);
    let suggested = suggested_specialists(diff_lines, &changed_files, &signals, &forced);
    let red_team = RedTeamTrigger {
        required: diff_lines > 200 || forced.contains("red-team"),
        reasons: red_team_reasons(diff_lines, forced.contains("red-team")),
        diff_lines_gt_200: diff_lines > 200,
        critical_finding: false,
        forced: forced.contains("red-team"),
    };
    let small_diff_skip = diff_lines < 50 && forced.is_empty() && suggested.is_empty();

    Ok(ScopeResult {
        schema: SCOPE_SCHEMA.to_string(),
        repo: display_path(&repo),
        base: args.base.clone(),
        diff_lines,
        changed_files,
        stack: signals.stack,
        test_framework: signals.test_framework,
        scope_api: signals.scope_api,
        scope_auth: signals.scope_auth,
        scope_backend: signals.scope_backend,
        scope_frontend: signals.scope_frontend,
        scope_migrations: signals.scope_migrations,
        forced_specialists: forced.into_iter().collect(),
        suggested_specialists: suggested,
        small_diff_skip,
        red_team,
    })
}

fn find_git_repo(path: &Path) -> Result<PathBuf, CliError> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|err| {
            CliError::runtime("git-unavailable", format!("failed to run git: {err}"), None)
        })?;
    if !output.status.success() {
        return Err(CliError::runtime(
            "not-git-repo",
            format!("{} is not a Git worktree", path.display()),
            Some(json!({ "repo": display_path(path) })),
        ));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

fn verify_ref(repo: &Path, base: &str) -> Result<(), CliError> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", base])
        .output()
        .map_err(|err| {
            CliError::runtime("git-unavailable", format!("failed to run git: {err}"), None)
        })?;
    if !output.status.success() {
        return Err(CliError::runtime(
            "missing-base-ref",
            format!("base ref {base:?} does not resolve"),
            Some(json!({ "base": base, "repo": display_path(repo) })),
        ));
    }
    Ok(())
}

fn git_lines(repo: &Path, args: &[&str]) -> Result<Vec<String>, CliError> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|err| {
            CliError::runtime("git-failed", format!("failed to run git: {err}"), None)
        })?;
    if !output.status.success() {
        return Err(CliError::runtime(
            "git-failed",
            format!("git {} failed", args.join(" ")),
            Some(json!({ "stderr": String::from_utf8_lossy(&output.stderr).trim() })),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn git_numstat_lines(repo: &Path, base: &str) -> Result<u64, CliError> {
    let lines = git_lines(repo, &["diff", "--numstat", base, "--"])?;
    let mut total = 0_u64;
    for line in lines {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        for value in parts.iter().take(2) {
            if let Ok(count) = value.parse::<u64>() {
                total += count;
            }
        }
    }
    Ok(total)
}

fn forced_specialists(args: &ScopeArgs) -> BTreeSet<String> {
    if args.all_specialists {
        return INITIAL_SPECIALISTS
            .iter()
            .map(|item| (*item).to_string())
            .collect();
    }
    let mut forced = BTreeSet::new();
    if args.testing {
        forced.insert("testing".to_string());
    }
    if args.security {
        forced.insert("security".to_string());
    }
    if args.performance {
        forced.insert("performance".to_string());
    }
    if args.data_migration {
        forced.insert("data-migration".to_string());
    }
    if args.api_contract {
        forced.insert("api-contract".to_string());
    }
    if args.maintainability {
        forced.insert("maintainability".to_string());
    }
    if args.red_team {
        forced.insert("red-team".to_string());
    }
    forced
}

fn suggested_specialists(
    diff_lines: u64,
    files: &[String],
    signals: &ScopeSignals,
    forced: &BTreeSet<String>,
) -> Vec<String> {
    let mut selected = forced.clone();
    if diff_lines >= 50 {
        selected.insert("maintainability".to_string());
        selected.insert("testing".to_string());
        if signals.scope_api {
            selected.insert("api-contract".to_string());
        }
        if signals.scope_migrations {
            selected.insert("data-migration".to_string());
        }
        if signals.scope_backend || signals.scope_frontend {
            selected.insert("performance".to_string());
        }
        if signals.scope_auth || (signals.scope_backend && diff_lines > 100) {
            selected.insert("security".to_string());
        }
    }
    if files.iter().any(|path| is_test_path(path)) {
        selected.insert("testing".to_string());
    }
    SPECIALISTS
        .iter()
        .filter(|specialist| selected.contains(**specialist))
        .map(|specialist| (*specialist).to_string())
        .collect()
}

fn red_team_reasons(diff_lines: u64, forced: bool) -> Vec<String> {
    let mut reasons = Vec::new();
    if diff_lines > 200 {
        reasons.push(format!("diff_lines {diff_lines} > 200"));
    }
    if forced {
        reasons.push("red-team forced".to_string());
    }
    reasons
}

fn safe_relative_path(path: &str) -> bool {
    let path = Path::new(path);
    if path.is_absolute() {
        return false;
    }
    !path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

fn has_any(path: &str, needles: &[&str]) -> bool {
    let lowered = path.to_ascii_lowercase();
    needles.iter().any(|needle| lowered.contains(needle))
}

fn is_test_path(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    lowered.contains("/tests/")
        || lowered.starts_with("tests/")
        || lowered.ends_with("_test.rs")
        || lowered.ends_with("_test.py")
        || lowered.ends_with(".test.ts")
        || lowered.ends_with(".test.tsx")
        || lowered.ends_with(".spec.ts")
        || lowered.ends_with(".spec.tsx")
        || lowered.ends_with(".test.js")
        || lowered.ends_with(".spec.js")
}

#[derive(Debug, Parser)]
#[command(
    name = "review-specialists",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Validate, merge, render, bundle, and scope specialist review findings.",
    long_about = "Deterministic primitive for the non-judgment parts of code-review-specialists workflows. It never runs reviewers, posts provider comments, opens issues, merges PRs, or closes issues.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  review-specialists validate --input findings.jsonl --format json\n  review-specialists merge --input findings.jsonl --summary-out review.md\n  review-specialists render --profile issue-body --input findings.merged.json --repo sympoies/nils-cli --ref HEAD --out issue.md\n  review-specialists bundle --input findings.jsonl --out-dir target/review-specialists/bundle --profile issue-body\n  review-specialists scope --base main --format json\n  review-specialists completion zsh\n\nENVIRONMENT:\n  none\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
enum Command {
    /// Validate and normalize specialist finding JSONL.
    Validate(ValidateArgs),
    /// Merge normalized specialist findings by stable fingerprint.
    Merge(MergeArgs),
    /// Render merged findings for terminal, reports, provider bodies, or evidence.
    Render(RenderArgs),
    /// Write a stable specialist review artifact bundle.
    Bundle(BundleArgs),
    /// Classify a Git diff for specialist review routing.
    Scope(ScopeArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
struct CommonArgs {
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args, Default)]
struct ProviderReviewArgs {
    /// PR/MR identifier or URL shown in the provider review body.
    #[arg(long, value_name = "REVIEWABLE")]
    reviewable: Option<String>,

    /// Exactly one reviewer lens represented by this report.
    #[arg(long, value_name = "LENS")]
    lens: Option<String>,

    /// Single-lens verdict represented by this report.
    #[arg(long = "lens-verdict", value_enum)]
    lens_verdict: Option<ProviderLensVerdict>,

    /// Compact description of files or behavior reviewed.
    #[arg(long, value_name = "TEXT")]
    scope: Option<String>,

    /// Compact validation, diff, or provider evidence summary.
    #[arg(long = "evidence-reviewed", value_name = "TEXT")]
    evidence_reviewed: Option<String>,
}

#[derive(Debug, Args)]
struct ValidateArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Fingerprint policy. Delivery requires stable lifecycle identities.
    #[arg(long, value_enum, default_value_t = ReviewMode::Advisory)]
    mode: ReviewMode,

    /// Specialist finding JSONL input file.
    #[arg(long = "input", value_name = "FILE", required = true, value_hint = ValueHint::FilePath)]
    inputs: Vec<PathBuf>,

    /// Repository root for optional path or line validation.
    #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
    repo: Option<PathBuf>,

    /// Check that finding paths exist under --repo.
    #[arg(long)]
    validate_paths: bool,

    /// Check that finding lines are within file bounds under --repo.
    #[arg(long)]
    validate_lines: bool,
}

#[derive(Debug, Args)]
struct MergeArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Fingerprint policy. Delivery requires stable lifecycle identities.
    #[arg(long, value_enum, default_value_t = ReviewMode::Advisory)]
    mode: ReviewMode,

    /// Specialist finding JSONL input file.
    #[arg(long = "input", value_name = "FILE", required = true, value_hint = ValueHint::FilePath)]
    inputs: Vec<PathBuf>,

    /// Minimum confidence for displayed findings.
    #[arg(long, default_value_t = DEFAULT_DISPLAY_THRESHOLD)]
    display_threshold: f64,

    /// Optional Markdown report path to write.
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    summary_out: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct RenderArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Render profile.
    #[arg(long, value_enum)]
    profile: RenderProfile,

    /// Merged findings JSON from `review-specialists merge --format json`.
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    input: PathBuf,

    /// Output path. If omitted, text profiles print the rendered body.
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    out: Option<PathBuf>,

    /// Repository slug for GitHub source links, e.g. owner/repo.
    #[arg(long)]
    repo: Option<String>,

    /// Ref for source links.
    #[arg(long = "ref")]
    git_ref: Option<String>,

    /// Custom link base. Defaults to https://github.com/<repo>/blob when --repo is provided.
    #[arg(long)]
    link_base: Option<String>,

    /// Write actionable GitHub review-thread specs derived from the same findings.
    #[arg(long = "thread-out", value_name = "FILE", value_hint = ValueHint::FilePath)]
    thread_out: Option<PathBuf>,

    #[command(flatten)]
    provider_review: ProviderReviewArgs,
}

#[derive(Debug, Args)]
struct BundleArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Fingerprint policy. Delivery requires stable lifecycle identities.
    #[arg(long, value_enum, default_value_t = ReviewMode::Advisory)]
    mode: ReviewMode,

    /// Specialist finding JSONL input file.
    #[arg(long = "input", value_name = "FILE", required = true, value_hint = ValueHint::FilePath)]
    inputs: Vec<PathBuf>,

    /// Bundle output directory.
    #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
    out_dir: PathBuf,

    /// Minimum confidence for displayed findings.
    #[arg(long, default_value_t = DEFAULT_DISPLAY_THRESHOLD)]
    display_threshold: f64,

    /// Optional additional render profile to write.
    #[arg(long, value_enum)]
    profile: Option<RenderProfile>,

    /// Repository slug for GitHub source links, e.g. owner/repo.
    #[arg(long)]
    repo: Option<String>,

    /// Ref for source links.
    #[arg(long = "ref")]
    git_ref: Option<String>,

    /// Custom link base. Defaults to https://github.com/<repo>/blob when --repo is provided.
    #[arg(long)]
    link_base: Option<String>,

    #[command(flatten)]
    provider_review: ProviderReviewArgs,
}

#[derive(Debug, Args)]
struct ScopeArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Git worktree to inspect.
    #[arg(long, value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
    repo: PathBuf,

    /// Base ref for git diff.
    #[arg(long)]
    base: String,

    /// Force testing specialist.
    #[arg(long)]
    testing: bool,

    /// Force security specialist.
    #[arg(long)]
    security: bool,

    /// Force performance specialist.
    #[arg(long)]
    performance: bool,

    /// Force data-migration specialist.
    #[arg(long)]
    data_migration: bool,

    /// Force api-contract specialist.
    #[arg(long)]
    api_contract: bool,

    /// Force maintainability specialist.
    #[arg(long)]
    maintainability: bool,

    /// Force red-team specialist.
    #[arg(long)]
    red_team: bool,

    /// Force all initial specialists.
    #[arg(long)]
    all_specialists: bool,
}

#[derive(Debug, Args)]
struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum)]
    shell: CompletionShell,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
enum RenderProfile {
    Terminal,
    Report,
    IssueBody,
    PrComment,
    ProviderReview,
    Evidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum ProviderLensVerdict {
    Pass,
    Findings,
    Blocked,
    FollowUpPass,
}

impl ProviderLensVerdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Findings => "findings",
            Self::Blocked => "blocked",
            Self::FollowUpPass => "follow-up-pass",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
enum ReviewMode {
    #[default]
    Advisory,
    Delivery,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Severity::Critical => "critical",
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
            Severity::Info => "info",
        };
        formatter.write_str(text)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct NormalizedFinding {
    severity: Severity,
    confidence: f64,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<u64>,
    category: String,
    summary: String,
    evidence: String,
    recommendation: String,
    fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    root_cause_fingerprint: Option<String>,
    specialist: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_suggestion: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    actionable: bool,
    source_file: String,
    source_line: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MergedFinding {
    fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lifecycle_fingerprint: Option<String>,
    primary: NormalizedFinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    actionable_source: Option<NormalizedFinding>,
    confirming_specialists: Vec<String>,
    confirming_count: usize,
    source_rows: Vec<SourceRow>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SourceRow {
    source_file: String,
    source_line: usize,
    specialist: String,
    confidence: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MergeResult {
    schema: String,
    input_files: Vec<String>,
    display_threshold: f64,
    counts: FindingCounts,
    specialist_stats: BTreeMap<String, SpecialistStat>,
    red_team: RedTeamTrigger,
    findings: Vec<MergedFinding>,
    suppressed_findings: Vec<MergedFinding>,
}

impl MergeResult {
    fn text_summary(&self) -> String {
        format!(
            "review-specialists: input_rows={} merged={} displayed={} suppressed={} red_team_required={}",
            self.counts.input_rows,
            self.counts.merged,
            self.counts.displayed,
            self.counts.suppressed,
            self.red_team.required
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct FindingCounts {
    input_rows: usize,
    merged: usize,
    displayed: usize,
    suppressed: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct SpecialistStat {
    input: usize,
    displayed: usize,
    suppressed: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RedTeamTrigger {
    required: bool,
    reasons: Vec<String>,
    diff_lines_gt_200: bool,
    critical_finding: bool,
    forced: bool,
}

#[derive(Clone, Debug, Serialize)]
struct ValidateResult {
    schema: String,
    input_files: Vec<String>,
    findings_count: usize,
    findings: Vec<NormalizedFinding>,
}

impl ValidateResult {
    fn text_summary(&self) -> String {
        format!(
            "review-specialists: validated {} finding(s) from {} file(s)",
            self.findings_count,
            self.input_files.len()
        )
    }
}

#[derive(Debug, Serialize)]
struct RenderResult {
    profile: RenderProfile,
    body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    threads: Vec<ProviderReviewThread>,
    counts: FindingCounts,
}

impl RenderResult {
    fn text_summary(&self) -> String {
        format!(
            "review-specialists: rendered {:?} displayed={} suppressed={}",
            self.profile, self.counts.displayed, self.counts.suppressed
        )
    }
}

#[derive(Debug, Serialize)]
struct BundleResult {
    out_dir: String,
    artifacts: Vec<String>,
    profile: Option<RenderProfile>,
    profile_artifact: Option<String>,
    counts: FindingCounts,
}

impl BundleResult {
    fn text_summary(&self) -> String {
        format!(
            "review-specialists: bundle artifacts={} displayed={} suppressed={}",
            self.artifacts.len(),
            self.counts.displayed,
            self.counts.suppressed
        )
    }
}

#[derive(Debug, Serialize)]
struct ScopeResult {
    schema: String,
    repo: String,
    base: String,
    diff_lines: u64,
    changed_files: Vec<String>,
    stack: Vec<String>,
    test_framework: Vec<String>,
    scope_api: bool,
    scope_auth: bool,
    scope_backend: bool,
    scope_frontend: bool,
    scope_migrations: bool,
    forced_specialists: Vec<String>,
    suggested_specialists: Vec<String>,
    small_diff_skip: bool,
    red_team: RedTeamTrigger,
}

impl ScopeResult {
    fn text_summary(&self) -> String {
        format!(
            "review-specialists: changed_files={} diff_lines={} suggested={} small_diff_skip={}",
            self.changed_files.len(),
            self.diff_lines,
            self.suggested_specialists.join(","),
            self.small_diff_skip
        )
    }
}

#[derive(Debug, Serialize)]
struct RowError {
    source_file: String,
    source_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    message: String,
}

impl RowError {
    fn new(source_file: String, source_line: usize, message: String) -> Self {
        Self {
            source_file,
            source_line,
            code: None,
            message,
        }
    }

    fn typed(
        source_file: String,
        source_line: usize,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            source_file,
            source_line,
            code: Some(code.into()),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct PathPolicy {
    repo: Option<PathBuf>,
    validate_paths: bool,
    validate_lines: bool,
}

#[derive(Debug)]
struct MergedFindingBuilder {
    primary: NormalizedFinding,
    lifecycle_fingerprint: String,
    mode: ReviewMode,
    rows: Vec<NormalizedFinding>,
}

impl MergedFindingBuilder {
    fn new(primary: NormalizedFinding, lifecycle_fingerprint: String, mode: ReviewMode) -> Self {
        Self {
            primary,
            lifecycle_fingerprint,
            mode,
            rows: Vec::new(),
        }
    }

    fn push(&mut self, finding: NormalizedFinding) {
        if is_higher_priority(&finding, &self.primary) {
            self.primary = finding.clone();
        }
        self.rows.push(finding);
    }

    fn finish(self) -> MergedFinding {
        let primary = self.primary;
        let actionable_source = self
            .rows
            .iter()
            .filter(|finding| finding.actionable)
            .cloned()
            .reduce(|current, candidate| {
                if is_higher_priority(&candidate, &current) {
                    candidate
                } else {
                    current
                }
            });
        let mut confirming: BTreeSet<String> = BTreeSet::new();
        let mut source_rows = Vec::new();
        for finding in self.rows {
            confirming.insert(finding.specialist.clone());
            source_rows.push(SourceRow {
                source_file: finding.source_file,
                source_line: finding.source_line,
                specialist: finding.specialist,
                confidence: finding.confidence,
            });
        }
        source_rows.sort_by(|left, right| {
            left.source_file
                .cmp(&right.source_file)
                .then_with(|| left.source_line.cmp(&right.source_line))
                .then_with(|| left.specialist.cmp(&right.specialist))
        });
        MergedFinding {
            fingerprint: primary.fingerprint.clone(),
            lifecycle_fingerprint: (self.mode == ReviewMode::Delivery)
                .then_some(self.lifecycle_fingerprint),
            primary,
            actionable_source,
            confirming_specialists: confirming.iter().cloned().collect(),
            confirming_count: confirming.len(),
            source_rows,
        }
    }
}

fn is_higher_priority(left: &NormalizedFinding, right: &NormalizedFinding) -> bool {
    left.confidence
        .partial_cmp(&right.confidence)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| severity_rank(right.severity).cmp(&severity_rank(left.severity)))
        .then_with(|| right.path.cmp(&left.path))
        .then_with(|| right.summary.cmp(&left.summary))
        == std::cmp::Ordering::Greater
}

#[derive(Clone, Debug, Default)]
struct RenderContext {
    link_base: Option<String>,
    git_ref: Option<String>,
}

impl RenderContext {
    fn from_args(repo: Option<&str>, git_ref: Option<&str>, link_base: Option<&str>) -> Self {
        let resolved_base = link_base
            .map(ToOwned::to_owned)
            .or_else(|| repo.map(|repo| format!("https://github.com/{repo}/blob")));
        Self {
            link_base: resolved_base,
            git_ref: git_ref.map(ToOwned::to_owned),
        }
    }
}

fn provider_review_context(
    args: &ProviderReviewArgs,
    merged: &MergeResult,
) -> ProviderReviewContext {
    ProviderReviewContext {
        reviewable: args
            .reviewable
            .clone()
            .unwrap_or_else(|| "not provided".to_string()),
        lens: args
            .lens
            .clone()
            .unwrap_or_else(|| "unspecified".to_string()),
        verdict: args
            .lens_verdict
            .map(ProviderLensVerdict::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                if merged.findings.is_empty() {
                    "pass".to_string()
                } else {
                    "findings".to_string()
                }
            }),
        scope: args
            .scope
            .clone()
            .unwrap_or_else(|| "not provided".to_string()),
        evidence_reviewed: args.evidence_reviewed.clone().unwrap_or_else(|| {
            if merged.input_files.is_empty() {
                "no input files".to_string()
            } else {
                merged.input_files.join(", ")
            }
        }),
    }
}

#[derive(Debug)]
struct ScopeSignals {
    stack: Vec<String>,
    test_framework: Vec<String>,
    scope_api: bool,
    scope_auth: bool,
    scope_backend: bool,
    scope_frontend: bool,
    scope_migrations: bool,
}

impl ScopeSignals {
    fn from_files(files: &[String]) -> Self {
        let mut stack = BTreeSet::new();
        let mut test_framework = BTreeSet::new();
        let mut scope_api = false;
        let mut scope_auth = false;
        let mut scope_backend = false;
        let mut scope_frontend = false;
        let mut scope_migrations = false;

        for file in files {
            let lowered = file.to_ascii_lowercase();
            let suffix = Path::new(&lowered)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            match suffix {
                "py" => {
                    stack.insert("python".to_string());
                    if is_test_path(&lowered) {
                        test_framework.insert("pytest".to_string());
                    }
                }
                "js" | "jsx" | "ts" | "tsx" | "vue" | "svelte" => {
                    stack.insert("javascript".to_string());
                    if is_test_path(&lowered) {
                        test_framework.insert("jest/vitest".to_string());
                    }
                }
                "go" => {
                    stack.insert("go".to_string());
                    if is_test_path(&lowered) {
                        test_framework.insert("go test".to_string());
                    }
                }
                "rs" => {
                    stack.insert("rust".to_string());
                    if is_test_path(&lowered) {
                        test_framework.insert("cargo test".to_string());
                    }
                }
                "java" | "kt" | "kts" => {
                    stack.insert("jvm".to_string());
                }
                "rb" => {
                    stack.insert("ruby".to_string());
                    if is_test_path(&lowered) {
                        test_framework.insert("rspec/minitest".to_string());
                    }
                }
                "php" => {
                    stack.insert("php".to_string());
                }
                "sql" => {
                    stack.insert("sql".to_string());
                    scope_migrations = true;
                }
                "md" | "mdx" | "rst" => {
                    stack.insert("docs".to_string());
                }
                _ => {}
            }
            scope_api |= has_any(
                &lowered,
                &[
                    "api",
                    "openapi",
                    "graphql",
                    "proto",
                    "schema",
                    "route",
                    "routes",
                    "controller",
                    "contract",
                    "sdk",
                ],
            );
            scope_auth |= has_any(
                &lowered,
                &[
                    "auth",
                    "oauth",
                    "jwt",
                    "session",
                    "permission",
                    "rbac",
                    "acl",
                    "security",
                    "token",
                ],
            );
            scope_migrations |= has_any(
                &lowered,
                &[
                    "migration",
                    "migrations",
                    "alembic",
                    "db/migrate",
                    "schema",
                    "backfill",
                ],
            );
            scope_frontend |= has_any(
                &lowered,
                &["frontend", "web/", "ui/", "components/", "pages/", "app/"],
            ) || matches!(
                suffix,
                "js" | "jsx" | "ts" | "tsx" | "vue" | "svelte" | "css" | "scss" | "html"
            );
            scope_backend |= has_any(
                &lowered,
                &[
                    "backend", "server", "service", "worker", "api", "app/", "src/", "lib/",
                ],
            ) || matches!(
                suffix,
                "py" | "go" | "rs" | "rb" | "php" | "java" | "kt" | "cs"
            );
        }

        Self {
            stack: stack.into_iter().collect(),
            test_framework: test_framework.into_iter().collect(),
            scope_api,
            scope_auth,
            scope_backend,
            scope_frontend,
            scope_migrations,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn computed_fingerprint_is_stable() {
        let left = computed_fingerprint("src/lib.rs", Some(10), "testing", "Missing test");
        let right = computed_fingerprint("src/lib.rs", Some(10), "testing", "missing test");
        assert_eq!(left, right);
    }

    #[test]
    fn unsafe_relative_paths_are_detected() {
        assert!(safe_relative_path("src/lib.rs"));
        assert!(!safe_relative_path("../src/lib.rs"));
        assert!(!safe_relative_path("/tmp/src/lib.rs"));
    }

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("golden")
            .join("review_specialists")
            .join(name)
    }

    fn assert_or_bless(name: &str, actual: &str) {
        let path = fixture_path(name);
        if std::env::var_os("BLESS_REVIEW_SPECIALISTS_GOLDEN").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir fixture dir");
            std::fs::write(&path, actual).expect("write fixture");
            return;
        }
        let expected = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()));
        pretty_assertions::assert_eq!(expected, actual, "golden mismatch for {name}");
    }

    fn build_finding(
        severity: Severity,
        confidence: f64,
        specialist: &str,
        path: &str,
        line: Option<u64>,
        summary: &str,
        recommendation: &str,
    ) -> MergedFinding {
        let primary = NormalizedFinding {
            severity,
            confidence,
            path: path.to_string(),
            line,
            category: "code".to_string(),
            summary: summary.to_string(),
            evidence: format!("evidence for {summary}"),
            recommendation: recommendation.to_string(),
            fingerprint: computed_fingerprint(path, line, specialist, summary),
            root_cause_fingerprint: None,
            specialist: specialist.to_string(),
            test_suggestion: None,
            actionable: false,
            source_file: "fixture.jsonl".to_string(),
            source_line: 1,
        };
        let fingerprint = primary.fingerprint.clone();
        MergedFinding {
            fingerprint,
            lifecycle_fingerprint: None,
            primary,
            actionable_source: None,
            confirming_specialists: vec![specialist.to_string()],
            confirming_count: 1,
            source_rows: vec![SourceRow {
                source_file: "fixture.jsonl".to_string(),
                source_line: 1,
                specialist: specialist.to_string(),
                confidence,
            }],
        }
    }

    fn empty_merge_result() -> MergeResult {
        MergeResult {
            schema: MERGED_SCHEMA.to_string(),
            input_files: vec![],
            display_threshold: DEFAULT_DISPLAY_THRESHOLD,
            counts: FindingCounts {
                input_rows: 0,
                merged: 0,
                displayed: 0,
                suppressed: 0,
            },
            specialist_stats: BTreeMap::new(),
            red_team: RedTeamTrigger {
                required: false,
                reasons: vec![],
                diff_lines_gt_200: false,
                critical_finding: false,
                forced: false,
            },
            findings: vec![],
            suppressed_findings: vec![],
        }
    }

    fn mixed_merge_result() -> MergeResult {
        let displayed = vec![
            build_finding(
                Severity::High,
                0.85,
                "testing",
                "src/lib.rs",
                Some(42),
                "Missing test for new branch",
                "Add a unit test exercising the new branch.",
            ),
            build_finding(
                Severity::Medium,
                0.72,
                "maintainability",
                "src/util.rs",
                Some(10),
                "Function is too long",
                "Split the helper into smaller functions.",
            ),
        ];
        let suppressed = vec![build_finding(
            Severity::Low,
            0.45,
            "performance",
            "src/main.rs",
            Some(5),
            "Allocation inside loop",
            "Hoist the allocation outside the loop.",
        )];
        let mut specialist_stats = BTreeMap::new();
        specialist_stats.insert(
            "testing".to_string(),
            SpecialistStat {
                input: 1,
                displayed: 1,
                suppressed: 0,
            },
        );
        specialist_stats.insert(
            "maintainability".to_string(),
            SpecialistStat {
                input: 1,
                displayed: 1,
                suppressed: 0,
            },
        );
        specialist_stats.insert(
            "performance".to_string(),
            SpecialistStat {
                input: 1,
                displayed: 0,
                suppressed: 1,
            },
        );
        MergeResult {
            schema: MERGED_SCHEMA.to_string(),
            input_files: vec![
                "findings-a.jsonl".to_string(),
                "findings-b.jsonl".to_string(),
            ],
            display_threshold: DEFAULT_DISPLAY_THRESHOLD,
            counts: FindingCounts {
                input_rows: 3,
                merged: 3,
                displayed: 2,
                suppressed: 1,
            },
            specialist_stats,
            red_team: RedTeamTrigger {
                required: true,
                reasons: vec!["forced via flag".to_string()],
                diff_lines_gt_200: false,
                critical_finding: false,
                forced: true,
            },
            findings: displayed,
            suppressed_findings: suppressed,
        }
    }

    #[test]
    fn terminal_empty_matches_golden() {
        let out = render_terminal(&empty_merge_result(), RenderContext::default());
        assert_or_bless("terminal_empty.md", &out);
    }

    #[test]
    fn terminal_mixed_matches_golden() {
        let out = render_terminal(&mixed_merge_result(), RenderContext::default());
        assert_or_bless("terminal_mixed.md", &out);
    }

    #[test]
    fn report_empty_matches_golden() {
        let out = render_report(&empty_merge_result(), RenderContext::default());
        assert_or_bless("report_empty.md", &out);
    }

    #[test]
    fn report_mixed_matches_golden() {
        let out = render_report(&mixed_merge_result(), RenderContext::default());
        assert_or_bless("report_mixed.md", &out);
    }

    #[test]
    fn issue_body_empty_matches_golden() {
        let out = render_issue_body(&empty_merge_result(), RenderContext::default());
        assert_or_bless("issue_body_empty.md", &out);
    }

    #[test]
    fn issue_body_mixed_matches_golden() {
        let out = render_issue_body(&mixed_merge_result(), RenderContext::default());
        assert_or_bless("issue_body_mixed.md", &out);
    }

    #[test]
    fn pr_comment_empty_matches_golden() {
        let out = render_pr_comment(&empty_merge_result(), RenderContext::default());
        assert_or_bless("pr_comment_empty.md", &out);
    }

    #[test]
    fn pr_comment_mixed_matches_golden() {
        let out = render_pr_comment(&mixed_merge_result(), RenderContext::default());
        assert_or_bless("pr_comment_mixed.md", &out);
    }

    #[test]
    fn provider_review_artifacts_include_only_explicitly_actionable_findings() {
        let mut merged = mixed_merge_result();
        merged.findings[0].primary.actionable = true;
        merged.findings[0].actionable_source = Some(merged.findings[0].primary.clone());
        merged.findings[1].primary.actionable = false;
        let context = ProviderReviewContext {
            reviewable: "PR #44".to_string(),
            lens: "testing".to_string(),
            verdict: "findings".to_string(),
            scope: "changed review publication paths".to_string(),
            evidence_reviewed: "focused tests and diff".to_string(),
        };

        let artifacts =
            render_provider_review_artifacts(&merged, &context).expect("provider artifacts render");

        assert!(
            artifacts
                .body
                .contains("<!-- agent-kit:specialist-review-report:v1 -->")
        );
        assert!(
            artifacts
                .body
                .contains("| Finding | Severity | Confidence | Evidence | Recommendation |")
        );
        assert_eq!(artifacts.threads.len(), 1);
        assert_eq!(artifacts.threads[0].path, "src/lib.rs");
        assert_eq!(artifacts.threads[0].line, Some(42));
        assert_eq!(artifacts.threads[0].subject_type, "LINE");
        assert!(
            artifacts.threads[0]
                .body
                .contains(&merged.findings[0].fingerprint)
        );
    }

    #[test]
    fn provider_review_artifacts_emit_file_threads_without_a_line_anchor() {
        let mut merged = mixed_merge_result();
        merged.findings.truncate(1);
        merged.findings[0].primary.actionable = true;
        merged.findings[0].primary.line = None;
        merged.findings[0].actionable_source = Some(merged.findings[0].primary.clone());
        let context = ProviderReviewContext {
            reviewable: "PR #44".to_string(),
            lens: "testing".to_string(),
            verdict: "findings".to_string(),
            scope: "src/lib.rs".to_string(),
            evidence_reviewed: "diff".to_string(),
        };

        let artifacts =
            render_provider_review_artifacts(&merged, &context).expect("provider artifacts render");

        assert_eq!(artifacts.threads.len(), 1);
        assert_eq!(artifacts.threads[0].line, None);
        assert_eq!(artifacts.threads[0].subject_type, "FILE");
    }

    #[test]
    fn provider_review_threads_preserve_the_explicitly_actionable_source_row() {
        let mut primary = build_finding(
            Severity::High,
            0.95,
            "maintainability",
            "src/primary.rs",
            Some(11),
            "Higher confidence summary",
            "Keep the primary recommendation.",
        )
        .primary;
        primary.root_cause_fingerprint = Some("shared-root-cause".to_string());
        let mut actionable = build_finding(
            Severity::Medium,
            0.80,
            "testing",
            "src/actionable.rs",
            Some(22),
            "Actionable summary",
            "Fix the actionable row.",
        )
        .primary;
        actionable.root_cause_fingerprint = Some("shared-root-cause".to_string());
        actionable.actionable = true;

        let mut builder = MergedFindingBuilder::new(
            primary.clone(),
            "shared-root-cause".to_string(),
            ReviewMode::Delivery,
        );
        builder.push(primary);
        builder.push(actionable);
        let merged_finding = builder.finish();
        assert_eq!(merged_finding.primary.path, "src/primary.rs");

        let mut merged = empty_merge_result();
        merged.schema = DELIVERY_MERGED_SCHEMA.to_string();
        merged.input_files = vec!["findings.jsonl".to_string()];
        merged.findings = vec![merged_finding];
        let artifacts = render_provider_review_artifacts(
            &merged,
            &ProviderReviewContext {
                reviewable: "PR #44".to_string(),
                lens: "testing".to_string(),
                verdict: "findings".to_string(),
                scope: "merged source selection".to_string(),
                evidence_reviewed: "unit regression".to_string(),
            },
        )
        .expect("provider artifacts render");

        assert_eq!(artifacts.threads.len(), 1);
        assert_eq!(artifacts.threads[0].path, "src/actionable.rs");
        assert_eq!(artifacts.threads[0].line, Some(22));
        assert!(artifacts.threads[0].body.contains("Actionable summary"));
    }

    // -----------------------------------------------------------------
    // Row-level normalization. A specialist writes JSONL by hand, so every
    // field guard below is what keeps a malformed row out of the merged
    // review rather than silently degrading it.
    // -----------------------------------------------------------------

    fn input() -> PathBuf {
        PathBuf::from("/repo/findings.jsonl")
    }

    fn object(value: serde_json::Value) -> serde_json::Map<String, Value> {
        value.as_object().expect("object").clone()
    }

    #[test]
    fn a_required_string_field_must_be_present_and_non_blank() {
        let row = object(serde_json::json!({"summary": "text", "blank": "  ", "number": 1}));

        assert_eq!(
            required_string(&row, "summary", &input(), 3).expect("present"),
            "text"
        );

        let missing = required_string(&row, "absent", &input(), 3).expect_err("missing");
        assert_eq!(missing.source_line, 3);
        assert_eq!(missing.source_file, "/repo/findings.jsonl");
        assert_eq!(missing.message, "absent must be a string");

        assert_eq!(
            required_string(&row, "number", &input(), 3)
                .expect_err("wrong type")
                .message,
            "number must be a string"
        );
        assert_eq!(
            required_string(&row, "blank", &input(), 3)
                .expect_err("blank")
                .message,
            "blank must not be empty"
        );
    }

    #[test]
    fn an_optional_string_treats_null_and_blank_as_absent() {
        assert_eq!(
            optional_string(None, "note", &input(), 1).expect("absent"),
            None
        );
        assert_eq!(
            optional_string(Some(&Value::Null), "note", &input(), 1).expect("null"),
            None
        );
        assert_eq!(
            optional_string(Some(&serde_json::json!("  ")), "note", &input(), 1).expect("blank"),
            None
        );
        assert_eq!(
            optional_string(Some(&serde_json::json!("text")), "note", &input(), 1).expect("text"),
            Some("text".to_string())
        );
        assert_eq!(
            optional_string(Some(&serde_json::json!(7)), "note", &input(), 1)
                .expect_err("wrong type")
                .message,
            "note must be a string"
        );
    }

    #[test]
    fn an_optional_line_number_must_be_a_positive_integer() {
        assert_eq!(
            optional_positive_u64(None, &input(), 1).expect("absent"),
            None
        );
        assert_eq!(
            optional_positive_u64(Some(&Value::Null), &input(), 1).expect("null"),
            None
        );
        assert_eq!(
            optional_positive_u64(Some(&serde_json::json!(12)), &input(), 1).expect("positive"),
            Some(12)
        );

        for bad in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("12"),
        ] {
            assert_eq!(
                optional_positive_u64(Some(&bad), &input(), 1)
                    .expect_err("invalid line")
                    .message,
                "line must be a positive integer when present",
                "{bad}"
            );
        }
    }

    #[test]
    fn the_severity_vocabulary_accepts_its_documented_aliases() {
        for (raw, expected) in [
            ("critical", Severity::Critical),
            ("CRIT", Severity::Critical),
            ("High", Severity::High),
            ("medium", Severity::Medium),
            (" med ", Severity::Medium),
            ("low", Severity::Low),
            ("info", Severity::Info),
            ("informational", Severity::Info),
        ] {
            assert_eq!(
                normalize_severity(raw, &input(), 1).expect("severity"),
                expected,
                "{raw}"
            );
        }

        let error = normalize_severity("catastrophic", &input(), 4).expect_err("unknown severity");
        assert_eq!(error.source_line, 4);
        assert!(
            error
                .message
                .contains("expected critical|high|medium|low|info"),
            "{}",
            error.message
        );
    }

    #[test]
    fn confidence_must_be_a_number_within_the_unit_interval() {
        for raw in [
            serde_json::json!(0.0),
            serde_json::json!(0.5),
            serde_json::json!(1.0),
            serde_json::json!(1),
        ] {
            normalize_confidence(&raw, &input(), 1)
                .unwrap_or_else(|error| panic!("{raw}: {}", error.message));
        }

        assert_eq!(
            normalize_confidence(&serde_json::json!("0.5"), &input(), 1)
                .expect_err("string")
                .message,
            "confidence must be a number from 0.0 to 1.0"
        );
        for out_of_range in [serde_json::json!(-0.1), serde_json::json!(1.1)] {
            assert_eq!(
                normalize_confidence(&out_of_range, &input(), 1)
                    .expect_err("out of range")
                    .message,
                "confidence must be from 0.0 to 1.0"
            );
        }
    }

    #[test]
    fn the_display_threshold_is_bounded_to_the_unit_interval() {
        validate_threshold(0.0).expect("lower bound");
        validate_threshold(1.0).expect("upper bound");
        assert_eq!(
            validate_threshold(1.5).expect_err("above").code(),
            "invalid-threshold"
        );
        assert_eq!(
            validate_threshold(-0.5).expect_err("below").code(),
            "invalid-threshold"
        );
    }

    #[test]
    fn a_stable_fingerprint_needs_three_lowercase_parts() {
        validate_stable_fingerprint("correctness:parser:off-by-one", "fingerprint", &input(), 1)
            .expect("stable");
        validate_stable_fingerprint("a1:b2:c3", "fingerprint", &input(), 1).expect("digits");

        for bad in [
            "correctness",
            "correctness:parser",
            "a:b:c:d",
            "Correctness:parser:x",
            "correctness::x",
            "-lead:parser:x",
            "trail-:parser:x",
            "correctness:parser:under_score",
        ] {
            let error =
                validate_stable_fingerprint(bad, "fingerprint", &input(), 2).expect_err("unstable");
            assert_eq!(
                error.code.as_deref(),
                Some("review_fingerprint_collision"),
                "{bad}"
            );
            assert!(error.message.contains(bad), "{bad}");
        }

        assert!(stable_fingerprint_part("ok-1"));
        assert!(!stable_fingerprint_part(""));
    }

    #[test]
    fn path_validation_is_skipped_unless_it_is_requested() {
        let off = PathPolicy {
            repo: None,
            validate_paths: false,
            validate_lines: false,
        };
        validate_path_policy("anything/../escape", None, &off, &input(), 1)
            .expect("no policy, no checks");

        // Asking for validation without a repo root is a configuration error.
        let no_repo = PathPolicy {
            repo: None,
            validate_paths: true,
            validate_lines: false,
        };
        assert!(
            validate_path_policy("src/lib.rs", None, &no_repo, &input(), 1)
                .expect_err("no repo")
                .message
                .contains("--repo is required")
        );
    }

    #[test]
    fn a_validated_path_must_exist_inside_the_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().to_path_buf();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/lib.rs"), "one\ntwo\nthree\n").unwrap();
        let policy = PathPolicy {
            repo: Some(repo.clone()),
            validate_paths: true,
            validate_lines: true,
        };

        validate_path_policy("src/lib.rs", Some(3), &policy, &input(), 1).expect("in range");
        validate_path_policy("src/lib.rs", None, &policy, &input(), 1)
            .expect("no line to validate");

        assert!(
            validate_path_policy("../outside.rs", None, &policy, &input(), 1)
                .expect_err("escape")
                .message
                .contains("path must be relative to the repo")
        );
        assert!(
            validate_path_policy("src/absent.rs", None, &policy, &input(), 1)
                .expect_err("missing file")
                .message
                .contains("path does not exist")
        );
        assert!(
            validate_path_policy("src/lib.rs", Some(99), &policy, &input(), 1)
                .expect_err("past end")
                .message
                .contains("line 99 is past end of")
        );
    }

    #[test]
    fn delivery_mode_rejects_one_fingerprint_with_two_identities() {
        let finding = |fingerprint: &str, category: &str| NormalizedFinding {
            severity: Severity::High,
            confidence: 0.9,
            path: "src/lib.rs".to_string(),
            line: Some(1),
            category: category.to_string(),
            summary: "summary".to_string(),
            evidence: "evidence".to_string(),
            recommendation: "fix it".to_string(),
            fingerprint: fingerprint.to_string(),
            root_cause_fingerprint: None,
            specialist: "reviewer-testing".to_string(),
            test_suggestion: None,
            actionable: false,
            source_file: "/repo/findings.jsonl".to_string(),
            source_line: 1,
        };

        let consistent = vec![
            finding("correctness:parser:one", "correctness"),
            finding("correctness:parser:one", "correctness"),
        ];
        validate_delivery_collisions(&consistent, ReviewMode::Delivery).expect("same identity");

        let conflicting = vec![
            finding("correctness:parser:one", "correctness"),
            finding("correctness:parser:one", "testing"),
        ];
        assert_eq!(
            validate_delivery_collisions(&conflicting, ReviewMode::Delivery)
                .expect_err("collision")
                .code(),
            "review_fingerprint_collision"
        );

        // Advisory mode does not carry lifecycle identity, so it is exempt.
        validate_delivery_collisions(&conflicting, ReviewMode::Advisory)
            .expect("advisory is exempt");
    }

    #[test]
    fn severity_ranks_order_the_merged_report() {
        assert!(severity_rank(Severity::Critical) < severity_rank(Severity::High));
        assert!(severity_rank(Severity::High) < severity_rank(Severity::Medium));
        assert!(severity_rank(Severity::Medium) < severity_rank(Severity::Low));
        assert!(severity_rank(Severity::Low) < severity_rank(Severity::Info));
    }

    #[test]
    fn the_computed_fingerprint_binds_path_line_category_and_summary() {
        let base = computed_fingerprint("src/lib.rs", Some(10), "testing", "Missing test");

        assert_ne!(
            base,
            computed_fingerprint("src/other.rs", Some(10), "testing", "Missing test")
        );
        assert_ne!(
            base,
            computed_fingerprint("src/lib.rs", Some(11), "testing", "Missing test")
        );
        assert_ne!(
            base,
            computed_fingerprint("src/lib.rs", Some(10), "security", "Missing test")
        );
        assert_ne!(
            base,
            computed_fingerprint("src/lib.rs", Some(10), "testing", "Other summary")
        );
        // A missing line is its own identity, not line zero.
        assert_ne!(
            base,
            computed_fingerprint("src/lib.rs", None, "testing", "Missing test")
        );
        assert_eq!(
            base.len(),
            16,
            "the fingerprint is a fixed-width hex digest"
        );
        assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
    }

    #[test]
    fn a_validation_error_reports_the_first_typed_code_and_the_row_count() {
        let untyped = validation_error(vec![RowError::new(
            "/repo/findings.jsonl".to_string(),
            1,
            "bad".to_string(),
        )]);
        assert_eq!(untyped.code(), "invalid-findings");

        let typed = validation_error(vec![
            RowError::new("/repo/findings.jsonl".to_string(), 1, "bad".to_string()),
            RowError::typed(
                "/repo/findings.jsonl".to_string(),
                2,
                "review_fingerprint_collision",
                "collision",
            ),
        ]);
        assert_eq!(typed.code(), "review_fingerprint_collision");
    }
}

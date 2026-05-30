use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};
use nils_markdown::Engine;
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;
use time::{Date, Duration, Month, OffsetDateTime};

use crate::common::{CliError, OutputFormat, display_path, render_error};
use crate::completion::{self, CompletionShell};

const REPO_RETRO_TEMPLATE: &str = include_str!("../templates/repo_retro.md.tera");
const REPO_RETRO_TEMPLATE_NAME: &str = "repo_retro";

#[derive(Debug, Serialize)]
struct RepoRetroView<'a> {
    repo_name: &'a str,
    generated_at: &'a str,
    mode: &'a str,
    window_label: &'a str,
    window_start: &'a str,
    window_end: &'a str,
    repo_root: &'a str,
    commit_count: usize,
    changed_lines: i64,
    insertions: i64,
    deletions: i64,
    active_days_count: usize,
    test_related_commits: usize,
    churn_by_class_block: String,
    themes_block: String,
    attention_items_block: String,
    hotspots_block: String,
    validation_signals_block: String,
    heuristic_state: &'a str,
    heuristic_active_inbox_total: usize,
    heuristic_movement_summary: &'a str,
    heuristic_op_records_changed: usize,
    heuristic_aging_summary: &'a str,
    follow_up_questions_block: String,
    show_warnings: bool,
    warnings_block: String,
}

const REPORT_ENVELOPE_SCHEMA_VERSION: &str = "cli.repo-retro.report.v2";
const REPORT_SCHEMA_VERSION: &str = "repo-retro.report.v2";
const INDEX_SCHEMA_VERSION: &str = "repo-retro.index.v1";
const REPORT_COMMAND: &str = "repo-retro report";
const COMMIT_TYPES: &[&str] = &[
    "feat", "fix", "refactor", "test", "docs", "chore", "ci", "release", "other",
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
        Err(err) => return crate::common::handle_parse_error("repo-retro", argv, err),
    };

    match cli.command {
        CommandKind::Report(args) => report(*args),
        CommandKind::Completion(args) => completion::run::<Cli>(args.shell, "repo-retro"),
    }
}

fn report(args: ReportArgs) -> i32 {
    let format = args.format;
    match build_report(&args).and_then(|report| {
        write_history(&report)?;
        Ok(report)
    }) {
        Ok(report) => match format {
            ReportFormat::Json => {
                let envelope = nils_common::cli_contract::Envelope::success(
                    REPORT_ENVELOPE_SCHEMA_VERSION,
                    &report,
                );
                println!(
                    "{}",
                    serde_json::to_string_pretty(&envelope)
                        .expect("report envelope should serialize")
                );
                0
            }
            ReportFormat::Markdown => {
                print!("{}", render_markdown(&report));
                0
            }
        },
        Err(err) => render_error(
            REPORT_ENVELOPE_SCHEMA_VERSION,
            REPORT_COMMAND,
            format.error_output_format(),
            err,
        ),
    }
}

#[derive(Parser)]
#[command(
    name = "repo-retro",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Generate deterministic local repository retrospectives",
    long_about = "Generate source-grounded repository retrospectives from Git history and optional structured evidence inputs.",
    after_help = "EXAMPLES:\n  repo-retro report --repo . --from 2026-05-01 --to 2026-05-07 --mode team\n  repo-retro report --repo . --since 7d --format json --write\n  repo-retro completion zsh\n\nENVIRONMENT:\n  HOME  Fallback base path for local history expansion.\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
)]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand)]
enum CommandKind {
    Report(Box<ReportArgs>),
    Completion(CompletionArgs),
}

#[derive(Args)]
struct CompletionArgs {
    shell: CompletionShell,
}

#[derive(Args)]
struct ReportArgs {
    #[arg(long, default_value = ".", value_hint = ValueHint::DirPath, help = "Git work tree to inspect")]
    repo: PathBuf,
    #[arg(long, value_enum, default_value_t = ReviewMode::Team, help = "Review framing mode")]
    mode: ReviewMode,
    #[arg(long, value_enum, default_value_t = ReportFormat::Markdown, help = "Output format")]
    format: ReportFormat,
    #[arg(
        long,
        help = "Start date for an open-ended window ending today (YYYY-MM-DD)"
    )]
    since: Option<String>,
    #[arg(long, help = "Rolling window length ending today")]
    days: Option<i64>,
    #[arg(
        long = "from",
        help = "Fixed inclusive start date (YYYY-MM-DD). Requires --to"
    )]
    from_date: Option<String>,
    #[arg(
        long = "to",
        help = "Fixed inclusive end date (YYYY-MM-DD). Requires --from"
    )]
    to_date: Option<String>,
    #[arg(long, value_hint = ValueHint::FilePath, help = "Optional explicit timeline JSONL input")]
    timeline_jsonl: Option<PathBuf>,
    #[arg(long, value_hint = ValueHint::FilePath, help = "Optional explicit learnings JSONL input")]
    learnings_jsonl: Option<PathBuf>,
    #[arg(long, value_hint = ValueHint::FilePath, help = "Optional explicit validation JSONL input")]
    validation_jsonl: Option<PathBuf>,
    #[arg(long, value_hint = ValueHint::FilePath, help = "Optional explicit review JSONL input")]
    review_jsonl: Option<PathBuf>,
    #[arg(long, value_hint = ValueHint::FilePath, help = "Optional explicit incidents JSONL input")]
    incidents_jsonl: Option<PathBuf>,
    #[arg(long, value_hint = ValueHint::FilePath, help = "Optional explicit decisions JSONL input")]
    decisions_jsonl: Option<PathBuf>,
    #[arg(long, value_hint = ValueHint::DirPath, help = "Explicit local history directory. Does not write unless --write is also set")]
    history_dir: Option<PathBuf>,
    #[arg(
        long,
        help = "Write Markdown, raw JSON, and index.jsonl under --history-dir"
    )]
    write: bool,
    #[arg(long = "path-class-config", value_hint = ValueHint::FilePath, help = "Optional JSON file overriding path-class assignment: {\"<class>\": [\"<path-prefix>\", ...]}")]
    path_class_config: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum ReportFormat {
    Json,
    Markdown,
}

impl ReportFormat {
    fn error_output_format(self) -> OutputFormat {
        match self {
            Self::Json => OutputFormat::Json,
            Self::Markdown => OutputFormat::Text,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum ReviewMode {
    Personal,
    Team,
    Maintainer,
}

impl ReviewMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Team => "team",
            Self::Maintainer => "maintainer",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Personal => "Personal",
            Self::Team => "Team",
            Self::Maintainer => "Maintainer",
        }
    }
}

#[derive(Clone, Debug)]
struct ChangedFile {
    path: String,
    insertions: i64,
    deletions: i64,
}

impl ChangedFile {
    fn changed_lines(&self) -> i64 {
        self.insertions + self.deletions
    }
}

#[derive(Clone, Debug)]
struct Commit {
    hash: String,
    date: String,
    author: String,
    email: String,
    subject: String,
    files: Vec<ChangedFile>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RepoRetroReport {
    schema: String,
    mode: String,
    generated_at: String,
    repo: RepoIdentity,
    window: Window,
    git: GitReport,
    heuristic_system: HeuristicSystemReport,
    optional_inputs: BTreeMap<String, JsonlSummary>,
    history: HistoryMetadata,
    analysis: Analysis,
    warnings: Vec<String>,
    sources: Sources,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RepoIdentity {
    root: String,
    name: String,
    slug: String,
    branch: String,
    head: String,
    remote: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Window {
    mode: String,
    label: String,
    start: String,
    end: String,
    days: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GitReport {
    summary: CommitSummary,
    commit_types: BTreeMap<String, usize>,
    authors: Vec<AuthorSummary>,
    churn_by_class: ChurnByClass,
    archival: Archival,
    file_hotspots: FileHotspots,
    test_signals: TestSignals,
    recent_commits: Vec<RecentCommit>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CommitSummary {
    commit_count: usize,
    active_days: Vec<String>,
    first_commit_date: Option<String>,
    last_commit_date: Option<String>,
    insertions: i64,
    deletions: i64,
    changed_lines: i64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthorSummary {
    name: String,
    email: String,
    commit_count: usize,
    insertions: i64,
    deletions: i64,
    changed_lines: i64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileChangeSummary {
    path: String,
    commits: usize,
    insertions: i64,
    deletions: i64,
    changed_lines: i64,
    class: PathClass,
    net_deleted: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AreaSummary {
    area: String,
    file_count: usize,
    insertions: i64,
    deletions: i64,
    changed_lines: i64,
    class: PathClass,
}

/// Per-class churn rollup — the deterministic headline that separates real
/// engineering surface from process-doc byproducts.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChurnByClass {
    source: ClassChurn,
    tests: ClassChurn,
    product_docs: ClassChurn,
    process_artifacts: ClassChurn,
    other: ClassChurn,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClassChurn {
    file_count: usize,
    commits: usize,
    insertions: i64,
    deletions: i64,
    changed_lines: i64,
}

/// Archival facts. Net-deletion (`insertions == 0 && deletions > 0`) is the
/// primary signal; `plans_archived_estimate` is a secondary commit-subject
/// heuristic only.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Archival {
    net_deleted_file_count: usize,
    net_deleted_files: Vec<NetDeletedFile>,
    process_artifacts_deleted_lines: i64,
    plans_archived_estimate: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NetDeletedFile {
    path: String,
    deletions: i64,
    class: PathClass,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileHotspots {
    top_files: Vec<FileChangeSummary>,
    top_areas: Vec<AreaSummary>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TestSignals {
    changed_test_files: Vec<String>,
    changed_test_file_count: usize,
    test_related_commit_count: usize,
    test_changed_lines: i64,
    test_loc_ratio: Option<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RecentCommit {
    hash: String,
    date: String,
    commit_type: String,
    author: String,
    subject: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HeuristicSystemReport {
    state: String,
    active_inbox: ActiveInboxSummary,
    error_inbox_movement: ErrorInboxMovement,
    operation_records: OperationRecords,
    aging: HeuristicAging,
    boundary: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActiveInboxSummary {
    state: String,
    total: usize,
    by_status: BTreeMap<String, usize>,
    by_severity: BTreeMap<String, usize>,
    entries: Vec<InboxEntry>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InboxEntry {
    path: String,
    status: String,
    severity: String,
    first_observed: Option<String>,
    age_days: Option<i64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorInboxMovement {
    added: MovementBucket,
    modified: MovementBucket,
    archived: MovementBucket,
    removed: MovementBucket,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MovementBucket {
    count: usize,
    paths: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationRecords {
    changed_count: usize,
    by_status: BTreeMap<String, usize>,
    paths: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HeuristicAging {
    oldest_open_days: Option<i64>,
    entries_over_30_days: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonlSummary {
    provided: bool,
    path: Option<String>,
    total_lines: usize,
    valid_lines: usize,
    malformed_lines: usize,
    malformed: Vec<MalformedLine>,
    recent: Vec<JsonlItemSummary>,
}

#[derive(Clone, Serialize)]
struct MalformedLine {
    line: usize,
    error: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonlItemSummary {
    timestamp: Option<String>,
    summary: Option<String>,
    keys: Vec<String>,
    item_type: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryMetadata {
    enabled: bool,
    write: bool,
    history_dir: Option<String>,
    intended: Option<HistoryPaths>,
    written: Vec<String>,
    comparison: HistoryComparison,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryPaths {
    markdown: String,
    json: String,
    index: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryComparison {
    status: String,
    prior_schema: Option<String>,
    prior_window: Option<Value>,
    prior_commit_count: Option<i64>,
    commit_count_delta: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Analysis {
    themes: Vec<String>,
    attention_items: Vec<String>,
    follow_up_questions: Vec<String>,
    validation_signals: Vec<String>,
    heuristic_system_review: HeuristicSystemReview,
    history_comparison: HistoryComparison,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HeuristicSystemReview {
    boundary: String,
    active_inbox_total: usize,
    high_severity_count: usize,
    movement_summary: String,
    aging_summary: String,
}

#[derive(Serialize)]
struct Sources {
    commands: Vec<SourceCommand>,
}

#[derive(Serialize)]
struct SourceCommand {
    label: String,
    command: String,
}

#[derive(Clone)]
struct NameStatusEvent {
    status: String,
    path: String,
}

fn build_report(args: &ReportArgs) -> Result<RepoRetroReport, CliError> {
    let repo = repo_root(&expand_user(&args.repo))?;
    let window = resolve_window(args)?;
    let mut warnings = Vec::new();
    let mut sources = Vec::new();
    let (identity, identity_warnings) = repo_identity(&repo);
    warnings.extend(identity_warnings);
    let commits = collect_git_commits(&repo, &window, &mut warnings, &mut sources)?;
    let classifier = load_path_classifier(args.path_class_config.as_ref())?;
    let git = summarize_commits(&commits, &classifier);
    if commits.is_empty() {
        warnings.push("selected window has no commits".to_string());
    }
    let heuristic_system = summarize_heuristic_system(&repo, &window, &mut sources)?;
    let optional_inputs = load_optional_inputs(args)?;
    for (label, summary) in &optional_inputs {
        if summary.malformed_lines > 0 {
            warnings.push(format!(
                "{label} JSONL had {} malformed line(s)",
                summary.malformed_lines
            ));
        }
    }
    let mut history = build_history_metadata(args, &identity.slug, &window.end)?;
    if let Some(prior_count) = history.comparison.prior_commit_count {
        history.comparison.commit_count_delta = Some(git.summary.commit_count as i64 - prior_count);
    }
    let analysis = build_analysis(
        args.mode,
        &git,
        &heuristic_system,
        &optional_inputs,
        &warnings,
        &history,
    );
    Ok(RepoRetroReport {
        schema: REPORT_SCHEMA_VERSION.to_string(),
        mode: args.mode.as_str().to_string(),
        generated_at: now_rfc3339()?,
        repo: identity,
        window,
        git,
        heuristic_system,
        optional_inputs,
        history,
        analysis,
        warnings,
        sources: Sources { commands: sources },
    })
}

fn resolve_window(args: &ReportArgs) -> Result<Window, CliError> {
    let supplied = [
        args.since.is_some(),
        args.days.is_some(),
        args.from_date.is_some() || args.to_date.is_some(),
    ]
    .into_iter()
    .filter(|used| *used)
    .count();
    if supplied > 1 {
        return Err(CliError::usage(
            "ambiguous-window",
            "choose only one window mode: --since, --days, or --from/--to",
            None,
        ));
    }

    let today = today_local();
    let (mode, label, start, end) = if args.from_date.is_some() || args.to_date.is_some() {
        let from = args.from_date.as_deref().ok_or_else(|| {
            CliError::usage(
                "incomplete-window",
                "--from and --to must be supplied together",
                None,
            )
        })?;
        let to = args.to_date.as_deref().ok_or_else(|| {
            CliError::usage(
                "incomplete-window",
                "--from and --to must be supplied together",
                None,
            )
        })?;
        let start = parse_iso_date(from, "--from")?;
        let end = parse_iso_date(to, "--to")?;
        (
            "fixed".to_string(),
            format!("{}..{}", date_string(start), date_string(end)),
            start,
            end,
        )
    } else if let Some(since) = &args.since {
        let start = parse_iso_date(since, "--since")?;
        (
            "since".to_string(),
            format!("since {}", date_string(start)),
            start,
            today,
        )
    } else if let Some(days) = args.days {
        if days <= 0 {
            return Err(CliError::usage(
                "invalid-window",
                "--days must be a positive integer",
                Some(json!({ "days": days })),
            ));
        }
        let start = today - Duration::days(days - 1);
        (
            "rolling".to_string(),
            format!("last {days} day(s)"),
            start,
            today,
        )
    } else {
        let start = today - Duration::days(today.weekday().number_days_from_monday() as i64);
        (
            "current_week".to_string(),
            "current week".to_string(),
            start,
            today,
        )
    };

    if start > end {
        return Err(CliError::usage(
            "invalid-window",
            "window start must be on or before window end",
            Some(json!({ "start": date_string(start), "end": date_string(end) })),
        ));
    }

    Ok(Window {
        mode,
        label,
        start: date_string(start),
        end: date_string(end),
        days: (end.to_julian_day() - start.to_julian_day() + 1) as i64,
    })
}

fn parse_iso_date(value: &str, flag: &str) -> Result<Date, CliError> {
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() != 3 {
        return Err(invalid_date(flag, value));
    }
    let year = parts[0]
        .parse::<i32>()
        .map_err(|_| invalid_date(flag, value))?;
    let month_number = parts[1]
        .parse::<u8>()
        .map_err(|_| invalid_date(flag, value))?;
    let day = parts[2]
        .parse::<u8>()
        .map_err(|_| invalid_date(flag, value))?;
    let month = Month::try_from(month_number).map_err(|_| invalid_date(flag, value))?;
    Date::from_calendar_date(year, month, day).map_err(|_| invalid_date(flag, value))
}

fn invalid_date(flag: &str, value: &str) -> CliError {
    CliError::usage(
        "invalid-date",
        format!("invalid {flag}: {value}"),
        Some(json!({ "flag": flag, "value": value })),
    )
}

fn date_string(date: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

fn today_local() -> Date {
    OffsetDateTime::now_local()
        .map(|date_time| date_time.date())
        .unwrap_or_else(|_| OffsetDateTime::now_utc().date())
}

fn now_rfc3339() -> Result<String, CliError> {
    OffsetDateTime::now_utc().format(&Rfc3339).map_err(|err| {
        CliError::runtime(
            "timestamp-format-failed",
            format!("failed to format timestamp: {err}"),
            None,
        )
    })
}

fn repo_root(path: &Path) -> Result<PathBuf, CliError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|err| {
            CliError::runtime("git-unavailable", format!("failed to run git: {err}"), None)
        })?;
    if !output.status.success() {
        return Err(CliError::runtime(
            "not-git-work-tree",
            format!("not a git work tree: {}", path.display()),
            Some(json!({ "path": display_path(path) })),
        ));
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(root))
}

fn repo_identity(repo: &Path) -> (RepoIdentity, Vec<String>) {
    let mut warnings = Vec::new();
    let branch = run_git_optional(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_else(|| "unknown".to_string());
    let head = match run_git_optional(repo, &["rev-parse", "--short", "HEAD"]) {
        Some(value) => value,
        None => {
            warnings.push("git repository has no commits".to_string());
            "unknown".to_string()
        }
    };
    let remote = run_git_optional(repo, &["config", "--get", "remote.origin.url"]);
    let name = repo
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());
    (
        RepoIdentity {
            root: repo.display().to_string(),
            slug: slugify(&name),
            name,
            branch,
            head,
            remote,
        },
        warnings,
    )
}

fn run_git_optional(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn collect_git_commits(
    repo: &Path,
    window: &Window,
    warnings: &mut Vec<String>,
    sources: &mut Vec<SourceCommand>,
) -> Result<Vec<Commit>, CliError> {
    let args = vec![
        "log".to_string(),
        format!("--since={} 00:00:00", window.start),
        format!("--until={} 23:59:59", window.end),
        // --since/--until select on committer date and parse the boundaries in
        // local time, so collect the committer date (%cd) rendered in that same
        // local zone (short-local). Using %ad or the stored-zone short format
        // can surface a day outside the requested window in activeDays.
        "--date=short-local".to_string(),
        "--pretty=format:COMMIT%x09%H%x09%cd%x09%an%x09%ae%x09%s".to_string(),
        "--numstat".to_string(),
    ];
    let command = git_command_text(repo, &args);
    let (stdout, warning) = run_git(repo, &args, true)?;
    sources.push(SourceCommand {
        label: "git log commits with numstat".to_string(),
        command,
    });
    if let Some(warning) = warning {
        warnings.push(warning);
    }
    Ok(parse_git_log(&stdout))
}

fn run_git(
    repo: &Path,
    args: &[String],
    allow_empty_repo: bool,
) -> Result<(String, Option<String>), CliError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|err| {
            CliError::runtime("git-unavailable", format!("failed to run git: {err}"), None)
        })?;
    if output.status.success() {
        return Ok((String::from_utf8_lossy(&output.stdout).to_string(), None));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if allow_empty_repo
        && (stderr.contains("does not have any commits")
            || stderr.contains("ambiguous argument")
            || stderr.contains("unknown revision"))
    {
        return Ok((
            "".to_string(),
            Some("git repository has no commits".to_string()),
        ));
    }
    Err(CliError::runtime(
        "git-command-failed",
        format!(
            "git command failed: {}\n{}",
            git_command_text(repo, args),
            stderr.trim()
        ),
        Some(json!({ "command": git_command_text(repo, args) })),
    ))
}

fn git_command_text(repo: &Path, args: &[String]) -> String {
    let mut parts = vec![
        "git".to_string(),
        "-C".to_string(),
        repo.display().to_string(),
    ];
    parts.extend(args.iter().cloned());
    shell_join(&parts)
}

fn shell_join(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| sh_quote(part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn sh_quote(value: &str) -> String {
    static SAFE_RE: OnceLock<Regex> = OnceLock::new();
    let re = SAFE_RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9_./:=@%+-]+$").expect("regex"));
    if re.is_match(value) {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

fn parse_git_log(raw: &str) -> Vec<Commit> {
    let mut commits = Vec::new();
    let mut current: Option<Commit> = None;
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with("COMMIT\t") {
            if let Some(commit) = current.take() {
                commits.push(commit);
            }
            let parts: Vec<&str> = line.splitn(6, '\t').collect();
            if parts.len() == 6 {
                current = Some(Commit {
                    hash: parts[1].to_string(),
                    date: parts[2].to_string(),
                    author: parts[3].to_string(),
                    email: parts[4].to_string(),
                    subject: parts[5].to_string(),
                    files: Vec::new(),
                });
            }
            continue;
        }
        if let Some(commit) = current.as_mut()
            && let Some(changed) = parse_numstat(line)
        {
            commit.files.push(changed);
        }
    }
    if let Some(commit) = current {
        commits.push(commit);
    }
    commits
}

fn parse_numstat(line: &str) -> Option<ChangedFile> {
    let parts: Vec<&str> = line.splitn(3, '\t').collect();
    if parts.len() != 3 {
        return None;
    }
    let binary = parts[0] == "-" || parts[1] == "-";
    Some(ChangedFile {
        path: parts[2].to_string(),
        insertions: if binary { 0 } else { parts[0].parse().ok()? },
        deletions: if binary { 0 } else { parts[1].parse().ok()? },
    })
}

fn summarize_commits(commits: &[Commit], classifier: &PathClassifier) -> GitReport {
    let mut type_counts: BTreeMap<String, usize> = COMMIT_TYPES
        .iter()
        .map(|kind| ((*kind).to_string(), 0))
        .collect();
    let mut authors: BTreeMap<String, AuthorSummary> = BTreeMap::new();
    let mut files: BTreeMap<String, FileChangeSummary> = BTreeMap::new();
    let mut areas: BTreeMap<String, AreaAccum> = BTreeMap::new();
    let mut class_accum: BTreeMap<PathClass, ClassAccum> = BTreeMap::new();
    let mut active_days = BTreeSet::new();
    let mut changed_test_files = BTreeSet::new();
    let mut total_insertions = 0;
    let mut total_deletions = 0;
    let mut test_changed_lines = 0;
    let mut test_related_commit_count = 0;
    let plans_archived_estimate = commits
        .iter()
        .filter(|commit| subject_is_archival(&commit.subject))
        .count();

    for commit in commits {
        let commit_type = classify_commit(&commit.subject);
        *type_counts.entry(commit_type.clone()).or_default() += 1;
        active_days.insert(commit.date.clone());
        let author_key = format!("{} <{}>", commit.author, commit.email);
        let author = authors.entry(author_key).or_insert_with(|| AuthorSummary {
            name: commit.author.clone(),
            email: commit.email.clone(),
            commit_count: 0,
            insertions: 0,
            deletions: 0,
            changed_lines: 0,
        });
        author.commit_count += 1;
        if commit_type == "test" || subject_is_test_related(&commit.subject) {
            test_related_commit_count += 1;
        }
        for changed in &commit.files {
            let class = classifier.classify(&changed.path);
            total_insertions += changed.insertions;
            total_deletions += changed.deletions;
            author.insertions += changed.insertions;
            author.deletions += changed.deletions;
            author.changed_lines += changed.changed_lines();
            let file = files
                .entry(changed.path.clone())
                .or_insert_with(|| FileChangeSummary {
                    path: changed.path.clone(),
                    commits: 0,
                    insertions: 0,
                    deletions: 0,
                    changed_lines: 0,
                    class,
                    net_deleted: false,
                });
            file.commits += 1;
            file.insertions += changed.insertions;
            file.deletions += changed.deletions;
            file.changed_lines += changed.changed_lines();
            let area_item = areas.entry(top_level_area(&changed.path)).or_default();
            area_item.paths.insert(changed.path.clone());
            area_item.insertions += changed.insertions;
            area_item.deletions += changed.deletions;
            *area_item.class_lines.entry(class).or_default() += changed.changed_lines();
            let class_item = class_accum.entry(class).or_default();
            class_item.files.insert(changed.path.clone());
            class_item.commits.insert(commit.hash.clone());
            class_item.insertions += changed.insertions;
            class_item.deletions += changed.deletions;
            if is_test_path(&changed.path) {
                changed_test_files.insert(changed.path.clone());
                test_changed_lines += changed.changed_lines();
            }
        }
    }

    let mut authors: Vec<AuthorSummary> = authors.into_values().collect();
    authors.sort_by(|left, right| {
        right
            .commit_count
            .cmp(&left.commit_count)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });

    let mut all_files: Vec<FileChangeSummary> = files.into_values().collect();
    for file in &mut all_files {
        // Net-deletion is the primary archival signal: a file with no
        // insertions and some deletions across the window was removed, not
        // reworked, and must never be nominated for review.
        file.net_deleted = file.insertions == 0 && file.deletions > 0;
    }

    let mut net_deleted_files: Vec<NetDeletedFile> = all_files
        .iter()
        .filter(|file| file.net_deleted)
        .map(|file| NetDeletedFile {
            path: file.path.clone(),
            deletions: file.deletions,
            class: file.class,
        })
        .collect();
    net_deleted_files.sort_by(|left, right| {
        right
            .deletions
            .cmp(&left.deletions)
            .then_with(|| left.path.cmp(&right.path))
    });
    let process_artifacts_deleted_lines = net_deleted_files
        .iter()
        .filter(|file| file.class == PathClass::ProcessArtifacts)
        .map(|file| file.deletions)
        .sum();
    let archival = Archival {
        net_deleted_file_count: net_deleted_files.len(),
        net_deleted_files,
        process_artifacts_deleted_lines,
        plans_archived_estimate,
    };

    // Rank hotspots by how often a file was touched (commit count), not raw
    // line churn, so an iterated source file outranks a once-written plan.
    let mut top_files = all_files;
    top_files.sort_by(|left, right| {
        right
            .commits
            .cmp(&left.commits)
            .then_with(|| right.changed_lines.cmp(&left.changed_lines))
            .then_with(|| left.path.cmp(&right.path))
    });
    top_files.truncate(10);

    let mut top_areas: Vec<AreaSummary> = areas
        .into_iter()
        .map(|(area, accum)| AreaSummary {
            area,
            file_count: accum.paths.len(),
            insertions: accum.insertions,
            deletions: accum.deletions,
            changed_lines: accum.insertions + accum.deletions,
            class: dominant_class(&accum.class_lines),
        })
        .collect();
    top_areas.sort_by(|left, right| {
        right
            .changed_lines
            .cmp(&left.changed_lines)
            .then_with(|| left.area.cmp(&right.area))
    });
    top_areas.truncate(10);

    let churn_by_class = build_churn_by_class(&class_accum);

    let active_days: Vec<String> = active_days.into_iter().collect();
    let total_changed = total_insertions + total_deletions;
    GitReport {
        summary: CommitSummary {
            commit_count: commits.len(),
            first_commit_date: active_days.first().cloned(),
            last_commit_date: active_days.last().cloned(),
            active_days,
            insertions: total_insertions,
            deletions: total_deletions,
            changed_lines: total_changed,
        },
        commit_types: type_counts,
        authors,
        churn_by_class,
        archival,
        file_hotspots: FileHotspots {
            top_files,
            top_areas,
        },
        test_signals: TestSignals {
            changed_test_file_count: changed_test_files.len(),
            changed_test_files: changed_test_files.into_iter().collect(),
            test_related_commit_count,
            test_changed_lines,
            test_loc_ratio: if total_changed == 0 {
                None
            } else {
                Some(
                    ((test_changed_lines as f64 / total_changed as f64) * 10000.0).round()
                        / 10000.0,
                )
            },
        },
        recent_commits: commits
            .iter()
            .take(10)
            .map(|commit| RecentCommit {
                hash: commit.hash.chars().take(12).collect(),
                date: commit.date.clone(),
                commit_type: classify_commit(&commit.subject),
                author: commit.author.clone(),
                subject: commit.subject.clone(),
            })
            .collect(),
    }
}

fn classify_commit(subject: &str) -> String {
    static COMMIT_TYPE_RE: OnceLock<Regex> = OnceLock::new();
    let re =
        COMMIT_TYPE_RE.get_or_init(|| Regex::new(r"^([a-z]+)(?:\([^)]+\))?!?:").expect("regex"));
    if let Some(captures) = re.captures(subject.trim()) {
        let kind = captures.get(1).map(|item| item.as_str()).unwrap_or("other");
        if COMMIT_TYPES.contains(&kind) {
            return kind.to_string();
        }
    }
    "other".to_string()
}

fn subject_is_test_related(subject: &str) -> bool {
    static TEST_SUBJECT_RE: OnceLock<Regex> = OnceLock::new();
    TEST_SUBJECT_RE
        .get_or_init(|| Regex::new(r"(?i)\b(test|pytest|spec|validation)\b").expect("regex"))
        .is_match(subject)
}

fn is_test_path(path: &str) -> bool {
    static TEST_PATH_RE: OnceLock<Regex> = OnceLock::new();
    TEST_PATH_RE
        .get_or_init(|| {
            Regex::new(r"(^|/)(tests?|specs?)/|(^|/)(test_[^/]+|[^/]+_test)\.|(\.spec|\.test)\.")
                .expect("regex")
        })
        .is_match(path)
}

/// Test-tree paths: a `tests/` / `test/` directory segment or a test-file
/// name. Unlike `is_test_path` this omits a bare `specs/` directory, so
/// `docs/specs` product docs are not swept in. It is checked before the `.md`
/// rule so generated Markdown fixtures under `tests/golden/**` classify as
/// tests, not product-docs.
fn is_test_tree_path(path: &str) -> bool {
    static TEST_TREE_RE: OnceLock<Regex> = OnceLock::new();
    TEST_TREE_RE
        .get_or_init(|| {
            Regex::new(r"(^|/)tests?/|(^|/)(test_[^/]+|[^/]+_test)\.|(\.spec|\.test)\.")
                .expect("regex")
        })
        .is_match(path)
}

fn top_level_area(path: &str) -> String {
    path.split('/').next().unwrap_or(".").to_string()
}

/// Deterministic path classification. `source`/`tests`/`productDocs` describe
/// real engineering surface; `processArtifacts` captures workflow byproducts
/// (plan and discussion authoring, heuristic-system records) that would
/// otherwise dominate raw line-churn ranking; `other` is reachable only via an
/// explicit config override.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
enum PathClass {
    Source,
    Tests,
    ProductDocs,
    ProcessArtifacts,
    Other,
}

impl PathClass {
    fn from_config_key(key: &str) -> Option<Self> {
        match key {
            "source" => Some(Self::Source),
            "tests" => Some(Self::Tests),
            "productDocs" => Some(Self::ProductDocs),
            "processArtifacts" => Some(Self::ProcessArtifacts),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

fn path_class_label(class: PathClass) -> &'static str {
    match class {
        PathClass::Source => "source",
        PathClass::Tests => "tests",
        PathClass::ProductDocs => "product-docs",
        PathClass::ProcessArtifacts => "process-artifacts",
        PathClass::Other => "other",
    }
}

/// Built-in default classification. Absent any convention (no `docs/plans`,
/// no `docs`), a repo simply yields `source`/`tests` and empty doc classes —
/// never a misclassification.
fn default_path_class(path: &str) -> PathClass {
    if path.starts_with("docs/plans/")
        || path.starts_with("docs/discussions/")
        || path.starts_with("heuristic-system/error-inbox/")
        || path.starts_with("heuristic-system/operation-records/")
    {
        return PathClass::ProcessArtifacts;
    }
    // Test-tree files — including generated Markdown fixtures under
    // `tests/golden/**` — are tests, classified before the `.md` rule so they
    // are not mislabeled product-docs.
    if is_test_tree_path(path) {
        return PathClass::Tests;
    }
    // Any remaining Markdown is documentation, classified before the residual
    // test/spec heuristic — `is_test_path` also matches a bare `specs/` segment
    // (e.g. `docs/specs/`), which is product docs, not tests.
    if path.to_lowercase().ends_with(".md") {
        return PathClass::ProductDocs;
    }
    if is_test_path(path) {
        return PathClass::Tests;
    }
    PathClass::Source
}

#[derive(Default)]
struct PathClassifier {
    /// (class, path-prefix) overrides, checked before the built-in defaults in
    /// declared order; the first matching prefix wins.
    overrides: Vec<(PathClass, String)>,
}

impl PathClassifier {
    fn classify(&self, path: &str) -> PathClass {
        for (class, prefix) in &self.overrides {
            if path.starts_with(prefix.as_str()) {
                return *class;
            }
        }
        default_path_class(path)
    }
}

fn load_path_classifier(path: Option<&PathBuf>) -> Result<PathClassifier, CliError> {
    let Some(path) = path else {
        return Ok(PathClassifier::default());
    };
    let path = expand_user(path);
    let body = fs::read_to_string(&path).map_err(|err| {
        CliError::runtime(
            "read-path-class-config-failed",
            format!("failed to read {}: {err}", path.display()),
            Some(json!({ "path": display_path(&path) })),
        )
    })?;
    let value: Value = serde_json::from_str(&body).map_err(|err| {
        CliError::data(
            "invalid-path-class-config",
            format!("failed to parse {} as JSON: {err}", path.display()),
            Some(json!({ "path": display_path(&path) })),
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        CliError::data(
            "invalid-path-class-config",
            "path-class config must be a JSON object of { class: [path-prefix, ...] }",
            Some(json!({ "path": display_path(&path) })),
        )
    })?;
    let mut overrides = Vec::new();
    for (key, entry) in object {
        let class = PathClass::from_config_key(key).ok_or_else(|| {
            CliError::data(
                "invalid-path-class-config",
                format!(
                    "unknown path class `{key}`; expected source, tests, productDocs, processArtifacts, or other"
                ),
                Some(json!({ "class": key })),
            )
        })?;
        let prefixes = entry.as_array().ok_or_else(|| {
            CliError::data(
                "invalid-path-class-config",
                format!("path class `{key}` must map to an array of path prefixes"),
                Some(json!({ "class": key })),
            )
        })?;
        for prefix in prefixes {
            let prefix = prefix.as_str().ok_or_else(|| {
                CliError::data(
                    "invalid-path-class-config",
                    format!("path class `{key}` prefixes must be strings"),
                    Some(json!({ "class": key })),
                )
            })?;
            overrides.push((class, prefix.to_string()));
        }
    }
    Ok(PathClassifier { overrides })
}

/// Per-class churn accumulator. `commits` is the set of distinct commit hashes
/// that touched a file of the class, so a commit spanning several files of one
/// class still counts once.
#[derive(Default)]
struct ClassAccum {
    files: BTreeSet<String>,
    commits: BTreeSet<String>,
    insertions: i64,
    deletions: i64,
}

#[derive(Default)]
struct AreaAccum {
    paths: BTreeSet<String>,
    insertions: i64,
    deletions: i64,
    class_lines: BTreeMap<PathClass, i64>,
}

fn build_churn_by_class(accum: &BTreeMap<PathClass, ClassAccum>) -> ChurnByClass {
    let churn = |class: PathClass| match accum.get(&class) {
        Some(item) => ClassChurn {
            file_count: item.files.len(),
            commits: item.commits.len(),
            insertions: item.insertions,
            deletions: item.deletions,
            changed_lines: item.insertions + item.deletions,
        },
        None => ClassChurn {
            file_count: 0,
            commits: 0,
            insertions: 0,
            deletions: 0,
            changed_lines: 0,
        },
    };
    ChurnByClass {
        source: churn(PathClass::Source),
        tests: churn(PathClass::Tests),
        product_docs: churn(PathClass::ProductDocs),
        process_artifacts: churn(PathClass::ProcessArtifacts),
        other: churn(PathClass::Other),
    }
}

/// Dominant class for an area: most changed lines wins; ties resolve to the
/// lowest `PathClass` ordinal for determinism.
fn dominant_class(class_lines: &BTreeMap<PathClass, i64>) -> PathClass {
    let mut best: Option<(PathClass, i64)> = None;
    for (class, lines) in class_lines {
        match best {
            Some((_, best_lines)) if best_lines >= *lines => {}
            _ => best = Some((*class, *lines)),
        }
    }
    best.map(|(class, _)| class).unwrap_or(PathClass::Other)
}

fn subject_is_archival(subject: &str) -> bool {
    static ARCHIVE_RE: OnceLock<Regex> = OnceLock::new();
    ARCHIVE_RE
        .get_or_init(|| Regex::new(r"(?i)\barchive\b").expect("regex"))
        .is_match(subject)
}

fn summarize_heuristic_system(
    repo: &Path,
    window: &Window,
    sources: &mut Vec<SourceCommand>,
) -> Result<HeuristicSystemReport, CliError> {
    let root = repo.join("heuristic-system");
    if !root.exists() {
        return Ok(HeuristicSystemReport {
            state: "not_present".to_string(),
            active_inbox: empty_active_inbox(),
            error_inbox_movement: summarize_error_inbox_events(&[]),
            operation_records: summarize_operation_events(&[]),
            aging: HeuristicAging {
                oldest_open_days: None,
                entries_over_30_days: Vec::new(),
            },
            boundary: "read_only".to_string(),
        });
    }
    let error_events = collect_name_status(repo, window, "heuristic-system/error-inbox", sources)?;
    let operation_events =
        collect_name_status(repo, window, "heuristic-system/operation-records", sources)?;
    let active_inbox = active_inbox_summary(repo, &window.end);
    let aging = summarize_heuristic_aging(&active_inbox);
    Ok(HeuristicSystemReport {
        state: "present".to_string(),
        active_inbox,
        error_inbox_movement: summarize_error_inbox_events(&error_events),
        operation_records: summarize_operation_events(&operation_events),
        aging,
        boundary: "read_only".to_string(),
    })
}

fn empty_active_inbox() -> ActiveInboxSummary {
    ActiveInboxSummary {
        state: "not_present".to_string(),
        total: 0,
        by_status: BTreeMap::new(),
        by_severity: BTreeMap::new(),
        entries: Vec::new(),
    }
}

fn active_inbox_summary(repo: &Path, window_end: &str) -> ActiveInboxSummary {
    let inbox = repo.join("heuristic-system").join("error-inbox");
    if !inbox.exists() {
        return empty_active_inbox();
    }
    let end_date = parse_iso_date(window_end, "window.end").ok();
    let mut by_status = BTreeMap::new();
    let mut by_severity = BTreeMap::new();
    let mut entries = Vec::new();
    if let Ok(read_dir) = fs::read_dir(&inbox) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.file_name().and_then(|name| name.to_str()) == Some("README.md")
                || path.extension().and_then(|ext| ext.to_str()) != Some("md")
            {
                continue;
            }
            let fields = extract_inbox_fields(&path);
            *by_status.entry(fields.status.clone()).or_insert(0) += 1;
            *by_severity.entry(fields.severity.clone()).or_insert(0) += 1;
            let age_days = fields
                .first_observed
                .as_deref()
                .and_then(|value| parse_iso_date(value, "first observed").ok())
                .and_then(|observed| {
                    end_date.map(|end| end.to_julian_day() - observed.to_julian_day())
                });
            entries.push(InboxEntry {
                path: path
                    .strip_prefix(repo)
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_else(|_| path.display().to_string()),
                status: fields.status,
                severity: fields.severity,
                first_observed: fields.first_observed,
                age_days: age_days.map(i64::from),
            });
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    ActiveInboxSummary {
        state: "present".to_string(),
        total: entries.len(),
        by_status,
        by_severity,
        entries,
    }
}

struct InboxFields {
    status: String,
    severity: String,
    first_observed: Option<String>,
}

fn extract_inbox_fields(path: &Path) -> InboxFields {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => {
            return InboxFields {
                status: "unreadable".to_string(),
                severity: "unknown".to_string(),
                first_observed: None,
            };
        }
    };
    InboxFields {
        status: capture_bullet_field(&text, "Status").unwrap_or_else(|| "unknown".to_string()),
        severity: capture_bullet_field(&text, "Severity").unwrap_or_else(|| "unknown".to_string()),
        first_observed: capture_bullet_field(&text, "First observed"),
    }
}

fn capture_bullet_field(text: &str, field: &str) -> Option<String> {
    let pattern = format!(r"(?m)^-\s+{}:\s*`?([^`\n]+)`?\s*$", regex::escape(field));
    Regex::new(&pattern)
        .ok()?
        .captures(text)?
        .get(1)
        .map(|value| value.as_str().trim().to_string())
}

fn summarize_heuristic_aging(active: &ActiveInboxSummary) -> HeuristicAging {
    let mut oldest_open_days = None;
    let mut entries_over_30_days = Vec::new();
    for entry in &active.entries {
        if matches!(
            entry.status.as_str(),
            "closed" | "done" | "resolved" | "completed"
        ) {
            continue;
        }
        if let Some(age) = entry.age_days {
            oldest_open_days = Some(oldest_open_days.map_or(age, |current: i64| current.max(age)));
            if age > 30 {
                entries_over_30_days.push(entry.path.clone());
            }
        }
    }
    HeuristicAging {
        oldest_open_days,
        entries_over_30_days,
    }
}

fn collect_name_status(
    repo: &Path,
    window: &Window,
    pathspec: &str,
    sources: &mut Vec<SourceCommand>,
) -> Result<Vec<NameStatusEvent>, CliError> {
    let args = vec![
        "log".to_string(),
        format!("--since={} 00:00:00", window.start),
        format!("--until={} 23:59:59", window.end),
        // parse_name_status_log consumes only status and path, so this date
        // column is unused today. Keep it aligned with collect_git_commits
        // (committer date in local time) so it stays correct if ever parsed.
        "--date=short-local".to_string(),
        "--name-status".to_string(),
        "--pretty=format:COMMIT%x09%H%x09%cd%x09%s".to_string(),
        "--".to_string(),
        pathspec.to_string(),
    ];
    let command = git_command_text(repo, &args);
    let (stdout, _) = run_git(repo, &args, true)?;
    sources.push(SourceCommand {
        label: format!("git name-status for {pathspec}"),
        command,
    });
    Ok(parse_name_status_log(&stdout))
}

fn parse_name_status_log(raw: &str) -> Vec<NameStatusEvent> {
    let mut events = Vec::new();
    let mut in_commit = false;
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with("COMMIT\t") {
            in_commit = true;
            continue;
        }
        if !in_commit {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            continue;
        }
        events.push(NameStatusEvent {
            status: parts[0].to_string(),
            path: parts.last().unwrap_or(&"").to_string(),
        });
    }
    events
}

fn summarize_error_inbox_events(events: &[NameStatusEvent]) -> ErrorInboxMovement {
    let mut added = BTreeSet::new();
    let mut modified = BTreeSet::new();
    let mut archived = BTreeSet::new();
    let mut removed = BTreeSet::new();
    for event in events {
        let path = &event.path;
        let status = event.status.as_str();
        let is_archived = format!("/{path}").contains("/archive/");
        if is_archived && matches!(status.chars().next(), Some('A' | 'M' | 'R')) {
            archived.insert(path.clone());
        } else if status.starts_with('A') {
            added.insert(path.clone());
        } else if status.starts_with('M') {
            modified.insert(path.clone());
        } else if status.starts_with('D') {
            removed.insert(path.clone());
        }
    }
    ErrorInboxMovement {
        added: movement_bucket(added),
        modified: movement_bucket(modified),
        archived: movement_bucket(archived),
        removed: movement_bucket(removed),
    }
}

fn summarize_operation_events(events: &[NameStatusEvent]) -> OperationRecords {
    let mut by_status = BTreeMap::new();
    let mut paths = BTreeSet::new();
    for event in events {
        if let Some(kind) = event.status.chars().next() {
            *by_status.entry(kind.to_string()).or_insert(0) += 1;
        }
        paths.insert(event.path.clone());
    }
    OperationRecords {
        changed_count: paths.len(),
        by_status,
        paths: paths.into_iter().collect(),
    }
}

fn movement_bucket(paths: BTreeSet<String>) -> MovementBucket {
    MovementBucket {
        count: paths.len(),
        paths: paths.into_iter().collect(),
    }
}

fn load_optional_inputs(args: &ReportArgs) -> Result<BTreeMap<String, JsonlSummary>, CliError> {
    let inputs = [
        ("timeline", &args.timeline_jsonl),
        ("learnings", &args.learnings_jsonl),
        ("validation", &args.validation_jsonl),
        ("review", &args.review_jsonl),
        ("incidents", &args.incidents_jsonl),
        ("decisions", &args.decisions_jsonl),
    ];
    let mut summaries = BTreeMap::new();
    for (label, path) in inputs {
        summaries.insert(label.to_string(), load_jsonl_summary(path.as_ref(), label)?);
    }
    Ok(summaries)
}

fn load_jsonl_summary(path: Option<&PathBuf>, label: &str) -> Result<JsonlSummary, CliError> {
    let Some(path) = path else {
        return Ok(JsonlSummary {
            provided: false,
            path: None,
            total_lines: 0,
            valid_lines: 0,
            malformed_lines: 0,
            malformed: Vec::new(),
            recent: Vec::new(),
        });
    };
    let path = expand_user(path);
    if !path.is_file() {
        return Err(CliError::runtime(
            "missing-jsonl-input",
            format!("missing {label} JSONL input: {}", path.display()),
            Some(json!({ "label": label, "path": display_path(&path) })),
        ));
    }
    let body = fs::read_to_string(&path).map_err(|err| {
        CliError::runtime(
            "read-jsonl-failed",
            format!("failed to read {}: {err}", path.display()),
            Some(json!({ "path": display_path(&path) })),
        )
    })?;
    let mut total_lines = 0;
    let mut valid = Vec::new();
    let mut malformed = Vec::new();
    for (idx, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        total_lines += 1;
        match serde_json::from_str::<Value>(line) {
            Ok(value) => valid.push(value),
            Err(err) => malformed.push(MalformedLine {
                line: idx + 1,
                error: err.to_string(),
            }),
        }
    }
    if total_lines > 0 && valid.is_empty() {
        return Err(CliError::runtime(
            "unusable-jsonl-input",
            format!(
                "{label} JSONL input has no usable lines: {}",
                path.display()
            ),
            Some(json!({ "label": label, "path": display_path(&path) })),
        ));
    }
    let recent = valid
        .iter()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(summarize_jsonl_item)
        .collect();
    Ok(JsonlSummary {
        provided: true,
        path: Some(path.display().to_string()),
        total_lines,
        valid_lines: valid.len(),
        malformed_lines: malformed.len(),
        malformed: malformed.into_iter().take(10).collect(),
        recent,
    })
}

fn summarize_jsonl_item(item: &Value) -> JsonlItemSummary {
    let Some(object) = item.as_object() else {
        return JsonlItemSummary {
            timestamp: None,
            summary: None,
            keys: Vec::new(),
            item_type: Some(value_type_name(item).to_string()),
        };
    };
    let timestamp = first_string(object, &["timestamp", "time", "created_at", "date"]);
    let summary = first_string(object, &["summary", "title", "message", "event", "text"])
        .map(|value| value.chars().take(240).collect());
    let mut keys: Vec<String> = object.keys().cloned().collect();
    keys.sort();
    keys.truncate(12);
    JsonlItemSummary {
        timestamp,
        summary,
        keys,
        item_type: None,
    }
}

fn first_string(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .or_else(|| Some(value.to_string()))
        })
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn build_history_metadata(
    args: &ReportArgs,
    repo_slug: &str,
    report_date: &str,
) -> Result<HistoryMetadata, CliError> {
    if args.write && args.history_dir.is_none() {
        return Err(CliError::usage(
            "history-dir-required",
            "--write requires --history-dir <dir>",
            None,
        ));
    }
    let Some(history_dir) = args.history_dir.as_ref().map(|path| expand_user(path)) else {
        return Ok(HistoryMetadata {
            enabled: false,
            write: false,
            history_dir: None,
            intended: None,
            written: Vec::new(),
            comparison: HistoryComparison {
                status: "not_requested".to_string(),
                prior_schema: None,
                prior_window: None,
                prior_commit_count: None,
                commit_count_delta: None,
            },
        });
    };
    let intended = intended_history_paths(&history_dir, repo_slug, report_date);
    let comparison = if args.write {
        load_prior_report(&history_dir, Path::new(&intended.json))
    } else {
        HistoryComparison {
            status: "not_written".to_string(),
            prior_schema: None,
            prior_window: None,
            prior_commit_count: None,
            commit_count_delta: None,
        }
    };
    let written = if args.write {
        vec![
            intended.markdown.clone(),
            intended.json.clone(),
            intended.index.clone(),
        ]
    } else {
        Vec::new()
    };
    Ok(HistoryMetadata {
        enabled: true,
        write: args.write,
        history_dir: Some(history_dir.display().to_string()),
        intended: Some(intended),
        written,
        comparison,
    })
}

fn intended_history_paths(history_dir: &Path, repo_slug: &str, report_date: &str) -> HistoryPaths {
    let year = report_date.chars().take(4).collect::<String>();
    let filename = format!("{report_date}-{repo_slug}-repo-retro");
    HistoryPaths {
        markdown: history_dir
            .join("retros")
            .join(&year)
            .join(format!("{filename}.md"))
            .display()
            .to_string(),
        json: history_dir
            .join("raw")
            .join(&year)
            .join(format!("{filename}.json"))
            .display()
            .to_string(),
        index: history_dir.join("index.jsonl").display().to_string(),
    }
}

fn load_prior_report(history_dir: &Path, current_json_path: &Path) -> HistoryComparison {
    let index = history_dir.join("index.jsonl");
    if !index.is_file() {
        return HistoryComparison {
            status: "no_prior_report".to_string(),
            prior_schema: None,
            prior_window: None,
            prior_commit_count: None,
            commit_count_delta: None,
        };
    }
    let Ok(body) = fs::read_to_string(index) else {
        return HistoryComparison {
            status: "no_prior_report".to_string(),
            prior_schema: None,
            prior_window: None,
            prior_commit_count: None,
            commit_count_delta: None,
        };
    };
    for line in body.lines().rev() {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(raw_path) = row.get("rawPath").and_then(Value::as_str) else {
            continue;
        };
        let candidate = if Path::new(raw_path).is_absolute() {
            PathBuf::from(raw_path)
        } else {
            history_dir.join(raw_path)
        };
        if candidate == current_json_path || !candidate.is_file() {
            continue;
        }
        let Ok(raw) = fs::read_to_string(candidate) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        return compare_prior_report(&value);
    }
    HistoryComparison {
        status: "no_prior_report".to_string(),
        prior_schema: None,
        prior_window: None,
        prior_commit_count: None,
        commit_count_delta: None,
    }
}

fn compare_prior_report(value: &Value) -> HistoryComparison {
    let report = value.get("result").unwrap_or(value);
    let prior_schema = report
        .get("schema")
        .or_else(|| report.get("schema_version"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let prior_window = report.get("window").cloned();
    let prior_commit_count = report
        .get("git")
        .and_then(|git| git.get("summary"))
        .and_then(|summary| summary.get("commitCount"))
        .and_then(Value::as_i64);
    HistoryComparison {
        status: "compared".to_string(),
        prior_schema,
        prior_window,
        prior_commit_count,
        commit_count_delta: None,
    }
}

fn write_history(report: &RepoRetroReport) -> Result<(), CliError> {
    if !report.history.write {
        return Ok(());
    }
    let Some(paths) = &report.history.intended else {
        return Ok(());
    };
    let markdown_path = Path::new(&paths.markdown);
    let json_path = Path::new(&paths.json);
    let index_path = Path::new(&paths.index);
    create_parent(markdown_path)?;
    create_parent(json_path)?;
    create_parent(index_path)?;
    fs::write(markdown_path, render_markdown(report)).map_err(|err| {
        CliError::runtime(
            "history-write-failed",
            format!("failed to write {}: {err}", markdown_path.display()),
            Some(json!({ "path": display_path(markdown_path) })),
        )
    })?;
    fs::write(
        json_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(report).expect("report should serialize")
        ),
    )
    .map_err(|err| {
        CliError::runtime(
            "history-write-failed",
            format!("failed to write {}: {err}", json_path.display()),
            Some(json!({ "path": display_path(json_path) })),
        )
    })?;
    let history_dir = report
        .history
        .history_dir
        .as_deref()
        .map(Path::new)
        .unwrap_or_else(|| Path::new("."));
    let row = json!({
        "schema": INDEX_SCHEMA_VERSION,
        "generatedAt": report.generated_at,
        "repoSlug": report.repo.slug,
        "mode": report.mode,
        "window": report.window,
        "markdownPath": relative_or_display(markdown_path, history_dir),
        "rawPath": relative_or_display(json_path, history_dir),
        "commitCount": report.git.summary.commit_count,
        "testSignals": report.git.test_signals,
        "heuristicSystem": {
            "state": report.heuristic_system.state,
            "activeInbox": report.heuristic_system.active_inbox,
            "errorInboxMovement": report.heuristic_system.error_inbox_movement,
        },
        "warnings": report.warnings,
    });
    let mut row_text = serde_json::to_string(&row).expect("index row should serialize");
    row_text.push('\n');
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(index_path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, row_text.as_bytes()))
        .map_err(|err| {
            CliError::runtime(
                "history-index-write-failed",
                format!("failed to append {}: {err}", index_path.display()),
                Some(json!({ "path": display_path(index_path) })),
            )
        })
}

fn create_parent(path: &Path) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            CliError::runtime(
                "history-dir-create-failed",
                format!("failed to create {}: {err}", parent.display()),
                Some(json!({ "path": display_path(parent) })),
            )
        })?;
    }
    Ok(())
}

fn relative_or_display(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn build_analysis(
    mode: ReviewMode,
    git: &GitReport,
    heuristic: &HeuristicSystemReport,
    optional_inputs: &BTreeMap<String, JsonlSummary>,
    warnings: &[String],
    history: &HistoryMetadata,
) -> Analysis {
    let mut themes = Vec::new();
    if git.summary.commit_count == 0 {
        themes.push("No local commits landed in the selected window.".to_string());
    }
    for kind in ["feat", "fix", "refactor", "docs", "test"] {
        if let Some(count) = git.commit_types.get(kind)
            && *count > 0
        {
            themes.push(format!("{kind} work appeared in {count} commit(s)."));
        }
    }
    // Lead with real engineering churn from the class split; report process-doc
    // movement separately and flag when it is mostly archival, so a doc-heavy
    // window can no longer crowd the headline.
    let churn = &git.churn_by_class;
    let source_lines = churn.source.changed_lines + churn.tests.changed_lines;
    if source_lines > 0 {
        themes.push(format!(
            "Source and test churn was {} changed line(s) across {} file(s).",
            source_lines,
            churn.source.file_count + churn.tests.file_count
        ));
    }
    let process_lines = churn.process_artifacts.changed_lines;
    if process_lines > 0 {
        let archived = git.archival.process_artifacts_deleted_lines;
        if archived * 2 >= process_lines {
            themes.push(format!(
                "Process-doc churn was {process_lines} changed line(s), mostly plan/discussion archival ({archived} deleted)."
            ));
        } else {
            themes.push(format!(
                "Process-doc (plan/discussion) churn was {process_lines} changed line(s)."
            ));
        }
    }
    if themes.is_empty() {
        themes.push(format!(
            "{} review has only light local source movement.",
            mode.title()
        ));
    }

    let mut attention_items = Vec::new();
    if let Some(ratio) = git.test_signals.test_loc_ratio
        && git.summary.changed_lines > 0
        && ratio < 0.05
    {
        attention_items
            .push("Test-surface movement is low relative to total changed lines.".to_string());
    }
    let high_severity_count = high_severity_count(&heuristic.active_inbox.by_severity);
    if high_severity_count > 0 {
        attention_items.push(format!(
            "HEURISTIC_SYSTEM has {high_severity_count} high/critical active inbox item(s)."
        ));
    }
    if !warnings.is_empty() {
        attention_items.push(format!(
            "{} warning(s) should be reviewed before sharing.",
            warnings.len()
        ));
    }
    if attention_items.is_empty() {
        attention_items.push(
            "No obvious local attention item was derived from deterministic signals.".to_string(),
        );
    }

    let mut validation_signals = Vec::new();
    if git.test_signals.test_related_commit_count > 0 {
        validation_signals.push(format!(
            "{} test-related commit(s), {} changed test file(s).",
            git.test_signals.test_related_commit_count, git.test_signals.changed_test_file_count
        ));
    }
    if let Some(summary) = optional_inputs.get("validation")
        && summary.valid_lines > 0
    {
        validation_signals.push(format!(
            "{} explicit validation input line(s) were supplied.",
            summary.valid_lines
        ));
    }
    if validation_signals.is_empty() {
        validation_signals.push(
            "No explicit validation artifact was supplied beyond local git signals.".to_string(),
        );
    }

    let mut follow_up_questions = Vec::new();
    if let Some(file) = git
        .file_hotspots
        .top_files
        .iter()
        .find(|file| !file.net_deleted)
    {
        follow_up_questions.push(format!(
            "Does `{}` need focused review? It was touched in {} commit(s) this window.",
            file.path, file.commits
        ));
    }
    if git.archival.net_deleted_file_count > 0 {
        follow_up_questions.push(format!(
            "{} file(s) were net-deleted this window (likely archival); confirm none needs migration rather than removal.",
            git.archival.net_deleted_file_count
        ));
    }
    if heuristic.active_inbox.total > 0 {
        follow_up_questions.push(
            "Which active HEURISTIC_SYSTEM inbox items should be closed, promoted, or retained next?".to_string(),
        );
    }
    if mode == ReviewMode::Maintainer {
        follow_up_questions.push(
            "Is the current change mix ready for release notes or should it stay unreleased?"
                .to_string(),
        );
    }
    if follow_up_questions.is_empty() {
        follow_up_questions
            .push("What outcome should be carried into the next selected window?".to_string());
    }

    let movement = &heuristic.error_inbox_movement;
    let aging_summary = match heuristic.aging.oldest_open_days {
        Some(days) => format!("oldest open retained item is {days} day(s) old"),
        None => "no aged open retained item detected".to_string(),
    };
    Analysis {
        themes,
        attention_items,
        follow_up_questions,
        validation_signals,
        heuristic_system_review: HeuristicSystemReview {
            boundary: heuristic.boundary.clone(),
            active_inbox_total: heuristic.active_inbox.total,
            high_severity_count,
            movement_summary: format!(
                "added {}, modified {}, archived {}, removed {}",
                movement.added.count,
                movement.modified.count,
                movement.archived.count,
                movement.removed.count
            ),
            aging_summary,
        },
        history_comparison: history.comparison.clone(),
    }
}

fn high_severity_count(by_severity: &BTreeMap<String, usize>) -> usize {
    by_severity
        .iter()
        .filter(|(severity, _)| {
            let severity = severity.to_lowercase();
            severity == "high" || severity == "critical"
        })
        .map(|(_, count)| *count)
        .sum()
}

fn format_bullet_block<'a, I>(items: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    let mut iter = items.into_iter().peekable();
    if iter.peek().is_none() {
        return String::new();
    }
    let mut text = iter
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    text
}

fn render_markdown(report: &RepoRetroReport) -> String {
    let git = &report.git;
    let heuristic = &report.heuristic_system;

    // Each `*_block` ends with a trailing `\n` when non-empty so the
    // template can put the next `## Heading` directly on the next line
    // and still produce one blank line between the block and the
    // heading. Empty blocks render as the empty string and the
    // template-side `\n` becomes the single blank line between
    // adjacent headings (matching the pre-migration `format!` output's
    // double-`String::new()`-around-empty-loop quirk).
    let themes_block = format_bullet_block(report.analysis.themes.iter().map(String::as_str));
    let attention_items_block =
        format_bullet_block(report.analysis.attention_items.iter().map(String::as_str));
    let hotspots_block = if git.file_hotspots.top_files.is_empty() {
        "- No changed files in the selected window.\n".to_string()
    } else {
        let mut text = git
            .file_hotspots
            .top_files
            .iter()
            .take(5)
            .map(|item| {
                let marker = if item.net_deleted {
                    " (net-deleted)"
                } else {
                    ""
                };
                format!(
                    "- `{}` [{}]: {} commit(s), {} changed lines{}",
                    item.path,
                    path_class_label(item.class),
                    item.commits,
                    item.changed_lines,
                    marker
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        text.push('\n');
        text
    };
    let churn_by_class_block = {
        let churn = &git.churn_by_class;
        let rows = [
            ("source", &churn.source),
            ("tests", &churn.tests),
            ("product-docs", &churn.product_docs),
            ("process-artifacts", &churn.process_artifacts),
            ("other", &churn.other),
        ];
        let lines: Vec<String> = rows
            .iter()
            .filter(|(_, item)| item.changed_lines > 0)
            .map(|(label, item)| {
                format!(
                    "- {}: {} changed lines across {} file(s), {} commit(s)",
                    label, item.changed_lines, item.file_count, item.commits
                )
            })
            .collect();
        if lines.is_empty() {
            "- No classified changes in the selected window.\n".to_string()
        } else {
            let mut text = lines.join("\n");
            text.push('\n');
            text
        }
    };
    let validation_signals_block = format_bullet_block(
        report
            .analysis
            .validation_signals
            .iter()
            .map(String::as_str),
    );
    let follow_up_questions_block = format_bullet_block(
        report
            .analysis
            .follow_up_questions
            .iter()
            .map(String::as_str),
    );
    let warnings_block = format_bullet_block(report.warnings.iter().map(String::as_str));

    let view = RepoRetroView {
        repo_name: &report.repo.name,
        generated_at: &report.generated_at,
        mode: &report.mode,
        window_label: &report.window.label,
        window_start: &report.window.start,
        window_end: &report.window.end,
        repo_root: &report.repo.root,
        commit_count: git.summary.commit_count,
        changed_lines: git.summary.changed_lines,
        insertions: git.summary.insertions,
        deletions: git.summary.deletions,
        active_days_count: git.summary.active_days.len(),
        test_related_commits: git.test_signals.test_related_commit_count,
        churn_by_class_block,
        themes_block,
        attention_items_block,
        hotspots_block,
        validation_signals_block,
        heuristic_state: &heuristic.state,
        heuristic_active_inbox_total: heuristic.active_inbox.total,
        heuristic_movement_summary: &report.analysis.heuristic_system_review.movement_summary,
        heuristic_op_records_changed: heuristic.operation_records.changed_count,
        heuristic_aging_summary: &report.analysis.heuristic_system_review.aging_summary,
        follow_up_questions_block,
        show_warnings: !report.warnings.is_empty(),
        warnings_block,
    };

    let mut engine = Engine::builder().build();
    engine
        .register_template(REPO_RETRO_TEMPLATE_NAME, REPO_RETRO_TEMPLATE)
        .expect("repo_retro template registers");
    engine
        .render(REPO_RETRO_TEMPLATE_NAME, &view)
        .expect("repo_retro template renders")
}

fn expand_user(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    if value == "~" {
        return home_dir().unwrap_or_else(|| path.to_path_buf());
    }
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn slugify(value: &str) -> String {
    static SLUG_RE: OnceLock<Regex> = OnceLock::new();
    let re = SLUG_RE.get_or_init(|| Regex::new(r"[^a-z0-9]+").expect("regex"));
    let slug = re
        .replace_all(&value.to_lowercase(), "-")
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        "repo".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("golden")
            .join("repo_retro")
            .join(name)
    }

    fn assert_or_bless(name: &str, actual: &str) {
        let path = fixture_path(name);
        if std::env::var_os("BLESS_REPO_RETRO_GOLDEN").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir fixture dir");
            std::fs::write(&path, actual).expect("write fixture");
            return;
        }
        let expected = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()));
        pretty_assertions::assert_eq!(expected, actual, "golden mismatch for {name}");
    }

    fn sample_report() -> RepoRetroReport {
        RepoRetroReport {
            schema: REPORT_SCHEMA_VERSION.to_string(),
            mode: "git+heuristic".to_string(),
            generated_at: "2026-05-26T07:00:00Z".to_string(),
            repo: RepoIdentity {
                root: "/tmp/sample".to_string(),
                name: "sample".to_string(),
                slug: "sample".to_string(),
                branch: "main".to_string(),
                head: "abc1234".to_string(),
                remote: None,
            },
            window: Window {
                mode: "explicit".to_string(),
                label: "last 5 days".to_string(),
                start: "2026-05-21".to_string(),
                end: "2026-05-26".to_string(),
                days: 5,
            },
            git: GitReport {
                summary: CommitSummary {
                    commit_count: 12,
                    active_days: vec!["2026-05-22".to_string(), "2026-05-24".to_string()],
                    first_commit_date: Some("2026-05-22".to_string()),
                    last_commit_date: Some("2026-05-24".to_string()),
                    insertions: 240,
                    deletions: 80,
                    changed_lines: 320,
                },
                commit_types: BTreeMap::new(),
                authors: vec![],
                churn_by_class: ChurnByClass {
                    source: ClassChurn {
                        file_count: 1,
                        commits: 4,
                        insertions: 100,
                        deletions: 20,
                        changed_lines: 120,
                    },
                    tests: ClassChurn {
                        file_count: 0,
                        commits: 0,
                        insertions: 0,
                        deletions: 0,
                        changed_lines: 0,
                    },
                    product_docs: ClassChurn {
                        file_count: 0,
                        commits: 0,
                        insertions: 0,
                        deletions: 0,
                        changed_lines: 0,
                    },
                    process_artifacts: ClassChurn {
                        file_count: 0,
                        commits: 0,
                        insertions: 0,
                        deletions: 0,
                        changed_lines: 0,
                    },
                    other: ClassChurn {
                        file_count: 0,
                        commits: 0,
                        insertions: 0,
                        deletions: 0,
                        changed_lines: 0,
                    },
                },
                archival: Archival {
                    net_deleted_file_count: 0,
                    net_deleted_files: vec![],
                    process_artifacts_deleted_lines: 0,
                    plans_archived_estimate: 0,
                },
                file_hotspots: FileHotspots {
                    top_files: vec![FileChangeSummary {
                        path: "src/main.rs".to_string(),
                        commits: 4,
                        insertions: 100,
                        deletions: 20,
                        changed_lines: 120,
                        class: PathClass::Source,
                        net_deleted: false,
                    }],
                    top_areas: vec![],
                },
                test_signals: TestSignals {
                    changed_test_files: vec![],
                    changed_test_file_count: 0,
                    test_related_commit_count: 3,
                    test_changed_lines: 40,
                    test_loc_ratio: None,
                },
                recent_commits: vec![],
            },
            heuristic_system: HeuristicSystemReport {
                state: "stable".to_string(),
                active_inbox: ActiveInboxSummary {
                    state: "stable".to_string(),
                    total: 2,
                    by_status: BTreeMap::new(),
                    by_severity: BTreeMap::new(),
                    entries: vec![],
                },
                error_inbox_movement: ErrorInboxMovement {
                    added: MovementBucket {
                        count: 0,
                        paths: vec![],
                    },
                    modified: MovementBucket {
                        count: 0,
                        paths: vec![],
                    },
                    archived: MovementBucket {
                        count: 0,
                        paths: vec![],
                    },
                    removed: MovementBucket {
                        count: 0,
                        paths: vec![],
                    },
                },
                operation_records: OperationRecords {
                    changed_count: 1,
                    by_status: BTreeMap::new(),
                    paths: vec![],
                },
                aging: HeuristicAging {
                    oldest_open_days: None,
                    entries_over_30_days: vec![],
                },
                boundary: "local".to_string(),
            },
            optional_inputs: BTreeMap::new(),
            history: HistoryMetadata {
                enabled: false,
                write: false,
                history_dir: None,
                intended: None,
                written: vec![],
                comparison: HistoryComparison {
                    status: "absent".to_string(),
                    prior_schema: None,
                    prior_window: None,
                    prior_commit_count: None,
                    commit_count_delta: None,
                },
            },
            analysis: Analysis {
                themes: vec!["Refactor: extracted helper".to_string()],
                attention_items: vec!["No tests added for the new helper".to_string()],
                follow_up_questions: vec!["Should we add an integration test?".to_string()],
                validation_signals: vec!["cargo test -p sample passes".to_string()],
                heuristic_system_review: HeuristicSystemReview {
                    boundary: "local".to_string(),
                    active_inbox_total: 2,
                    high_severity_count: 0,
                    movement_summary: "no movement in window".to_string(),
                    aging_summary: "no entries over 30 days".to_string(),
                },
                history_comparison: HistoryComparison {
                    status: "absent".to_string(),
                    prior_schema: None,
                    prior_window: None,
                    prior_commit_count: None,
                    commit_count_delta: None,
                },
            },
            warnings: vec![],
            sources: Sources { commands: vec![] },
        }
    }

    #[test]
    fn render_markdown_matches_golden_sample() {
        let report = sample_report();
        let out = render_markdown(&report);
        assert_or_bless("sample.md", &out);
    }

    #[test]
    fn render_markdown_matches_golden_with_warnings() {
        let mut report = sample_report();
        report.warnings = vec![
            "test-fixture missing".to_string(),
            "config file outdated".to_string(),
        ];
        let out = render_markdown(&report);
        assert_or_bless("with_warnings.md", &out);
    }

    fn commit(hash: &str, subject: &str, files: &[(&str, i64, i64)]) -> Commit {
        Commit {
            hash: hash.to_string(),
            date: "2026-05-30".to_string(),
            author: "dev".to_string(),
            email: "dev@example.com".to_string(),
            subject: subject.to_string(),
            files: files
                .iter()
                .map(|(path, insertions, deletions)| ChangedFile {
                    path: (*path).to_string(),
                    insertions: *insertions,
                    deletions: *deletions,
                })
                .collect(),
        }
    }

    fn empty_heuristic() -> HeuristicSystemReport {
        HeuristicSystemReport {
            state: "not_present".to_string(),
            active_inbox: empty_active_inbox(),
            error_inbox_movement: ErrorInboxMovement {
                added: MovementBucket {
                    count: 0,
                    paths: vec![],
                },
                modified: MovementBucket {
                    count: 0,
                    paths: vec![],
                },
                archived: MovementBucket {
                    count: 0,
                    paths: vec![],
                },
                removed: MovementBucket {
                    count: 0,
                    paths: vec![],
                },
            },
            operation_records: OperationRecords {
                changed_count: 0,
                by_status: BTreeMap::new(),
                paths: vec![],
            },
            aging: HeuristicAging {
                oldest_open_days: None,
                entries_over_30_days: vec![],
            },
            boundary: "read_only".to_string(),
        }
    }

    fn empty_history() -> HistoryMetadata {
        HistoryMetadata {
            enabled: false,
            write: false,
            history_dir: None,
            intended: None,
            written: vec![],
            comparison: HistoryComparison {
                status: "not_requested".to_string(),
                prior_schema: None,
                prior_window: None,
                prior_commit_count: None,
                commit_count_delta: None,
            },
        }
    }

    #[test]
    fn classify_path_uses_built_in_defaults() {
        let classifier = PathClassifier::default();
        assert_eq!(
            classifier.classify("docs/plans/x/x-plan.md"),
            PathClass::ProcessArtifacts
        );
        assert_eq!(
            classifier.classify("docs/discussions/y.md"),
            PathClass::ProcessArtifacts
        );
        assert_eq!(
            classifier.classify("heuristic-system/error-inbox/z/ENTRY.md"),
            PathClass::ProcessArtifacts
        );
        assert_eq!(
            classifier.classify("crates/x/src/lib.rs"),
            PathClass::Source
        );
        assert_eq!(
            classifier.classify("crates/x/tests/foo.rs"),
            PathClass::Tests
        );
        assert_eq!(classifier.classify("README.md"), PathClass::ProductDocs);
        assert_eq!(
            classifier.classify("docs/specs/contract.md"),
            PathClass::ProductDocs
        );
        assert_eq!(classifier.classify("scripts/ci/run.sh"), PathClass::Source);
        // Generated Markdown fixtures under a `tests/` tree are tests, not
        // product-docs — checked before the `.md` rule, without re-sweeping
        // `docs/specs` product docs above.
        assert_eq!(
            classifier.classify(
                "tests/golden/claude/plugins/reporting/skills/project-retro/expected/SKILL.md"
            ),
            PathClass::Tests
        );
        assert_eq!(
            classifier.classify("crates/x/tests/golden/sample.md"),
            PathClass::Tests
        );
        assert_eq!(
            classifier.classify("tests/runtime-smoke/cases/meta/run.sh"),
            PathClass::Tests
        );
    }

    #[test]
    fn path_class_config_override_takes_precedence() {
        let classifier = PathClassifier {
            overrides: vec![(PathClass::Source, "docs/specs/".to_string())],
        };
        // docs/specs is productDocs by default; the override reclassifies it.
        assert_eq!(
            classifier.classify("docs/specs/contract.md"),
            PathClass::Source
        );
        // unrelated paths still follow the defaults.
        assert_eq!(
            classifier.classify("docs/plans/x.md"),
            PathClass::ProcessArtifacts
        );
    }

    #[test]
    fn churn_by_class_reconciles_to_summary_total() {
        let commits = vec![
            commit("a1", "feat: x", &[("crates/x/src/lib.rs", 30, 5)]),
            commit("a2", "test: x", &[("crates/x/tests/x.rs", 10, 0)]),
            commit(
                "a3",
                "docs(plans): archive y",
                &[("docs/plans/y/y-plan.md", 0, 200)],
            ),
            commit("a4", "docs: readme", &[("README.md", 4, 1)]),
        ];
        let git = summarize_commits(&commits, &PathClassifier::default());
        let churn = &git.churn_by_class;
        let class_total = churn.source.changed_lines
            + churn.tests.changed_lines
            + churn.product_docs.changed_lines
            + churn.process_artifacts.changed_lines
            + churn.other.changed_lines;
        assert_eq!(class_total, git.summary.changed_lines);
        assert_eq!(churn.source.changed_lines, 35);
        assert_eq!(churn.tests.changed_lines, 10);
        assert_eq!(churn.product_docs.changed_lines, 5);
        assert_eq!(churn.process_artifacts.changed_lines, 200);
    }

    #[test]
    fn hotspots_rank_by_commit_count_and_carry_class_and_net_deleted() {
        let commits = vec![
            commit("c1", "feat", &[("src/iterated.rs", 2, 1)]),
            commit(
                "c2",
                "fix",
                &[("src/iterated.rs", 1, 1), ("docs/plans/p-plan.md", 0, 500)],
            ),
            commit("c3", "refactor", &[("src/iterated.rs", 1, 0)]),
            commit("c4", "feat", &[("src/bulk.rs", 300, 0)]),
        ];
        let git = summarize_commits(&commits, &PathClassifier::default());
        let top = &git.file_hotspots.top_files;
        // 3 commits beats the once-written bulk file's 300 lines.
        assert_eq!(top[0].path, "src/iterated.rs");
        assert_eq!(top[0].commits, 3);
        assert_eq!(top[0].class, PathClass::Source);
        assert!(!top[0].net_deleted);
        let plan = top
            .iter()
            .find(|file| file.path == "docs/plans/p-plan.md")
            .expect("archived plan present in hotspots");
        assert!(plan.net_deleted);
        assert_eq!(plan.class, PathClass::ProcessArtifacts);
    }

    #[test]
    fn net_deletion_drives_archival_facts() {
        let commits = vec![
            commit(
                "d1",
                "docs(plans): archive old",
                &[("docs/plans/old/old-plan.md", 0, 120)],
            ),
            commit("d2", "feat: keep", &[("src/keep.rs", 10, 3)]),
        ];
        let git = summarize_commits(&commits, &PathClassifier::default());
        assert_eq!(git.archival.net_deleted_file_count, 1);
        assert_eq!(
            git.archival.net_deleted_files[0].path,
            "docs/plans/old/old-plan.md"
        );
        assert_eq!(git.archival.process_artifacts_deleted_lines, 120);
        assert_eq!(git.archival.plans_archived_estimate, 1);
        // a file with insertions is not net-deleted.
        assert!(
            !git.file_hotspots
                .top_files
                .iter()
                .any(|file| file.path == "src/keep.rs" && file.net_deleted)
        );
    }

    #[test]
    fn analysis_skips_net_deleted_files_and_splits_churn_by_class() {
        let commits = vec![
            commit(
                "e1",
                "docs(plans): archive a",
                &[("docs/plans/a/a-plan.md", 0, 700)],
            ),
            commit("e2", "feat: real", &[("src/real.rs", 5, 1)]),
        ];
        let git = summarize_commits(&commits, &PathClassifier::default());
        let analysis = build_analysis(
            ReviewMode::Team,
            &git,
            &empty_heuristic(),
            &BTreeMap::new(),
            &[],
            &empty_history(),
        );
        // AC1: the archived plan (net-deleted and largest by lines) is never
        // nominated for review; only the real source file is.
        assert!(
            analysis
                .follow_up_questions
                .iter()
                .all(|question| !question.contains("a-plan.md"))
        );
        assert!(
            analysis
                .follow_up_questions
                .iter()
                .any(|question| question.contains("src/real.rs"))
        );
        assert!(
            analysis
                .follow_up_questions
                .iter()
                .any(|question| question.contains("net-deleted"))
        );
        // AC4: themes carry a class-aware split, not a bare docs-largest line.
        assert!(
            analysis
                .themes
                .iter()
                .any(|theme| theme.contains("Source and test churn"))
        );
        assert!(
            analysis
                .themes
                .iter()
                .any(|theme| theme.contains("archival"))
        );
        assert!(
            analysis
                .themes
                .iter()
                .all(|theme| !theme.contains("largest code/doc movement"))
        );
    }
}

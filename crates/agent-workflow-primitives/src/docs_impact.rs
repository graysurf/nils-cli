use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::common::{
    CliError, OutputFormat, absolute_path, display_path, render_error, render_success,
};
use crate::completion::{self, CompletionShell};

const SCAN_SCHEMA_VERSION: &str = "cli.docs-impact.scan.v1";
const SCAN_COMMAND: &str = "docs-impact scan";
const RECORD_SCHEMA: &str = "docs-impact.record.v1";
const RECORD_FILE: &str = "docs-impact.record.json";
const RECORD_SCHEMA_VERSION: &str = "cli.docs-impact.record.v1";
const SHOW_SCHEMA_VERSION: &str = "cli.docs-impact.show.v1";
const VERIFY_SCHEMA_VERSION: &str = "cli.docs-impact.verify.v1";

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
        Err(err) => return crate::common::handle_parse_error("docs-impact", argv, err),
    };

    match cli.command {
        Command::Scan(args) => run_scan(args),
        Command::Record(args) => run_record(args),
        Command::Show(args) => run_show(args),
        Command::Verify(args) => run_verify(args),
        Command::Completion(args) => completion::run::<Cli>(args.shell, "docs-impact"),
    }
}

fn run_record(args: RecordArgs) -> i32 {
    let format = args.common.format;
    match record(&args) {
        Ok(result) => render_success(
            RECORD_SCHEMA_VERSION,
            "docs-impact record",
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(RECORD_SCHEMA_VERSION, "docs-impact record", format, err),
    }
}

fn run_show(args: OutArgs) -> i32 {
    let format = args.format;
    match read_record(&args.out_dir) {
        Ok(record) => render_success(
            SHOW_SCHEMA_VERSION,
            "docs-impact show",
            format,
            || format!("docs-impact: disposition={}", record.disposition),
            &record,
        ),
        Err(err) => render_error(SHOW_SCHEMA_VERSION, "docs-impact show", format, err),
    }
}

fn run_verify(args: VerifyArgs) -> i32 {
    let format = args.common.format;
    match verify_record(&args) {
        Ok(result) if result.verified => render_success(
            VERIFY_SCHEMA_VERSION,
            "docs-impact verify",
            format,
            || "docs-impact: verified=true".to_string(),
            &result,
        ),
        Ok(result) => render_error(
            VERIFY_SCHEMA_VERSION,
            "docs-impact verify",
            format,
            CliError::data(
                result.reason_code.clone(),
                "docs-impact record is not current and complete",
                Some(json!({
                    "reason_code": result.reason_code,
                    "disposition": result.record.disposition,
                })),
            ),
        ),
        Err(err) => render_error(VERIFY_SCHEMA_VERSION, "docs-impact verify", format, err),
    }
}

fn record(args: &RecordArgs) -> Result<RecordResult, CliError> {
    let repo = absolute_path(&args.repo)?;
    let snapshot = durable_snapshot(&repo, args.base.as_deref())?;
    validate_disposition(
        args.disposition.as_str(),
        args.rationale.as_deref(),
        &snapshot,
    )?;
    let record = DocsImpactRecord {
        schema: RECORD_SCHEMA.to_string(),
        base: args.base.clone(),
        base_revision: snapshot.base_revision,
        head_revision: snapshot.head_revision,
        changed_path_digest: snapshot.changed_path_digest,
        change_fingerprint: snapshot.change_fingerprint,
        paths: snapshot.paths,
        path_classes: snapshot.path_classes,
        disposition: args.disposition.as_str().to_string(),
        rationale: args.rationale.as_deref().map(crate::common::redact_text),
        recorded_at: jiff::Timestamp::now().to_string(),
        producer: Producer {
            tool: "docs-impact".to_string(),
            nils_cli_version: env!("CARGO_PKG_VERSION").to_string(),
        },
    };
    let path = record_file(&args.common.out_dir)?;
    write_record(&path, &record)?;
    Ok(RecordResult {
        record_file: display_path(&path),
        record,
    })
}

fn verify_record(args: &VerifyArgs) -> Result<VerifyResult, CliError> {
    let record = read_record(&args.common.out_dir)?;
    let repo = absolute_path(&args.repo)?;
    let snapshot = durable_snapshot(&repo, record.base.as_deref())?;
    let stale = snapshot.base_revision != record.base_revision
        || snapshot.head_revision != record.head_revision
        || snapshot.changed_path_digest != record.changed_path_digest
        || snapshot.change_fingerprint != record.change_fingerprint;
    let reason_code = if stale {
        "stale-scan".to_string()
    } else if record.disposition == "pending" {
        "pending-disposition".to_string()
    } else if let Err(err) =
        validate_disposition(&record.disposition, record.rationale.as_deref(), &snapshot)
    {
        err.code().to_string()
    } else {
        "verified".to_string()
    };
    Ok(VerifyResult {
        verified: reason_code == "verified",
        reason_code,
        record,
    })
}

fn validate_disposition(
    disposition: &str,
    rationale: Option<&str>,
    snapshot: &DurableSnapshot,
) -> Result<(), CliError> {
    match disposition {
        "pending" => Ok(()),
        "docs-updated" if snapshot.docs_changed => Ok(()),
        "docs-updated" => Err(CliError::data(
            "docs-update-missing",
            "docs-updated requires at least one documentation change",
            None,
        )),
        "no-docs-needed"
            if !snapshot.non_docs_changed
                || rationale.is_some_and(|value| !value.trim().is_empty()) =>
        {
            Ok(())
        }
        "no-docs-needed" => Err(CliError::data(
            "rationale-required",
            "no-docs-needed requires --rationale when non-doc files changed",
            None,
        )),
        _ => Err(CliError::data(
            "invalid-disposition",
            format!("unsupported disposition `{disposition}`"),
            None,
        )),
    }
}

fn durable_snapshot(repo: &Path, base: Option<&str>) -> Result<DurableSnapshot, CliError> {
    ensure_git_repo(repo)?;
    let head_revision = git_one(repo, &["rev-parse", "HEAD"])?;
    let base_revision = match base {
        Some(base) => git_one(repo, &["rev-parse", base])?,
        None => head_revision.clone(),
    };
    let mut changed = BTreeSet::new();
    if let Some(base) = base {
        let range = format!("{base}...HEAD");
        changed.extend(git_lines(
            repo,
            &["diff", "--name-only", "--diff-filter=ACMRTUXBD", &range],
        )?);
    }
    for argv in [
        ["diff", "--name-only", "--diff-filter=ACMRTUXBD"].as_slice(),
        ["diff", "--cached", "--name-only", "--diff-filter=ACMRTUXBD"].as_slice(),
        ["ls-files", "--others", "--exclude-standard"].as_slice(),
    ] {
        changed.extend(git_lines(repo, argv)?);
    }
    let paths: Vec<String> = changed.into_iter().collect();
    let changed_path_digest = agent_docs::path_classes::changed_path_digest(paths.clone());
    let mut hasher = Sha256::new();
    hasher.update(base_revision.as_bytes());
    hasher.update(head_revision.as_bytes());
    let catalog = agent_docs::config::load_catalog(repo, repo)
        .map_err(|err| CliError::data("path-class-catalog-invalid", err.to_string(), None))?;
    let contract = agent_docs::path_classes::project_contract(&catalog);
    let mut path_classes = Vec::new();
    let mut docs_changed = false;
    let mut non_docs_changed = false;
    for path in &paths {
        hasher.update(path.as_bytes());
        let absolute = repo.join(path);
        match fs::read(&absolute) {
            Ok(bytes) => hasher.update(&bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => hasher.update(b"<deleted>"),
            Err(err) => {
                return Err(CliError::runtime(
                    "changed-file-read-failed",
                    err.to_string(),
                    None,
                ));
            }
        }
        let path_class = match contract {
            Some(contract) => {
                contract
                    .classify(Path::new(path))
                    .map_err(|message| CliError::data("path-class-failed", message, None))?
                    .path_class
            }
            None => {
                if is_docs_path(path) {
                    "docs".to_string()
                } else {
                    "not-configured".to_string()
                }
            }
        };
        docs_changed |= path_class == "docs" || is_docs_path(path);
        non_docs_changed |= path_class != "docs" && !is_docs_path(path);
        path_classes.push(PathClassEntry {
            path: path.clone(),
            path_class,
        });
    }
    Ok(DurableSnapshot {
        base_revision,
        head_revision,
        changed_path_digest,
        change_fingerprint: format!("sha256:{}", hex(&hasher.finalize())),
        paths,
        path_classes,
        docs_changed,
        non_docs_changed,
    })
}

fn git_one(repo: &Path, args: &[&str]) -> Result<String, CliError> {
    git_lines(repo, args)?.into_iter().next().ok_or_else(|| {
        CliError::runtime(
            "git-empty-output",
            format!("git {} returned no output", args.join(" ")),
            None,
        )
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn record_file(out: &Path) -> Result<PathBuf, CliError> {
    Ok(absolute_path(out)?.join(RECORD_FILE))
}

fn write_record(path: &Path, record: &DocsImpactRecord) -> Result<(), CliError> {
    let mut bytes = serde_json::to_vec_pretty(record)
        .map_err(|err| CliError::runtime("record-render-failed", err.to_string(), None))?;
    bytes.push(b'\n');
    nils_common::fs::write_atomic(path, &bytes, 0o600)
        .map_err(|err| CliError::runtime("record-write-failed", err.to_string(), None))
}

fn read_record(out: &Path) -> Result<DocsImpactRecord, CliError> {
    let path = record_file(out)?;
    let raw = fs::read_to_string(&path).map_err(|err| {
        CliError::runtime(
            "record-read-failed",
            format!("{}: {err}", path.display()),
            None,
        )
    })?;
    let record: DocsImpactRecord = serde_json::from_str(&raw)
        .map_err(|err| CliError::data("invalid-record", err.to_string(), None))?;
    if record.schema != RECORD_SCHEMA {
        return Err(CliError::data(
            "unsupported-record",
            format!("unsupported schema `{}`", record.schema),
            None,
        ));
    }
    Ok(record)
}

fn run_scan(args: ScanArgs) -> i32 {
    let format = args.format;
    match scan(&args) {
        Ok(result) => render_success(
            SCAN_SCHEMA_VERSION,
            SCAN_COMMAND,
            format,
            || result.text_summary(),
            &result,
        ),
        Err(err) => render_error(SCAN_SCHEMA_VERSION, SCAN_COMMAND, format, err),
    }
}

fn scan(args: &ScanArgs) -> Result<ScanResult, CliError> {
    let repo = absolute_path(&args.repo)?;
    ensure_git_repo(&repo)?;

    let mut changed = BTreeSet::new();
    if let Some(base) = args.base.as_deref() {
        let range = format!("{base}...HEAD");
        for path in git_lines(
            &repo,
            &["diff", "--name-only", "--diff-filter=ACMRTUXB", &range],
        )? {
            changed.insert(path);
        }
    } else {
        for argv in [
            ["diff", "--name-only", "--diff-filter=ACMRTUXB"].as_slice(),
            ["diff", "--cached", "--name-only", "--diff-filter=ACMRTUXB"].as_slice(),
        ] {
            for path in git_lines(&repo, argv)? {
                changed.insert(path);
            }
        }
    }

    if args.include_untracked {
        for path in git_lines(&repo, &["ls-files", "--others", "--exclude-standard"])? {
            changed.insert(path);
        }
    }

    let mut docs_files = Vec::new();
    let mut non_docs_files = Vec::new();
    for path in changed {
        if is_docs_path(&path) {
            docs_files.push(path);
        } else {
            non_docs_files.push(path);
        }
    }

    let mut suggested_review = Vec::new();
    if !non_docs_files.is_empty() && docs_files.is_empty() {
        suggested_review.push("non-doc changes detected without changed docs".to_string());
        suggested_review.push(
            "check README.md, docs/, runbooks, changelog, and skill docs for stale guidance"
                .to_string(),
        );
    }
    if !docs_files.is_empty() {
        suggested_review
            .push("docs changed; run markdown/docs validation before delivery".to_string());
    }

    Ok(ScanResult {
        repo: display_path(&repo),
        base: args.base.clone(),
        include_untracked: args.include_untracked,
        docs_changed: !docs_files.is_empty(),
        non_docs_changed: !non_docs_files.is_empty(),
        docs_files,
        non_docs_files,
        suggested_review,
    })
}

fn ensure_git_repo(repo: &Path) -> Result<(), CliError> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map_err(|err| {
            CliError::runtime("git-unavailable", format!("failed to run git: {err}"), None)
        })?;
    if !output.status.success() {
        return Err(CliError::runtime(
            "not-git-repo",
            format!("{} is not a Git worktree", repo.display()),
            Some(json!({ "repo": display_path(repo) })),
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
            CliError::runtime(
                "git-failed",
                format!("failed to run git {}: {err}", args.join(" ")),
                Some(json!({ "args": args })),
            )
        })?;
    if !output.status.success() {
        return Err(CliError::runtime(
            "git-failed",
            format!("git {} failed", args.join(" ")),
            Some(json!({
                "args": args,
                "stderr": String::from_utf8_lossy(&output.stderr).trim(),
            })),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn is_docs_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    if lower.starts_with("docs/") || lower.contains("/docs/") {
        return true;
    }
    let file_name = Path::new(&lower)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    matches!(
        file_name,
        "readme.md" | "agents.md" | "development.md" | "changelog.md" | "license" | "license.md"
    ) || matches!(
        Path::new(&lower)
            .extension()
            .and_then(|value| value.to_str()),
        Some("md" | "mdx" | "rst" | "adoc" | "txt")
    )
}

#[derive(Debug, Parser)]
#[command(
    name = "docs-impact",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Scan Git changes for documentation impact.",
    long_about = "Scan Git changes and classify whether implementation work requires documentation updates.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  docs-impact scan --include-untracked\n  docs-impact scan --repo . --base origin/main --format json\n  docs-impact completion zsh\n\nENVIRONMENT:\n  none\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
enum Command {
    /// Scan changed files and classify documentation impact.
    Scan(ScanArgs),
    /// Persist a human docs-impact disposition against the complete Git change state.
    Record(RecordArgs),
    /// Show a persisted docs-impact disposition.
    Show(OutArgs),
    /// Rescan and verify a persisted docs-impact disposition is current.
    Verify(VerifyArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
struct OutArgs {
    /// Directory containing the durable docs-impact record.
    #[arg(long = "out", value_name = "DIR", value_hint = ValueHint::DirPath)]
    out_dir: PathBuf,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Debug, Args)]
struct RecordArgs {
    #[command(flatten)]
    common: OutArgs,
    #[arg(long, value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
    repo: PathBuf,
    #[arg(long, value_name = "REF")]
    base: Option<String>,
    #[arg(long, value_enum)]
    disposition: Disposition,
    #[arg(long, value_name = "TEXT")]
    rationale: Option<String>,
}

#[derive(Debug, Args)]
struct VerifyArgs {
    #[command(flatten)]
    common: OutArgs,
    #[arg(long, value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
    repo: PathBuf,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum Disposition {
    DocsUpdated,
    NoDocsNeeded,
    Pending,
}

impl Disposition {
    fn as_str(self) -> &'static str {
        match self {
            Self::DocsUpdated => "docs-updated",
            Self::NoDocsNeeded => "no-docs-needed",
            Self::Pending => "pending",
        }
    }
}

#[derive(Debug, Args)]
struct ScanArgs {
    /// Git worktree to scan.
    #[arg(long, value_name = "DIR", default_value = ".", value_hint = ValueHint::DirPath)]
    repo: PathBuf,

    /// Base ref for committed branch comparison (`<base>...HEAD`).
    #[arg(long, value_name = "REF")]
    base: Option<String>,

    /// Include untracked files from `git ls-files --others --exclude-standard`.
    #[arg(long)]
    include_untracked: bool,

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

#[derive(Debug, Serialize)]
struct ScanResult {
    repo: String,
    base: Option<String>,
    include_untracked: bool,
    docs_changed: bool,
    non_docs_changed: bool,
    docs_files: Vec<String>,
    non_docs_files: Vec<String>,
    suggested_review: Vec<String>,
}

impl ScanResult {
    fn text_summary(&self) -> String {
        format!(
            "docs-impact: docs_changed={} non_docs_changed={} docs_files={} non_docs_files={}",
            self.docs_changed,
            self.non_docs_changed,
            self.docs_files.len(),
            self.non_docs_files.len()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DocsImpactRecord {
    schema: String,
    base: Option<String>,
    base_revision: String,
    head_revision: String,
    changed_path_digest: String,
    change_fingerprint: String,
    paths: Vec<String>,
    path_classes: Vec<PathClassEntry>,
    disposition: String,
    rationale: Option<String>,
    recorded_at: String,
    producer: Producer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PathClassEntry {
    path: String,
    path_class: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Producer {
    tool: String,
    nils_cli_version: String,
}

struct DurableSnapshot {
    base_revision: String,
    head_revision: String,
    changed_path_digest: String,
    change_fingerprint: String,
    paths: Vec<String>,
    path_classes: Vec<PathClassEntry>,
    docs_changed: bool,
    non_docs_changed: bool,
}

#[derive(Debug, Serialize)]
struct RecordResult {
    record_file: String,
    record: DocsImpactRecord,
}

impl RecordResult {
    fn text_summary(&self) -> String {
        format!("docs-impact: disposition={}", self.record.disposition)
    }
}

#[derive(Debug, Serialize)]
struct VerifyResult {
    verified: bool,
    reason_code: String,
    record: DocsImpactRecord,
}

//! `plan-archive discover` — read-only archive-candidate scanner.
//!
//! Enumerates plan folders under the source repo's `docs/plans/` (or an
//! explicit `--plans-root`), infers provider refs and closeout evidence
//! from local files, previews each folder's archive target, and
//! classifies it as `eligible`, `blocked`, or `unknown`.
//!
//! Discover is strictly read-only: it never copies, deletes, writes,
//! commits, pushes, or refreshes provider state. It reuses the same
//! source-identity, host-classification, and archive-target derivation
//! as `migrate` (via [`crate::source`], [`crate::migrate`]) so the two
//! commands cannot drift, and it only *suggests* a `plan-archive
//! migrate` command — applying a migration remains migrate's job,
//! dry-run-first and gated on explicit confirmation.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use regex::Regex;
use serde::Serialize;

use crate::migrate::{SourceIdentity, archive_target_path, derive_source_identity};
use crate::refresh::refparse::{RefKind, parse_ref_url};
use crate::source::{self, SourceError};

const COMMAND: &str = "discover";
const BINARY: &str = "plan-archive";

/// Status keywords that signal a plan is finished. Matched
/// case-insensitively inside a `Status:` value.
const TERMINAL_KEYWORDS: &[&str] = &[
    "complete",
    "completed",
    "closed",
    "done",
    "delivered",
    "merged",
    "archived",
    "shipped",
    "ready for close",
    "ready_for_close",
    "ready-for-close",
];

/// Status keywords that signal a plan is still in flight. Their
/// presence vetoes a closeout match so sprint-level "complete" notes
/// (e.g. "Sprint 2 complete; Sprint 3 active") do not promote a folder
/// to `eligible`.
const ACTIVE_KEYWORDS: &[&str] = &[
    "active",
    "in progress",
    "in-progress",
    "not started",
    "not-started",
    "pending",
    "ready to implement",
    "wip",
    "todo",
    "blocked",
    "ongoing",
    "draft",
];

/// Args forwarded from `cli::run`.
pub struct DispatchArgs {
    pub source_repo: Option<PathBuf>,
    pub plans_root: Option<PathBuf>,
    pub archive: Option<PathBuf>,
    pub hosts: Option<PathBuf>,
    pub include_unknown: bool,
    pub format: OutputFormat,
}

/// Candidate classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoverStatus {
    /// Has ≥1 provider ref, a free archive target, a clean folder, a
    /// known host, and confident closeout evidence.
    Eligible,
    /// Has at least one actionable blocker that must be resolved before
    /// migration.
    Blocked,
    /// Otherwise migrate-able, but closeout evidence could not be
    /// confidently inferred from local files.
    Unknown,
}

/// Why a candidate landed in `blocked` or `unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverReason {
    ArchiveTargetExists,
    SourcePlanFolderDirty,
    UnknownHost,
    NoProviderRefs,
    CloseoutEvidenceUncertain,
}

impl DiscoverReason {
    pub fn code(self) -> &'static str {
        match self {
            DiscoverReason::ArchiveTargetExists => "archive-target-exists",
            DiscoverReason::SourcePlanFolderDirty => "source-plan-folder-dirty",
            DiscoverReason::UnknownHost => "unknown-host",
            DiscoverReason::NoProviderRefs => "no-provider-refs",
            DiscoverReason::CloseoutEvidenceUncertain => "closeout-evidence-uncertain",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            DiscoverReason::ArchiveTargetExists => {
                "archive target already exists; resolve the collision before migrating"
            }
            DiscoverReason::SourcePlanFolderDirty => {
                "plan folder has uncommitted or untracked changes; commit or stash them first"
            }
            DiscoverReason::UnknownHost => {
                "source host is absent from the archive `config/hosts.yaml`"
            }
            DiscoverReason::NoProviderRefs => {
                "no issue/PR/MR reference could be inferred from the plan folder"
            }
            DiscoverReason::CloseoutEvidenceUncertain => {
                "no confident closeout marker found in the plan folder; review before migrating"
            }
        }
    }
}

/// Serializable `{code, detail}` reason row.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReasonDetail {
    pub code: String,
    pub detail: String,
}

impl From<DiscoverReason> for ReasonDetail {
    fn from(r: DiscoverReason) -> Self {
        Self {
            code: r.code().to_string(),
            detail: r.detail().to_string(),
        }
    }
}

/// A provider reference inferred from a plan folder's Markdown.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InferredRef {
    /// Canonical provider URL.
    pub url: String,
    pub kind: RefKind,
    /// Source-repo-relative path of the file the ref was found in.
    pub source_file: String,
    /// Whether the ref points at the source repo itself (vs. a
    /// cross-repo reference).
    pub matches_source_repo: bool,
}

/// Local evidence that a plan is finished.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CloseoutEvidence {
    /// The matched marker text (a closeout heading or a terminal
    /// `Status:` value).
    pub marker: String,
    /// Source-repo-relative path of the file the marker was found in.
    pub source_file: String,
}

/// Preview of where a plan folder would land in the archive.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ArchiveTargetPreview {
    pub relative_path: String,
    pub absolute_path: String,
    pub exists: bool,
}

/// One classified plan folder.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoverCandidate {
    /// Folder name, e.g. `2026-05-27-my-plan`.
    pub plan_folder: String,
    /// Source-repo-relative folder path with a trailing slash.
    pub source_path: String,
    pub status: DiscoverStatus,
    pub reasons: Vec<ReasonDetail>,
    pub refs: Vec<InferredRef>,
    pub archive_target: ArchiveTargetPreview,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closeout_evidence: Option<CloseoutEvidence>,
    pub dirty: bool,
    /// A ready-to-review `plan-archive migrate …` command. Present only
    /// for `eligible` candidates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_migrate_command: Option<String>,
}

/// Aggregate counts. Always reports true totals even when `unknown`
/// candidates are omitted from `candidates`.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoverSummary {
    pub scanned: usize,
    pub eligible: usize,
    pub blocked: usize,
    pub unknown: usize,
    pub included_unknown: bool,
}

/// Full discovery report.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoverReport {
    pub source: SourceIdentity,
    /// Source-repo-relative plan-folder root.
    pub plans_root: String,
    /// Absolute archive clone path.
    pub archive: String,
    pub host_known: bool,
    pub summary: DiscoverSummary,
    pub candidates: Vec<DiscoverCandidate>,
}

/// Failure modes for `plan-archive discover`.
#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    #[error("source repo not found at `{0}`")]
    SourceRepoNotFound(PathBuf),
    #[error(
        "archive clone path not found at `{0}` (set `--archive` or seed `archive_clone_path` in the local config)"
    )]
    ArchiveCloneMissing(PathBuf),
    #[error("failed to load archive `config/hosts.yaml`: {0}")]
    HostsLoadFailed(String),
    #[error("failed to parse archive `config/hosts.yaml`: {0}")]
    HostsParseFailed(String),
    #[error("failed to read source repo identity: {0}")]
    IdentityFailed(String),
    #[error("plans root `{0}` resolves outside the source repo")]
    PlansRootOutsideRepo(PathBuf),
    #[error("io error during discovery: {0}")]
    Io(String),
}

impl DiscoverError {
    pub fn code(&self) -> &'static str {
        match self {
            DiscoverError::SourceRepoNotFound(_) => "discover-source-repo-not-found",
            DiscoverError::ArchiveCloneMissing(_) => "discover-archive-clone-missing",
            DiscoverError::HostsLoadFailed(_) => "discover-hosts-load-failed",
            DiscoverError::HostsParseFailed(_) => "discover-hosts-parse-failed",
            DiscoverError::IdentityFailed(_) => "discover-identity-failed",
            DiscoverError::PlansRootOutsideRepo(_) => "discover-plans-root-outside-repo",
            DiscoverError::Io(_) => "discover-io-error",
        }
    }
}

impl From<SourceError> for DiscoverError {
    fn from(err: SourceError) -> Self {
        match err {
            SourceError::SourceRepoNotFound(p) => DiscoverError::SourceRepoNotFound(p),
            SourceError::ArchiveCloneMissing(p) => DiscoverError::ArchiveCloneMissing(p),
            SourceError::HostsLoadFailed(s) => DiscoverError::HostsLoadFailed(s),
            SourceError::HostsParseFailed(s) => DiscoverError::HostsParseFailed(s),
            SourceError::Io(s) => DiscoverError::Io(s),
        }
    }
}

/// Entry point called from `cli::run`.
pub fn dispatch(args: DispatchArgs) -> i32 {
    let format = args.format;
    match scan(&args) {
        Ok(report) => emit(format, &report),
        Err(err) => emit_error(format, err.code(), &err.to_string()),
    }
}

/// Read-only scan. Resolves the source repo, archive, and hosts config,
/// enumerates plan folders, and classifies each. Performs no writes.
///
/// The returned `candidates` honour `args.include_unknown` (unknowns are
/// omitted unless requested); `summary` always carries true totals.
pub fn scan(args: &DispatchArgs) -> Result<DiscoverReport, DiscoverError> {
    let source_repo = source::resolve_source_repo(args.source_repo.as_deref())?;
    let archive = source::resolve_archive(args.archive.as_deref())?;
    let hosts_path = source::hosts_path_for(&archive, args.hosts.as_deref());
    let hosts = source::load_hosts(&hosts_path)?;

    let identity = derive_source_identity(&source_repo)
        .map_err(|e| DiscoverError::IdentityFailed(e.to_string()))?;
    let host_known = hosts.hosts.contains_key(&identity.host);

    let plans_root_rel = resolve_plans_root_rel(&source_repo, args.plans_root.as_deref())?;
    let plans_root_abs = source_repo.join(&plans_root_rel);

    let url_re = url_regex();
    let status_re = status_regex();
    let closeout_heading_re = closeout_heading_regex();

    let mut all: Vec<DiscoverCandidate> = Vec::new();
    if plans_root_abs.is_dir() {
        for dir in sorted_subdirs(&plans_root_abs)? {
            let folder = match dir.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            let rel_dir = plans_root_rel.join(&folder);
            let source_path = format!("{}/", rel_dir.to_string_lossy().trim_end_matches('/'));

            let (refs, closeout) = scan_folder_files(
                &dir,
                &source_path,
                &identity,
                &url_re,
                &status_re,
                &closeout_heading_re,
            );

            let target_rel = archive_target_path(
                &identity.host,
                &identity.org_or_group_path,
                &identity.repo,
                &folder,
            );
            let target_abs = archive.join(&target_rel);
            let archive_target = ArchiveTargetPreview {
                relative_path: target_rel.to_string_lossy().to_string(),
                absolute_path: target_abs.to_string_lossy().to_string(),
                exists: target_abs.exists(),
            };

            let dirty = source::has_dirty_path(&source_repo, &rel_dir)?;

            // Accumulate blockers, then fall through to the
            // eligible/unknown split.
            let mut reasons: Vec<DiscoverReason> = Vec::new();
            if archive_target.exists {
                reasons.push(DiscoverReason::ArchiveTargetExists);
            }
            if dirty {
                reasons.push(DiscoverReason::SourcePlanFolderDirty);
            }
            if !host_known {
                reasons.push(DiscoverReason::UnknownHost);
            }
            if refs.is_empty() {
                reasons.push(DiscoverReason::NoProviderRefs);
            }

            let status = if !reasons.is_empty() {
                DiscoverStatus::Blocked
            } else if closeout.is_some() {
                DiscoverStatus::Eligible
            } else {
                reasons.push(DiscoverReason::CloseoutEvidenceUncertain);
                DiscoverStatus::Unknown
            };

            let suggested_migrate_command = if status == DiscoverStatus::Eligible {
                Some(build_migrate_command(
                    rel_dir.to_string_lossy().trim_end_matches('/'),
                    &refs,
                ))
            } else {
                None
            };

            all.push(DiscoverCandidate {
                plan_folder: folder,
                source_path,
                status,
                reasons: reasons.into_iter().map(ReasonDetail::from).collect(),
                refs,
                archive_target,
                closeout_evidence: closeout,
                dirty,
                suggested_migrate_command,
            });
        }
    }

    let scanned = all.len();
    let eligible = all
        .iter()
        .filter(|c| c.status == DiscoverStatus::Eligible)
        .count();
    let blocked = all
        .iter()
        .filter(|c| c.status == DiscoverStatus::Blocked)
        .count();
    let unknown = all
        .iter()
        .filter(|c| c.status == DiscoverStatus::Unknown)
        .count();

    if !args.include_unknown {
        all.retain(|c| c.status != DiscoverStatus::Unknown);
    }

    Ok(DiscoverReport {
        source: identity,
        plans_root: plans_root_rel.to_string_lossy().to_string(),
        archive: archive.to_string_lossy().to_string(),
        host_known,
        summary: DiscoverSummary {
            scanned,
            eligible,
            blocked,
            unknown,
            included_unknown: args.include_unknown,
        },
        candidates: all,
    })
}

/// Resolve `--plans-root` to a source-repo-relative path. Defaults to
/// `docs/plans`. An absolute path must live under the source repo.
fn resolve_plans_root_rel(
    source_repo: &Path,
    arg: Option<&Path>,
) -> Result<PathBuf, DiscoverError> {
    match arg {
        None => Ok(PathBuf::from("docs/plans")),
        Some(p) if p.is_absolute() => p
            .strip_prefix(source_repo)
            .map(Path::to_path_buf)
            .map_err(|_| DiscoverError::PlansRootOutsideRepo(p.to_path_buf())),
        Some(p) => Ok(p.to_path_buf()),
    }
}

/// Immediate sub-directories of `root`, sorted, skipping dot-entries.
fn sorted_subdirs(root: &Path) -> Result<Vec<PathBuf>, DiscoverError> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| DiscoverError::Io(e.to_string()))? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with('.')
        {
            continue;
        }
        dirs.push(path);
    }
    dirs.sort();
    Ok(dirs)
}

/// Scan a plan folder's top-level Markdown files for provider refs and
/// closeout evidence. Reads only; never mutates.
fn scan_folder_files(
    dir: &Path,
    source_path: &str,
    identity: &SourceIdentity,
    url_re: &Regex,
    status_re: &Regex,
    closeout_heading_re: &Regex,
) -> (Vec<InferredRef>, Option<CloseoutEvidence>) {
    let mut refs: Vec<InferredRef> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut closeout: Option<CloseoutEvidence> = None;

    for (file_name, content) in read_markdown_files(dir) {
        let source_file = format!("{source_path}{file_name}");

        for cap in url_re.find_iter(&content) {
            let raw = cap.as_str().trim_end_matches(['.', ',', ';', ':']);
            if let Some(target) = parse_ref_url(raw) {
                let url = target.canonical_url();
                if seen.insert(url.clone()) {
                    let matches_source_repo = target.host == identity.host
                        && target.org_or_group_path == identity.org_or_group_path
                        && target.repo == identity.repo;
                    refs.push(InferredRef {
                        url,
                        kind: target.kind,
                        source_file: source_file.clone(),
                        matches_source_repo,
                    });
                }
            }
        }

        if closeout.is_none()
            && let Some(marker) = detect_closeout(&content, status_re, closeout_heading_re)
        {
            closeout = Some(CloseoutEvidence {
                marker,
                source_file,
            });
        }
    }

    (refs, closeout)
}

/// Top-level `*.md` files in `dir` as `(file_name, content)`, sorted by
/// file name. Unreadable or non-UTF-8 files are skipped.
fn read_markdown_files(dir: &Path) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return files,
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        })
        .collect();
    paths.sort();
    for path in paths {
        if let (Some(name), Ok(content)) = (
            path.file_name().and_then(|n| n.to_str()),
            fs::read_to_string(&path),
        ) {
            files.push((name.to_string(), content));
        }
    }
    files
}

/// Return the first confident closeout marker in `content`, or `None`.
///
/// A Markdown "Closeout" heading is authoritative. Otherwise a
/// `Status:` value counts only when it carries a terminal keyword and
/// no active keyword, so sprint-level "complete" notes alongside an
/// active sprint do not qualify.
fn detect_closeout(
    content: &str,
    status_re: &Regex,
    closeout_heading_re: &Regex,
) -> Option<String> {
    if let Some(m) = closeout_heading_re.find(content) {
        return Some(m.as_str().trim().to_string());
    }
    for cap in status_re.captures_iter(content) {
        let value = cap.get(1).map(|m| m.as_str().trim()).unwrap_or_default();
        if value.is_empty() {
            continue;
        }
        let lower = value.to_ascii_lowercase();
        let has_terminal = TERMINAL_KEYWORDS.iter().any(|k| lower.contains(k));
        let has_active = ACTIVE_KEYWORDS.iter().any(|k| lower.contains(k));
        if has_terminal && !has_active {
            return Some(value.to_string());
        }
    }
    None
}

/// Build a single combined `plan-archive migrate` command for an
/// eligible folder, preferring self-referential refs for each kind.
fn build_migrate_command(plan_rel: &str, refs: &[InferredRef]) -> String {
    let mut cmd = format!("plan-archive migrate --plan {plan_rel}");
    if let Some(r) = pick_ref(refs, RefKind::Issue) {
        cmd.push_str(&format!(" --issue {}", r.url));
    }
    if let Some(r) = pick_ref(refs, RefKind::Pull) {
        cmd.push_str(&format!(" --pr {}", r.url));
    }
    if let Some(r) = pick_ref(refs, RefKind::MergeRequest) {
        cmd.push_str(&format!(" --mr {}", r.url));
    }
    cmd.push_str(" --format json");
    cmd
}

/// First ref of `kind`, preferring one that matches the source repo.
fn pick_ref(refs: &[InferredRef], kind: RefKind) -> Option<&InferredRef> {
    refs.iter()
        .filter(|r| r.kind == kind)
        .find(|r| r.matches_source_repo)
        .or_else(|| refs.iter().find(|r| r.kind == kind))
}

fn url_regex() -> Regex {
    // Static, validated pattern: a `expect` here is a programmer error,
    // not a runtime input failure.
    Regex::new(r#"https?://[^\s<>()\[\]"'`]+"#).expect("valid url regex")
}

fn status_regex() -> Regex {
    Regex::new(r"(?im)^[ \t>]*[-*]?[ \t]*status[ \t]*:[ \t]*(.+)$").expect("valid status regex")
}

fn closeout_heading_regex() -> Regex {
    Regex::new(r"(?im)^[ \t>]*#{1,6}[ \t]*close[ _-]?out\b").expect("valid closeout regex")
}

fn emit(format: OutputFormat, report: &DiscoverReport) -> i32 {
    match format {
        OutputFormat::Json => emit_json(report),
        OutputFormat::Text => emit_text(report),
    }
}

fn emit_text(report: &DiscoverReport) -> i32 {
    println!("plan-archive discover (read-only)");
    println!(
        "  source     : {}/{}/{} @ {}",
        report.source.host,
        report.source.org_or_group_path,
        report.source.repo,
        report.source.branch
    );
    println!("  plans root : {}", report.plans_root);
    println!("  archive    : {}", report.archive);
    if !report.host_known {
        println!("  host       : NOT in config/hosts.yaml (every folder is blocked)");
    }
    let s = &report.summary;
    println!(
        "  scanned {} — {} eligible, {} blocked, {} unknown",
        s.scanned, s.eligible, s.blocked, s.unknown
    );
    if !s.included_unknown && s.unknown > 0 {
        println!(
            "  ({} unknown hidden; pass --include-unknown to list them)",
            s.unknown
        );
    }

    for (label, status) in [
        ("eligible", DiscoverStatus::Eligible),
        ("blocked", DiscoverStatus::Blocked),
        ("unknown", DiscoverStatus::Unknown),
    ] {
        let group: Vec<&DiscoverCandidate> = report
            .candidates
            .iter()
            .filter(|c| c.status == status)
            .collect();
        if group.is_empty() {
            continue;
        }
        println!("\n{label} ({}):", group.len());
        for c in group {
            println!("  • {}", c.plan_folder);
            if !c.reasons.is_empty() {
                let codes: Vec<&str> = c.reasons.iter().map(|r| r.code.as_str()).collect();
                println!("    reasons : {}", codes.join(", "));
            }
            if c.refs.is_empty() {
                println!("    refs    : (none inferred)");
            } else {
                for r in &c.refs {
                    let scope = if r.matches_source_repo {
                        ""
                    } else {
                        " (cross-repo)"
                    };
                    println!(
                        "    ref     : {} {}{}",
                        ref_kind_label(r.kind),
                        r.url,
                        scope
                    );
                }
            }
            println!(
                "    target  : {} ({})",
                c.archive_target.relative_path,
                if c.archive_target.exists {
                    "exists"
                } else {
                    "free"
                }
            );
            if let Some(co) = &c.closeout_evidence {
                println!("    closeout: {} [{}]", co.marker, co.source_file);
            }
            if let Some(cmd) = &c.suggested_migrate_command {
                println!("    migrate : {cmd}");
            }
        }
    }
    exit::SUCCESS
}

fn ref_kind_label(kind: RefKind) -> &'static str {
    match kind {
        RefKind::Issue => "issue",
        RefKind::Pull => "pr",
        RefKind::MergeRequest => "mr",
    }
}

fn emit_json(report: &DiscoverReport) -> i32 {
    let envelope = Envelope::success(schema_version_for(BINARY, COMMAND, 1), report);
    match serde_json::to_string(&envelope) {
        Ok(s) => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            if writeln!(handle, "{s}").is_err() {
                return exit::SOFTWARE;
            }
            exit::SUCCESS
        }
        Err(_) => exit::SOFTWARE,
    }
}

fn emit_error(format: OutputFormat, code: &str, message: &str) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope: Envelope<()> = Envelope::failure(
                schema_version_for(BINARY, COMMAND, 1),
                EnvelopeError::new(code, message),
            );
            if let Ok(s) = serde_json::to_string(&envelope) {
                eprintln!("{s}");
            }
            exit::DATA
        }
        OutputFormat::Text => {
            eprintln!("error [{code}]: {message}");
            exit::DATA
        }
    }
}

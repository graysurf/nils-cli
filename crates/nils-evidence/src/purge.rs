//! `evidence purge` — delete archived evidence for named hosts / host classes.
//!
//! The retention counterpart to `migrate`: where migrate writes rollups in,
//! purge removes the `evidence/<host>/` trees for a scope the operator names
//! explicitly. The primary use is employer `delete-on-termination` — purge
//! `--class employer` (or a specific `--host`) when an employment relationship
//! ends.
//!
//! Safety:
//! - Dry-run by default; `--apply` is required to delete, regenerate the
//!   catalog, commit, and push.
//! - A scope is REQUIRED — at least one of `--host` / `--class`. There is no
//!   implicit whole-archive purge.
//! - Explicit `--host` values must be plain host labels: a single path
//!   component, never absolute, `.`, `..`, or path-separated, so the resolved
//!   `evidence/<host>` tree can never escape the archive subtree.
//! - `--apply` refuses to run against a dirty archive working tree (under
//!   `evidence/` or `catalog.json`) AND against any pre-existing staged change
//!   anywhere in the archive, so it only ever commits the purge-owned pathspec.
//! - A scoped host tree is deleted whenever it exists, even when it holds only
//!   orphaned files without a rollup; the dry-run surfaces this so it always
//!   describes what apply will remove.
//! - Apply refuses a scoped host tree that carries untracked or ignored files,
//!   since a rollback (`git checkout`) could not restore them.
//! - Any failure after deletion and before the commit lands (catalog regen,
//!   stage, commit) rolls the deletions back, so apply never leaves uncommitted
//!   destructive state.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::Serialize;

use crate::migrate::MigrateError;
use crate::source::{self, SourceError};
use crate::validate::hosts::HostClass;

const COMMAND: &str = "purge";
const BINARY: &str = "evidence";

/// Args forwarded from `cli::run`.
pub struct PurgeArgs {
    pub archive: Option<PathBuf>,
    pub hosts: Option<PathBuf>,
    /// Explicit host(s) to purge (repeatable `--host`).
    pub host: Vec<String>,
    /// Purge every host classified as this class in `config/hosts.yaml`.
    pub class: Option<HostClass>,
    pub apply: bool,
    pub format: OutputFormat,
}

/// Per-host purge plan/result.
#[derive(Debug, Clone, Serialize)]
pub struct PurgeTarget {
    pub host: String,
    pub records: usize,
    /// Archive-relative record directories under this host.
    pub paths: Vec<String>,
    /// Whether `evidence/<host>/` exists on disk. Apply removes the entire tree
    /// when it does, even with zero discovered rollups — so the dry-run surfaces
    /// this to describe exactly what apply will delete.
    pub host_tree_present: bool,
}

/// Result of a purge dry-run or apply.
#[derive(Debug, Clone, Serialize)]
pub struct PurgeReport {
    pub archive: String,
    /// The resolved scope (sorted union of `--host` and `--class` hosts).
    pub scope_hosts: Vec<String>,
    pub targets: Vec<PurgeTarget>,
    pub total_records: usize,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_commit: Option<String>,
}

/// Errors produced by the purge pipeline.
#[derive(Debug, thiserror::Error)]
pub enum PurgeError {
    #[error("specify a purge scope: at least one of --host or --class")]
    NoScope,
    #[error(
        "unsafe --host value `{0}`: expected a plain host label (no path separators, `.`, or `..`)"
    )]
    UnsafeHost(String),
    #[error(
        "archive has pre-existing staged changes; purge --apply commits only its own changes, so stash or commit them first"
    )]
    StagedArchiveChanges,
    #[error(
        "scoped host `{0}` contains untracked or ignored files that a rollback cannot restore; remove or commit them before purging"
    )]
    UnrestorableScope(String),
    #[error(
        "purge failed after deleting evidence ({cause}) AND the rollback also failed ({rollback}); the archive working tree still has uncommitted deletions — recover with `git checkout -- evidence catalog.json`"
    )]
    RollbackFailed { cause: String, rollback: String },
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("failed to regenerate archive catalog: {0}")]
    Catalog(String),
    #[error("io error during purge: {0}")]
    Io(String),
    /// Shared archive-transaction error (dirty repo, commit / push, etc.).
    #[error(transparent)]
    Transaction(#[from] MigrateError),
}

impl PurgeError {
    pub fn code(&self) -> &'static str {
        match self {
            PurgeError::NoScope => "purge-no-scope",
            PurgeError::UnsafeHost(_) => "purge-unsafe-host",
            PurgeError::StagedArchiveChanges => "purge-staged-archive-changes",
            PurgeError::UnrestorableScope(_) => "purge-unrestorable-scope",
            PurgeError::RollbackFailed { .. } => "purge-rollback-failed",
            PurgeError::Source(_) => "purge-source-error",
            PurgeError::Catalog(_) => "purge-catalog-error",
            PurgeError::Io(_) => "purge-io-error",
            PurgeError::Transaction(e) => e.code(),
        }
    }
}

/// Entry point called from `cli::run`.
pub fn dispatch(args: PurgeArgs) -> i32 {
    let format = args.format;
    match run(&args) {
        Ok(report) => emit(format, &report),
        Err(err) => emit_error(format, err.code(), &err.to_string()),
    }
}

pub fn run(args: &PurgeArgs) -> Result<PurgeReport, PurgeError> {
    // A scope is mandatory: never an implicit whole-archive purge.
    if args.host.is_empty() && args.class.is_none() {
        return Err(PurgeError::NoScope);
    }

    let archive = source::resolve_archive(args.archive.as_deref())?;
    let hosts_path = source::hosts_path_for(&archive, args.hosts.as_deref());
    let hosts = source::load_hosts(&hosts_path)?;

    // Resolve the scope: explicit --host values plus every host of --class.
    let mut scope: BTreeSet<String> = args.host.iter().cloned().collect();
    if let Some(class) = args.class {
        for (host, entry) in hosts.hosts.iter() {
            if entry.class == class {
                scope.insert(host.clone());
            }
        }
    }
    let scope_hosts: Vec<String> = scope.into_iter().collect();

    // Reject any path-like host before a join / deletion can act on it. This
    // covers BOTH explicit --host values and --class-derived keys read from
    // config/hosts.yaml (which is not shape-validated at load), so a single
    // safe path component is the only thing that ever reaches `evidence/<host>`.
    for host in &scope_hosts {
        ensure_plain_host_label(host)?;
    }

    // Enumerate the record directories under evidence/<host>/ for each host,
    // and track which scoped host trees actually exist on disk. The apply no-op
    // decision keys off existence, not the rollup count, so a host tree holding
    // only orphaned files (no rollup) is still deleted.
    let mut targets = Vec::new();
    let mut total = 0usize;
    let mut existing_host_roots = 0usize;
    for host in &scope_hosts {
        let host_root = archive.join("evidence").join(host);
        let host_tree_present = host_root.exists();
        if host_tree_present {
            existing_host_roots += 1;
        }
        let mut paths = Vec::new();
        collect_record_dirs(&archive, &host_root, &mut paths)?;
        paths.sort();
        total += paths.len();
        targets.push(PurgeTarget {
            host: host.clone(),
            records: paths.len(),
            paths,
            host_tree_present,
        });
    }

    if !args.apply {
        return Ok(PurgeReport {
            archive: archive.display().to_string(),
            scope_hosts,
            targets,
            total_records: total,
            applied: false,
            archive_commit: None,
        });
    }

    // Apply: refuse if the archive is dirty under evidence/ or catalog.json so
    // we never commit unrelated changes.
    if source::has_dirty_path(&archive, Path::new("evidence"))?
        || source::has_dirty_path(&archive, Path::new("catalog.json"))?
    {
        return Err(MigrateError::ArchiveRepoDirty.into());
    }

    // The dirty guard above only covers the purge-owned pathspec, but the
    // semantic-commit step commits the whole index. Refuse any pre-existing
    // staged change anywhere in the archive so purge only commits what it
    // stages itself (evidence/ + catalog.json).
    if nils_common::git::has_staged_changes_in(&archive)
        .map_err(|e| PurgeError::Io(e.to_string()))?
    {
        return Err(PurgeError::StagedArchiveChanges);
    }

    // Rollback restores tracked content from HEAD; it cannot bring back
    // untracked or ignored files. Refuse any scoped host root that carries such
    // files so a failed apply is always fully recoverable (and so deletion
    // always produces a stageable change rather than an empty commit).
    for host in &scope_hosts {
        if scope_has_unrestorable_files(&archive, host)? {
            return Err(PurgeError::UnrestorableScope(host.clone()));
        }
    }

    // No scoped host tree exists on disk: a clean no-op (no deletion, no
    // commit). Keyed on tree existence, not the rollup count, so a host tree
    // with only orphaned files (total == 0) is still deleted below.
    if existing_host_roots == 0 {
        return Ok(PurgeReport {
            archive: archive.display().to_string(),
            scope_hosts,
            targets,
            total_records: total,
            applied: true,
            archive_commit: None,
        });
    }

    // Remove each host tree under evidence/.
    for host in &scope_hosts {
        let host_root = archive.join("evidence").join(host);
        if host_root.exists() {
            fs::remove_dir_all(&host_root).map_err(|e| PurgeError::Io(e.to_string()))?;
        }
    }

    // From here until the commit lands, every step (catalog regen, stage,
    // commit) leaves the destructive deletions in the working tree if it fails.
    // Roll them back on ANY such failure so apply never leaves uncommitted
    // destructive state; if the rollback itself fails, surface that loudly.
    // Push failure is exempt — the commit already exists locally and a re-run
    // dedups via the catalog.
    let archive_commit = match commit_purge(&archive, &scope_hosts, total) {
        Ok(sha) => sha,
        Err(cause) => {
            if let Err(rollback_err) = rollback_deletions(&archive) {
                return Err(PurgeError::RollbackFailed {
                    cause: cause.to_string(),
                    rollback: rollback_err,
                });
            }
            return Err(cause);
        }
    };
    crate::migrate::push_archive(&archive)?;

    Ok(PurgeReport {
        archive: archive.display().to_string(),
        scope_hosts,
        targets,
        total_records: total,
        applied: true,
        archive_commit: Some(archive_commit),
    })
}

/// Regenerate the catalog, stage the purge pathspec, and create the purge
/// commit, returning the new commit SHA. Every failure here leaves the working
/// tree holding the destructive deletions, so the caller rolls back on `Err`.
fn commit_purge(
    archive: &Path,
    scope_hosts: &[String],
    total: usize,
) -> Result<String, PurgeError> {
    crate::catalog::write_catalog(archive).map_err(|e| PurgeError::Catalog(e.to_string()))?;

    let stage = ["add", "-A", "--", "evidence", "catalog.json"];
    let out = nils_common::git::run_output_in(archive, &stage)
        .map_err(|e| PurgeError::Io(e.to_string()))?;
    if !out.status.success() {
        return Err(MigrateError::Subprocess(
            "git add (archive)".to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
        .into());
    }

    let message = purge_commit_message(scope_hosts, total);
    crate::migrate::run_semantic_commit(archive, &message)?;
    Ok(crate::migrate::head_sha(archive)?)
}

/// Reject a host value that is not a plain host label. A host must be a single
/// normal path component so `evidence/<host>` stays exactly one level under
/// `evidence/`; absolute values, `.`, `..`, path-separated values, and empty
/// strings are refused before any join or deletion.
///
/// The raw-separator check comes first because `Path::components()` normalizes
/// a trailing separator (and a non-leading `.`) away — `github.com/` and
/// `github.com/.` would otherwise pass as a single `Normal` component.
fn ensure_plain_host_label(host: &str) -> Result<(), PurgeError> {
    use std::path::Component;
    if host.contains('/') || host.contains('\\') {
        return Err(PurgeError::UnsafeHost(host.to_string()));
    }
    let mut components = Path::new(host).components();
    let only_normal = matches!(components.next(), Some(Component::Normal(_)));
    if !only_normal || components.next().is_some() {
        return Err(PurgeError::UnsafeHost(host.to_string()));
    }
    Ok(())
}

/// Whether `evidence/<host>/` carries untracked or ignored files that a
/// `git checkout` rollback could not restore. Such files make a failed apply
/// unrecoverable, so purge refuses the scope rather than risk permanent loss.
fn scope_has_unrestorable_files(archive: &Path, host: &str) -> Result<bool, PurgeError> {
    let rel = format!("evidence/{host}");
    let out = nils_common::git::run_output_in(
        archive,
        &["status", "--porcelain", "--ignored", "--", &rel],
    )
    .map_err(|e| PurgeError::Io(e.to_string()))?;
    if !out.status.success() {
        return Err(PurgeError::Io(format!(
            "git status --ignored failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .any(|l| l.starts_with("??") || l.starts_with("!!")))
}

/// Roll back the working-tree deletions after a failed apply step. The tree was
/// proven clean at HEAD (and the index clean) before deletion, so resetting the
/// purge-owned pathspec back to HEAD and dropping any untracked residue returns
/// the archive exactly to its pre-purge state — regardless of how far the
/// partial run got (a catalog failure leaves nothing staged; a commit failure
/// leaves the deletions and a regenerated `catalog.json` staged).
///
/// Restoring `evidence/` is the destructive change that MUST be undone, so a
/// failure of that worktree restore is surfaced to the caller (`Err`); the
/// `catalog.json` restore and untracked cleanup are best-effort because that
/// path may legitimately not exist in HEAD.
fn rollback_deletions(archive: &Path) -> Result<(), String> {
    // Unstage the purge pathspec so the index matches HEAD before restoring.
    let _ = nils_common::git::run_output_in(
        archive,
        &["reset", "-q", "HEAD", "--", "evidence", "catalog.json"],
    );
    // Restore the deleted evidence trees from HEAD — the destructive undo.
    let out = nils_common::git::run_output_in(archive, &["checkout", "-q", "--", "evidence"])
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "git checkout -- evidence failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // Restore catalog.json if it exists in HEAD; drop any untracked residue
    // (e.g. a freshly regenerated catalog.json that was never committed).
    let _ = nils_common::git::run_output_in(archive, &["checkout", "-q", "--", "catalog.json"]);
    let _ = nils_common::git::run_output_in(
        archive,
        &["clean", "-fdq", "--", "evidence", "catalog.json"],
    );
    Ok(())
}

/// Collect archive-relative record directories (those containing a
/// `skill-usage.rollup.json`) under `root`. A missing `root` yields nothing.
fn collect_record_dirs(
    archive: &Path,
    root: &Path,
    out: &mut Vec<String>,
) -> Result<(), PurgeError> {
    if !root.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(root).map_err(|e| PurgeError::Io(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| PurgeError::Io(e.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            if path.join("skill-usage.rollup.json").is_file() {
                let rel = path.strip_prefix(archive).unwrap_or(&path);
                out.push(rel.to_string_lossy().replace('\\', "/"));
            } else {
                collect_record_dirs(archive, &path, out)?;
            }
        }
    }
    Ok(())
}

fn purge_commit_message(scope_hosts: &[String], total: usize) -> String {
    format!(
        "chore(evidence): purge archived evidence for {} host(s)\n\n- Removed {} record(s) across: {}",
        scope_hosts.len(),
        total,
        scope_hosts.join(", ")
    )
}

fn emit(format: OutputFormat, report: &PurgeReport) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope = Envelope::success(schema_version_for(BINARY, COMMAND, 1), report);
            match serde_json::to_string(&envelope) {
                Ok(s) => {
                    println!("{s}");
                    exit::SUCCESS
                }
                Err(_) => exit::SOFTWARE,
            }
        }
        OutputFormat::Text => {
            let mode = if report.applied { "apply" } else { "dry-run" };
            println!("evidence purge ({mode})");
            println!("  archive : {}", report.archive);
            println!("  scope   : {}", report.scope_hosts.join(", "));
            for t in &report.targets {
                // Flag a present host tree with no discovered rollups: apply
                // removes the whole tree, so dry-run must not read as empty.
                let note = if t.host_tree_present && t.records == 0 {
                    "  (host tree present, no rollups — entire tree removed)"
                } else {
                    ""
                };
                println!("  - {} : {} record(s){note}", t.host, t.records);
                for p in &t.paths {
                    println!("      {p}");
                }
            }
            println!("  total   : {} record(s)", report.total_records);
            match (&report.applied, &report.archive_commit) {
                (true, Some(sha)) => println!("  committed: {sha} (pushed)"),
                (true, None) => println!("  nothing to delete; no commit"),
                (false, _) => println!("  (no files modified; pass --apply to delete)"),
            }
            exit::SUCCESS
        }
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

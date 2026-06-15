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
//! - `--apply` refuses to run against a dirty archive working tree (under
//!   `evidence/` or `catalog.json`), so it never commits unrelated changes.

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

    // Enumerate the record directories under evidence/<host>/ for each host.
    let mut targets = Vec::new();
    let mut total = 0usize;
    for host in &scope_hosts {
        let host_root = archive.join("evidence").join(host);
        let mut paths = Vec::new();
        collect_record_dirs(&archive, &host_root, &mut paths)?;
        paths.sort();
        total += paths.len();
        targets.push(PurgeTarget {
            host: host.clone(),
            records: paths.len(),
            paths,
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

    // Nothing matched the scope: a clean no-op (no deletion, no commit).
    if total == 0 {
        return Ok(PurgeReport {
            archive: archive.display().to_string(),
            scope_hosts,
            targets,
            total_records: 0,
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

    // Regenerate the catalog after deletion.
    crate::catalog::write_catalog(&archive).map_err(|e| PurgeError::Catalog(e.to_string()))?;

    // Stage the deletions + catalog, then one commit and push.
    let stage = ["add", "-A", "--", "evidence", "catalog.json"];
    let out = nils_common::git::run_output_in(&archive, &stage)
        .map_err(|e| PurgeError::Io(e.to_string()))?;
    if !out.status.success() {
        return Err(MigrateError::Subprocess(
            "git add (archive)".to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
        .into());
    }

    let message = purge_commit_message(&scope_hosts, total);
    crate::migrate::run_semantic_commit(&archive, &message)?;
    let archive_commit = crate::migrate::head_sha(&archive)?;
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
                println!("  - {} : {} record(s)", t.host, t.records);
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

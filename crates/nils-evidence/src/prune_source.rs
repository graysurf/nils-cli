//! `evidence prune-source` — remove local agent-out source runs that are
//! already archived.
//!
//! This is the source-side counterpart to `evidence migrate`'s copy-only
//! semantics. It never writes the archive. It reads `<archive>/catalog.json`,
//! computes each local `skill-usage.record.json` digest, and only prunes the
//! record's containing directory when that digest is present in the archive
//! catalog. Dry-run is the default; `--apply` removes the eligible directories.

use std::fs;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::Serialize;

use crate::record::SkillUsageRecord;
use crate::source::{self, SourceError};

const BINARY: &str = "evidence";
const COMMAND: &str = "prune-source";

#[derive(Debug, Clone)]
pub struct PruneSourceArgs {
    pub source_out: Option<PathBuf>,
    pub archive: Option<PathBuf>,
    pub repo: Option<String>,
    pub archived_only: bool,
    pub apply: bool,
    pub format: OutputFormat,
}

#[derive(Debug, Clone, Serialize)]
pub struct PruneSourceRecord {
    pub run_dir: String,
    pub record_path: String,
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PruneSourceReport {
    pub source_out: String,
    pub archive: String,
    pub applied: bool,
    pub scanned: usize,
    pub prunable: usize,
    pub deleted: usize,
    pub kept: usize,
    pub pruned: Vec<PruneSourceRecord>,
    pub retained: Vec<PruneSourceRecord>,
}

#[derive(Debug, thiserror::Error)]
pub enum PruneSourceError {
    #[error(
        "specify --archived-only to confirm source pruning is limited to records already present in the archive catalog"
    )]
    ArchivedOnlyRequired,
    #[error("agent-out projects root not found at `{0}`")]
    SourceOutMissing(PathBuf),
    #[error("archive clone path not found at `{0}`")]
    ArchiveCloneMissing(PathBuf),
    #[error("io error during source pruning: {0}")]
    Io(String),
}

impl PruneSourceError {
    pub fn code(&self) -> &'static str {
        match self {
            PruneSourceError::ArchivedOnlyRequired => "prune-source-archived-only-required",
            PruneSourceError::SourceOutMissing(_) => "prune-source-source-out-missing",
            PruneSourceError::ArchiveCloneMissing(_) => "prune-source-archive-clone-missing",
            PruneSourceError::Io(_) => "prune-source-io-error",
        }
    }
}

impl From<SourceError> for PruneSourceError {
    fn from(err: SourceError) -> Self {
        match err {
            SourceError::SourceOutMissing(p) => PruneSourceError::SourceOutMissing(p),
            SourceError::ArchiveCloneMissing(p) => PruneSourceError::ArchiveCloneMissing(p),
            other => PruneSourceError::Io(other.to_string()),
        }
    }
}

pub fn dispatch(args: PruneSourceArgs) -> i32 {
    let format = args.format;
    match run(&args) {
        Ok(report) => emit(format, &report),
        Err(err) => emit_error(format, err.code(), &err.to_string()),
    }
}

pub fn run(args: &PruneSourceArgs) -> Result<PruneSourceReport, PruneSourceError> {
    if !args.archived_only {
        return Err(PruneSourceError::ArchivedOnlyRequired);
    }

    let source_out = source::resolve_source_out(args.source_out.as_deref())?;
    let archive = source::resolve_archive(args.archive.as_deref())?;
    let archived_digests = crate::catalog::existing_source_digests(&archive)
        .map_err(|e| PruneSourceError::Io(e.to_string()))?;

    let mut pruned = Vec::new();
    let mut retained = Vec::new();
    for entry in enumerate_records(&source_out, args.repo.as_deref()) {
        let raw = match fs::read(&entry.record_path) {
            Ok(raw) => raw,
            Err(e) => {
                retained.push(entry.into_record(None, None, format!("read failed: {e}")));
                continue;
            }
        };
        let digest = format!("sha256:{}", sha256_hex(&raw));
        let parsed_skill = SkillUsageRecord::from_json_bytes(&raw)
            .ok()
            .and_then(|record| record.normalized_owner().ok().map(|owner| owner.id));
        if archived_digests.contains(&digest) {
            pruned.push(entry.into_record(
                parsed_skill,
                Some(digest),
                "already archived".to_string(),
            ));
        } else {
            retained.push(entry.into_record(
                parsed_skill,
                Some(digest),
                "not archived".to_string(),
            ));
        }
    }

    let mut deleted = 0;
    if args.apply {
        for record in &pruned {
            fs::remove_dir_all(&record.run_dir).map_err(|e| PruneSourceError::Io(e.to_string()))?;
            deleted += 1;
        }
    }

    Ok(PruneSourceReport {
        source_out: source_out.display().to_string(),
        archive: archive.display().to_string(),
        applied: args.apply,
        scanned: pruned.len() + retained.len(),
        prunable: pruned.len(),
        deleted,
        kept: retained.len(),
        pruned,
        retained,
    })
}

#[derive(Debug)]
struct SourceRecordEntry {
    run_dir: PathBuf,
    record_path: PathBuf,
    project: String,
}

impl SourceRecordEntry {
    fn into_record(
        self,
        skill: Option<String>,
        source_digest: Option<String>,
        reason: String,
    ) -> PruneSourceRecord {
        PruneSourceRecord {
            run_dir: self.run_dir.display().to_string(),
            record_path: self.record_path.display().to_string(),
            project: self.project,
            skill,
            source_digest,
            reason,
        }
    }
}

fn enumerate_records(source_out: &Path, repo: Option<&str>) -> Vec<SourceRecordEntry> {
    let mut out = Vec::new();
    for record_path in source::enumerate_skill_usage_records(source_out) {
        let Some(project_name) = record_path
            .strip_prefix(source_out)
            .ok()
            .and_then(|rel| rel.components().next())
            .and_then(|name| name.as_os_str().to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if !repo_matches(&project_name, repo) {
            continue;
        }
        let Some(run_dir) = record_path.parent().map(Path::to_path_buf) else {
            continue;
        };
        out.push(SourceRecordEntry {
            run_dir,
            record_path,
            project: project_name,
        });
    }
    out.sort_by(|a, b| a.record_path.cmp(&b.record_path));
    out
}

fn repo_matches(project_name: &str, repo: Option<&str>) -> bool {
    let Some(want) = repo else {
        return true;
    };
    project_name == want
        || project_name
            .rsplit_once("__")
            .is_some_and(|(_, repo_name)| repo_name == want)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn emit(format: OutputFormat, report: &PruneSourceReport) -> i32 {
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
            println!(
                "prune-source ({mode}): {} prunable, {} retained, {} deleted",
                report.prunable, report.kept, report.deleted
            );
            for record in &report.pruned {
                println!("  [prunable] {}", record.run_dir);
            }
            for record in &report.retained {
                println!("  [retained:{}] {}", record.reason, record.run_dir);
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

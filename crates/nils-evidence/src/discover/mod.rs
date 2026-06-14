//! `evidence discover` — read-only scan of the agent-out tree for archivable
//! skill-usage records.
//!
//! Classifies each `*/<ts>-skill-usage/skill-usage.record.json` as:
//! - `eligible`: parseable and not yet archived (no matching `source_digest`
//!   in the archive catalog);
//! - `blocked`: present in the archive catalog already (would be skipped by
//!   migrate dedup);
//! - `unknown`: cannot be read or parsed.
//!
//! Never mutates the source or archive. Mirrors plan-archive's `discover`
//! shape, but the "source" is the agent-out tree (globbing only
//! `skill-usage.record.json`) rather than a plan-folder root.

use std::path::{Path, PathBuf};

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::Serialize;

use crate::record::SkillUsageRecord;
use crate::source::{self, SourceError};

const BINARY: &str = "evidence";
const COMMAND: &str = "discover";

pub struct DispatchArgs {
    pub source_out: Option<PathBuf>,
    pub archive: Option<PathBuf>,
    pub include_unknown: bool,
    pub format: OutputFormat,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Classification {
    Eligible,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub source_path: String,
    pub classification: Classification,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoverSummary {
    pub eligible: usize,
    pub blocked: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoverReport {
    pub source_out: String,
    pub archive: String,
    pub summary: DiscoverSummary,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    #[error("agent-out projects root not found at `{0}`")]
    SourceOutMissing(PathBuf),
    #[error("archive clone path not found at `{0}`")]
    ArchiveCloneMissing(PathBuf),
    #[error("io error during discovery: {0}")]
    Io(String),
}

impl DiscoverError {
    pub fn code(&self) -> &'static str {
        match self {
            DiscoverError::SourceOutMissing(_) => "discover-source-out-missing",
            DiscoverError::ArchiveCloneMissing(_) => "discover-archive-clone-missing",
            DiscoverError::Io(_) => "discover-io-error",
        }
    }
}

impl From<SourceError> for DiscoverError {
    fn from(err: SourceError) -> Self {
        match err {
            SourceError::SourceOutMissing(p) => DiscoverError::SourceOutMissing(p),
            SourceError::ArchiveCloneMissing(p) => DiscoverError::ArchiveCloneMissing(p),
            other => DiscoverError::Io(other.to_string()),
        }
    }
}

pub fn dispatch(args: DispatchArgs) -> i32 {
    let format = args.format;
    match run(&args) {
        Ok(report) => emit(format, &report),
        Err(err) => emit_error(format, err.code(), &err.to_string()),
    }
}

pub fn run(args: &DispatchArgs) -> Result<DiscoverReport, DiscoverError> {
    let source_out = source::resolve_source_out(args.source_out.as_deref())?;
    let archive = source::resolve_archive(args.archive.as_deref())?;
    let existing = crate::catalog::existing_source_digests(&archive)
        .map_err(|e| DiscoverError::Io(e.to_string()))?;

    let mut candidates = Vec::new();
    let mut eligible = 0;
    let mut blocked = 0;
    let mut unknown = 0;

    for record_path in enumerate_records(&source_out) {
        let source_path = record_path.display().to_string();
        let raw = match std::fs::read(&record_path) {
            Ok(b) => b,
            Err(e) => {
                unknown += 1;
                candidates.push(Candidate {
                    source_path,
                    classification: Classification::Unknown,
                    skill: None,
                    source_digest: None,
                    reason: Some(format!("read failed: {e}")),
                });
                continue;
            }
        };
        let record = match SkillUsageRecord::from_json_bytes(&raw) {
            Ok(r) => r,
            Err(e) => {
                unknown += 1;
                candidates.push(Candidate {
                    source_path,
                    classification: Classification::Unknown,
                    skill: None,
                    source_digest: None,
                    reason: Some(e),
                });
                continue;
            }
        };
        let digest = format!("sha256:{}", sha256_hex(&raw));
        if existing.contains(&digest) {
            blocked += 1;
            candidates.push(Candidate {
                source_path,
                classification: Classification::Blocked,
                skill: Some(record.skill),
                source_digest: Some(digest),
                reason: Some("already archived (catalog)".to_string()),
            });
        } else {
            eligible += 1;
            candidates.push(Candidate {
                source_path,
                classification: Classification::Eligible,
                skill: Some(record.skill),
                source_digest: Some(digest),
                reason: None,
            });
        }
    }

    if !args.include_unknown {
        candidates.retain(|c| c.classification != Classification::Unknown);
    }

    Ok(DiscoverReport {
        source_out: source_out.display().to_string(),
        archive: archive.display().to_string(),
        summary: DiscoverSummary {
            eligible,
            blocked,
            unknown,
        },
        candidates,
    })
}

fn enumerate_records(source_out: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(source_out) else {
        return out;
    };
    for project in projects.flatten() {
        let project_path = project.path();
        if !project_path.is_dir() {
            continue;
        }
        let Ok(runs) = std::fs::read_dir(&project_path) else {
            continue;
        };
        for run in runs.flatten() {
            let run_path = run.path();
            if !run_path.is_dir() {
                continue;
            }
            let candidate = run_path.join("skill-usage.record.json");
            if candidate.is_file() {
                out.push(candidate);
            }
        }
    }
    out.sort();
    out
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

fn emit(format: OutputFormat, report: &DiscoverReport) -> i32 {
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
            println!(
                "discover: {} eligible, {} blocked, {} unknown",
                report.summary.eligible, report.summary.blocked, report.summary.unknown
            );
            for c in &report.candidates {
                let label = match c.classification {
                    Classification::Eligible => "eligible",
                    Classification::Blocked => "blocked",
                    Classification::Unknown => "unknown",
                };
                println!("  [{label}] {}", c.source_path);
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

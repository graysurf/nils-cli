//! `plan-archive query` — read and aggregate `_index/` snapshots and
//! traverse archived-plan ↔ ref links.
//!
//! Sprint 5 of Plan 1. Query is read-only: it never fetches, never
//! writes, and never commits. Three modes, mutually exclusive at the
//! CLI:
//!
//! - single-ref (`--ref`): latest snapshot for one issue/PR/MR.
//! - aggregate (`--host`/`--org`/`--repo`/`--since`): every matching
//!   ref's latest snapshot.
//! - link traversal (`--plan` / `--refs-from`): the refs an archived
//!   plan points at, each resolved to its latest snapshot.

use std::fs;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::Serialize;

use crate::refresh::refparse::{RefKind, parse_ref_url};

pub mod index;
pub mod scan;

pub use index::{IndexEntry, parse_index_path};

const COMMAND: &str = "query";
const BINARY: &str = "plan-archive";

/// Args forwarded from `cli::run`.
pub struct DispatchArgs {
    pub r#ref: Option<String>,
    pub host: Option<String>,
    pub org: Option<String>,
    pub repo: Option<String>,
    pub since: Option<String>,
    pub plan: Option<String>,
    pub refs_from: Option<String>,
    pub archive: Option<PathBuf>,
    pub format: OutputFormat,
}

/// A single snapshot record returned by a query.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SnapshotRecord {
    pub r#ref: String,
    pub host: String,
    pub org_or_group_path: String,
    pub repo: String,
    pub kind: RefKind,
    pub number: u64,
    /// Latest snapshot path relative to the archive root, or `None`
    /// when the ref folder exists in a link but has no snapshots yet.
    pub latest_snapshot: Option<String>,
    /// `fetched_at` in ISO8601 (with colons), derived from the
    /// latest snapshot filename. `None` when no snapshots exist.
    pub fetched_at: Option<String>,
}

/// Top-level query result envelope payload.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum QueryResult {
    SingleRef {
        record: SnapshotRecord,
    },
    Aggregate {
        records: Vec<SnapshotRecord>,
    },
    PlanLink {
        plan: String,
        records: Vec<SnapshotRecord>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("no query selector supplied; pass `--ref`, a filter, `--plan`, or `--refs-from`")]
    NoSelector,
    #[error("`--ref` value `{0}` is not a parseable issue/PR/MR URL")]
    UnparseableRef(String),
    #[error("archive clone path not found at `{0}`")]
    ArchiveCloneMissing(PathBuf),
    #[error("invalid `--since` value `{0}`; expected YYYY-MM-DD")]
    InvalidSince(String),
    #[error("metadata file `{0}` not found")]
    MetadataNotFound(String),
    #[error("failed to read metadata `{0}`: {1}")]
    MetadataReadFailed(String, String),
    #[error("metadata `{0}` carries no refs to traverse")]
    MetadataNoRefs(String),
    #[error("io error during query: {0}")]
    Io(String),
}

impl QueryError {
    pub fn code(&self) -> &'static str {
        match self {
            QueryError::NoSelector => "query-no-selector",
            QueryError::UnparseableRef(_) => "query-unparseable-ref",
            QueryError::ArchiveCloneMissing(_) => "query-archive-clone-missing",
            QueryError::InvalidSince(_) => "query-invalid-since",
            QueryError::MetadataNotFound(_) => "query-metadata-not-found",
            QueryError::MetadataReadFailed(_, _) => "query-metadata-read-failed",
            QueryError::MetadataNoRefs(_) => "query-metadata-no-refs",
            QueryError::Io(_) => "query-io-error",
        }
    }
}

/// Entry point from `cli::run`.
pub fn dispatch(args: DispatchArgs) -> i32 {
    let format = args.format;
    match run(&args) {
        Ok(result) => emit(format, &result),
        Err(err) => emit_error(format, err.code(), &err.to_string()),
    }
}

/// Core read-only routine.
pub fn run(args: &DispatchArgs) -> Result<QueryResult, QueryError> {
    let archive = resolve_archive(args.archive.as_deref())?;

    if let Some(ref_url) = &args.r#ref {
        let target =
            parse_ref_url(ref_url).ok_or_else(|| QueryError::UnparseableRef(ref_url.clone()))?;
        let entry = IndexEntry {
            host: target.host,
            org_or_group_path: target.org_or_group_path,
            repo: target.repo,
            kind: target.kind,
            number: target.number,
        };
        let record = record_for(&archive, &entry);
        return Ok(QueryResult::SingleRef { record });
    }

    if let Some(plan) = &args.plan {
        let metadata_path = archive.join(plan).join("metadata.yaml");
        let records = traverse_metadata(&archive, &metadata_path, plan)?;
        return Ok(QueryResult::PlanLink {
            plan: plan.clone(),
            records,
        });
    }

    if let Some(refs_from) = &args.refs_from {
        let metadata_path = PathBuf::from(refs_from);
        let records = traverse_metadata(&archive, &metadata_path, refs_from)?;
        return Ok(QueryResult::PlanLink {
            plan: refs_from.clone(),
            records,
        });
    }

    // Aggregate mode (filters, all optional).
    if args.host.is_some() || args.org.is_some() || args.repo.is_some() || args.since.is_some() {
        if let Some(since) = &args.since {
            validate_since(since)?;
        }
        let records = aggregate(&archive, args)?;
        return Ok(QueryResult::Aggregate { records });
    }

    Err(QueryError::NoSelector)
}

fn record_for(archive: &Path, entry: &IndexEntry) -> SnapshotRecord {
    let dir = archive.join(entry.index_dir());
    let (latest_snapshot, fetched_at) = latest_snapshot_in(&dir, entry);
    SnapshotRecord {
        r#ref: entry.canonical_url(),
        host: entry.host.clone(),
        org_or_group_path: entry.org_or_group_path.clone(),
        repo: entry.repo.clone(),
        kind: entry.kind,
        number: entry.number,
        latest_snapshot,
        fetched_at,
    }
}

/// Find the lexically-last `*.json` snapshot in `dir`, returning its
/// archive-relative path and decoded `fetched_at`.
fn latest_snapshot_in(dir: &Path, _entry: &IndexEntry) -> (Option<String>, Option<String>) {
    let Ok(read) = fs::read_dir(dir) else {
        return (None, None);
    };
    let mut stamps: Vec<String> = read
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".json"))
        .collect();
    stamps.sort();
    let Some(latest) = stamps.last() else {
        return (None, None);
    };
    let stamp = latest.trim_end_matches(".json");
    let fetched_at = decode_basic_stamp(stamp);
    let rel = dir.join(latest);
    (Some(rel.display().to_string()), fetched_at)
}

/// Decode a basic-format `YYYYMMDDThhmmssZ` stamp back into an
/// extended ISO8601 `YYYY-MM-DDThh:mm:ssZ`. Returns the raw stamp
/// when it does not match the expected shape.
pub fn decode_basic_stamp(stamp: &str) -> Option<String> {
    let bytes = stamp.as_bytes();
    if bytes.len() == 16 && bytes[8] == b'T' && bytes[15] == b'Z' {
        let digits =
            |range: std::ops::Range<usize>| stamp[range].chars().all(|c| c.is_ascii_digit());
        if digits(0..8) && digits(9..15) {
            return Some(format!(
                "{}-{}-{}T{}:{}:{}Z",
                &stamp[0..4],
                &stamp[4..6],
                &stamp[6..8],
                &stamp[9..11],
                &stamp[11..13],
                &stamp[13..15],
            ));
        }
    }
    Some(stamp.to_string())
}

fn aggregate(archive: &Path, args: &DispatchArgs) -> Result<Vec<SnapshotRecord>, QueryError> {
    let index_root = archive.join("_index");
    let entries = index::walk_index(&index_root).map_err(|e| QueryError::Io(e.to_string()))?;
    let mut records = Vec::new();
    for entry in entries {
        if let Some(h) = &args.host
            && &entry.host != h
        {
            continue;
        }
        if let Some(o) = &args.org
            && &entry.org_or_group_path != o
        {
            continue;
        }
        if let Some(r) = &args.repo
            && &entry.repo != r
        {
            continue;
        }
        let record = record_for(archive, &entry);
        if let Some(since) = &args.since {
            // Compare basic stamps lexically: derive the latest
            // stamp from the record's fetched_at.
            match &record.fetched_at {
                Some(fetched) if iso_date_part(fetched) >= *since => {}
                _ => continue,
            }
        }
        records.push(record);
    }
    records.sort_by(|a, b| {
        (&a.host, &a.org_or_group_path, &a.repo, a.number).cmp(&(
            &b.host,
            &b.org_or_group_path,
            &b.repo,
            b.number,
        ))
    });
    Ok(records)
}

fn iso_date_part(iso: &str) -> String {
    iso.split('T').next().unwrap_or(iso).to_string()
}

fn traverse_metadata(
    archive: &Path,
    metadata_path: &Path,
    label: &str,
) -> Result<Vec<SnapshotRecord>, QueryError> {
    if !metadata_path.is_file() {
        return Err(QueryError::MetadataNotFound(label.to_string()));
    }
    let raw = fs::read_to_string(metadata_path)
        .map_err(|e| QueryError::MetadataReadFailed(label.to_string(), e.to_string()))?;
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw)
        .map_err(|e| QueryError::MetadataReadFailed(label.to_string(), e.to_string()))?;

    let refs = value.get("refs");
    let mut urls = Vec::new();
    if let Some(refs) = refs {
        for key in ["issue", "pr", "mr"] {
            if let Some(v) = refs.get(key).and_then(|v| v.as_str()) {
                urls.push(v.to_string());
            }
        }
    }
    if urls.is_empty() {
        return Err(QueryError::MetadataNoRefs(label.to_string()));
    }

    let mut records = Vec::new();
    for url in urls {
        if let Some(target) = parse_ref_url(&url) {
            let entry = IndexEntry {
                host: target.host,
                org_or_group_path: target.org_or_group_path,
                repo: target.repo,
                kind: target.kind,
                number: target.number,
            };
            records.push(record_for(archive, &entry));
        }
    }
    Ok(records)
}

fn validate_since(s: &str) -> Result<(), QueryError> {
    let parts: Vec<&str> = s.split('-').collect();
    let ok = parts.len() == 3
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()));
    if ok {
        Ok(())
    } else {
        Err(QueryError::InvalidSince(s.to_string()))
    }
}

fn resolve_archive(arg: Option<&Path>) -> Result<PathBuf, QueryError> {
    let candidate = match arg {
        Some(p) => p.to_path_buf(),
        None => default_archive_clone_path()?,
    };
    if !candidate.is_dir() {
        return Err(QueryError::ArchiveCloneMissing(candidate));
    }
    Ok(candidate)
}

fn default_archive_clone_path() -> Result<PathBuf, QueryError> {
    let local = crate::validate::local::validate_local_path(&local_config_path())
        .map_err(|e| QueryError::Io(e.to_string()))?;
    Ok(local.data.config.archive_clone_path)
}

fn local_config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("PLAN_ARCHIVE_LOCAL_CONFIG") {
        return PathBuf::from(p);
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg)
            .join("agent-plan-archive")
            .join("config.yaml");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("agent-plan-archive")
            .join("config.yaml");
    }
    PathBuf::from("/nonexistent/agent-plan-archive/config.yaml")
}

fn emit(format: OutputFormat, result: &QueryResult) -> i32 {
    match format {
        OutputFormat::Json => {
            let envelope = Envelope::success(schema_version_for(BINARY, COMMAND, 1), result);
            match serde_json::to_string(&envelope) {
                Ok(s) => {
                    println!("{s}");
                    exit::SUCCESS
                }
                Err(_) => exit::SOFTWARE,
            }
        }
        OutputFormat::Text => {
            match result {
                QueryResult::SingleRef { record } => print_record(record),
                QueryResult::Aggregate { records } => {
                    if records.is_empty() {
                        println!("no matching snapshots");
                    }
                    for r in records {
                        print_record(r);
                    }
                }
                QueryResult::PlanLink { plan, records } => {
                    println!("plan {plan} → {} ref(s)", records.len());
                    for r in records {
                        print_record(r);
                    }
                }
            }
            exit::SUCCESS
        }
    }
}

fn print_record(r: &SnapshotRecord) {
    match (&r.latest_snapshot, &r.fetched_at) {
        (Some(path), Some(at)) => {
            println!("{}  fetched_at={}  {}", r.r#ref, at, path);
        }
        _ => {
            println!("{}  (no snapshots)", r.r#ref);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_stamp_extended() {
        assert_eq!(
            decode_basic_stamp("20260527T013045Z").as_deref(),
            Some("2026-05-27T01:30:45Z")
        );
    }

    #[test]
    fn decode_stamp_passthrough_on_garbage() {
        assert_eq!(decode_basic_stamp("weird").as_deref(), Some("weird"));
    }

    #[test]
    fn since_validation() {
        assert!(validate_since("2026-05-27").is_ok());
        assert!(validate_since("2026-5-7").is_err());
    }

    #[test]
    fn iso_date_part_splits_on_t() {
        assert_eq!(iso_date_part("2026-05-27T01:30:45Z"), "2026-05-27");
    }
}

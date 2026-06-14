//! Deterministic derived catalog for archived skill-usage rollups.
//!
//! The catalog is a committed JSON projection built by walking
//! `evidence/**/skill-usage.rollup.json` directly. There is no `_index/`
//! provider-snapshot tree (skill-usage rollups have no provider refs), so —
//! unlike plan-archive's catalog — generation never reads a snapshot tree.
//!
//! The catalog carries a `source_digest` column so `migrate` can dedup in
//! O(catalog) instead of an O(n) tree walk, plus a `record_schema` column so
//! readers can flag out-of-range rows without re-reading the rollup.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::{Deserialize, Serialize};

const BINARY: &str = "evidence";
const COMMAND: &str = "catalog";
const CATALOG_SCHEMA: &str = "evidence.catalog.v1";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CatalogDocument {
    pub schema_version: String,
    pub records: Vec<CatalogRecord>,
}

/// Flattened projection of a rollup (discrete columns power the filters).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CatalogRecord {
    pub id: String,
    pub host: String,
    pub org: String,
    pub repo: String,
    pub archived_at: String,
    pub date: String,
    pub skill: String,
    pub intent: String,
    pub trigger: String,
    pub outcome_status: String,
    pub outcome_summary: String,
    pub producer_version: String,
    pub validation_count: usize,
    pub failure_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion_case: Option<String>,
    /// Powers O(1) migrate dedup.
    pub source_digest: String,
    /// The rollup's `schema`; lets readers flag out-of-range rows.
    pub record_schema: String,
    /// Relative path to the rollup json.
    pub archive_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CatalogReport {
    pub catalog_path: String,
    pub wrote: bool,
    pub total_records: usize,
    pub records: Vec<CatalogRecord>,
}

pub struct DispatchArgs {
    pub write: bool,
    pub grep: Option<String>,
    pub deep: bool,
    pub outcome: Option<String>,
    pub case_id: Option<String>,
    pub archive: Option<PathBuf>,
    pub format: OutputFormat,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("archive clone path not found at `{0}`")]
    ArchiveCloneMissing(PathBuf),
    #[error("failed to read rollup `{0}`: {1}")]
    RollupReadFailed(String, String),
    #[error("failed to parse rollup `{0}`: {1}")]
    RollupParseFailed(String, String),
    #[error("io error during catalog generation: {0}")]
    Io(String),
}

impl CatalogError {
    pub fn code(&self) -> &'static str {
        match self {
            CatalogError::ArchiveCloneMissing(_) => "catalog-archive-clone-missing",
            CatalogError::RollupReadFailed(_, _) => "catalog-rollup-read-failed",
            CatalogError::RollupParseFailed(_, _) => "catalog-rollup-parse-failed",
            CatalogError::Io(_) => "catalog-io-error",
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

pub fn run(args: &DispatchArgs) -> Result<CatalogReport, CatalogError> {
    let archive = resolve_archive(args.archive.as_deref())?;
    let document = build_document(&archive)?;
    let filtered = filter_records(
        &document.records,
        args.grep.as_deref(),
        args.outcome.as_deref(),
        args.case_id.as_deref(),
        args.deep,
        &archive,
    );
    let catalog_path = archive.join("catalog.json");
    if args.write {
        write_document(&catalog_path, &document)?;
    }
    Ok(CatalogReport {
        catalog_path: catalog_path.display().to_string(),
        wrote: args.write,
        total_records: document.records.len(),
        records: filtered,
    })
}

/// Regenerate and write `<archive>/catalog.json`. Used by `migrate --apply`.
pub fn write_catalog(archive: &Path) -> Result<PathBuf, CatalogError> {
    let document = build_document(archive)?;
    let path = archive.join("catalog.json");
    write_document(&path, &document)?;
    Ok(path)
}

/// Set of `source_digest`s already present in the committed catalog. Powers
/// migrate dedup. A missing catalog yields an empty set (not an error).
pub fn existing_source_digests(archive: &Path) -> Result<BTreeSet<String>, CatalogError> {
    let path = archive.join("catalog.json");
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(CatalogError::Io(e.to_string())),
    };
    let doc: RawCatalog =
        serde_json::from_str(&raw).map_err(|e| CatalogError::Io(format!("catalog parse: {e}")))?;
    Ok(doc
        .records
        .into_iter()
        .filter_map(|r| r.source_digest)
        .collect())
}

/// Build the catalog by walking every rollup. Reads ALL rollups regardless of
/// schema so the row count is honest; the per-row `record_schema` lets readers
/// apply a readable range.
pub fn build_document(archive: &Path) -> Result<CatalogDocument, CatalogError> {
    let mut records = Vec::new();
    let evidence_root = archive.join("evidence");
    if evidence_root.is_dir() {
        let mut rollup_paths = Vec::new();
        collect_rollup_paths(&evidence_root, &mut rollup_paths)?;
        rollup_paths.sort();
        for rollup_path in rollup_paths {
            records.push(record_from_rollup(archive, &rollup_path)?);
        }
    }
    records.sort_by(|a, b| {
        (&a.host, &a.org, &a.repo, &a.date, &a.id).cmp(&(&b.host, &b.org, &b.repo, &b.date, &b.id))
    });
    Ok(CatalogDocument {
        schema_version: CATALOG_SCHEMA.to_string(),
        records,
    })
}

pub fn to_catalog_json(document: &CatalogDocument) -> Result<String, CatalogError> {
    let mut body = serde_json::to_string_pretty(document)
        .map_err(|e| CatalogError::Io(format!("catalog serialize: {e}")))?;
    body.push('\n');
    Ok(body)
}

fn collect_rollup_paths(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), CatalogError> {
    for entry in fs::read_dir(root).map_err(|e| CatalogError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| CatalogError::Io(e.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rollup_paths(&path, out)?;
        } else if path.file_name().and_then(|n| n.to_str()) == Some("skill-usage.rollup.json") {
            out.push(path);
        }
    }
    Ok(())
}

fn record_from_rollup(archive: &Path, rollup_path: &Path) -> Result<CatalogRecord, CatalogError> {
    let label = rollup_path.display().to_string();
    let raw = fs::read_to_string(rollup_path)
        .map_err(|e| CatalogError::RollupReadFailed(label.clone(), e.to_string()))?;
    let rollup: RawRollup = serde_json::from_str(&raw)
        .map_err(|e| CatalogError::RollupParseFailed(label.clone(), e.to_string()))?;

    let archive_path = rollup_path
        .strip_prefix(archive)
        .unwrap_or(rollup_path)
        .display()
        .to_string();
    let archived_at = rollup.archived_at.unwrap_or_default();
    let date = iso_date_part(&archived_at);
    let repo = rollup.repo.unwrap_or_default();
    let outcome = rollup.outcome.unwrap_or_default();
    let counts = rollup.counts.unwrap_or_default();
    let producer_version = rollup
        .producer
        .and_then(|p| p.nils_cli_version)
        .unwrap_or_default();
    let promotion_case = rollup.promotion.and_then(|p| p.heuristic_inbox_case);

    Ok(CatalogRecord {
        id: rollup.id.unwrap_or_default(),
        host: repo.host.unwrap_or_default(),
        org: repo.org.unwrap_or_default(),
        repo: repo.repo.unwrap_or_default(),
        archived_at,
        date,
        skill: rollup.skill.unwrap_or_default(),
        intent: rollup.intent.unwrap_or_default(),
        trigger: rollup.trigger.unwrap_or_default(),
        outcome_status: outcome.status.unwrap_or_default(),
        outcome_summary: outcome.summary.unwrap_or_default(),
        producer_version,
        validation_count: counts.validation,
        failure_count: counts.failures,
        promotion_case,
        source_digest: rollup.source_digest.unwrap_or_default(),
        record_schema: rollup.schema.unwrap_or_default(),
        archive_path,
    })
}

fn iso_date_part(iso: &str) -> String {
    iso.split('T').next().unwrap_or(iso).to_string()
}

/// Filter records by case-insensitive `--grep` (over a wide column set), an
/// `--outcome` status (exact, case-insensitive), and a `--case-id`
/// (promotion case substring). `--deep` widens `--grep` to also match the
/// full-text intent and outcome summary (which the shallow grep already
/// covers here, so `--deep` is accepted for parity and currently a superset).
fn filter_records(
    records: &[CatalogRecord],
    grep: Option<&str>,
    outcome: Option<&str>,
    case_id: Option<&str>,
    _deep: bool,
    _archive: &Path,
) -> Vec<CatalogRecord> {
    let grep = grep.map(|s| s.to_ascii_lowercase());
    let outcome = outcome.map(|s| s.to_ascii_lowercase());
    let case_id = case_id.map(|s| s.to_ascii_lowercase());

    records
        .iter()
        .filter(|r| match grep.as_deref() {
            Some(term) => record_matches_grep(r, term),
            None => true,
        })
        .filter(|r| match outcome.as_deref() {
            Some(want) => r.outcome_status.to_ascii_lowercase() == want,
            None => true,
        })
        .filter(|r| match case_id.as_deref() {
            Some(want) => r
                .promotion_case
                .as_deref()
                .is_some_and(|c| c.to_ascii_lowercase().contains(want)),
            None => true,
        })
        .cloned()
        .collect()
}

fn record_matches_grep(record: &CatalogRecord, term: &str) -> bool {
    let fields = [
        record.id.as_str(),
        record.skill.as_str(),
        record.intent.as_str(),
        record.trigger.as_str(),
        record.outcome_status.as_str(),
        record.outcome_summary.as_str(),
        record.host.as_str(),
        record.org.as_str(),
        record.repo.as_str(),
    ];
    fields
        .iter()
        .any(|field| field.to_ascii_lowercase().contains(term))
}

fn write_document(path: &Path, document: &CatalogDocument) -> Result<(), CatalogError> {
    let body = to_catalog_json(document)?;
    fs::write(path, body).map_err(|e| CatalogError::Io(e.to_string()))
}

fn resolve_archive(arg: Option<&Path>) -> Result<PathBuf, CatalogError> {
    match crate::source::resolve_archive(arg) {
        Ok(p) => Ok(p),
        Err(crate::source::SourceError::ArchiveCloneMissing(p)) => {
            Err(CatalogError::ArchiveCloneMissing(p))
        }
        Err(e) => Err(CatalogError::Io(e.to_string())),
    }
}

fn emit(format: OutputFormat, report: &CatalogReport) -> i32 {
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
            if report.wrote {
                println!(
                    "catalog written: {} ({} record(s))",
                    report.catalog_path, report.total_records
                );
            }
            if report.records.is_empty() {
                println!("no matching archived rollups");
            }
            for record in &report.records {
                println!(
                    "{}  {}/{}/{}  {}  outcome={}",
                    record.id,
                    record.host,
                    record.org,
                    record.repo,
                    record.skill,
                    record.outcome_status
                );
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

// --- Raw rollup deserialization (tolerant; mirrors the written shape) ---

#[derive(Debug, Deserialize)]
struct RawCatalog {
    #[serde(default)]
    records: Vec<RawCatalogRow>,
}

#[derive(Debug, Deserialize)]
struct RawCatalogRow {
    #[serde(default)]
    source_digest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawRollup {
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    archived_at: Option<String>,
    #[serde(default)]
    skill: Option<String>,
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    trigger: Option<String>,
    #[serde(default)]
    repo: Option<RawRepo>,
    #[serde(default)]
    outcome: Option<RawOutcome>,
    #[serde(default)]
    producer: Option<RawProducer>,
    #[serde(default)]
    counts: Option<RawCounts>,
    #[serde(default)]
    source_digest: Option<String>,
    #[serde(default)]
    promotion: Option<RawPromotion>,
}

#[derive(Debug, Default, Deserialize)]
struct RawRepo {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    org: Option<String>,
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawOutcome {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    summary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawProducer {
    #[serde(default)]
    nils_cli_version: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawCounts {
    #[serde(default)]
    validation: usize,
    #[serde(default)]
    failures: usize,
}

#[derive(Debug, Deserialize)]
struct RawPromotion {
    #[serde(default)]
    heuristic_inbox_case: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_rollup(
        archive: &Path,
        rel_dir: &str,
        id: &str,
        schema: &str,
        status: &str,
        digest: &str,
    ) {
        let dir = archive.join("evidence").join(rel_dir);
        fs::create_dir_all(&dir).unwrap();
        let body = format!(
            r#"{{
                "schema": "{schema}",
                "id": "{id}",
                "archived_at": "2026-06-14T10:00:00Z",
                "skill": "deliver-pr",
                "intent": "deliver pr",
                "trigger": "user_explicit",
                "repo": {{ "host": "github.com", "org": "graysurf", "repo": "kit" }},
                "outcome": {{ "status": "{status}", "summary": "done with rollback" }},
                "producer": {{ "tool": "skill-usage", "nils_cli_version": "1.4.0" }},
                "counts": {{ "validation": 2, "failures": 0 }},
                "source_digest": "{digest}"
            }}"#
        );
        fs::write(dir.join("skill-usage.rollup.json"), body).unwrap();
    }

    #[test]
    fn build_document_projects_columns() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollup(
            tmp.path(),
            "github.com/graysurf/kit/id1",
            "id1",
            "skill-usage.rollup.v1",
            "pass",
            "sha256:aaa",
        );
        let doc = build_document(tmp.path()).unwrap();
        assert_eq!(doc.schema_version, "evidence.catalog.v1");
        assert_eq!(doc.records.len(), 1);
        let r = &doc.records[0];
        assert_eq!(r.host, "github.com");
        assert_eq!(r.org, "graysurf");
        assert_eq!(r.repo, "kit");
        assert_eq!(r.producer_version, "1.4.0");
        assert_eq!(r.validation_count, 2);
        assert_eq!(r.source_digest, "sha256:aaa");
        assert_eq!(r.record_schema, "skill-usage.rollup.v1");
        assert_eq!(r.date, "2026-06-14");
    }

    #[test]
    fn catalog_generation_is_deterministic_byte_identical() {
        let tmp = tempfile::tempdir().unwrap();
        // Write in non-sorted order.
        write_rollup(
            tmp.path(),
            "github.com/graysurf/kit/idz",
            "idz",
            "skill-usage.rollup.v1",
            "pass",
            "sha256:z",
        );
        write_rollup(
            tmp.path(),
            "github.com/graysurf/kit/ida",
            "ida",
            "skill-usage.rollup.v1",
            "fail",
            "sha256:a",
        );
        let first = to_catalog_json(&build_document(tmp.path()).unwrap()).unwrap();
        let second = to_catalog_json(&build_document(tmp.path()).unwrap()).unwrap();
        assert_eq!(first, second, "catalog must be byte-identical");
        // Sorted by (host,org,repo,date,id): ida before idz.
        let pos_a = first.find("\"ida\"").unwrap();
        let pos_z = first.find("\"idz\"").unwrap();
        assert!(pos_a < pos_z, "records sorted by id");
    }

    #[test]
    fn existing_source_digests_reads_catalog() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollup(
            tmp.path(),
            "github.com/graysurf/kit/id1",
            "id1",
            "skill-usage.rollup.v1",
            "pass",
            "sha256:dedup",
        );
        write_catalog(tmp.path()).unwrap();
        let set = existing_source_digests(tmp.path()).unwrap();
        assert!(set.contains("sha256:dedup"));
    }

    #[test]
    fn existing_source_digests_missing_catalog_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(existing_source_digests(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn filter_by_outcome_and_grep() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollup(
            tmp.path(),
            "github.com/graysurf/kit/idp",
            "idp",
            "skill-usage.rollup.v1",
            "pass",
            "sha256:p",
        );
        write_rollup(
            tmp.path(),
            "github.com/graysurf/kit/idf",
            "idf",
            "skill-usage.rollup.v1",
            "fail",
            "sha256:f",
        );
        let records = build_document(tmp.path()).unwrap().records;
        let only_pass = filter_records(&records, None, Some("pass"), None, false, tmp.path());
        assert_eq!(only_pass.len(), 1);
        assert_eq!(only_pass[0].outcome_status, "pass");
        // grep matches the outcome_summary "done with rollback"
        let matched = filter_records(&records, Some("rollback"), None, None, false, tmp.path());
        assert_eq!(matched.len(), 2);
        let unmatched = filter_records(
            &records,
            Some("nonexistent-term"),
            None,
            None,
            false,
            tmp.path(),
        );
        assert!(unmatched.is_empty());
    }
}

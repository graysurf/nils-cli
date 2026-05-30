//! Deterministic derived catalog for archived plans.
//!
//! The catalog is a committed text projection built from authoritative
//! `plans/**/metadata.yaml` files plus the latest scrubbed `_index/` snapshots.
//! It is rebuildable and intentionally mirrors the row shape that the future
//! SQLite index will consume.

use std::fs;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::{Deserialize, Serialize};

use crate::query::{IndexEntry, decode_basic_stamp, scan};
use crate::refresh::refparse::{RefKind, RefTarget, parse_ref_url};
use crate::validate;

const BINARY: &str = "plan-archive";
const COMMAND: &str = "catalog";
const CATALOG_SCHEMA: &str = "plan-archive.catalog.v1";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CatalogDocument {
    pub schema_version: String,
    pub records: Vec<CatalogRecord>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CatalogRecord {
    pub slug: String,
    pub host: String,
    pub org: String,
    pub repo: String,
    pub date: String,
    pub original_path: String,
    pub archive_commit: String,
    pub title: String,
    pub summary: String,
    pub area: Vec<String>,
    pub refs: Vec<CatalogRef>,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CatalogRef {
    pub url: String,
    pub kind: RefKind,
    pub number: u64,
    pub state: String,
    pub title: String,
    pub latest_snapshot: Option<String>,
    pub fetched_at: Option<String>,
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
    pub area: Option<String>,
    pub refs_to: Option<String>,
    /// Extend `--grep` to also match issue / PR / MR body and comment
    /// text (via the latest snapshot), not just catalog metadata.
    pub deep: bool,
    pub archive: Option<PathBuf>,
    pub format: OutputFormat,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("archive clone path not found at `{0}`")]
    ArchiveCloneMissing(PathBuf),
    #[error("`--refs-to` value `{0}` is not a parseable issue/PR/MR URL")]
    UnparseableRef(String),
    #[error("failed to read plan metadata `{0}`: {1}")]
    MetadataReadFailed(String, String),
    #[error("failed to parse plan metadata `{0}`: {1}")]
    MetadataParseFailed(String, String),
    #[error("plan metadata `{0}` is missing required source field `{1}`")]
    MetadataMissingSourceField(String, &'static str),
    #[error("io error during catalog generation: {0}")]
    Io(String),
}

impl CatalogError {
    pub fn code(&self) -> &'static str {
        match self {
            CatalogError::ArchiveCloneMissing(_) => "catalog-archive-clone-missing",
            CatalogError::UnparseableRef(_) => "catalog-unparseable-ref",
            CatalogError::MetadataReadFailed(_, _) => "catalog-metadata-read-failed",
            CatalogError::MetadataParseFailed(_, _) => "catalog-metadata-parse-failed",
            CatalogError::MetadataMissingSourceField(_, _) => "catalog-metadata-missing-source",
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
        args.area.as_deref(),
        args.refs_to.as_deref(),
        args.deep,
        &archive,
    )?;
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

pub fn write_catalog(archive: &Path) -> Result<PathBuf, CatalogError> {
    let document = build_document(archive)?;
    let path = archive.join("catalog.json");
    write_document(&path, &document)?;
    Ok(path)
}

pub fn build_document(archive: &Path) -> Result<CatalogDocument, CatalogError> {
    let mut records = Vec::new();
    let plans_root = archive.join("plans");
    if plans_root.is_dir() {
        let mut metadata_paths = Vec::new();
        collect_metadata_paths(&plans_root, &mut metadata_paths)?;
        metadata_paths.sort();
        for metadata_path in metadata_paths {
            records.push(record_from_metadata(archive, &metadata_path)?);
        }
    }
    records.sort_by(|a, b| {
        (&a.host, &a.org, &a.repo, &a.date, &a.slug)
            .cmp(&(&b.host, &b.org, &b.repo, &b.date, &b.slug))
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

fn collect_metadata_paths(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), CatalogError> {
    for entry in fs::read_dir(root).map_err(|e| CatalogError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| CatalogError::Io(e.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_metadata_paths(&path, out)?;
        } else if path.file_name().and_then(|n| n.to_str()) == Some("metadata.yaml") {
            out.push(path);
        }
    }
    Ok(())
}

fn record_from_metadata(
    archive: &Path,
    metadata_path: &Path,
) -> Result<CatalogRecord, CatalogError> {
    let label = metadata_path.display().to_string();
    let raw = fs::read_to_string(metadata_path)
        .map_err(|e| CatalogError::MetadataReadFailed(label.clone(), e.to_string()))?;
    let metadata: RawMetadata = serde_yaml_ng::from_str(&raw)
        .map_err(|e| CatalogError::MetadataParseFailed(label.clone(), e.to_string()))?;
    let source = metadata
        .source
        .ok_or(CatalogError::MetadataMissingSourceField(
            label.clone(),
            "source",
        ))?;
    let host = required_source_field(&label, "host", source.host)?;
    let org = required_source_field(&label, "org_or_group_path", source.org_or_group_path)?;
    let repo = required_source_field(&label, "repo", source.repo)?;
    let original_path = required_source_field(&label, "original_path", source.original_path)?;
    let archive_commit = required_source_field(&label, "archive_commit", source.archive_commit)?;
    let slug = metadata_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let date = date_from_slug(&slug);

    let mut refs = Vec::new();
    if let Some(raw_refs) = metadata.refs {
        for url in raw_refs.urls() {
            if let Some(target) = parse_ref_url(&url) {
                refs.push(catalog_ref_for(archive, target));
            }
        }
    }
    refs.sort_by(|a, b| (&a.url, a.number).cmp(&(&b.url, b.number)));
    let title = refs
        .iter()
        .find_map(|r| non_empty(&r.title))
        .unwrap_or_else(|| slug.clone());

    Ok(CatalogRecord {
        slug,
        host,
        org,
        repo,
        date,
        original_path,
        archive_commit,
        title,
        summary: String::new(),
        area: Vec::new(),
        refs,
        files: Vec::new(),
    })
}

fn required_source_field(
    label: &str,
    field: &'static str,
    value: Option<String>,
) -> Result<String, CatalogError> {
    match value {
        Some(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(CatalogError::MetadataMissingSourceField(
            label.to_string(),
            field,
        )),
    }
}

fn catalog_ref_for(archive: &Path, target: RefTarget) -> CatalogRef {
    let url = target.canonical_url();
    let (latest_snapshot, fetched_at, snapshot_value) = latest_snapshot(archive, &target);
    let title = snapshot_string(&snapshot_value, "title").unwrap_or_default();
    let state = snapshot_string(&snapshot_value, "state").unwrap_or_default();
    CatalogRef {
        url,
        kind: target.kind,
        number: target.number,
        state,
        title,
        latest_snapshot,
        fetched_at,
    }
}

fn latest_snapshot(
    archive: &Path,
    target: &RefTarget,
) -> (Option<String>, Option<String>, Option<serde_json::Value>) {
    let rel_dir = target.index_dir();
    let dir = archive.join(&rel_dir);
    let Ok(read) = fs::read_dir(&dir) else {
        return (None, None, None);
    };
    let mut files: Vec<String> = read
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".json"))
        .collect();
    files.sort();
    let Some(file_name) = files.last() else {
        return (None, None, None);
    };
    let stamp = file_name.trim_end_matches(".json");
    let fetched_at = decode_basic_stamp(stamp);
    let rel = rel_dir.join(file_name);
    let value = fs::read_to_string(archive.join(&rel))
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok());
    (Some(rel.display().to_string()), fetched_at, value)
}

fn snapshot_string(value: &Option<serde_json::Value>, key: &str) -> Option<String> {
    let value = value.as_ref()?;
    value
        .get("data")
        .and_then(|data| data.get(key))
        .or_else(|| value.get(key))
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
}

fn filter_records(
    records: &[CatalogRecord],
    grep: Option<&str>,
    area: Option<&str>,
    refs_to: Option<&str>,
    deep: bool,
    archive: &Path,
) -> Result<Vec<CatalogRecord>, CatalogError> {
    let grep = grep.map(|s| s.to_ascii_lowercase());
    let refs_to = refs_to
        .map(|url| {
            parse_ref_url(url)
                .map(|target| target.canonical_url())
                .ok_or_else(|| CatalogError::UnparseableRef(url.to_string()))
        })
        .transpose()?;

    Ok(records
        .iter()
        .filter(|record| match grep.as_deref() {
            Some(term) => {
                record_matches_grep(record, term)
                    || (deep && record_matches_deep(archive, record, term))
            }
            None => true,
        })
        .filter(|record| match area {
            Some(area) => record.area.iter().any(|a| a == area),
            None => true,
        })
        .filter(|record| match refs_to.as_deref() {
            Some(url) => record.refs.iter().any(|r| r.url == url),
            None => true,
        })
        .cloned()
        .collect())
}

fn record_matches_grep(record: &CatalogRecord, term: &str) -> bool {
    let fields = [
        record.slug.as_str(),
        record.title.as_str(),
        record.summary.as_str(),
        record.original_path.as_str(),
        record.host.as_str(),
        record.org.as_str(),
        record.repo.as_str(),
    ];
    fields
        .iter()
        .any(|field| field.to_ascii_lowercase().contains(term))
        || record.refs.iter().any(|r| {
            r.url.to_ascii_lowercase().contains(term)
                || r.title.to_ascii_lowercase().contains(term)
                || r.state.to_ascii_lowercase().contains(term)
        })
}

/// Does any of the record's refs' latest snapshot match `term` in its
/// body or comments? Backs `catalog --deep`. A snapshot read error for
/// one ref is treated as "no match" for that ref so a single unreadable
/// snapshot never aborts the whole filter.
fn record_matches_deep(archive: &Path, record: &CatalogRecord, term: &str) -> bool {
    record.refs.iter().any(|r| {
        let entry = IndexEntry {
            host: record.host.clone(),
            org_or_group_path: record.org.clone(),
            repo: record.repo.clone(),
            kind: r.kind,
            number: r.number,
        };
        scan::entry_matches(archive, &entry, term).unwrap_or(false)
    })
}

fn write_document(path: &Path, document: &CatalogDocument) -> Result<(), CatalogError> {
    let body = to_catalog_json(document)?;
    fs::write(path, body).map_err(|e| CatalogError::Io(e.to_string()))
}

fn date_from_slug(slug: &str) -> String {
    let candidate = slug.get(0..10).unwrap_or_default();
    let parts: Vec<&str> = candidate.split('-').collect();
    if parts.len() == 3
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        candidate.to_string()
    } else {
        String::new()
    }
}

fn non_empty(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn resolve_archive(arg: Option<&Path>) -> Result<PathBuf, CatalogError> {
    let candidate = match arg {
        Some(p) => p.to_path_buf(),
        None => default_archive_clone_path()?,
    };
    if !candidate.is_dir() {
        return Err(CatalogError::ArchiveCloneMissing(candidate));
    }
    Ok(candidate)
}

fn default_archive_clone_path() -> Result<PathBuf, CatalogError> {
    let local = validate::local::validate_local_path(&local_config_path())
        .map_err(|e| CatalogError::Io(e.to_string()))?;
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
                println!("no matching archived plans");
            }
            for record in &report.records {
                println!(
                    "{}  {}/{}/{}  refs={}",
                    record.slug,
                    record.host,
                    record.org,
                    record.repo,
                    record.refs.len()
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

#[derive(Debug, Deserialize)]
struct RawMetadata {
    #[serde(default)]
    source: Option<RawSource>,
    #[serde(default)]
    refs: Option<RawRefs>,
}

#[derive(Debug, Deserialize)]
struct RawSource {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    org_or_group_path: Option<String>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    archive_commit: Option<String>,
    #[serde(default)]
    original_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawRefs {
    #[serde(default)]
    issue: Option<String>,
    #[serde(default)]
    pr: Option<String>,
    #[serde(default)]
    mr: Option<String>,
}

impl RawRefs {
    fn urls(self) -> Vec<String> {
        [self.issue, self.pr, self.mr]
            .into_iter()
            .flatten()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_from_slug_reads_date_prefix() {
        assert_eq!(
            date_from_slug("2026-05-27-plan-archive-search-layer"),
            "2026-05-27"
        );
        assert_eq!(date_from_slug("undated-plan"), "");
    }

    #[test]
    fn grep_matches_ref_title() {
        let record = CatalogRecord {
            slug: "s".into(),
            host: "github.com".into(),
            org: "o".into(),
            repo: "r".into(),
            date: "2026-05-27".into(),
            original_path: "docs/plans/s/".into(),
            archive_commit: "abc".into(),
            title: "fallback".into(),
            summary: String::new(),
            area: vec!["archive".into()],
            refs: vec![CatalogRef {
                url: "https://github.com/o/r/issues/1".into(),
                kind: RefKind::Issue,
                number: 1,
                state: "open".into(),
                title: "Search layer".into(),
                latest_snapshot: None,
                fetched_at: None,
            }],
            files: Vec::new(),
        };

        let no_archive = std::path::Path::new("/nonexistent");
        assert_eq!(
            filter_records(
                std::slice::from_ref(&record),
                Some("search"),
                None,
                None,
                false,
                no_archive,
            )
            .unwrap(),
            vec![record.clone()]
        );
        assert_eq!(
            filter_records(
                std::slice::from_ref(&record),
                None,
                Some("archive"),
                None,
                false,
                no_archive,
            )
            .unwrap(),
            vec![record.clone()]
        );
        assert!(
            filter_records(&[record], None, Some("other"), None, false, no_archive)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn deep_grep_matches_body_only_term() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let entry = IndexEntry {
            host: "github.com".into(),
            org_or_group_path: "graysurf".into(),
            repo: "agent-runtime-kit".into(),
            kind: RefKind::Issue,
            number: 55,
        };
        let dir = archive.join(entry.index_dir());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("20260527T052454Z.json"),
            r#"{"data":{"body":"steps after live acceptance with rollback proven","comments":[]}}"#,
        )
        .unwrap();

        let record = CatalogRecord {
            slug: "cutover".into(),
            host: "github.com".into(),
            org: "graysurf".into(),
            repo: "agent-runtime-kit".into(),
            date: "2026-05-23".into(),
            original_path: "docs/plans/cutover/".into(),
            archive_commit: "abc".into(),
            title: "plain title".into(),
            summary: String::new(),
            area: vec!["archive".into()],
            refs: vec![CatalogRef {
                url: "https://github.com/graysurf/agent-runtime-kit/issues/55".into(),
                kind: RefKind::Issue,
                number: 55,
                state: "closed".into(),
                title: "no keyword here".into(),
                latest_snapshot: None,
                fetched_at: None,
            }],
            files: Vec::new(),
        };

        // Shallow grep misses a body-only term.
        assert!(
            filter_records(
                std::slice::from_ref(&record),
                Some("rollback"),
                None,
                None,
                false,
                archive,
            )
            .unwrap()
            .is_empty()
        );
        // --deep finds it.
        assert_eq!(
            filter_records(
                std::slice::from_ref(&record),
                Some("rollback"),
                None,
                None,
                true,
                archive,
            )
            .unwrap(),
            vec![record.clone()]
        );
        // --deep composes with a matching --area...
        assert_eq!(
            filter_records(
                std::slice::from_ref(&record),
                Some("rollback"),
                Some("archive"),
                None,
                true,
                archive,
            )
            .unwrap(),
            vec![record.clone()]
        );
        // ...and a non-matching --area still excludes it.
        assert!(
            filter_records(
                &[record],
                Some("rollback"),
                Some("other"),
                None,
                true,
                archive
            )
            .unwrap()
            .is_empty()
        );
    }
}

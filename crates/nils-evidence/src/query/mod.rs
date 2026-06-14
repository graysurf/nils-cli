//! `evidence query` — filtered read over archived rollups.
//!
//! Reads every `evidence/**/skill-usage.rollup.json`, applies the requested
//! filters, and reports the readable matches. Cross-version handling
//! (Decision 6): each rollup's `schema` is checked against an explicit
//! readable range; a rollup whose schema is out of range is **reported** as a
//! warning and **excluded** from results, never silently dropped. The JSON
//! envelope carries `records[]`, `warnings[]`, and a `{scanned, returned,
//! out_of_range}` count triple. The triple is an honest funnel: `scanned`
//! counts every rollup parsed, `out_of_range` counts the unreadable-schema
//! rollups (reported independent of any content filter), and `returned`
//! counts the in-range rollups that survive the content filters — so
//! `scanned >= returned + out_of_range` always holds.

use std::fs;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::{Deserialize, Serialize};

pub mod index;

pub use index::decode_basic_stamp;

const BINARY: &str = "evidence";
const COMMAND: &str = "query";

/// Exact-version readable range. No implicit future-minor compat: a rollup
/// must declare one of these schemas to appear in results.
pub const READABLE_SCHEMA_RANGE: &[&str] = &["skill-usage.rollup.v1"];

pub struct DispatchArgs {
    pub skill: Option<String>,
    pub outcome: Option<String>,
    pub repo: Option<String>,
    pub host: Option<String>,
    pub org: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub archive: Option<PathBuf>,
    pub format: OutputFormat,
}

/// One queried rollup, flattened for output.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct QueryRecord {
    pub id: String,
    pub schema: String,
    pub host: String,
    pub org: String,
    pub repo: String,
    pub skill: String,
    pub intent: String,
    pub outcome_status: String,
    pub started_at: String,
    pub archive_path: String,
}

/// A schema-out-of-range warning.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct QueryWarning {
    pub code: String,
    pub record_id: String,
    pub schema_found: String,
    pub readable_range: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryCounts {
    pub scanned: usize,
    pub returned: usize,
    pub out_of_range: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryReport {
    pub records: Vec<QueryRecord>,
    pub warnings: Vec<QueryWarning>,
    pub counts: QueryCounts,
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("archive clone path not found at `{0}`")]
    ArchiveCloneMissing(PathBuf),
    #[error("failed to read rollup `{0}`: {1}")]
    RollupReadFailed(String, String),
    #[error("failed to parse rollup `{0}`: {1}")]
    RollupParseFailed(String, String),
    #[error("io error during query: {0}")]
    Io(String),
}

impl QueryError {
    pub fn code(&self) -> &'static str {
        match self {
            QueryError::ArchiveCloneMissing(_) => "query-archive-clone-missing",
            QueryError::RollupReadFailed(_, _) => "query-rollup-read-failed",
            QueryError::RollupParseFailed(_, _) => "query-rollup-parse-failed",
            QueryError::Io(_) => "query-io-error",
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

pub fn run(args: &DispatchArgs) -> Result<QueryReport, QueryError> {
    let archive = resolve_archive(args.archive.as_deref())?;
    let mut rollup_paths = Vec::new();
    let evidence_root = archive.join("evidence");
    if evidence_root.is_dir() {
        collect_rollup_paths(&evidence_root, &mut rollup_paths)?;
    }
    rollup_paths.sort();

    let mut records = Vec::new();
    let mut warnings = Vec::new();
    let mut scanned = 0usize;
    let mut out_of_range = 0usize;

    for path in &rollup_paths {
        let label = path.display().to_string();
        let raw = fs::read_to_string(path)
            .map_err(|e| QueryError::RollupReadFailed(label.clone(), e.to_string()))?;
        let rollup: RawRollup = serde_json::from_str(&raw)
            .map_err(|e| QueryError::RollupParseFailed(label.clone(), e.to_string()))?;
        let schema = rollup.schema.clone().unwrap_or_default();
        let id = rollup.id.clone().unwrap_or_default();
        let repo = rollup.repo.clone().unwrap_or_default();

        // F7: `scanned` is a true funnel head — count EVERY rollup parsed,
        // before any filter or range check.
        scanned += 1;

        // F6: the schema range is evaluated FIRST and independently of the
        // content filters (Decision 6: out-of-range rollups are *reported*,
        // never silently dropped). An out-of-range rollup is counted and
        // warned even when it would not match the active content filters, so a
        // `--skill`/`--repo`/etc. filter can never hide an unreadable record.
        if !READABLE_SCHEMA_RANGE.contains(&schema.as_str()) {
            out_of_range += 1;
            warnings.push(QueryWarning {
                code: "record-schema-version-out-of-range".to_string(),
                record_id: id.clone(),
                schema_found: schema.clone(),
                readable_range: READABLE_SCHEMA_RANGE
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                message: format!(
                    "rollup `{id}` declares schema `{schema}` which is outside the readable range; excluded from results"
                ),
            });
            continue;
        }

        // Content filters apply ONLY to in-range records.
        if !matches_filters(args, &rollup, &repo) {
            continue;
        }

        let archive_path = path
            .strip_prefix(&archive)
            .unwrap_or(path)
            .display()
            .to_string();
        let outcome = rollup.outcome.unwrap_or_default();
        records.push(QueryRecord {
            id,
            schema,
            host: repo.host.unwrap_or_default(),
            org: repo.org.unwrap_or_default(),
            repo: repo.repo.unwrap_or_default(),
            skill: rollup.skill.unwrap_or_default(),
            intent: rollup.intent.unwrap_or_default(),
            outcome_status: outcome.status.unwrap_or_default(),
            started_at: rollup.started_at.unwrap_or_default(),
            archive_path,
        });
    }

    records.sort_by(|a, b| {
        (&a.host, &a.org, &a.repo, &a.started_at, &a.id).cmp(&(
            &b.host,
            &b.org,
            &b.repo,
            &b.started_at,
            &b.id,
        ))
    });

    let returned = records.len();
    Ok(QueryReport {
        records,
        warnings,
        counts: QueryCounts {
            scanned,
            returned,
            out_of_range,
        },
    })
}

fn matches_filters(args: &DispatchArgs, rollup: &RawRollup, repo: &RawRepo) -> bool {
    if let Some(skill) = &args.skill
        && !rollup.skill.as_deref().unwrap_or_default().contains(skill)
    {
        return false;
    }
    if let Some(outcome) = &args.outcome {
        let status = rollup
            .outcome
            .as_ref()
            .and_then(|o| o.status.as_deref())
            .unwrap_or_default();
        if !status.eq_ignore_ascii_case(outcome) {
            return false;
        }
    }
    if let Some(repo_filter) = &args.repo
        && repo.repo.as_deref().unwrap_or_default() != repo_filter
    {
        return false;
    }
    if let Some(host) = &args.host
        && repo.host.as_deref().unwrap_or_default() != host
    {
        return false;
    }
    if let Some(org) = &args.org
        && repo.org.as_deref().unwrap_or_default() != org
    {
        return false;
    }
    let day = iso_date_part(rollup.started_at.as_deref().unwrap_or_default());
    if let Some(since) = &args.since
        && day.as_str() < since.as_str()
    {
        return false;
    }
    if let Some(until) = &args.until
        && day.as_str() > until.as_str()
    {
        return false;
    }
    true
}

fn iso_date_part(iso: &str) -> String {
    iso.split('T').next().unwrap_or(iso).to_string()
}

fn collect_rollup_paths(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), QueryError> {
    for entry in fs::read_dir(root).map_err(|e| QueryError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| QueryError::Io(e.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rollup_paths(&path, out)?;
        } else if path.file_name().and_then(|n| n.to_str()) == Some("skill-usage.rollup.json") {
            out.push(path);
        }
    }
    Ok(())
}

fn resolve_archive(arg: Option<&Path>) -> Result<PathBuf, QueryError> {
    match crate::source::resolve_archive(arg) {
        Ok(p) => Ok(p),
        Err(crate::source::SourceError::ArchiveCloneMissing(p)) => {
            Err(QueryError::ArchiveCloneMissing(p))
        }
        Err(e) => Err(QueryError::Io(e.to_string())),
    }
}

fn emit(format: OutputFormat, report: &QueryReport) -> i32 {
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
                "query: {} returned, {} out-of-range, {} scanned",
                report.counts.returned, report.counts.out_of_range, report.counts.scanned
            );
            for r in &report.records {
                println!(
                    "{}  {}/{}/{}  {}  {}",
                    r.id, r.host, r.org, r.repo, r.skill, r.outcome_status
                );
            }
            for w in &report.warnings {
                eprintln!("warning [{}]: {}", w.code, w.message);
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

#[derive(Debug, Clone, Deserialize)]
struct RawRollup {
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    skill: Option<String>,
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    repo: Option<RawRepo>,
    #[serde(default)]
    outcome: Option<RawOutcome>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawRepo {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    org: Option<String>,
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawOutcome {
    #[serde(default)]
    status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_rollup(
        archive: &Path,
        id: &str,
        schema: &str,
        skill: &str,
        status: &str,
        started: &str,
    ) {
        let dir = archive
            .join("evidence")
            .join("github.com/graysurf/kit")
            .join(id);
        fs::create_dir_all(&dir).unwrap();
        let body = format!(
            r#"{{
                "schema": "{schema}",
                "id": "{id}",
                "skill": "{skill}",
                "intent": "x",
                "started_at": "{started}",
                "repo": {{ "host": "github.com", "org": "graysurf", "repo": "kit" }},
                "outcome": {{ "status": "{status}" }}
            }}"#
        );
        fs::write(dir.join("skill-usage.rollup.json"), body).unwrap();
    }

    #[test]
    fn filters_by_skill_and_outcome_and_repo() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollup(
            tmp.path(),
            "id1",
            "skill-usage.rollup.v1",
            "deliver-pr",
            "pass",
            "2026-06-10T10:00:00Z",
        );
        write_rollup(
            tmp.path(),
            "id2",
            "skill-usage.rollup.v1",
            "code-review",
            "fail",
            "2026-06-12T10:00:00Z",
        );

        let args = DispatchArgs {
            skill: Some("deliver".into()),
            outcome: None,
            repo: None,
            host: None,
            org: None,
            since: None,
            until: None,
            archive: Some(tmp.path().to_path_buf()),
            format: OutputFormat::Json,
        };
        let report = run(&args).unwrap();
        assert_eq!(report.counts.returned, 1);
        assert_eq!(report.records[0].skill, "deliver-pr");
    }

    #[test]
    fn filters_by_since_until() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollup(
            tmp.path(),
            "id1",
            "skill-usage.rollup.v1",
            "s",
            "pass",
            "2026-06-01T10:00:00Z",
        );
        write_rollup(
            tmp.path(),
            "id2",
            "skill-usage.rollup.v1",
            "s",
            "pass",
            "2026-06-15T10:00:00Z",
        );
        let args = DispatchArgs {
            skill: None,
            outcome: None,
            repo: None,
            host: None,
            org: None,
            since: Some("2026-06-10".into()),
            until: None,
            archive: Some(tmp.path().to_path_buf()),
            format: OutputFormat::Json,
        };
        let report = run(&args).unwrap();
        assert_eq!(report.counts.returned, 1);
        assert_eq!(report.records[0].id, "id2");
    }

    #[test]
    fn out_of_range_reported_even_when_it_fails_an_active_filter() {
        // F6: an out-of-range rollup that would NOT match the active `--skill`
        // filter must still be reported (counted + warned), never silently
        // dropped by the content filter.
        let tmp = tempfile::tempdir().unwrap();
        // In-range, matches the filter.
        write_rollup(
            tmp.path(),
            "id-match",
            "skill-usage.rollup.v1",
            "deliver-pr",
            "pass",
            "2026-06-10T10:00:00Z",
        );
        // Out-of-range AND its skill would fail the `--skill deliver` filter.
        write_rollup(
            tmp.path(),
            "id-oor",
            "skill-usage.rollup.v2",
            "code-review",
            "pass",
            "2026-06-11T10:00:00Z",
        );
        let args = DispatchArgs {
            skill: Some("deliver".into()),
            outcome: None,
            repo: None,
            host: None,
            org: None,
            since: None,
            until: None,
            archive: Some(tmp.path().to_path_buf()),
            format: OutputFormat::Json,
        };
        let report = run(&args).unwrap();
        assert_eq!(report.counts.returned, 1, "only the in-range match returns");
        assert_eq!(
            report.counts.out_of_range, 1,
            "the filtered-out OOR rollup is still counted"
        );
        assert_eq!(report.warnings.len(), 1, "the OOR rollup is still warned");
        assert_eq!(report.warnings[0].record_id, "id-oor");
        // F7: funnel invariant holds.
        assert!(report.counts.scanned >= report.counts.returned + report.counts.out_of_range);
        assert_eq!(report.counts.scanned, 2);
    }

    #[test]
    fn counts_form_an_honest_funnel() {
        // F7: scanned = total parsed; returned + out_of_range never exceed it,
        // even with a content filter dropping in-range records.
        let tmp = tempfile::tempdir().unwrap();
        write_rollup(
            tmp.path(),
            "id-a",
            "skill-usage.rollup.v1",
            "deliver-pr",
            "pass",
            "2026-06-10T10:00:00Z",
        );
        write_rollup(
            tmp.path(),
            "id-b",
            "skill-usage.rollup.v1",
            "code-review",
            "pass",
            "2026-06-10T10:00:00Z",
        );
        write_rollup(
            tmp.path(),
            "id-c",
            "skill-usage.rollup.v2",
            "deliver-pr",
            "pass",
            "2026-06-10T10:00:00Z",
        );
        let args = DispatchArgs {
            skill: Some("deliver".into()),
            outcome: None,
            repo: None,
            host: None,
            org: None,
            since: None,
            until: None,
            archive: Some(tmp.path().to_path_buf()),
            format: OutputFormat::Json,
        };
        let report = run(&args).unwrap();
        assert_eq!(report.counts.scanned, 3, "every rollup is scanned");
        assert_eq!(
            report.counts.returned, 1,
            "only in-range deliver-pr returns"
        );
        assert_eq!(report.counts.out_of_range, 1, "the v2 rollup is OOR");
        assert!(
            report.counts.scanned >= report.counts.returned + report.counts.out_of_range,
            "scanned ({}) >= returned ({}) + out_of_range ({})",
            report.counts.scanned,
            report.counts.returned,
            report.counts.out_of_range
        );
    }

    #[test]
    fn out_of_range_schema_reported_and_excluded() {
        let tmp = tempfile::tempdir().unwrap();
        write_rollup(
            tmp.path(),
            "idok",
            "skill-usage.rollup.v1",
            "s",
            "pass",
            "2026-06-10T10:00:00Z",
        );
        write_rollup(
            tmp.path(),
            "idfuture",
            "skill-usage.rollup.v2",
            "s",
            "pass",
            "2026-06-11T10:00:00Z",
        );
        let args = DispatchArgs {
            skill: None,
            outcome: None,
            repo: None,
            host: None,
            org: None,
            since: None,
            until: None,
            archive: Some(tmp.path().to_path_buf()),
            format: OutputFormat::Json,
        };
        let report = run(&args).unwrap();
        assert_eq!(report.counts.scanned, 2);
        assert_eq!(report.counts.returned, 1);
        assert_eq!(report.counts.out_of_range, 1);
        assert_eq!(report.warnings.len(), 1);
        let w = &report.warnings[0];
        assert_eq!(w.code, "record-schema-version-out-of-range");
        assert_eq!(w.schema_found, "skill-usage.rollup.v2");
        assert_eq!(w.record_id, "idfuture");
        assert!(report.records.iter().all(|r| r.id != "idfuture"));
    }
}

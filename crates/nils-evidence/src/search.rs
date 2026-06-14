//! `evidence search <term>` — simple catalog-row substring matcher.
//!
//! NET-NEW and intentionally minimal. Plan-archive's `search` is full-text
//! over provider issue/PR/MR body and comment snapshots fetched into an
//! `_index/` tree — none of which exists here. Evidence search is a
//! case-insensitive substring match over each catalog row's `intent` and
//! `outcome_summary`, returning hit-level results. It does **not** reuse
//! `query::scan`, `refparse`, or any `_index/` tree.

use std::path::PathBuf;

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::Serialize;

use crate::catalog::{self, CatalogRecord};

const BINARY: &str = "evidence";
const COMMAND: &str = "search";

pub struct DispatchArgs {
    pub term: String,
    pub archive: Option<PathBuf>,
    pub format: OutputFormat,
}

/// Which field matched the term.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatchField {
    Intent,
    OutcomeSummary,
}

/// A single hit (one record may produce one hit per matching field).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SearchHit {
    pub id: String,
    pub host: String,
    pub org: String,
    pub repo: String,
    pub skill: String,
    pub field: MatchField,
    /// The matched text (already scrubbed at archive time).
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchReport {
    pub term: String,
    pub hits: Vec<SearchHit>,
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("archive clone path not found at `{0}`")]
    ArchiveCloneMissing(PathBuf),
    #[error("io error during search: {0}")]
    Io(String),
}

impl SearchError {
    pub fn code(&self) -> &'static str {
        match self {
            SearchError::ArchiveCloneMissing(_) => "search-archive-clone-missing",
            SearchError::Io(_) => "search-io-error",
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

pub fn run(args: &DispatchArgs) -> Result<SearchReport, SearchError> {
    let archive = match crate::source::resolve_archive(args.archive.as_deref()) {
        Ok(p) => p,
        Err(crate::source::SourceError::ArchiveCloneMissing(p)) => {
            return Err(SearchError::ArchiveCloneMissing(p));
        }
        Err(e) => return Err(SearchError::Io(e.to_string())),
    };
    let document = catalog::build_document(&archive).map_err(|e| SearchError::Io(e.to_string()))?;
    let hits = match_records(&document.records, &args.term);
    Ok(SearchReport {
        term: args.term.clone(),
        hits,
    })
}

/// Case-insensitive substring match over `intent` and `outcome_summary`.
fn match_records(records: &[CatalogRecord], term: &str) -> Vec<SearchHit> {
    let needle = term.to_ascii_lowercase();
    let mut hits = Vec::new();
    for r in records {
        if r.intent.to_ascii_lowercase().contains(&needle) {
            hits.push(SearchHit {
                id: r.id.clone(),
                host: r.host.clone(),
                org: r.org.clone(),
                repo: r.repo.clone(),
                skill: r.skill.clone(),
                field: MatchField::Intent,
                snippet: r.intent.clone(),
            });
        }
        if r.outcome_summary.to_ascii_lowercase().contains(&needle) {
            hits.push(SearchHit {
                id: r.id.clone(),
                host: r.host.clone(),
                org: r.org.clone(),
                repo: r.repo.clone(),
                skill: r.skill.clone(),
                field: MatchField::OutcomeSummary,
                snippet: r.outcome_summary.clone(),
            });
        }
    }
    // Deterministic order: by id, then field.
    hits.sort_by(|a, b| (&a.id, field_ord(a.field)).cmp(&(&b.id, field_ord(b.field))));
    hits
}

fn field_ord(f: MatchField) -> u8 {
    match f {
        MatchField::Intent => 0,
        MatchField::OutcomeSummary => 1,
    }
}

fn emit(format: OutputFormat, report: &SearchReport) -> i32 {
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
            if report.hits.is_empty() {
                println!("no matches for `{}`", report.term);
            }
            for h in &report.hits {
                let field = match h.field {
                    MatchField::Intent => "intent",
                    MatchField::OutcomeSummary => "outcome_summary",
                };
                println!(
                    "{}  {}/{}/{}  [{field}]  {}",
                    h.id, h.host, h.org, h.repo, h.snippet
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

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, intent: &str, summary: &str) -> CatalogRecord {
        CatalogRecord {
            id: id.into(),
            host: "github.com".into(),
            org: "graysurf".into(),
            repo: "kit".into(),
            archived_at: "2026-06-14T10:00:00Z".into(),
            date: "2026-06-14".into(),
            skill: "deliver-pr".into(),
            intent: intent.into(),
            trigger: "user_explicit".into(),
            outcome_status: "pass".into(),
            outcome_summary: summary.into(),
            producer_version: "1.4.0".into(),
            validation_count: 0,
            failure_count: 0,
            promotion_case: None,
            source_digest: "sha256:x".into(),
            record_schema: "skill-usage.rollup.v1".into(),
            archive_path: "evidence/x".into(),
        }
    }

    #[test]
    fn matches_intent_case_insensitively() {
        let recs = vec![record("id1", "Deliver the ROLLBACK plan", "done")];
        let hits = match_records(&recs, "rollback");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].field, MatchField::Intent);
    }

    #[test]
    fn matches_outcome_summary() {
        let recs = vec![record("id1", "deliver", "Completed with a clean rebase")];
        let hits = match_records(&recs, "rebase");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].field, MatchField::OutcomeSummary);
    }

    #[test]
    fn one_record_can_hit_both_fields() {
        let recs = vec![record("id1", "fix the cache", "cache fixed")];
        let hits = match_records(&recs, "cache");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].field, MatchField::Intent);
        assert_eq!(hits[1].field, MatchField::OutcomeSummary);
    }

    #[test]
    fn no_match_returns_empty() {
        let recs = vec![record("id1", "deliver", "done")];
        assert!(match_records(&recs, "nonexistent").is_empty());
    }
}

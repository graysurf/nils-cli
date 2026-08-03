//! `plan-archive search <term>` — hit-level full-text search across
//! archived issue / PR / MR body and comment text.
//!
//! Where `catalog --deep` answers "which plans mention this term" at
//! record granularity, `search` answers "where does this term appear",
//! emitting one hit per matching field with the owning plan slug, the
//! ref URL, the matched field, and a context snippet. It reuses the
//! shared [`crate::query::scan`] core (no second scanner) and resolves
//! each hit's ref to a plan via the derived [`crate::catalog`] map. It
//! is read-only: it never fetches, writes, or commits.
//!
//! v1 is deliberately minimal: case-insensitive substring over each
//! ref's latest snapshot only, no ranking.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::{Envelope, EnvelopeError, OutputFormat, exit, schema_version_for};
use serde::Serialize;

use crate::query::index;
use crate::query::scan::{self, MatchField};
use crate::refresh::refparse::parse_ref_url;

const BINARY: &str = "plan-archive";
const COMMAND: &str = "search";

/// Args forwarded from `cli::run`.
pub struct DispatchArgs {
    pub term: String,
    pub archive: Option<PathBuf>,
    pub format: OutputFormat,
}

/// One matched field within one ref's latest snapshot.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SearchHit {
    /// Archived-plan slug that references this ref, when one maps to it.
    pub plan: Option<String>,
    /// Canonical provider URL of the matched ref.
    pub r#ref: String,
    /// Which field matched (`body` or `comment`).
    pub field: MatchField,
    /// Context snippet around the first match.
    pub snippet: String,
}

/// Search result payload. `hits` are sorted by ref identity, body
/// before comments within a ref.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SearchResult {
    pub term: String,
    pub total_hits: usize,
    pub hits: Vec<SearchHit>,
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("search term must not be empty")]
    EmptyTerm,
    #[error("archive clone path not found at `{0}`")]
    ArchiveCloneMissing(PathBuf),
    #[error("io error during search: {0}")]
    Io(String),
}

impl SearchError {
    pub fn code(&self) -> &'static str {
        match self {
            SearchError::EmptyTerm => "search-empty-term",
            SearchError::ArchiveCloneMissing(_) => "search-archive-clone-missing",
            SearchError::Io(_) => "search-io-error",
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
pub fn run(args: &DispatchArgs) -> Result<SearchResult, SearchError> {
    let term = args.term.trim();
    if term.is_empty() {
        return Err(SearchError::EmptyTerm);
    }
    let archive = resolve_archive(args.archive.as_deref())?;
    let plan_map = plan_map(&archive);

    let mut entries =
        index::walk_index(&archive.join("_index")).map_err(|e| SearchError::Io(e.to_string()))?;
    entries.sort_by(|a, b| {
        (&a.host, &a.org_or_group_path, &a.repo, a.number).cmp(&(
            &b.host,
            &b.org_or_group_path,
            &b.repo,
            b.number,
        ))
    });

    let mut hits = Vec::new();
    for entry in &entries {
        let scan_hits =
            scan::scan_entry(&archive, entry, term).map_err(|e| SearchError::Io(e.to_string()))?;
        for hit in scan_hits {
            hits.push(SearchHit {
                plan: plan_map.get(&hit.url).cloned(),
                r#ref: hit.url,
                field: hit.field,
                snippet: hit.snippet,
            });
        }
    }

    Ok(SearchResult {
        term: term.to_string(),
        total_hits: hits.len(),
        hits,
    })
}

/// Map each ref's canonical URL to the archived-plan slug that
/// references it. Best-effort: a catalog build failure yields an empty
/// map, so hits are still returned, just without plan attribution.
fn plan_map(archive: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Ok(document) = crate::catalog::build_document(archive) {
        for record in document.records {
            for r in record.refs {
                let key = parse_ref_url(&r.url)
                    .map(|t| t.canonical_url())
                    .unwrap_or(r.url);
                map.entry(key).or_insert_with(|| record.slug.clone());
            }
        }
    }
    map
}

fn resolve_archive(arg: Option<&Path>) -> Result<PathBuf, SearchError> {
    let candidate = match arg {
        Some(p) => p.to_path_buf(),
        None => default_archive_clone_path()?,
    };
    if !candidate.is_dir() {
        return Err(SearchError::ArchiveCloneMissing(candidate));
    }
    Ok(candidate)
}

fn default_archive_clone_path() -> Result<PathBuf, SearchError> {
    let local = crate::validate::local::validate_local_path(&local_config_path())
        .map_err(|e| SearchError::Io(e.to_string()))?;
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

fn field_label(field: MatchField) -> &'static str {
    match field {
        MatchField::Body => "body",
        MatchField::Comment => "comment",
    }
}

fn emit(format: OutputFormat, result: &SearchResult) -> i32 {
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
            if result.hits.is_empty() {
                println!("no matches for \"{}\"", result.term);
            }
            for hit in &result.hits {
                let plan = hit.plan.as_deref().unwrap_or("(unlinked)");
                println!(
                    "{}  [{}] {}\n    {}",
                    hit.r#ref,
                    field_label(hit.field),
                    plan,
                    hit.snippet
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

    use pretty_assertions::assert_eq;

    fn args(term: &str, archive: Option<PathBuf>) -> DispatchArgs {
        DispatchArgs {
            term: term.to_string(),
            archive,
            format: OutputFormat::Json,
        }
    }

    #[test]
    fn every_error_maps_to_a_stable_machine_code() {
        assert_eq!(SearchError::EmptyTerm.code(), "search-empty-term");
        assert_eq!(
            SearchError::ArchiveCloneMissing(PathBuf::from("/nope")).code(),
            "search-archive-clone-missing"
        );
        assert_eq!(SearchError::Io("boom".into()).code(), "search-io-error");
        assert_eq!(
            SearchError::ArchiveCloneMissing(PathBuf::from("/nope")).to_string(),
            "archive clone path not found at `/nope`"
        );
    }

    #[test]
    fn an_empty_or_whitespace_term_is_refused_before_any_io() {
        // The guard must fire before archive resolution, so an unusable
        // archive path cannot mask the real complaint.
        let missing = PathBuf::from("/definitely/not/an/archive");
        assert_eq!(
            run(&args("", Some(missing.clone())))
                .expect_err("empty term")
                .code(),
            "search-empty-term"
        );
        assert_eq!(
            run(&args("   \t ", Some(missing)))
                .expect_err("whitespace term")
                .code(),
            "search-empty-term"
        );
    }

    #[test]
    fn a_missing_archive_clone_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("absent");

        let err = resolve_archive(Some(&missing)).expect_err("missing archive");
        assert_eq!(err.code(), "search-archive-clone-missing");
        assert!(
            err.to_string().contains(&*missing.to_string_lossy()),
            "the error must name the path it looked for"
        );

        // A real directory resolves to itself, unchanged.
        assert_eq!(
            resolve_archive(Some(tmp.path())).expect("existing archive"),
            tmp.path().to_path_buf()
        );
    }

    #[test]
    fn an_archive_without_an_index_yields_an_empty_result() {
        let tmp = tempfile::tempdir().unwrap();

        let result = run(&args("anything", Some(tmp.path().to_path_buf())))
            .expect("empty archive is not an error");

        assert_eq!(result.term, "anything");
        assert_eq!(result.total_hits, 0);
        assert!(result.hits.is_empty());
    }

    #[test]
    fn the_term_is_trimmed_into_the_result_payload() {
        let tmp = tempfile::tempdir().unwrap();

        let result = run(&args("  spaced  ", Some(tmp.path().to_path_buf()))).expect("search");

        assert_eq!(result.term, "spaced");
    }

    #[test]
    fn match_fields_render_with_their_wire_labels() {
        assert_eq!(field_label(MatchField::Body), "body");
        assert_eq!(field_label(MatchField::Comment), "comment");
    }

    #[test]
    fn emit_reports_success_for_both_formats() {
        let empty = SearchResult {
            term: "term".to_string(),
            total_hits: 0,
            hits: Vec::new(),
        };
        assert_eq!(emit(OutputFormat::Json, &empty), exit::SUCCESS);
        assert_eq!(emit(OutputFormat::Text, &empty), exit::SUCCESS);

        let populated = SearchResult {
            term: "term".to_string(),
            total_hits: 2,
            hits: vec![
                SearchHit {
                    plan: Some("plan-slug".to_string()),
                    r#ref: "https://github.com/acme/widgets/pull/7".to_string(),
                    field: MatchField::Body,
                    snippet: "…term…".to_string(),
                },
                SearchHit {
                    // An unattributed hit still renders; plan attribution is
                    // best-effort and must never suppress a match.
                    plan: None,
                    r#ref: "https://github.com/acme/widgets/issues/8".to_string(),
                    field: MatchField::Comment,
                    snippet: "…term…".to_string(),
                },
            ],
        };
        assert_eq!(emit(OutputFormat::Json, &populated), exit::SUCCESS);
        assert_eq!(emit(OutputFormat::Text, &populated), exit::SUCCESS);
    }

    #[test]
    fn emit_error_uses_the_data_exit_code_for_both_formats() {
        assert_eq!(
            emit_error(OutputFormat::Json, "search-empty-term", "empty"),
            exit::DATA
        );
        assert_eq!(
            emit_error(OutputFormat::Text, "search-empty-term", "empty"),
            exit::DATA
        );
    }

    #[test]
    fn dispatch_returns_the_emitted_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            dispatch(args("term", Some(tmp.path().to_path_buf()))),
            exit::SUCCESS
        );
        assert_eq!(
            dispatch(args("", Some(tmp.path().to_path_buf()))),
            exit::DATA
        );
    }

    #[test]
    fn plan_attribution_is_best_effort_for_an_archive_without_a_catalog() {
        let tmp = tempfile::tempdir().unwrap();

        // No catalog to build from: hits would still be emitted, just
        // unattributed, so the map is empty rather than an error.
        assert!(plan_map(tmp.path()).is_empty());
    }
}

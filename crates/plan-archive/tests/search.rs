//! Integration coverage for `plan-archive search` — hit-level body /
//! comment full-text search with plan resolution.

use std::fs;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::OutputFormat;
use plan_archive::query::scan::MatchField;
use plan_archive::search::{self, DispatchArgs};

struct Archive {
    _tmp: tempfile::TempDir,
    path: PathBuf,
}

fn seed_snapshot(archive: &Path, rel_dir: &str, stamp: &str, body: &str) {
    let dir = archive.join("_index").join(rel_dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{stamp}.json")), body).unwrap();
}

fn seed_metadata(archive: &Path, slug: &str, refs: &str) {
    let dir = archive
        .join("plans/github.com/graysurf/agent-runtime-kit")
        .join(slug);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("metadata.yaml"),
        format!(
            "version: 1\nsource:\n  host: github.com\n  org_or_group_path: graysurf\n  repo: agent-runtime-kit\n  branch: main\n  archive_commit: abc123\n  original_path: docs/plans/{slug}/\ncaptured_classification:\n  class: personal\nrefs:\n{refs}"
        ),
    )
    .unwrap();
}

fn build_archive() -> Archive {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("archive");
    fs::create_dir_all(&path).unwrap();
    // Issue 126: body carries the term.
    seed_snapshot(
        &path,
        "github.com/graysurf/agent-runtime-kit/issues/126",
        "20260527T010000Z",
        r#"{"data":{"body":"steps after live acceptance with rollback proven","comments":[]}}"#,
    );
    // PR 127: only a comment carries the term.
    seed_snapshot(
        &path,
        "github.com/graysurf/agent-runtime-kit/pulls/127",
        "20260527T020000Z",
        r#"{"data":{"body":"plain description","comments":[{"body":"please document the rollback path"}]}}"#,
    );
    seed_metadata(
        &path,
        "2026-05-27-cutover",
        "  issue: https://github.com/graysurf/agent-runtime-kit/issues/126\n  pr: https://github.com/graysurf/agent-runtime-kit/pull/127\n",
    );
    Archive { _tmp: tmp, path }
}

fn args(archive: &Path, term: &str) -> DispatchArgs {
    DispatchArgs {
        term: term.to_string(),
        archive: Some(archive.to_path_buf()),
        format: OutputFormat::Json,
    }
}

#[test]
fn search_returns_body_and_comment_hits_with_plan() {
    let archive = build_archive();
    let result = search::run(&args(&archive.path, "rollback")).unwrap();

    assert_eq!(result.term, "rollback");
    assert_eq!(result.total_hits, 2, "one body hit + one comment hit");

    let body_hit = result
        .hits
        .iter()
        .find(|h| h.field == MatchField::Body)
        .expect("body hit");
    assert_eq!(
        body_hit.r#ref,
        "https://github.com/graysurf/agent-runtime-kit/issues/126"
    );
    assert_eq!(body_hit.plan.as_deref(), Some("2026-05-27-cutover"));
    assert!(body_hit.snippet.to_ascii_lowercase().contains("rollback"));

    let comment_hit = result
        .hits
        .iter()
        .find(|h| h.field == MatchField::Comment)
        .expect("comment hit");
    assert_eq!(
        comment_hit.r#ref,
        "https://github.com/graysurf/agent-runtime-kit/pull/127"
    );
    assert_eq!(comment_hit.plan.as_deref(), Some("2026-05-27-cutover"));
}

#[test]
fn search_no_match_is_well_formed_empty() {
    let archive = build_archive();
    let result = search::run(&args(&archive.path, "nonexistent-token")).unwrap();
    assert_eq!(result.total_hits, 0);
    assert!(result.hits.is_empty());
    assert_eq!(result.term, "nonexistent-token");
}

#[test]
fn search_empty_term_errors() {
    let archive = build_archive();
    let err = search::run(&args(&archive.path, "   ")).unwrap_err();
    assert_eq!(err.code(), "search-empty-term");
}

#[test]
fn search_is_case_insensitive() {
    let archive = build_archive();
    let result = search::run(&args(&archive.path, "ROLLBACK")).unwrap();
    assert_eq!(result.total_hits, 2);
}

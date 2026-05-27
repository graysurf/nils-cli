//! Integration coverage for the derived `plan-archive catalog` projection.

use std::fs;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::OutputFormat;
use plan_archive::catalog::{self, DispatchArgs};

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
    let refs_section = if refs.is_empty() {
        String::new()
    } else {
        format!("refs:\n{refs}")
    };
    fs::write(
        dir.join("metadata.yaml"),
        format!(
            "version: 1\nsource:\n  host: github.com\n  org_or_group_path: graysurf\n  repo: agent-runtime-kit\n  branch: main\n  archive_commit: abc123\n  original_path: docs/plans/{slug}/\ncaptured_classification:\n  class: personal\n{refs_section}"
        ),
    )
    .unwrap();
}

fn build_archive() -> Archive {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("archive");
    fs::create_dir_all(&path).unwrap();
    seed_snapshot(
        &path,
        "github.com/graysurf/agent-runtime-kit/issues/126",
        "20260527T010000Z",
        r#"{"data":{"title":"Catalog issue","state":"open"}}"#,
    );
    seed_snapshot(
        &path,
        "github.com/graysurf/agent-runtime-kit/pulls/127",
        "20260527T020000Z",
        r#"{"title":"Catalog PR","state":"merged"}"#,
    );
    seed_metadata(
        &path,
        "2026-05-27-plan-archive-search-layer",
        "  issue: https://github.com/graysurf/agent-runtime-kit/issues/126\n  pr: https://github.com/graysurf/agent-runtime-kit/pull/127\n",
    );
    seed_metadata(
        &path,
        "2026-05-28-plan-with-missing-snapshot",
        "  issue: https://github.com/graysurf/agent-runtime-kit/issues/999\n",
    );
    seed_metadata(&path, "2026-05-29-plan-without-refs", "");
    Archive { _tmp: tmp, path }
}

fn base_args(archive: &Path) -> DispatchArgs {
    DispatchArgs {
        write: false,
        grep: None,
        area: None,
        refs_to: None,
        archive: Some(archive.to_path_buf()),
        format: OutputFormat::Json,
    }
}

#[test]
fn catalog_document_includes_plans_refs_and_latest_snapshots() {
    let archive = build_archive();
    let document = catalog::build_document(&archive.path).unwrap();

    assert_eq!(document.records.len(), 3);
    let first = &document.records[0];
    assert_eq!(first.slug, "2026-05-27-plan-archive-search-layer");
    assert_eq!(first.date, "2026-05-27");
    assert_eq!(first.title, "Catalog issue");
    assert_eq!(first.refs.len(), 2);
    assert_eq!(first.refs[0].state, "open");
    assert_eq!(
        first.refs[0].latest_snapshot.as_deref(),
        Some("_index/github.com/graysurf/agent-runtime-kit/issues/126/20260527T010000Z.json")
    );
    assert_eq!(
        first.refs[0].fetched_at.as_deref(),
        Some("2026-05-27T01:00:00Z")
    );

    let missing = document
        .records
        .iter()
        .find(|r| r.slug == "2026-05-28-plan-with-missing-snapshot")
        .unwrap();
    assert!(missing.refs[0].latest_snapshot.is_none());
}

#[test]
fn catalog_serialization_is_deterministic() {
    let archive = build_archive();
    let first = catalog::to_catalog_json(&catalog::build_document(&archive.path).unwrap()).unwrap();
    let second =
        catalog::to_catalog_json(&catalog::build_document(&archive.path).unwrap()).unwrap();
    assert_eq!(first, second);
}

#[test]
fn refs_to_returns_referencing_plans() {
    let archive = build_archive();
    let mut args = base_args(&archive.path);
    args.refs_to = Some("https://github.com/graysurf/agent-runtime-kit/issues/126".into());

    let report = catalog::run(&args).unwrap();
    assert_eq!(report.records.len(), 1);
    assert_eq!(
        report.records[0].slug,
        "2026-05-27-plan-archive-search-layer"
    );
}

#[test]
fn write_catalog_persists_deterministic_json() {
    let archive = build_archive();
    let mut args = base_args(&archive.path);
    args.write = true;

    let report = catalog::run(&args).unwrap();
    let written = fs::read_to_string(archive.path.join("catalog.json")).unwrap();
    let rebuilt =
        catalog::to_catalog_json(&catalog::build_document(&archive.path).unwrap()).unwrap();
    assert_eq!(written, rebuilt);
    assert_eq!(report.total_records, 3);
}

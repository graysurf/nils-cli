//! Integration coverage for `plan-archive query` against a seeded
//! `_index/` tree and archived plan metadata.

use std::fs;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::OutputFormat;
use plan_archive::query::{self, DispatchArgs, QueryResult};

struct Archive {
    _tmp: tempfile::TempDir,
    path: PathBuf,
}

fn seed_snapshot(archive: &Path, rel_dir: &str, stamp: &str, body: &str) {
    let dir = archive.join("_index").join(rel_dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{stamp}.json")), body).unwrap();
}

fn build_archive() -> Archive {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("archive");
    fs::create_dir_all(&path).unwrap();

    // github.com/graysurf/agent-runtime-kit issue 126 — two snapshots.
    seed_snapshot(
        &path,
        "github.com/graysurf/agent-runtime-kit/issues/126",
        "20260527T010000Z",
        r#"{"title":"older"}"#,
    );
    seed_snapshot(
        &path,
        "github.com/graysurf/agent-runtime-kit/issues/126",
        "20260527T020000Z",
        r#"{"title":"newer"}"#,
    );
    // github.com/graysurf/agent-runtime-kit pull 127 — one snapshot.
    seed_snapshot(
        &path,
        "github.com/graysurf/agent-runtime-kit/pulls/127",
        "20260527T030000Z",
        r#"{"title":"pr"}"#,
    );
    // gitlab.example.com/acme/platform/ingest MR 42 — one snapshot.
    seed_snapshot(
        &path,
        "gitlab.example.com/acme/platform/ingest/merge_requests/42",
        "20260101T120000Z",
        r#"{"title":"mr"}"#,
    );

    Archive { _tmp: tmp, path }
}

fn base_args(archive: &Path) -> DispatchArgs {
    DispatchArgs {
        r#ref: None,
        host: None,
        org: None,
        repo: None,
        since: None,
        plan: None,
        refs_from: None,
        archive: Some(archive.to_path_buf()),
        format: OutputFormat::Json,
    }
}

#[test]
fn single_ref_returns_latest_snapshot_with_fetched_at() {
    let archive = build_archive();
    let mut args = base_args(&archive.path);
    args.r#ref = Some("https://github.com/graysurf/agent-runtime-kit/issues/126".to_string());

    let result = query::run(&args).unwrap();
    let QueryResult::SingleRef { record } = result else {
        panic!("expected single-ref result");
    };
    assert_eq!(record.number, 126);
    assert_eq!(record.fetched_at.as_deref(), Some("2026-05-27T02:00:00Z"));
    assert!(
        record
            .latest_snapshot
            .as_deref()
            .unwrap()
            .ends_with("20260527T020000Z.json")
    );
}

#[test]
fn single_ref_missing_returns_no_snapshot_not_error() {
    let archive = build_archive();
    let mut args = base_args(&archive.path);
    args.r#ref = Some("https://github.com/graysurf/agent-runtime-kit/issues/9999".to_string());

    let result = query::run(&args).unwrap();
    let QueryResult::SingleRef { record } = result else {
        panic!("expected single-ref result");
    };
    assert!(record.latest_snapshot.is_none());
    assert!(record.fetched_at.is_none());
}

#[test]
fn aggregate_by_host_returns_all_repos_one_pass() {
    let archive = build_archive();
    let mut args = base_args(&archive.path);
    args.host = Some("github.com".to_string());

    let result = query::run(&args).unwrap();
    let QueryResult::Aggregate { records } = result else {
        panic!("expected aggregate result");
    };
    assert_eq!(records.len(), 2); // issue 126 + pull 127
    assert!(records.iter().all(|r| r.host == "github.com"));
}

#[test]
fn aggregate_cross_host_returns_every_reachable_host() {
    let archive = build_archive();
    let mut args = base_args(&archive.path);
    // No host filter, but a repo filter that matches nothing forces
    // the empty path; instead use since far in the past to match all.
    args.since = Some("2020-01-01".to_string());

    let result = query::run(&args).unwrap();
    let QueryResult::Aggregate { records } = result else {
        panic!("expected aggregate result");
    };
    // issue 126 + pull 127 + gitlab MR 42 = 3
    assert_eq!(records.len(), 3);
    let hosts: Vec<&str> = records.iter().map(|r| r.host.as_str()).collect();
    assert!(hosts.contains(&"github.com"));
    assert!(hosts.contains(&"gitlab.example.com"));
}

#[test]
fn aggregate_no_match_returns_empty_array() {
    let archive = build_archive();
    let mut args = base_args(&archive.path);
    args.repo = Some("does-not-exist".to_string());

    let result = query::run(&args).unwrap();
    let QueryResult::Aggregate { records } = result else {
        panic!("expected aggregate result");
    };
    assert!(records.is_empty());
}

#[test]
fn aggregate_since_filters_out_older_snapshots() {
    let archive = build_archive();
    let mut args = base_args(&archive.path);
    args.since = Some("2026-05-01".to_string());

    let result = query::run(&args).unwrap();
    let QueryResult::Aggregate { records } = result else {
        panic!("expected aggregate result");
    };
    // The gitlab MR snapshot is from 2026-01-01, filtered out.
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|r| r.host == "github.com"));
}

#[test]
fn aggregate_invalid_since_rejected() {
    let archive = build_archive();
    let mut args = base_args(&archive.path);
    args.since = Some("2026/05/01".to_string());
    let err = query::run(&args).unwrap_err();
    assert_eq!(err.code(), "query-invalid-since");
}

#[test]
fn plan_link_resolves_metadata_refs_to_snapshots() {
    let archive = build_archive();
    // Seed an archived plan with metadata pointing at issue 126 + pull 127.
    let plan_rel = "plans/github.com/graysurf/agent-runtime-kit/2026-05-27-demo";
    let plan_dir = archive.path.join(plan_rel);
    fs::create_dir_all(&plan_dir).unwrap();
    fs::write(
        plan_dir.join("metadata.yaml"),
        "version: 1\nrefs:\n  issue: https://github.com/graysurf/agent-runtime-kit/issues/126\n  pr: https://github.com/graysurf/agent-runtime-kit/pull/127\n",
    )
    .unwrap();

    let mut args = base_args(&archive.path);
    args.plan = Some(plan_rel.to_string());

    let result = query::run(&args).unwrap();
    let QueryResult::PlanLink { plan, records } = result else {
        panic!("expected plan-link result");
    };
    assert_eq!(plan, plan_rel);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].number, 126);
    assert!(records[0].fetched_at.is_some());
    assert_eq!(records[1].number, 127);
}

#[test]
fn refs_from_reads_metadata_file_directly() {
    let archive = build_archive();
    let meta = archive.path.join("scratch-metadata.yaml");
    fs::write(
        &meta,
        "version: 1\nrefs:\n  mr: https://gitlab.example.com/acme/platform/ingest/-/merge_requests/42\n",
    )
    .unwrap();

    let mut args = base_args(&archive.path);
    args.refs_from = Some(meta.display().to_string());

    let result = query::run(&args).unwrap();
    let QueryResult::PlanLink { records, .. } = result else {
        panic!("expected plan-link result");
    };
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].number, 42);
    assert_eq!(
        records[0].fetched_at.as_deref(),
        Some("2026-01-01T12:00:00Z")
    );
}

#[test]
fn plan_link_missing_metadata_errors() {
    let archive = build_archive();
    let mut args = base_args(&archive.path);
    args.plan = Some("plans/nope".to_string());
    let err = query::run(&args).unwrap_err();
    assert_eq!(err.code(), "query-metadata-not-found");
}

#[test]
fn no_selector_is_rejected() {
    let archive = build_archive();
    let args = base_args(&archive.path);
    let err = query::run(&args).unwrap_err();
    assert_eq!(err.code(), "query-no-selector");
}

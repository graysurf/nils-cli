//! Integration coverage for `plan-archive refresh`, driving the
//! pipeline through a stub `ForgeFetcher` and a frozen clock so the
//! snapshot writes are fully deterministic with no network.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use nils_common::cli_contract::OutputFormat;
use plan_archive::refresh::{self, Clock, DispatchArgs, ForgeFetcher, RefTarget};

struct FrozenClock(String);
impl Clock for FrozenClock {
    fn now_basic_utc(&self) -> String {
        self.0.clone()
    }
}

#[derive(Default)]
struct StubForge {
    payloads: HashMap<String, String>,
    listing: Vec<RefTarget>,
    list_calls: RefCell<u32>,
}

impl ForgeFetcher for StubForge {
    fn fetch_payload(&self, target: &RefTarget) -> Result<String, String> {
        self.payloads
            .get(&target.canonical_url())
            .cloned()
            .ok_or_else(|| format!("no stub payload for {}", target.canonical_url()))
    }

    fn list_open_refs(
        &self,
        _host: &str,
        _org: &str,
        _repo: &str,
        _since: Option<&str>,
    ) -> Result<Vec<RefTarget>, String> {
        *self.list_calls.borrow_mut() += 1;
        Ok(self.listing.clone())
    }
}

struct Archive {
    _tmp: tempfile::TempDir,
    path: PathBuf,
}

fn build_archive() -> Archive {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("archive");
    let config = path.join("config");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("hosts.yaml"),
        "version: 1\nhosts:\n  github.com:\n    class: personal\n",
    )
    .unwrap();
    Archive { _tmp: tmp, path }
}

fn ref_target(url: &str) -> RefTarget {
    refresh::parse_ref_url(url).unwrap()
}

fn base_args(archive: &Path) -> DispatchArgs {
    DispatchArgs {
        r#ref: None,
        repo: None,
        since: None,
        archive: Some(archive.to_path_buf()),
        hosts: None,
        format: OutputFormat::Json,
    }
}

#[test]
fn single_ref_clean_payload_writes_snapshot_no_log() {
    let archive = build_archive();
    let url = "https://github.com/graysurf/agent-runtime-kit/issues/126";
    let mut forge = StubForge::default();
    forge.payloads.insert(
        url.to_string(),
        r#"{"title":"hello","body":"no secrets"}"#.to_string(),
    );
    let clock = FrozenClock("20260527T010000Z".to_string());

    let mut args = base_args(&archive.path);
    args.r#ref = Some(url.to_string());

    let report = refresh::run(&args, &forge, &clock).expect("refresh ok");
    assert_eq!(report.snapshots.len(), 1);
    assert!(report.failed.is_empty());
    assert!(!report.requires_review);

    let snap = &report.snapshots[0];
    assert!(snap.scrub_log_path.is_none());
    assert_eq!(snap.redaction_count, 0);

    let expected = archive
        .path
        .join("_index/github.com/graysurf/agent-runtime-kit/issues/126/20260527T010000Z.json");
    assert!(expected.exists(), "missing {expected:?}");
    let body = fs::read_to_string(&expected).unwrap();
    assert!(body.contains("no secrets"));
}

#[test]
fn single_ref_dirty_payload_emits_scrub_log_and_requires_review() {
    let archive = build_archive();
    let url = "https://github.com/graysurf/agent-runtime-kit/pull/127";
    let mut forge = StubForge::default();
    forge.payloads.insert(
        url.to_string(),
        r#"{"body":"token: ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#.to_string(),
    );
    let clock = FrozenClock("20260527T020000Z".to_string());

    let mut args = base_args(&archive.path);
    args.r#ref = Some(url.to_string());

    let report = refresh::run(&args, &forge, &clock).expect("refresh ok");
    assert!(report.requires_review);
    let snap = &report.snapshots[0];
    assert_eq!(snap.redaction_count, 1);
    let log = snap.scrub_log_path.as_ref().expect("scrub log emitted");
    assert!(PathBuf::from(log).exists());

    // Snapshot must be scrubbed on disk.
    let dir = archive
        .path
        .join("_index/github.com/graysurf/agent-runtime-kit/pulls/127");
    let snap_file = dir.join("20260527T020000Z.json");
    let body = fs::read_to_string(&snap_file).unwrap();
    assert!(!body.contains("ghp_"), "snapshot leaked secret: {body}");
    assert!(body.contains("[REDACTED]"));

    // Scrub log never carries the secret.
    let log_body = fs::read_to_string(dir.join("20260527T020000Z.scrub.log")).unwrap();
    assert!(!log_body.contains("ghp_"));
    assert!(log_body.contains("pattern=github-token"));
}

#[test]
fn snapshots_are_append_only_across_two_refreshes() {
    let archive = build_archive();
    let url = "https://github.com/graysurf/agent-runtime-kit/issues/126";
    let mut forge = StubForge::default();
    forge
        .payloads
        .insert(url.to_string(), r#"{"title":"v"}"#.to_string());

    let mut args = base_args(&archive.path);
    args.r#ref = Some(url.to_string());

    refresh::run(&args, &forge, &FrozenClock("20260527T030000Z".into())).unwrap();
    refresh::run(&args, &forge, &FrozenClock("20260527T040000Z".into())).unwrap();

    let dir = archive
        .path
        .join("_index/github.com/graysurf/agent-runtime-kit/issues/126");
    let mut files: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    files.sort();
    assert_eq!(
        files,
        vec!["20260527T030000Z.json", "20260527T040000Z.json"]
    );
}

#[test]
fn unknown_host_is_rejected() {
    let archive = build_archive();
    let mut args = base_args(&archive.path);
    args.r#ref = Some("https://gitlab.com/g/r/issues/1".to_string());
    let forge = StubForge::default();
    let err = refresh::run(&args, &forge, &FrozenClock("20260527T050000Z".into())).unwrap_err();
    assert_eq!(err.code(), "refresh-unknown-host");
}

#[test]
fn batch_repo_mode_refreshes_each_listed_ref_and_isolates_failures() {
    let archive = build_archive();
    let ok_url = "https://github.com/graysurf/agent-runtime-kit/issues/1";
    let missing_url = "https://github.com/graysurf/agent-runtime-kit/issues/2";

    // Only the first ref has a stub payload; the second fails to
    // fetch and must not abort the batch.
    let mut payloads = HashMap::new();
    payloads.insert(ok_url.to_string(), r#"{"title":"ok"}"#.to_string());
    let forge = StubForge {
        payloads,
        listing: vec![ref_target(ok_url), ref_target(missing_url)],
        list_calls: RefCell::new(0),
    };

    let mut args = base_args(&archive.path);
    args.repo = Some("github.com/graysurf/agent-runtime-kit".to_string());

    let report = refresh::run(&args, &forge, &FrozenClock("20260527T060000Z".into())).unwrap();
    assert_eq!(report.snapshots.len(), 1);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].code, "refresh-forge-fetch-failed");
    assert_eq!(*forge.list_calls.borrow(), 1);
}

#[test]
fn batch_with_invalid_since_is_rejected() {
    let archive = build_archive();
    let mut args = base_args(&archive.path);
    args.repo = Some("github.com/graysurf/agent-runtime-kit".to_string());
    args.since = Some("2026/05/27".to_string());
    let forge = StubForge::default();
    let err = refresh::run(&args, &forge, &FrozenClock("20260527T070000Z".into())).unwrap_err();
    assert_eq!(err.code(), "refresh-invalid-since");
}

#[test]
fn missing_selector_is_rejected() {
    let archive = build_archive();
    let args = base_args(&archive.path);
    let forge = StubForge::default();
    let err = refresh::run(&args, &forge, &FrozenClock("20260527T080000Z".into())).unwrap_err();
    assert_eq!(err.code(), "refresh-no-selector");
}

#[test]
fn unparseable_ref_is_rejected() {
    let archive = build_archive();
    let mut args = base_args(&archive.path);
    args.r#ref = Some("https://github.com/org/repo/wiki/3".to_string());
    let forge = StubForge::default();
    let err = refresh::run(&args, &forge, &FrozenClock("20260527T090000Z".into())).unwrap_err();
    assert_eq!(err.code(), "refresh-unparseable-ref");
}

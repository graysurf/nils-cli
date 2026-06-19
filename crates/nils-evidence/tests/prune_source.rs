//! Integration coverage for `evidence prune-source`.
//!
//! The command prunes local `agent-out` source run directories only after their
//! raw `skill-usage.record.json` digest is already present in the evidence
//! archive catalog. The archive remains read-only for this command.

use std::fs;
use std::path::{Path, PathBuf};

use evidence::prune_source::{self, PruneSourceArgs, PruneSourceError};
use nils_common::cli_contract::OutputFormat;
use pretty_assertions::assert_eq;

struct Scenario {
    _tmp: tempfile::TempDir,
    source_out: PathBuf,
    archive: PathBuf,
}

fn build_empty() -> Scenario {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let source_out = root.join("out").join("projects");
    let archive = root.join("archive");
    fs::create_dir_all(&source_out).unwrap();
    fs::create_dir_all(&archive).unwrap();
    Scenario {
        _tmp: tmp,
        source_out,
        archive,
    }
}

fn record_json(skill: &str, started: &str) -> String {
    format!(
        r#"{{
            "schema": "skill-usage.record.v1",
            "producer": {{ "tool": "skill-usage", "nils_cli_version": "1.11.2" }},
            "skill": "{skill}",
            "started_at": "{started}",
            "ended_at": "{started}",
            "cwd": "/Users/tester/Project/kit",
            "trigger": "user_explicit",
            "intent": "intent",
            "inputs": {{ "user_request_summary": "x", "referenced_files": [], "external_sources": [] }},
            "outcome": {{ "status": "pass", "summary": "done" }},
            "artifacts": [],
            "linked_records": [],
            "validation": [],
            "failures": []
        }}"#
    )
}

fn write_record(
    source_out: &Path,
    project: &str,
    run_id: &str,
    body: &str,
) -> (PathBuf, PathBuf, String) {
    let run_dir = source_out.join(project).join(run_id);
    fs::create_dir_all(&run_dir).unwrap();
    let record_path = run_dir.join("skill-usage.record.json");
    fs::write(&record_path, body).unwrap();
    fs::write(run_dir.join("child-evidence.txt"), "remove with the run").unwrap();
    let digest = format!("sha256:{}", sha256_hex(body.as_bytes()));
    (run_dir, record_path, digest)
}

fn write_catalog(archive: &Path, digests: &[String]) {
    let rows = digests
        .iter()
        .map(|digest| serde_json::json!({ "source_digest": digest }))
        .collect::<Vec<_>>();
    let body = serde_json::json!({
        "schema_version": "evidence.catalog.v1",
        "records": rows
    });
    fs::write(
        archive.join("catalog.json"),
        serde_json::to_string_pretty(&body).unwrap(),
    )
    .unwrap();
}

fn args(source_out: &Path, archive: &Path, archived_only: bool, apply: bool) -> PruneSourceArgs {
    PruneSourceArgs {
        source_out: Some(source_out.to_path_buf()),
        archive: Some(archive.to_path_buf()),
        repo: None,
        archived_only,
        apply,
        format: OutputFormat::Json,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[test]
fn prune_source_requires_archived_only_scope() {
    let s = build_empty();
    write_catalog(&s.archive, &[]);

    let err = prune_source::run(&args(&s.source_out, &s.archive, false, false)).unwrap_err();

    assert!(matches!(err, PruneSourceError::ArchivedOnlyRequired));
}

#[test]
fn prune_source_dry_run_lists_only_archived_records() {
    let s = build_empty();
    let (archived_dir, archived_record, archived_digest) = write_record(
        &s.source_out,
        "graysurf__kit",
        "20260620-010000-skill-usage",
        &record_json("deliver-pr", "2026-06-20T01:00:00Z"),
    );
    let (unarchived_dir, _, _) = write_record(
        &s.source_out,
        "graysurf__kit",
        "20260620-020000-skill-usage",
        &record_json("code-review", "2026-06-20T02:00:00Z"),
    );
    write_catalog(&s.archive, std::slice::from_ref(&archived_digest));

    let report = prune_source::run(&args(&s.source_out, &s.archive, true, false)).unwrap();

    assert!(!report.applied);
    assert_eq!(report.scanned, 2);
    assert_eq!(report.prunable, 1);
    assert_eq!(report.kept, 1);
    assert_eq!(report.pruned.len(), 1);
    assert_eq!(
        report.pruned[0].source_digest.as_deref(),
        Some(archived_digest.as_str())
    );
    assert_eq!(
        report.pruned[0].record_path,
        archived_record.display().to_string()
    );
    assert_eq!(report.pruned[0].run_dir, archived_dir.display().to_string());
    assert_eq!(report.pruned[0].skill.as_deref(), Some("deliver-pr"));
    assert_eq!(report.retained.len(), 1);
    assert_eq!(report.retained[0].reason, "not archived");

    assert!(
        archived_dir.exists(),
        "dry-run does not delete archived source"
    );
    assert!(
        unarchived_dir.exists(),
        "dry-run does not delete unarchived source"
    );
}

#[test]
fn prune_source_apply_deletes_only_archived_run_dirs() {
    let s = build_empty();
    let (archived_dir, _, archived_digest) = write_record(
        &s.source_out,
        "graysurf__kit",
        "20260620-010000-skill-usage",
        &record_json("deliver-pr", "2026-06-20T01:00:00Z"),
    );
    let (unarchived_dir, _, _) = write_record(
        &s.source_out,
        "graysurf__kit",
        "20260620-020000-skill-usage",
        &record_json("code-review", "2026-06-20T02:00:00Z"),
    );
    fs::create_dir_all(s.source_out.join("graysurf__kit/notes")).unwrap();
    write_catalog(&s.archive, &[archived_digest]);

    let report = prune_source::run(&args(&s.source_out, &s.archive, true, true)).unwrap();

    assert!(report.applied);
    assert_eq!(report.prunable, 1);
    assert_eq!(report.deleted, 1);
    assert!(
        !archived_dir.exists(),
        "archived source run directory is removed"
    );
    assert!(unarchived_dir.exists(), "unarchived run directory remains");
    assert!(
        s.source_out.join("graysurf__kit/notes").exists(),
        "non-record directories are never touched"
    );
}

#[cfg(unix)]
#[test]
fn prune_source_skips_symlinked_projects_and_records() {
    use std::os::unix::fs::symlink;

    let s = build_empty();
    let outside = tempfile::tempdir().unwrap();
    let (outside_run, _, outside_digest) = write_record(
        outside.path(),
        "graysurf__outside",
        "20260620-010000-skill-usage",
        &record_json("deliver-pr", "2026-06-20T01:00:00Z"),
    );
    symlink(
        outside.path().join("graysurf__outside"),
        s.source_out.join("graysurf__symlinked"),
    )
    .unwrap();

    let symlink_record_body = record_json("deliver-pr", "2026-06-20T02:00:00Z");
    let symlink_digest = format!("sha256:{}", sha256_hex(symlink_record_body.as_bytes()));
    let linked_record_target = outside.path().join("linked-record.json");
    fs::write(&linked_record_target, symlink_record_body).unwrap();
    let linked_record_run = s
        .source_out
        .join("graysurf__kit/20260620-020000-skill-usage");
    fs::create_dir_all(&linked_record_run).unwrap();
    symlink(
        &linked_record_target,
        linked_record_run.join("skill-usage.record.json"),
    )
    .unwrap();
    write_catalog(&s.archive, &[outside_digest, symlink_digest]);

    let report = prune_source::run(&args(&s.source_out, &s.archive, true, true)).unwrap();

    assert_eq!(report.scanned, 0);
    assert_eq!(report.deleted, 0);
    assert!(
        outside_run.exists(),
        "project symlink target must not be pruned"
    );
    assert!(
        linked_record_run.exists(),
        "run with symlinked record must not be pruned"
    );
}

#[test]
fn prune_source_repo_filter_limits_candidates_to_one_project() {
    let s = build_empty();
    let (kit_dir, _, kit_digest) = write_record(
        &s.source_out,
        "graysurf__kit",
        "20260620-010000-skill-usage",
        &record_json("deliver-pr", "2026-06-20T01:00:00Z"),
    );
    let (other_dir, _, other_digest) = write_record(
        &s.source_out,
        "sympoies__other",
        "20260620-020000-skill-usage",
        &record_json("deliver-pr", "2026-06-20T02:00:00Z"),
    );
    write_catalog(&s.archive, &[kit_digest, other_digest]);

    let mut prune_args = args(&s.source_out, &s.archive, true, true);
    prune_args.repo = Some("kit".to_string());
    let report = prune_source::run(&prune_args).unwrap();

    assert_eq!(report.scanned, 1);
    assert_eq!(report.deleted, 1);
    assert!(!kit_dir.exists());
    assert!(
        other_dir.exists(),
        "repo filter leaves other project intact"
    );
}

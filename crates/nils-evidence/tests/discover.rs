//! Contract coverage for `evidence discover` — the read-only scan that
//! classifies agent-out `skill-usage.record.json` files as `eligible`,
//! `blocked` (already in the archive catalog), or `unknown`.
//!
//! The command must never mutate the source tree or the archive, so every test
//! asserts the classification contract and, where a mutation would be visible,
//! that the fixture bytes survive the run unchanged.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use pretty_assertions::assert_eq;
use serde_json::Value;

fn evidence_bin() -> PathBuf {
    nils_test_support::bin::resolve("evidence")
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> Out {
    let output = Command::new(evidence_bin())
        .args(args)
        .output()
        .expect("evidence command");
    Out {
        code: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
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

fn v1_record(skill: &str) -> String {
    format!(
        r#"{{
  "schema": "skill-usage.record.v1",
  "producer": {{ "tool": "skill-usage", "nils_cli_version": "1.11.2" }},
  "skill": "{skill}",
  "started_at": "2026-06-20T01:00:00Z",
  "ended_at": "2026-06-20T01:05:00Z",
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

/// Write a raw record body at `<source_out>/<project>/<run>/skill-usage.record.json`
/// and return its `sha256:` digest, which is the archive dedup key.
fn write_record_at(source_out: &Path, project: &str, run_id: &str, body: &str) -> String {
    let dir = source_out.join(project).join(run_id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("skill-usage.record.json"), body).unwrap();
    format!("sha256:{}", sha256_hex(body.as_bytes()))
}

fn write_catalog_for_digests(archive: &Path, digests: &[&str]) {
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

/// An empty source tree plus an empty archive clone, both real directories so
/// resolution succeeds and only the classification logic is under test.
fn discover_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let source_out = tmp.path().join("out").join("projects");
    let archive = tmp.path().join("archive");
    fs::create_dir_all(&source_out).unwrap();
    fs::create_dir_all(archive.join("evidence")).unwrap();
    (tmp, source_out, archive)
}

fn discover_json(source_out: &Path, archive: &Path, extra: &[&str]) -> (i32, Value) {
    let mut args = vec![
        "discover".to_string(),
        "--source-out".to_string(),
        source_out.to_string_lossy().to_string(),
        "--archive".to_string(),
        archive.to_string_lossy().to_string(),
        "--format".to_string(),
        "json".to_string(),
    ];
    args.extend(extra.iter().map(|s| (*s).to_string()));
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    let out = run(&borrowed);
    let value = serde_json::from_str(out.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "json parse {e}: stdout={} stderr={}",
            out.stdout, out.stderr
        )
    });
    (out.code, value)
}

fn summary(value: &Value) -> (u64, u64, u64) {
    let summary = &value["data"]["summary"];
    (
        summary["eligible"].as_u64().unwrap(),
        summary["blocked"].as_u64().unwrap(),
        summary["unknown"].as_u64().unwrap(),
    )
}

#[test]
fn discover_json_separates_eligible_from_catalog_blocked() {
    let (_tmp, source_out, archive) = discover_fixture();
    let blocked_digest =
        write_record_at(&source_out, "kit", "run-blocked", &v1_record("deliver-pr"));
    write_record_at(
        &source_out,
        "kit",
        "run-eligible",
        &v1_record("code-review"),
    );
    write_catalog_for_digests(&archive, &[blocked_digest.as_str()]);

    let (code, value) = discover_json(&source_out, &archive, &[]);

    assert_eq!(code, 0);
    assert_eq!(
        value["schema_version"].as_str().unwrap(),
        "cli.evidence.discover.v1"
    );
    assert_eq!(value["ok"].as_bool().unwrap(), true);
    assert_eq!(summary(&value), (1, 1, 0));
    assert_eq!(
        value["data"]["source_out"].as_str().unwrap(),
        source_out.to_string_lossy()
    );
    assert_eq!(
        value["data"]["archive"].as_str().unwrap(),
        archive.to_string_lossy()
    );

    let candidates = value["data"]["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    let blocked = candidates
        .iter()
        .find(|c| c["classification"] == "blocked")
        .expect("blocked candidate");
    assert_eq!(blocked["skill"].as_str().unwrap(), "deliver-pr");
    assert_eq!(blocked["source_digest"].as_str().unwrap(), blocked_digest);
    assert_eq!(
        blocked["reason"].as_str().unwrap(),
        "already archived (catalog)"
    );

    let eligible = candidates
        .iter()
        .find(|c| c["classification"] == "eligible")
        .expect("eligible candidate");
    assert_eq!(eligible["skill"].as_str().unwrap(), "code-review");
    assert!(
        eligible["source_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    // `reason` is skipped when absent, so an eligible row carries no reason.
    assert_eq!(eligible.get("reason"), None);
}

#[test]
fn discover_counts_unknown_records_but_hides_them_by_default() {
    let (_tmp, source_out, archive) = discover_fixture();
    write_record_at(&source_out, "kit", "run-ok", &v1_record("deliver-pr"));
    write_record_at(&source_out, "kit", "run-bad-json", "{ not json");

    let (code, value) = discover_json(&source_out, &archive, &[]);

    assert_eq!(code, 0);
    // The unknown record is still counted; only the row is withheld.
    assert_eq!(summary(&value), (1, 0, 1));
    let candidates = value["data"]["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["classification"], "eligible");
}

#[test]
fn discover_include_unknown_reports_each_rejection_reason() {
    let (_tmp, source_out, archive) = discover_fixture();
    write_record_at(&source_out, "kit", "run-bad-json", "{ not json");
    write_record_at(&source_out, "kit", "run-no-skill", &v1_record(""));
    write_record_at(
        &source_out,
        "kit",
        "run-future",
        &v1_record("x").replace("skill-usage.record.v1", "skill-usage.record.v9"),
    );

    let (code, value) = discover_json(&source_out, &archive, &["--include-unknown"]);

    assert_eq!(code, 0);
    assert_eq!(summary(&value), (0, 0, 3));
    let candidates = value["data"]["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 3);
    for candidate in candidates {
        assert_eq!(candidate["classification"], "unknown");
        // An unknown row can name neither an owner nor a dedup digest.
        assert_eq!(candidate.get("skill"), None);
        assert_eq!(candidate.get("source_digest"), None);
    }

    let reason_for = |run_id: &str| -> String {
        candidates
            .iter()
            .find(|c| c["source_path"].as_str().unwrap().contains(run_id))
            .unwrap_or_else(|| panic!("candidate for {run_id}"))["reason"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(
        reason_for("run-bad-json").starts_with("skill-usage.record.json parse:"),
        "unexpected reason: {}",
        reason_for("run-bad-json")
    );
    assert_eq!(
        reason_for("run-no-skill"),
        "v1 record is missing skill ownership"
    );
    assert_eq!(
        reason_for("run-future"),
        "unsupported source schema `skill-usage.record.v9`"
    );
}

#[test]
fn discover_reports_unreadable_records_as_unknown() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let (_tmp, source_out, archive) = discover_fixture();
        write_record_at(&source_out, "kit", "run-locked", &v1_record("deliver-pr"));
        let locked = source_out
            .join("kit")
            .join("run-locked")
            .join("skill-usage.record.json");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        // A privileged test runner can still read a 0o000 file, which would make
        // the read-failure branch unreachable rather than wrong.
        if fs::read(&locked).is_ok() {
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
            return;
        }

        let (code, value) = discover_json(&source_out, &archive, &["--include-unknown"]);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(code, 0);
        assert_eq!(summary(&value), (0, 0, 1));
        let candidates = value["data"]["candidates"].as_array().unwrap();
        assert!(
            candidates[0]["reason"]
                .as_str()
                .unwrap()
                .starts_with("read failed:"),
            "unexpected reason: {}",
            candidates[0]["reason"]
        );
    }
}

#[test]
fn discover_leaves_source_and_archive_untouched() {
    let (_tmp, source_out, archive) = discover_fixture();
    let body = v1_record("deliver-pr");
    write_record_at(&source_out, "kit", "run-a", &body);
    write_catalog_for_digests(&archive, &["sha256:unrelated"]);
    let record_path = source_out
        .join("kit")
        .join("run-a")
        .join("skill-usage.record.json");
    let catalog_path = archive.join("catalog.json");
    let record_before = fs::read(&record_path).unwrap();
    let catalog_before = fs::read(&catalog_path).unwrap();

    let (code, value) = discover_json(&source_out, &archive, &["--include-unknown"]);

    assert_eq!(code, 0);
    assert_eq!(summary(&value), (1, 0, 0));
    assert_eq!(fs::read(&record_path).unwrap(), record_before);
    assert_eq!(fs::read(&catalog_path).unwrap(), catalog_before);
    // Discovery must not create an archive entry for the eligible record.
    assert_eq!(fs::read_dir(archive.join("evidence")).unwrap().count(), 0);
}

#[test]
fn discover_text_lists_every_classification() {
    let (_tmp, source_out, archive) = discover_fixture();
    let blocked_digest =
        write_record_at(&source_out, "kit", "run-blocked", &v1_record("deliver-pr"));
    write_record_at(
        &source_out,
        "kit",
        "run-eligible",
        &v1_record("code-review"),
    );
    write_record_at(&source_out, "kit", "run-bad", "{ not json");
    write_catalog_for_digests(&archive, &[blocked_digest.as_str()]);

    let out = run(&[
        "discover",
        "--source-out",
        &source_out.to_string_lossy(),
        "--archive",
        &archive.to_string_lossy(),
        "--include-unknown",
        "--format",
        "text",
    ]);

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let mut lines = out.stdout.lines();
    assert_eq!(
        lines.next().unwrap(),
        "discover: 1 eligible, 1 blocked, 1 unknown"
    );
    let rest = lines.collect::<Vec<_>>();
    assert_eq!(rest.len(), 3);
    assert!(rest.iter().any(|l| l.starts_with("  [eligible] ")));
    assert!(rest.iter().any(|l| l.starts_with("  [blocked] ")));
    assert!(rest.iter().any(|l| l.starts_with("  [unknown] ")));
}

#[test]
fn discover_text_omits_unknown_rows_without_the_flag() {
    let (_tmp, source_out, archive) = discover_fixture();
    write_record_at(&source_out, "kit", "run-bad", "{ not json");

    let out = run(&[
        "discover",
        "--source-out",
        &source_out.to_string_lossy(),
        "--archive",
        &archive.to_string_lossy(),
        "--format",
        "text",
    ]);

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert_eq!(out.stdout, "discover: 0 eligible, 0 blocked, 1 unknown\n");
}

#[test]
fn discover_missing_source_out_fails_closed() {
    let (tmp, _source_out, archive) = discover_fixture();
    let missing = tmp.path().join("no-such-out");

    let out = run(&[
        "discover",
        "--source-out",
        &missing.to_string_lossy(),
        "--archive",
        &archive.to_string_lossy(),
        "--format",
        "json",
    ]);

    assert_eq!(out.code, 65);
    let value: Value = serde_json::from_str(out.stderr.trim()).expect("json on stderr");
    assert_eq!(value["ok"].as_bool().unwrap(), false);
    assert_eq!(
        value["schema_version"].as_str().unwrap(),
        "cli.evidence.discover.v1"
    );
    assert_eq!(
        value["error"]["code"].as_str().unwrap(),
        "discover-source-out-missing"
    );
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&*missing.to_string_lossy()),
        "message should name the missing path"
    );
}

#[test]
fn discover_missing_archive_fails_closed_in_text_mode() {
    let (tmp, source_out, _archive) = discover_fixture();
    let missing = tmp.path().join("no-such-archive");

    let out = run(&[
        "discover",
        "--source-out",
        &source_out.to_string_lossy(),
        "--archive",
        &missing.to_string_lossy(),
        "--format",
        "text",
    ]);

    assert_eq!(out.code, 65);
    assert_eq!(out.stdout, "");
    assert!(
        out.stderr
            .starts_with("error [discover-archive-clone-missing]: "),
        "unexpected stderr: {}",
        out.stderr
    );
}

#[test]
fn discover_unreadable_catalog_is_an_io_error() {
    let (_tmp, source_out, archive) = discover_fixture();
    write_record_at(&source_out, "kit", "run-a", &v1_record("deliver-pr"));
    fs::write(archive.join("catalog.json"), "{ not json").unwrap();

    let out = run(&[
        "discover",
        "--source-out",
        &source_out.to_string_lossy(),
        "--archive",
        &archive.to_string_lossy(),
        "--format",
        "json",
    ]);

    assert_eq!(out.code, 65);
    let value: Value = serde_json::from_str(out.stderr.trim()).expect("json on stderr");
    assert_eq!(
        value["error"]["code"].as_str().unwrap(),
        "discover-io-error"
    );
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("catalog parse"),
        "unexpected message: {}",
        value["error"]["message"]
    );
}

#[test]
fn discover_finds_records_nested_under_a_workflow_run_directory() {
    let (_tmp, source_out, archive) = discover_fixture();
    let nested = source_out.join("kit").join("run-a").join("skill-usage");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        nested.join("skill-usage.record.json"),
        v1_record("deliver-pr"),
    )
    .unwrap();

    let (code, value) = discover_json(&source_out, &archive, &[]);

    assert_eq!(code, 0);
    assert_eq!(summary(&value), (1, 0, 0));
    assert!(
        value["data"]["candidates"][0]["source_path"]
            .as_str()
            .unwrap()
            .ends_with("skill-usage/skill-usage.record.json")
    );
}

#[test]
fn discover_empty_source_tree_reports_an_empty_inventory() {
    let (_tmp, source_out, archive) = discover_fixture();

    let (code, value) = discover_json(&source_out, &archive, &["--include-unknown"]);

    assert_eq!(code, 0);
    assert_eq!(summary(&value), (0, 0, 0));
    assert_eq!(
        value["data"]["candidates"].as_array().unwrap().len(),
        0,
        "an empty tree must still emit the candidates array"
    );
}

//! End-to-end coverage driving the `evidence` binary, asserting the JSON
//! Envelope contract (`cli.evidence.<cmd>.v1`) and the catalog/query/search/
//! validate surfaces against a fixture archive.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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

fn write_rollup(
    archive: &Path,
    id: &str,
    schema: &str,
    skill: &str,
    status: &str,
    intent: &str,
    digest: &str,
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
            "archived_at": "2026-06-14T11:00:00Z",
            "skill": "{skill}",
            "intent": "{intent}",
            "trigger": "user_explicit",
            "repo": {{ "host": "github.com", "org": "graysurf", "repo": "kit" }},
            "cwd": "~/Project/kit",
            "started_at": "2026-06-14T10:00:00Z",
            "ended_at": "2026-06-14T10:30:00Z",
            "outcome": {{ "status": "{status}", "summary": "completed via a clean rebase" }},
            "producer": {{ "tool": "skill-usage", "nils_cli_version": "1.4.0" }},
            "counts": {{ "validation": 2, "failures": 0 }},
            "linked_evidence": [],
            "source_digest": "{digest}"
        }}"#
    );
    fs::write(dir.join("skill-usage.rollup.json"), body).unwrap();
}

fn fixture_archive() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let archive = tmp.path().join("archive");
    fs::create_dir_all(archive.join("evidence")).unwrap();
    fs::create_dir_all(archive.join("config")).unwrap();
    fs::write(
        archive.join("config").join("hosts.yaml"),
        "version: 1\nhosts:\n  github.com:\n    class: personal\n",
    )
    .unwrap();
    write_rollup(
        &archive,
        "id-pass",
        "skill-usage.rollup.v1",
        "deliver-pr",
        "pass",
        "deliver the rollback plan",
        "sha256:p",
    );
    write_rollup(
        &archive,
        "id-fail",
        "skill-usage.rollup.v1",
        "code-review",
        "fail",
        "review diff",
        "sha256:f",
    );
    write_rollup(
        &archive,
        "id-future",
        "skill-usage.rollup.v2",
        "future",
        "pass",
        "future record",
        "sha256:x",
    );
    (tmp, archive)
}

fn arc(archive: &Path) -> String {
    archive.to_string_lossy().to_string()
}

#[test]
fn validate_hosts_json_envelope() {
    let (_tmp, archive) = fixture_archive();
    let hosts = archive.join("config").join("hosts.yaml");
    let out = run(&[
        "validate-hosts",
        "--input",
        &hosts.to_string_lossy(),
        "--format",
        "json",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let v: Value = serde_json::from_str(out.stdout.trim()).expect("json");
    assert_eq!(v["schema_version"], "cli.evidence.validate-hosts.v1");
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"]["summary"]["host_count"], 1);
}

#[test]
fn validate_local_missing_file_defaults() {
    let out = run(&[
        "validate-local",
        "--input",
        "/nonexistent/x.yaml",
        "--format",
        "json",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let v: Value = serde_json::from_str(out.stdout.trim()).expect("json");
    assert_eq!(v["schema_version"], "cli.evidence.validate-local.v1");
    assert_eq!(v["data"]["source"], "Defaults");
    assert_eq!(v["data"]["config"]["performance"]["migrate_batch_size"], 50);
}

#[test]
fn validate_record_accepts_and_warns() {
    let (_tmp, archive) = fixture_archive();
    let rollup = archive.join("evidence/github.com/graysurf/kit/id-future/skill-usage.rollup.json");
    let out = run(&[
        "validate-record",
        "--input",
        &rollup.to_string_lossy(),
        "--format",
        "json",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let v: Value = serde_json::from_str(out.stdout.trim()).expect("json");
    assert_eq!(v["schema_version"], "cli.evidence.validate-record.v1");
    // v2 schema -> out-of-range warning, but still validates structurally.
    let warnings = v["warnings"].as_array().expect("warnings");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("out-of-range"))
    );
}

#[test]
fn catalog_write_is_deterministic() {
    let (_tmp, archive) = fixture_archive();
    let out1 = run(&[
        "catalog",
        "--write",
        "--archive",
        &arc(&archive),
        "--format",
        "json",
    ]);
    assert_eq!(out1.code, 0, "stderr={}", out1.stderr);
    let catalog1 = fs::read_to_string(archive.join("catalog.json")).unwrap();
    let out2 = run(&["catalog", "--write", "--archive", &arc(&archive)]);
    assert_eq!(out2.code, 0);
    let catalog2 = fs::read_to_string(archive.join("catalog.json")).unwrap();
    assert_eq!(catalog1, catalog2, "catalog must be byte-identical");

    let v: Value = serde_json::from_str(out1.stdout.trim()).expect("json");
    assert_eq!(v["schema_version"], "cli.evidence.catalog.v1");
    // All three rollups (including v2) are counted honestly.
    assert_eq!(v["data"]["total_records"], 3);

    // The committed catalog declares the catalog schema + source_digest column.
    let doc: Value = serde_json::from_str(&catalog1).unwrap();
    assert_eq!(doc["schema_version"], "evidence.catalog.v1");
    assert!(doc["records"][0].get("source_digest").is_some());
    assert!(doc["records"][0].get("record_schema").is_some());
    assert!(doc["records"][0].get("producer_version").is_some());
}

#[test]
fn query_reports_out_of_range_and_counts() {
    let (_tmp, archive) = fixture_archive();
    let out = run(&["query", "--archive", &arc(&archive), "--format", "json"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let v: Value = serde_json::from_str(out.stdout.trim()).expect("json");
    assert_eq!(v["schema_version"], "cli.evidence.query.v1");
    assert_eq!(v["data"]["counts"]["scanned"], 3);
    assert_eq!(v["data"]["counts"]["returned"], 2);
    assert_eq!(v["data"]["counts"]["out_of_range"], 1);
    let warnings = v["data"]["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0]["code"], "record-schema-version-out-of-range");
    assert_eq!(warnings[0]["schema_found"], "skill-usage.rollup.v2");
}

#[test]
fn query_filters_by_outcome() {
    let (_tmp, archive) = fixture_archive();
    let out = run(&[
        "query",
        "--archive",
        &arc(&archive),
        "--outcome",
        "fail",
        "--format",
        "json",
    ]);
    let v: Value = serde_json::from_str(out.stdout.trim()).expect("json");
    assert_eq!(v["data"]["counts"]["returned"], 1);
    assert_eq!(v["data"]["records"][0]["outcome_status"], "fail");
}

#[test]
fn search_substring_over_intent_and_summary() {
    let (_tmp, archive) = fixture_archive();
    // "rollback" appears in id-pass intent.
    let out = run(&[
        "search",
        "rollback",
        "--archive",
        &arc(&archive),
        "--format",
        "json",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let v: Value = serde_json::from_str(out.stdout.trim()).expect("json");
    assert_eq!(v["schema_version"], "cli.evidence.search.v1");
    let hits = v["data"]["hits"].as_array().unwrap();
    assert!(hits.iter().any(|h| h["field"] == "intent"));

    // "rebase" appears in every outcome_summary.
    let out2 = run(&[
        "search",
        "rebase",
        "--archive",
        &arc(&archive),
        "--format",
        "json",
    ]);
    let v2: Value = serde_json::from_str(out2.stdout.trim()).expect("json");
    let hits2 = v2["data"]["hits"].as_array().unwrap();
    assert!(hits2.iter().all(|h| h["field"] == "outcome_summary"));
    assert!(hits2.len() >= 2);
}

#[test]
fn unknown_subcommand_emits_envelope_error() {
    let out = run(&["bogus", "--format", "json"]);
    assert_ne!(out.code, 0);
    // The shared contract writes the parse-error envelope to stdout.
    let v: Value = serde_json::from_str(out.stdout.trim()).expect("json on stdout");
    assert_eq!(v["ok"], false);
    assert_eq!(v["schema_version"], "cli.evidence.error.v1");
    assert_eq!(v["error"]["code"], "unknown-subcommand");
}

#[test]
fn completion_emits_scripts() {
    let bash = run(&["completion", "bash"]);
    assert_eq!(bash.code, 0, "stderr={}", bash.stderr);
    assert!(bash.stdout.contains("evidence"));
    let zsh = run(&["completion", "zsh"]);
    assert_eq!(zsh.code, 0);
    assert!(zsh.stdout.contains("evidence"));
}

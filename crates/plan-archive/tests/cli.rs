//! Binary-level CLI contract coverage for `plan-archive`.
//!
//! The module-level tests exercise the underlying validators and
//! refresh/migrate pipelines directly. These cases keep the public
//! command-line dispatch, JSON envelopes, stdin handling, and parse-error
//! behavior covered as shipped.

use std::fs;
use std::path::{Path, PathBuf};

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn run(args: &[&str]) -> CmdOutput {
    let tmp = tempfile::tempdir().expect("tempdir");
    let options = CmdOptions::new().with_cwd(tmp.path());
    run_resolved("plan-archive", args, &options)
}

fn run_with_stdin(args: &[&str], stdin: &str) -> CmdOutput {
    let tmp = tempfile::tempdir().expect("tempdir");
    let options = CmdOptions::new().with_cwd(tmp.path()).with_stdin_str(stdin);
    run_resolved("plan-archive", args, &options)
}

fn fixture(rel: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(rel)
        .display()
        .to_string()
}

struct Archive {
    _tmp: tempfile::TempDir,
    path: PathBuf,
    plan_rel: String,
}

fn seed_snapshot(archive: &Path, rel_dir: &str, stamp: &str, body: &str) {
    let dir = archive.join("_index").join(rel_dir);
    fs::create_dir_all(&dir).expect("snapshot dir");
    fs::write(dir.join(format!("{stamp}.json")), body).expect("snapshot");
}

fn seed_metadata(archive: &Path, slug: &str, refs: &str) -> String {
    let plan_rel = format!("plans/github.com/graysurf/agent-runtime-kit/{slug}");
    let dir = archive.join(&plan_rel);
    fs::create_dir_all(&dir).expect("plan dir");
    fs::write(
        dir.join("metadata.yaml"),
        format!(
            "version: 1\nsource:\n  host: github.com\n  org_or_group_path: graysurf\n  repo: agent-runtime-kit\n  branch: main\n  archive_commit: abc123\n  original_path: docs/plans/{slug}/\ncaptured_classification:\n  class: personal\nrefs:\n{refs}"
        ),
    )
    .expect("metadata");
    plan_rel
}

fn build_query_catalog_archive() -> Archive {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("archive");
    fs::create_dir_all(&path).expect("archive");
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
    let plan_rel = seed_metadata(
        &path,
        "2026-05-27-cli-query-catalog",
        "  issue: https://github.com/graysurf/agent-runtime-kit/issues/126\n  pr: https://github.com/graysurf/agent-runtime-kit/pull/127\n",
    );
    seed_metadata(
        &path,
        "2026-05-28-cli-query-missing-snapshot",
        "  issue: https://github.com/graysurf/agent-runtime-kit/issues/999\n",
    );
    Archive {
        _tmp: tmp,
        path,
        plan_rel,
    }
}

fn json_stdout(output: &CmdOutput) -> Value {
    serde_json::from_str(&output.stdout_text()).expect("stdout should be json")
}

fn json_stderr(output: &CmdOutput) -> Value {
    serde_json::from_str(&output.stderr_text()).expect("stderr should be json")
}

#[test]
fn validate_hosts_text_reads_stdin_and_reports_summary() {
    let hosts = "\
version: 1
hosts:
  github.com:
    class: personal
    primary_identity: graysurf
  gitlab.com:
    class: employer
    employer: ExampleCorp
    retention: delete-on-termination
";

    let output = run_with_stdin(&["validate-hosts", "--input", "-"], hosts);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("hosts: 2 entries (1 personal, 1 employer)"));
    assert!(stdout.contains("github.com: class=personal"));
    assert!(stdout.contains(
        "gitlab.com: class=employer employer=ExampleCorp retention=delete-on-termination"
    ));
    assert_eq!(output.stderr_text(), "");
}

#[test]
fn validate_metadata_json_preserves_warning_envelope() {
    let input = fixture("metadata/orphan-plan.yaml");
    let output = run(&["--format", "json", "validate-metadata", "--input", &input]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let json = json_stdout(&output);
    assert_eq!(
        json["schema_version"],
        "cli.plan-archive.validate-metadata.v1"
    );
    assert_eq!(json["ok"].as_bool(), Some(true));
    assert_eq!(
        json["data"]["config"]["source"]["original_path"],
        "docs/plans/2026-01-15-orphan-experiment/"
    );
    let warning = json["warnings"][0].as_str().expect("warning");
    assert!(warning.starts_with("[metadata-captured-classification-missing]"));
    assert!(warning.contains("pre-classification plan"));
}

#[test]
fn refresh_no_selector_json_uses_refresh_error_contract() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let archive = tmp.path().join("archive");
    std::fs::create_dir_all(archive.join("config")).expect("archive config");
    std::fs::write(
        archive.join("config/hosts.yaml"),
        "version: 1\nhosts:\n  github.com:\n    class: personal\n",
    )
    .expect("hosts");
    let archive_arg = archive.display().to_string();

    let output = run(&["--format", "json", "refresh", "--archive", &archive_arg]);

    assert_eq!(output.code, 65, "stdout={}", output.stdout_text());
    assert_eq!(output.stdout_text(), "");
    let json = json_stderr(&output);
    assert_eq!(json["schema_version"], "cli.plan-archive.refresh.v1");
    assert_eq!(json["ok"].as_bool(), Some(false));
    assert_eq!(json["error"]["code"], "refresh-no-selector");
}

#[test]
fn unknown_subcommand_json_exits_usage_with_contract_error() {
    let output = run(&["--format", "json", "bogus-command"]);

    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    assert_eq!(output.stderr_text(), "");
    let json = json_stdout(&output);
    assert_eq!(json["schema_version"], "cli.plan-archive.error.v1");
    assert_eq!(json["ok"].as_bool(), Some(false));
    assert_eq!(json["error"]["code"], "unknown-subcommand");
}

#[test]
fn completion_bash_lists_plan_archive_subcommands() {
    let output = run(&["completion", "bash"]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("validate-hosts"));
    assert!(stdout.contains("migrate"));
    assert!(stdout.contains("query"));
    assert!(stdout.contains("catalog"));
    assert_eq!(output.stderr_text(), "");
}

#[test]
fn query_single_ref_text_reports_latest_snapshot() {
    let archive = build_query_catalog_archive();
    let archive_arg = archive.path.display().to_string();
    let url = "https://github.com/graysurf/agent-runtime-kit/issues/126";

    let output = run(&["query", "--ref", url, "--archive", &archive_arg]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains(url));
    assert!(stdout.contains("fetched_at=2026-05-27T01:00:00Z"));
    assert!(stdout.contains("20260527T010000Z.json"));
}

#[test]
fn query_plan_json_resolves_metadata_refs() {
    let archive = build_query_catalog_archive();
    let archive_arg = archive.path.display().to_string();

    let output = run(&[
        "--format",
        "json",
        "query",
        "--plan",
        &archive.plan_rel,
        "--archive",
        &archive_arg,
    ]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let json = json_stdout(&output);
    assert_eq!(json["schema_version"], "cli.plan-archive.query.v1");
    assert_eq!(json["data"]["mode"], "plan_link");
    assert_eq!(json["data"]["plan"], archive.plan_rel);
    assert_eq!(json["data"]["records"].as_array().unwrap().len(), 2);
    assert_eq!(json["data"]["records"][0]["number"], 126);
    assert_eq!(json["data"]["records"][1]["number"], 127);
}

#[test]
fn catalog_write_text_persists_catalog_and_reports_records() {
    let archive = build_query_catalog_archive();
    let archive_arg = archive.path.display().to_string();

    let output = run(&["catalog", "--write", "--archive", &archive_arg]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("catalog written:"));
    assert!(
        stdout.contains(
            "2026-05-27-cli-query-catalog  github.com/graysurf/agent-runtime-kit  refs=2"
        )
    );
    assert!(archive.path.join("catalog.json").exists());
}

#[test]
fn catalog_refs_to_json_filters_by_canonical_ref() {
    let archive = build_query_catalog_archive();
    let archive_arg = archive.path.display().to_string();

    let output = run(&[
        "--format",
        "json",
        "catalog",
        "--refs-to",
        "https://github.com/graysurf/agent-runtime-kit/issues/126",
        "--archive",
        &archive_arg,
    ]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let json = json_stdout(&output);
    assert_eq!(json["schema_version"], "cli.plan-archive.catalog.v1");
    assert_eq!(json["data"]["total_records"], 2);
    assert_eq!(json["data"]["records"].as_array().unwrap().len(), 1);
    assert_eq!(
        json["data"]["records"][0]["slug"],
        "2026-05-27-cli-query-catalog"
    );
}

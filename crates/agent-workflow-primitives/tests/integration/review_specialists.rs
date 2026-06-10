use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use nils_common::cli_contract::exit;
use nils_test_support::cmd::{CmdOutput, run_resolved_in_dir};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/review-specialists")
        .join(name)
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> CmdOutput {
    let agent_home = dir.join(".agent-home");
    let agent_home_value = agent_home.to_string_lossy().to_string();
    run_resolved_in_dir(bin, dir, args, &[("AGENT_HOME", &agent_home_value)], None)
}

fn path_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[test]
fn review_specialists_validate_normalizes_findings() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let input = path_arg(&fixture("findings.valid.jsonl"));

    let output = run(
        "review-specialists",
        tmp.path(),
        &["validate", "--input", &input, "--format", "json"],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(
        value["schema_version"],
        "cli.review-specialists.validate.v1"
    );
    assert_eq!(value["data"]["schema"], "review-specialists.findings.v1");
    assert_eq!(value["data"]["findings_count"], 2);
    assert_eq!(value["data"]["findings"][0]["severity"], "high");
    assert_eq!(value["data"]["findings"][1]["severity"], "info");
}

#[test]
fn review_specialists_validate_rejects_bad_rows_with_data_exit() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let input = path_arg(&fixture("findings.invalid.jsonl"));

    let output = run(
        "review-specialists",
        tmp.path(),
        &["validate", "--input", &input],
    );

    assert_eq!(output.code, exit::DATA, "stderr={}", output.stderr_text());
    assert!(output.stderr_text().contains("invalid-findings"));
    assert!(output.stderr_text().contains("missing required field"));
}

#[test]
fn review_specialists_validate_lines_rejects_missing_paths() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let input = tmp.path().join("findings.jsonl");
    fs::write(
        &input,
        r#"{"severity":"high","confidence":0.8,"path":"missing.rs","line":10,"summary":"Missing path should fail line validation.","evidence":"The file does not exist under the repo root.","recommendation":"Report a validation error.","specialist":"testing"}"#,
    )
    .expect("write findings");
    let input_arg = path_arg(&input);
    let repo_arg = path_arg(tmp.path());

    let output = run(
        "review-specialists",
        tmp.path(),
        &[
            "validate",
            "--input",
            &input_arg,
            "--repo",
            &repo_arg,
            "--validate-lines",
        ],
    );

    assert_eq!(output.code, exit::DATA, "stderr={}", output.stderr_text());
    assert!(output.stderr_text().contains("failed to read"));
    assert!(output.stderr_text().contains("line validation"));
}

#[test]
fn review_specialists_merge_dedupes_and_writes_summary() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let input = path_arg(&fixture("findings.duplicates.jsonl"));
    let summary = tmp.path().join("review.md");
    let summary_arg = path_arg(&summary);

    let output = run(
        "review-specialists",
        tmp.path(),
        &[
            "merge",
            "--input",
            &input,
            "--summary-out",
            &summary_arg,
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["data"]["counts"]["input_rows"], 3);
    assert_eq!(value["data"]["counts"]["merged"], 2);
    assert_eq!(value["data"]["counts"]["displayed"], 1);
    assert_eq!(value["data"]["counts"]["suppressed"], 1);
    assert_eq!(
        value["data"]["findings"][0]["confirming_specialists"]
            .as_array()
            .expect("specialists")
            .len(),
        2
    );
    let summary_body = fs::read_to_string(summary).expect("summary");
    assert!(summary_body.contains("Specialist Review Report"));
    assert!(summary_body.contains("api-contract"));
}

#[test]
fn review_specialists_render_issue_body_uses_github_links_from_envelope() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let input = path_arg(&fixture("findings.duplicates.jsonl"));
    let merge = run(
        "review-specialists",
        tmp.path(),
        &["merge", "--input", &input, "--format", "json"],
    );
    assert_eq!(merge.code, 0, "stderr={}", merge.stderr_text());
    let merged_path = tmp.path().join("merged-envelope.json");
    fs::write(&merged_path, merge.stdout_text()).expect("merged");
    let merged_arg = path_arg(&merged_path);

    let render = run(
        "review-specialists",
        tmp.path(),
        &[
            "render",
            "--profile",
            "issue-body",
            "--input",
            &merged_arg,
            "--repo",
            "sympoies/nils-cli",
            "--ref",
            "abc123",
        ],
    );

    assert_eq!(render.code, 0, "stderr={}", render.stderr_text());
    assert!(render.stdout_text().contains("## Current Behavior"));
    assert!(
        render
            .stdout_text()
            .contains("https://github.com/sympoies/nils-cli/blob/abc123/src/api/users.rs#L42")
    );
    assert!(
        render
            .stdout_text()
            .contains("No provider action was taken")
    );
}

#[test]
fn review_specialists_render_evidence_includes_traceability_metadata() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let input = path_arg(&fixture("findings.duplicates.jsonl"));
    let merge = run(
        "review-specialists",
        tmp.path(),
        &["merge", "--input", &input, "--format", "json"],
    );
    assert_eq!(merge.code, 0, "stderr={}", merge.stderr_text());
    let merged_path = tmp.path().join("merged-envelope.json");
    fs::write(&merged_path, merge.stdout_text()).expect("merged");
    let merged_arg = path_arg(&merged_path);

    let render = run(
        "review-specialists",
        tmp.path(),
        &["render", "--profile", "evidence", "--input", &merged_arg],
    );

    assert_eq!(render.code, 0, "stderr={}", render.stderr_text());
    let value: Value = serde_json::from_str(&render.stdout_text()).expect("evidence json");
    assert_eq!(value["schema"], "review-specialists.evidence.v1");
    assert_eq!(
        value["artifacts"]["input_files"]
            .as_array()
            .expect("input files")
            .len(),
        1
    );
    assert_eq!(
        value["suppressed_findings"]
            .as_array()
            .expect("suppressed findings")
            .len(),
        1
    );
}

#[test]
fn review_specialists_skill_helper_parity_fixture_renders_report() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let input = path_arg(&fixture("skill-helper-parity.jsonl"));
    let merge = run(
        "review-specialists",
        tmp.path(),
        &["merge", "--input", &input, "--format", "json"],
    );
    assert_eq!(merge.code, 0, "stderr={}", merge.stderr_text());
    let merged_path = tmp.path().join("merged-envelope.json");
    fs::write(&merged_path, merge.stdout_text()).expect("merged");
    let merged_arg = path_arg(&merged_path);

    let render = run(
        "review-specialists",
        tmp.path(),
        &["render", "--profile", "report", "--input", &merged_arg],
    );

    assert_eq!(render.code, 0, "stderr={}", render.stderr_text());
    assert!(render.stdout_text().contains("Specialist Review Report"));
    assert!(
        render
            .stdout_text()
            .contains("Response shape changed without migration guidance.")
    );
    assert!(render.stdout_text().contains("api-contract"));
}

#[test]
fn review_specialists_bundle_writes_stable_artifacts() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let input = path_arg(&fixture("findings.valid.jsonl"));
    let out_dir = tmp.path().join("bundle");
    let out_arg = path_arg(&out_dir);

    let output = run(
        "review-specialists",
        tmp.path(),
        &[
            "bundle",
            "--input",
            &input,
            "--out-dir",
            &out_arg,
            "--profile",
            "issue-body",
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(out_dir.join("findings.normalized.jsonl").is_file());
    assert!(out_dir.join("findings.merged.json").is_file());
    assert!(out_dir.join("specialist-review.md").is_file());
    assert!(out_dir.join("issue-body.md").is_file());
}

#[test]
fn review_specialists_scope_classifies_git_diff() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    init_git_fixture(tmp.path());
    fs::write(
        tmp.path().join("src/api.rs"),
        "pub fn answer() -> u8 {\n    43\n}\n",
    )
    .expect("write api");
    fs::write(
        tmp.path().join("tests/api_test.rs"),
        "#[test]\nfn answer_is_stable() {\n    assert_eq!(43, 43);\n}\n",
    )
    .expect("write test");

    let output = run(
        "review-specialists",
        tmp.path(),
        &["scope", "--base", "HEAD", "--format", "json"],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["data"]["schema"], "review-specialists.scope.v1");
    assert_eq!(value["data"]["scope_backend"], true);
    assert!(
        value["data"]["stack"]
            .as_array()
            .expect("stack")
            .contains(&Value::String("rust".to_string()))
    );
    assert!(
        value["data"]["test_framework"]
            .as_array()
            .expect("test framework")
            .contains(&Value::String("cargo test".to_string()))
    );
    assert!(
        value["data"]["suggested_specialists"]
            .as_array()
            .expect("specialists")
            .contains(&Value::String("testing".to_string()))
    );
}

fn init_git_fixture(path: &Path) {
    fs::create_dir_all(path.join("src")).expect("src");
    fs::create_dir_all(path.join("tests")).expect("tests");
    fs::write(
        path.join("src/api.rs"),
        "pub fn answer() -> u8 {\n    42\n}\n",
    )
    .expect("api");
    fs::write(
        path.join("tests/api_test.rs"),
        "#[test]\nfn answer_is_stable() {\n    assert_eq!(42, 42);\n}\n",
    )
    .expect("test");
    git(path, &["init"]);
    git(path, &["config", "user.email", "tester@example.com"]);
    git(path, &["config", "user.name", "Tester"]);
    git(path, &["add", "."]);
    git(path, &["commit", "-m", "initial"]);
}

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout={}\nstderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

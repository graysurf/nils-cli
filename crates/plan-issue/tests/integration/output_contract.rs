use std::fs;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::common;
#[test]
fn output_json_contract_success_envelope_contains_version_status_and_payload() {
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "build-task-spec",
        "--plan",
        "crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
        "--sprint",
        "2",
        "--pr-grouping",
        "per-sprint",
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert!(
        out.stderr_text().trim().is_empty(),
        "stderr should be empty: {}",
        out.stderr_text()
    );

    let payload: Value = serde_json::from_str(&out.stdout_text()).expect("stdout should be JSON");
    assert!(
        payload["schema_version"]
            .as_str()
            .is_some_and(|value| value.starts_with("plan-issue.")),
        "{}",
        out.stdout_text()
    );
    assert_eq!(payload["command"], "build-task-spec");
    assert_eq!(payload["status"], "ok");
    assert!(payload["payload"].is_object(), "{}", out.stdout_text());
    assert_eq!(payload["payload"]["execution_mode"], "live");
    assert_eq!(payload["payload"]["arguments"]["sprint"], 2);
}

#[test]
fn output_json_contract_error_envelope_contains_code_and_message() {
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "build-task-spec",
        "--plan",
        "crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
        "--sprint",
        "2",
        "--pr-grouping",
        "group",
    ]);

    assert_eq!(out.code, 1);

    let payload: Value = serde_json::from_str(&out.stdout_text()).expect("stdout should be JSON");
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error"]["code"], "invalid-pr-grouping");
    assert!(
        payload["error"]["message"]
            .as_str()
            .is_some_and(|value| value.contains("with --strategy deterministic")),
        "{}",
        out.stdout_text()
    );
}

#[test]
fn output_text_contract_success_output_is_deterministic() {
    let out = common::run_plan_issue(&[
        "build-plan-task-spec",
        "--plan",
        "crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
        "--pr-grouping",
        "per-sprint",
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert!(
        out.stderr_text().trim().is_empty(),
        "stderr should be empty: {}",
        out.stderr_text()
    );

    let stdout = out.stdout_text();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 4, "unexpected output: {stdout}");
    assert_eq!(
        lines[0],
        "schema_version: plan-issue.build.plan.task.spec.v1"
    );
    assert_eq!(lines[1], "command: build-plan-task-spec");
    assert_eq!(lines[2], "status: ok");
    assert!(lines[3].starts_with("payload: {"), "{}", lines[3]);
}

#[test]
fn output_text_contract_error_output_is_deterministic() {
    let out = common::run_plan_issue(&[
        "build-plan-task-spec",
        "--plan",
        "crates/plan-issue/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
        "--pr-grouping",
        "group",
    ]);

    assert_eq!(out.code, 1);
    assert!(
        out.stdout_text().trim().is_empty(),
        "stdout should be empty: {}",
        out.stdout_text()
    );

    let stderr = out.stderr_text();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 5, "unexpected stderr: {stderr}");
    assert_eq!(
        lines[0],
        "schema_version: plan-issue.build.plan.task.spec.v1"
    );
    assert_eq!(lines[1], "command: build-plan-task-spec");
    assert_eq!(lines[2], "status: error");
    assert_eq!(lines[3], "code: invalid-pr-grouping");
    assert_eq!(
        lines[4],
        "message: --pr-grouping group with --strategy deterministic requires --pr-group mappings"
    );
}

#[test]
fn output_contract_redacts_global_repository_credentials() {
    let tmp = TempDir::new().expect("tmp");
    let out_path = tmp.path().join("run-state.json");
    let repo = "https://global-user:global-secret@gitlab.example.test:8443/acme/widgets.git";

    for format in ["json", "text"] {
        let out = common::run_plan_issue_with_options(
            &[
                "--repo",
                repo,
                "--format",
                format,
                "--dry-run",
                "tracking",
                "run",
                "init",
                "--provider-repo",
                "acme/widgets",
                "--issue",
                "1271",
                "--run-id",
                "global-repo-redaction",
                "--now",
                "2026-07-18T00:00:00Z",
                "--out",
                out_path.to_str().expect("run-state path"),
            ],
            common::plan_issue_cmd_options().with_cwd(tmp.path()),
        );
        assert_eq!(out.code, 0, "format={format} stderr={}", out.stderr_text());
        let stdout = out.stdout_text();
        assert!(!stdout.contains("global-user"), "format={format}: {stdout}");
        assert!(
            !stdout.contains("global-secret"),
            "format={format}: {stdout}"
        );
    }
}

#[test]
fn tracking_checkpoint_output_redacts_repository_credentials() {
    let tmp = TempDir::new().expect("tmp");
    let run_state = tmp.path().join("run-state.json");
    fs::write(
        &run_state,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "checkpoint-repo-redaction",
            "repo": "acme/widgets",
            "issue": 1271,
            "profile": "tracking",
            "phase": "implementing",
            "created_at": "2026-07-18T00:00:00Z",
            "updated_at": "2026-07-18T00:00:00Z"
        })
        .to_string(),
    )
    .expect("run state");
    let fixture = tmp.path().join("fixture");
    fs::create_dir(&fixture).expect("fixture");
    fs::write(fixture.join("body.md"), "## Current Dashboard\n").expect("body");
    fs::write(fixture.join("comments.json"), "{\"comments\":[]}").expect("comments");
    let repo =
        "https://checkpoint-user:checkpoint-secret@gitlab.example.test:8443/acme/widgets.git";

    for format in ["json", "text"] {
        let out = common::run_plan_issue_with_options(
            &[
                "--format",
                format,
                "tracking",
                "checkpoint",
                "--provider-repo",
                repo,
                "--run-state",
                run_state.to_str().expect("run-state path"),
                "--fixture",
                fixture.to_str().expect("fixture path"),
            ],
            common::plan_issue_cmd_options().with_cwd(tmp.path()),
        );
        assert_eq!(out.code, 0, "format={format} stderr={}", out.stderr_text());
        let stdout = out.stdout_text();
        assert!(
            !stdout.contains("checkpoint-user"),
            "format={format}: {stdout}"
        );
        assert!(
            !stdout.contains("checkpoint-secret"),
            "format={format}: {stdout}"
        );
    }
}

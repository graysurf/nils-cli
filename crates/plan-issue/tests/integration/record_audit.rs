//! `plan-issue record audit --expect-visible` integration coverage (Task 2.3).
//!
//! Source: `docs/source/plan-issue-redesign/plan-tracking-issue-cli-redesign-v1.md`
//! Workstream 2 ("Visible Completeness Lint").

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

use plan_issue::lifecycle_record::PAYLOAD_SCHEMA_V2;

use crate::common;

fn v2_comment(role: &str, profile: &str, data: Value, visible: &str) -> String {
    let envelope = json!({
        "schema": PAYLOAD_SCHEMA_V2,
        "role": role,
        "profile": profile,
        "data": data,
    });
    let payload = serde_json::to_string(&envelope).expect("serialize payload");
    format!(
        "<!-- plan-issue-record:v2 role={role} profile={profile} -->\n\n{visible}\n\n```plan-issue-record-payload\n{payload}\n```\n",
    )
}

fn write_fixture(
    role_bodies: &[(&str, Value, &str)],
) -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = TempDir::new().expect("tmp");
    let body = tmp.path().join("body.md");
    fs::write(&body, "## Current Dashboard\n").expect("body");

    let comments: Vec<Value> = role_bodies
        .iter()
        .enumerate()
        .map(|(idx, (role, data, visible))| {
            json!({
                "url": format!("https://example.com/c{idx}"),
                "created_at": format!("2026-05-26T08:00:0{idx}Z"),
                "body": v2_comment(role, "tracking", data.clone(), visible),
            })
        })
        .collect();
    let comments_path = tmp.path().join("comments.json");
    fs::write(&comments_path, json!({"comments": comments}).to_string()).expect("comments");
    (tmp, body, comments_path)
}

#[test]
fn record_audit_default_does_not_emit_visible_block() {
    let (_tmp, body, comments) = write_fixture(&[(
        "state",
        json!({"status": "in-progress", "target_scope": "x", "tasks": [], "prs": []}),
        "## Execution State\n\n- Profile: tracking\n- Status: in-progress\n\n## Task Ledger\n\n<details></details>",
    )]);
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "record",
        "audit",
        "--profile",
        "tracking",
        "--body-file",
        body.to_str().expect("body"),
        "--comments-json",
        comments.to_str().expect("comments"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let envelope = out.stdout_json();
    assert!(
        envelope["payload"]["result"]["visible"].is_null(),
        "default audit must not emit visible block: {envelope}"
    );
}

#[test]
fn record_audit_expect_visible_passes_for_complete_evidence() {
    let (_tmp, body, comments) = write_fixture(&[
        (
            "source",
            json!({"path": "docs/plans/foo/foo.md", "commit": "abc"}),
            "## Source Snapshot\n\n- Profile: tracking\n- Path: `docs/plans/foo/foo.md`\n- Commit: `abc`",
        ),
        (
            "plan",
            json!({"path": "docs/plans/foo/foo-plan.md", "commit": "abc"}),
            "## Plan Snapshot\n\n- Profile: tracking\n- Path: `docs/plans/foo/foo-plan.md`\n- Commit: `abc`",
        ),
        (
            "state",
            json!({"status": "in-progress", "target_scope": "x", "tasks": [], "prs": []}),
            "## Execution State\n\n- Profile: tracking\n- Status: in-progress\n\n## Task Ledger\n\n<details>\n\n| ID | Status |\n| --- | --- |\n| 1.1 | done |\n\n</details>",
        ),
    ]);

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "record",
        "audit",
        "--profile",
        "tracking",
        "--body-file",
        body.to_str().expect("body"),
        "--comments-json",
        comments.to_str().expect("comments"),
        "--expect-visible",
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let envelope = out.stdout_json();
    let visible = &envelope["payload"]["result"]["visible"];
    assert_eq!(visible["expect_visible"], true, "{envelope}");
    assert_eq!(visible["overall_pass"], true, "{envelope}");
    assert!(
        visible["codes"].as_array().unwrap().is_empty(),
        "visible codes should be empty: {envelope}"
    );
    let roles = visible["roles"].as_array().expect("roles array");
    let role_names: Vec<&str> = roles.iter().map(|r| r["role"].as_str().unwrap()).collect();
    assert_eq!(
        role_names,
        vec![
            "source",
            "plan",
            "state",
            "session",
            "validation",
            "review",
            "closeout"
        ]
    );
}

#[test]
fn record_audit_expect_visible_blocks_missing_task_ledger() {
    let (_tmp, body, comments) = write_fixture(&[(
        "state",
        json!({"status": "in-progress", "target_scope": "x", "tasks": [], "prs": []}),
        "## Execution State\n\n- Profile: tracking\n- Status: in-progress\n",
    )]);

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "record",
        "audit",
        "--profile",
        "tracking",
        "--body-file",
        body.to_str().expect("body"),
        "--comments-json",
        comments.to_str().expect("comments"),
        "--expect-visible",
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let envelope = out.stdout_json();
    let visible = &envelope["payload"]["result"]["visible"];
    assert_eq!(visible["overall_pass"], false, "{envelope}");
    let codes: Vec<&str> = visible["codes"]
        .as_array()
        .expect("codes array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        codes.contains(&"state-missing-task-ledger"),
        "codes={codes:?}"
    );
}

#[test]
fn record_audit_expect_visible_blocks_profile_only_session() {
    let (_tmp, body, comments) = write_fixture(&[(
        "session",
        json!({"summary": "internal note"}),
        "## Execution Session\n\n- Profile: tracking\n",
    )]);

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "record",
        "audit",
        "--body-file",
        body.to_str().expect("body"),
        "--comments-json",
        comments.to_str().expect("comments"),
        "--expect-visible",
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let envelope = out.stdout_json();
    let visible = &envelope["payload"]["result"]["visible"];
    let codes: Vec<&str> = visible["codes"]
        .as_array()
        .expect("codes")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        codes
            .iter()
            .any(|c| *c == "session-missing-summary" || *c == "profile-only"),
        "expected session-missing-summary or profile-only; got {codes:?}"
    );
}

#[test]
fn record_audit_expect_visible_help_mentions_flag() {
    let out = common::run_plan_issue(&["record", "audit", "--help"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert!(
        out.stdout_text().contains("--expect-visible"),
        "help missing --expect-visible flag: {}",
        out.stdout_text()
    );
}

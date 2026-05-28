//! `plan-issue tracking checkpoint --dry-run` integration coverage (Task 5.2).

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

use plan_issue_cli::lifecycle_record::PAYLOAD_SCHEMA_V2;

use crate::common;

fn json_stdout(out: &common::CmdOut) -> Value {
    serde_json::from_str(&out.stdout).expect("json stdout")
}

fn v2_comment(role: &str, profile: &str, data: Value, visible: &str) -> String {
    let envelope = json!({
        "schema": PAYLOAD_SCHEMA_V2,
        "role": role,
        "profile": profile,
        "data": data,
    });
    let payload = serde_json::to_string(&envelope).expect("serialize");
    format!(
        "<!-- plan-issue-record:v2 role={role} profile={profile} -->\n\n{visible}\n\n```plan-issue-record-payload\n{payload}\n```\n",
    )
}

fn write_fixture(roles: &[(&str, Value, &str, &str)]) -> TempDir {
    let tmp = TempDir::new().expect("tmp");
    fs::write(tmp.path().join("body.md"), "## Current Dashboard\n").expect("body");
    let comments: Vec<Value> = roles
        .iter()
        .enumerate()
        .map(|(idx, (role, data, visible, at))| {
            json!({
                "url": format!("https://example.com/c{idx}"),
                "created_at": at,
                "body": v2_comment(role, "tracking", data.clone(), visible),
            })
        })
        .collect();
    fs::write(
        tmp.path().join("comments.json"),
        json!({"comments": comments}).to_string(),
    )
    .expect("comments");
    tmp
}

fn write_run_state(path: &std::path::Path, phase: &str, notes: &[&str]) {
    let body = json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "run-1",
        "repo": "owner/repo",
        "issue": 123,
        "profile": "tracking",
        "phase": phase,
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z",
        "selected_scope": {
            "task": "1.2",
            "title": "demo"
        },
        "branch": "feat/x",
        "notes": notes,
    });
    fs::write(path, body.to_string()).expect("run-state");
}

fn write_run_state_with_exec_file(
    path: &std::path::Path,
    phase: &str,
    notes: &[&str],
    exec_file: &std::path::Path,
) {
    let body = json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "run-1",
        "repo": "owner/repo",
        "issue": 123,
        "profile": "tracking",
        "phase": phase,
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z",
        "execution_state_file": exec_file.to_string_lossy(),
        "selected_scope": {
            "task": "1.2",
            "title": "demo"
        },
        "branch": "feat/x",
        "notes": notes,
    });
    fs::write(path, body.to_string()).expect("run-state");
}

#[test]
fn tracking_checkpoint_dry_run_renders_state_role_from_run_state() {
    let fixture = write_fixture(&[
        (
            "source",
            json!({"path": "p", "commit": "c"}),
            "## Source Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:00Z",
        ),
        (
            "plan",
            json!({"path": "p", "commit": "c"}),
            "## Plan Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:01Z",
        ),
        (
            "state",
            json!({"status": "in-progress", "target_scope": "x", "tasks": [], "prs": []}),
            "## Execution State\n\n- Profile: tracking\n- Status: in-progress\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | done |",
            "2026-05-26T00:00:02Z",
        ),
    ]);
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "implementing", &["latest progress"]);

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--profile",
        "tracking",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let result = json_stdout(&out)["payload"]["result"].clone();
    assert_eq!(result["mode"], "dry-run");
    let planned: Vec<&str> = result["roles_planned"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        planned.contains(&"state"),
        "planned={planned:?} full={result}"
    );
    let rendered = &result["rendered"];
    assert!(rendered.is_array() && !rendered.as_array().unwrap().is_empty());
    let body = rendered[0]["body"].as_str().expect("body").to_string();
    assert!(body.starts_with("<!-- plan-issue-record:v2 role=state"));
    assert!(body.contains("## Task Ledger"));
}

#[test]
fn tracking_checkpoint_state_body_renders_full_execution_state_ledger() {
    // Regression guard for the render-layer half of the
    // plan-task-ledger-durability rollout: when the run state declares
    // an `execution_state_file`, the state lifecycle body must carry
    // the *full* per-task ledger from that file, not just the
    // synthesized `selected_task` row.
    let fixture = write_fixture(&[
        (
            "source",
            json!({"path": "p", "commit": "c"}),
            "## Source Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:00Z",
        ),
        (
            "plan",
            json!({"path": "p", "commit": "c"}),
            "## Plan Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:01Z",
        ),
        (
            "state",
            json!({"status": "in-progress", "target_scope": "x", "tasks": [], "prs": []}),
            "## Execution State\n\n- Profile: tracking\n- Status: in-progress\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | done |",
            "2026-05-26T00:00:02Z",
        ),
    ]);
    let tmp = TempDir::new().expect("tmp");
    let exec_path = tmp.path().join("plan-execution-state.md");
    fs::write(
        &exec_path,
        "\
# Demo Plan Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: in-progress
- Target scope: demo
- Current task: Task 1.2

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | First task | https://example/c/aaa | initial |
| 1.2 | in-progress | Second task | — | continuing |
| 1.3 | pending | Third task | — | not started |
",
    )
    .expect("write exec state");

    let rs_path = tmp.path().join("run-state.json");
    write_run_state_with_exec_file(&rs_path, "implementing", &["progress"], &exec_path);

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--profile",
        "tracking",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let result = json_stdout(&out)["payload"]["result"].clone();
    let body = result["rendered"][0]["body"]
        .as_str()
        .expect("body")
        .to_string();
    assert!(
        body.contains("## Task Ledger"),
        "body missing ledger: {body}"
    );
    assert!(
        body.contains("| 1.1 | done |"),
        "body missing row 1.1 done: {body}"
    );
    assert!(
        body.contains("| 1.2 | in-progress |"),
        "body missing row 1.2 in-progress: {body}"
    );
    assert!(
        body.contains("| 1.3 | pending |"),
        "body missing row 1.3 pending: {body}"
    );
    // The fallback synthesizer emits a single "selected" placeholder row;
    // assert we did NOT regress to it.
    assert!(
        !body.contains("| Task 1.2 | in-progress | selected |"),
        "regressed to single-row synthesized fallback: {body}"
    );
}

#[test]
fn tracking_checkpoint_dry_run_skips_empty_session_and_validation_roles() {
    let fixture = write_fixture(&[
        (
            "source",
            json!({"path": "p", "commit": "c"}),
            "## Source Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:00Z",
        ),
        (
            "plan",
            json!({"path": "p", "commit": "c"}),
            "## Plan Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:01Z",
        ),
    ]);
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "implementing", &[]); // no notes, no validation

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "session,validation",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let result = json_stdout(&out)["payload"]["result"].clone();
    let planned: Vec<&str> = result["roles_planned"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(planned.is_empty(), "planned should be empty: {planned:?}");
    let skipped: Vec<&str> = result["roles_skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["role"].as_str().unwrap())
        .collect();
    assert!(skipped.contains(&"session"));
    assert!(skipped.contains(&"validation"));
}

#[test]
fn tracking_checkpoint_blocks_review_role_without_decision() {
    // Finding #20 from the plan-tracking testbed: `--post state,review` with
    // no review decision in run state used to post only `state` and report
    // `blocked: []`, silently dropping `review`. A review checkpoint with no
    // decision carries no delivery evidence, so a requested-but-empty review
    // must surface as a `review-missing-decision` blocker. (session/validation
    // keep the skip-empty behavior covered by the test above.)
    let fixture = write_fixture(&[
        (
            "source",
            json!({"path": "p", "commit": "c"}),
            "## Source Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:00Z",
        ),
        (
            "plan",
            json!({"path": "p", "commit": "c"}),
            "## Plan Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:01Z",
        ),
    ]);
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "reviewing", &[]); // no review decision recorded

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "review",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let result = json_stdout(&out)["payload"]["result"].clone();
    let blocked_codes: Vec<&str> = result["blocked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap_or(""))
        .collect();
    assert!(
        blocked_codes.contains(&"review-missing-decision"),
        "blocked should carry review-missing-decision, got: {blocked_codes:?}"
    );
    let planned: Vec<&str> = result["roles_planned"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        planned.is_empty(),
        "review must not be planned without a decision: {planned:?}"
    );
}

#[test]
fn tracking_checkpoint_dry_run_writes_rendered_bodies_under_run_dir() {
    let fixture = write_fixture(&[
        (
            "source",
            json!({"path": "p", "commit": "c"}),
            "## Source Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:00Z",
        ),
        (
            "plan",
            json!({"path": "p", "commit": "c"}),
            "## Plan Snapshot\n\n- Profile: tracking\n- Path: `p`",
            "2026-05-26T00:00:01Z",
        ),
        (
            "state",
            json!({"status": "in-progress", "target_scope": "x", "tasks": [], "prs": []}),
            "## Execution State\n\n- Profile: tracking\n- Status: in-progress\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | done |",
            "2026-05-26T00:00:02Z",
        ),
    ]);
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state(&rs_path, "implementing", &["latest"]);
    let rendered_dir = tmp.path().join("rendered-out");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--rendered-out",
        rendered_dir.to_str().expect("rendered out"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let result = json_stdout(&out)["payload"]["result"].clone();
    assert_eq!(result["mode"], "dry-run");
    let written = rendered_dir.join("state-comment.md");
    assert!(
        written.exists(),
        "rendered file not written: {}",
        written.display()
    );
}

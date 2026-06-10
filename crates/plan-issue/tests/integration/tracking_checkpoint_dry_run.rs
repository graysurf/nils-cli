//! `plan-issue tracking checkpoint --dry-run` integration coverage (Task 5.2).

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
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
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
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
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
fn tracking_checkpoint_state_body_renders_live_header_not_frozen_preflight() {
    // graysurf/plan-tracking-testbed#54 / sympoies/nils-cli#700 (Part B): the
    // visible Execution State header must be re-rendered from the derived
    // payload, not spliced verbatim from the execution-state.md. Otherwise a
    // completed plan keeps its pre-flight header ("ready-to-start", "tbd",
    // "snapshot: pending"). The `## Task Ledger` and any other authored
    // sections (e.g. `## Validation Plan`) still come from the file.
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

- Status: ready-to-start; tracking issue not yet opened.
- Target scope: two append-only commits to notes.md
- Current task: none (tracking issue not yet opened).
- Next task: Task 1.1
- Tracking issue: tbd
- Source snapshot: pending

## Validation Plan

- Per-task: git diff shows one added line.

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | done | Append line A | log | first |
| 1.2 | done | Append line B | log | second |
",
    )
    .expect("write exec state");

    let rs_path = tmp.path().join("run-state.json");
    write_run_state_with_exec_file(&rs_path, "ready_for_close", &["done"], &exec_path);

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
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let body = out.stdout_json()["payload"]["result"]["rendered"][0]["body"]
        .as_str()
        .expect("body")
        .to_string();

    // Header re-rendered from the derived payload, reflecting completion.
    assert!(
        body.contains("- Status: complete"),
        "want live `Status: complete`; body:\n{body}"
    );
    assert!(
        body.contains("- Current task: complete"),
        "want `Current task: complete`; body:\n{body}"
    );
    assert!(
        body.contains("- Next task: closeout"),
        "want `Next task: closeout`; body:\n{body}"
    );
    // Frozen pre-flight header bullets must be gone.
    assert!(
        !body.contains("ready-to-start"),
        "frozen `Status: ready-to-start` leaked; body:\n{body}"
    );
    assert!(
        !body.contains("Tracking issue: tbd"),
        "frozen `Tracking issue: tbd` leaked; body:\n{body}"
    );
    assert!(
        !body.contains("snapshot: pending"),
        "frozen `snapshot: pending` leaked; body:\n{body}"
    );
    // The ledger and other authored sections still come from the file.
    assert!(
        body.contains("## Task Ledger"),
        "ledger missing; body:\n{body}"
    );
    assert!(
        body.contains("| 1.1 | done |"),
        "ledger row missing; body:\n{body}"
    );
    assert!(
        body.contains("## Validation Plan"),
        "authored `## Validation Plan` section dropped; body:\n{body}"
    );
}

/// Finding #45: a `session` checkpoint with run-state activity but no explicit
/// note now renders from that activity (selected task + branch) instead of
/// being silently skipped. `validation` with no validation summary is still
/// legitimately skipped.
#[test]
fn tracking_checkpoint_dry_run_session_renders_from_activity_validation_still_skips() {
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
    // `write_run_state` sets selected_scope (task 1.2) + branch feat/x; no notes.
    write_run_state(&rs_path, "implementing", &[]);

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
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    let planned: Vec<&str> = result["roles_planned"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let skipped: Vec<&str> = result["roles_skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["role"].as_str().unwrap())
        .collect();
    assert!(
        planned.contains(&"session"),
        "session must render from activity: planned={planned:?}"
    );
    assert!(
        !skipped.contains(&"session"),
        "session must not be skipped when activity exists: skipped={skipped:?}"
    );
    assert!(
        skipped.contains(&"validation"),
        "validation with no summary still skips: skipped={skipped:?}"
    );

    // The synthesized session body must carry a non-empty Summary that names
    // the run-state activity.
    let rendered = result["rendered"].as_array().expect("rendered array");
    let session_body = rendered
        .iter()
        .find(|e| e["role"] == "session")
        .expect("session rendered")["body"]
        .as_str()
        .expect("session body");
    assert!(
        session_body.contains("Task 1.2"),
        "session summary should name the task: {session_body}"
    );
    assert!(
        session_body.contains("feat/x"),
        "session summary should name the branch: {session_body}"
    );
}

/// A genuinely empty run-state — no scope, branch, PR, or validation and still
/// at the initial phase — keeps the skip-empty behavior for `session`.
#[test]
fn tracking_checkpoint_dry_run_session_skipped_for_bare_run_state() {
    let fixture = write_fixture(&[(
        "source",
        json!({"path": "p", "commit": "c"}),
        "## Source Snapshot\n\n- Profile: tracking\n- Path: `p`",
        "2026-05-26T00:00:00Z",
    )]);
    let tmp = TempDir::new().expect("tmp");
    let rs_path = tmp.path().join("run-state.json");
    let minimal = json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "run-1",
        "repo": "owner/repo",
        "issue": 123,
        "profile": "tracking",
        "phase": "initial",
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z"
    });
    fs::write(&rs_path, minimal.to_string()).expect("rs");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--post",
        "session",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    let skipped: Vec<&str> = result["roles_skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["role"].as_str().unwrap())
        .collect();
    assert!(
        skipped.contains(&"session"),
        "bare run-state should skip session: {skipped:?}"
    );
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
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
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
fn tracking_checkpoint_dry_run_renders_rich_review_evidence() {
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
    let rendered_dir = tmp.path().join("rendered-out");
    let body = json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "run-1",
        "repo": "owner/repo",
        "issue": 123,
        "profile": "tracking",
        "phase": "ready_for_close",
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z",
        "review": {
            "decision": "approve",
            "lenses": ["testing", "maintainability"],
            "evidence": "https://example.test/review",
            "findings": [
                {
                    "id": "F1",
                    "severity": "minor",
                    "disposition": "fixed",
                    "summary": "Review context renders visibly"
                }
            ]
        }
    });
    fs::write(&rs_path, body.to_string()).expect("run-state");

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
        "--expect-visible",
        "--rendered-out",
        rendered_dir.to_str().expect("rendered out"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let body = fs::read_to_string(rendered_dir.join("review-comment.md")).expect("rendered review");
    assert!(body.contains("- Decision: approve"));
    assert!(body.contains("- Lenses: testing, maintainability"));
    assert!(body.contains("- Outcome comment: https://example.test/review"));
    assert!(body.contains("| F1 | minor | fixed | Review context renders visibly |"));
}

#[test]
fn tracking_checkpoint_dry_run_ignores_prior_review_disposition_hint() {
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
    let rendered_dir = tmp.path().join("rendered-out");
    let body = json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "run-1",
        "repo": "owner/repo",
        "issue": 123,
        "profile": "tracking",
        "phase": "ready_for_close",
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z",
        "review": {
            "decision": "approve",
            "lenses": ["testing"],
            "findings_disposition": ["prior finding fixed"]
        }
    });
    fs::write(&rs_path, body.to_string()).expect("run-state");

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
        "--expect-visible",
        "--rendered-out",
        rendered_dir.to_str().expect("rendered out"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let body = fs::read_to_string(rendered_dir.join("review-comment.md")).expect("rendered review");
    assert!(body.contains("- Decision: approve"));
    assert!(body.contains("- Lenses: testing"));
    assert!(!body.contains("| prior finding fixed |"));
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
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    assert_eq!(result["mode"], "dry-run");
    let written = rendered_dir.join("state-comment.md");
    assert!(
        written.exists(),
        "rendered file not written: {}",
        written.display()
    );
}

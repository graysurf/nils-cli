//! `plan-issue tracking checkpoint --dry-run` integration coverage (Task 5.2).

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

use plan_issue::lifecycle_record::{PAYLOAD_SCHEMA_V2, extract_payload};

use crate::common;

fn repo_root() -> &'static std::path::Path {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
}

fn repo_tempdir(prefix: &str) -> TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(repo_root())
        .expect("repository fixture")
}

fn current_repo_remote() -> String {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("read origin remote");
    assert!(output.status.success(), "git remote failed");
    String::from_utf8(output.stdout)
        .expect("utf-8 remote")
        .trim()
        .to_string()
}

fn current_repo_slug() -> String {
    nils_common::git::parse_git_remote_url(&current_repo_remote())
        .expect("parse current repository")
        .path
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
        "repo": current_repo_remote(),
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

fn assert_state_role_failed_closed(result: &Value, blocker_code: &str) {
    assert!(
        result["blocked"]
            .as_array()
            .expect("blocked")
            .iter()
            .any(|entry| entry["code"] == blocker_code),
        "missing {blocker_code}: {result}"
    );
    assert!(
        !result["roles_planned"]
            .as_array()
            .expect("roles planned")
            .iter()
            .any(|role| role == "state"),
        "blocked state must not be planned: {result}"
    );
    assert!(
        !result["rendered"]
            .as_array()
            .expect("rendered")
            .iter()
            .any(|entry| entry["role"] == "state"),
        "blocked state must not be rendered: {result}"
    );
    assert!(
        result["roles_skipped"]
            .as_array()
            .expect("roles skipped")
            .iter()
            .any(|entry| {
                entry["role"] == "state"
                    && entry["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.contains(blocker_code))
            }),
        "blocked state must report its skip reason: {result}"
    );
    if let Some(rendered_out) = result["rendered_out"].as_str() {
        assert!(
            !std::path::Path::new(rendered_out)
                .join("state-comment.md")
                .exists(),
            "blocked state must not write a rendered artifact"
        );
    }
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
    let tmp = repo_tempdir(".checkpoint-exact-");
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
    let tmp = repo_tempdir(".checkpoint-exact-");
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

#[test]
fn tracking_checkpoint_ready_for_close_with_pending_rows_rejects_terminal_projection() {
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
    let tmp = repo_tempdir(".checkpoint-ready-pending-");
    let exec_path = tmp.path().join("pending-execution-state.md");
    fs::write(
        &exec_path,
        "\
# Execution State: pending defense

## Execution State

- Target scope: pending defense

## Task Ledger

| ID | Title | Status | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | Completed task | done | test | complete |
| 1.2 | Active task | in-progress | — | active |
| 1.3 | Pending task | pending | — | queued |
",
    )
    .expect("write execution state");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state_with_exec_file(
        &rs_path,
        "ready_for_close",
        &["premature phase"],
        &exec_path,
    );

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--profile",
        "tracking",
        "--run-state",
        rs_path.to_str().expect("run state"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = &out.stdout_json()["payload"]["result"];
    let planned = result["roles_planned"].as_array().expect("planned roles");
    let blocked_codes = result["blocked"]
        .as_array()
        .expect("blocked")
        .iter()
        .filter_map(|entry| entry["code"].as_str())
        .collect::<Vec<_>>();

    assert!(
        planned.is_empty(),
        "pending rows must not produce a terminal state checkpoint: {result}"
    );
    assert!(
        blocked_codes.contains(&"visible-completeness-failed"),
        "pending rows at ready_for_close must fail closed: {result}"
    );
    assert!(
        !result["visible_failures"]
            .as_array()
            .expect("visible failures")
            .is_empty(),
        "the rejected projection must report visible-lint evidence: {result}"
    );
}

#[test]
fn tracking_checkpoint_closed_title_ledger_renders_canonical_terminal_state() {
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
    let tmp = repo_tempdir(".checkpoint-closed-");
    let exec_path = tmp.path().join("terminal-execution-state.md");
    fs::write(
        &exec_path,
        "\
# Execution State: terminal dashboard repair

## Execution State

- Status: in-progress
- Current task: 2.3
- Next task: closeout

## Task Ledger

| ID | Title | Status | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 2.2 | Preserve non-terminal derivation | done | test | complete |
| 2.3 | Repair terminal dashboard | done | test | complete |
",
    )
    .expect("write execution state");
    let rs_path = tmp.path().join("run-state.json");
    write_run_state_with_exec_file(&rs_path, "closed", &["closed"], &exec_path);

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--profile",
        "tracking",
        "--run-state",
        rs_path.to_str().expect("run state"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let envelope = out.stdout_json();
    let body = envelope["payload"]["result"]["rendered"][0]["body"]
        .as_str()
        .expect("state body");
    let payload = extract_payload(body).expect("state payload");
    let state = payload.parse_state().expect("state data");

    assert_eq!(
        state.target_scope.as_deref(),
        Some("terminal dashboard repair")
    );
    assert_eq!(state.current.as_deref(), Some("complete"));
    assert_eq!(state.next_action.as_deref(), Some("none"));
    assert_eq!(state.tasks.len(), 2);
    assert_eq!(
        state.tasks[1].title.as_deref(),
        Some("Repair terminal dashboard")
    );
    assert!(body.contains("- Current task: complete"), "{body}");
    assert!(body.contains("- Next task: none"), "{body}");
}

#[test]
fn tracking_checkpoint_relocates_execution_state_after_worktree_cleanup() {
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
    let repo_root = repo_root();
    let checkout_dir = tempfile::Builder::new()
        .prefix(".issue-1271-relocation-")
        .tempdir_in(repo_root)
        .expect("checkout fixture");
    let bundle = checkout_dir.path().join("bundle");
    fs::create_dir(&bundle).expect("bundle");
    let exec_path = bundle.join("portable-execution-state.md");
    fs::write(
        &exec_path,
        "\
# Portable terminal repair

## Execution State

- Target scope: portable terminal repair

## Task Ledger

| ID | Title | Status | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 2.3 | Repair terminal dashboard | done | test | complete |
",
    )
    .expect("execution state");
    let bundle_relative = bundle.strip_prefix(repo_root).expect("relative bundle");
    let exec_relative = exec_path
        .strip_prefix(repo_root)
        .expect("relative execution state");
    let removed_worktree = std::path::Path::new("/removed/original-worktree");
    let rs_path = checkout_dir.path().join("run-state.json");
    fs::write(
        &rs_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-1",
            "repo": current_repo_remote(),
            "issue": 0,
            "profile": "tracking",
            "phase": "closed",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "bundle": removed_worktree.join(bundle_relative).to_string_lossy(),
            "execution_state_file": removed_worktree.join(exec_relative).to_string_lossy(),
            "bundle_repo_relative": bundle_relative,
            "execution_state_repo_relative": exec_relative,
            "worktree": removed_worktree,
            "selected_scope": {"task": "2.3", "title": "stale selected task"}
        })
        .to_string(),
    )
    .expect("run state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--profile",
        "tracking",
        "--run-state",
        rs_path.to_str().expect("run state"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let envelope = out.stdout_json();
    let result = &envelope["payload"]["result"];
    let blocked_codes = result["blocked"]
        .as_array()
        .expect("blocked")
        .iter()
        .filter_map(|entry| entry["code"].as_str())
        .collect::<Vec<_>>();
    assert!(
        !blocked_codes.contains(&"state-ledger-unresolved"),
        "relocation must resolve the retained checkout: {result}"
    );
    let body = result["rendered"][0]["body"].as_str().expect("state body");
    let state = extract_payload(body)
        .expect("state payload")
        .parse_state()
        .expect("state data");
    assert_eq!(state.current.as_deref(), Some("complete"));
    assert_eq!(state.next_action.as_deref(), Some("none"));
    assert_eq!(state.tasks.len(), 1);
}

#[test]
fn tracking_checkpoint_rejects_ambiguous_relocated_execution_states() {
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
    let repo_root = repo_root();
    let checkout_dir = tempfile::Builder::new()
        .prefix(".issue-1271-ambiguous-")
        .tempdir_in(repo_root)
        .expect("checkout fixture");
    let bundle = checkout_dir.path().join("bundle");
    fs::create_dir(&bundle).expect("bundle");
    let ledger = "## Task Ledger\n\n| ID | Title | Status | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | task | done | test |\n";
    fs::write(bundle.join("a-execution-state.md"), ledger).expect("first state");
    fs::write(bundle.join("b-execution-state.md"), ledger).expect("second state");
    let bundle_relative = bundle.strip_prefix(repo_root).expect("relative bundle");
    let removed_worktree = std::path::Path::new("/removed/original-worktree");
    let rs_path = checkout_dir.path().join("run-state.json");
    fs::write(
        &rs_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-1",
            "repo": current_repo_remote(),
            "issue": 0,
            "profile": "tracking",
            "phase": "closed",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "bundle": removed_worktree.join(bundle_relative).to_string_lossy(),
            "bundle_repo_relative": bundle_relative,
            "worktree": removed_worktree
        })
        .to_string(),
    )
    .expect("run state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--profile",
        "tracking",
        "--run-state",
        rs_path.to_str().expect("run state"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let envelope = out.stdout_json();
    let result = &envelope["payload"]["result"];
    let blocked_codes = result["blocked"]
        .as_array()
        .expect("blocked")
        .iter()
        .filter_map(|entry| entry["code"].as_str())
        .collect::<Vec<_>>();

    assert!(
        blocked_codes.contains(&"state-ledger-ambiguous"),
        "ambiguous relocation must fail deterministically: {result}"
    );
    assert_state_role_failed_closed(result, "state-ledger-ambiguous");
}

#[cfg(unix)]
#[test]
fn tracking_checkpoint_rejects_matching_symlink_alongside_real_execution_state() {
    use std::os::unix::fs::symlink;

    let fixture = write_fixture(&[]);
    let run_dir = repo_tempdir(".checkpoint-symlinked-state-");
    let bundle = run_dir.path().join("bundle");
    fs::create_dir(&bundle).expect("bundle");
    let real = bundle.join("real-execution-state.md");
    fs::write(
        &real,
        "## Task Ledger\n\n| ID | Title | Status | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | task | done | test |\n",
    )
    .expect("real execution state");
    symlink(&real, bundle.join("linked-execution-state.md")).expect("execution-state symlink");
    let rs_path = run_dir.path().join("run-state.json");
    fs::write(
        &rs_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-symlinked-state",
            "repo": current_repo_remote(),
            "issue": 0,
            "profile": "tracking",
            "phase": "closed",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "bundle": bundle
        })
        .to_string(),
    )
    .expect("run state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("run state"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = &out.stdout_json()["payload"]["result"];
    assert_state_role_failed_closed(result, "state-ledger-unresolved");
}

#[test]
fn tracking_checkpoint_missing_exact_identity_does_not_fall_back_to_bundle() {
    let fixture = write_fixture(&[]);
    let run_dir = repo_tempdir(".checkpoint-missing-exact-");
    let bundle = run_dir.path().join("bundle");
    fs::create_dir(&bundle).expect("bundle");
    fs::write(
        bundle.join("fallback-execution-state.md"),
        "## Execution State\n\n- Target scope: unrelated bundle fallback\n\n## Task Ledger\n\n| ID | Title | Status | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | fallback | done | test |\n",
    )
    .expect("bundle fallback");
    let missing = run_dir.path().join("missing-execution-state.md");
    let rs_path = run_dir.path().join("run-state.json");
    fs::write(
        &rs_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-missing-exact",
            "repo": current_repo_remote(),
            "issue": 0,
            "profile": "tracking",
            "phase": "closed",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "bundle": bundle,
            "execution_state_file": missing
        })
        .to_string(),
    )
    .expect("run state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("run state"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = &out.stdout_json()["payload"]["result"];
    assert_state_role_failed_closed(result, "state-ledger-unresolved");
}

#[test]
fn tracking_checkpoint_malformed_recorded_ledger_does_not_render_state() {
    let fixture = write_fixture(&[]);
    let run_dir = repo_tempdir(".checkpoint-malformed-");
    let exec_path = run_dir.path().join("malformed-execution-state.md");
    fs::write(
        &exec_path,
        "## Execution State\n\n- Target scope: malformed\n\n## Task Ledger\n\n| ID | Title | Status | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | malformed | complete | test |\n",
    )
    .expect("execution state");
    let rs_path = run_dir.path().join("run-state.json");
    fs::write(
        &rs_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-malformed",
            "repo": current_repo_remote(),
            "issue": 0,
            "profile": "tracking",
            "phase": "closed",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "execution_state_file": exec_path
        })
        .to_string(),
    )
    .expect("run state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("run state"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = &out.stdout_json()["payload"]["result"];
    assert_state_role_failed_closed(result, "state-ledger-malformed");
}

#[test]
fn tracking_checkpoint_resolves_historical_bare_repo_exact_path_outside_current_checkout() {
    let fixture = write_fixture(&[]);
    let run_dir = repo_tempdir(".checkpoint-exact-outside-cwd-");
    let exec_path = run_dir.path().join("exact-execution-state.md");
    fs::write(
        &exec_path,
        "## Execution State\n\n- Target scope: exact outside cwd\n\n## Task Ledger\n\n| ID | Title | Status | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | exact | done | test |\n",
    )
    .expect("execution state");
    let rs_path = run_dir.path().join("run-state.json");
    fs::write(
        &rs_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-exact-outside-cwd",
            "repo": current_repo_slug(),
            "issue": 0,
            "profile": "tracking",
            "phase": "closed",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "execution_state_file": exec_path
        })
        .to_string(),
    )
    .expect("run state");
    let outside_cwd = TempDir::new().expect("outside cwd");

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--fixture",
            fixture.path().to_str().expect("fixture"),
        ],
        common::plan_issue_cmd_options().with_cwd(outside_cwd.path()),
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = &out.stdout_json()["payload"]["result"];
    assert!(
        !result["blocked"]
            .as_array()
            .expect("blocked")
            .iter()
            .any(|entry| entry["code"] == "state-ledger-unresolved"),
        "exact path should resolve from its own repository: {result}"
    );
    assert!(
        result["rendered"][0]["body"]
            .as_str()
            .expect("state body")
            .contains("exact outside cwd")
    );
}

#[test]
fn tracking_checkpoint_existing_exact_copy_precedes_other_checkout_relative_copy() {
    let fixture = write_fixture(&[]);
    let parent = TempDir::new().expect("checkout parent");
    let original = parent.path().join("original");
    let other = parent.path().join("other");
    let remote = current_repo_remote();
    for root in [&original, &other] {
        fs::create_dir(root).expect("checkout root");
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .expect("git init");
        assert!(init.success());
        let add_remote = std::process::Command::new("git")
            .args(["remote", "add", "origin", &remote])
            .current_dir(root)
            .status()
            .expect("git remote add");
        assert!(add_remote.success());
        fs::create_dir(root.join("bundle")).expect("bundle");
    }
    let relative = std::path::Path::new("bundle/selected-execution-state.md");
    let exact = original.join(relative);
    fs::write(
        &exact,
        "## Execution State\n\n- Target scope: original exact copy\n\n## Task Ledger\n\n| ID | Title | Status | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | original | done | test |\n",
    )
    .expect("original state");
    fs::write(
        other.join(relative),
        "## Execution State\n\n- Target scope: other checkout copy\n\n## Task Ledger\n\n| ID | Title | Status | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | other | done | test |\n",
    )
    .expect("other state");
    let rs_path = parent.path().join("run-state.json");
    fs::write(
        &rs_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-exact-precedence",
            "repo": remote,
            "issue": 0,
            "profile": "tracking",
            "phase": "closed",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "execution_state_file": exact,
            "execution_state_repo_relative": relative
        })
        .to_string(),
    )
    .expect("run state");

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "checkpoint",
            "--run-state",
            rs_path.to_str().expect("run state"),
            "--post",
            "state",
            "--fixture",
            fixture.path().to_str().expect("fixture"),
        ],
        common::plan_issue_cmd_options().with_cwd(&other),
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = &out.stdout_json()["payload"]["result"];
    let body = result["rendered"][0]["body"].as_str().expect("state body");
    assert!(body.contains("original exact copy"), "{body}");
    assert!(!body.contains("other checkout copy"), "{body}");
}

#[test]
fn tracking_checkpoint_rejects_exact_execution_state_outside_repository() {
    let fixture = write_fixture(&[]);
    let outside = TempDir::new().expect("outside fixture");
    let exec_path = outside.path().join("outside-execution-state.md");
    fs::write(
        &exec_path,
        "## Execution State\n\n- Target scope: outside\n\n## Task Ledger\n\n| ID | Title | Status | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | outside | done | test |\n",
    )
    .expect("outside execution state");
    let run_dir = repo_tempdir(".checkpoint-outside-");
    let rs_path = run_dir.path().join("run-state.json");
    fs::write(
        &rs_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-outside",
            "repo": current_repo_remote(),
            "issue": 0,
            "profile": "tracking",
            "phase": "closed",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "execution_state_file": exec_path
        })
        .to_string(),
    )
    .expect("run state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("run state"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(
        out.code,
        0,
        "stdout: {} stderr: {}",
        out.stdout_text(),
        out.stderr_text()
    );
    let result = &out.stdout_json()["payload"]["result"];
    let codes = result["blocked"]
        .as_array()
        .expect("blocked")
        .iter()
        .filter_map(|entry| entry["code"].as_str())
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&"state-ledger-unresolved"),
        "outside path must fail closed: {result}"
    );
}

#[cfg(unix)]
#[test]
fn tracking_checkpoint_rejects_symlinked_bundle_escape() {
    use std::os::unix::fs::symlink;

    let fixture = write_fixture(&[]);
    let outside = TempDir::new().expect("outside fixture");
    fs::write(
        outside.path().join("outside-execution-state.md"),
        "## Execution State\n\n- Target scope: outside\n\n## Task Ledger\n\n| ID | Title | Status | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | outside | done | test |\n",
    )
    .expect("outside execution state");
    let run_dir = repo_tempdir(".checkpoint-symlink-");
    let bundle_link = run_dir.path().join("bundle");
    symlink(outside.path(), &bundle_link).expect("bundle symlink");
    let rs_path = run_dir.path().join("run-state.json");
    fs::write(
        &rs_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-symlink",
            "repo": current_repo_remote(),
            "issue": 0,
            "profile": "tracking",
            "phase": "closed",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "bundle": bundle_link
        })
        .to_string(),
    )
    .expect("run state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("run state"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = &out.stdout_json()["payload"]["result"];
    let codes = result["blocked"]
        .as_array()
        .expect("blocked")
        .iter()
        .filter_map(|entry| entry["code"].as_str())
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&"state-ledger-unresolved"),
        "symlink escape must fail closed: {result}"
    );
}

#[test]
fn tracking_checkpoint_relocated_explicit_file_precedes_exact_bundle() {
    let fixture = write_fixture(&[]);
    let run_dir = repo_tempdir(".checkpoint-precedence-");
    let bundle = run_dir.path().join("bundle");
    fs::create_dir(&bundle).expect("bundle");
    fs::write(
        bundle.join("bundle-execution-state.md"),
        "# Execution State: bundle fallback\n\n## Execution State\n\n- Target scope: bundle fallback\n\n## Task Ledger\n\n| ID | Title | Status | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | bundle | done | test |\n",
    )
    .expect("bundle execution state");
    let explicit = run_dir.path().join("explicit-execution-state.md");
    fs::write(
        &explicit,
        "# Execution State: explicit identity\n\n## Execution State\n\n- Target scope: explicit identity\n\n## Task Ledger\n\n| ID | Title | Status | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | explicit | done | test |\n",
    )
    .expect("explicit execution state");
    let explicit_relative = explicit
        .strip_prefix(repo_root())
        .expect("relative explicit");
    let removed_worktree = std::path::Path::new("/removed/original-worktree");
    let rs_path = run_dir.path().join("run-state.json");
    fs::write(
        &rs_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-precedence",
            "repo": current_repo_remote(),
            "issue": 0,
            "profile": "tracking",
            "phase": "closed",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "bundle": bundle,
            "execution_state_file": removed_worktree.join(explicit_relative),
            "execution_state_repo_relative": explicit_relative,
            "worktree": removed_worktree
        })
        .to_string(),
    )
    .expect("run state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("run state"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = &out.stdout_json()["payload"]["result"];
    let body = result["rendered"][0]["body"].as_str().expect("state body");
    assert!(
        body.contains("explicit identity"),
        "explicit identity must win: {body}"
    );
    assert!(
        !body.contains("bundle fallback"),
        "bundle incorrectly won: {body}"
    );
}

#[test]
fn tracking_checkpoint_v1_relocates_from_worktree_prefix() {
    let fixture = write_fixture(&[]);
    let run_dir = repo_tempdir(".checkpoint-v1-");
    let explicit = run_dir.path().join("v1-execution-state.md");
    fs::write(
        &explicit,
        "# Execution State: portable v1 identity\n\n## Execution State\n\n- Target scope: portable v1 identity\n\n## Task Ledger\n\n| ID | Title | Status | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | v1 | done | test |\n",
    )
    .expect("v1 execution state");
    let relative = explicit
        .strip_prefix(repo_root())
        .expect("relative explicit");
    let removed_worktree = std::path::Path::new("/removed/original-worktree");
    let rs_path = run_dir.path().join("run-state.json");
    fs::write(
        &rs_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-v1",
            "repo": current_repo_remote(),
            "issue": 0,
            "profile": "tracking",
            "phase": "closed",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "execution_state_file": removed_worktree.join(relative),
            "worktree": removed_worktree
        })
        .to_string(),
    )
    .expect("run state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("run state"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = &out.stdout_json()["payload"]["result"];
    let codes = result["blocked"]
        .as_array()
        .expect("blocked")
        .iter()
        .filter_map(|entry| entry["code"].as_str())
        .collect::<Vec<_>>();
    assert!(
        !codes.contains(&"state-ledger-unresolved"),
        "v1 relocation failed: {result}"
    );
    let body = result["rendered"][0]["body"].as_str().expect("state body");
    assert!(body.contains("portable v1 identity"), "{body}");
}

#[test]
fn tracking_checkpoint_rejects_recorded_repository_host_mismatch() {
    let fixture = write_fixture(&[]);
    let run_dir = repo_tempdir(".checkpoint-host-mismatch-");
    let exec_path = run_dir.path().join("host-execution-state.md");
    fs::write(
        &exec_path,
        "## Execution State\n\n- Target scope: host\n\n## Task Ledger\n\n| ID | Title | Status | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | host | done | test |\n",
    )
    .expect("execution state");
    let rs_path = run_dir.path().join("run-state.json");
    fs::write(
        &rs_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-host-mismatch",
            "repo": current_repo_remote(),
            "repo_host": "evil.example.test",
            "issue": 0,
            "profile": "tracking",
            "phase": "closed",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "execution_state_file": exec_path
        })
        .to_string(),
    )
    .expect("run state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "checkpoint",
        "--run-state",
        rs_path.to_str().expect("run state"),
        "--post",
        "state",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = &out.stdout_json()["payload"]["result"];
    let codes = result["blocked"]
        .as_array()
        .expect("blocked")
        .iter()
        .filter_map(|entry| entry["code"].as_str())
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&"state-ledger-unresolved"),
        "host mismatch must fail closed: {result}"
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

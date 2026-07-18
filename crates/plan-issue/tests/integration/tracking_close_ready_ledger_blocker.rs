//! `tracking close-ready` `ledger-rows-pending` blocker (Task 1.3).

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

use plan_issue::lifecycle_record::PAYLOAD_SCHEMA_V2;

use crate::common;

fn repo_tempdir(prefix: &str) -> TempDir {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(repo_root)
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

fn write_complete_fixture(tmp: &TempDir) -> std::path::PathBuf {
    let fixture_dir = tmp.path().join("fixture");
    fs::create_dir_all(&fixture_dir).expect("fixture dir");
    fs::write(fixture_dir.join("body.md"), "## Final Dashboard\n").expect("body");
    let roles = [
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
            json!({
                "status": "complete",
                "target_scope": "x",
                "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
                "prs": [{"ref": "owner/repo#1", "url": "https://example.com/pr/1", "status": "merged"}]
            }),
            "## Execution State\n\n- Profile: tracking\n- Status: complete\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | done |",
            "2026-05-26T00:00:02Z",
        ),
        (
            "session",
            json!({"summary": "done"}),
            "## Execution Session\n\n- Profile: tracking\n- Summary: done",
            "2026-05-26T00:00:03Z",
        ),
        (
            "validation",
            json!({"overall": "pass", "commands": [{"command": "cargo test", "status": "pass"}], "waivers": []}),
            "## Validation Evidence\n\n- Profile: tracking\n- Overall: pass\n\n| Command | Status | Evidence |\n|---|---|---|\n| cargo test | pass | log |",
            "2026-05-26T00:00:04Z",
        ),
        (
            "review",
            json!({"decision": "approve", "findings": [], "lenses": ["testing"]}),
            "## Review Evidence\n\n- Profile: tracking\n- Decision: approve\n- Lenses: testing",
            "2026-05-26T00:00:05Z",
        ),
    ];
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
        fixture_dir.join("comments.json"),
        json!({"comments": comments}).to_string(),
    )
    .expect("comments");
    fixture_dir
}

fn write_run_state(
    tmp: &TempDir,
    phase: &str,
    bundle: Option<&std::path::Path>,
) -> std::path::PathBuf {
    let rs_path = tmp.path().join("run-state.json");
    let mut body = json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "run-1",
        "repo": current_repo_remote(),
        "issue": 123,
        "profile": "tracking",
        "phase": phase,
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T01:00:00Z",
    });
    if let Some(path) = bundle {
        body.as_object_mut()
            .unwrap()
            .insert("bundle".into(), json!(path.to_string_lossy()));
    }
    fs::write(&rs_path, body.to_string()).expect("run-state");
    rs_path
}

fn write_ledger_bundle(tmp: &TempDir, ledger_body: &str) -> std::path::PathBuf {
    let bundle = tmp.path().join("docs/plans/demo");
    fs::create_dir_all(&bundle).expect("bundle dir");
    fs::write(bundle.join("demo-execution-state.md"), ledger_body).expect("ledger");
    bundle
}

#[test]
fn close_ready_emits_ledger_rows_pending_when_phase_ready_and_rows_pending() {
    let tmp = repo_tempdir(".close-ready-ledger-");
    let fixture = write_complete_fixture(&tmp);
    let bundle = write_ledger_bundle(
        &tmp,
        "# Demo\n\n## Task Ledger\n\n| ID | Status | Task | Evidence | Notes |\n| --- | --- | --- | --- | --- |\n| 1.1 | pending | A |  |  |\n| 1.2 | done | B | PR#1 |  |\n",
    );
    let rs_path = write_run_state(&tmp, "ready_for_close", Some(&bundle));

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.to_str().expect("fixture"),
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--approval",
        "approver",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    let blockers = result["blockers"].as_array().expect("blockers");
    let ledger_blockers: Vec<&Value> = blockers
        .iter()
        .filter(|b| b["code"] == "ledger-rows-pending")
        .collect();
    assert_eq!(
        ledger_blockers.len(),
        1,
        "only 1.1 should block; got: {ledger_blockers:?}"
    );
    assert_eq!(ledger_blockers[0]["task_id"], "1.1");
    assert_eq!(ledger_blockers[0]["status"], "pending");
    assert!(
        ledger_blockers[0]["suggested_unblock"]
            .as_str()
            .unwrap()
            .contains("plan-tooling ledger-update")
    );
    assert_eq!(result["ready"], false);
}

#[test]
fn close_ready_no_ledger_blocker_when_all_rows_done() {
    let tmp = repo_tempdir(".close-ready-ledger-");
    let fixture = write_complete_fixture(&tmp);
    let bundle = write_ledger_bundle(
        &tmp,
        "# Demo\n\n## Task Ledger\n\n| ID | Status | Task | Evidence | Notes |\n| --- | --- | --- | --- | --- |\n| 1.1 | done | A | PR#1 |  |\n| 1.2 | waived | B |  |  |\n",
    );
    let rs_path = write_run_state(&tmp, "ready_for_close", Some(&bundle));

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.to_str().expect("fixture"),
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--approval",
        "approver",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    let codes: Vec<&str> = result["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(
        !codes.contains(&"ledger-rows-pending"),
        "no ledger-rows-pending when all done; codes: {codes:?}"
    );
}

#[test]
fn close_ready_preserves_non_default_issue_url_authority_port() {
    let tmp = repo_tempdir(".close-ready-port-");
    let fixture = write_complete_fixture(&tmp);
    let bundle = write_ledger_bundle(
        &tmp,
        "# Demo\n\n## Execution State\n\n- Tracking issue: <https://internal.ghe.com:8443/acme/widgets/issues/123>\n\n## Task Ledger\n\n| ID | Status | Task | Evidence | Notes |\n| --- | --- | --- | --- | --- |\n| 1.1 | done | A | PR#1 |  |\n",
    );
    let rs_path = tmp.path().join("run-state.json");
    fs::write(
        &rs_path,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-port",
            "repo": "https://internal.ghe.com:8443/acme/widgets",
            "repo_provider": "github",
            "repo_host": "internal.ghe.com:8443",
            "issue": 123,
            "profile": "tracking",
            "phase": "ready_for_close",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "bundle": bundle.to_string_lossy(),
        })
        .to_string(),
    )
    .expect("run-state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.to_str().expect("fixture"),
        "--run-state",
        rs_path.to_str().expect("run state"),
        "--approval",
        "approver",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = &out.stdout_json()["payload"]["result"];
    let codes = result["blockers"]
        .as_array()
        .expect("blockers")
        .iter()
        .filter_map(|blocker| blocker["code"].as_str())
        .collect::<Vec<_>>();
    assert!(
        !codes.contains(&"execution-state-issue-mismatch"),
        "matching non-default authority port must remain consistent: {result}"
    );
}

#[test]
fn close_ready_blocks_every_nonterminal_ledger_status() {
    for status in ["pending", "in-progress", "blocked"] {
        let tmp = repo_tempdir(".close-ready-nonterminal-");
        let fixture = write_complete_fixture(&tmp);
        let bundle = write_ledger_bundle(
            &tmp,
            &format!(
                "# Demo\n\n## Task Ledger\n\n| ID | Status | Task | Evidence | Notes |\n| --- | --- | --- | --- | --- |\n| 1.1 | {status} | A |  |  |\n"
            ),
        );
        let rs_path = write_run_state(&tmp, "ready_for_close", Some(&bundle));

        let out = common::run_plan_issue(&[
            "--format",
            "json",
            "tracking",
            "close-ready",
            "--fixture",
            fixture.to_str().expect("fixture"),
            "--run-state",
            rs_path.to_str().expect("rs"),
            "--approval",
            "approver",
        ]);
        assert_eq!(out.code, 0, "{status}: {}", out.stderr_text());
        let result = out.stdout_json()["payload"]["result"].clone();
        let blocker = result["blockers"]
            .as_array()
            .expect("blockers")
            .iter()
            .find(|blocker| blocker["code"] == "ledger-rows-pending")
            .unwrap_or_else(|| panic!("{status} must block close-ready: {result}"));
        assert_eq!(blocker["status"], status);
        assert_eq!(result["ready"], false);
    }
}

#[test]
fn close_ready_reports_semantically_malformed_recorded_ledgers() {
    let cases = [
        (
            "invalid-status",
            "# Demo\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | complete | A | PR#1 |\n",
        ),
        (
            "missing-leading-pipe",
            "# Demo\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | done | A | PR#1 |\n1.2 | pending | Hidden task |  |\n",
        ),
    ];
    for (case, ledger) in cases {
        let tmp = repo_tempdir(".close-ready-malformed-");
        let fixture = write_complete_fixture(&tmp);
        let bundle = write_ledger_bundle(&tmp, ledger);
        let rs_path = write_run_state(&tmp, "ready_for_close", Some(&bundle));

        let out = common::run_plan_issue(&[
            "--format",
            "json",
            "tracking",
            "close-ready",
            "--fixture",
            fixture.to_str().expect("fixture"),
            "--run-state",
            rs_path.to_str().expect("rs"),
            "--approval",
            "approver",
        ]);
        assert_eq!(out.code, 0, "{case}: stderr: {}", out.stderr_text());
        let result = out.stdout_json()["payload"]["result"].clone();
        let codes = result["blockers"]
            .as_array()
            .expect("blockers")
            .iter()
            .filter_map(|blocker| blocker["code"].as_str())
            .collect::<Vec<_>>();
        assert!(
            codes.contains(&"state-ledger-malformed"),
            "{case} must fail closed: {result}"
        );
        assert_eq!(result["ready"], false, "{case}");
    }
}

#[test]
fn close_ready_reports_ambiguous_execution_state_bundle() {
    let tmp = repo_tempdir(".close-ready-ambiguous-");
    let fixture = write_complete_fixture(&tmp);
    let bundle = write_ledger_bundle(
        &tmp,
        "# Demo\n\n## Task Ledger\n\n| ID | Status | Task | Evidence | Notes |\n| --- | --- | --- | --- | --- |\n| 1.1 | done | A | PR#1 |  |\n",
    );
    fs::write(
        bundle.join("other-execution-state.md"),
        "# Other\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 2.1 | done | B | PR#2 |\n",
    )
    .expect("second execution state");
    let rs_path = write_run_state(&tmp, "ready_for_close", Some(&bundle));

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.to_str().expect("fixture"),
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--approval",
        "approver",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    let codes = result["blockers"]
        .as_array()
        .expect("blockers")
        .iter()
        .filter_map(|blocker| blocker["code"].as_str())
        .collect::<Vec<_>>();
    assert!(
        codes.contains(&"state-ledger-ambiguous"),
        "ambiguous bundle must block close-ready: {result}"
    );
    assert_eq!(result["ready"], false);
}

#[test]
fn close_ready_silent_skips_when_bundle_absent() {
    let tmp = repo_tempdir(".close-ready-ledger-");
    let fixture = write_complete_fixture(&tmp);
    let rs_path = write_run_state(&tmp, "ready_for_close", None);

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.to_str().expect("fixture"),
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--approval",
        "approver",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    let codes: Vec<&str> = result["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(
        !codes.contains(&"ledger-rows-pending"),
        "silent-skip when bundle absent; codes: {codes:?}"
    );
}

#[test]
fn close_ready_no_ledger_blocker_when_phase_implementing() {
    let tmp = repo_tempdir(".close-ready-ledger-");
    let fixture = write_complete_fixture(&tmp);
    let bundle = write_ledger_bundle(
        &tmp,
        "# Demo\n\n## Task Ledger\n\n| ID | Status | Task | Evidence | Notes |\n| --- | --- | --- | --- | --- |\n| 1.1 | pending | A |  |  |\n",
    );
    let rs_path = write_run_state(&tmp, "implementing", Some(&bundle));

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.to_str().expect("fixture"),
        "--run-state",
        rs_path.to_str().expect("rs"),
        "--approval",
        "approver",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    let codes: Vec<&str> = result["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(
        !codes.contains(&"ledger-rows-pending"),
        "no ledger gate when phase=implementing; codes: {codes:?}"
    );
}

//! `plan-issue tracking close-ready` integration coverage (Task 6.2).

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
    fs::write(tmp.path().join("body.md"), "## Final Dashboard\n").expect("body");
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

fn complete_fixture() -> TempDir {
    write_fixture(&[
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
            json!({"summary": "completed"}),
            "## Execution Session\n\n- Profile: tracking\n- Summary: completed",
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
    ])
}

#[test]
fn tracking_close_ready_reports_ready_for_complete_fixture() {
    let fixture = complete_fixture();
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--approval",
        "https://example.com/approval",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    assert_eq!(result["fsm_state"], "RECORD_READY_FOR_CLOSE");
    let blockers: Vec<&str> = result["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert_eq!(
        result["ready"], true,
        "expected ready=true; blockers={blockers:?} result={result}"
    );
}

#[test]
fn tracking_close_ready_blocks_when_missing_validation() {
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
            json!({"status": "complete", "target_scope": "x", "tasks": [], "prs": []}),
            "## Execution State\n\n- Profile: tracking\n- Status: complete\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | done |",
            "2026-05-26T00:00:02Z",
        ),
        (
            "session",
            json!({"summary": "done"}),
            "## Execution Session\n\n- Profile: tracking\n- Summary: done",
            "2026-05-26T00:00:03Z",
        ),
    ]);
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--approval",
        "approver",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    assert_eq!(result["ready"], false);
    let codes: Vec<&str> = result["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"validation-missing"));
}

#[test]
fn tracking_close_ready_collects_linked_prs_from_state_and_flag() {
    let fixture = complete_fixture();
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--approval",
        "approver",
        "--linked-pr",
        "owner/repo#999",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    let linked: Vec<&str> = result["linked_prs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(linked.contains(&"owner/repo#1"));
    assert!(linked.contains(&"owner/repo#999"));
}

#[test]
fn tracking_close_ready_rejects_unsafe_linked_prs_without_leaking_the_envelope() {
    let fixture = complete_fixture();
    for (linked_pr, secret) in [
        (
            "https://alice:secret-userinfo@github.com/owner/repo/pull/42",
            "secret-userinfo",
        ),
        (
            "https://github.com/owner/repo/pull/42?token=secret-query",
            "secret-query",
        ),
        (
            "prefix Authorization: Bearer secret~token+/== suffix",
            "secret~token+/==",
        ),
    ] {
        let out = common::run_plan_issue(&[
            "--format",
            "json",
            "tracking",
            "close-ready",
            "--fixture",
            fixture.path().to_str().expect("fixture"),
            "--approval",
            "approver",
            "--linked-pr",
            linked_pr,
        ]);

        assert_eq!(out.code, 64, "stdout={}", out.stdout_text());
        let envelope = out.stdout_text();
        assert!(!envelope.contains(secret), "secret leaked: {envelope}");
        assert_eq!(
            out.stdout_json()["error"]["code"],
            "record-invalid-linked-pr"
        );
    }
}

#[test]
fn tracking_close_ready_rejects_unsafe_linked_pr_from_run_state_without_leaking() {
    let fixture = complete_fixture();
    let run_state_path = fixture.path().join("run-state.json");
    let init = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--run-id",
        "unsafe-historical-linked-pr",
        "--now",
        "2026-05-26T00:00:00Z",
        "--out",
        run_state_path.to_str().expect("run-state path"),
    ]);
    assert_eq!(init.code, 0, "stderr={}", init.stderr_text());
    let mut run_state: Value =
        serde_json::from_str(&fs::read_to_string(&run_state_path).expect("run-state body"))
            .expect("run-state json");
    run_state["pr"] = json!({
        "ref": "https://alice:historical-secret@github.com/owner/repo/pull/42"
    });
    fs::write(
        &run_state_path,
        serde_json::to_vec_pretty(&run_state).expect("render run state"),
    )
    .expect("write historical state");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--approval",
        "approver",
        "--run-state",
        run_state_path.to_str().expect("run-state path"),
    ]);

    assert_eq!(out.code, 64, "stdout={}", out.stdout_text());
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "record-invalid-linked-pr"
    );
    assert!(!out.stdout_text().contains("historical-secret"));
}

#[test]
fn tracking_close_ready_preserves_safe_token_prefix_repository_names() {
    let fixture = complete_fixture();
    for linked_pr in ["acme/ghp_docs#42", "acme/glpat-examples#43"] {
        let out = common::run_plan_issue(&[
            "--format",
            "json",
            "tracking",
            "close-ready",
            "--fixture",
            fixture.path().to_str().expect("fixture"),
            "--approval",
            "approver",
            "--linked-pr",
            linked_pr,
        ]);

        assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
        let envelope = out.stdout_json();
        assert_eq!(
            envelope["payload"]["arguments"]["command"]["CloseReady"]["linked_pr"][0],
            linked_pr
        );
        assert!(
            envelope["payload"]["result"]["linked_prs"]
                .as_array()
                .expect("linked prs")
                .iter()
                .any(|value| value == linked_pr)
        );
    }
}

#[test]
fn tracking_close_ready_is_non_mutating() {
    // The command must not post comments or repair dashboards; the JSON
    // envelope therefore never names a `posted` field.
    let fixture = complete_fixture();
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
        "--approval",
        "approver",
    ]);
    assert_eq!(out.code, 0);
    let result = out.stdout_json()["payload"]["result"].clone();
    assert!(result.get("posted").is_none());
    assert!(result.get("dashboard_repaired").is_none());
}

#[test]
fn tracking_close_ready_help_lists_required_args() {
    let out = common::run_plan_issue(&["tracking", "close-ready", "--help"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert!(out.stdout_text().contains("--linked-pr"));
    assert!(out.stdout_text().contains("--approval"));
    assert!(out.stdout_text().contains("--expect-visible"));
}

/// Build a closeout-complete tracking fixture (source, plan, complete state,
/// session, validation) plus a caller-supplied `review` payload — or no
/// `review` role when `review` is `None`. Writes both `body.md` (read by
/// `tracking close-ready`) and `issue-body.md` (read by `record close`) plus a
/// shared `comments.json`, so one directory drives both commands over
/// identical evidence (plan-tracking-testbed#79).
fn closeout_fixture_with_state_and_validation(
    review: Option<(Value, &str)>,
    state_task_status: &str,
    validation_overall: &str,
) -> TempDir {
    let tmp = TempDir::new().expect("tmp");
    let body = "## Final Dashboard\n";
    fs::write(tmp.path().join("body.md"), body).expect("body");
    fs::write(tmp.path().join("issue-body.md"), body).expect("issue-body");

    let validation_visible = match validation_overall {
        "pass" => {
            "## Validation Evidence\n\n- Profile: tracking\n- Overall: pass\n\n| Command | Status | Evidence |\n|---|---|---|\n| cargo test | pass | log |"
        }
        "partial" => {
            "## Validation Evidence\n\n- Profile: tracking\n- Overall: partial\n\n| Command | Status | Evidence |\n|---|---|---|\n| cargo test | pass | log |"
        }
        other => panic!("unsupported validation status: {other}"),
    };
    let state_visible = match state_task_status {
        "done" => {
            "## Execution State\n\n- Profile: tracking\n- Status: complete\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | done |"
        }
        "pending" => {
            "## Execution State\n\n- Profile: tracking\n- Status: complete\n\n## Task Ledger\n\n| ID | Status |\n| --- | --- |\n| 1.1 | pending |"
        }
        other => panic!("unsupported state task status: {other}"),
    };
    let mut roles: Vec<(&str, Value, &str, &str)> = vec![
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
                "tasks": [{"id": "1.1", "status": state_task_status, "title": "x"}],
                "prs": [{"ref": "owner/repo#1", "url": "https://example.com/pr/1", "status": "merged"}]
            }),
            state_visible,
            "2026-05-26T00:00:02Z",
        ),
        (
            "session",
            json!({"summary": "completed"}),
            "## Execution Session\n\n- Profile: tracking\n- Summary: completed",
            "2026-05-26T00:00:03Z",
        ),
        (
            "validation",
            json!({"overall": validation_overall, "commands": [{"command": "cargo test", "status": "pass"}], "waivers": []}),
            validation_visible,
            "2026-05-26T00:00:04Z",
        ),
    ];
    if let Some((data, visible)) = review {
        roles.push(("review", data, visible, "2026-05-26T00:00:05Z"));
    }

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

fn closeout_fixture_with_validation(
    review: Option<(Value, &str)>,
    validation_overall: &str,
) -> TempDir {
    closeout_fixture_with_state_and_validation(review, "done", validation_overall)
}

fn closeout_fixture(review: Option<(Value, &str)>) -> TempDir {
    closeout_fixture_with_validation(review, "pass")
}

/// An approved review body carrying a single residual finding at `severity`.
fn approved_review_with_residual(severity: &str) -> (Value, &'static str) {
    let data = json!({
        "decision": "approve",
        "findings": [
            {"id": "F1", "severity": severity, "disposition": "residual", "summary": "residual finding"}
        ],
        "lenses": ["testing"]
    });
    (
        data,
        "## Review Evidence\n\n- Profile: tracking\n- Decision: approve\n- Lenses: testing\n\n| ID | Severity | Disposition | Summary |\n|---|---|---|---|\n| F1 | s | residual | residual finding |",
    )
}

fn blocker_codes(result: &Value) -> Vec<String> {
    result["blockers"]
        .as_array()
        .expect("blockers array")
        .iter()
        .map(|b| b["code"].as_str().expect("code").to_string())
        .collect()
}

/// plan-tracking-testbed#79: the non-mutating `tracking close-ready` probe and
/// the mutating `record close` gate must reach the SAME verdict on an approved
/// review that still carries a residual blocker/major finding. Before the fix,
/// close-ready reported `ready: true` / `RECORD_READY_FOR_CLOSE` while
/// `record close` rejected the identical evidence with
/// `review-unresolved-findings`, stranding the closeout skill mid-flight. Both
/// commands must now block with the same stable code.
/// Run `tracking close-ready` over `fixture` and return the `result` payload.
fn run_close_ready(fixture: &TempDir) -> Value {
    run_close_ready_with_approval(fixture, Some("https://example.com/approval"))
}

fn run_close_ready_with_approval(fixture: &TempDir, approval: Option<&str>) -> Value {
    let mut args = vec![
        "--format",
        "json",
        "tracking",
        "close-ready",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ];
    if let Some(approval) = approval {
        args.extend(["--approval", approval]);
    }
    let out = common::run_plan_issue(&args);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    out.stdout_json()["payload"]["result"].clone()
}

/// Run `record close` over `fixture` and return the parsed JSON envelope plus
/// exit code.
fn run_record_close(fixture: &TempDir) -> (i32, Value) {
    run_record_close_with_approval(fixture, Some("https://example.com/approval"))
}

fn run_record_close_with_approval(fixture: &TempDir, approval: Option<&str>) -> (i32, Value) {
    let mut args = vec![
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "79",
        "--profile",
        "tracking",
        "--fixture",
        fixture.path().to_str().expect("fixture"),
    ];
    if let Some(approval) = approval {
        args.extend(["--approval", approval]);
    }
    let out = common::run_plan_issue(&args);
    (out.code, out.stdout_json())
}

/// plan-tracking-testbed#79: the non-mutating `tracking close-ready` probe and
/// the mutating `record close` gate must reach the SAME verdict on an approved
/// review that still carries a residual blocker/major finding. Before the fix,
/// close-ready reported `ready: true` / `RECORD_READY_FOR_CLOSE` while
/// `record close` rejected the identical evidence with
/// `review-unresolved-findings`, stranding the closeout skill mid-flight. Both
/// commands must now block with the same stable code.
#[test]
fn close_ready_and_record_close_agree_on_residual_major_review() {
    let fixture = closeout_fixture(Some(approved_review_with_residual("major")));

    // Non-mutating probe: must NOT authorize the close.
    let result = run_close_ready(&fixture);
    let codes = blocker_codes(&result);
    assert_eq!(
        result["ready"], false,
        "close-ready must not authorize close; result={result}"
    );
    assert!(
        codes.iter().any(|c| c == "review-unresolved-findings"),
        "close-ready blockers must include review-unresolved-findings: {codes:?}"
    );

    // Mutating gate: must fail with the same stable code.
    let (code, err) = run_record_close(&fixture);
    assert_eq!(
        code, 1,
        "record close must reject residual major findings; envelope: {err}"
    );
    assert_eq!(err["status"], "error");
    assert_eq!(err["error"]["code"], "record-close-gate-failed");
    let message = err["error"]["message"].as_str().expect("error message");
    assert!(
        message.contains("review-unresolved-findings"),
        "record close must cite the same review-unresolved-findings code: {message}"
    );
}

/// The severity predicate must discriminate in both directions: a residual
/// `minor` finding is not a close blocker, so close-ready must stay ready and
/// emit no `review-*` blocker. Guards against the predicate regressing to
/// over-broad (blocking any residual severity).
#[test]
fn close_ready_allows_approved_review_with_residual_minor() {
    let fixture = closeout_fixture(Some(approved_review_with_residual("minor")));
    let result = run_close_ready(&fixture);
    let codes = blocker_codes(&result);
    assert!(
        !codes.iter().any(|c| c.starts_with("review-")),
        "residual minor must not raise a review blocker: {codes:?}"
    );
    assert_eq!(
        result["ready"], true,
        "close-ready must stay ready for a residual minor finding; result={result}"
    );
}

/// The `contains_key("review")` guard exists so the shared review gate does
/// not duplicate the `review-missing` blocker the reconcile step already
/// emits for a fully-absent review role. Assert exactly one `review-missing`.
#[test]
fn close_ready_emits_review_missing_once_when_review_absent() {
    let fixture = closeout_fixture(None);
    let result = run_close_ready(&fixture);
    let codes = blocker_codes(&result);
    assert_eq!(
        result["ready"], false,
        "close-ready must block without review evidence; result={result}"
    );
    assert_eq!(
        codes.iter().filter(|c| *c == "review-missing").count(),
        1,
        "review-missing must appear exactly once (no reconcile/gate double-emit): {codes:?}"
    );
}

/// Parity also holds for a `request-changes` decision: both surfaces block with
/// the shared `review-rejected` code, not just for residual findings.
#[test]
fn close_ready_and_record_close_agree_on_request_changes_review() {
    let fixture = closeout_fixture(Some((
        json!({"decision": "request-changes", "findings": [], "lenses": ["testing"]}),
        "## Review Evidence\n\n- Profile: tracking\n- Decision: request-changes\n- Lenses: testing",
    )));

    let result = run_close_ready(&fixture);
    let codes = blocker_codes(&result);
    assert_eq!(result["ready"], false, "result={result}");
    assert!(
        codes.iter().any(|c| c == "review-rejected"),
        "close-ready must block request-changes with review-rejected: {codes:?}"
    );

    let (code, err) = run_record_close(&fixture);
    assert_eq!(
        code, 1,
        "record close must reject request-changes; envelope: {err}"
    );
    assert_eq!(err["error"]["code"], "record-close-gate-failed");
    let message = err["error"]["message"].as_str().expect("error message");
    assert!(
        message.contains("review-rejected"),
        "record close must cite the same review-rejected code: {message}"
    );
}

/// Provider-latest validation is canonical for both closeout surfaces. A
/// retained `partial` result must block the non-mutating probe and the
/// mutating close with the same stable code.
#[test]
fn close_ready_and_record_close_agree_on_partial_validation() {
    let fixture = closeout_fixture_with_validation(
        Some((
            json!({"decision": "approve", "findings": [], "lenses": ["testing"]}),
            "## Review Evidence\n\n- Profile: tracking\n- Decision: approve\n- Lenses: testing",
        )),
        "partial",
    );

    // Reproduce the real disagreement: local accumulated validation says
    // `pass`, but provider-latest lifecycle evidence still says `partial`.
    // The provider record must win.
    let run_state = fixture.path().join("run-state.json");
    fs::write(
        &run_state,
        json!({
            "schema": "plan-issue.execution-run.v1",
            "run_id": "run-validation-parity",
            "repo": "owner/repo",
            "issue": 79,
            "profile": "tracking",
            "phase": "ready_for_close",
            "created_at": "2026-05-26T00:00:00Z",
            "updated_at": "2026-05-26T01:00:00Z",
            "validation": {"overall": "pass"}
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
        fixture.path().to_str().expect("fixture"),
        "--run-state",
        run_state.to_str().expect("run-state"),
        "--approval",
        "https://example.com/approval",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let result = out.stdout_json()["payload"]["result"].clone();
    let codes = blocker_codes(&result);
    assert_eq!(result["ready"], false, "result={result}");
    assert!(
        codes.iter().any(|c| c == "validation-failed"),
        "close-ready must block partial validation with validation-failed: {codes:?}"
    );

    let (code, err) = run_record_close(&fixture);
    assert_eq!(
        code, 1,
        "record close must reject partial validation: {err}"
    );
    assert_eq!(err["error"]["code"], "record-close-gate-failed");
    let message = err["error"]["message"].as_str().expect("error message");
    assert!(
        message.contains("validation-failed"),
        "record close must cite the same validation-failed code: {message}"
    );
}

/// Once provider validation is repaired to `pass`, the same complete evidence
/// clears both closeout surfaces.
#[test]
fn close_ready_and_record_close_agree_on_passing_validation() {
    let fixture = closeout_fixture(Some((
        json!({"decision": "approve", "findings": [], "lenses": ["testing"]}),
        "## Review Evidence\n\n- Profile: tracking\n- Decision: approve\n- Lenses: testing",
    )));

    let result = run_close_ready(&fixture);
    assert_eq!(result["ready"], true, "result={result}");
    assert!(blocker_codes(&result).is_empty(), "result={result}");

    let (code, envelope) = run_record_close(&fixture);
    assert_eq!(
        code, 0,
        "record close must accept passing validation: {envelope}"
    );
    assert_eq!(envelope["status"], "ok");
}

#[test]
fn close_ready_and_record_close_agree_on_nonterminal_state_tasks() {
    let fixture = closeout_fixture_with_state_and_validation(
        Some((
            json!({"decision": "approve", "findings": [], "lenses": ["testing"]}),
            "## Review Evidence\n\n- Profile: tracking\n- Decision: approve\n- Lenses: testing",
        )),
        "pending",
        "pass",
    );

    let result = run_close_ready(&fixture);
    let codes = blocker_codes(&result);
    assert_eq!(result["ready"], false, "result={result}");
    assert!(
        codes.iter().any(|c| c == "state-tasks-incomplete"),
        "close-ready must reject nonterminal provider tasks: {codes:?}"
    );

    let (code, err) = run_record_close(&fixture);
    assert_eq!(code, 1, "record close must reject nonterminal tasks: {err}");
    let message = err["error"]["message"].as_str().expect("error message");
    assert!(
        message.contains("state-tasks-incomplete"),
        "message={message}"
    );
}

#[test]
fn close_ready_and_record_close_agree_on_missing_approval_with_linked_pr() {
    let fixture = closeout_fixture(Some((
        json!({"decision": "approve", "findings": [], "lenses": ["testing"]}),
        "## Review Evidence\n\n- Profile: tracking\n- Decision: approve\n- Lenses: testing",
    )));

    let result = run_close_ready_with_approval(&fixture, None);
    let codes = blocker_codes(&result);
    assert_eq!(result["ready"], false, "result={result}");
    assert!(
        codes.iter().any(|c| c == "record-close-missing-approval"),
        "close-ready must require approval even with a linked PR: {codes:?}"
    );

    let (code, err) = run_record_close_with_approval(&fixture, None);
    assert_eq!(code, 64, "record close must reject missing approval: {err}");
    assert_eq!(
        err["error"]["code"], "record-close-missing-approval",
        "both public command surfaces must expose the same stable code"
    );
}

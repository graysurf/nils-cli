use std::fs;
use std::path::Path;

use pretty_assertions::{assert_eq, assert_ne};
use serde_json::{Value, json};
use tempfile::TempDir;

use nils_test_support::StubBinDir;
use nils_test_support::cmd::CmdOptions;
use plan_issue::commands::record::RecordProfile;

use crate::common;

const PAYLOAD_SCHEMA_V2: &str = "plan-issue-record.payload.v2";
const PAYLOAD_FENCE_INFO: &str = "plan-issue-record-payload";

/// Build an older `plan-issue-record:v2` comment body with a visible payload
/// fence carrying `data` for the given role/profile.
fn v2_comment_body(role: &str, profile: &str, data: Value) -> String {
    let envelope = json!({
        "schema": PAYLOAD_SCHEMA_V2,
        "role": role,
        "profile": profile,
        "data": data,
    });
    let payload = serde_json::to_string(&envelope).expect("payload json");
    format!(
        "<!-- plan-issue-record:v2 role={role} profile={profile} -->\n\n```{PAYLOAD_FENCE_INFO}\n{payload}\n```\n",
    )
}

fn live_record_gh_stub() -> &'static str {
    r#"#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${PLAN_ISSUE_GH_LOG:-}" ]]; then
  printf '%s\n' "$*" >> "$PLAN_ISSUE_GH_LOG"
fi

cmd="${1:-}"
sub="${2:-}"
case "$cmd $sub" in
  "issue view")
    if [[ -n "${PLAN_ISSUE_GH_VIEW_JSON_FILE:-}" ]]; then
      cat "$PLAN_ISSUE_GH_VIEW_JSON_FILE"
    else
      printf '%s\n' "${PLAN_ISSUE_GH_VIEW_JSON:-{\"body\":\"\",\"comments\":[]}}"
    fi
    ;;
  "issue create" | "issue edit" | "issue comment" | "issue close")
    echo "provider mutation should have been blocked before gh: $*" >&2
    exit 99
    ;;
  *)
    echo "unsupported gh call: $*" >&2
    exit 1
    ;;
esac
"#
}

fn live_record_options(stub_dir: &Path, envs: &[(&str, &str)]) -> CmdOptions {
    common::plan_issue_cmd_options()
        .with_env_remove_prefix("PLAN_ISSUE_GH_")
        .with_path_prepend(stub_dir)
        .with_envs(envs)
}

fn assert_comment_visible_prefix(body: &str, expected: &str) {
    let payload_start = body
        .find("<!-- plan-issue-record-payload:hex:")
        .expect("hidden payload carrier");
    assert_eq!(&body[..payload_start], expected, "{body}");
    assert!(
        body[payload_start..].ends_with(" -->\n"),
        "payload carrier should terminate the comment body:\n{body}"
    );
    assert!(
        !body.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
        "payload must remain hidden:\n{body}"
    );
}

fn assert_provider_payload_privacy_error(
    out: &nils_test_support::cmd::CmdOutput,
    code: &str,
    home_suggestion: &str,
) {
    assert_eq!(
        out.code,
        1,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let parsed = out.stdout_json();
    assert_eq!(parsed["status"], "error");
    assert_eq!(
        parsed["error"]["code"],
        code,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let message = parsed["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("machine-local home path"),
        "message should name local-path class: {message}"
    );
    assert!(
        message.contains(home_suggestion),
        "message should suggest $HOME-relative replacement: {message}"
    );
    assert!(!message.contains("/Users/dev"), "{message}");
    assert!(!message.contains("/home/alice"), "{message}");
}

fn write_fixture_files(dir: &Path, body: &str, comments: &Value) {
    fs::write(dir.join("issue-body.md"), body).expect("write fixture body");
    fs::write(
        dir.join("comments.json"),
        serde_json::to_string(comments).expect("comments json"),
    )
    .expect("write fixture comments");
}

fn write_pr_fixture(dir: &Path, repo: &str, pr: u64, value: Value) {
    let prs = dir.join("prs");
    fs::create_dir_all(&prs).expect("create prs dir");
    let slug = repo.replace('/', "__");
    fs::write(
        prs.join(format!("{slug}__{pr}.json")),
        serde_json::to_string(&value).expect("pr json"),
    )
    .expect("write pr fixture");
}

fn audit_single_comment_body(body: &str) -> Value {
    let tmp = TempDir::new().expect("tempdir");
    let comments_json = tmp.path().join("comments.json");
    fs::write(
        &comments_json,
        json!({
            "comments": [
                {"body": body, "url": "https://github.com/owner/repo/issues/1#issuecomment-record"}
            ]
        })
        .to_string(),
    )
    .expect("write comments json");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "audit",
        "--comments-json",
        comments_json.to_str().expect("comments path"),
        "--profile",
        "tracking",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    out.stdout_json()["payload"]["result"]["audit"].clone()
}

#[test]
fn record_post_state_with_payload_file_renders_v2_marker_in_dry_run() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "scope",
            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.post");
    assert_eq!(result["kind"], "state");
    let body = result["comment_body"]
        .as_str()
        .expect("comment_body in dry-run");
    assert!(
        body.starts_with("<!-- plan-issue-record:v2 role=state profile=tracking -->"),
        "{body}"
    );
    assert!(
        !body.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
        "{body}"
    );
    assert!(
        body.contains("<!-- plan-issue-record-payload:hex:"),
        "{body}"
    );
}

#[test]
fn record_post_state_summary_file_is_rendered_in_dry_run() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    let summary = tmp.path().join("summary.md");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "summary surface",
            "tasks": [],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &summary,
        "- Updated runtime-kit skills to the v3 surface.\n",
    )
    .expect("write summary");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--summary-file",
        summary.to_str().expect("summary str"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let body = parsed["payload"]["result"]["comment_body"]
        .as_str()
        .expect("comment body");
    assert!(
        body.contains("- Updated runtime-kit skills to the v3 surface."),
        "{body}"
    );
    assert!(
        body.starts_with("<!-- plan-issue-record:v2 role=state profile=tracking -->"),
        "{body}"
    );
}

#[test]
fn record_post_live_rejects_local_path_from_summary_file_before_provider_mutation() {
    let tmp = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe("gh", live_record_gh_stub());
    let log_path = tmp.path().join("gh.log");
    let log_s = log_path.to_string_lossy().to_string();

    let payload = tmp.path().join("state.json");
    let summary = tmp.path().join("summary.md");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "summary surface",
            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &summary,
        "- Evidence: /Users/dev/Project/private/rendered.md\n",
    )
    .expect("write summary");

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "sympoies/nils-cli",
            "record",
            "post",
            "--issue",
            "217",
            "--kind",
            "state",
            "--payload-file",
            payload.to_str().expect("payload str"),
            "--summary-file",
            summary.to_str().expect("summary str"),
        ],
        live_record_options(stub.path(), &[("PLAN_ISSUE_GH_LOG", &log_s)]),
    );

    assert_provider_payload_privacy_error(
        &out,
        "record-post-comment-post-failed",
        "$HOME/Project/private/rendered.md",
    );
    assert!(
        !log_path.exists(),
        "gh must not run when final rendered comment is unsafe"
    );
}

#[test]
fn record_post_state_execution_state_file_collapses_non_final_in_dry_run() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    let execution_state = tmp.path().join("state.md");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "ledger surface",
            "current": "working",
            "next_action": "continue",
            "tasks": [{"id": "1.1", "status": "pending", "title": "Demo task"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n## Execution State\n\n- Status: in-progress\n\n## Task Ledger\n\n| ID | Status | Task |\n| --- | --- | --- |\n| 1.1 | pending | Demo task |\n\n## Validation\n\n| Command | Status |\n| --- | --- |\n| `true` | pass |\n",
    )
    .expect("write execution state");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--execution-state-file",
        execution_state.to_str().expect("execution state str"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let body = parsed["payload"]["result"]["comment_body"]
        .as_str()
        .expect("comment body");
    assert_comment_visible_prefix(
        body,
        concat!(
            "<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n",
            "## Execution State\n\n",
            "- Profile: tracking\n",
            "- Status: in-progress\n\n",
            "## Task Ledger\n\n",
            "<details>\n",
            "<summary>Show task ledger</summary>\n\n",
            "| ID | Status | Task |\n",
            "| --- | --- | --- |\n",
            "| 1.1 | pending | Demo task |\n\n",
            "</details>\n\n",
            "## Validation\n\n",
            "| Command | Status |\n",
            "| --- | --- |\n",
            "| `true` | pass |\n\n",
        ),
    );
    let details_start = body.find("<details>").expect("details start");
    let validation_start = body.find("## Validation").expect("validation heading");
    let payload_start = body
        .find("<!-- plan-issue-record-payload:hex:")
        .expect("payload marker");
    assert!(details_start < validation_start, "{body}");
    assert!(validation_start < payload_start, "{body}");
}

#[test]
fn record_post_state_execution_state_file_expands_final_in_dry_run() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    let execution_state = tmp.path().join("state.md");
    fs::write(
        &payload,
        json!({
            "status": "complete",
            "target_scope": "ledger surface",
            "current": "done",
            "next_action": "closeout",
            "tasks": [{"id": "1.1", "status": "done", "title": "Demo task"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n## Execution State\n\n- Status: complete\n\n## Task Ledger\n\n| ID | Status | Task |\n| --- | --- | --- |\n| 1.1 | done | Demo task |\n",
    )
    .expect("write execution state");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--execution-state-file",
        execution_state.to_str().expect("execution state str"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let body = parsed["payload"]["result"]["comment_body"]
        .as_str()
        .expect("comment body");
    assert_comment_visible_prefix(
        body,
        concat!(
            "<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n",
            "## Execution State\n\n",
            "- Profile: tracking\n",
            "- Status: complete\n\n",
            "## Task Ledger\n\n",
            "| ID | Status | Task |\n",
            "| --- | --- | --- |\n",
            "| 1.1 | done | Demo task |\n\n",
        ),
    );
}

#[test]
fn record_post_state_execution_state_file_preserves_execution_metadata_fields() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    let execution_state = tmp.path().join("state.md");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "plan issue lifecycle comment visibility",
            "current": "implement Sprint 1 in sympoies/nils-cli",
            "next_action": "add lifecycle visible rendering support",
            "tasks": [{"id": "1.1", "status": "pending", "title": "Renderer"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &execution_state,
        "# Execution State: Plan Issue Lifecycle Comment Visibility\n\n<!-- execute-from-tracking-issue:state:v1 -->\n## Execution State\n\n- Status: tracking issue opened\n- Profile: tracking\n- Target scope: make plan-issue lifecycle comments visibly include detailed state, validation, review, session, and closeout evidence\n- Current task: implement Sprint 1 in sympoies/nils-cli.\n- Next task: add lifecycle visible rendering support to plan-issue record post and record close.\n- Last updated: 2026-05-25\n- Branch: feat/plan-issue-state-visibility\n- Source document: docs/plans/plan-issue-lifecycle-comment-visibility/plan-issue-lifecycle-comment-visibility-plan.md\n- Plan document: docs/plans/plan-issue-lifecycle-comment-visibility/plan-issue-lifecycle-comment-visibility-plan.md\n- Review source: docs/plans/plan-issue-lifecycle-comment-visibility/plan-issue-lifecycle-comment-visibility-review-source.md\n\n## Task Ledger\n\n| ID | Status | Task |\n| --- | --- | --- |\n| 1.1 | pending | Renderer |\n",
    )
    .expect("write execution state");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "115",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--execution-state-file",
        execution_state.to_str().expect("execution state str"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let body = parsed["payload"]["result"]["comment_body"]
        .as_str()
        .expect("comment body");
    assert_comment_visible_prefix(
        body,
        concat!(
            "<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n",
            "## Execution State\n\n",
            "- Profile: tracking\n",
            "- Status: tracking issue opened\n",
            "- Target scope: make plan-issue lifecycle comments visibly include detailed state, validation, review, session, and closeout evidence\n",
            "- Current task: implement Sprint 1 in sympoies/nils-cli.\n",
            "- Next task: add lifecycle visible rendering support to plan-issue record post and record close.\n",
            "- Last updated: 2026-05-25\n",
            "- Branch: feat/plan-issue-state-visibility\n",
            "- Source document: docs/plans/plan-issue-lifecycle-comment-visibility/plan-issue-lifecycle-comment-visibility-plan.md\n",
            "- Plan document: docs/plans/plan-issue-lifecycle-comment-visibility/plan-issue-lifecycle-comment-visibility-plan.md\n",
            "- Review source: docs/plans/plan-issue-lifecycle-comment-visibility/plan-issue-lifecycle-comment-visibility-review-source.md\n\n",
            "## Task Ledger\n\n",
            "<details>\n",
            "<summary>Show task ledger</summary>\n\n",
            "| ID | Status | Task |\n",
            "| --- | --- | --- |\n",
            "| 1.1 | pending | Renderer |\n\n",
            "</details>\n\n",
        ),
    );
}

#[test]
fn record_post_execution_state_file_requires_state_kind_and_task_ledger() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("validation.json");
    let execution_state = tmp.path().join("state.md");
    fs::write(
        &payload,
        json!({"overall": "pass", "commands": [], "waivers": []}).to_string(),
    )
    .expect("write payload");
    fs::write(&execution_state, "# State\n").expect("write execution state");

    let wrong_kind = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "validation",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--execution-state-file",
        execution_state.to_str().expect("execution state str"),
    ]);
    assert_ne!(wrong_kind.code, 0);
    assert!(
        wrong_kind
            .stdout_text()
            .contains("record-post-execution-state-file-kind-invalid"),
        "{}",
        wrong_kind.stdout_text()
    );

    let state_payload = tmp.path().join("state.json");
    fs::write(
        &state_payload,
        json!({
            "status": "in-progress",
            "tasks": [],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write state payload");
    let missing_ledger = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        state_payload.to_str().expect("payload str"),
        "--execution-state-file",
        execution_state.to_str().expect("execution state str"),
    ]);
    assert_ne!(missing_ledger.code, 0);
    assert!(
        missing_ledger
            .stdout_text()
            .contains("record-post-execution-state-task-ledger-missing"),
        "{}",
        missing_ledger.stdout_text()
    );
}

#[test]
fn record_post_state_rejects_payload_that_cannot_drive_dashboard() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "schema drift",
            "current": "PRs are open as drafts",
            "next_action": "review draft PRs",
            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
            "prs": [
                {"ref": "owner/repo#9", "url": "https://github.com/owner/repo/pull/9", "status": "draft-open"}
            ],
            "blockers": [
                {"code": "live-home-drift", "status": "open", "detail": "extra surface"}
            ],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
    ]);

    assert_ne!(out.code, 0, "invalid state payload must fail");
    assert!(
        out.stderr_text()
            .contains("record-post-payload-schema-invalid")
            || out
                .stdout_text()
                .contains("record-post-payload-schema-invalid"),
        "expected schema-invalid error: stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
}

#[test]
fn record_post_rejects_source_plan_and_closeout_kinds() {
    for kind in ["source", "plan", "closeout"] {
        let out = common::run_plan_issue_local(&[
            "--format", "json", "record", "post", "--issue", "1", "--kind", kind,
        ]);
        assert_ne!(out.code, 0, "kind {kind} should be rejected");
        assert!(
            out.stderr_text().contains("record-post-")
                || out.stdout_text().contains("record-post-"),
            "expected record-post error for kind {kind}: stdout={} stderr={}",
            out.stdout_text(),
            out.stderr_text()
        );
    }
}

#[test]
fn record_repair_dashboard_rejects_malformed_state_payload_instead_of_pending() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    fs::write(&body_path, "## Current Dashboard\n\n- Status: pending\n").expect("write body");

    fs::write(
        &comments_path,
        json!({
            "comments": [
                {
                    "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
                    "body": v2_comment_body(
                        "state",
                        "tracking",
                        json!({
                            "status": "in-progress",
                            "target_scope": "schema drift",
                            "current": "PRs are open as drafts",
                            "next_action": "review draft PRs",
                            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
                            "prs": [
                                {"ref": "owner/repo#9", "url": "https://github.com/owner/repo/pull/9", "status": "draft-open-green"}
                            ],
                            "blockers": [
                                {"code": "live-home-drift", "status": "open", "detail": "extra surface"}
                            ],
                            "links": {}
                        }),
                    ),
                    "created_at": "2026-05-23T10:00:00Z"
                }
            ]
        })
        .to_string(),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "repair-dashboard",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
    ]);

    assert_ne!(out.code, 0, "malformed state payload must fail repair");
    assert!(
        out.stderr_text().contains("malformed payload")
            || out.stdout_text().contains("malformed payload"),
        "expected malformed payload error: stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
}

#[test]
fn record_repair_dashboard_allows_new_valid_state_to_supersede_old_malformed_state() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    fs::write(&body_path, "## Current Dashboard\n\n- Status: pending\n").expect("write body");

    fs::write(
        &comments_path,
        json!({
            "comments": [
                {
                    "url": "https://github.com/owner/repo/issues/9#issuecomment-state-old",
                    "body": v2_comment_body(
                        "state",
                        "tracking",
                        json!({
                            "status": "in-progress",
                            "target_scope": "old schema drift",
                            "prs": [{"ref": "owner/repo#9", "status": "draft-open"}],
                            "blockers": [{"code": "x"}],
                            "links": {}
                        }),
                    ),
                    "created_at": "2026-05-23T10:00:00Z"
                },
                {
                    "url": "https://github.com/owner/repo/issues/9#issuecomment-state-new",
                    "body": v2_comment_body(
                        "state",
                        "tracking",
                        json!({
                            "status": "in-progress",
                            "target_scope": "schema repaired",
                            "current": "latest valid state",
                            "next_action": "continue",
                            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
                            "prs": [{"ref": "owner/repo#9", "url": "https://github.com/owner/repo/pull/9", "status": "open"}],
                            "blockers": ["older malformed state superseded"],
                            "links": {}
                        }),
                    ),
                    "created_at": "2026-05-23T11:00:00Z"
                }
            ]
        })
        .to_string(),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "repair-dashboard",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let dashboard = parsed["payload"]["result"]["dashboard_markdown"]
        .as_str()
        .expect("dashboard markdown");
    assert!(dashboard.contains("- Status: in-progress"), "{dashboard}");
    assert!(
        dashboard.contains("- Target scope: schema repaired"),
        "{dashboard}"
    );
    assert!(
        dashboard.contains("https://github.com/owner/repo/issues/9#issuecomment-state-new"),
        "{dashboard}"
    );
}

#[test]
fn record_repair_dashboard_renders_canonical_dashboard_from_body_and_comments() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    fs::write(
        &body_path,
        "## Current Dashboard\n\n- Status: in-progress\n",
    )
    .expect("write body");

    let comments = json!({
        "comments": [
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-state-1",
                "body": v2_comment_body(
                    "state",
                    "tracking",
                    json!({
                        "status": "in-progress",
                        "target_scope": "sample plan",
                        "current": "Sprint 2 in progress",
                        "next_action": "land Sprint 2",
                        "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
                        "prs": [{"ref": "owner/repo#1", "url": "https://github.com/owner/repo/pull/1", "status": "merged"}],
                        "blockers": [],
                        "links": {}
                    }),
                ),
                "created_at": "2026-05-23T10:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-source",
                "body": v2_comment_body(
                    "source",
                    "tracking",
                    json!({"path": "docs/plans/sample/sample-discussion-source.md", "commit": "abc"}),
                ),
                "created_at": "2026-05-23T09:00:00Z"
            }
        ]
    });
    fs::write(
        &comments_path,
        serde_json::to_string(&comments).expect("json"),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "repair-dashboard",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let dashboard = parsed["payload"]["result"]["dashboard_markdown"]
        .as_str()
        .expect("dashboard markdown");
    assert!(dashboard.starts_with("## Current Dashboard"), "{dashboard}");
    assert!(dashboard.contains("- Status: in-progress"), "{dashboard}");
    // Source URL from latest audit evidence should appear in Durable Record.
    assert!(
        dashboard.contains("https://github.com/owner/repo/issues/9#issuecomment-source"),
        "{dashboard}"
    );
}

#[test]
fn record_repair_dashboard_live_rejects_local_path_in_rendered_dashboard_before_edit() {
    let tmp = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe("gh", live_record_gh_stub());
    let log_path = tmp.path().join("gh.log");
    let log_s = log_path.to_string_lossy().to_string();

    let comments = json!([
        {
            "url": "https://github.com/owner/repo/issues/9#issuecomment-state-1",
            "body": v2_comment_body(
                "state",
                "tracking",
                json!({
                    "status": "in-progress",
                    "target_scope": "/Users/dev/Project/private/dashboard",
                    "current": "repair dashboard",
                    "next_action": "block unsafe payload",
                    "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
                    "prs": [],
                    "blockers": [],
                    "links": {}
                }),
            ),
            "created_at": "2026-05-23T10:00:00Z"
        }
    ]);
    let view_json = json!({
        "body": "## Current Dashboard\n\n- Status: stale\n",
        "comments": comments
    })
    .to_string();
    let view_json_path = tmp.path().join("issue-view.json");
    fs::write(&view_json_path, &view_json).expect("write view json");
    let view_json_path_s = view_json_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "sympoies/nils-cli",
            "record",
            "repair-dashboard",
            "--issue",
            "217",
        ],
        live_record_options(
            stub.path(),
            &[
                ("PLAN_ISSUE_GH_LOG", &log_s),
                ("PLAN_ISSUE_GH_VIEW_JSON_FILE", &view_json_path_s),
            ],
        ),
    );

    assert_provider_payload_privacy_error(
        &out,
        "record-repair-edit-failed",
        "$HOME/Project/private/dashboard",
    );
    let log = fs::read_to_string(&log_path).expect("read gh log");
    assert!(log.contains("issue view 217"), "{log}");
    assert!(!log.contains("issue edit 217"), "{log}");
}

#[test]
fn record_repair_dashboard_out_writes_local_dashboard_file() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    let out_path = tmp.path().join("dashboard.md");
    fs::write(&body_path, "## Current Dashboard\n\n- Status: stale\n").expect("write body");
    fs::write(
        &comments_path,
        json!({
            "comments": [
                {
                    "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
                    "body": v2_comment_body(
                        "state",
                        "tracking",
                        json!({
                            "status": "in-progress",
                            "target_scope": "repair out",
                            "current": "refresh dashboard",
                            "next_action": "continue",
                            "tasks": [],
                            "prs": [],
                            "blockers": [],
                            "links": {}
                        }),
                    ),
                    "created_at": "2026-05-23T10:00:00Z"
                }
            ]
        })
        .to_string(),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "repair-dashboard",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
        "--out",
        out_path.to_str().expect("out path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "local");
    assert_eq!(
        result["out_path"],
        out_path.to_string_lossy().as_ref(),
        "{result}"
    );
    let dashboard = fs::read_to_string(&out_path).expect("read dashboard");
    assert!(dashboard.starts_with("## Current Dashboard"), "{dashboard}");
    assert!(dashboard.contains("- Status: in-progress"), "{dashboard}");
}

#[test]
fn record_close_requires_non_empty_approval() {
    let out =
        common::run_plan_issue_local(&["--format", "json", "record", "close", "--issue", "9"]);
    assert_ne!(out.code, 0, "missing --approval should fail");
    assert!(
        out.stderr_text().contains("record-close-missing-approval")
            || out.stdout_text().contains("record-close-missing-approval"),
        "stderr: {} stdout: {}",
        out.stderr_text(),
        out.stdout_text()
    );
}

fn build_closeout_evidence(linked_pr_ref: &str) -> Value {
    json!({
        "comments": [
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-source",
                "body": v2_comment_body(
                    "source",
                    "tracking",
                    json!({"path": "docs/plans/sample/sample-discussion-source.md", "commit": "src1234"}),
                ),
                "created_at": "2026-05-23T09:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-plan",
                "body": v2_comment_body(
                    "plan",
                    "tracking",
                    json!({"path": "docs/plans/sample/sample-plan.md", "commit": "pln1234"}),
                ),
                "created_at": "2026-05-23T09:01:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
                "body": v2_comment_body(
                    "state",
                    "tracking",
                    json!({
                        "status": "complete",
                        "target_scope": "sample plan",
                        "current": "complete",
                        "next_action": "closeout",
                        "tasks": [
                            {"id": "1.1", "status": "done", "title": "x"},
                            {"id": "1.2", "status": "deferred", "title": "y"},
                        ],
                        "prs": [{"ref": linked_pr_ref, "url": "https://github.com/owner/repo/pull/1", "status": "merged"}],
                        "blockers": [],
                        "links": {}
                    }),
                ),
                "created_at": "2026-05-23T10:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-session",
                "body": v2_comment_body(
                    "session",
                    "tracking",
                    json!({
                        "summary": "implementation session completed",
                        "highlights": ["state, validation, and review evidence recorded"]
                    }),
                ),
                "created_at": "2026-05-23T10:30:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-validation",
                "body": v2_comment_body(
                    "validation",
                    "tracking",
                    json!({
                        "overall": "pass",
                        "commands": [{"command": "cargo test", "status": "pass"}],
                        "waivers": []
                    }),
                ),
                "created_at": "2026-05-23T11:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-review",
                "body": v2_comment_body(
                    "review",
                    "tracking",
                    json!({
                        "decision": "approve",
                        "lenses": ["testing", "maintainability"],
                        "findings": [],
                    }),
                ),
                "created_at": "2026-05-23T12:00:00Z"
            }
        ]
    })
}

fn remove_session_comment(comments: &mut Value) {
    comments["comments"]
        .as_array_mut()
        .expect("comments array")
        .retain(|comment| {
            !comment["body"]
                .as_str()
                .unwrap_or_default()
                .contains("role=session")
        });
}

#[test]
fn record_close_body_file_mode_blocks_unresolved_linked_pr() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    fs::write(&body_path, "## Current Dashboard\n").expect("write body");
    fs::write(
        &comments_path,
        build_closeout_evidence("owner/repo#1").to_string(),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "https://github.com/owner/repo/issues/9#issuecomment-approval",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
    ]);

    assert_ne!(out.code, 0, "missing provider PR evidence should block");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("linked-pr-not-merged"),
        "expected linked-pr-not-merged without PR evidence: {joined}"
    );
}

#[test]
fn record_close_fixture_passes_strict_gate_with_complete_v2_evidence() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n\n- Status: in-progress\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "deadbeefcafebabe"},
            "statusCheckRollup": {"state": "success"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "https://github.com/owner/repo/issues/9#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.close");
    assert_eq!(result["mode"], "fixture");
    assert_eq!(result["dry_run"], true);
    let preview = &result["preview"];
    let body = preview["closeout_comment_body"]
        .as_str()
        .expect("closeout body");
    assert!(
        body.starts_with("<!-- plan-issue-record:v2 role=closeout profile=tracking -->"),
        "{body}"
    );
    assert!(
        !body.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
        "{body}"
    );
    assert!(
        body.contains("<!-- plan-issue-record-payload:hex:"),
        "{body}"
    );
    let audit = audit_single_comment_body(body);
    let closeout = &audit["evidence"]["closeout"]["payload"]["data"];
    assert_eq!(closeout["final_status"], "complete");
    assert_eq!(closeout["linked_prs"][0]["merge_sha"], "deadbeefcafebabe");
    let final_dashboard = preview["final_dashboard"]
        .as_str()
        .expect("final dashboard");
    assert!(
        final_dashboard.starts_with("## Final Dashboard"),
        "{final_dashboard}"
    );
}

#[test]
fn record_close_fixture_blocks_when_session_comment_missing() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let mut comments = build_closeout_evidence("owner/repo#1");
    remove_session_comment(&mut comments);
    let body = "## Current Dashboard\n\n- Status: complete\n- Latest session: pending\n\n## Session Log\n\n- Notes embedded in state only.\n";
    write_fixture_files(&fixture, body, &comments);
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "deadbeefcafebabe"},
            "statusCheckRollup": {"state": "success"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "https://github.com/owner/repo/issues/9#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_ne!(out.code, 0, "missing session must block closeout");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("session-missing"),
        "expected session-missing, got: {joined}"
    );
}

#[test]
fn record_close_fixture_blocks_when_linked_pr_not_merged() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "OPEN",
            "mergeCommit": null,
            "statusCheckRollup": {"state": "pending"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_ne!(out.code, 0, "unmerged PR should block strict gate");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("linked-pr-not-merged"),
        "expected linked-pr-not-merged code, got: {joined}"
    );
}

#[test]
fn record_close_fixture_blocks_when_review_request_changes() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    // Replace the review entry in the evidence stack with a
    // request-changes decision.
    let mut comments = build_closeout_evidence("owner/repo#1");
    let comments_list = comments["comments"].as_array_mut().expect("comments array");
    let last_index = comments_list.len() - 1;
    comments_list[last_index] = json!({
        "url": "https://github.com/owner/repo/issues/9#issuecomment-review-rej",
        "body": v2_comment_body(
            "review",
            "tracking",
            json!({"decision": "request-changes", "findings": []}),
        ),
        "created_at": "2026-05-23T12:00:00Z"
    });
    write_fixture_files(&fixture, "## Current Dashboard\n", &comments);
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "success"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);
    assert_ne!(out.code, 0);
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("review-rejected"),
        "expected review-rejected: {joined}"
    );
}

#[test]
fn record_close_fixture_passes_with_non_required_failure_when_zero_required() {
    // Regression for sympoies/nils-cli#502:
    // PR merged, zero required checks, one non-required check failed.
    // Strict closeout gate must not block on non-required failures.
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n\n- Status: in-progress\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "deadbeefcafebabe"},
            "statusCheckRollup": {"state": "failure"},
            "requiredCheckRollup": {"state": "success", "count": 0},
            "nonRequiredFailures": ["scripts/ci/all.sh"],
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "https://github.com/owner/repo/issues/9#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.close");
    let preview = &result["preview"];
    assert!(
        preview["blocked_codes"]
            .as_array()
            .expect("array")
            .is_empty(),
        "blocked_codes should be empty: {}",
        preview["blocked_codes"]
    );
    let linked = &result["linked_prs"][0];
    assert_eq!(linked["required_count"], 0);
    assert_eq!(linked["required_state"], "pass");
    assert_eq!(linked["non_required_failures"][0], "scripts/ci/all.sh");

    // sympoies/nils-cli#561 follow-up: rendered closeout-comment table
    // must label the zero-required case `none required`, not `unknown`
    // and not `pass (0)`.
    let body = preview["closeout_comment_body"]
        .as_str()
        .expect("closeout body");
    assert!(
        body.contains("| none required |"),
        "expected `none required` in closeout body, got: {body}"
    );
    assert!(
        !body.contains("| unknown |"),
        "expected no `unknown` cell in closeout body, got: {body}"
    );
}

#[test]
fn record_close_fixture_passes_with_non_required_failure_when_required_pass() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "failure"},
            "requiredCheckRollup": {"state": "success", "count": 3},
            "nonRequiredFailures": ["lint-experimental"],
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
}

#[test]
fn record_close_fixture_blocks_with_linked_pr_checks_failed_when_required_fail() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "failure"},
            "requiredCheckRollup": {"state": "failure", "count": 2},
            "nonRequiredFailures": [],
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_ne!(out.code, 0, "required-check failure must block");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("linked-pr-checks-failed"),
        "expected linked-pr-checks-failed: {joined}"
    );
    assert!(
        !joined.contains("linked-pr-not-merged"),
        "must not collapse into linked-pr-not-merged: {joined}"
    );
}

#[test]
fn record_close_fixture_override_passes_when_required_unknown_aggregate_fails() {
    // When the adapter cannot resolve required-check state (`requiredCheckRollup`
    // absent), the gate stays conservative and blocks on aggregate failure.
    // The override flag unblocks it and records evidence in the closeout body.
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "failure"},
            "nonRequiredFailures": ["opt-in/lint"],
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    // Without the override → blocked.
    let blocked = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);
    assert_ne!(blocked.code, 0, "conservative block expected");
    assert!(
        format!("{}\n{}", blocked.stderr_text(), blocked.stdout_text())
            .contains("linked-pr-checks-failed"),
        "expected linked-pr-checks-failed under unknown required state"
    );

    // With the override + reason → passes and records evidence.
    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--allow-non-required-check-failure",
        "--allow-non-required-check-failure-reason",
        "operator verified opt-in/lint is non-required",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(
        out.code,
        0,
        "override should unblock: {}",
        out.stderr_text()
    );
    let parsed = out.stdout_json();
    let body = parsed["payload"]["result"]["preview"]["closeout_comment_body"]
        .as_str()
        .expect("closeout body")
        .to_string();
    assert!(
        body.contains("non-required-check failure override"),
        "expected override summary in body: {body}"
    );
    let audit = audit_single_comment_body(&body);
    let closeout = &audit["evidence"]["closeout"]["payload"]["data"];
    let override_block = &closeout["non_required_check_override"];
    assert_eq!(
        override_block["reason"], "operator verified opt-in/lint is non-required",
        "override block reason recorded"
    );
    assert!(
        override_block["observed_non_required_failures"]
            .as_array()
            .is_some_and(|arr| arr.iter().any(|item| item == "owner/repo#1: opt-in/lint")),
        "expected observed failure list to include opt-in/lint: {override_block}"
    );
}

#[test]
fn record_close_fixture_blocks_when_state_not_complete() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let mut comments = build_closeout_evidence("owner/repo#1");
    let comments_list = comments["comments"].as_array_mut().expect("comments array");
    // Replace the state entry (index 2) with status=in-progress.
    comments_list[2] = json!({
        "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
        "body": v2_comment_body(
            "state",
            "tracking",
            json!({
                "status": "in-progress",
                "target_scope": "x",
                "tasks": [],
                "prs": [],
                "blockers": [],
                "links": {}
            }),
        ),
        "created_at": "2026-05-23T10:00:00Z"
    });
    write_fixture_files(&fixture, "## Current Dashboard\n", &comments);
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "success"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);
    assert_ne!(out.code, 0);
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("state-not-complete"),
        "expected state-not-complete: {joined}"
    );
}

#[test]
fn record_open_fixture_mode_returns_v2_evidence_urls() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    let comments = json!({
        "comments": [
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-source",
                "body": v2_comment_body(
                    "source",
                    "tracking",
                    json!({"path": "docs/plans/sample/sample-discussion-source.md", "commit": "src1"}),
                ),
                "created_at": "2026-05-23T09:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-plan",
                "body": v2_comment_body(
                    "plan",
                    "tracking",
                    json!({"path": "docs/plans/sample/sample-plan.md", "commit": "pln1"}),
                ),
                "created_at": "2026-05-23T09:01:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
                "body": v2_comment_body(
                    "state",
                    "tracking",
                    json!({"status": "in-progress", "tasks": [], "prs": [], "blockers": [], "links": {}}),
                ),
                "created_at": "2026-05-23T09:02:00Z"
            }
        ]
    });
    write_fixture_files(&fixture, body, &comments);

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "open",
        "--fixture",
        fixture.to_str().expect("fixture path"),
        "--title",
        "Sample Plan",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.open");
    assert_eq!(result["mode"], "fixture");
    let comments_result = &result["comments"];
    assert_eq!(
        comments_result["source"],
        "https://github.com/owner/repo/issues/9#issuecomment-source"
    );
    assert_eq!(
        comments_result["plan"],
        "https://github.com/owner/repo/issues/9#issuecomment-plan"
    );
    assert_eq!(
        comments_result["state"],
        "https://github.com/owner/repo/issues/9#issuecomment-state"
    );
}

#[test]
fn record_post_state_fixture_returns_rendered_body_without_provider_call() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");
    write_fixture_files(&fixture, "## Current Dashboard\n", &json!({"comments": []}));
    let payload = tmp.path().join("payload.json");
    fs::write(
        &payload,
        json!({"status": "in-progress", "tasks": [], "prs": [], "blockers": [], "links": {}})
            .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "9",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "fixture");
    assert_eq!(result["kind"], "state");
    let body = result["comment_body"]
        .as_str()
        .expect("comment body in fixture mode");
    assert!(
        body.contains("<!-- plan-issue-record:v2 role=state profile=tracking -->"),
        "{body}"
    );
}

fn record_open_dry_run_gh_stub() -> &'static str {
    r#"#!/usr/bin/env bash
echo "record_open_dry_run_gh_stub should not be called" >&2
exit 1
"#
}

fn dry_run_cmd_options(stub_dir: &Path) -> CmdOptions {
    common::plan_issue_cmd_options()
        .with_env_remove_prefix("PLAN_ISSUE_GH_")
        .with_path_prepend(stub_dir)
}

#[test]
fn record_open_dry_run_returns_preview_without_gh_calls() {
    use nils_test_support::git::{InitRepoOptions, git, init_repo_with};

    let stub = StubBinDir::new();
    stub.write_exe("gh", record_open_dry_run_gh_stub());

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    let bundle = repo.path().join("docs/plans/sample");
    fs::create_dir_all(&bundle).expect("create bundle dir");
    let source = bundle.join("sample-discussion-source.md");
    let plan = bundle.join("sample-plan.md");
    let execution_state = bundle.join("sample-execution-state.md");
    fs::write(&source, "# Source\n\n- Decision: implement v2 lifecycle.\n").expect("write source");
    fs::write(
        &plan,
        "# Plan: Sample Plan\n\n## Overview\n\n- Sample plan body.\n\n## Read First\n\n- Primary source: docs/plans/sample/sample-discussion-source.md\n- Source type: discussion-to-implementation-doc\n- Open questions carried into execution: none\n\n## Scope\n\n- In scope:\n  - Demo plan.\n- Out of scope:\n  - none.\n\n## Assumptions\n\n1. Demo only.\n\n## Sprint 1: Demo\n\n**Goal**: Demo the surface.\n\n**PR grouping intent**: group\n**Execution Profile**: serial\n\n### Task 1.1: Demo task\n\n- **Location**:\n  - `docs/plans/sample/sample-plan.md`\n- **Description**: Demo task description.\n- **Dependencies**:\n  - none\n- **Complexity**: 1\n- **Acceptance criteria**:\n  - The demo task is complete.\n- **Validation**:\n  - `true`\n",
    )
    .expect("write plan");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## Execution State\n\n- Status: pending\n- Target scope: Sample Plan\n",
    )
    .expect("write execution state");
    git(repo.path(), &["add", "."]);
    git(
        repo.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "seed bundle",
            "--no-gpg-sign",
        ],
    );

    let opts = dry_run_cmd_options(stub.path()).with_cwd(repo.path());
    let bundle_arg = bundle.to_string_lossy().to_string();
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            &bundle_arg,
        ],
        &opts,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.open");
    assert_eq!(result["mode"], "dry-run");
    assert_eq!(result["dry_run"], true);
    let preview = &result["preview"];
    assert_eq!(preview["plan_title"], "Plan: Sample Plan");
    let source_comment = preview["comments"]["source"]
        .as_str()
        .expect("source comment");
    assert!(
        source_comment.starts_with("<!-- plan-issue-record:v2 role=source profile=tracking -->"),
        "{source_comment}"
    );
    let plan_comment = preview["comments"]["plan"].as_str().expect("plan comment");
    let state_comment = preview["comments"]["state"]
        .as_str()
        .expect("state comment");
    for (label, comment) in [
        ("source", source_comment),
        ("plan", plan_comment),
        ("state", state_comment),
    ] {
        assert!(
            !comment.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
            "{label} comment should not visibly leak payload JSON:\n{comment}"
        );
    }
    assert!(
        state_comment.contains("# Sample Execution State"),
        "{state_comment}"
    );
    assert!(
        state_comment.contains("- Status: pending"),
        "{state_comment}"
    );
    assert!(
        !state_comment.contains("Initial execution state seeded"),
        "{state_comment}"
    );

    let comments_json = repo.path().join("comments.json");
    fs::write(
        &comments_json,
        json!({
            "comments": [
                {"body": source_comment, "url": "https://github.com/owner/repo/issues/1#issuecomment-source"},
                {"body": plan_comment, "url": "https://github.com/owner/repo/issues/1#issuecomment-plan"},
                {"body": state_comment, "url": "https://github.com/owner/repo/issues/1#issuecomment-state"}
            ]
        })
        .to_string(),
    )
    .expect("write comments json");

    let audit = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "audit",
            "--comments-json",
            comments_json.to_str().expect("comments path"),
            "--profile",
            "tracking",
        ],
        &opts,
    );
    assert_eq!(audit.code, 0, "stderr: {}", audit.stderr_text());
    let parsed_audit: Value = serde_json::from_str(&audit.stdout_text()).expect("audit json");
    let audit_result = &parsed_audit["payload"]["result"]["audit"];
    assert_eq!(audit_result["recognized_count"], 3);
    assert_eq!(
        audit_result["missing_required"],
        json!([]),
        "{audit_result}"
    );
}

/// Write the minimal source/plan/execution-state trio used by the `record open`
/// dry-run tests into `bundle` (created if needed).
fn write_sample_bundle(bundle: &Path) {
    fs::create_dir_all(bundle).expect("create bundle dir");
    fs::write(
        bundle.join("sample-discussion-source.md"),
        "# Source\n\n- Decision: implement v2 lifecycle.\n",
    )
    .expect("write source");
    fs::write(
        bundle.join("sample-plan.md"),
        "# Plan: Sample Plan\n\n## Overview\n\n- Sample plan body.\n\n## Read First\n\n- Primary source: docs/plans/sample/sample-discussion-source.md\n- Source type: discussion-to-implementation-doc\n- Open questions carried into execution: none\n\n## Scope\n\n- In scope:\n  - Demo plan.\n- Out of scope:\n  - none.\n\n## Assumptions\n\n1. Demo only.\n\n## Sprint 1: Demo\n\n**Goal**: Demo the surface.\n\n**PR grouping intent**: group\n**Execution Profile**: serial\n\n### Task 1.1: Demo task\n\n- **Location**:\n  - `docs/plans/sample/sample-plan.md`\n- **Description**: Demo task description.\n- **Dependencies**:\n  - none\n- **Complexity**: 1\n- **Acceptance criteria**:\n  - The demo task is complete.\n- **Validation**:\n  - `true`\n",
    )
    .expect("write plan");
    fs::write(
        bundle.join("sample-execution-state.md"),
        "# Sample Execution State\n\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## Execution State\n\n- Status: pending\n- Target scope: Sample Plan\n",
    )
    .expect("write execution state");
}

fn commit_all(repo: &Path) {
    use nils_test_support::git::git;
    git(repo, &["add", "."]);
    git(
        repo,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "seed bundle",
            "--no-gpg-sign",
        ],
    );
}

/// Regression for the false `record-open-uncommitted` tracked in
/// graysurf/plan-tracking-testbed#48: a committed bundle passed through a
/// *relative* `--bundle` must resolve its commit, not be misread as
/// uncommitted. Before the fix `last_commit_for_path` ran `git log` from the
/// bundle's parent dir but passed the full relative path as the pathspec, which
/// re-anchored under that subdir cwd and matched nothing.
#[test]
fn record_open_dry_run_resolves_relative_bundle() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let stub = StubBinDir::new();
    stub.write_exe("gh", record_open_dry_run_gh_stub());

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    write_sample_bundle(&repo.path().join("docs/plans/sample"));
    commit_all(repo.path());

    // Relative `--bundle`, resolved against the process cwd (the repo root).
    let opts = dry_run_cmd_options(stub.path()).with_cwd(repo.path());
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            "docs/plans/sample",
        ],
        &opts,
    );
    assert_eq!(
        out.code,
        0,
        "relative --bundle must succeed; stderr: {}",
        out.stderr_text()
    );
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let preview = &parsed["payload"]["result"]["preview"];
    let source_commit = preview["source_commit"]
        .as_str()
        .expect("source_commit string");
    let plan_commit = preview["plan_commit"].as_str().expect("plan_commit string");
    assert!(
        !source_commit.is_empty(),
        "committed source must resolve a commit: {preview}"
    );
    assert!(
        !plan_commit.is_empty(),
        "committed plan must resolve a commit: {preview}"
    );
    let source_comment = preview["comments"]["source"]
        .as_str()
        .expect("source comment");
    assert!(
        source_comment.contains("- Commit: `"),
        "committed source snapshot should render a Commit line:\n{source_comment}"
    );
    assert!(
        source_comment.contains("- Snapshot mode: local committed Markdown"),
        "committed snapshot should be labeled committed:\n{source_comment}"
    );
}

/// Companion to graysurf/plan-tracking-testbed#48: `--allow-dirty` must actually
/// bypass the commit check, as its error hint advertises. A never-committed
/// bundle is rejected by default but allowed through with `--allow-dirty`,
/// recording an empty commit (no rendered `Commit:` line) instead of failing
/// `record-open-uncommitted`.
#[test]
fn record_open_allow_dirty_permits_uncommitted_bundle() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let stub = StubBinDir::new();
    stub.write_exe("gh", record_open_dry_run_gh_stub());

    // The repo has history (initial commit), but the bundle files below are
    // never committed — the realistic "open a record before committing the
    // bundle" case, distinct from an empty repo with an unborn HEAD.
    let repo = init_repo_with(
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    write_sample_bundle(&repo.path().join("docs/plans/sample"));
    // Intentionally left uncommitted (untracked working-tree files).

    let opts = dry_run_cmd_options(stub.path()).with_cwd(repo.path());

    // Default: an uncommitted bundle is rejected.
    let blocked = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            "docs/plans/sample",
        ],
        &opts,
    );
    assert_ne!(
        blocked.code, 0,
        "an uncommitted bundle must be rejected without --allow-dirty"
    );

    // With --allow-dirty: the open proceeds and the snapshot omits the commit.
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            "docs/plans/sample",
            "--allow-dirty",
        ],
        &opts,
    );
    assert_eq!(
        out.code,
        0,
        "--allow-dirty must bypass the commit check; stderr: {}",
        out.stderr_text()
    );
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let preview = &parsed["payload"]["result"]["preview"];
    assert_eq!(
        preview["source_commit"], "",
        "uncommitted snapshot records an empty commit: {preview}"
    );
    let source_comment = preview["comments"]["source"]
        .as_str()
        .expect("source comment");
    assert!(
        !source_comment.contains("- Commit:"),
        "uncommitted snapshot should omit the Commit line:\n{source_comment}"
    );
    assert!(
        source_comment.contains("- Snapshot mode: local uncommitted Markdown"),
        "uncommitted snapshot should be labeled uncommitted:\n{source_comment}"
    );
    assert!(
        !source_comment.contains("local committed Markdown"),
        "uncommitted snapshot must not claim committed:\n{source_comment}"
    );
}

/// The first Execution State posted by `record open` defaults to an open fold
/// (`<details open>`) when the execution-state file carries a `## Task Ledger`,
/// so a reader sees the full plan on load while the toggle stays. Later
/// checkpoints keep the `auto` default (collapsed while in-progress).
#[test]
fn record_open_initial_state_task_ledger_defaults_to_open_fold() {
    use nils_test_support::git::{InitRepoOptions, git, init_repo_with};

    let stub = StubBinDir::new();
    stub.write_exe("gh", record_open_dry_run_gh_stub());

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    let bundle = repo.path().join("docs/plans/sample");
    fs::create_dir_all(&bundle).expect("create bundle dir");
    let source = bundle.join("sample-discussion-source.md");
    let plan = bundle.join("sample-plan.md");
    let execution_state = bundle.join("sample-execution-state.md");
    fs::write(&source, "# Source\n\n- Decision: implement v2 lifecycle.\n").expect("write source");
    fs::write(
        &plan,
        "# Plan: Sample Plan\n\n## Overview\n\n- Sample plan body.\n\n## Read First\n\n- Primary source: docs/plans/sample/sample-discussion-source.md\n- Source type: discussion-to-implementation-doc\n- Open questions carried into execution: none\n\n## Scope\n\n- In scope:\n  - Demo plan.\n- Out of scope:\n  - none.\n\n## Assumptions\n\n1. Demo only.\n\n## Sprint 1: Demo\n\n**Goal**: Demo the surface.\n\n**PR grouping intent**: group\n**Execution Profile**: serial\n\n### Task 1.1: Demo task\n\n- **Location**:\n  - `docs/plans/sample/sample-plan.md`\n- **Description**: Demo task description.\n- **Dependencies**:\n  - none\n- **Complexity**: 1\n- **Acceptance criteria**:\n  - The demo task is complete.\n- **Validation**:\n  - `true`\n",
    )
    .expect("write plan");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## Execution State\n\n- Status: pending\n- Target scope: Sample Plan\n\n## Task Ledger\n\n| ID | Status | Task |\n| --- | --- | --- |\n| 1.1 | pending | Demo task |\n",
    )
    .expect("write execution state");
    git(repo.path(), &["add", "."]);
    git(
        repo.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "seed bundle",
            "--no-gpg-sign",
        ],
    );

    let opts = dry_run_cmd_options(stub.path()).with_cwd(repo.path());
    let bundle_arg = bundle.to_string_lossy().to_string();
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            &bundle_arg,
        ],
        &opts,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let state_comment = parsed["payload"]["result"]["preview"]["comments"]["state"]
        .as_str()
        .expect("state comment");
    assert!(
        state_comment.contains("<details open>"),
        "first Execution State should default to an open fold: {state_comment}"
    );
    assert!(
        state_comment.contains("<summary>Show task ledger</summary>"),
        "open fold must keep the toggle summary: {state_comment}"
    );
    assert!(
        state_comment.contains("| 1.1 | pending | Demo task |"),
        "ledger rows must be present inside the open fold: {state_comment}"
    );
}

/// Sprint 4 Task 4.3: exercise the v3 closeout end-to-end against a sanitized
/// agent-runtime-kit fixture. Asserts that one `record close` invocation can
/// audit the issue, verify provider PR merge evidence, render the closeout
/// comment + final dashboard, and that no v1 markers leak into the result.
#[test]
fn agent_runtime_kit_lifecycle_fixture_passes_strict_v2_closeout_end_to_end() {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout").to_path_buf();
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.close");
    assert_eq!(result["mode"], "fixture");
    assert_eq!(result["dry_run"], true);

    let preview = &result["preview"];
    let closeout_body = preview["closeout_comment_body"]
        .as_str()
        .expect("closeout body present");
    // Closeout comment uses the v2 marker and carries provider-verified
    // merge_sha from the fixture PR snapshot in the hidden payload.
    assert!(
        closeout_body.starts_with("<!-- plan-issue-record:v2 role=closeout profile=tracking -->"),
        "{closeout_body}"
    );
    assert!(
        !closeout_body.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
        "{closeout_body}"
    );
    assert!(
        closeout_body.contains("<!-- plan-issue-record-payload:hex:"),
        "{closeout_body}"
    );
    let audit = audit_single_comment_body(closeout_body);
    let closeout = &audit["evidence"]["closeout"]["payload"]["data"];
    assert_eq!(closeout["final_status"], "complete");
    assert_eq!(
        closeout["linked_prs"][0]["merge_sha"], "merge1111111111111111111111111111111111",
        "merge_sha must come from PR fixture, not state payload: {closeout_body}"
    );
    // Sanity: no v1 marker bleed-through.
    assert!(
        !closeout_body.contains("execute-from-tracking-issue:")
            && !closeout_body.contains("plan-tracking-issue:"),
        "v1 markers must not appear in v2 closeout body: {closeout_body}"
    );

    let final_dashboard = preview["final_dashboard"]
        .as_str()
        .expect("final dashboard present");
    assert!(
        final_dashboard.starts_with("## Final Dashboard"),
        "complete state must render Final Dashboard: {final_dashboard}"
    );
    // Durable record links derive from audit, not caller-supplied URLs.
    assert!(
        final_dashboard.contains(
            "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-source"
        ),
        "dashboard must include source URL from audit: {final_dashboard}"
    );
    assert!(
        final_dashboard
            .contains("https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-state"),
        "dashboard must include state URL from audit: {final_dashboard}"
    );
}

/// Issue sympoies/nils-cli#479: `record open --label` exposes labels in the
/// dry-run preview so downstream consumers can audit creation-time labels
/// without hitting the provider.
#[test]
fn record_open_dry_run_includes_labels_in_preview() {
    use nils_test_support::git::{InitRepoOptions, git, init_repo_with};

    let stub = StubBinDir::new();
    stub.write_exe("gh", record_open_dry_run_gh_stub());

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    let bundle = repo.path().join("docs/plans/sample");
    fs::create_dir_all(&bundle).expect("create bundle dir");
    let source = bundle.join("sample-discussion-source.md");
    let plan = bundle.join("sample-plan.md");
    let execution_state = bundle.join("sample-execution-state.md");
    fs::write(&source, "# Source\n\n- Decision: implement v2 lifecycle.\n").expect("write source");
    fs::write(
        &plan,
        "# Plan: Sample Plan\n\n## Overview\n\n- Sample plan body.\n\n## Read First\n\n- Primary source: docs/plans/sample/sample-discussion-source.md\n- Source type: discussion-to-implementation-doc\n- Open questions carried into execution: none\n\n## Scope\n\n- In scope:\n  - Demo plan.\n- Out of scope:\n  - none.\n\n## Assumptions\n\n1. Demo only.\n\n## Sprint 1: Demo\n\n**Goal**: Demo the surface.\n\n**PR grouping intent**: group\n**Execution Profile**: serial\n\n### Task 1.1: Demo task\n\n- **Location**:\n  - `docs/plans/sample/sample-plan.md`\n- **Description**: Demo task description.\n- **Dependencies**:\n  - none\n- **Complexity**: 1\n- **Acceptance criteria**:\n  - The demo task is complete.\n- **Validation**:\n  - `true`\n",
    )
    .expect("write plan");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## Execution State\n\n- Status: pending\n- Target scope: Sample Plan\n",
    )
    .expect("write execution state");
    git(repo.path(), &["add", "."]);
    git(
        repo.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "seed bundle",
            "--no-gpg-sign",
        ],
    );

    let opts = dry_run_cmd_options(stub.path()).with_cwd(repo.path());
    let bundle_arg = bundle.to_string_lossy().to_string();
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            &bundle_arg,
            "--label",
            "workflow::plan",
            "--label",
            " state::needs-triage ",
            "--label",
            "",
        ],
        &opts,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "dry-run");
    let labels = result["preview"]["labels"]
        .as_array()
        .expect("preview.labels array");
    let labels: Vec<&str> = labels.iter().filter_map(Value::as_str).collect();
    assert_eq!(
        labels,
        vec!["workflow::plan", "state::needs-triage"],
        "empty/whitespace labels must be dropped and non-empty values trimmed"
    );
}

#[test]
fn record_attach_dry_run_renders_source_plan_and_state_comments() {
    use nils_test_support::git::{InitRepoOptions, git, init_repo_with};

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    let bundle = repo.path().join("docs/plans/sample");
    fs::create_dir_all(&bundle).expect("create bundle dir");
    let source = bundle.join("sample-discussion-source.md");
    let plan = bundle.join("sample-plan.md");
    let execution_state = bundle.join("sample-execution-state.md");
    fs::write(&source, "# Source\n\n- Decision: attach existing issue.\n").expect("write source");
    fs::write(
        &plan,
        "# Plan: Existing Issue Attach\n\n## Overview\n\n- Attach v2 lifecycle comments.\n\n## Read First\n\n- Primary source: docs/plans/sample/sample-discussion-source.md\n- Source type: discussion-to-implementation-doc\n- Open questions carried into execution: none\n\n## Scope\n\n- In scope:\n  - Demo attach.\n- Out of scope:\n  - none.\n\n## Assumptions\n\n1. Demo only.\n\n## Sprint 1: Demo\n\n**Goal**: Demo the attach surface.\n\n**PR grouping intent**: group\n**Execution Profile**: serial\n\n### Task 1.1: Demo task\n\n- **Location**:\n  - `docs/plans/sample/sample-plan.md`\n- **Description**: Demo task description.\n- **Dependencies**:\n  - none\n- **Complexity**: 1\n- **Acceptance criteria**:\n  - The demo task is complete.\n- **Validation**:\n  - `true`\n",
    )
    .expect("write plan");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## Execution State\n\n- Status: pending\n- Target scope: Existing Issue Attach\n",
    )
    .expect("write execution state");
    git(repo.path(), &["add", "."]);
    git(
        repo.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "seed bundle",
            "--no-gpg-sign",
        ],
    );

    let bundle_arg = bundle.to_string_lossy().to_string();
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "--dry-run",
            "record",
            "attach",
            "--issue",
            "69",
            "--bundle",
            &bundle_arg,
        ],
        &nils_test_support::cmd::CmdOptions::new().with_cwd(repo.path()),
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "dry-run");
    assert_eq!(result["preview"]["issue_number"], 69);
    let comments = &result["preview"]["comments"];
    assert!(comments["source"].as_str().unwrap().contains("role=source"));
    assert!(comments["plan"].as_str().unwrap().contains("role=plan"));
    assert!(comments["state"].as_str().unwrap().contains("role=state"));
}

/// `record post --add-label / --remove-label` exposes the planned label
/// mutation in dry-run output and in fixture mode without touching gh.
#[test]
fn record_post_dry_run_includes_label_mutations() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({"status": "blocked", "tasks": [], "prs": [], "blockers": [], "links": {}})
            .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "--dry-run",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload path"),
        "--add-label",
        "state::blocked",
        "--remove-label",
        "state::in-progress",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "dry-run");
    assert_eq!(result["labels"]["add"][0], "state::blocked");
    assert_eq!(result["labels"]["remove"][0], "state::in-progress");
}

#[test]
fn record_post_live_refuses_when_lifecycle_lock_is_busy() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = TempDir::new().expect("state-dir");
    let stub = StubBinDir::new();
    stub.write_exe("gh", live_record_gh_stub());

    plan_issue::state::set_state_dir_override(Some(state_dir.path().to_path_buf()));
    let _busy_lock = plan_issue::lifecycle_lock::acquire_for_identity(
        "github",
        Some("github.com"),
        "owner/repo",
        448,
        RecordProfile::Tracking,
    )
    .expect("pre-acquire lifecycle lock");
    plan_issue::state::set_state_dir_override(None);

    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({"status": "blocked", "tasks": [], "prs": [], "blockers": [], "links": {}})
            .to_string(),
    )
    .expect("write payload");

    let state_dir_arg = state_dir.path().to_string_lossy().to_string();
    let payload_arg = payload.to_string_lossy().to_string();
    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--state-dir",
            &state_dir_arg,
            "--repo",
            "https://github.com/owner/repo.git",
            "record",
            "post",
            "--issue",
            "448",
            "--kind",
            "state",
            "--payload-file",
            &payload_arg,
        ],
        live_record_options(stub.path(), &[]),
    );

    assert_eq!(
        out.code,
        1,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let parsed = out.stdout_json();
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["error"]["code"], "plan-issue-lifecycle-lock-busy");
    let message = parsed["error"]["message"].as_str().expect("message");
    assert!(message.contains("issue=448"), "{message}");
    assert!(message.contains("profile=tracking"), "{message}");
}

/// `record close --add-label / --remove-label` shows the planned closeout
/// label transition in fixture preview output.
#[test]
fn record_close_fixture_includes_label_mutations() {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout").to_path_buf();
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
        "--add-label",
        "state::closed",
        "--remove-label",
        "state::in-progress",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let labels = &parsed["payload"]["result"]["preview"]["labels"];
    assert_eq!(labels["add"][0], "state::closed");
    assert_eq!(labels["remove"][0], "state::in-progress");
}

/// Same label name in `--add-label` and `--remove-label` is incoherent — the
/// helper rejects it with a usage error so the live `gh issue edit` call is
/// never built.
#[test]
fn record_post_rejects_conflicting_label_mutations() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({"status": "in-progress", "tasks": [], "prs": [], "blockers": [], "links": {}})
            .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "--dry-run",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload path"),
        "--add-label",
        "state::needs-triage",
        "--remove-label",
        "state::needs-triage",
    ]);
    assert_ne!(out.code, 0, "conflicting label mutation should fail");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("record-label-mutation-conflict"),
        "expected record-label-mutation-conflict code, got: {joined}"
    );
}

#[test]
fn record_close_rejects_conflicting_label_mutations() {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout").to_path_buf();
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
        "--add-label",
        "state::closed",
        "--remove-label",
        "state::closed",
    ]);
    assert_ne!(out.code, 0, "conflicting label mutation should fail");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("record-label-mutation-conflict"),
        "expected record-label-mutation-conflict code, got: {joined}"
    );
}

/// Sprint 4 Task 4.3: same fixture, but force the strict gate to fail by
/// flipping the PR snapshot to unmerged. Verifies the gate code surfaces.
#[test]
fn agent_runtime_kit_lifecycle_fixture_blocks_when_pr_unmerged() {
    let src = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    let tmp = TempDir::new().expect("tmp");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(fixture.join("prs")).expect("create fixture dirs");
    fs::copy(src.join("issue-body.md"), fixture.join("issue-body.md")).expect("copy body");
    fs::copy(src.join("comments.json"), fixture.join("comments.json")).expect("copy comments");
    // Replace the PR snapshot with an open PR so the strict gate fails.
    fs::write(
        fixture.join("prs/sympoies__agent-runtime-kit__1.json"),
        serde_json::to_string(&json!({
            "state": "OPEN",
            "mergeCommit": null,
            "statusCheckRollup": {"state": "pending"},
            "url": "https://github.com/sympoies/agent-runtime-kit/pull/1"
        }))
        .expect("pr json"),
    )
    .expect("write open pr fixture");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_ne!(out.code, 0, "unmerged PR should block strict closeout");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("linked-pr-not-merged"),
        "expected linked-pr-not-merged code, got: {joined}"
    );
}

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

use crate::common;

fn json_stdout(out: &common::CmdOut) -> Value {
    serde_json::from_str(&out.stdout).expect("json stdout")
}

#[test]
fn issue_backed_lifecycle_render_comment_emits_compat_tracking_snapshot() {
    let tmp = TempDir::new().expect("tmp");
    let content = tmp.path().join("source.md");
    let rendered = tmp.path().join("comment.md");
    fs::write(
        &content,
        "# Source\n\n<!-- execute-from-tracking-issue:validation:v1 -->\n",
    )
    .expect("write content");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "render-comment",
        "--kind",
        "source",
        "--path",
        "docs/source.md",
        "--commit",
        "abc123",
        "--content-file",
        content.to_str().expect("content path"),
        "--out",
        rendered.to_str().expect("rendered path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let body = fs::read_to_string(&rendered).expect("read rendered");
    assert!(
        body.starts_with("<!-- plan-tracking-issue:snapshot:v1 kind=source -->"),
        "{body}"
    );
    assert!(body.contains("## Source Snapshot"), "{body}");
    assert!(body.contains("<details>"), "{body}");
    assert!(!body.contains("## Task Decomposition"), "{body}");

    let payload = json_stdout(&out);
    assert_eq!(payload["command"], "record.render-comment");
    assert_eq!(payload["payload"]["result"]["kind"], "source");
}

#[test]
fn issue_backed_lifecycle_audit_uses_only_top_level_comment_markers() {
    let tmp = TempDir::new().expect("tmp");
    let body = tmp.path().join("body.md");
    let comments = tmp.path().join("comments.json");
    fs::write(
        &body,
        "## Current Dashboard\n\n## Durable Record\n\nNo task table here.\n",
    )
    .expect("write body");
    fs::write(
        &comments,
        r##"{
  "comments": [
    {
      "url": "https://github.com/owner/repo/issues/1#issuecomment-source",
      "body": "<!-- plan-tracking-issue:snapshot:v1 kind=source -->\n\n## Source Snapshot\n\n<details>\n<summary>Source snapshot</summary>\n\n<!-- execute-from-tracking-issue:validation:v1 -->\n\n</details>\n"
    },
    {
      "url": "https://github.com/owner/repo/issues/1#issuecomment-plan",
      "body": "<!-- plan-tracking-issue:snapshot:v1 kind=plan -->\n\n## Plan Snapshot\n"
    },
    {
      "url": "https://github.com/owner/repo/issues/1#issuecomment-state",
      "body": "<!-- execute-from-tracking-issue:state:v1 -->\n\n## Execution State\n\n- Status: complete\n"
    }
  ]
}"##,
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "audit",
        "--profile",
        "tracking",
        "--body-file",
        body.to_str().expect("body path"),
        "--comments-json",
        comments.to_str().expect("comments path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = json_stdout(&out);
    let audit = &payload["payload"]["result"]["audit"];
    assert_eq!(audit["recognized_count"], 3);
    assert_eq!(
        audit["markers"]["source_snapshot"]["url"],
        "https://github.com/owner/repo/issues/1#issuecomment-source"
    );
    assert!(audit["markers"]["validation"].is_null());
    assert_eq!(audit["missing_required"].as_array().unwrap().len(), 0);
    assert_eq!(audit["body_sections"]["task_decomposition"], false);
}

#[test]
fn issue_backed_lifecycle_closeout_gate_reports_ready_when_required_markers_pass() {
    let tmp = TempDir::new().expect("tmp");
    let body = tmp.path().join("body.md");
    let comments = tmp.path().join("comments.json");
    fs::write(
        &body,
        "## Final Dashboard\n\n## Durable Record\n\n## Closeout Checks\n",
    )
    .expect("write body");
    fs::write(
        &comments,
        r##"[
  {"url":"https://github.com/owner/repo/issues/1#issuecomment-source","body":"<!-- plan-tracking-issue:snapshot:v1 kind=source -->\n\n## Source Snapshot\n"},
  {"url":"https://github.com/owner/repo/issues/1#issuecomment-plan","body":"<!-- plan-tracking-issue:snapshot:v1 kind=plan -->\n\n## Plan Snapshot\n"},
  {"url":"https://github.com/owner/repo/issues/1#issuecomment-state","body":"<!-- execute-from-tracking-issue:state:v1 -->\n\n## Execution State\n\n- Status: complete\n"},
  {"url":"https://github.com/owner/repo/issues/1#issuecomment-session","body":"<!-- execute-from-tracking-issue:session:v1 -->\n\n## Execution Session\n"},
  {"url":"https://github.com/owner/repo/issues/1#issuecomment-validation","body":"<!-- execute-from-tracking-issue:validation:v1 -->\n\n## Validation Evidence\n"}
]"##,
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "closeout-gate",
        "--profile",
        "tracking",
        "--require-complete",
        "--require-session",
        "--require-validation",
        "--approval",
        "explicit close approval flag",
        "--body-file",
        body.to_str().expect("body path"),
        "--comments-json",
        comments.to_str().expect("comments path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = json_stdout(&out);
    let result = &payload["payload"]["result"];
    assert_eq!(result["ready"], true);
    assert!(
        result["checks_markdown"]
            .as_str()
            .unwrap()
            .contains("| close approval | pass | explicit close approval flag |"),
        "{}",
        result["checks_markdown"]
    );
}

#[test]
fn issue_backed_lifecycle_build_dispatch_ledger_uses_plan_tooling_without_task_decomposition() {
    let tmp = TempDir::new().expect("tmp");
    let rendered = tmp.path().join("dispatch-ledger.md");
    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "build-dispatch-ledger",
        "--plan",
        "crates/plan-issue-cli/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md",
        "--pr-grouping",
        "per-sprint",
        "--out",
        rendered.to_str().expect("rendered path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let ledger = fs::read_to_string(&rendered).expect("read ledger");
    assert!(ledger.starts_with("## Dispatch Ledger"), "{ledger}");
    assert!(
        ledger.contains("| Task | Summary | Sprint | Owner/Subagent |"),
        "{ledger}"
    );
    assert!(!ledger.contains("## Task Decomposition"), "{ledger}");

    let payload = json_stdout(&out);
    assert_eq!(payload["command"], "record.build-dispatch-ledger");
    assert!(
        payload["payload"]["result"]["record_count"]
            .as_u64()
            .unwrap()
            > 0
    );
}

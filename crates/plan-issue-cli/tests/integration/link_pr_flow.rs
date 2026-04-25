use std::fs;

use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

use crate::common;
fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("stdout should be JSON")
}

fn issue_body_with_rows(rows: &[&str]) -> String {
    let mut out = vec![
        "# Example Plan".to_string(),
        String::new(),
        "## Task Decomposition".to_string(),
        String::new(),
        "| Task | Summary | Owner | Branch | Worktree | Execution Mode | PR | Status | Notes |"
            .to_string(),
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- |".to_string(),
    ];
    out.extend(rows.iter().map(|row| row.to_string()));
    out.push(String::new());
    out.push("## Evidence".to_string());
    out.push(String::new());
    out.push("- local test fixture".to_string());
    out.push(String::new());
    out.join("\n")
}

fn row_line<'a>(body: &'a str, task_id: &str) -> &'a str {
    body.lines()
        .find(|line| line.trim_start().starts_with(&format!("| {task_id} |")))
        .unwrap_or_else(|| panic!("missing row for {task_id}\n{body}"))
}

#[test]
fn link_pr_body_file_task_target_syncs_all_rows_in_per_sprint_lane() {
    let tmp = TempDir::new().expect("temp dir");
    let body_path = tmp.path().join("issue-body.md");
    fs::write(
        &body_path,
        issue_body_with_rows(&[
            "| S4T1 | Core A | subagent-s4 | issue/s4 | issue-s4 | per-sprint | TBD | planned | sprint=S4 |",
            "| S4T2 | Core B | subagent-s4 | issue/s4 | issue-s4 | per-sprint | TBD | planned | sprint=S4 |",
            "| S5T1 | Next | subagent-s5 | issue/s5 | issue-s5 | per-sprint | TBD | planned | sprint=S5 |",
        ]),
    )
    .expect("write body");
    let body_path_s = body_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "link-pr",
        "--body-file",
        &body_path_s,
        "--task",
        "S4T1",
        "--pr",
        "https://github.com/sympoies/nils-cli/pull/221",
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    let result = &payload["payload"]["result"];
    assert_eq!(payload["command"], "link-pr");
    assert_eq!(payload["status"], "ok");
    assert_eq!(result["lane_sync_applied"], true);
    assert_eq!(result["rows_changed"], 2);
    assert_eq!(result["pr"], "#221");
    assert_eq!(result["status"], "in-progress");
    assert_eq!(result["body_file_updated"], true);

    let updated = fs::read_to_string(&body_path).expect("read updated body");
    assert!(row_line(&updated, "S4T1").contains("| #221 | in-progress |"));
    assert!(row_line(&updated, "S4T2").contains("| #221 | in-progress |"));
    assert!(row_line(&updated, "S5T1").contains("| TBD | planned |"));
}

#[test]
fn link_pr_body_file_sprint_pr_group_updates_only_selected_shared_lane() {
    let tmp = TempDir::new().expect("temp dir");
    let body_path = tmp.path().join("issue-body.md");
    fs::write(
        &body_path,
        issue_body_with_rows(&[
            "| S4T1 | Core A | subagent-s4-core | issue/s4-core | issue-s4-core | pr-shared | TBD | planned | sprint=S4; pr-group=core |",
            "| S4T2 | Core B | subagent-s4-core | issue/s4-core | issue-s4-core | pr-shared | TBD | planned | sprint=S4; pr-group=core |",
            "| S4T3 | UI A | subagent-s4-ui | issue/s4-ui | issue-s4-ui | pr-shared | TBD | planned | sprint=S4; pr-group=ui |",
        ]),
    )
    .expect("write body");
    let body_path_s = body_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "link-pr",
        "--body-file",
        &body_path_s,
        "--sprint",
        "4",
        "--pr-group",
        "core",
        "--pr",
        "333",
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    let result = &payload["payload"]["result"];
    assert_eq!(result["target"], "sprint:S4/pr-group:core");
    assert_eq!(result["rows_changed"], 2);
    assert_eq!(result["pr"], "#333");
    assert_eq!(result["lane"], "S4/core");

    let updated = fs::read_to_string(&body_path).expect("read updated body");
    assert!(row_line(&updated, "S4T1").contains("| #333 | in-progress |"));
    assert!(row_line(&updated, "S4T2").contains("| #333 | in-progress |"));
    assert!(row_line(&updated, "S4T3").contains("| TBD | planned |"));
}

#[test]
fn link_pr_body_file_sprint_target_rejects_ambiguous_multi_lane_scope() {
    let tmp = TempDir::new().expect("temp dir");
    let body_path = tmp.path().join("issue-body.md");
    fs::write(
        &body_path,
        issue_body_with_rows(&[
            "| S4T1 | Core A | subagent-s4-core | issue/s4-core | issue-s4-core | pr-shared | TBD | planned | sprint=S4; pr-group=core |",
            "| S4T2 | UI A | subagent-s4-ui | issue/s4-ui | issue-s4-ui | pr-shared | TBD | planned | sprint=S4; pr-group=ui |",
        ]),
    )
    .expect("write body");
    let body_path_s = body_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "link-pr",
        "--body-file",
        &body_path_s,
        "--sprint",
        "4",
        "--pr",
        "#444",
    ]);

    assert_eq!(out.code, 1, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    assert_eq!(payload["command"], "link-pr");
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error"]["code"], "link-pr-target-invalid");
    assert!(
        payload["error"]["message"]
            .as_str()
            .is_some_and(|msg| msg.contains("ambiguous") && msg.contains("--pr-group")),
        "{}",
        out.stdout
    );
}

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use nils_test_support::StubBinDir;
use nils_test_support::cmd::CmdOptions;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::common;
#[derive(Debug, Clone)]
struct IssueTaskRow {
    owner: String,
    branch: String,
    worktree: String,
    execution_mode: String,
    notes: String,
}

#[derive(Debug, Clone)]
struct SpecRow {
    owner: String,
    branch: String,
    worktree: String,
    notes: String,
}

fn result_path(payload: &Value, key: &str) -> String {
    payload["payload"]["result"][key]
        .as_str()
        .unwrap_or_else(|| panic!("missing result path key: {key}"))
        .to_string()
}

fn parse_markdown_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
        return Vec::new();
    }

    trimmed[1..trimmed.len() - 1]
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

fn parse_task_decomposition_rows(issue_body: &str) -> HashMap<String, IssueTaskRow> {
    let lines = issue_body.lines().collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| line.trim() == "## Task Decomposition")
        .map(|idx| idx + 1)
        .expect("task decomposition heading");

    let table_lines = lines
        .iter()
        .skip(start)
        .take_while(|line| !line.starts_with("## "))
        .copied()
        .filter(|line| line.trim().starts_with('|'))
        .collect::<Vec<_>>();
    assert!(table_lines.len() >= 3, "missing task table\n{issue_body}");

    let headers = parse_markdown_row(table_lines[0]);
    let task_idx = headers.iter().position(|h| h == "Task").expect("Task col");
    let owner_idx = headers
        .iter()
        .position(|h| h == "Owner")
        .expect("Owner col");
    let branch_idx = headers
        .iter()
        .position(|h| h == "Branch")
        .expect("Branch col");
    let worktree_idx = headers
        .iter()
        .position(|h| h == "Worktree")
        .expect("Worktree col");
    let mode_idx = headers
        .iter()
        .position(|h| h == "Execution Mode")
        .expect("Execution Mode col");
    let notes_idx = headers
        .iter()
        .position(|h| h == "Notes")
        .expect("Notes col");

    let mut rows = HashMap::new();
    for line in table_lines.iter().skip(2) {
        let cells = parse_markdown_row(line);
        if cells.len() != headers.len() {
            continue;
        }
        let task = cells[task_idx].trim();
        if task.is_empty() {
            continue;
        }

        rows.insert(
            task.to_string(),
            IssueTaskRow {
                owner: cells[owner_idx].clone(),
                branch: cells[branch_idx].clone(),
                worktree: cells[worktree_idx].clone(),
                execution_mode: cells[mode_idx].clone(),
                notes: cells[notes_idx].clone(),
            },
        );
    }

    rows
}

fn parse_task_spec_rows(tsv: &str) -> HashMap<String, SpecRow> {
    let mut rows = HashMap::new();

    for line in tsv.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let cols = line.split('\t').collect::<Vec<_>>();
        assert_eq!(cols.len(), 7, "unexpected task-spec row: {line}");
        rows.insert(
            cols[0].to_string(),
            SpecRow {
                branch: cols[2].to_string(),
                worktree: cols[3].to_string(),
                owner: cols[4].to_string(),
                notes: cols[5].to_string(),
            },
        );
    }

    rows
}

fn forge_cmd_options(stub_dir: &Path, envs: &[(&str, &str)]) -> CmdOptions {
    common::plan_issue_cmd_options()
        .with_env_remove_prefix("FORGE_CLI_STUB_")
        .with_path_prepend(stub_dir)
        .with_envs(envs)
}

#[test]
fn auto_single_lane_end_to_end_keeps_per_sprint_runtime_truth() {
    let tmp = TempDir::new().expect("temp dir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    let state_dir = tmp.path().join("state-dir");
    fs::create_dir_all(&state_dir).expect("create agent home");
    let state_dir_s = state_dir.to_string_lossy().to_string();

    let plan_file = tmp.path().join("sprint1-auto-single-lane.md");
    let plan_file_s = plan_file.to_string_lossy().to_string();
    fs::write(
        &plan_file,
        r#"# Plan: Sprint 1 runtime-truth end-to-end

## Sprint 1: Shared lane
- **PR grouping intent**: `group`.
- **Execution Profile**: `serial` (parallel width 1).

### Task 1.1: First lane task
- **Location**:
  - crates/plan-issue/src/a.rs
- **Dependencies**:
  - none

### Task 1.2: Follow-up lane task
- **Location**:
  - crates/plan-issue/src/b.rs
- **Dependencies**:
  - Task 1.1
"#,
    )
    .expect("write plan");

    let plan_task_spec = tmp.path().join("plan-task-spec.tsv");
    let plan_issue_body = tmp.path().join("issue-body.md");
    let plan_task_spec_s = plan_task_spec.to_string_lossy().to_string();
    let plan_issue_body_s = plan_issue_body.to_string_lossy().to_string();

    let start_plan_out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-plan",
            "--plan",
            &plan_file_s,
            "--strategy",
            "auto",
            "--task-spec-out",
            &plan_task_spec_s,
            "--issue-body-out",
            &plan_issue_body_s,
        ],
        &[("PLAN_ISSUE_HOME", &state_dir_s)],
    );
    assert_eq!(
        start_plan_out.code,
        0,
        "stderr: {}",
        start_plan_out.stderr_text()
    );

    let issue_body = fs::read_to_string(&plan_issue_body).expect("read issue body");
    let issue_rows = parse_task_decomposition_rows(&issue_body);
    let issue_s1t1 = issue_rows.get("S1T1").expect("S1T1 issue row");
    let issue_s1t2 = issue_rows.get("S1T2").expect("S1T2 issue row");
    assert_eq!(issue_s1t1.execution_mode, "per-sprint");
    assert_eq!(issue_s1t2.execution_mode, "per-sprint");
    assert_eq!(issue_s1t1.owner, issue_s1t2.owner);
    assert_eq!(issue_s1t1.branch, issue_s1t2.branch);
    assert_eq!(issue_s1t1.worktree, issue_s1t2.worktree);
    assert_ne!(issue_s1t1.notes, issue_s1t2.notes);
    assert!(issue_s1t1.notes.contains("pr-group=s1-auto-g1"));
    assert!(issue_s1t2.notes.contains("pr-group=s1-auto-g1"));
    assert!(issue_s1t1.notes.contains("shared-pr-anchor=S1T1"));
    assert!(issue_s1t2.notes.contains("shared-pr-anchor=S1T1"));

    let body_json = json!(issue_body).to_string();
    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let sprint_task_spec = tmp.path().join("sprint1-task-spec.tsv");
    let sprint_task_spec_s = sprint_task_spec.to_string_lossy().to_string();
    let prompts_out = tmp.path().join("sprint1-prompts");
    let prompts_out_s = prompts_out.to_string_lossy().to_string();

    let start_sprint_out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            "sympoies/nils-cli",
            "start-sprint",
            "--plan",
            &plan_file_s,
            "--issue",
            "217",
            "--sprint",
            "1",
            "--task-spec-out",
            &sprint_task_spec_s,
            "--subagent-prompts-out",
            &prompts_out_s,
            "--strategy",
            "auto",
            "--no-comment",
        ],
        forge_cmd_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );
    assert_eq!(
        start_sprint_out.code,
        0,
        "stdout:\n{}\nstderr:\n{}",
        start_sprint_out.stdout_text(),
        start_sprint_out.stderr_text()
    );

    let sprint_payload = start_sprint_out.stdout_json();
    let start_comment_path = result_path(&sprint_payload, "comment_path");
    let start_comment = fs::read_to_string(&start_comment_path).expect("read start comment");
    assert!(
        start_comment.contains("| S1T1 | First lane task | per-sprint |"),
        "{start_comment}"
    );
    assert!(
        start_comment.contains("| S1T2 | Follow-up lane task | per-sprint |"),
        "{start_comment}"
    );
    assert!(!start_comment.contains("pr-shared"), "{start_comment}");

    let spec_text = fs::read_to_string(&sprint_task_spec).expect("read sprint task-spec");
    let spec_rows = parse_task_spec_rows(&spec_text);
    for task in ["S1T1", "S1T2"] {
        let issue_row = issue_rows.get(task).expect("issue row");
        let spec_row = spec_rows.get(task).expect("spec row");
        assert_eq!(spec_row.owner, issue_row.owner);
        assert_eq!(spec_row.branch, issue_row.branch);
        assert_eq!(spec_row.worktree, issue_row.worktree);
        assert_eq!(spec_row.notes, issue_row.notes);
    }

    let prompt_files = sprint_payload["payload"]["result"]["subagent_prompt_files"]
        .as_array()
        .expect("prompt files");
    assert_eq!(prompt_files.len(), 2, "{}", start_sprint_out.stdout_text());

    let prompt_path = prompt_files[0].as_str().expect("prompt path");
    assert!(prompt_path.ends_with("/S1T1.md"), "{prompt_path}");
    let prompt = fs::read_to_string(prompt_path).expect("read prompt");
    assert!(prompt.contains("Tasks: S1T1, S1T2"), "{prompt}");
    assert!(prompt.contains("Execution Mode: per-sprint"), "{prompt}");

    let ready_out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            "sympoies/nils-cli",
            "ready-sprint",
            "--plan",
            &plan_file_s,
            "--issue",
            "217",
            "--sprint",
            "1",
            "--summary",
            "runtime truth ready",
            "--strategy",
            "auto",
        ],
        forge_cmd_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );
    assert_eq!(
        ready_out.code,
        0,
        "stdout:\n{}\nstderr:\n{}",
        ready_out.stdout_text(),
        ready_out.stderr_text()
    );

    let ready_payload = ready_out.stdout_json();
    let ready_comment_path = result_path(&ready_payload, "comment_path");
    let ready_comment = fs::read_to_string(&ready_comment_path).expect("read ready comment");
    assert!(
        ready_comment.contains("| S1T1 | First lane task | TBD (per-sprint) |"),
        "{ready_comment}"
    );
    assert!(
        ready_comment.contains("| S1T2 | Follow-up lane task | TBD (per-sprint) |"),
        "{ready_comment}"
    );
    assert!(
        ready_comment.contains("runtime truth ready"),
        "{ready_comment}"
    );
    assert!(!ready_comment.contains("pr-shared"), "{ready_comment}");

    let log = fs::read_to_string(&log_path).expect("read forge-cli log");
    assert!(
        log.contains("--repo sympoies/nils-cli issue view 217"),
        "{log}"
    );
}

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

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("stdout should be valid JSON")
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

fn gh_stub_script() -> &'static str {
    r#"#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${PLAN_ISSUE_GH_LOG:-}" ]]; then
  printf '%s\n' "$*" >> "$PLAN_ISSUE_GH_LOG"
fi

capture_body_file() {
  local body_file=""
  local prev=""
  for arg in "$@"; do
    if [[ "$prev" == "--body-file" ]]; then
      body_file="$arg"
      break
    fi
    prev="$arg"
  done

  if [[ -n "${PLAN_ISSUE_GH_CAPTURE_BODY_FILE:-}" && -n "$body_file" ]]; then
    cp "$body_file" "$PLAN_ISSUE_GH_CAPTURE_BODY_FILE"
  fi
}

case "${1:-} ${2:-}" in
  "issue view")
    body_json="${PLAN_ISSUE_GH_BODY_JSON:-}"
    if [[ -z "$body_json" ]]; then
      body_json='{"body":""}'
    fi
    printf '%s\n' "$body_json"
    ;;
  "issue edit")
    capture_body_file "$@"
    ;;
  "issue comment")
    ;;
  *)
    printf 'unsupported gh call: %s\n' "$*" >&2
    exit 1
    ;;
esac
"#
}

fn gh_cmd_options(stub_dir: &Path, envs: &[(&str, &str)]) -> CmdOptions {
    common::plan_issue_cmd_options()
        .with_env_remove_prefix("PLAN_ISSUE_GH_")
        .with_path_prepend(stub_dir)
        .with_envs(envs)
}

#[test]
fn live_start_sprint_uses_issue_table_runtime_truth_without_rewrite() {
    let tmp = TempDir::new().expect("temp dir");
    let stub = StubBinDir::new();
    stub.write_exe("gh", gh_stub_script());

    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let log_path = tmp.path().join("gh.log");
    let log_s = log_path.to_string_lossy().to_string();
    let capture_body = tmp.path().join("captured-start-sprint-body.md");
    let capture_body_s = capture_body.to_string_lossy().to_string();

    let plan_file = tmp.path().join("sprint1-runtime-truth.md");
    let plan_file_s = plan_file.to_string_lossy().to_string();
    fs::write(
        &plan_file,
        r#"# Plan: Sprint 1 runtime truth

## Sprint 1: Shared lane
- **PR grouping intent**: `group`.
- **Execution Profile**: `serial` (parallel width 1).

### Task 1.1: First lane task
- **Location**:
  - crates/plan-issue-cli/src/a.rs
- **Dependencies**:
  - none

### Task 1.2: Follow-up lane task
- **Location**:
  - crates/plan-issue-cli/src/b.rs
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
        &[("AGENT_HOME", &agent_home_s)],
    );
    assert_eq!(start_plan_out.code, 0, "stderr: {}", start_plan_out.stderr);

    let issue_body = fs::read_to_string(&plan_issue_body).expect("read issue body");
    let body_json = json!({ "body": issue_body.clone() }).to_string();

    let task_spec_out = tmp.path().join("sprint1-task-spec.tsv");
    let task_spec_out_s = task_spec_out.to_string_lossy().to_string();
    let prompts_out = tmp.path().join("sprint1-prompts");
    let prompts_out_s = prompts_out.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
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
            &task_spec_out_s,
            "--subagent-prompts-out",
            &prompts_out_s,
            "--strategy",
            "auto",
            "--no-comment",
        ],
        gh_cmd_options(
            stub.path(),
            &[
                ("PLAN_ISSUE_GH_LOG", &log_s),
                ("PLAN_ISSUE_GH_BODY_JSON", &body_json),
                ("PLAN_ISSUE_GH_CAPTURE_BODY_FILE", &capture_body_s),
                ("AGENT_HOME", &agent_home_s),
            ],
        ),
    );

    assert_eq!(
        out.code, 0,
        "stdout:\n{}\nstderr:\n{}",
        out.stdout, out.stderr
    );
    let payload = parse_json(&out.stdout);
    assert_eq!(payload["command"], "start-sprint");
    assert_eq!(payload["payload"]["result"]["synced_issue_rows"], 2);
    assert_eq!(
        payload["payload"]["result"]["live_mutations_performed"],
        false
    );

    assert!(
        !capture_body.exists(),
        "start-sprint should not rewrite issue body in runtime-truth mode"
    );

    let issue_rows = parse_task_decomposition_rows(&issue_body);
    let issue_s1t1 = issue_rows.get("S1T1").expect("S1T1 issue row");
    let issue_s1t2 = issue_rows.get("S1T2").expect("S1T2 issue row");
    assert_eq!(issue_s1t1.execution_mode, "per-sprint");
    assert_eq!(issue_s1t2.execution_mode, "per-sprint");

    let spec_path = result_path(&payload, "task_spec_path");
    let spec_text = fs::read_to_string(&spec_path).expect("read task-spec");
    let spec_rows = parse_task_spec_rows(&spec_text);

    for (task_id, issue_row) in [("S1T1", issue_s1t1), ("S1T2", issue_s1t2)] {
        let spec_row = spec_rows
            .get(task_id)
            .unwrap_or_else(|| panic!("missing spec row {task_id}"));
        assert_eq!(issue_row.owner, spec_row.owner);
        assert_eq!(issue_row.branch, spec_row.branch);
        assert_eq!(issue_row.worktree, spec_row.worktree);
        assert_eq!(issue_row.notes, spec_row.notes);
    }

    let prompt_files = payload["payload"]["result"]["subagent_prompt_files"]
        .as_array()
        .expect("subagent prompt files");
    assert_eq!(prompt_files.len(), 2, "{}", out.stdout);
    let prompt_path = prompt_files[0].as_str().expect("prompt path");
    assert!(prompt_path.ends_with("/S1T1.md"), "{prompt_path}");
    let prompt = fs::read_to_string(prompt_path).expect("read prompt");
    assert!(prompt.contains("Tasks: S1T1, S1T2"), "{prompt}");
    assert!(prompt.contains("Execution Mode: per-sprint"), "{prompt}");

    let log = fs::read_to_string(&log_path).expect("read gh log");
    assert!(
        log.contains("issue view 217 --repo sympoies/nils-cli --json body"),
        "{log}"
    );
    assert!(
        !log.contains("issue edit 217 --repo sympoies/nils-cli --body-file"),
        "{log}"
    );
    assert!(
        !log.contains("issue comment 217 --repo sympoies/nils-cli --body-file"),
        "{log}"
    );
}

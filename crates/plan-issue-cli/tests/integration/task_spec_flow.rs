use std::collections::HashMap;
use std::fs;

use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

use crate::common;
const PLAN_PATH: &str =
    "crates/plan-issue-cli/tests/fixtures/plans/plan-issue-rust-cli-full-delivery-plan.md";

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("stdout should be valid JSON")
}

fn result_path(payload: &Value, key: &str) -> String {
    payload["payload"]["result"][key]
        .as_str()
        .unwrap_or_else(|| panic!("missing result path key: {key}"))
        .to_string()
}

fn render_issue_body_for_local_plan(tmp: &TempDir, agent_home: &str) -> String {
    let task_spec = tmp.path().join("plan.tsv");
    let issue_body = tmp.path().join("issue-body.md");
    let task_spec_s = task_spec.to_string_lossy().to_string();
    let issue_body_s = issue_body.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-plan",
            "--plan",
            PLAN_PATH,
            "--pr-grouping",
            "per-sprint",
            "--task-spec-out",
            &task_spec_s,
            "--issue-body-out",
            &issue_body_s,
        ],
        &[("AGENT_HOME", agent_home)],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    issue_body_s
}

#[test]
fn task_spec_generation_build_task_spec_writes_grouped_rows() {
    let tmp = TempDir::new().expect("temp dir");
    let out_path = tmp.path().join("sprint3.tsv");
    let out_path_s = out_path.to_string_lossy().to_string();

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "build-task-spec",
        "--plan",
        PLAN_PATH,
        "--sprint",
        "3",
        "--pr-grouping",
        "group",
        "--pr-group",
        "S3T1=s3-a",
        "--pr-group",
        "S3T2=s3-b",
        "--pr-group",
        "S3T3=s3-c",
        "--task-spec-out",
        &out_path_s,
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    assert_eq!(payload["command"], "build-task-spec");
    assert_eq!(payload["status"], "ok");

    let rendered = fs::read_to_string(&out_path).expect("read task-spec");
    let mut lines = rendered.lines();
    assert_eq!(
        lines.next().unwrap_or_default(),
        "# task_id\tsummary\tbranch\tworktree\towner\tnotes\tpr_group"
    );

    let task_rows: Vec<&str> = rendered
        .lines()
        .filter(|line| line.starts_with("S3T"))
        .collect();
    assert_eq!(task_rows.len(), 3, "{rendered}");
    assert!(rendered.contains("\ts3-a"), "{rendered}");
    assert!(rendered.contains("\ts3-b"), "{rendered}");
    assert!(rendered.contains("\ts3-c"), "{rendered}");
}

#[test]
fn task_spec_generation_creates_missing_output_directory() {
    let tmp = TempDir::new().expect("temp dir");
    let out_path = tmp.path().join("nested").join("deep").join("sprint3.tsv");
    let out_path_s = out_path.to_string_lossy().to_string();
    assert!(
        !out_path.parent().expect("parent").exists(),
        "precondition: parent should not exist"
    );

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "build-task-spec",
        "--plan",
        PLAN_PATH,
        "--sprint",
        "3",
        "--pr-grouping",
        "per-sprint",
        "--task-spec-out",
        &out_path_s,
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let rendered = fs::read_to_string(&out_path).expect("read task-spec");
    assert!(
        rendered.starts_with("# task_id\tsummary\tbranch\tworktree\towner\tnotes\tpr_group\n"),
        "{rendered}"
    );
}

#[test]
fn strategy_auto_partial_mapping_allows_unmapped_rows() {
    let tmp = TempDir::new().expect("temp dir");
    let out_path = tmp.path().join("sprint3-auto.tsv");
    let out_path_s = out_path.to_string_lossy().to_string();

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "build-task-spec",
        "--plan",
        PLAN_PATH,
        "--sprint",
        "3",
        "--strategy",
        "auto",
        "--default-pr-grouping",
        "group",
        "--pr-group",
        "S3T3=manual-docs",
        "--task-spec-out",
        &out_path_s,
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    assert_eq!(payload["command"], "build-task-spec");
    assert_eq!(payload["status"], "ok");

    let rendered = fs::read_to_string(&out_path).expect("read task-spec");
    let mut groups_by_task: HashMap<String, String> = HashMap::new();
    let mut notes_by_task: HashMap<String, String> = HashMap::new();
    for row in rendered.lines().filter(|line| line.starts_with("S3T")) {
        let cols: Vec<&str> = row.split('\t').collect();
        assert_eq!(cols.len(), 7, "unexpected row: {row}");
        groups_by_task.insert(cols[0].to_string(), cols[6].to_string());
        notes_by_task.insert(cols[0].to_string(), cols[5].to_string());
    }
    assert_eq!(groups_by_task.len(), 3, "{rendered}");

    let pinned = groups_by_task.get("S3T3").expect("S3T3 group");
    assert_eq!(pinned, "manual-docs");
    assert!(
        notes_by_task
            .get("S3T3")
            .expect("S3T3 notes")
            .contains("pr-group=manual-docs")
    );

    for task_id in ["S3T1", "S3T2"] {
        let group = groups_by_task.get(task_id).expect("auto-assigned group");
        assert!(group.starts_with("s3-auto-g"), "{group}");
        assert_ne!(group, pinned);
        assert!(
            notes_by_task
                .get(task_id)
                .expect("notes by task")
                .contains(&format!("pr-group={group}"))
        );
    }
}

#[test]
fn render_issue_body_start_plan_writes_issue_body_artifact() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);

    let task_spec = tmp.path().join("plan.tsv");
    let issue_body = tmp.path().join("issue-body.md");
    let task_spec_s = task_spec.to_string_lossy().to_string();
    let issue_body_s = issue_body.to_string_lossy().to_string();
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-plan",
            "--plan",
            PLAN_PATH,
            "--pr-grouping",
            "per-sprint",
            "--task-spec-out",
            &task_spec_s,
            "--issue-body-out",
            &issue_body_s,
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let rendered = fs::read_to_string(&issue_body).expect("read issue body");
    assert!(
        rendered.starts_with("# Plan: Rust Plan-Issue CLI Full Delivery\n\n## Overview\n"),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "This plan delivers a shell-free Rust implementation for the current plan-issue orchestration workflow"
        ),
        "{rendered}"
    );
    assert!(rendered.contains("## Scope"), "{rendered}");
    assert!(rendered.contains("## Assumptions"), "{rendered}");
    assert!(rendered.contains("## Success criteria"), "{rendered}");
    assert!(rendered.contains("## Sprint gate policy"), "{rendered}");
    assert!(
        rendered.contains("## Validation command conventions"),
        "{rendered}"
    );
    assert!(!rendered.contains("## Goal"), "{rendered}");
    assert!(rendered.contains("## Task Decomposition"), "{rendered}");
    assert!(
        rendered.contains("| S3T1 | Implement task-spec generation core using `plan-tooling` |")
    );
    assert!(
        !rendered.contains("| TBD | TBD | TBD | TBD | TBD | planned |"),
        "{rendered}"
    );

    for line in rendered.lines().filter(|line| line.starts_with("| S3T")) {
        assert!(line.contains("sprint=S3"), "{line}");
        assert!(line.contains("pr-group="), "{line}");
    }
}

#[test]
fn render_issue_body_start_plan_creates_missing_issue_body_directory() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);

    let task_spec = tmp.path().join("nested").join("spec").join("plan.tsv");
    let issue_body = tmp
        .path()
        .join("nested")
        .join("issue")
        .join("issue-body.md");
    let task_spec_s = task_spec.to_string_lossy().to_string();
    let issue_body_s = issue_body.to_string_lossy().to_string();
    let agent_home_s = agent_home.to_string_lossy().to_string();
    assert!(
        !issue_body.parent().expect("parent").exists(),
        "precondition: parent should not exist"
    );

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-plan",
            "--plan",
            PLAN_PATH,
            "--pr-grouping",
            "per-sprint",
            "--task-spec-out",
            &task_spec_s,
            "--issue-body-out",
            &issue_body_s,
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let rendered = fs::read_to_string(&issue_body).expect("read issue body");
    assert!(rendered.contains("## Task Decomposition"), "{rendered}");
}

#[test]
fn local_start_plan_returns_deterministic_issue_placeholder() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-plan",
            "--plan",
            PLAN_PATH,
            "--pr-grouping",
            "per-sprint",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    assert_eq!(payload["command"], "start-plan");
    assert_eq!(payload["payload"]["binary"], "plan-issue-local");
    assert_eq!(payload["payload"]["result"]["issue_number"], 999);
}

#[test]
fn render_issue_body_start_plan_falls_back_when_preface_sections_missing() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let plan = tmp.path().join("minimal-plan.md");
    fs::write(
        &plan,
        r#"# Plan: Minimal fallback

## Sprint 1: Minimal
### Task 1.1: Keep defaults
- **Location**:
  - crates/plan-issue-cli/src/render.rs
- **Description**: Keep fallback issue body text deterministic.
- **Dependencies**:
  - none
- **Complexity**: 1
- **Acceptance criteria**:
  - Fallback text is present.
- **Validation**:
  - cargo test -p nils-plan-issue-cli render_issue_body_start_plan_falls_back_when_preface_sections_missing -- --exact
"#,
    )
    .expect("write fallback plan");

    let task_spec = tmp.path().join("plan.tsv");
    let issue_body = tmp.path().join("issue-body.md");
    let plan_s = plan.to_string_lossy().to_string();
    let task_spec_s = task_spec.to_string_lossy().to_string();
    let issue_body_s = issue_body.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-plan",
            "--plan",
            &plan_s,
            "--pr-grouping",
            "per-sprint",
            "--task-spec-out",
            &task_spec_s,
            "--issue-body-out",
            &issue_body_s,
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let rendered = fs::read_to_string(&issue_body).expect("read issue body");
    assert!(
        rendered.starts_with("# Plan: Minimal fallback"),
        "{rendered}"
    );
    assert!(!rendered.contains("## Goal"), "{rendered}");
    assert!(rendered.contains("## Task Decomposition"), "{rendered}");
}

#[test]
fn task_decomposition_writer_and_parser_use_one_sanitized_schema() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let plan = tmp.path().join("pipe-summary-plan.md");
    fs::write(
        &plan,
        r#"# Plan: Pipe summary

## Sprint 1: Minimal
### Task 1.1: Keep parser | writer aligned
- **Location**:
  - crates/plan-issue-cli/src/render.rs
- **Description**: Ensure pipe in summary does not break task table parsing.
- **Dependencies**:
  - none
- **Complexity**: 1
- **Acceptance criteria**:
  - start-plan output can be parsed by status-plan parser.
- **Validation**:
  - cargo test -p nils-plan-issue-cli task_decomposition_writer_and_parser_use_one_sanitized_schema -- --exact
"#,
    )
    .expect("write plan");

    let task_spec = tmp.path().join("plan.tsv");
    let issue_body = tmp.path().join("issue-body.md");
    let plan_s = plan.to_string_lossy().to_string();
    let task_spec_s = task_spec.to_string_lossy().to_string();
    let issue_body_s = issue_body.to_string_lossy().to_string();

    let start_out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-plan",
            "--plan",
            &plan_s,
            "--pr-grouping",
            "per-sprint",
            "--task-spec-out",
            &task_spec_s,
            "--issue-body-out",
            &issue_body_s,
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );
    assert_eq!(start_out.code, 0, "stderr: {}", start_out.stderr);

    let rendered = fs::read_to_string(&issue_body).expect("read issue body");
    assert!(
        rendered.contains("Keep parser / writer aligned"),
        "{rendered}"
    );

    let status_out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "--dry-run",
        "status-plan",
        "--body-file",
        &issue_body_s,
    ]);
    assert_eq!(status_out.code, 0, "stderr: {}", status_out.stderr);
    let payload = parse_json(&status_out.stdout);
    assert_eq!(payload["command"], "status-plan");
    assert_eq!(payload["payload"]["result"]["task_count"], 1);
}

#[test]
fn render_issue_body_start_sprint_writes_start_comment_with_modes() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let task_spec = tmp.path().join("s3.tsv");
    let task_spec_s = task_spec.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-sprint",
            "--plan",
            PLAN_PATH,
            "--issue",
            "217",
            "--sprint",
            "3",
            "--task-spec-out",
            &task_spec_s,
            "--pr-grouping",
            "group",
            "--pr-group",
            "S3T1=s3-a",
            "--pr-group",
            "S3T2=s3-b",
            "--pr-group",
            "S3T3=s3-c",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    let comment_path = result_path(&payload, "comment_path");
    let comment = fs::read_to_string(&comment_path).expect("read sprint comment");
    assert!(comment.contains("## Sprint 3 Start"), "{comment}");
    assert!(
        comment.contains("| Task | Summary | Execution Mode |"),
        "{comment}"
    );
    assert!(comment.contains("pr-isolated"), "{comment}");
}

#[test]
fn render_issue_body_start_sprint_group_auto_single_pr_lane_uses_per_sprint_mode() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let plan_file = tmp.path().join("auto-single-lane-plan.md");
    let plan_file_s = plan_file.to_string_lossy().to_string();
    fs::write(
        &plan_file,
        r#"# Plan: auto single lane mode test

## Sprint 1: Serial lane
- **PR grouping intent**: `group`.
- **Execution Profile**: `serial` (parallel width 1).

### Task 1.1: First lane task
- **Location**:
  - crates/plan-issue-cli/src/a.rs
- **Dependencies**:
  - none

### Task 1.2: Follow-up task
- **Location**:
  - crates/plan-issue-cli/src/b.rs
- **Dependencies**:
  - Task 1.1
"#,
    )
    .expect("write plan");

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-sprint",
            "--plan",
            &plan_file_s,
            "--issue",
            "217",
            "--sprint",
            "1",
            "--strategy",
            "auto",
            "--default-pr-grouping",
            "group",
            "--no-comment",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    let comment_path = result_path(&payload, "comment_path");
    let comment = fs::read_to_string(&comment_path).expect("read sprint comment");
    assert!(
        comment.contains("| Task | Summary | Execution Mode |"),
        "{comment}"
    );
    assert!(
        comment.contains("| S1T1 | First lane task | per-sprint |"),
        "{comment}"
    );
    assert!(
        comment.contains("| S1T2 | Follow-up task | per-sprint |"),
        "{comment}"
    );
    assert!(!comment.contains("pr-shared"), "{comment}");
}

#[test]
fn render_issue_body_start_sprint_group_deterministic_single_pr_lane_uses_per_sprint_mode() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let plan_file = tmp.path().join("deterministic-single-lane-plan.md");
    let plan_file_s = plan_file.to_string_lossy().to_string();
    fs::write(
        &plan_file,
        r#"# Plan: deterministic single lane mode test

## Sprint 1: Serial lane
- **PR grouping intent**: `group`.
- **Execution Profile**: `serial` (parallel width 1).

### Task 1.1: First lane task
- **Location**:
  - crates/plan-issue-cli/src/a.rs
- **Dependencies**:
  - none

### Task 1.2: Follow-up task
- **Location**:
  - crates/plan-issue-cli/src/b.rs
- **Dependencies**:
  - Task 1.1
"#,
    )
    .expect("write plan");

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-sprint",
            "--plan",
            &plan_file_s,
            "--issue",
            "217",
            "--sprint",
            "1",
            "--pr-grouping",
            "group",
            "--strategy",
            "deterministic",
            "--pr-group",
            "S1T1=s1-serial",
            "--pr-group",
            "S1T2=s1-serial",
            "--no-comment",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    let comment_path = result_path(&payload, "comment_path");
    let comment = fs::read_to_string(&comment_path).expect("read sprint comment");
    assert!(
        comment.contains("| Task | Summary | Execution Mode |"),
        "{comment}"
    );
    assert!(
        comment.contains("| S1T1 | First lane task | per-sprint |"),
        "{comment}"
    );
    assert!(
        comment.contains("| S1T2 | Follow-up task | per-sprint |"),
        "{comment}"
    );
    assert!(!comment.contains("pr-shared"), "{comment}");
}

#[test]
fn start_sprint_rejects_deterministic_grouping_mismatch_with_plan_metadata() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let plan_file = tmp.path().join("mismatch-plan.md");
    let plan_file_s = plan_file.to_string_lossy().to_string();
    fs::write(
        &plan_file,
        r#"# Plan: metadata mismatch test

## Sprint 1: Single lane metadata
- **PR grouping intent**: `per-sprint`.
- **Execution Profile**: `serial` (parallel width 1).

### Task 1.1: First task
- **Location**:
  - crates/plan-issue-cli/src/a.rs
- **Dependencies**:
  - none
"#,
    )
    .expect("write plan");

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-sprint",
            "--plan",
            &plan_file_s,
            "--issue",
            "217",
            "--sprint",
            "1",
            "--pr-grouping",
            "group",
            "--strategy",
            "deterministic",
            "--pr-group",
            "S1T1=s1-lane",
            "--no-comment",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 1, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error"]["code"], "task-spec-generation-failed");
    assert!(
        payload["error"]["message"]
            .as_str()
            .is_some_and(|value| value.contains("plan metadata/CLI grouping mismatch")),
        "{}",
        out.stdout
    );
}

#[test]
fn write_subagent_prompts_groups_tasks_by_runtime_lane() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let plan_file = tmp.path().join("auto-single-lane-prompts-plan.md");
    let plan_file_s = plan_file.to_string_lossy().to_string();
    fs::write(
        &plan_file,
        r#"# Plan: auto single lane prompt grouping test

## Sprint 1: Serial lane
- **PR grouping intent**: `group`.
- **Execution Profile**: `serial` (parallel width 1).

### Task 1.1: First lane task
- **Location**:
  - crates/plan-issue-cli/src/a.rs
- **Dependencies**:
  - none

### Task 1.2: Follow-up task
- **Location**:
  - crates/plan-issue-cli/src/b.rs
- **Dependencies**:
  - Task 1.1
"#,
    )
    .expect("write plan");

    let prompts_out = tmp.path().join("sprint1-prompts");
    let prompts_out_s = prompts_out.to_string_lossy().to_string();
    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-sprint",
            "--plan",
            &plan_file_s,
            "--issue",
            "217",
            "--sprint",
            "1",
            "--subagent-prompts-out",
            &prompts_out_s,
            "--strategy",
            "auto",
            "--default-pr-grouping",
            "group",
            "--no-comment",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    let prompt_files = payload["payload"]["result"]["subagent_prompt_files"]
        .as_array()
        .expect("prompt files");
    assert_eq!(prompt_files.len(), 2, "{}", out.stdout);

    let prompt_path = prompt_files[0].as_str().expect("prompt path");
    assert!(prompt_path.ends_with("/S1T1.md"), "{prompt_path}");
    let prompt = fs::read_to_string(prompt_path).expect("read prompt");
    assert!(prompt.contains("- Tasks: S1T1, S1T2"), "{prompt}");
    assert!(prompt.contains("- Execution Mode: per-sprint"), "{prompt}");
    assert!(prompt.contains("## Lane Tasks"), "{prompt}");
    assert!(prompt.contains("- S1T1: First lane task"), "{prompt}");
    assert!(prompt.contains("- S1T2: Follow-up task"), "{prompt}");
}

#[test]
fn local_flow_plan_issue_local_dry_run_end_to_end_generates_artifacts() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let start_plan_out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-plan",
            "--plan",
            PLAN_PATH,
            "--pr-grouping",
            "per-sprint",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );
    assert_eq!(start_plan_out.code, 0, "stderr: {}", start_plan_out.stderr);
    let start_plan_json = parse_json(&start_plan_out.stdout);
    let issue_body_path = result_path(&start_plan_json, "issue_body_path");
    assert!(std::path::Path::new(&issue_body_path).is_file());

    let start_sprint_out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "start-sprint",
            "--plan",
            PLAN_PATH,
            "--issue",
            "217",
            "--sprint",
            "3",
            "--pr-grouping",
            "per-sprint",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );
    assert_eq!(
        start_sprint_out.code, 0,
        "stderr: {}",
        start_sprint_out.stderr
    );
    let start_sprint_json = parse_json(&start_sprint_out.stdout);
    let start_comment_path = result_path(&start_sprint_json, "comment_path");
    assert!(std::path::Path::new(&start_comment_path).is_file());

    let ready_sprint_out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "ready-sprint",
            "--plan",
            PLAN_PATH,
            "--issue",
            "217",
            "--sprint",
            "3",
            "--pr-grouping",
            "per-sprint",
            "--summary",
            "Ready for review",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );
    assert_eq!(
        ready_sprint_out.code, 0,
        "stderr: {}",
        ready_sprint_out.stderr
    );
    let ready_sprint_json = parse_json(&ready_sprint_out.stdout);
    let ready_comment_path = result_path(&ready_sprint_json, "comment_path");
    assert!(std::path::Path::new(&ready_comment_path).is_file());

    let accept_sprint_out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "accept-sprint",
            "--plan",
            PLAN_PATH,
            "--issue",
            "217",
            "--sprint",
            "3",
            "--pr-grouping",
            "per-sprint",
            "--approved-comment-url",
            "https://github.com/sympoies/nils-cli/issues/217#issuecomment-123456789",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );
    assert_eq!(
        accept_sprint_out.code, 0,
        "stderr: {}",
        accept_sprint_out.stderr
    );
    let accept_sprint_json = parse_json(&accept_sprint_out.stdout);
    let accepted_comment_path = result_path(&accept_sprint_json, "comment_path");
    let accepted_comment =
        fs::read_to_string(&accepted_comment_path).expect("read accepted comment");
    assert!(
        accepted_comment.contains("## Sprint 3 Accepted"),
        "{accepted_comment}"
    );
    assert!(
        accepted_comment
            .contains("https://github.com/sympoies/nils-cli/issues/217#issuecomment-123456789"),
        "{accepted_comment}"
    );
}

#[test]
fn local_flow_status_plan_body_file_reports_counts_and_comment_preview() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let issue_body_s = render_issue_body_for_local_plan(&tmp, &agent_home_s);
    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "status-plan",
            "--body-file",
            &issue_body_s,
            "--comment",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    let result = &payload["payload"]["result"];
    assert_eq!(payload["command"], "status-plan");
    assert_eq!(payload["status"], "ok");
    assert!(
        result["issue_source"]
            .as_str()
            .is_some_and(|source| source.starts_with("body-file:")),
        "{}",
        out.stdout
    );
    assert!(result["task_count"].as_u64().unwrap_or_default() > 0);
    assert!(
        result["status_counts"]["planned"]
            .as_u64()
            .unwrap_or_default()
            > 0
    );
    assert_eq!(result["comment_requested"], true);
    assert!(
        result["comment_preview"]
            .as_str()
            .is_some_and(|preview| preview.contains("## Plan Status Snapshot")),
        "{}",
        out.stdout
    );
}

#[test]
fn local_flow_ready_plan_body_file_accepts_summary_file_without_comment() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let issue_body_s = render_issue_body_for_local_plan(&tmp, &agent_home_s);
    let summary_file = tmp.path().join("ready-summary.md");
    fs::write(&summary_file, "Final plan review from summary file.\n").expect("write summary file");
    let summary_file_s = summary_file.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "ready-plan",
            "--body-file",
            &issue_body_s,
            "--summary-file",
            &summary_file_s,
            "--no-comment",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    let result = &payload["payload"]["result"];
    assert_eq!(payload["command"], "ready-plan");
    assert_eq!(payload["status"], "ok");
    assert_eq!(result["summary"], "Final plan review from summary file.\n");
    assert_eq!(result["label_update_requested"], false);
    assert_eq!(result["label_update_applied"], false);
    assert_eq!(result["comment_requested"], false);
    assert_eq!(result["comment_posted"], false);
}

#[test]
fn local_flow_ready_plan_missing_summary_file_returns_error() {
    let tmp = TempDir::new().expect("temp dir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let issue_body_s = render_issue_body_for_local_plan(&tmp, &agent_home_s);
    let missing_summary = tmp.path().join("missing-summary.md");
    let missing_summary_s = missing_summary.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "ready-plan",
            "--body-file",
            &issue_body_s,
            "--summary-file",
            &missing_summary_s,
            "--no-comment",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 1, "stderr: {}", out.stderr);
    let payload = parse_json(&out.stdout);
    assert_eq!(payload["command"], "ready-plan");
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["error"]["code"], "summary-read-failed");
    assert!(
        payload["error"]["message"]
            .as_str()
            .is_some_and(|msg| msg.contains("failed to read summary file")),
        "{}",
        out.stdout
    );
}

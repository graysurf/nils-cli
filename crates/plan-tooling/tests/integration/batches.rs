use crate::common;
use common::{run_plan_tooling, write_file};

use pretty_assertions::assert_eq;

#[test]
fn batches_json_includes_layers_external_blockers_and_conflict_risk() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    write_file(&dir.path().join("plan.md"), BATCH_PLAN);

    let out = run_plan_tooling(
        dir.path(),
        &["batches", "--file", "plan.md", "--sprint", "1"],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["file"], "plan.md");
    assert_eq!(v["sprint"], 1);
    assert_eq!(
        v["batches"],
        serde_json::json!([["Task 1.1"], ["Task 1.2", "Task 1.3"]])
    );
    assert_eq!(
        v["blocked_by_external"],
        serde_json::json!({"Task 1.3": ["Task 0.1"]})
    );
    assert_eq!(
        v["conflict_risk"],
        serde_json::json!([{"batch": 2, "overlap": ["src/shared.rs"]}])
    );
}

#[test]
fn batches_text_prints_sections() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    write_file(&dir.path().join("plan.md"), BATCH_PLAN);

    let out = run_plan_tooling(
        dir.path(),
        &[
            "batches", "--file", "plan.md", "--sprint", "1", "--format", "text",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("Batch 1:"));
    assert!(out.stdout.contains("External blockers:"));
    assert!(out.stdout.contains("Conflict risk"));
}

#[test]
fn batches_cycle_is_error() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    write_file(&dir.path().join("plan.md"), CYCLE_PLAN);

    let out = run_plan_tooling(
        dir.path(),
        &["batches", "--file", "plan.md", "--sprint", "1"],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("dependency cycle detected"));
}

#[test]
fn batches_invalid_sprint_is_usage_error() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    write_file(&dir.path().join("plan.md"), BATCH_PLAN);

    let out = run_plan_tooling(
        dir.path(),
        &["batches", "--file", "plan.md", "--sprint", "nope"],
    );
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("error: invalid --sprint"));
    assert!(out.stderr.contains("'nope'"));
}

const BATCH_PLAN: &str = r#"# Plan: Batches

## Sprint 1: S1

### Task 1.1: One
- **Location**:
  - src/a.rs
- **Description**: A
- **Dependencies**:
  - none
- **Acceptance criteria**:
  - ok
- **Validation**:
  - ok

### Task 1.2: Two
- **Location**:
  - src/shared.rs
- **Description**: B
- **Dependencies**:
  - Task 1.1
- **Acceptance criteria**:
  - ok
- **Validation**:
  - ok

### Task 1.3: Three
- **Location**:
  - src/shared.rs
- **Description**: C
- **Dependencies**:
  - Task 1.1, Task 0.1
- **Acceptance criteria**:
  - ok
- **Validation**:
  - ok
"#;

const CYCLE_PLAN: &str = r#"# Plan: Cycle

## Sprint 1: S1

### Task 1.1: One
- **Location**:
  - src/a.rs
- **Description**: A
- **Dependencies**:
  - Task 1.2
- **Acceptance criteria**:
  - ok
- **Validation**:
  - ok

### Task 1.2: Two
- **Location**:
  - src/b.rs
- **Description**: B
- **Dependencies**:
  - Task 1.1
- **Acceptance criteria**:
  - ok
- **Validation**:
  - ok
"#;

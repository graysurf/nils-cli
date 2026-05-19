use crate::common;
use common::{run_plan_tooling, write_file};

use pretty_assertions::assert_eq;

#[test]
fn to_json_pretty_parses_and_includes_start_lines() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let plan_path = dir.path().join("plan.md");
    write_file(&plan_path, VALID_PLAN);

    let out = run_plan_tooling(dir.path(), &["to-json", "--file", "plan.md", "--pretty"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["title"], "Plan: Example");
    assert_eq!(v["file"], "plan.md");
    assert_eq!(
        v["read_first"]["primary_source"],
        "plan-only waiver: integration fixture"
    );
    assert_eq!(v["read_first"]["source_type"], "plan-only waiver");
    assert_eq!(v["read_first"]["open_questions"], "none");
    assert_eq!(v["sprints"][0]["number"], 1);
    assert_eq!(v["sprints"][0]["start_line"], 9);
    assert_eq!(v["sprints"][0]["tasks"][0]["id"], "Task 1.1");
    assert_eq!(v["sprints"][0]["tasks"][0]["start_line"], 15);
    assert_eq!(v["sprints"][0]["tasks"][1]["id"], "Task 1.2");
    assert_eq!(v["sprints"][0]["tasks"][1]["start_line"], 27);
}

#[test]
fn to_json_merges_scalar_and_list_continuation_lines() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let plan_path = dir.path().join("plan.md");
    write_file(&plan_path, CONTINUATION_PLAN);

    let out = run_plan_tooling(dir.path(), &["to-json", "--file", "plan.md"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    let task = &v["sprints"][0]["tasks"][0];
    assert_eq!(
        task["description"],
        "Document nested area rules and link the target map source."
    );
    assert_eq!(task["complexity"], 3);
    assert_eq!(
        task["acceptance_criteria"][0],
        "The target map is referenced from the source artifact rather than copied into multiple docs."
    );
}

#[test]
fn to_json_sprint_filter_returns_exact_sprint() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let plan_path = dir.path().join("plan.md");
    write_file(&plan_path, VALID_PLAN);

    let out = run_plan_tooling(
        dir.path(),
        &["to-json", "--file", "plan.md", "--sprint", "1"],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["sprints"].as_array().unwrap().len(), 1);

    let out = run_plan_tooling(
        dir.path(),
        &["to-json", "--file", "plan.md", "--sprint", "2"],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["sprints"].as_array().unwrap().len(), 0);
}

#[test]
fn to_json_includes_sprint_metadata_when_present() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let plan_path = dir.path().join("plan.md");
    write_file(&plan_path, METADATA_PLAN);

    let out = run_plan_tooling(dir.path(), &["to-json", "--file", "plan.md"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["sprints"][0]["metadata"]["pr_grouping_intent"], "group");
    assert_eq!(
        v["sprints"][0]["metadata"]["execution_profile"],
        "parallel-x2"
    );
    assert_eq!(v["sprints"][0]["metadata"]["parallel_width"], 2);
}

#[test]
fn to_json_rejects_near_miss_metadata_field_name() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let plan_path = dir.path().join("plan.md");
    write_file(&plan_path, METADATA_BAD_FIELD_PLAN);

    let out = run_plan_tooling(dir.path(), &["to-json", "--file", "plan.md"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("invalid metadata field"));
    assert!(out.stderr.contains("PR grouping intent"));
}

#[test]
fn to_json_dependency_objects_carry_id_and_notes() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let plan_path = dir.path().join("plan.md");
    write_file(&plan_path, DEPENDENCY_ANNOTATION_PLAN);

    let out = run_plan_tooling(dir.path(), &["to-json", "--file", "plan.md"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    let deps_t1_2 = v["sprints"][0]["tasks"][1]["dependencies"]
        .as_array()
        .expect("dependencies array on Task 1.2");
    assert_eq!(deps_t1_2.len(), 1);
    assert_eq!(deps_t1_2[0]["id"], "Task 1.1");
    assert_eq!(deps_t1_2[0]["notes"], "(only when X flagged)");
    let deps_t1_1 = v["sprints"][0]["tasks"][0]["dependencies"]
        .as_array()
        .expect("dependencies array on Task 1.1 (none → empty array)");
    assert!(deps_t1_1.is_empty());
}

#[test]
fn to_json_invalid_sprint_is_usage_error() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let plan_path = dir.path().join("plan.md");
    write_file(&plan_path, VALID_PLAN);

    let out = run_plan_tooling(
        dir.path(),
        &["to-json", "--file", "plan.md", "--sprint", "nope"],
    );
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("error: invalid --sprint"));
    assert!(out.stderr.contains("'nope'"));
}

#[test]
fn to_json_help_prints_usage_and_exits_zero() {
    let dir = tempfile::TempDir::new().expect("tempdir");

    let out = run_plan_tooling(dir.path(), &["to-json", "--help"]);
    assert_eq!(out.code, 0);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.contains("Usage:"));
    assert!(out.stderr.contains("plan_to_json.sh"));
}

#[test]
fn to_json_unknown_argument_is_usage_error() {
    let dir = tempfile::TempDir::new().expect("tempdir");

    let out = run_plan_tooling(dir.path(), &["to-json", "--wat"]);
    assert_eq!(out.code, 2);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.contains("plan_to_json: unknown argument: --wat"));
}

#[test]
fn to_json_missing_value_for_file_is_usage_error() {
    let dir = tempfile::TempDir::new().expect("tempdir");

    let out = run_plan_tooling(dir.path(), &["to-json", "--file"]);
    assert_eq!(out.code, 2);
    assert!(out.stdout.is_empty());
    assert!(
        out.stderr
            .contains("plan_to_json: missing value for --file")
    );
}

#[test]
fn to_json_missing_value_for_sprint_is_usage_error() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let plan_path = dir.path().join("plan.md");
    write_file(&plan_path, VALID_PLAN);

    let out = run_plan_tooling(dir.path(), &["to-json", "--file", "plan.md", "--sprint"]);
    assert_eq!(out.code, 2);
    assert!(out.stdout.is_empty());
    assert!(
        out.stderr
            .contains("plan_to_json: missing value for --sprint")
    );
}

#[test]
fn to_json_missing_file_is_parse_error() {
    let dir = tempfile::TempDir::new().expect("tempdir");

    let out = run_plan_tooling(dir.path(), &["to-json", "--file", "missing.md"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("error: plan file not found: missing.md")
    );
}

const VALID_PLAN: &str = r#"# Plan: Example

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

## Sprint 1: First sprint
**Goal**: ...
**Demo/Validation**:
- Command(s): ...
- Verify: ...

### Task 1.1: Do thing
- **Location**:
  - `src/a.rs`
- **Description**: Do A
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - A works
- **Validation**:
  - cargo test -p plan-tooling

### Task 1.2: Do other
- **Location**:
  - `src/b.rs`
- **Description**: Do B
- **Dependencies**:
  - Task 1.1
- **Complexity**: 2
- **Acceptance criteria**:
  - B works
- **Validation**:
  - cargo test -p plan-tooling
"#;

const METADATA_PLAN: &str = r#"# Plan: Metadata Example

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

## Sprint 1: First sprint
- **PR grouping intent**: `group`
- **Execution Profile**: `parallel-x2` (parallel width 2)

### Task 1.1: Do thing
- **Location**:
  - `src/a.rs`
- **Description**: Do A
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - A works
- **Validation**:
  - cargo test -p plan-tooling
"#;

const METADATA_BAD_FIELD_PLAN: &str = r#"# Plan: Metadata Bad Field

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

## Sprint 1: First sprint
- **PR Grouping Intent**: `group`
- **Execution Profile**: `serial` (parallel width 1)

### Task 1.1: Do thing
- **Location**:
  - `src/a.rs`
- **Description**: Do A
- **Dependencies**:
  - none
- **Complexity**: 3
- **Acceptance criteria**:
  - A works
- **Validation**:
  - cargo test -p plan-tooling
"#;

const DEPENDENCY_ANNOTATION_PLAN: &str = r#"# Plan: Annotated deps

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

## Sprint 1: First sprint

### Task 1.1: Anchor
- **Location**:
  - `src/a.rs`
- **Description**: Anchor task
- **Dependencies**:
  - none
- **Acceptance criteria**:
  - A works
- **Validation**:
  - cargo test

### Task 1.2: Annotated
- **Location**:
  - `src/b.rs`
- **Description**: Depends on 1.1 with a note.
- **Dependencies**:
  - Task 1.1 (only when X flagged)
- **Acceptance criteria**:
  - B works
- **Validation**:
  - cargo test
"#;

const CONTINUATION_PLAN: &str = r#"# Plan: Continuation Example

## Read First

- Primary source:
  plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

## Sprint 1: First sprint

### Task 1.1: Document nested area rules
- **Location**:
  - `src/a.rs`
- **Description**: Document nested area rules and link
  the target map source.
- **Dependencies**:
  - none
- **Complexity**:
  - 3
- **Acceptance criteria**:
  - The target map is referenced from the source artifact rather than copied
    into multiple docs.
- **Validation**:
  - cargo test -p plan-tooling
"#;

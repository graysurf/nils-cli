use crate::common;
use common::{git, init_repo, run_plan_tooling, write_file};

use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[test]
fn validate_ok_with_explicit_file() {
    let repo = init_repo();
    write_file(&repo.path().join("plan.md"), VALID_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "plan.md"]);
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

#[test]
fn validate_explicit_file_without_git_repo() {
    let dir = TempDir::new().expect("tempdir");
    write_file(&dir.path().join("plan.md"), VALID_PLAN);

    let out = run_plan_tooling(dir.path(), &["validate", "--file", "plan.md"]);
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

#[test]
fn validate_fails_with_errors() {
    let repo = init_repo();
    write_file(&repo.path().join("bad.md"), INVALID_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "bad.md"]);
    assert_eq!(out.code, 1);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.contains("error:"));
    assert!(out.stderr.contains("Location"));
}

#[test]
fn validate_default_discovers_tracked_docs_plans() {
    let repo = init_repo();

    let plan_path = repo.path().join("docs/plans/example-plan.md");
    write_file(&plan_path, VALID_PLAN);

    git(repo.path(), &["add", "docs/plans/example-plan.md"]);
    git(repo.path(), &["commit", "-m", "add plan", "-q"]);

    let out = run_plan_tooling(repo.path(), &["validate"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

#[test]
fn validate_repo_relative_file_works_from_nested_dir() {
    let repo = init_repo();

    let plan_path = repo.path().join("docs/plans/example-plan.md");
    write_file(&plan_path, VALID_PLAN);

    let nested = repo.path().join("nested/dir");
    std::fs::create_dir_all(&nested).expect("create_dir_all");

    let out = run_plan_tooling(
        &nested,
        &["validate", "--file", "docs/plans/example-plan.md"],
    );
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

#[test]
fn validate_missing_dependencies_is_error() {
    let repo = init_repo();
    write_file(&repo.path().join("missing-deps.md"), MISSING_DEPS_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "missing-deps.md"]);
    assert_eq!(out.code, 1);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.contains("missing Dependencies"));
}

#[test]
fn validate_redirect_command_is_not_placeholder() {
    let repo = init_repo();
    write_file(&repo.path().join("redirect.md"), REDIRECT_VALIDATION_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "redirect.md"]);
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

#[test]
fn validate_dependency_error_carries_line_and_example() {
    let repo = init_repo();
    write_file(&repo.path().join("bad-deps.md"), INVALID_DEP_FORMAT_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "bad-deps.md"]);
    assert_eq!(out.code, 1);
    assert!(out.stdout.is_empty());
    assert!(
        out.stderr.contains("invalid dependency"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("e.g. 'Task 1.2'"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("line "),
        "stderr should reference a line number, got: {}",
        out.stderr
    );
}

#[test]
fn validate_json_ok_with_explicit_file() {
    let repo = init_repo();
    write_file(&repo.path().join("plan.md"), VALID_PLAN);

    let out = run_plan_tooling(
        repo.path(),
        &["validate", "--file", "plan.md", "--format", "json"],
    );
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stderr.is_empty());

    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["ok"], true);
    assert_eq!(v["files"], serde_json::json!(["plan.md"]));
    assert_eq!(v["errors"], serde_json::json!([]));
}

#[test]
fn validate_json_returns_errors_and_exit_one() {
    let repo = init_repo();
    write_file(&repo.path().join("bad.md"), INVALID_PLAN);

    let out = run_plan_tooling(
        repo.path(),
        &["validate", "--file", "bad.md", "--format", "json"],
    );
    assert_eq!(
        out.code, 1,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stderr.is_empty());

    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["ok"], false);
    assert_eq!(v["files"], serde_json::json!(["bad.md"]));
    let errs = v["errors"].as_array().expect("errors array");
    assert!(!errs.is_empty());
    assert!(errs.iter().any(|e| {
        e.as_str()
            .is_some_and(|s| s.contains("Location must be repo-relative"))
    }));
}

#[test]
fn validate_json_no_files_emits_empty_payload() {
    let dir = TempDir::new().expect("tempdir");

    let out = run_plan_tooling(dir.path(), &["validate", "--format", "json"]);
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stderr.is_empty());

    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(
        v,
        serde_json::json!({
            "ok": true,
            "files": [],
            "errors": []
        })
    );
}

#[test]
fn validate_invalid_format_is_usage_error() {
    let repo = init_repo();
    write_file(&repo.path().join("plan.md"), VALID_PLAN);

    let out = run_plan_tooling(
        repo.path(),
        &["validate", "--file", "plan.md", "--format", "yaml"],
    );
    assert_eq!(out.code, 2);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.contains("invalid --format"));
}

#[test]
fn validate_fails_when_per_sprint_intent_conflicts_with_parallel_profile() {
    let repo = init_repo();
    write_file(
        &repo.path().join("metadata-mismatch.md"),
        METADATA_MISMATCH_PLAN,
    );

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "metadata-mismatch.md"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("PR grouping intent"));
    assert!(out.stderr.contains("parallel width 2"));
}

#[test]
fn validate_fails_when_sprint_metadata_is_partial() {
    let repo = init_repo();
    write_file(
        &repo.path().join("metadata-partial.md"),
        METADATA_PARTIAL_PLAN,
    );

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "metadata-partial.md"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("must include both `PR grouping intent` and `Execution Profile`")
    );
}

const VALID_PLAN: &str = r#"# Plan: Example

## Sprint 1: First sprint

### Task 1.1: Do thing
- **Location**:
  - `src/a.rs`
- **Description**: Do A
- **Dependencies**:
  - none
- **Acceptance criteria**:
  - A works
- **Validation**:
  - cargo test -p plan-tooling
"#;

const INVALID_PLAN: &str = r#"# Plan: Bad

## Sprint 1: Bad sprint

### Task 1.1: Broken
- **Location**:
  - `/abs/path.rs`
- **Description**: TODO
- **Dependencies**:
  - Task 1.2
- **Acceptance criteria**:
  - <TBD>
- **Validation**:
  - TBD
"#;

const MISSING_DEPS_PLAN: &str = r#"# Plan: Missing deps

## Sprint 1: First sprint

### Task 1.1: Do thing
- **Location**:
  - `src/a.rs`
- **Description**: Do A
- **Dependencies**:
- **Acceptance criteria**:
  - A works
- **Validation**:
  - cargo test -p plan-tooling
"#;

const REDIRECT_VALIDATION_PLAN: &str = r#"# Plan: Redirect

## Sprint 1: First sprint

### Task 1.1: Validate shell redirect command
- **Location**:
  - `src/a.rs`
- **Description**: Keep redirect-based checks
- **Dependencies**:
  - none
- **Acceptance criteria**:
  - Redirect command is accepted
- **Validation**:
  - cat < input.txt > output.txt
"#;

const INVALID_DEP_FORMAT_PLAN: &str = r#"# Plan: Bad deps

## Sprint 1: First sprint

### Task 1.1: Do thing
- **Location**:
  - `src/a.rs`
- **Description**: Do A
- **Dependencies**:
  - 1.1
  - Task x.y
- **Acceptance criteria**:
  - A works
- **Validation**:
  - cargo test
"#;

const METADATA_MISMATCH_PLAN: &str = r#"# Plan: Metadata mismatch

## Sprint 1: First sprint
- **PR grouping intent**: `per-sprint`
- **Execution Profile**: `parallel-x2` (parallel width 2)

### Task 1.1: Do thing
- **Location**:
  - `src/a.rs`
- **Description**: Do A
- **Dependencies**:
  - none
- **Acceptance criteria**:
  - A works
- **Validation**:
  - cargo test -p plan-tooling
"#;

const METADATA_PARTIAL_PLAN: &str = r#"# Plan: Metadata partial

## Sprint 1: First sprint
- **PR grouping intent**: `group`

### Task 1.1: Do thing
- **Location**:
  - `src/a.rs`
- **Description**: Do A
- **Dependencies**:
  - none
- **Acceptance criteria**:
  - A works
- **Validation**:
  - cargo test -p plan-tooling
"#;

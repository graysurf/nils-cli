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
fn validate_fails_when_read_first_section_is_missing() {
    let repo = init_repo();
    write_file(
        &repo.path().join("missing-read-first.md"),
        MISSING_READ_FIRST_PLAN,
    );

    let out = run_plan_tooling(
        repo.path(),
        &["validate", "--file", "missing-read-first.md"],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("missing Read First"));
    assert!(out.stderr.contains("Primary source"));
}

#[test]
fn validate_fails_when_read_first_source_path_is_missing() {
    let repo = init_repo();
    write_file(
        &repo.path().join("missing-source.md"),
        MISSING_SOURCE_PATH_PLAN,
    );

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "missing-source.md"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("Primary source path not found"));
    assert!(out.stderr.contains("docs/source/missing.md"));
}

#[test]
fn validate_accepts_existing_read_first_source_path() {
    let repo = init_repo();
    write_file(&repo.path().join("docs/source/spec.md"), "# Spec\n");
    write_file(&repo.path().join("source-backed.md"), SOURCE_BACKED_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "source-backed.md"]);
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

#[test]
fn validate_fails_when_complexity_field_is_present_without_integer() {
    let repo = init_repo();
    write_file(
        &repo.path().join("empty-complexity.md"),
        EMPTY_COMPLEXITY_PLAN,
    );

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "empty-complexity.md"]);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("missing Complexity"));
    assert!(out.stderr.contains("omit the field or set a 1-10 integer"));
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
fn validate_fix_rewrites_inline_comma_dep_list_and_passes() {
    let repo = init_repo();
    write_file(&repo.path().join("fixable.md"), FIXABLE_INLINE_COMMA_PLAN);
    let plan_path = repo.path().join("fixable.md");

    // Before --fix: validation fails because the inline-comma form yields
    // an "invalid dependency" entry for the second comma-separated token
    // (the parser today splits on commas but the value `1.1, 1.2` reaches
    // it as a single list item that the user wrote inline).
    let before = run_plan_tooling(repo.path(), &["validate", "--file", "fixable.md"]);
    // Then run --fix and re-validate via the same invocation.
    let fix_out = run_plan_tooling(repo.path(), &["validate", "--file", "fixable.md", "--fix"]);
    assert_eq!(
        fix_out.code, 0,
        "fixable plan should validate after --fix; stdout:{} stderr:{}",
        fix_out.stdout, fix_out.stderr,
    );
    // Disk content must match canonical form.
    let after_disk = std::fs::read_to_string(&plan_path).expect("plan exists");
    assert!(
        after_disk.contains("  - Task 1.1\n  - Task 1.2"),
        "rewritten plan must split into canonical bullets, got:\n{after_disk}",
    );
    // Running --fix again must be a fixed point (no second rewrite).
    let after_disk_again = std::fs::read_to_string(&plan_path).expect("plan exists");
    let twice = run_plan_tooling(repo.path(), &["validate", "--file", "fixable.md", "--fix"]);
    assert_eq!(twice.code, 0);
    let after_twice_disk = std::fs::read_to_string(&plan_path).expect("plan exists");
    assert_eq!(
        after_disk_again, after_twice_disk,
        "--fix must be idempotent"
    );

    // Sanity check the pre-fix run produced the dependency error.
    assert_eq!(before.code, 1);
}

#[test]
fn validate_fix_strips_backtick_wrapped_primary_source() {
    let repo = init_repo();
    std::fs::create_dir_all(repo.path().join("docs/source")).expect("create_dir_all");
    write_file(&repo.path().join("docs/source/spec.md"), "# Spec\n");
    write_file(
        &repo.path().join("backtick.md"),
        BACKTICK_PRIMARY_SOURCE_PLAN,
    );

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "backtick.md", "--fix"]);
    assert_eq!(
        out.code, 0,
        "should validate after stripping backticks; stderr: {}",
        out.stderr,
    );
    let after = std::fs::read_to_string(repo.path().join("backtick.md")).expect("plan exists");
    assert!(
        after.contains("- Primary source: docs/source/spec.md\n"),
        "expected stripped Primary source, got:\n{after}",
    );
}

#[test]
fn validate_text_groups_errors_when_three_or_more_share_class() {
    let repo = init_repo();
    write_file(&repo.path().join("group.md"), GROUPED_DEP_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "group.md"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("[dependency-invalid] (x3)"),
        "stderr should contain grouped header, got:\n{}",
        out.stderr,
    );
    assert!(
        out.stderr.contains("  - "),
        "stderr should contain indented occurrence lines, got:\n{}",
        out.stderr,
    );
}

#[test]
fn validate_no_group_flag_restores_flat_text_output() {
    let repo = init_repo();
    write_file(&repo.path().join("group.md"), GROUPED_DEP_PLAN);

    let out = run_plan_tooling(
        repo.path(),
        &["validate", "--file", "group.md", "--no-group"],
    );
    assert_eq!(out.code, 1);
    assert!(
        !out.stderr.contains("[dependency-invalid] (x3)"),
        "--no-group should suppress grouped headers, got:\n{}",
        out.stderr,
    );
    // Each occurrence still emitted on its own `error:` line.
    let dep_invalid_lines = out
        .stderr
        .lines()
        .filter(|l| l.contains("invalid dependency"))
        .count();
    assert!(
        dep_invalid_lines >= 3,
        "expected >=3 invalid-dependency `error:` lines, got:\n{}",
        out.stderr,
    );
}

#[test]
fn validate_json_output_unaffected_by_grouping() {
    let repo = init_repo();
    write_file(&repo.path().join("group.md"), GROUPED_DEP_PLAN);

    let default_out = run_plan_tooling(
        repo.path(),
        &["validate", "--file", "group.md", "--format", "json"],
    );
    let no_group_out = run_plan_tooling(
        repo.path(),
        &[
            "validate",
            "--file",
            "group.md",
            "--format",
            "json",
            "--no-group",
        ],
    );
    assert_eq!(default_out.code, 1);
    assert_eq!(no_group_out.code, 1);
    // Byte-for-byte identical JSON regardless of --no-group.
    assert_eq!(default_out.stdout, no_group_out.stdout);
}

#[test]
fn validate_accepts_directory_location_when_dir_exists() {
    let repo = init_repo();
    std::fs::create_dir_all(repo.path().join("sip_automation/results/rounds"))
        .expect("create_dir_all");
    write_file(&repo.path().join("dir-loc.md"), DIRECTORY_LOCATION_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "dir-loc.md"]);
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
}

#[test]
fn validate_rejects_missing_directory_location() {
    let repo = init_repo();
    // Do NOT create the directory — directory missing should fail.
    write_file(&repo.path().join("dir-loc.md"), DIRECTORY_LOCATION_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "dir-loc.md"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("Location directory not found"),
        "stderr: {}",
        out.stderr,
    );
}

#[test]
fn validate_accepts_dependency_with_trailing_note() {
    let repo = init_repo();
    write_file(&repo.path().join("annotated.md"), ANNOTATED_DEP_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "annotated.md"]);
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
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
fn validate_backtick_wrapped_placeholder_in_description_is_accepted() {
    let repo = init_repo();
    write_file(&repo.path().join("backtick.md"), BACKTICK_DESCRIPTION_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "backtick.md"]);
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
fn validate_explain_appends_examples_only_for_triggered_classes_on_failure() {
    let repo = init_repo();
    write_file(&repo.path().join("bad.md"), INVALID_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "bad.md", "--explain"]);
    assert_eq!(out.code, 1);
    assert!(out.stdout.is_empty());
    // Errors still printed.
    assert!(out.stderr.contains("error:"));
    // Examples block appears.
    assert!(
        out.stderr.contains("Examples:"),
        "stderr should append Examples block, got: {}",
        out.stderr
    );
    // Triggered classes from INVALID_PLAN: location-absolute, description-placeholder
    // (TODO), dependency-unknown (Task 1.2 not in plan), acceptance-placeholder
    // (<TBD>), validation-placeholder (TBD).
    assert!(out.stderr.contains("[location-absolute]"));
    assert!(out.stderr.contains("[description-placeholder]"));
    assert!(out.stderr.contains("[dependency-unknown]"));
    // Classes that did NOT fire should be absent (no globs, no missing fields).
    assert!(!out.stderr.contains("[location-glob]"));
    assert!(!out.stderr.contains("[description-missing]"));
    assert!(!out.stderr.contains("[dependencies-missing]"));
}

#[test]
fn validate_explain_on_success_prints_full_catalog() {
    let repo = init_repo();
    write_file(&repo.path().join("plan.md"), VALID_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "plan.md", "--explain"]);
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stdout.is_empty());
    // Successful validate prints zero errors but still emits the explainer.
    assert!(!out.stderr.contains("error:"));
    assert!(out.stderr.contains("Examples:"));
    // Catalog should expose every known class on success.
    for class in [
        "[location-absolute]",
        "[location-glob]",
        "[description-placeholder]",
        "[dependency-invalid]",
        "[dependency-unknown]",
        "[sprint-metadata-mismatch]",
    ] {
        assert!(
            out.stderr.contains(class),
            "stderr missing class {class}, got: {}",
            out.stderr
        );
    }
}

#[test]
fn validate_explain_json_includes_explanations_array() {
    let repo = init_repo();
    write_file(&repo.path().join("bad.md"), INVALID_PLAN);

    let out = run_plan_tooling(
        repo.path(),
        &[
            "validate",
            "--file",
            "bad.md",
            "--format",
            "json",
            "--explain",
        ],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr.is_empty());

    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["ok"], false);
    let explanations = v["explanations"].as_array().expect("explanations array");
    assert!(!explanations.is_empty());
    let classes: Vec<&str> = explanations
        .iter()
        .map(|e| e["class"].as_str().unwrap_or(""))
        .collect();
    assert!(classes.contains(&"location-absolute"));
    assert!(classes.contains(&"description-placeholder"));
}

#[test]
fn validate_no_explain_omits_explanations() {
    let repo = init_repo();
    write_file(&repo.path().join("bad.md"), INVALID_PLAN);

    let out = run_plan_tooling(
        repo.path(),
        &["validate", "--file", "bad.md", "--format", "json"],
    );
    assert_eq!(out.code, 1);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    // explanations field should be absent (skip_serializing_if).
    assert!(
        v.get("explanations").is_none(),
        "default validate must not emit explanations: {}",
        out.stdout
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
fn validate() {
    let repo = init_repo();
    write_file(&repo.path().join("plan.md"), VALID_PLAN);

    let out = run_plan_tooling(repo.path(), &["validate", "--file", "plan.md"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
}

#[test]
fn validate_bundle_contract_docs() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let spec = std::fs::read_to_string(repo.join("docs/specs/plan-source-bundle-contract-v1.md"))
        .expect("bundle contract spec");
    assert!(spec.contains("Direct source-doc execution waiver"));
    assert!(spec.contains("Recommended execution state"));
    assert!(spec.contains("discussion-source.md"));
    assert!(spec.contains("review-source.md"));
}

#[test]
fn validate_accepts_not_yet_started_plan_bundle() {
    let repo = init_repo();
    write_valid_bundle_source_and_plan(repo.path(), "demo");

    let out = run_plan_tooling(
        repo.path(),
        &["validate", "--file", "docs/plans/demo/demo-plan.md"],
    );
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
}

#[test]
fn validate_fails_when_source_doc_recommends_another_plan() {
    let repo = init_repo();
    write_valid_bundle_source_and_plan(repo.path(), "demo");
    write_file(
        &repo
            .path()
            .join("docs/plans/demo/demo-discussion-source.md"),
        r#"# Demo Source

- Recommended plan:
  `docs/plans/other/other-plan.md`
- Recommended execution state:
  `docs/plans/demo/demo-execution-state.md`
"#,
    );

    let out = run_plan_tooling(
        repo.path(),
        &["validate", "--file", "docs/plans/demo/demo-plan.md"],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("recommends wrong plan"));
    assert!(out.stderr.contains("docs/plans/demo/demo-plan.md"));
}

#[test]
fn validate_fails_when_execution_state_points_to_wrong_plan() {
    let repo = init_repo();
    write_valid_bundle_source_and_plan(repo.path(), "demo");
    write_file(
        &repo.path().join("docs/plans/demo/demo-execution-state.md"),
        r#"# Execution State: Demo

- Status: in progress
- Source document:
  `docs/plans/other/other-plan.md`
- Direct source-doc execution waiver: not applicable
"#,
    );

    let out = run_plan_tooling(
        repo.path(),
        &["validate", "--file", "docs/plans/demo/demo-plan.md"],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("points to wrong source document"));
}

#[test]
fn validate_fails_without_direct_source_doc_waiver() {
    let repo = init_repo();
    write_valid_bundle_source_and_plan(repo.path(), "demo");
    write_file(
        &repo.path().join("docs/plans/demo/demo-execution-state.md"),
        r#"# Execution State: Demo

- Status: in progress
- Source document:
  `docs/plans/demo/demo-discussion-source.md`
- Direct source-doc execution waiver: not applicable
"#,
    );

    let out = run_plan_tooling(
        repo.path(),
        &["validate", "--file", "docs/plans/demo/demo-plan.md"],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("without `Direct source-doc execution waiver`")
    );
}

#[test]
fn validate_accepts_direct_source_doc_waiver() {
    let repo = init_repo();
    write_valid_bundle_source_and_plan(repo.path(), "demo");
    write_file(
        &repo.path().join("docs/plans/demo/demo-execution-state.md"),
        r#"# Execution State: Demo

- Status: in progress
- Source document:
  `docs/plans/demo/demo-discussion-source.md`
- Direct source-doc execution waiver: bounded single-step source execution
"#,
    );

    let out = run_plan_tooling(
        repo.path(),
        &["validate", "--file", "docs/plans/demo/demo-plan.md"],
    );
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
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

fn write_valid_bundle_source_and_plan(repo: &std::path::Path, slug: &str) {
    write_file(
        &repo.join(format!("docs/plans/{slug}/{slug}-discussion-source.md")),
        &format!(
            r#"# Demo Source

- Recommended plan:
  `docs/plans/{slug}/{slug}-plan.md`
- Recommended execution state:
  `docs/plans/{slug}/{slug}-execution-state.md`
"#
        ),
    );
    write_file(
        &repo.join(format!("docs/plans/{slug}/{slug}-plan.md")),
        &format!(
            r#"# Plan: Demo

## Read First

- Primary source: docs/plans/{slug}/{slug}-discussion-source.md
- Source type: discussion-to-implementation-doc
- Open questions carried into execution: none

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
  - cargo test -p nils-plan-tooling
"#
        ),
    );
}

const VALID_PLAN: &str = r#"# Plan: Example

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

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

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

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

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

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

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

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

const BACKTICK_DESCRIPTION_PLAN: &str = r#"# Plan: Backtick description

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

## Sprint 1: First sprint

### Task 1.1: Document a usage slot
- **Location**:
  - `src/a.rs`
- **Description**: Invoke `<arg>` and `<TBD>` like `TODO: keep` to wire the slot.
- **Dependencies**:
  - none
- **Acceptance criteria**:
  - Slot resolves
- **Validation**:
  - cargo test
"#;

const FIXABLE_INLINE_COMMA_PLAN: &str = r#"# Plan: Fixable inline-comma

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

### Task 1.2: B
- **Location**:
  - `src/b.rs`
- **Description**: B
- **Dependencies**:
  - none
- **Acceptance criteria**:
  - B works
- **Validation**:
  - cargo test

### Task 1.3: C inline-comma deps
- **Location**:
  - `src/c.rs`
- **Description**: Depends on 1.1 and 1.2 written inline.
- **Dependencies**:
  - 1.1, 1.2
- **Acceptance criteria**:
  - C works
- **Validation**:
  - cargo test
"#;

const BACKTICK_PRIMARY_SOURCE_PLAN: &str = r#"# Plan: Backtick primary source

## Read First

- Primary source: `docs/source/spec.md`
- Source type: existing issue/spec
- Open questions carried into execution: none

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
  - cargo test
"#;

const GROUPED_DEP_PLAN: &str = r#"# Plan: Grouped deps

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

## Sprint 1: First sprint

### Task 1.1: Bad dep A
- **Location**:
  - `src/a.rs`
- **Description**: Bad dep A
- **Dependencies**:
  - bad-a
- **Acceptance criteria**:
  - Done
- **Validation**:
  - cargo test

### Task 1.2: Bad dep B
- **Location**:
  - `src/b.rs`
- **Description**: Bad dep B
- **Dependencies**:
  - bad-b
- **Acceptance criteria**:
  - Done
- **Validation**:
  - cargo test

### Task 1.3: Bad dep C
- **Location**:
  - `src/c.rs`
- **Description**: Bad dep C
- **Dependencies**:
  - bad-c
- **Acceptance criteria**:
  - Done
- **Validation**:
  - cargo test
"#;

const DIRECTORY_LOCATION_PLAN: &str = r#"# Plan: Directory location

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

## Sprint 1: First sprint

### Task 1.1: Anchor on directory
- **Location**:
  - `sip_automation/results/rounds/`
- **Description**: Round-baseline results live under this dir.
- **Dependencies**:
  - none
- **Acceptance criteria**:
  - Anchored on directory tree
- **Validation**:
  - cargo test
"#;

const ANNOTATED_DEP_PLAN: &str = r#"# Plan: Annotated deps

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

## Sprint 1: First sprint

### Task 1.1: Anchor task
- **Location**:
  - `src/a.rs`
- **Description**: Anchor for downstream tasks
- **Dependencies**:
  - none
- **Acceptance criteria**:
  - A works
- **Validation**:
  - cargo test

### Task 1.2: Annotated dependency
- **Location**:
  - `src/b.rs`
- **Description**: Depends on 1.1 with a free-form note.
- **Dependencies**:
  - Task 1.1 (only when feature flag is set)
- **Acceptance criteria**:
  - B works
- **Validation**:
  - cargo test
"#;

const INVALID_DEP_FORMAT_PLAN: &str = r#"# Plan: Bad deps

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

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

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

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

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

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

const MISSING_READ_FIRST_PLAN: &str = r#"# Plan: Missing Read First

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

const MISSING_SOURCE_PATH_PLAN: &str = r#"# Plan: Missing source path

## Read First

- Primary source: docs/source/missing.md
- Source type: review-to-improvement-doc
- Open questions carried into execution: none

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

const SOURCE_BACKED_PLAN: &str = r#"# Plan: Source backed

## Read First

- Primary source: docs/source/spec.md
- Source type: existing issue/spec
- Open questions carried into execution: none

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

const EMPTY_COMPLEXITY_PLAN: &str = r#"# Plan: Empty complexity

## Read First

- Primary source: plan-only waiver: integration fixture
- Source type: plan-only waiver
- Open questions carried into execution: none

## Sprint 1: First sprint

### Task 1.1: Do thing
- **Location**:
  - `src/a.rs`
- **Description**: Do A
- **Dependencies**:
  - none
- **Complexity**:
- **Acceptance criteria**:
  - A works
- **Validation**:
  - cargo test -p plan-tooling
"#;

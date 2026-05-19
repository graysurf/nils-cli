use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;

use plan_tooling::parse::parse_plan_with_display;
use plan_tooling::split_prs::{
    SplitPlanOptions, SplitPrGrouping, SplitPrStrategy, SplitScope, build_split_plan_records,
    select_sprints_for_scope,
};
use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

use crate::common;
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("split_prs")
        .join(name)
}

fn fixture_text(name: &str) -> String {
    fs::read_to_string(fixture_path(name)).expect("fixture exists")
}

fn tsv_rows(name: &str) -> Vec<Vec<String>> {
    fixture_text(name)
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split('\t').map(|part| part.to_string()).collect())
        .collect()
}

fn parsed_fixture_plan(name: &str) -> plan_tooling::parse::Plan {
    let path = fixture_path(name);
    let (plan, errors) =
        parse_plan_with_display(&path, &path.to_string_lossy()).expect("fixture parses");
    assert!(errors.is_empty(), "unexpected parse errors: {errors:?}");
    plan
}

#[test]
fn split_prs_deterministic_per_sprint_tsv_matches_fixture() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(&dir.path().join("plan.md"), &fixture_text("duck-plan.md"));

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "1",
            "--pr-grouping",
            "per-sprint",
            "--format",
            "tsv",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert_eq!(out.stdout, fixture_text("per_sprint_expected.tsv"));
}

#[test]
fn split_prs_deterministic_group_json_matches_fixture() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(&dir.path().join("plan.md"), &fixture_text("duck-plan.md"));

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "2",
            "--pr-grouping",
            "group",
            "--pr-group",
            "S2T1=s2-isolated",
            "--pr-group",
            "S2T2=s2-shared",
            "--pr-group",
            "S2T3=s2-shared",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let actual: Value = serde_json::from_str(&out.stdout).expect("json");
    let mut expected: Value =
        serde_json::from_str(&fixture_text("group_expected.json")).expect("fixture json");

    expected["file"] = actual["file"].clone();
    assert_eq!(actual, expected);
}

#[test]
fn split_prs_library_core_auto_group_records_are_deterministic() {
    let plan = parsed_fixture_plan("duck-plan.md");
    let selected = select_sprints_for_scope(&plan, SplitScope::Sprint(2)).expect("scope selection");
    let options = SplitPlanOptions {
        pr_grouping: None,
        default_pr_grouping: Some(SplitPrGrouping::Group),
        strategy: SplitPrStrategy::Auto,
        pr_group_entries: vec![],
        owner_prefix: "subagent".to_string(),
        branch_prefix: "issue".to_string(),
        worktree_prefix: "issue__".to_string(),
    };

    let first = build_split_plan_records(&selected, &options).expect("first run");
    let second = build_split_plan_records(&selected, &options).expect("second run");
    assert_eq!(first, second);
    assert_eq!(first.len(), 3);

    let mut group_by_task: HashMap<String, String> = HashMap::new();
    for record in &first {
        group_by_task.insert(record.task_id.clone(), record.pr_group.clone());
        assert!(
            record.pr_group.starts_with("s2-auto-g"),
            "{}",
            record.pr_group
        );
    }

    assert_eq!(
        group_by_task.get("S2T1"),
        group_by_task.get("S2T2"),
        "same-layer overlap should be grouped"
    );
}

#[test]
fn split_prs_library_core_deterministic_group_requires_mapping() {
    let plan = parsed_fixture_plan("duck-plan.md");
    let selected = select_sprints_for_scope(&plan, SplitScope::Sprint(2)).expect("scope selection");
    let options = SplitPlanOptions {
        pr_grouping: Some(SplitPrGrouping::Group),
        default_pr_grouping: None,
        strategy: SplitPrStrategy::Deterministic,
        pr_group_entries: vec![],
        owner_prefix: "subagent".to_string(),
        branch_prefix: "issue".to_string(),
        worktree_prefix: "issue__".to_string(),
    };

    let err = build_split_plan_records(&selected, &options).expect_err("must reject");
    assert!(
        err.contains("--pr-grouping group requires explicit mapping for every task"),
        "{err}"
    );
}

#[test]
fn split_prs_error_group_requires_mapping() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(&dir.path().join("plan.md"), &fixture_text("duck-plan.md"));

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "2",
            "--pr-grouping",
            "group",
        ],
    );
    assert_eq!(out.code, 2);
    assert!(
        out.stderr
            .contains("--pr-grouping group requires at least one --pr-group"),
        "{}",
        out.stderr
    );
}

#[test]
fn split_prs_error_unknown_mapping_key() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(&dir.path().join("plan.md"), &fixture_text("duck-plan.md"));

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "2",
            "--pr-grouping",
            "group",
            "--pr-group",
            "S2T1=s2-isolated",
            "--pr-group",
            "S2T2=s2-shared",
            "--pr-group",
            "S2T9=s2-shared",
        ],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("unknown task keys"), "{}", out.stderr);
}

#[test]
fn split_prs_auto_group_without_mapping_succeeds() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(&dir.path().join("plan.md"), &fixture_text("duck-plan.md"));

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "2",
            "--strategy",
            "auto",
            "--default-pr-grouping",
            "group",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let value: Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(value["strategy"], "auto");
    assert_eq!(value["pr_grouping"], "group");

    let records = value["records"].as_array().expect("records");
    assert_eq!(records.len(), 3);

    let mut group_by_task: HashMap<String, String> = HashMap::new();
    for record in records {
        let task_id = record["task_id"].as_str().unwrap_or_default().to_string();
        let group = record["pr_group"].as_str().unwrap_or_default().to_string();
        assert!(group.starts_with("s2-auto-g"), "{group}");
        group_by_task.insert(task_id.clone(), group.clone());
        assert!(record["summary"].is_string(), "{}", out.stdout);
    }

    assert_eq!(
        group_by_task.get("S2T1"),
        group_by_task.get("S2T2"),
        "same-layer location overlap should be grouped"
    );
}

#[test]
fn split_prs_auto_group_partial_mapping_preserves_pinned_group() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(&dir.path().join("plan.md"), &fixture_text("duck-plan.md"));

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "2",
            "--strategy",
            "auto",
            "--default-pr-grouping",
            "group",
            "--pr-group",
            "S2T3=manual-docs",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let value: Value = serde_json::from_str(&out.stdout).expect("json");
    let records = value["records"].as_array().expect("records");
    assert_eq!(records.len(), 3);

    let mut group_by_task: HashMap<String, String> = HashMap::new();
    for record in records {
        let task_id = record["task_id"].as_str().unwrap_or_default().to_string();
        let group = record["pr_group"].as_str().unwrap_or_default().to_string();
        group_by_task.insert(task_id.clone(), group);
        assert!(record["summary"].is_string());
    }

    let pinned = group_by_task.get("S2T3").expect("S2T3 group");
    assert_eq!(pinned, "manual-docs");
    let auto_a = group_by_task.get("S2T1").expect("S2T1 group");
    let auto_b = group_by_task.get("S2T2").expect("S2T2 group");
    assert_eq!(auto_a, auto_b, "overlap pair should stay shared");
    assert!(auto_a.starts_with("s2-auto-g"), "{auto_a}");
    assert_ne!(auto_a, pinned);
}

#[test]
fn split_prs_auto_group_rejects_malformed_pin_entry() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(&dir.path().join("plan.md"), &fixture_text("duck-plan.md"));

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "2",
            "--strategy",
            "auto",
            "--default-pr-grouping",
            "group",
            "--pr-group",
            "S2T2",
        ],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("--pr-group must use <task-or-plan-id>=<group> format"),
        "{}",
        out.stderr
    );
}

#[test]
fn split_prs_auto_group_rejects_unknown_pin_key() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(&dir.path().join("plan.md"), &fixture_text("duck-plan.md"));

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "2",
            "--strategy",
            "auto",
            "--default-pr-grouping",
            "group",
            "--pr-group",
            "S2T9=manual-docs",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("unknown task keys"), "{}", out.stderr);
}

#[test]
fn split_prs_auto_requires_metadata_or_default_grouping() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(&dir.path().join("plan.md"), &fixture_text("duck-plan.md"));

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "2",
            "--strategy",
            "auto",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("auto grouping requires `PR grouping intent` metadata"),
        "{}",
        out.stderr
    );
}

#[test]
fn split_prs_auto_rejects_explicit_pr_grouping_flag() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(&dir.path().join("plan.md"), &fixture_text("duck-plan.md"));

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "2",
            "--strategy",
            "auto",
            "--pr-grouping",
            "group",
        ],
    );
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("cannot be used with --strategy auto"),
        "{}",
        out.stderr
    );
}

#[test]
fn split_prs_auto_repeatability_is_byte_stable() {
    for fixture in [
        "duck-plan.md",
        "auto_sparse_plan.md",
        "auto_overlap_plan.md",
        "auto_regression_matrix_plan.md",
    ] {
        let dir = TempDir::new().expect("tempdir");
        common::write_file(&dir.path().join("plan.md"), &fixture_text(fixture));

        let first = common::run_plan_tooling(
            dir.path(),
            &[
                "split-prs",
                "--file",
                "plan.md",
                "--scope",
                "sprint",
                "--sprint",
                "1",
                "--strategy",
                "auto",
                "--default-pr-grouping",
                "group",
                "--format",
                "json",
            ],
        );
        let second = common::run_plan_tooling(
            dir.path(),
            &[
                "split-prs",
                "--file",
                "plan.md",
                "--scope",
                "sprint",
                "--sprint",
                "1",
                "--strategy",
                "auto",
                "--default-pr-grouping",
                "group",
                "--format",
                "json",
            ],
        );

        assert_eq!(
            first.code, 0,
            "first run failed for fixture {fixture}: {}",
            first.stderr
        );
        assert_eq!(
            second.code, 0,
            "second run failed for fixture {fixture}: {}",
            second.stderr
        );
        assert_eq!(
            first.stdout, second.stdout,
            "repeatability drift in {fixture}"
        );
    }
}

#[test]
fn split_prs_auto_matrix_fixture_matches_expected_json() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(
        &dir.path().join("plan.md"),
        &fixture_text("auto_regression_matrix_plan.md"),
    );

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "1",
            "--strategy",
            "auto",
            "--default-pr-grouping",
            "group",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let actual: Value = serde_json::from_str(&out.stdout).expect("json");
    let mut expected: Value =
        serde_json::from_str(&fixture_text("auto_regression_matrix_expected.json"))
            .expect("fixture json");
    expected["file"] = actual["file"].clone();
    assert_eq!(actual, expected);
}

#[test]
fn split_prs_auto_json_contains_required_fields() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(&dir.path().join("plan.md"), &fixture_text("duck-plan.md"));

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "1",
            "--strategy",
            "auto",
            "--default-pr-grouping",
            "per-sprint",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let value: Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(value["scope"], "sprint");
    assert_eq!(value["sprint"], 1);
    assert_eq!(value["pr_grouping"], "per-sprint");
    assert_eq!(value["strategy"], "auto");

    let records = value["records"].as_array().expect("records array");
    assert_eq!(records.len(), 3);
    for record in records {
        assert!(record["task_id"].is_string());
        assert!(record["summary"].is_string());
        assert!(record["pr_group"].is_string());
        assert_eq!(record["pr_group"], "s1");
    }
}

#[test]
fn split_prs_cli_accepts_equals_style_value_flags() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(&dir.path().join("plan.md"), &fixture_text("duck-plan.md"));

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file=plan.md",
            "--scope=sprint",
            "--sprint=2",
            "--strategy=auto",
            "--default-pr-grouping=group",
            "--format=json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let value: Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(value["scope"], "sprint");
    assert_eq!(value["sprint"], 2);
    assert_eq!(value["pr_grouping"], "group");
    assert_eq!(value["strategy"], "auto");
}

#[test]
fn split_prs_auto_explain_json_includes_group_breakdown() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(&dir.path().join("plan.md"), &fixture_text("duck-plan.md"));

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file=plan.md",
            "--scope=sprint",
            "--sprint=2",
            "--strategy=auto",
            "--default-pr-grouping=group",
            "--format=json",
            "--explain",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let value: Value = serde_json::from_str(&out.stdout).expect("json");
    let explain = value["explain"].as_array().expect("explain array");
    assert_eq!(explain.len(), 1);
    assert_eq!(explain[0]["sprint"], 2);
    assert_eq!(
        explain[0]["pr_grouping_intent_source"],
        "default-pr-grouping"
    );
    assert!(explain[0]["groups"].is_array());
    assert!(
        explain[0]["groups"]
            .as_array()
            .expect("groups")
            .iter()
            .all(|entry| entry["task_ids"].is_array()),
        "{}",
        out.stdout
    );
}

#[test]
fn split_prs_auto_uses_execution_profile_parallel_width_as_target() {
    let dir = TempDir::new().expect("tempdir");
    let plan = r#"# Plan: metadata-guided auto split

## Sprint 1: Parallel lane
- **PR grouping intent**: `group` (parallel lanes).
- **Execution Profile**: `parallel-x2` (parallel width 2).

### Task 1.1: API slice A
- **Location**:
  - crates/plan-issue-cli/src/a.rs
- **Dependencies**:
  - none
- **Complexity**: 2

### Task 1.2: API slice B
- **Location**:
  - crates/plan-issue-cli/src/b.rs
- **Dependencies**:
  - none
- **Complexity**: 2

### Task 1.3: API slice C
- **Location**:
  - crates/plan-issue-cli/src/c.rs
- **Dependencies**:
  - none
- **Complexity**: 2

### Task 1.4: API slice D
- **Location**:
  - crates/plan-issue-cli/src/d.rs
- **Dependencies**:
  - none
- **Complexity**: 2

## Sprint 2: Serial lane
- **PR grouping intent**: `group` (single lane).
- **Execution Profile**: `serial` (parallel width 1).

### Task 2.1: Runtime A
- **Location**:
  - crates/plan-issue-cli/src/runtime_a.rs
- **Dependencies**:
  - none
- **Complexity**: 2

### Task 2.2: Runtime B
- **Location**:
  - crates/plan-issue-cli/src/runtime_b.rs
- **Dependencies**:
  - none
- **Complexity**: 2

### Task 2.3: Runtime C
- **Location**:
  - crates/plan-issue-cli/src/runtime_c.rs
- **Dependencies**:
  - none
- **Complexity**: 2
"#;
    common::write_file(&dir.path().join("plan.md"), plan);

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "plan",
            "--strategy",
            "auto",
            "--format",
            "json",
            "--explain",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let value: Value = serde_json::from_str(&out.stdout).expect("json");
    let records = value["records"].as_array().expect("records");
    let mut by_sprint: HashMap<i32, BTreeSet<String>> = HashMap::new();
    for record in records {
        let task_id = record["task_id"].as_str().unwrap_or_default();
        let sprint = task_id
            .strip_prefix('S')
            .and_then(|rest| rest.split('T').next())
            .and_then(|num| num.parse::<i32>().ok())
            .expect("task sprint");
        by_sprint
            .entry(sprint)
            .or_default()
            .insert(record["pr_group"].as_str().unwrap_or_default().to_string());
    }
    assert_eq!(by_sprint.get(&1).map(BTreeSet::len), Some(2));
    assert_eq!(by_sprint.get(&2).map(BTreeSet::len), Some(1));

    let explain = value["explain"].as_array().expect("explain");
    let sprint1 = explain
        .iter()
        .find(|entry| entry["sprint"] == 1)
        .expect("sprint1 explain");
    let sprint2 = explain
        .iter()
        .find(|entry| entry["sprint"] == 2)
        .expect("sprint2 explain");
    assert_eq!(sprint1["target_parallel_width"], 2);
    assert_eq!(sprint2["target_parallel_width"], 1);
    assert_eq!(sprint1["pr_grouping_intent_source"], "plan-metadata");
    assert_eq!(sprint2["pr_grouping_intent_source"], "plan-metadata");
}

#[test]
fn split_prs_auto_uses_same_line_sprint_metadata() {
    let dir = TempDir::new().expect("tempdir");
    let plan = r#"# Plan: same-line metadata split

## Sprint 1: Parallel lane
- **PR grouping intent**: `group` - **Execution Profile**: `parallel-x2` (parallel width 2).

### Task 1.1: API slice A
- **Location**:
  - crates/plan-issue-cli/src/a.rs
- **Dependencies**:
  - none
- **Complexity**: 2

### Task 1.2: API slice B
- **Location**:
  - crates/plan-issue-cli/src/b.rs
- **Dependencies**:
  - none
- **Complexity**: 2
"#;
    common::write_file(&dir.path().join("plan.md"), plan);

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "1",
            "--strategy",
            "auto",
            "--format",
            "json",
            "--explain",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let value: Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(value["strategy"], "auto");
    assert_eq!(value["pr_grouping"], "group");
    assert_eq!(value["explain"][0]["target_parallel_width"], 2);
    assert_eq!(
        value["explain"][0]["pr_grouping_intent_source"],
        "plan-metadata"
    );
}

#[test]
fn split_prs_non_regression_auto_sparse_plan_scaffold() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(
        &dir.path().join("plan.md"),
        &fixture_text("auto_sparse_plan.md"),
    );

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "1",
            "--strategy",
            "auto",
            "--default-pr-grouping",
            "group",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let value: Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(value["strategy"], "auto");
    assert_eq!(value["pr_grouping"], "group");

    let records = value["records"].as_array().expect("records");
    assert_eq!(records.len(), 2);

    for record in records {
        let group = record["pr_group"].as_str().unwrap_or_default();
        assert!(group.starts_with("s1-auto-g"), "{group}");
    }
}

#[test]
fn split_prs_non_regression_auto_overlap_heavy_plan_scaffold() {
    let dir = TempDir::new().expect("tempdir");
    common::write_file(
        &dir.path().join("plan.md"),
        &fixture_text("auto_overlap_plan.md"),
    );

    let out = common::run_plan_tooling(
        dir.path(),
        &[
            "split-prs",
            "--file",
            "plan.md",
            "--scope",
            "sprint",
            "--sprint",
            "1",
            "--strategy",
            "auto",
            "--default-pr-grouping",
            "group",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let value: Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(value["strategy"], "auto");
    assert_eq!(value["pr_grouping"], "group");

    let records = value["records"].as_array().expect("records");
    assert_eq!(records.len(), 3);

    let mut unique_groups = BTreeSet::new();
    for record in records {
        let group = record["pr_group"].as_str().unwrap_or_default();
        unique_groups.insert(group.to_string());
        assert!(group.starts_with("s1-auto-g"), "{group}");
    }

    assert_eq!(unique_groups.len(), 1);
}

#[test]
fn split_prs_fixture_tsv_header_is_stable() {
    for file in ["per_sprint_expected.tsv", "group_expected.tsv"] {
        let path = fixture_path(file);
        let text = fs::read_to_string(path).expect("fixture exists");
        let first = text.lines().next().unwrap_or_default();
        assert_eq!(first, "# task_id\tsummary\tpr_group");
    }
}

#[test]
fn split_prs_non_regression_fixture_tsv_schema_matches_reduced_output() {
    for row in tsv_rows("per_sprint_expected.tsv") {
        assert_eq!(row.len(), 3);
        assert_eq!(row[2], "s1");
    }

    let group_rows = tsv_rows("group_expected.tsv");
    let mut members_by_group: HashMap<String, Vec<String>> = HashMap::new();
    for row in &group_rows {
        assert_eq!(row.len(), 3);
        members_by_group
            .entry(row[2].clone())
            .or_default()
            .push(row[0].clone());
    }

    for row in &group_rows {
        let pr_group = &row[2];
        let members = members_by_group.get(pr_group).expect("group members");
        assert!(!members.is_empty());
    }
}

#[test]
fn split_prs_non_regression_group_fixture_preserves_expected_shared_membership() {
    for row in tsv_rows("group_expected.tsv") {
        assert_eq!(row.len(), 3);
        let task_id = &row[0];
        if task_id == "S2T1" {
            assert_eq!(&row[2], "s2-isolated");
        } else {
            assert_eq!(&row[2], "s2-shared");
        }
    }
}

#[test]
fn split_prs_fixture_json_contains_required_fields() {
    for file in ["per_sprint_expected.json", "group_expected.json"] {
        let path = fixture_path(file);
        let text = fs::read_to_string(path).expect("fixture exists");
        let value: Value = serde_json::from_str(&text).expect("valid json");

        assert!(value["file"].is_string() || value["file"].is_null());
        assert!(value["scope"].is_string());
        assert!(value["pr_grouping"].is_string());
        assert!(value["strategy"].is_string());
        let records = value["records"].as_array().expect("records array");
        assert!(!records.is_empty());

        for record in records {
            assert!(record["task_id"].is_string());
            assert!(record["summary"].is_string());
            assert!(record["pr_group"].is_string());
        }
    }
}

#[test]
fn split_prs_error_matrix_doc_mentions_core_cases() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("specs")
        .join("split-prs-contract-v1.md");
    let text = fs::read_to_string(path).expect("spec exists");

    for token in [
        "unknown mapping key",
        "missing mapping",
        "--pr-grouping",
        "scope=sprint",
    ] {
        assert!(
            text.contains(token),
            "expected token in error matrix: {token}"
        );
    }
}

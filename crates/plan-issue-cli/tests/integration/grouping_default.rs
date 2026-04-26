// Task 1.2: defaulting `--strategy` and `--default-pr-grouping` from the
// plan markdown's per-sprint `pr-grouping` metadata.

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

use crate::common;

fn parse_json(stdout: &str) -> Value {
    serde_json::from_str(stdout).expect("stdout should be valid JSON")
}

/// Plan declares `PR grouping intent: per-sprint` for sprint 1 — start-sprint
/// invoked without any grouping flag should infer `--strategy=auto` and
/// `--default-pr-grouping=per-sprint`, succeed, and emit the resolved hint to
/// stderr.
#[test]
fn start_sprint_grouping_default_inferred_when_plan_only() {
    let tmp = TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let plan_path = tmp.path().join("plan-grouping-per-sprint.md");
    fs::write(
        &plan_path,
        r#"# Plan: Grouping inference per-sprint

## Overview
Plan that declares per-sprint pr-grouping intent so plan-issue can default
`--strategy` and `--default-pr-grouping` without operator flags.

## Sprint 1: Inferred per-sprint
**PR grouping intent**: per-sprint
**Execution Profile**: serial

### Task 1.1: First task
- **Location**:
  - crates/plan-issue-cli/src/lib.rs
- **Description**: Single-task sprint demonstrating per-sprint inference.
- **Dependencies**:
  - none
- **Complexity**: 1
- **Acceptance criteria**:
  - start-sprint succeeds without explicit grouping flags.
- **Validation**:
  - cargo test -p nils-plan-issue-cli start_sprint_grouping_default_inferred_when_plan_only -- --exact
"#,
    )
    .expect("write plan fixture");

    let plan_s = plan_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            "graysurf/plan-issue-smoke",
            "start-sprint",
            "--plan",
            &plan_s,
            "--issue",
            "217",
            "--sprint",
            "1",
            "--no-comment",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains(
            "inferred --strategy=auto; --default-pr-grouping=per-sprint from plan sprint S1"
        ),
        "stderr should contain the inferred-defaults hint: {:?}",
        out.stderr
    );

    let payload = parse_json(&out.stdout);
    assert_eq!(
        payload["payload"]["result"]["inferred_grouping_defaults"], "auto/per-sprint",
        "result should record the inferred grouping defaults"
    );
}

/// Plan declares `PR grouping intent: group` for sprint 2 — inference picks
/// up `--default-pr-grouping=group` and tasks share a single auto group.
#[test]
fn start_sprint_grouping_default_inferred_for_group_intent() {
    let tmp = TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let plan_path = tmp.path().join("plan-grouping-group.md");
    fs::write(
        &plan_path,
        r#"# Plan: Grouping inference group

## Overview
Two-task sprint with `PR grouping intent: group`. plan-issue should infer
auto/group without explicit flags and produce a single combined PR group.

## Sprint 1: Setup
**PR grouping intent**: per-sprint
**Execution Profile**: serial

### Task 1.1: Setup task
- **Location**:
  - crates/plan-issue-cli/src/lib.rs
- **Description**: Setup task.
- **Dependencies**:
  - none
- **Complexity**: 1
- **Acceptance criteria**:
  - Setup completes.
- **Validation**:
  - cargo test -p nils-plan-issue-cli start_sprint_grouping_default_inferred_for_group_intent -- --exact

## Sprint 2: Inferred group
**PR grouping intent**: group
**Execution Profile**: serial

### Task 2.1: Group task A
- **Location**:
  - crates/plan-issue-cli/src/lib.rs
- **Description**: First task that should share a PR group with task 2.2.
- **Dependencies**:
  - none
- **Complexity**: 1
- **Acceptance criteria**:
  - Group inferred.
- **Validation**:
  - cargo test -p nils-plan-issue-cli start_sprint_grouping_default_inferred_for_group_intent -- --exact

### Task 2.2: Group task B
- **Location**:
  - crates/plan-issue-cli/src/lib.rs
- **Description**: Second task that should share a PR group with task 2.1.
- **Dependencies**:
  - Task 2.1
- **Complexity**: 1
- **Acceptance criteria**:
  - Group inferred.
- **Validation**:
  - cargo test -p nils-plan-issue-cli start_sprint_grouping_default_inferred_for_group_intent -- --exact
"#,
    )
    .expect("write plan fixture");

    let plan_s = plan_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            "graysurf/plan-issue-smoke",
            "start-sprint",
            "--plan",
            &plan_s,
            "--issue",
            "217",
            "--sprint",
            "2",
            "--no-comment",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr
            .contains("inferred --strategy=auto; --default-pr-grouping=group from plan sprint S2"),
        "stderr should contain the inferred-defaults hint: {:?}",
        out.stderr
    );

    let payload = parse_json(&out.stdout);
    let pr_groups = payload["payload"]["result"]["pr_groups"]
        .as_array()
        .expect("pr_groups");
    assert_eq!(
        pr_groups.len(),
        1,
        "two-task sprint with group intent should collapse into one PR group: {pr_groups:?}"
    );
    let task_ids = pr_groups[0]["task_ids"].as_array().expect("task_ids array");
    assert_eq!(task_ids.len(), 2, "{pr_groups:?}");
}

/// Operator passes explicit `--strategy=auto --default-pr-grouping=per-sprint`
/// — that should win over plan metadata, and no inference hint should fire.
#[test]
fn start_sprint_grouping_cli_overrides_plan_metadata() {
    let tmp = TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let plan_path = tmp.path().join("plan-grouping-override.md");
    fs::write(
        &plan_path,
        r#"# Plan: CLI flags override plan metadata

## Overview
Plan declares `group` intent for sprint 1 but the operator passes
`--default-pr-grouping=per-sprint` explicitly; CLI must win.

## Sprint 1: Group intent
**PR grouping intent**: group
**Execution Profile**: serial

### Task 1.1: Solo task
- **Location**:
  - crates/plan-issue-cli/src/lib.rs
- **Description**: Solo task; per-sprint override gives one row per sprint.
- **Dependencies**:
  - none
- **Complexity**: 1
- **Acceptance criteria**:
  - CLI wins.
- **Validation**:
  - cargo test -p nils-plan-issue-cli start_sprint_grouping_cli_overrides_plan_metadata -- --exact
"#,
    )
    .expect("write plan fixture");

    let plan_s = plan_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            "graysurf/plan-issue-smoke",
            "start-sprint",
            "--plan",
            &plan_s,
            "--issue",
            "217",
            "--sprint",
            "1",
            "--strategy",
            "auto",
            "--default-pr-grouping",
            "per-sprint",
            "--no-comment",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        !out.stderr.contains("inferred --strategy"),
        "no inference hint expected when CLI overrides: {:?}",
        out.stderr
    );

    let payload = parse_json(&out.stdout);
    assert!(
        payload["payload"]["result"]["inferred_grouping_defaults"].is_null(),
        "result.inferred_grouping_defaults must be null when CLI flags override: {}",
        payload
    );
}

/// Plan with no `pr-grouping` metadata still requires explicit grouping
/// flags; absent both, validation falls back to the original error.
#[test]
fn start_sprint_grouping_silent_plan_still_requires_explicit_flags() {
    let tmp = TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    fs::create_dir_all(&agent_home).expect("create agent home");
    common::seed_agent_home_prompts(&agent_home);
    let agent_home_s = agent_home.to_string_lossy().to_string();

    let plan_path = tmp.path().join("plan-grouping-silent.md");
    fs::write(
        &plan_path,
        r#"# Plan: No grouping metadata

## Overview
Plan with a sprint that declares no `pr-grouping`. plan-issue must not
infer; the existing validation should still demand `--pr-grouping`.

## Sprint 1: Silent
### Task 1.1: Sole task
- **Location**:
  - crates/plan-issue-cli/src/lib.rs
- **Description**: Plan declares no grouping intent so default validation runs.
- **Dependencies**:
  - none
- **Complexity**: 1
- **Acceptance criteria**:
  - Validation surfaces invalid-pr-grouping.
- **Validation**:
  - cargo test -p nils-plan-issue-cli start_sprint_grouping_silent_plan_still_requires_explicit_flags -- --exact
"#,
    )
    .expect("write plan fixture");

    let plan_s = plan_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_local_with_env(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            "graysurf/plan-issue-smoke",
            "start-sprint",
            "--plan",
            &plan_s,
            "--issue",
            "217",
            "--sprint",
            "1",
            "--no-comment",
        ],
        &[("AGENT_HOME", &agent_home_s)],
    );

    assert_eq!(
        out.code, 1,
        "expected failure when plan is silent and CLI passes no flags"
    );
    let combined = format!("{}{}", out.stdout, out.stderr);
    assert!(
        combined.contains("--strategy deterministic requires --pr-grouping"),
        "should still surface the standard grouping requirement; stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
}

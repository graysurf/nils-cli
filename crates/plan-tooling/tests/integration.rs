// Consolidated integration test target.
// Each former `tests/*.rs` is declared as a submodule here so the crate
// links one integration test binary instead of many. This keeps the
// dev-loop link phase O(crates) instead of O(test-files).

#[path = "integration/artifact_audit.rs"]
mod artifact_audit;
#[path = "integration/batches.rs"]
mod batches;
#[path = "integration/common.rs"]
pub mod common;
#[path = "integration/completion_outside_repo.rs"]
mod completion_outside_repo;
#[path = "integration/scaffold.rs"]
mod scaffold;
#[path = "integration/split_prs.rs"]
mod split_prs;
#[path = "integration/to_json.rs"]
mod to_json;
#[path = "integration/validate.rs"]
mod validate;

#[test]
fn validate() {
    let repo = common::init_repo();
    common::write_file(&repo.path().join("plan.md"), VALID_PLAN);

    let out = common::run_plan_tooling(repo.path(), &["validate", "--file", "plan.md"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
}

#[test]
fn validate_fails_without_direct_source_doc_waiver() {
    let repo = common::init_repo();
    write_valid_bundle_source_and_plan(repo.path(), "demo");
    common::write_file(
        &repo.path().join("docs/plans/demo/demo-execution-state.md"),
        r#"# Execution State: Demo

- Status: in progress
- Source document:
  `docs/plans/demo/demo-discussion-source.md`
- Direct source-doc execution waiver: not applicable
"#,
    );

    let out = common::run_plan_tooling(
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
    let repo = common::init_repo();
    write_valid_bundle_source_and_plan(repo.path(), "demo");
    common::write_file(
        &repo.path().join("docs/plans/demo/demo-execution-state.md"),
        r#"# Execution State: Demo

- Status: in progress
- Source document:
  `docs/plans/demo/demo-discussion-source.md`
- Direct source-doc execution waiver: bounded single-step source execution
"#,
    );

    let out = common::run_plan_tooling(
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
fn artifact_audit_classifies_completed_bundle() {
    let repo = common::init_repo();
    write_completed_bundle(repo.path());

    let out = common::run_plan_tooling(
        repo.path(),
        &[
            "artifact-audit",
            "--candidate",
            "docs/plans/demo/demo-plan.md",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["items"][0]["classification"], "delete");
}

#[test]
fn artifact_audit_keeps_referenced_bundle() {
    let repo = common::init_repo();
    write_completed_bundle(repo.path());
    common::write_file(
        &repo.path().join("docs/retained.md"),
        "This still links to docs/plans/demo/demo-plan.md.\n",
    );

    let out = common::run_plan_tooling(
        repo.path(),
        &[
            "artifact-audit",
            "--candidate",
            "docs/plans/demo/demo-plan.md",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["items"][0]["classification"], "keep");
}

#[test]
fn artifact_audit_manual_reviews_retained_evidence() {
    let repo = common::init_repo();
    common::write_file(
        &repo.path().join("out/tests/run.log"),
        "raw retained output\n",
    );

    let out = common::run_plan_tooling(
        repo.path(),
        &[
            "artifact-audit",
            "--candidate",
            "out/tests/run.log",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["items"][0]["classification"], "manual-review");
}

fn write_valid_bundle_source_and_plan(repo: &std::path::Path, slug: &str) {
    common::write_file(
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
    common::write_file(
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

fn write_completed_bundle(repo: &std::path::Path) {
    write_valid_bundle_source_and_plan(repo, "demo");
    common::write_file(
        &repo.join("docs/plans/demo/demo-execution-state.md"),
        r#"# Execution State: Demo

- Status: complete
- Source document:
  `docs/plans/demo/demo-plan.md`
- Direct source-doc execution waiver: not applicable
"#,
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

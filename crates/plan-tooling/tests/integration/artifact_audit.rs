use crate::common::{init_repo, run_plan_tooling, write_file};

use pretty_assertions::assert_eq;

#[test]
fn artifact_audit_usage() {
    let repo = init_repo();
    let out = run_plan_tooling(repo.path(), &["artifact-audit", "--help"]);
    assert_eq!(out.code, 0);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.contains("delete"));
    assert!(out.stderr.contains("keep"));
    assert!(out.stderr.contains("rehome"));
    assert!(out.stderr.contains("manual-review"));
    assert!(!out.stderr.contains("--execute"));
}

#[test]
fn artifact_audit_classifies_completed_bundle() {
    let repo = init_repo();
    write_completed_bundle(repo.path());

    let out = run_plan_tooling(
        repo.path(),
        &[
            "artifact-audit",
            "--candidate",
            "docs/plans/demo/demo-plan.md",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stderr.is_empty());

    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["schema_version"], "plan-tooling.artifact-audit.v1");
    assert_eq!(v["ok"], true);
    assert_eq!(v["items"][0]["classification"], "delete");
    assert!(
        v["items"][0]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("completed sibling bundle")
    );
}

#[test]
fn artifact_audit_keeps_referenced_bundle() {
    let repo = init_repo();
    write_completed_bundle(repo.path());
    write_file(
        &repo.path().join("docs/retained.md"),
        "This still links to docs/plans/demo/demo-plan.md.\n",
    );

    let out = run_plan_tooling(
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
    assert_eq!(
        v["items"][0]["blocking_references"],
        serde_json::json!(["docs/retained.md"])
    );
}

#[test]
fn artifact_audit_manual_reviews_retained_evidence() {
    let repo = init_repo();
    write_file(
        &repo.path().join("out/tests/run.log"),
        "raw retained output\n",
    );

    let out = run_plan_tooling(
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
    assert!(
        v["items"][0]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("manual retention policy")
    );
}

#[test]
fn artifact_audit_rehomes_retained_content() {
    let repo = init_repo();
    write_file(
        &repo
            .path()
            .join("docs/plans/demo/demo-discussion-source.md"),
        "- Status: complete\n- Retained content: reusable policy text\n",
    );

    let out = run_plan_tooling(
        repo.path(),
        &[
            "artifact-audit",
            "--candidate",
            "docs/plans/demo/demo-discussion-source.md",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["items"][0]["classification"], "rehome");
}

fn write_completed_bundle(repo: &std::path::Path) {
    write_file(
        &repo.join("docs/plans/demo/demo-discussion-source.md"),
        r#"# Demo Source

- Recommended plan:
  `docs/plans/demo/demo-plan.md`
- Recommended execution state:
  `docs/plans/demo/demo-execution-state.md`
"#,
    );
    write_file(
        &repo.join("docs/plans/demo/demo-plan.md"),
        r#"# Plan: Demo

## Read First

- Primary source: docs/plans/demo/demo-discussion-source.md
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
"#,
    );
    write_file(
        &repo.join("docs/plans/demo/demo-execution-state.md"),
        r#"# Execution State: Demo

- Status: complete
- Source document:
  `docs/plans/demo/demo-plan.md`
- Direct source-doc execution waiver: not applicable
"#,
    );
}

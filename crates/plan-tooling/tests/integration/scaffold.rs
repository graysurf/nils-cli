use crate::common;
use common::{run_plan_tooling, write_file};

use pretty_assertions::assert_eq;

#[test]
fn scaffold_slug_creates_plan_and_replaces_title() {
    let dir = tempfile::TempDir::new().expect("tempdir");

    let out = run_plan_tooling(
        dir.path(),
        &[
            "scaffold",
            "--slug",
            "plan-tooling-cli-consolidation-test",
            "--title",
            "Test plan",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("created: docs/plans/plan-tooling-cli-consolidation-test/plan-tooling-cli-consolidation-test-plan.md")
    );

    let created_path = dir
        .path()
        .join("docs/plans/plan-tooling-cli-consolidation-test/plan-tooling-cli-consolidation-test-plan.md");
    let created = std::fs::read_to_string(created_path).expect("read created");
    assert!(
        created
            .lines()
            .next()
            .unwrap_or("")
            .contains("# Plan: Test plan")
    );
}

#[test]
fn scaffold_existing_without_force_is_error() {
    let dir = tempfile::TempDir::new().expect("tempdir");

    write_file(
        &dir.path().join("docs/plans/test-plan.md"),
        "# Plan: Existing\n",
    );

    let out = run_plan_tooling(
        dir.path(),
        &[
            "scaffold",
            "--file",
            "docs/plans/test-plan.md",
            "--title",
            "X",
        ],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("output already exists"));
}

#[test]
fn scaffold_existing_with_force_overwrites() {
    let dir = tempfile::TempDir::new().expect("tempdir");

    write_file(
        &dir.path().join("docs/plans/test-plan.md"),
        "# Plan: Existing\n",
    );

    let out = run_plan_tooling(
        dir.path(),
        &[
            "scaffold",
            "--file",
            "docs/plans/test-plan.md",
            "--title",
            "New",
            "--force",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let created =
        std::fs::read_to_string(dir.path().join("docs/plans/test-plan.md")).expect("read");
    assert!(created.lines().next().unwrap_or("").contains("# Plan: New"));
}

#[test]
fn scaffold_invalid_slug_is_usage_error() {
    let dir = tempfile::TempDir::new().expect("tempdir");

    let out = run_plan_tooling(
        dir.path(),
        &["scaffold", "--slug", "Not-Kebab-Case", "--title", "X"],
    );
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("--slug must be kebab-case"));
    assert!(out.stderr.contains("Usage:"));
}

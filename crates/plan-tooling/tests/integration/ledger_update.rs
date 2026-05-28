use std::path::Path;

use super::common;

const LEDGER_FIXTURE: &str = "# Demo Execution State

## Execution State

- Status: in-progress
- Source document: docs/plans/demo/demo-plan.md

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | pending | Implement `ledger-update` |  | first row |
| 1.2 | pending | Implement `ledger-sync` |  | second row |
| 2.1 | pending | Release tag |  |  |

## Notes

- trailing section preserved verbatim
";

fn write_ledger(repo: &Path) {
    common::write_file(
        &repo.join("docs/plans/demo/demo-execution-state.md"),
        LEDGER_FIXTURE,
    );
}

#[test]
fn ledger_update_patches_row_in_place() {
    let repo = common::init_repo();
    write_ledger(repo.path());

    let out = common::run_plan_tooling(
        repo.path(),
        &[
            "ledger-update",
            "--execution-state",
            "docs/plans/demo/demo-execution-state.md",
            "--task",
            "1.1",
            "--status",
            "done",
            "--evidence",
            "PR #999 squash deadbeef",
            "--format",
            "json",
        ],
    );

    assert_eq!(
        out.code, 0,
        "stderr: {}\nstdout: {}",
        out.stderr, out.stdout
    );
    let value: serde_json::Value = serde_json::from_str(&out.stdout).expect("json output parses");
    assert_eq!(value["ok"], serde_json::Value::Bool(true));
    assert_eq!(value["operation"], "ledger-update");
    assert_eq!(value["task_id"], "1.1");
    assert_eq!(value["status"]["previous"], "pending");
    assert_eq!(value["status"]["new"], "done");
    assert_eq!(value["evidence"]["new"], "PR #999 squash deadbeef");
    assert_eq!(value["file_changed"], serde_json::Value::Bool(true));

    let new_text =
        std::fs::read_to_string(repo.path().join("docs/plans/demo/demo-execution-state.md"))
            .expect("read after patch");
    assert!(new_text.contains(
        "| 1.1 | done | Implement `ledger-update` | PR #999 squash deadbeef | first row |"
    ));
    assert!(new_text.contains("| 1.2 | pending | Implement `ledger-sync` |  | second row |"));
    assert!(new_text.contains("## Notes"));
}

#[test]
fn ledger_update_dry_run_does_not_write() {
    let repo = common::init_repo();
    write_ledger(repo.path());

    let before =
        std::fs::read_to_string(repo.path().join("docs/plans/demo/demo-execution-state.md"))
            .expect("read before");

    let out = common::run_plan_tooling(
        repo.path(),
        &[
            "ledger-update",
            "--execution-state",
            "docs/plans/demo/demo-execution-state.md",
            "--task",
            "1.1",
            "--status",
            "done",
            "--evidence",
            "PR #1",
            "--dry-run",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let value: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(value["dry_run"], serde_json::Value::Bool(true));
    assert_eq!(value["file_changed"], serde_json::Value::Bool(true));

    let after =
        std::fs::read_to_string(repo.path().join("docs/plans/demo/demo-execution-state.md"))
            .expect("read after");
    assert_eq!(before, after, "dry-run must not modify the file");
}

#[test]
fn ledger_update_row_not_found_returns_stable_code() {
    let repo = common::init_repo();
    write_ledger(repo.path());

    let out = common::run_plan_tooling(
        repo.path(),
        &[
            "ledger-update",
            "--execution-state",
            "docs/plans/demo/demo-execution-state.md",
            "--task",
            "99.9",
            "--status",
            "done",
            "--evidence",
            "PR #1",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 1, "stderr: {}", out.stderr);
    let value: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(value["ok"], serde_json::Value::Bool(false));
    assert_eq!(value["error"]["code"], "ledger-row-not-found");
}

#[test]
fn ledger_update_appends_to_existing_evidence() {
    let repo = common::init_repo();
    let prefilled = LEDGER_FIXTURE.replace(
        "| 1.1 | pending | Implement `ledger-update` |  | first row |",
        "| 1.1 | in-progress | Implement `ledger-update` | issue#146 | first row |",
    );
    common::write_file(
        &repo.path().join("docs/plans/demo/demo-execution-state.md"),
        &prefilled,
    );

    let out = common::run_plan_tooling(
        repo.path(),
        &[
            "ledger-update",
            "--execution-state",
            "docs/plans/demo/demo-execution-state.md",
            "--task",
            "1.1",
            "--status",
            "done",
            "--evidence",
            "PR #999",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let value: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(value["evidence"]["previous"], "issue#146");
    assert_eq!(value["evidence"]["new"], "issue#146; PR #999");
    assert_eq!(value["evidence"]["appended"], serde_json::Value::Bool(true));
}

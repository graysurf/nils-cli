//! Byte-equality golden tests for render.rs Markdown emitters.
//!
//! Covers `render::render_plan_issue_body` (3 representative inputs)
//! and `render::render_sprint_comment` (Start / Ready / Accepted
//! modes). Each scenario builds inputs from in-test data + a
//! tempfile-backed plan stub and asserts byte equality against a
//! committed fixture under `tests/golden/render/`.
//!
//! Set `BLESS_RENDER_GOLDEN=1` to overwrite the fixtures from the
//! current renderer output instead of asserting.

use std::path::{Path, PathBuf};

use plan_issue::commands::{PrGrouping, SplitStrategy};
use plan_issue::render::{
    SprintCommentInput, SprintCommentMode, render_plan_issue_body, render_sprint_comment,
};
use plan_issue::task_spec::TaskSpecRow;
use tempfile::TempDir;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join("render")
        .join(name)
}

fn assert_or_bless(name: &str, actual: &str) {
    let path = fixture_path(name);
    if std::env::var_os("BLESS_RENDER_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir fixture dir");
        std::fs::write(&path, actual).expect("write fixture");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()));
    pretty_assertions::assert_eq!(expected, actual, "golden mismatch for {name}");
}

fn write_plan_file(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write plan file");
    path
}

#[allow(clippy::too_many_arguments)]
fn row(
    task_id: &str,
    summary: &str,
    branch: &str,
    worktree: &str,
    owner: &str,
    notes: &str,
    pr_group: &str,
    sprint: i32,
    grouping: PrGrouping,
) -> TaskSpecRow {
    TaskSpecRow {
        task_id: task_id.into(),
        summary: summary.into(),
        branch: branch.into(),
        worktree: worktree.into(),
        owner: owner.into(),
        notes: notes.into(),
        pr_group: pr_group.into(),
        sprint,
        grouping,
    }
}

// --- render_plan_issue_body scenarios ----------------------------------

#[test]
fn plan_issue_body_no_rows_matches_golden() {
    let tmp = TempDir::new().expect("tempdir");
    // Empty plan file -> fallback title.
    let plan_path = write_plan_file(tmp.path(), "empty-plan.md", "");
    let out = render_plan_issue_body(
        &plan_path,
        "docs/plans/example/example-plan.md",
        "",
        &[],
        SplitStrategy::Deterministic,
    );
    assert_or_bless("plan_issue_body_no_rows.md", &out);
}

#[test]
fn plan_issue_body_single_row_matches_golden() {
    let tmp = TempDir::new().expect("tempdir");
    let plan_path = write_plan_file(
        tmp.path(),
        "single-plan.md",
        "# Example Plan\n\nIntro paragraph.\n\n## Sprint 1: First sprint\n\nbody\n",
    );
    let rows = vec![row(
        "1.1",
        "Add feature foo",
        "issue/1-1",
        "issue-1-1",
        "subagent-alpha",
        "Initial note",
        "1",
        1,
        PrGrouping::Group,
    )];
    let out = render_plan_issue_body(
        &plan_path,
        "docs/plans/example/example-plan.md",
        "Example Plan",
        &rows,
        SplitStrategy::Deterministic,
    );
    assert_or_bless("plan_issue_body_single_row.md", &out);
}

#[test]
fn plan_issue_body_multi_rows_matches_golden() {
    let tmp = TempDir::new().expect("tempdir");
    let plan_path = write_plan_file(
        tmp.path(),
        "multi-plan.md",
        "# Multi Sprint Plan\n\nOverview line.\n\n## Sprint 1: Setup\n\nfoo\n\n## Sprint 2: Build\n\nbar\n",
    );
    let rows = vec![
        row(
            "1.1",
            "Bootstrap repo",
            "issue/1-1",
            "issue-1-1",
            "subagent-alpha",
            "-",
            "1",
            1,
            PrGrouping::Group,
        ),
        row(
            "1.2",
            "Add ci",
            "issue/1-2",
            "issue-1-2",
            "subagent-bravo",
            "depends on 1.1",
            "1",
            1,
            PrGrouping::Group,
        ),
        row(
            "2.1",
            "Pipe|in|summary",
            "issue/2-1",
            "issue-2-1",
            "subagent-charlie",
            "Multi line\nnote",
            "2",
            2,
            PrGrouping::PerSprint,
        ),
    ];
    let out = render_plan_issue_body(
        &plan_path,
        "docs/plans/multi/multi-plan.md",
        "Multi Sprint Plan",
        &rows,
        SplitStrategy::Deterministic,
    );
    assert_or_bless("plan_issue_body_multi_rows.md", &out);
}

// --- render_sprint_comment scenarios -----------------------------------

fn sprint_comment_rows() -> Vec<TaskSpecRow> {
    vec![
        row(
            "1.1",
            "Bootstrap repo",
            "issue/1-1",
            "issue-1-1",
            "subagent-alpha",
            "-",
            "1",
            1,
            PrGrouping::Group,
        ),
        row(
            "1.2",
            "Add ci",
            "issue/1-2",
            "issue-1-2",
            "subagent-bravo",
            "-",
            "1",
            1,
            PrGrouping::Group,
        ),
    ]
}

fn sprint_comment_plan_file(dir: &Path) -> PathBuf {
    write_plan_file(
        dir,
        "sprint-plan.md",
        "# Sprint Plan\n\nIntro.\n\n## Sprint 1: First sprint\n\nGoals: do the thing.\n\n### Task 1.1: Bootstrap repo\n\n- Bootstrap.\n\n## Sprint 2: Second sprint\n\nMore.\n",
    )
}

#[test]
fn sprint_comment_start_matches_golden() {
    let tmp = TempDir::new().expect("tempdir");
    let plan_path = sprint_comment_plan_file(tmp.path());
    let rows = sprint_comment_rows();
    let out = render_sprint_comment(SprintCommentInput {
        mode: SprintCommentMode::Start,
        plan_file: &plan_path,
        sprint: 1,
        sprint_name: "First sprint",
        rows: &rows,
        strategy: SplitStrategy::Deterministic,
        note_text: None,
        approval_comment_url: None,
        issue_body_text: None,
    })
    .expect("render");
    assert_or_bless("sprint_comment_start.md", &out);
}

#[test]
fn sprint_comment_ready_matches_golden() {
    let tmp = TempDir::new().expect("tempdir");
    let plan_path = sprint_comment_plan_file(tmp.path());
    let rows = sprint_comment_rows();
    let out = render_sprint_comment(SprintCommentInput {
        mode: SprintCommentMode::Ready,
        plan_file: &plan_path,
        sprint: 1,
        sprint_name: "First sprint",
        rows: &rows,
        strategy: SplitStrategy::Deterministic,
        note_text: None,
        approval_comment_url: Some("https://example.test/approval/1"),
        issue_body_text: None,
    })
    .expect("render");
    assert_or_bless("sprint_comment_ready.md", &out);
}

#[test]
fn sprint_comment_accepted_matches_golden() {
    let tmp = TempDir::new().expect("tempdir");
    let plan_path = sprint_comment_plan_file(tmp.path());
    let rows = sprint_comment_rows();
    let issue_body = "## Task Decomposition\n\n| Task | PR |\n| --- | --- |\n| 1.1 | #101 |\n| 1.2 | https://github.com/foo/bar/pull/102 |\n";
    let out = render_sprint_comment(SprintCommentInput {
        mode: SprintCommentMode::Accepted,
        plan_file: &plan_path,
        sprint: 1,
        sprint_name: "First sprint",
        rows: &rows,
        strategy: SplitStrategy::Deterministic,
        note_text: Some("All sprint 1 tasks done."),
        approval_comment_url: Some("https://example.test/approval/1"),
        issue_body_text: Some(issue_body),
    })
    .expect("render");
    assert_or_bless("sprint_comment_accepted.md", &out);
}

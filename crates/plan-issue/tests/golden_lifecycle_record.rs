//! Byte-equality golden tests for `lifecycle_record` public renderers:
//! dashboards, snapshot comments, and per-kind post comments.
//!
//! Set `BLESS_LIFECYCLE_RECORD_GOLDEN=1` to overwrite fixtures from
//! the current renderer output instead of asserting.

use std::path::PathBuf;

use plan_issue::commands::record::{LifecycleCommentKind, RecordProfile};
use plan_issue::lifecycle_record::{
    DashboardInput, SnapshotData, render_dashboard, render_record_post_comment,
    render_record_snapshot_comment,
};
use serde_json::json;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join("lifecycle_record")
        .join(name)
}

fn assert_or_bless(name: &str, actual: &str) {
    let path = fixture_path(name);
    if std::env::var_os("BLESS_LIFECYCLE_RECORD_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir fixture dir");
        std::fs::write(&path, actual).expect("write fixture");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()));
    pretty_assertions::assert_eq!(expected, actual, "golden mismatch for {name}");
}

fn dashboard_input_pending() -> DashboardInput {
    DashboardInput {
        profile: RecordProfile::Tracking,
        status: "in-progress".to_string(),
        target_scope: "sympoies/nils-cli#541".to_string(),
        current: "Sprint 2 Task 2.5".to_string(),
        next_action: "Run validation".to_string(),
        validation: "pending".to_string(),
        linked_prs: vec![],
        blockers: vec![],
        approval: "pending".to_string(),
        source_url: None,
        plan_url: None,
        state_url: None,
        session_url: None,
        validation_url: None,
        review_url: None,
        closeout_url: None,
        title: None,
        issue_url: None,
    }
}

fn dashboard_input_full() -> DashboardInput {
    DashboardInput {
        profile: RecordProfile::Tracking,
        status: "in-progress".to_string(),
        target_scope: "sympoies/nils-cli#541".to_string(),
        current: "Task 2.5".to_string(),
        next_action: "Open PR".to_string(),
        validation: "partial".to_string(),
        linked_prs: vec![
            "https://github.com/foo/bar/pull/1".to_string(),
            "https://github.com/foo/bar/pull/2".to_string(),
        ],
        blockers: vec!["forge-cli pending fix".to_string()],
        approval: "pending".to_string(),
        source_url: Some("https://example.test/source".to_string()),
        plan_url: Some("https://example.test/plan".to_string()),
        state_url: Some("https://example.test/state".to_string()),
        session_url: Some("https://example.test/session".to_string()),
        validation_url: Some("https://example.test/validation".to_string()),
        review_url: Some("https://example.test/review".to_string()),
        closeout_url: None,
        title: Some("Original title".to_string()),
        issue_url: Some("https://github.com/foo/bar/issues/100".to_string()),
    }
}

fn dashboard_input_complete() -> DashboardInput {
    DashboardInput {
        profile: RecordProfile::Tracking,
        status: "complete".to_string(),
        target_scope: "sympoies/nils-cli#541".to_string(),
        current: "Task 2.5 done".to_string(),
        next_action: "Close issue".to_string(),
        validation: "pass".to_string(),
        linked_prs: vec!["sympoies/nils-cli#547".to_string()],
        blockers: vec![],
        approval: "https://example.test/approval".to_string(),
        source_url: Some("https://example.test/source".to_string()),
        plan_url: Some("https://example.test/plan".to_string()),
        state_url: Some("https://example.test/state".to_string()),
        session_url: Some("https://example.test/session".to_string()),
        validation_url: Some("https://example.test/validation".to_string()),
        review_url: Some("https://example.test/review".to_string()),
        closeout_url: Some("https://example.test/closeout".to_string()),
        title: None,
        issue_url: None,
    }
}

#[test]
fn dashboard_pending_matches_golden() {
    let out = render_dashboard(dashboard_input_pending());
    assert_or_bless("dashboard_pending.md", &out);
}

#[test]
fn dashboard_full_matches_golden() {
    let out = render_dashboard(dashboard_input_full());
    assert_or_bless("dashboard_full.md", &out);
}

#[test]
fn dashboard_complete_matches_golden() {
    let out = render_dashboard(dashboard_input_complete());
    assert_or_bless("dashboard_complete.md", &out);
}

// --- snapshot scenarios ------------------------------------------------

fn snapshot_data_full() -> SnapshotData {
    SnapshotData {
        path: "docs/plans/sample/sample-plan.md".to_string(),
        commit: "abc1234".to_string(),
        title: Some("Sample Plan".to_string()),
        summary: Some("One-liner".to_string()),
    }
}

fn snapshot_data_minimal() -> SnapshotData {
    SnapshotData {
        path: String::new(),
        commit: String::new(),
        title: None,
        summary: None,
    }
}

#[test]
fn snapshot_source_full_matches_golden() {
    let out = render_record_snapshot_comment(
        RecordProfile::Tracking,
        LifecycleCommentKind::Source,
        &snapshot_data_full(),
        "# Sample Source\n\nbody...\n",
        Some("2026-05-23T08:42:11Z"),
    )
    .expect("render");
    assert_or_bless("snapshot_source_full.md", &out);
}

#[test]
fn snapshot_plan_full_matches_golden() {
    let out = render_record_snapshot_comment(
        RecordProfile::Tracking,
        LifecycleCommentKind::Plan,
        &snapshot_data_full(),
        "# Sample Plan\n\nbody...\n",
        Some("2026-05-23T08:42:11Z"),
    )
    .expect("render");
    assert_or_bless("snapshot_plan_full.md", &out);
}

#[test]
fn snapshot_plan_minimal_matches_golden() {
    let out = render_record_snapshot_comment(
        RecordProfile::Tracking,
        LifecycleCommentKind::Plan,
        &snapshot_data_minimal(),
        "plan body\n",
        None,
    )
    .expect("render");
    assert_or_bless("snapshot_plan_minimal.md", &out);
}

// --- post_comment per-kind scenarios ----------------------------------

#[test]
fn post_comment_state_matches_golden() {
    let out = render_record_post_comment(
        RecordProfile::Tracking,
        LifecycleCommentKind::State,
        json!({
            "status": "in-progress",
            "target_scope": "sympoies/nils-cli#541",
            "current": "Task 2.5b",
            "next_action": "Open PR",
            "tasks": [
                {"id": "1.1", "status": "done", "title": "First"},
                {"id": "1.2", "status": "in-progress", "title": "Second"}
            ],
            "prs": [],
            "blockers": [],
            "links": {}
        }),
        None,
        Some("2026-05-26T06:00:00Z"),
    )
    .expect("render");
    assert_or_bless("post_comment_state.md", &out);
}

#[test]
fn post_comment_session_matches_golden() {
    let out = render_record_post_comment(
        RecordProfile::Tracking,
        LifecycleCommentKind::Session,
        json!({
            "summary": "Session summary",
            "highlights": ["First highlight", "Second highlight"],
            "links": {"state": "https://example.test/state", "pr": "https://example.test/pr/1"}
        }),
        None,
        Some("2026-05-26T06:00:00Z"),
    )
    .expect("render");
    assert_or_bless("post_comment_session.md", &out);
}

#[test]
fn post_comment_validation_matches_golden() {
    let out = render_record_post_comment(
        RecordProfile::Tracking,
        LifecycleCommentKind::Validation,
        json!({
            "overall": "partial",
            "commands": [
                {"command": "cargo test", "status": "pass", "evidence": "362/362"},
                {"command": "cargo bench", "status": "skipped", "evidence": ""}
            ],
            "waivers": [
                {"command": "cargo bench", "reason": "no bench target"}
            ]
        }),
        None,
        Some("2026-05-26T06:00:00Z"),
    )
    .expect("render");
    assert_or_bless("post_comment_validation.md", &out);
}

#[test]
fn post_comment_review_matches_golden() {
    let out = render_record_post_comment(
        RecordProfile::Tracking,
        LifecycleCommentKind::Review,
        json!({
            "decision": "approve",
            "lenses": ["testing", "maintainability"],
            "findings": [
                {"id": "F1", "severity": "minor", "disposition": "fixed", "summary": "covered"}
            ],
            "outcome_comment_url": "https://example.test/review"
        }),
        None,
        Some("2026-05-26T06:00:00Z"),
    )
    .expect("render");
    assert_or_bless("post_comment_review.md", &out);
}

#[test]
fn post_comment_closeout_matches_golden() {
    let out = render_record_post_comment(
        RecordProfile::Tracking,
        LifecycleCommentKind::Closeout,
        json!({
            "final_status": "complete",
            "approval": {"comment_url": "https://example.test/approval"},
            "linked_prs": [{
                "ref": "owner/repo#1",
                "url": "https://example.test/pr/1",
                "merge_sha": "abc123",
                "checks": "pass",
                "required_state": "pass",
                "required_count": 2,
                "non_required_failures": []
            }],
            "notes": "Closeout note"
        }),
        Some("Closeout summary."),
        Some("2026-05-26T06:00:00Z"),
    )
    .expect("render");
    assert_or_bless("post_comment_closeout.md", &out);
}

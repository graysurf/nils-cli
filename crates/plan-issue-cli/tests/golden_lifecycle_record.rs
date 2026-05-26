//! Byte-equality golden tests for `lifecycle_record::render_dashboard`
//! and `lifecycle_record::render_dashboard_from_audit`. Both share the
//! same dashboard shape and the same Tera template.
//!
//! Set `BLESS_LIFECYCLE_RECORD_GOLDEN=1` to overwrite fixtures from
//! the current renderer output instead of asserting.

use std::path::PathBuf;

use plan_issue_cli::commands::record::RecordProfile;
use plan_issue_cli::lifecycle_record::{DashboardInput, render_dashboard};

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

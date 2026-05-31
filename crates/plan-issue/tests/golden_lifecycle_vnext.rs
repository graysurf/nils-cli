//! Byte-equality golden tests for the lifecycle vNext template
//! preview. Each role is rendered through
//! [`plan_issue::lifecycle_vnext::templates::render_template`]
//! (which drives `nils_markdown::Engine`) and compared against a
//! committed Markdown fixture. The fixtures double as the source of
//! truth for provider-visible comment shape.
//!
//! Set `BLESS_LIFECYCLE_VNEXT_GOLDEN=1` to overwrite the fixtures
//! from the current renderer output instead of asserting.

use std::path::PathBuf;

use plan_issue::commands::record::RecordProfile;
use plan_issue::lifecycle_record::PayloadRole;
use plan_issue::lifecycle_vnext::templates::{TemplateFormat, render_template};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join("lifecycle_vnext")
        .join(name)
}

fn assert_or_bless(name: &str, actual: &str) {
    let path = fixture_path(name);
    if std::env::var_os("BLESS_LIFECYCLE_VNEXT_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir fixture dir");
        std::fs::write(&path, actual).expect("write fixture");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()));
    pretty_assertions::assert_eq!(expected, actual, "golden mismatch for {name}");
}

#[test]
fn lifecycle_vnext_source_tracking_matches_golden() {
    let out = render_template(
        RecordProfile::Tracking,
        PayloadRole::Source,
        TemplateFormat::Markdown,
    )
    .expect("render");
    assert_or_bless("source_tracking.md", &out);
}

#[test]
fn lifecycle_vnext_plan_tracking_matches_golden() {
    let out = render_template(
        RecordProfile::Tracking,
        PayloadRole::Plan,
        TemplateFormat::Markdown,
    )
    .expect("render");
    assert_or_bless("plan_tracking.md", &out);
}

#[test]
fn lifecycle_vnext_state_tracking_matches_golden() {
    let out = render_template(
        RecordProfile::Tracking,
        PayloadRole::State,
        TemplateFormat::Markdown,
    )
    .expect("render");
    assert_or_bless("state_tracking.md", &out);
}

#[test]
fn lifecycle_vnext_session_tracking_matches_golden() {
    let out = render_template(
        RecordProfile::Tracking,
        PayloadRole::Session,
        TemplateFormat::Markdown,
    )
    .expect("render");
    assert_or_bless("session_tracking.md", &out);
}

#[test]
fn lifecycle_vnext_validation_tracking_matches_golden() {
    let out = render_template(
        RecordProfile::Tracking,
        PayloadRole::Validation,
        TemplateFormat::Markdown,
    )
    .expect("render");
    assert_or_bless("validation_tracking.md", &out);
}

#[test]
fn lifecycle_vnext_review_tracking_matches_golden() {
    let out = render_template(
        RecordProfile::Tracking,
        PayloadRole::Review,
        TemplateFormat::Markdown,
    )
    .expect("render");
    assert_or_bless("review_tracking.md", &out);
}

#[test]
fn lifecycle_vnext_closeout_tracking_matches_golden() {
    let out = render_template(
        RecordProfile::Tracking,
        PayloadRole::Closeout,
        TemplateFormat::Markdown,
    )
    .expect("render");
    assert_or_bless("closeout_tracking.md", &out);
}

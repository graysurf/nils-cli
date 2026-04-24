use std::path::Path;

use api_testing_core::cli_report::{ReportMetadataConfig, build_report_metadata, endpoint_note};
use nils_test_support::{EnvGuard, GlobalStateLock};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn report_config<'a>(
    case_name: &'a str,
    out_path: Option<&'a str>,
    project_root: Option<&'a str>,
    report_dir_env: &'a str,
    invocation_dir: &'a Path,
) -> ReportMetadataConfig<'a> {
    ReportMetadataConfig {
        case_name,
        out_path,
        project_root,
        report_dir_env,
        invocation_dir,
    }
}

fn assert_stamp_prefix(name: &str) {
    assert!(name.len() >= 13, "name={name}");
    let stamp = &name[..13];
    let bytes = stamp.as_bytes();
    assert!(bytes[..8].iter().all(|b| b.is_ascii_digit()), "name={name}");
    assert_eq!(bytes[8], b'-', "name={name}");
    assert!(bytes[9..].iter().all(|b| b.is_ascii_digit()), "name={name}");
}

#[test]
fn build_report_metadata_defaults_to_docs_dir() {
    let lock = GlobalStateLock::new();
    let _guard = EnvGuard::remove(&lock, "TEST_REPORT_DIR");
    let tmp = TempDir::new().unwrap();
    let metadata = build_report_metadata(report_config(
        "Hello World",
        None,
        None,
        "TEST_REPORT_DIR",
        tmp.path(),
    ));

    assert_eq!(metadata.project_root, tmp.path());
    let docs_dir = tmp.path().join("docs");
    assert_eq!(metadata.out_path.parent().unwrap(), docs_dir.as_path());

    let filename = metadata.out_path.file_name().unwrap().to_string_lossy();
    assert_stamp_prefix(&filename);
    assert!(filename.ends_with("-hello-world-api-test-report.md"));
    assert_eq!(
        metadata.report_date.len(),
        10,
        "date={}",
        metadata.report_date
    );
    assert!(metadata.generated_at.contains('T'));
}

#[test]
fn build_report_metadata_uses_relative_env_dir() {
    let lock = GlobalStateLock::new();
    let tmp = TempDir::new().unwrap();
    let project_root = tmp.path().join("repo");
    std::fs::create_dir_all(&project_root).unwrap();
    let _guard = EnvGuard::set(&lock, "TEST_REPORT_DIR", "reports/custom");

    let metadata = build_report_metadata(report_config(
        "Case",
        None,
        Some(&project_root.to_string_lossy()),
        "TEST_REPORT_DIR",
        tmp.path(),
    ));

    let expected_parent = project_root.join("reports/custom");
    assert_eq!(
        metadata.out_path.parent().unwrap(),
        expected_parent.as_path()
    );
}

#[test]
fn build_report_metadata_uses_absolute_env_dir() {
    let lock = GlobalStateLock::new();
    let tmp = TempDir::new().unwrap();
    let absolute_dir = tmp.path().join("abs/reports");
    std::fs::create_dir_all(&absolute_dir).unwrap();
    let _guard = EnvGuard::set(&lock, "TEST_REPORT_DIR", &absolute_dir.to_string_lossy());

    let metadata = build_report_metadata(report_config(
        "Case",
        None,
        Some(&tmp.path().to_string_lossy()),
        "TEST_REPORT_DIR",
        tmp.path(),
    ));

    assert_eq!(metadata.out_path.parent().unwrap(), absolute_dir.as_path());
}

#[test]
fn build_report_metadata_honors_out_override() {
    let lock = GlobalStateLock::new();
    let tmp = TempDir::new().unwrap();
    let out_path = tmp.path().join("explicit/report.md");
    let _guard = EnvGuard::set(&lock, "TEST_REPORT_DIR", "ignored");

    let metadata = build_report_metadata(report_config(
        "Case",
        Some(&out_path.to_string_lossy()),
        Some(&tmp.path().to_string_lossy()),
        "TEST_REPORT_DIR",
        tmp.path(),
    ));

    assert_eq!(metadata.out_path, out_path);
}

#[test]
fn endpoint_note_prefers_url_then_env_then_implicit() {
    let note = endpoint_note(Some("http://example"), Some("prod"), "implicit");
    assert_eq!(note, "Endpoint: --url http://example");

    let note = endpoint_note(None, Some("prod"), "implicit");
    assert_eq!(note, "Endpoint: --env prod");

    let note = endpoint_note(None, None, "implicit");
    assert_eq!(note, "implicit");
}

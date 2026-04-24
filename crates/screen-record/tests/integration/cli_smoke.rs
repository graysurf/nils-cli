use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOutput, run};
use tempfile::TempDir;

use crate::common;
fn screen_record_bin() -> std::path::PathBuf {
    resolve("screen-record")
}

fn run_screen_record(args: &[&str]) -> CmdOutput {
    run(&screen_record_bin(), args, &[], None)
}

#[test]
fn help_includes_key_flags() {
    let out = run_screen_record(&["--help"]);
    assert_eq!(out.code, 0);
    let text = format!("{}{}", out.stdout_text(), out.stderr_text());
    assert!(text.contains("--list-windows"));
    assert!(text.contains("--list-displays"));
    assert!(text.contains("--list-apps"));
    assert!(text.contains("--screenshot"));
    assert!(text.contains("--portal"));
    assert!(text.contains("--image-format"));
    assert!(text.contains("--audio"));
    assert!(text.contains("--duration"));
    assert!(text.contains("--display-id"));
    assert!(text.contains("--metadata-out"));
    assert!(text.contains("--diagnostics-out"));
    assert!(text.contains("--if-changed"));
    assert!(text.contains("--if-changed-baseline"));
    assert!(text.contains("--if-changed-threshold"));
}

#[test]
fn invalid_flag_exits_nonzero() {
    let out = run_screen_record(&["--definitely-not-a-flag"]);
    assert_ne!(out.code, 0);
}

#[test]
fn diagnostics_contract_schema() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("diagnostics.mov");
    let diagnostics_path = cwd.path().join("diagnostics.json");

    let out = harness.run(
        cwd.path(),
        &[
            "--app",
            "Terminal",
            "--duration",
            "1",
            "--audio",
            "off",
            "--path",
            output_path.to_str().unwrap(),
            "--diagnostics-out",
            diagnostics_path.to_str().unwrap(),
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let text = std::fs::read_to_string(&diagnostics_path).expect("read diagnostics");
    for key in [
        "\"schema_version\"",
        "\"contract_version\"",
        "\"source_output_path\"",
        "\"source_output_bytes\"",
        "\"generated_at\"",
        "\"artifacts\"",
        "\"contact_sheet\"",
        "\"motion_intervals\"",
        "\"error\"",
    ] {
        assert!(text.contains(key), "missing diagnostics key: {key}");
    }
    assert!(text.contains("\"format\": \"svg\""));
    assert!(text.contains("\"format\": \"json\""));
    assert!(text.contains("\"interval_count\": 1"));
    assert!(text.contains("\"error\": null"));
}

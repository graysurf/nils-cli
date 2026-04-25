use std::fs;
#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

use nils_test_support::cmd::run_with;
use tempfile::TempDir;

use crate::common;
fn assert_metadata_shape_and_order(text: &str) {
    let keys = [
        "\"target\"",
        "\"duration_ms\"",
        "\"audio_mode\"",
        "\"format\"",
        "\"output_path\"",
        "\"output_bytes\"",
        "\"started_at\"",
        "\"ended_at\"",
        "\"error\"",
    ];
    let mut offset = 0usize;
    for key in keys {
        assert_eq!(
            text.matches(key).count(),
            1,
            "metadata key should appear exactly once: {key}"
        );
        let rel = text[offset..]
            .find(key)
            .unwrap_or_else(|| panic!("missing metadata key: {key}"));
        offset += rel + key.len();
    }
}

#[test]
fn list_windows_outputs_fixture() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output = harness.run(cwd.path(), &["--list-windows"]);

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let stdout = output.stdout_text();
    let mut lines: Vec<&str> = stdout.trim().split('\n').collect();
    lines.retain(|line| !line.is_empty());

    assert_eq!(
        lines,
        vec![
            "200\tFinder\tFinder\t80\t80\t900\t600\ttrue",
            "101\tTerminal\tDocs\t40\t40\t1100\t760\ttrue",
            "100\tTerminal\tInbox\t0\t0\t1200\t800\ttrue",
        ]
    );
}

#[test]
fn list_apps_outputs_fixture() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output = harness.run(cwd.path(), &["--list-apps"]);

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let stdout = output.stdout_text();
    let mut lines: Vec<&str> = stdout.trim().split('\n').collect();
    lines.retain(|line| !line.is_empty());

    assert_eq!(
        lines,
        vec![
            "Finder\t222\tcom.apple.Finder",
            "Terminal\t111\tcom.apple.Terminal",
        ]
    );
}

#[test]
fn list_displays_outputs_fixture() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output = harness.run(cwd.path(), &["--list-displays"]);

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let stdout = output.stdout_text();
    let mut lines: Vec<&str> = stdout.trim().split('\n').collect();
    lines.retain(|line| !line.is_empty());

    assert_eq!(lines, vec!["1\t1440\t900"]);
}

#[test]
fn record_mov_fixture_writes_file() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("recording.mov");

    let output = harness.run(
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
        ],
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let stdout = output.stdout_text();
    assert_eq!(stdout.trim(), output_path.display().to_string());
    let metadata = std::fs::metadata(&output_path).expect("output exists");
    assert!(metadata.len() > 0);
}

#[test]
fn record_main_display_fixture_writes_file() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("display.mov");

    let output = harness.run(
        cwd.path(),
        &[
            "--display",
            "--duration",
            "1",
            "--audio",
            "off",
            "--path",
            output_path.to_str().unwrap(),
        ],
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let stdout = output.stdout_text();
    assert_eq!(stdout.trim(), output_path.display().to_string());
    let metadata = std::fs::metadata(&output_path).expect("output exists");
    assert!(metadata.len() > 0);
}

#[test]
fn record_failure_removes_staged_and_target_output() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("broken.mov");

    let output = run_with(
        &harness.screen_record_bin(),
        &[
            "--app",
            "Terminal",
            "--duration",
            "1",
            "--audio",
            "off",
            "--path",
            output_path.to_str().unwrap(),
        ],
        &harness
            .cmd_options(cwd.path())
            .with_env("AGENTS_SCREEN_RECORD_TEST_MODE_FAIL_APPEND", "1"),
    );

    assert_eq!(output.code, 1);
    assert!(
        output
            .stderr_text()
            .contains("failed to append sample buffer")
    );
    assert!(
        !output_path.exists(),
        "requested output should not exist on failure"
    );

    let staged_leftovers: Vec<_> = fs::read_dir(cwd.path())
        .expect("read cwd")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter_map(|name| name.into_string().ok())
        .filter(|name| name.contains(".recording-"))
        .collect();
    assert!(
        staged_leftovers.is_empty(),
        "staged recording files should be cleaned up: {staged_leftovers:?}"
    );
}

#[test]
fn metadata_out_success_path() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("with-metadata.mov");
    let metadata_path = cwd.path().join("recording-metadata.json");

    let output = harness.run(
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
            "--metadata-out",
            metadata_path.to_str().unwrap(),
        ],
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    assert_eq!(
        output.stdout_text().trim(),
        output_path.display().to_string()
    );
    let metadata_text = fs::read_to_string(&metadata_path).expect("read metadata");
    assert_metadata_shape_and_order(&metadata_text);
    assert!(metadata_text.contains("\"target\": \"app:Terminal\""));
    assert!(metadata_text.contains("\"audio_mode\": \"off\""));
    assert!(metadata_text.contains("\"format\": \"mov\""));
    assert!(metadata_text.contains(&format!("\"output_path\": \"{}\"", output_path.display())));
    assert!(
        metadata_text.contains("\"output_bytes\": ")
            && !metadata_text.contains("\"output_bytes\": null")
    );
    assert!(metadata_text.contains("\"started_at\": \"2026-01-01T00:00:00.000Z\""));
    assert!(metadata_text.contains("\"ended_at\": \"2026-01-01T00:00:00.000Z\""));
    assert!(metadata_text.contains("\"error\": null"));
}

#[test]
fn metadata_out_failure_path() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("broken-with-metadata.mov");
    let metadata_path = cwd.path().join("recording-metadata-failure.json");

    let output = run_with(
        &harness.screen_record_bin(),
        &[
            "--app",
            "Terminal",
            "--duration",
            "1",
            "--audio",
            "off",
            "--path",
            output_path.to_str().unwrap(),
            "--metadata-out",
            metadata_path.to_str().unwrap(),
        ],
        &harness
            .cmd_options(cwd.path())
            .with_env("AGENTS_SCREEN_RECORD_TEST_MODE_FAIL_APPEND", "1"),
    );

    assert_eq!(output.code, 1);
    assert!(
        output
            .stderr_text()
            .contains("failed to append sample buffer")
    );
    assert!(
        !output_path.exists(),
        "requested output should not exist on failure"
    );

    let metadata_text = fs::read_to_string(&metadata_path).expect("read metadata");
    assert_metadata_shape_and_order(&metadata_text);
    assert!(metadata_text.contains("\"target\": \"app:Terminal\""));
    assert!(metadata_text.contains("\"audio_mode\": \"off\""));
    assert!(metadata_text.contains("\"format\": \"mov\""));
    assert!(metadata_text.contains(&format!("\"output_path\": \"{}\"", output_path.display())));
    assert!(metadata_text.contains("\"output_bytes\": null"));
    assert!(metadata_text.contains(
        "\"error\": \"error: failed to append sample buffer: The operation could not be completed\""
    ));
}

#[test]
fn diagnostics_out_success_writes_manifest_and_artifacts() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("with-diagnostics.mov");
    let diagnostics_path = cwd.path().join("recording-diagnostics.json");

    let output = harness.run(
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

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    assert_eq!(
        output.stdout_text().trim(),
        output_path.display().to_string()
    );
    let diagnostics_text = fs::read_to_string(&diagnostics_path).expect("read diagnostics");
    assert!(diagnostics_text.contains("\"schema_version\": 1"));
    assert!(diagnostics_text.contains("\"contract_version\": \"1.0\""));
    assert!(diagnostics_text.contains("\"format\": \"svg\""));
    assert!(diagnostics_text.contains("\"format\": \"json\""));
    assert!(diagnostics_text.contains("\"interval_count\": 1"));
    assert!(diagnostics_text.contains("\"error\": null"));

    let artifacts_dir = diagnostics_path.with_extension("diagnostics");
    let stem = output_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap();
    let contact_sheet = artifacts_dir.join(format!("{stem}-contact-sheet.svg"));
    let motion_intervals = artifacts_dir.join(format!("{stem}-motion-intervals.json"));
    assert!(contact_sheet.exists(), "contact sheet should exist");
    assert!(motion_intervals.exists(), "motion intervals should exist");
    assert!(
        fs::metadata(&contact_sheet)
            .expect("contact metadata")
            .len()
            > 0
    );
    let intervals_text = fs::read_to_string(&motion_intervals).expect("read intervals");
    assert!(intervals_text.contains("\"intervals\""));
    assert!(intervals_text.contains("\"start_ms\": 0"));
}

#[test]
fn diagnostics_out_failure_keeps_primary_output() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("diagnostics-failure.mov");
    let diagnostics_path = cwd.path().join("diagnostics-failure.json");

    let output = harness.run_with_options(
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
        harness
            .cmd_options(cwd.path())
            .with_env("AGENTS_SCREEN_RECORD_TEST_MODE_FAIL_DIAGNOSTICS", "1"),
    );

    assert_eq!(output.code, 1);
    assert!(
        output
            .stderr_text()
            .contains("diagnostics generation failed in test mode")
    );
    assert!(
        output_path.exists(),
        "primary output should still be published"
    );
    assert!(
        fs::metadata(&output_path).expect("output metadata").len() > 0,
        "primary output should stay readable"
    );
    assert!(
        !diagnostics_path.exists(),
        "diagnostics manifest should not be written on forced failure"
    );
}

#[test]
fn default_recording_run_does_not_create_diagnostics_artifacts() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("without-diagnostics.mov");

    let output = harness.run(
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
        ],
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let leftovers: Vec<_> = fs::read_dir(cwd.path())
        .expect("read cwd")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter_map(|name| name.into_string().ok())
        .filter(|name| name.ends_with(".diagnostics"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "default recording should not emit diagnostics artifacts: {leftovers:?}"
    );
}

#[cfg(unix)]
#[test]
fn record_realtime_mode_sigint_stops_early_and_keeps_valid_output() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("interrupted.mov");
    let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample.mov");

    let mut cmd = Command::new(harness.screen_record_bin());
    cmd.current_dir(cwd.path())
        .args([
            "--app",
            "Terminal",
            "--duration",
            "30",
            "--audio",
            "off",
            "--path",
            output_path.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_SCREEN_RECORD_TEST_MODE_REALTIME", "1");
    for key in options.env_remove {
        cmd.env_remove(key);
    }
    for (key, value) in options.envs {
        cmd.env(key, value);
    }

    let start = Instant::now();
    let mut child = cmd.spawn().expect("spawn screen-record");
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        child.try_wait().expect("poll child").is_none(),
        "recording finished before SIGINT"
    );

    let kill_status = Command::new("kill")
        .args(["-s", "INT", &child.id().to_string()])
        .status()
        .expect("send SIGINT");
    assert!(kill_status.success(), "failed to send SIGINT");

    let output = child.wait_with_output().expect("wait screen-record");
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected success after SIGINT"
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "recording should stop quickly after SIGINT"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), output_path.display().to_string());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.trim(), "");

    assert!(output_path.exists(), "expected output file");
    assert!(fs::metadata(&output_path).expect("metadata").len() > 0);
    assert_eq!(
        fs::read(&output_path).expect("read output"),
        fs::read(&fixture_path).expect("read fixture")
    );
}

#[test]
fn record_display_id_fixture_writes_file() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("display-1.mov");

    let output = harness.run(
        cwd.path(),
        &[
            "--display-id",
            "1",
            "--duration",
            "1",
            "--audio",
            "off",
            "--path",
            output_path.to_str().unwrap(),
        ],
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let stdout = output.stdout_text();
    assert_eq!(stdout.trim(), output_path.display().to_string());
    let metadata = std::fs::metadata(&output_path).expect("output exists");
    assert!(metadata.len() > 0);
}

#[test]
fn record_mp4_fixture_writes_file() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("recording.mp4");

    let output = harness.run(
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
        ],
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let stdout = output.stdout_text();
    assert_eq!(stdout.trim(), output_path.display().to_string());
    let metadata = std::fs::metadata(&output_path).expect("output exists");
    assert!(metadata.len() > 0);
}

#[test]
fn audio_both_requires_mov() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("recording.mp4");

    let output = harness.run(
        cwd.path(),
        &[
            "--app",
            "Terminal",
            "--duration",
            "1",
            "--audio",
            "both",
            "--path",
            output_path.to_str().unwrap(),
        ],
    );

    assert_eq!(output.code, 2);
    assert!(output.stderr_text().contains("--audio both requires .mov"));
}

#[test]
fn record_requires_path() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(
        cwd.path(),
        &["--app", "Terminal", "--duration", "1", "--audio", "off"],
    );

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("--path is required for recording")
    );
}

#[test]
fn screenshot_default_path_writes_png() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(cwd.path(), &["--screenshot", "--app", "Terminal"]);

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let base = std::fs::canonicalize(cwd.path()).expect("canonicalize cwd");
    let expected = base
        .join("screenshots")
        .join("screenshot-20260101-000000-win100-Terminal-Inbox.png");
    assert_eq!(output.stdout_text().trim(), expected.display().to_string());
    let metadata = std::fs::metadata(&expected).expect("output exists");
    assert!(metadata.len() > 0);
}

#[test]
fn screenshot_image_format_webp_writes_file() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(
        cwd.path(),
        &[
            "--screenshot",
            "--app",
            "Terminal",
            "--image-format",
            "webp",
        ],
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let base = std::fs::canonicalize(cwd.path()).expect("canonicalize cwd");
    let expected = base
        .join("screenshots")
        .join("screenshot-20260101-000000-win100-Terminal-Inbox.webp");
    assert_eq!(output.stdout_text().trim(), expected.display().to_string());
    let metadata = std::fs::metadata(&expected).expect("output exists");
    assert!(metadata.len() > 0);
}

#[test]
fn screenshot_path_and_image_format_conflict_errors() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("out.png");

    let output = harness.run(
        cwd.path(),
        &[
            "--screenshot",
            "--app",
            "Terminal",
            "--path",
            output_path.to_str().unwrap(),
            "--image-format",
            "jpg",
        ],
    );

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("--image-format jpg conflicts with --path extension")
    );
}

#[test]
fn screenshot_rejects_duration_flag() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(
        cwd.path(),
        &["--screenshot", "--app", "Terminal", "--duration", "1"],
    );

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("--duration is not valid with --screenshot")
    );
}

#[test]
fn screenshot_only_flags_require_screenshot_mode() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(cwd.path(), &["--dir", "screens"]);

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("screenshot flags require --screenshot")
    );
}

#[test]
fn list_mode_rejects_screenshot_flags() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(cwd.path(), &["--list-windows", "--image-format", "png"]);

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("screenshot flags require --screenshot")
    );
}

#[test]
fn list_mode_rejects_metadata_out_flag() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output = harness.run(
        cwd.path(),
        &[
            "--list-windows",
            "--metadata-out",
            "recording-metadata.json",
        ],
    );

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("--metadata-out is only valid with recording")
    );
}

#[test]
fn list_mode_rejects_diagnostics_out_flag() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output = harness.run(
        cwd.path(),
        &[
            "--list-windows",
            "--diagnostics-out",
            "recording-diagnostics.json",
        ],
    );

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("--diagnostics-out is only valid with recording")
    );
}

#[test]
fn multiple_modes_error() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(cwd.path(), &["--list-windows", "--list-apps"]);

    assert_eq!(output.code, 2);
    assert!(output.stderr_text().contains("select exactly one mode"));
}

#[test]
fn record_requires_selector() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(
        cwd.path(),
        &["--duration", "1", "--audio", "off", "--path", "out.mov"],
    );

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("recording requires exactly one selector")
    );
}

#[test]
fn screenshot_rejects_display_selector() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(cwd.path(), &["--screenshot", "--display"]);

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("display selectors are not valid with --screenshot")
    );
}

#[test]
fn screenshot_requires_selector() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(cwd.path(), &["--screenshot"]);

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("screenshot requires exactly one selector")
    );
}

#[test]
fn screenshot_rejects_audio_flag() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(
        cwd.path(),
        &["--screenshot", "--app", "Terminal", "--audio", "system"],
    );

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("--audio is not valid with --screenshot")
    );
}

#[test]
fn screenshot_path_and_dir_conflict_errors() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(
        cwd.path(),
        &[
            "--screenshot",
            "--app",
            "Terminal",
            "--path",
            "out.png",
            "--dir",
            "screens",
        ],
    );

    assert_eq!(output.code, 2);
    assert!(output.stderr_text().contains("use either --path or --dir"));
}

#[test]
fn screenshot_path_is_dir_errors() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let dir_path = cwd.path().join("out.png");
    std::fs::create_dir_all(&dir_path).expect("dir");

    let output = harness.run(
        cwd.path(),
        &[
            "--screenshot",
            "--app",
            "Terminal",
            "--path",
            dir_path.to_str().unwrap(),
        ],
    );

    assert_eq!(output.code, 2);
    assert!(output.stderr_text().contains("--path must be a file path"));
}

#[test]
fn screenshot_unsupported_extension_errors() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(
        cwd.path(),
        &["--screenshot", "--app", "Terminal", "--path", "out.tiff"],
    );

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("unsupported --path extension for screenshot")
    );
}

#[test]
fn portal_rejects_non_capture_modes() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(cwd.path(), &["--portal", "--list-windows"]);

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("--portal is only valid with recording or --screenshot")
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn portal_rejected_with_linux_only_message_on_non_linux() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(cwd.path(), &["--portal", "--screenshot"]);

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("--portal is only supported on Linux (Wayland)")
    );
}

#[test]
fn screenshot_rejects_format_flag() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(
        cwd.path(),
        &["--screenshot", "--app", "Terminal", "--format", "mov"],
    );

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("--format is not valid with --screenshot")
    );
}

#[test]
fn screenshot_if_changed_writes_output() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("if-changed.png");

    let output = harness.run(
        cwd.path(),
        &[
            "--screenshot",
            "--app",
            "Terminal",
            "--path",
            output_path.to_str().unwrap(),
            "--if-changed",
            "--if-changed-threshold",
            "0",
        ],
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    assert_eq!(
        output.stdout_text().trim(),
        output_path.display().to_string()
    );
    assert!(std::fs::metadata(&output_path).expect("metadata").len() > 0);
}

#[test]
fn if_changed_rejects_record_mode() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("recording.mov");

    let output = harness.run(
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
            "--if-changed",
        ],
    );

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("--if-changed is only valid with --screenshot")
    );
}

#[test]
fn if_changed_threshold_requires_if_changed() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(
        cwd.path(),
        &[
            "--screenshot",
            "--app",
            "Terminal",
            "--if-changed-threshold",
            "4",
        ],
    );

    assert_eq!(output.code, 2);
    assert!(output.stderr_text().contains("--if-changed"));
}

#[test]
fn preflight_mode_rejects_capture_flags() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(cwd.path(), &["--preflight", "--app", "Terminal"]);

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("capture flags are not valid with this mode")
    );
}

#[test]
fn request_permission_mode_rejects_capture_flags() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let output = harness.run(cwd.path(), &["--request-permission", "--app", "Terminal"]);

    assert_eq!(output.code, 2);
    assert!(
        output
            .stderr_text()
            .contains("capture flags are not valid with this mode")
    );
}

#[test]
fn record_relative_path_creates_parent_dirs() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let expected = cwd.path().join("captures").join("clip.mov");

    let output = harness.run(
        cwd.path(),
        &[
            "--app",
            "Terminal",
            "--duration",
            "1",
            "--audio",
            "off",
            "--path",
            "captures/clip.mov",
        ],
    );

    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let metadata = std::fs::metadata(&expected).expect("output exists");
    assert!(metadata.len() > 0);
    assert!(expected.parent().expect("parent").is_dir());
    let stdout_path = std::path::PathBuf::from(output.stdout_text().trim());
    let canonical_stdout = std::fs::canonicalize(stdout_path).expect("canonical stdout path");
    let canonical_expected = std::fs::canonicalize(expected).expect("canonical expected path");
    assert_eq!(canonical_stdout, canonical_expected);
}

#[test]
fn screenshot_rejects_file_path_for_dir_flag() {
    let harness = common::ScreenRecordHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let file_path = cwd.path().join("not-a-dir");
    std::fs::write(&file_path, b"file").expect("write file");

    let output = harness.run(
        cwd.path(),
        &[
            "--screenshot",
            "--app",
            "Terminal",
            "--dir",
            file_path.to_str().unwrap(),
        ],
    );

    assert_eq!(output.code, 2);
    assert!(output.stderr_text().contains("--dir must be a directory"));
}

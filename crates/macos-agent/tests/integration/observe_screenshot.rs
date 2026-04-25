use tempfile::TempDir;

use crate::common;
#[test]
fn observe_screenshot_writes_file_and_prints_path_line() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("artifact.png");

    let out = harness.run(
        cwd.path(),
        &[
            "observe",
            "screenshot",
            "--active-window",
            "--path",
            output_path.to_str().unwrap(),
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");
    assert_eq!(out.stdout_text().trim(), output_path.display().to_string());
    assert!(output_path.exists(), "screenshot output should exist");
    assert!(
        std::fs::metadata(&output_path).expect("metadata").len() > 0,
        "screenshot should be non-empty"
    );
}

#[test]
fn observe_screenshot_json_contract() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "observe",
            "screenshot",
            "--active-window",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");

    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be valid json");
    assert_eq!(payload["schema_version"], serde_json::json!(1));
    assert_eq!(payload["ok"], serde_json::json!(true));
    assert_eq!(payload["command"], serde_json::json!("observe.screenshot"));
    assert!(
        payload["result"]["path"]
            .as_str()
            .unwrap()
            .contains("window-100")
    );
}

#[test]
fn observe_screenshot_errors_keep_stdout_empty() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["observe", "screenshot", "--window-id", "999"]);

    assert_eq!(out.code, 2);
    assert_eq!(out.stdout_text(), "");
    assert!(out.stderr_text().starts_with("error:"));
}

#[test]
fn if_changed_payload_contract() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output_path = cwd.path().join("if-changed.png");
    let output_path_text = output_path.to_string_lossy().to_string();

    let first = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "observe",
            "screenshot",
            "--active-window",
            "--path",
            &output_path_text,
            "--if-changed",
            "--if-changed-threshold",
            "0",
        ],
    );
    assert_eq!(first.code, 0, "stderr: {}", first.stderr_text());
    let first_payload: serde_json::Value =
        serde_json::from_str(&first.stdout_text()).expect("first payload");
    let first_if_changed = &first_payload["result"]["if_changed"];
    assert_eq!(first_if_changed["changed"], serde_json::json!(true));
    assert_eq!(first_if_changed["baseline_hash"], serde_json::Value::Null);
    assert_eq!(first_if_changed["threshold"], serde_json::json!(0));
    assert_eq!(
        first_if_changed["captured_path"],
        serde_json::json!(output_path.display().to_string())
    );
    let first_hash = first_if_changed["current_hash"]
        .as_str()
        .expect("current_hash should be string")
        .to_string();

    let second = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "observe",
            "screenshot",
            "--active-window",
            "--path",
            &output_path_text,
            "--if-changed",
            "--if-changed-threshold",
            "0",
        ],
    );
    assert_eq!(second.code, 0, "stderr: {}", second.stderr_text());
    let second_payload: serde_json::Value =
        serde_json::from_str(&second.stdout_text()).expect("second payload");
    let second_if_changed = &second_payload["result"]["if_changed"];
    assert_eq!(second_if_changed["changed"], serde_json::json!(false));
    assert_eq!(second_if_changed["threshold"], serde_json::json!(0));
    assert_eq!(second_if_changed["captured_path"], serde_json::Value::Null);
    assert_eq!(
        second_if_changed["baseline_hash"],
        serde_json::json!(first_hash.clone())
    );
    assert_eq!(
        second_if_changed["current_hash"],
        serde_json::json!(first_hash)
    );
}

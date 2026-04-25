use tempfile::TempDir;

use crate::common;
#[test]
fn window_activate_json_contains_action_metadata() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "window",
            "activate",
            "--app",
            "Terminal",
            "--wait-ms",
            "25",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");

    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be valid json");
    assert_eq!(payload["schema_version"], serde_json::json!(1));
    assert_eq!(payload["ok"], serde_json::json!(true));
    assert_eq!(payload["command"], serde_json::json!("window.activate"));
    assert_eq!(
        payload["result"]["selected_app"],
        serde_json::json!("Terminal")
    );
    assert_eq!(
        payload["result"]["policy"]["dry_run"],
        serde_json::json!(false)
    );
    assert_eq!(payload["result"]["policy"]["retries"], serde_json::json!(0));
    assert_eq!(
        payload["result"]["policy"]["retry_delay_ms"],
        serde_json::json!(150)
    );
    assert_eq!(
        payload["result"]["policy"]["timeout_ms"],
        serde_json::json!(4000)
    );
    assert!(
        payload["result"]["meta"]["action_id"]
            .as_str()
            .unwrap()
            .contains("window.activate")
    );
}

#[test]
fn window_activate_error_includes_selector_and_fallback_hint() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "window",
            "activate",
            "--window-id",
            "999",
            "--wait-ms",
            "10",
        ],
    );

    assert_eq!(out.code, 1);
    assert_eq!(out.stdout_text(), "");
    let stderr = out.stderr_text();
    assert!(stderr.contains("--window-id 999"));
    assert!(
        stderr.contains("try --window-id <id> or --app <name> --window-title-contains <title>")
    );
}

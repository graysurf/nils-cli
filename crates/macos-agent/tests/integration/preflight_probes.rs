use tempfile::TempDir;

use crate::common;
use pretty_assertions::assert_eq;

#[test]
fn preflight_include_probes_reconciles_successful_screenshot() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "preflight",
            "--include-probes",
            "--strict",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("preflight json");
    assert_eq!(payload["result"]["status"], serde_json::json!("ready"));
    assert_eq!(
        payload["result"]["summary"]["warnings"],
        serde_json::json!(0)
    );
    assert_eq!(
        payload["result"]["permissions"]["screen_recording"],
        serde_json::json!("ready")
    );

    let checks = payload["result"]["checks"]
        .as_array()
        .expect("checks should be array");

    for id in ["probe_activate", "probe_input_hotkey", "probe_screenshot"] {
        assert!(
            checks.iter().any(|row| row["id"] == serde_json::json!(id)),
            "missing probe check `{id}`"
        );
    }

    let screen_recording = checks
        .iter()
        .find(|row| row["id"] == serde_json::json!("screen_recording"))
        .expect("screen_recording check should exist");
    assert_eq!(screen_recording["status"], serde_json::json!("ok"));
}

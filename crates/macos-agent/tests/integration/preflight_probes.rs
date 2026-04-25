use tempfile::TempDir;

use crate::common;
#[test]
fn preflight_include_probes_adds_probe_rows() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &["--format", "json", "preflight", "--include-probes"],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("preflight json");
    let checks = payload["result"]["checks"]
        .as_array()
        .expect("checks should be array");

    for id in ["probe_activate", "probe_input_hotkey", "probe_screenshot"] {
        assert!(
            checks.iter().any(|row| row["id"] == serde_json::json!(id)),
            "missing probe check `{id}`"
        );
    }
}

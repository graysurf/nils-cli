use tempfile::TempDir;

use crate::common;
#[test]
fn input_type_accepts_whitespace_and_punctuation() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "--timeout-ms",
            "15000",
            "input",
            "type",
            "--text",
            "hello, world!",
            "--submit",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(payload["command"], serde_json::json!("input.type"));
    assert_eq!(payload["result"]["text_length"], serde_json::json!(13));
    assert_eq!(payload["result"]["enter"], serde_json::json!(true));
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
        serde_json::json!(15000)
    );
}

#[test]
fn input_hotkey_json_reports_modifiers() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "--timeout-ms",
            "15000",
            "input",
            "hotkey",
            "--mods",
            "cmd,shift",
            "--key",
            "4",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    let mods = payload["result"]["mods"]
        .as_array()
        .expect("mods array")
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(mods, vec!["cmd".to_string(), "shift".to_string()]);
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
        serde_json::json!(15000)
    );
}

#[test]
fn input_hotkey_rejects_invalid_modifier() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &["input", "hotkey", "--mods", "cmd,nope", "--key", "4"],
    );

    assert_eq!(out.code, 2);
    assert_eq!(out.stdout_text(), "");
    assert!(out.stderr_text().contains("invalid modifier"));
}

#[test]
fn input_type_timeout_surfaces_as_runtime_error() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_OSASCRIPT_MODE", "timeout");
    let out = harness.run_with_options(
        cwd.path(),
        &["--timeout-ms", "10", "input", "type", "--text", "hello"],
        options,
    );

    assert_eq!(out.code, 1);
    assert_eq!(out.stdout_text(), "");
    assert!(out.stderr_text().contains("timed out"));
}

#[test]
fn input_keyboard_rejects_tsv_output_mode() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let type_out = harness.run(
        cwd.path(),
        &[
            "--format",
            "tsv",
            "input",
            "type",
            "--text",
            "hello",
            "--dry-run",
        ],
    );
    assert_eq!(type_out.code, 2);
    assert!(
        type_out
            .stderr_text()
            .contains("only supported for `windows list` and `apps list`")
    );

    let hotkey_out = harness.run(
        cwd.path(),
        &[
            "--format",
            "tsv",
            "input",
            "hotkey",
            "--mods",
            "cmd",
            "--key",
            "4",
            "--dry-run",
        ],
    );
    assert_eq!(hotkey_out.code, 2);
    assert!(
        hotkey_out
            .stderr_text()
            .contains("only supported for `windows list` and `apps list`")
    );
}

#[test]
fn ax_type_dry_run_reports_policy_and_text_length() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "--dry-run",
            "ax",
            "type",
            "--node-id",
            "1.1",
            "--text",
            "hello",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(payload["command"], serde_json::json!("ax.type"));
    assert_eq!(
        payload["result"]["applied_via"],
        serde_json::json!("dry-run")
    );
    assert_eq!(payload["result"]["text_length"], serde_json::json!(5));
    assert_eq!(
        payload["result"]["policy"]["dry_run"],
        serde_json::json!(true)
    );
}

#[test]
fn ax_type_reports_keyboard_fallback_when_backend_uses_it() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "AGENTS_MACOS_AGENT_AX_LIST_JSON",
            r#"{"nodes":[{"node_id":"1.1","role":"AXTextField","title":"Input","identifier":"input-1","enabled":true,"focused":true,"actions":[],"path":["1","1"]}],"warnings":[]}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_TYPE_JSON",
            r#"{"node_id":"1.1","matched_count":1,"applied_via":"keyboard-keystroke-fallback","text_length":5,"submitted":true,"used_keyboard_fallback":true}"#,
        );
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "ax",
            "type",
            "--node-id",
            "1.1",
            "--text",
            "hello",
            "--submit",
            "--allow-keyboard-fallback",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(
        payload["result"]["used_keyboard_fallback"],
        serde_json::json!(true)
    );
    assert_eq!(
        payload["result"]["applied_via"],
        serde_json::json!("keyboard-keystroke-fallback")
    );
    assert_eq!(payload["result"]["submitted"], serde_json::json!(true));
}

use tempfile::TempDir;

use crate::common;

#[test]
fn input_key_sends_named_key_without_modifiers() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let log = cwd.path().join("osascript.log");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_LOG", &log.to_string_lossy());

    let out = harness.run_with_options(
        cwd.path(),
        &["--format", "json", "input", "key", "--key", "return"],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(payload["command"], serde_json::json!("input.key"));
    assert_eq!(payload["result"]["key"], serde_json::json!("return"));
    let recorded = std::fs::read_to_string(log).expect("read osascript log");
    assert!(recorded.contains("key code 36"));
    assert!(!recorded.contains("using {"));
}

#[test]
fn input_key_count_uses_one_bounded_applescript_action() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let log = cwd.path().join("osascript.log");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_LOG", &log.to_string_lossy());

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format", "json", "input", "key", "--key", "left", "--count", "3",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let recorded = std::fs::read_to_string(log).expect("read osascript log");
    assert_eq!(recorded.matches("osascript ").count(), 1, "{recorded}");
    assert!(recorded.contains("repeat 3 times"), "{recorded}");
    assert_eq!(recorded.matches("key code 123").count(), 1, "{recorded}");
}

#[test]
fn input_key_and_hotkey_normalize_surrounding_whitespace() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let log = cwd.path().join("osascript.log");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_LOG", &log.to_string_lossy());

    let key_out = harness.run_with_options(
        cwd.path(),
        &["--format", "json", "input", "key", "--key", " x "],
        options.clone(),
    );
    assert_eq!(key_out.code, 0, "stderr: {}", key_out.stderr_text());
    let key_payload: serde_json::Value =
        serde_json::from_str(&key_out.stdout_text()).expect("key stdout should be json");
    assert_eq!(key_payload["result"]["key"], serde_json::json!("x"));

    let hotkey_out = harness.run_with_options(
        cwd.path(),
        &[
            "--format", "json", "input", "hotkey", "--mods", "cmd", "--key", " left ",
        ],
        options,
    );
    assert_eq!(hotkey_out.code, 0, "stderr: {}", hotkey_out.stderr_text());
    let hotkey_payload: serde_json::Value =
        serde_json::from_str(&hotkey_out.stdout_text()).expect("hotkey stdout should be json");
    assert_eq!(hotkey_payload["result"]["key"], serde_json::json!("left"));

    let recorded = std::fs::read_to_string(log).expect("read osascript log");
    assert!(recorded.contains("keystroke \"x\""), "{recorded}");
    assert!(
        recorded.contains("key code 123 using {command down}"),
        "{recorded}"
    );
    assert!(!recorded.contains("keystroke \" x \""), "{recorded}");
}

#[test]
fn input_hotkey_named_key_uses_key_code_with_modifiers() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let log = cwd.path().join("osascript.log");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_LOG", &log.to_string_lossy());

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format", "json", "input", "hotkey", "--mods", "cmd", "--key", "left",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let recorded = std::fs::read_to_string(log).expect("read osascript log");
    assert!(recorded.contains("key code 123 using {command down}"));
}

#[test]
fn input_key_rejects_unknown_named_key() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["input", "key", "--key", "unknown-key"]);

    assert_eq!(out.code, 2);
    assert_eq!(out.stdout_text(), "");
    assert!(out.stderr_text().contains("unsupported --key"));
}

#[test]
fn input_key_backend_error_keeps_input_key_operation() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_OSASCRIPT_MODE", "fail");

    let out = harness.run_with_options(
        cwd.path(),
        &["--error-format", "json", "input", "key", "--key", "return"],
        options,
    );

    assert_eq!(out.code, 1);
    let payload: serde_json::Value =
        serde_json::from_str(&out.stderr_text()).expect("stderr should be json");
    assert_eq!(
        payload["error"]["operation"],
        serde_json::json!("input.key")
    );
}

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

    let key_out = harness.run(
        cwd.path(),
        &[
            "--format",
            "tsv",
            "input",
            "key",
            "--key",
            "return",
            "--dry-run",
        ],
    );
    assert_eq!(key_out.code, 2);
    assert!(
        key_out
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

use tempfile::TempDir;

use crate::common;
#[test]
fn help_lists_command_groups() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["--help"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let text = format!("{}{}", out.stdout_text(), out.stderr_text());
    for token in [
        "preflight",
        "windows",
        "apps",
        "window",
        "input",
        "input-source",
        "ax",
        "observe",
        "wait",
        "scenario",
        "profile",
        "completion",
    ] {
        assert!(text.contains(token), "missing token in help: {token}");
    }
}

#[test]
fn ax_help_lists_subcommands() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["ax", "--help"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let text = format!("{}{}", out.stdout_text(), out.stderr_text());
    for token in ["list", "click", "type"] {
        assert!(text.contains(token), "missing token in ax help: {token}");
    }
}

#[test]
fn ax_click_rejects_mixed_selectors() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "ax",
            "click",
            "--node-id",
            "node-17",
            "--role",
            "AXButton",
            "--title-contains",
            "Save",
        ],
    );

    assert_eq!(out.code, 2);
    assert_eq!(out.stdout_text(), "");
    assert!(out.stderr_text().contains("cannot be used with"));
}

#[test]
fn input_source_current_and_switch_emit_json_payloads() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness.cmd_options(cwd.path()).with_env(
        "AGENTS_MACOS_AGENT_TEST_INPUT_SOURCE_CURRENT",
        "com.apple.keylayout.US",
    );

    let current = harness.run_with_options(
        cwd.path(),
        &["--format", "json", "input-source", "current"],
        options.clone(),
    );
    assert_eq!(current.code, 0, "stderr: {}", current.stderr_text());
    let current_payload: serde_json::Value =
        serde_json::from_str(&current.stdout_text()).expect("current should emit json");
    assert_eq!(
        current_payload["command"],
        serde_json::json!("input-source.current")
    );
    assert_eq!(
        current_payload["result"]["current"],
        serde_json::json!("com.apple.keylayout.US")
    );

    let switched = harness.run_with_options(
        cwd.path(),
        &["--format", "json", "input-source", "switch", "--id", "abc"],
        options,
    );
    assert_eq!(switched.code, 0, "stderr: {}", switched.stderr_text());
    let switch_payload: serde_json::Value =
        serde_json::from_str(&switched.stdout_text()).expect("switch should emit json");
    assert_eq!(
        switch_payload["command"],
        serde_json::json!("input-source.switch")
    );
    assert_eq!(
        switch_payload["result"]["previous"],
        serde_json::json!("com.apple.keylayout.US")
    );
    assert_eq!(
        switch_payload["result"]["current"],
        serde_json::json!("com.apple.keylayout.ABC")
    );
    assert_eq!(
        switch_payload["result"]["switched"],
        serde_json::json!(true)
    );
}

#[test]
fn windows_list_window_title_contains_emits_json_payload() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "windows",
            "list",
            "--app",
            "Terminal",
            "--window-title-contains",
            "Docs",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("windows json");
    assert_eq!(payload["command"], serde_json::json!("windows.list"));
    assert!(payload["result"]["windows"].is_array());
}

#[test]
fn input_type_submit_sets_enter_and_policy() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "--dry-run",
            "--retries",
            "2",
            "--retry-delay-ms",
            "9",
            "--timeout-ms",
            "1234",
            "input",
            "type",
            "--text",
            "hello",
            "--submit",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("submit payload json");

    assert_eq!(payload["command"], serde_json::json!("input.type"));
    assert_eq!(payload["result"]["text_length"], serde_json::json!(5));
    assert_eq!(payload["result"]["enter"], serde_json::json!(true));
    assert_eq!(
        payload["result"]["meta"]["dry_run"],
        serde_json::json!(true)
    );
    assert_eq!(
        payload["result"]["meta"]["attempts_used"],
        serde_json::json!(0)
    );
    assert!(payload["result"]["policy"].is_object());
}

#[test]
fn observe_screenshot_help_lists_if_changed_flags() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["observe", "screenshot", "--help"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let text = format!("{}{}", out.stdout_text(), out.stderr_text());
    assert!(text.contains("--if-changed"));
    assert!(text.contains("--if-changed-baseline"));
    assert!(text.contains("--if-changed-threshold"));
}

#[test]
fn ax_click_help_lists_wait_policy_overrides() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["ax", "click", "--help"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let text = format!("{}{}", out.stdout_text(), out.stderr_text());
    assert!(text.contains("--wait-timeout-ms"));
    assert!(text.contains("--wait-poll-ms"));
}

#[test]
fn preflight_json_smoke() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["--format", "json", "preflight"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");

    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(payload["schema_version"], serde_json::json!(1));
    assert_eq!(payload["command"], serde_json::json!("preflight"));
    let checks = payload["result"]["checks"]
        .as_array()
        .expect("preflight checks should be an array");
    assert!(
        checks
            .iter()
            .any(|check| check["id"] == serde_json::json!("ax_backend_capabilities")),
        "preflight checks should include ax_backend_capabilities row"
    );
}

#[test]
fn tsv_mode_is_rejected_for_input_commands() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &["--format", "tsv", "input", "click", "--x", "1", "--y", "2"],
    );

    assert_eq!(out.code, 2);
    assert_eq!(out.stdout_text(), "");
    assert!(
        out.stderr_text()
            .contains("only supported for `windows list` and `apps list`")
    );
}

#[test]
fn trace_command_label_stays_in_sync_with_runtime_mapping() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let trace_dir = cwd.path().join("trace-sync");
    let trace_dir_text = trace_dir.to_string_lossy().to_string();

    let out = harness.run(
        cwd.path(),
        &[
            "--trace",
            "--trace-dir",
            &trace_dir_text,
            "--format",
            "json",
            "input-source",
            "switch",
            "--id",
            "abc",
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let trace_path = std::fs::read_dir(&trace_dir)
        .expect("trace dir should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().map(|ext| ext == "json").unwrap_or(false))
        .expect("trace file should be created");
    let raw = std::fs::read_to_string(trace_path).expect("trace payload should be readable");
    let payload: serde_json::Value = serde_json::from_str(&raw).expect("trace payload is json");
    assert_eq!(payload["command"], serde_json::json!("input-source.switch"));
}

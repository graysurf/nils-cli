use tempfile::TempDir;

use crate::common;
#[test]
fn input_click_double_click_succeeds() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_CLICLICK_MODE", "ok");

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "input",
            "click",
            "--x",
            "200",
            "--y",
            "160",
            "--count",
            "2",
            "--timeout-ms",
            "10000",
            "--pre-wait-ms",
            "1",
            "--post-wait-ms",
            "1",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");
    assert!(out.stdout_text().contains("input.click"));
}

#[test]
fn input_click_uses_absolute_negative_coordinates_for_secondary_displays() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let log = cwd.path().join("cliclick.log");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_LOG", &log.to_string_lossy());

    let out = harness.run_with_options(
        cwd.path(),
        &["input", "click", "--x", "-120", "--y", "40"],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let recorded = std::fs::read_to_string(log).expect("read cliclick log");
    assert!(recorded.contains("cliclick c:=-120,40"));
}

#[test]
fn input_click_holds_and_releases_modifiers_in_one_backend_action() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let log = cwd.path().join("cliclick.log");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_LOG", &log.to_string_lossy());

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "input",
            "click",
            "--x",
            "120",
            "--y",
            "40",
            "--mods",
            "cmd,shift",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(
        payload["result"]["mods"],
        serde_json::json!(["cmd", "shift"])
    );
    let recorded = std::fs::read_to_string(log).expect("read cliclick log");
    assert!(
        recorded.contains("cliclick kd:cmd,shift c:120,40 ku:cmd,shift"),
        "{recorded}"
    );
}

#[test]
fn input_click_rejects_invalid_modifier() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "input", "click", "--x", "1", "--y", "2", "--mods", "cmd,nope",
        ],
    );

    assert_eq!(out.code, 2);
    assert!(out.stderr_text().contains("invalid modifier"));
}

#[test]
fn input_click_invalid_count_is_usage_error() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &["input", "click", "--x", "1", "--y", "2", "--count", "0"],
    );

    assert_eq!(out.code, 2);
    assert_eq!(out.stdout_text(), "");
    assert!(out.stderr_text().starts_with("error:"));
}

#[test]
fn input_click_runtime_error_from_cliclick_is_concise() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_CLICLICK_MODE", "fail");
    let out = harness.run_with_options(
        cwd.path(),
        &["input", "click", "--x", "10", "--y", "10"],
        options,
    );

    assert_eq!(out.code, 1);
    assert_eq!(out.stdout_text(), "");
    assert!(
        out.stderr_text()
            .contains("input.click failed via `cliclick`")
    );
}

#[test]
fn input_click_dry_run_does_not_execute_backend() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_CLICLICK_MODE", "fail");
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--dry-run",
            "input",
            "click",
            "--x",
            "10",
            "--y",
            "10",
            "--format",
            "json",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(
        payload["result"]["policy"]["dry_run"],
        serde_json::json!(true)
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
    assert_eq!(
        payload["result"]["meta"]["dry_run"],
        serde_json::json!(true)
    );
}

#[test]
fn ax_click_dry_run_reports_policy_and_meta() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "--dry-run",
            "ax",
            "click",
            "--node-id",
            "1.1",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(payload["command"], serde_json::json!("ax.click"));
    assert_eq!(
        payload["result"]["policy"]["dry_run"],
        serde_json::json!(true)
    );
    assert_eq!(payload["result"]["action"], serde_json::json!("dry-run"));
}

#[test]
fn ax_click_coordinate_fallback_executes_with_backend_coordinates() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "AGENTS_MACOS_AGENT_AX_LIST_JSON",
            r#"{"nodes":[{"node_id":"1.1","role":"AXButton","title":"Run","identifier":"run-btn","enabled":true,"focused":false,"actions":["AXPress"],"path":["1","1"]}],"warnings":[]}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_CLICK_JSON",
            r#"{"node_id":"1.1","matched_count":1,"action":"ax-press-fallback","used_coordinate_fallback":true,"fallback_x":320,"fallback_y":240}"#,
        );
    let out = harness.run_with_options(
        cwd.path(),
        &["--format", "json", "ax", "click", "--node-id", "1.1"],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(
        payload["result"]["used_coordinate_fallback"],
        serde_json::json!(true)
    );
    assert_eq!(
        payload["result"]["action"],
        serde_json::json!("coordinate-fallback")
    );
}

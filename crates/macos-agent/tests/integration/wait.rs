use tempfile::TempDir;

use crate::common;
#[test]
fn wait_sleep_returns_single_result_line() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["wait", "sleep", "--ms", "1"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");
    assert!(out.stdout_text().starts_with("wait.sleep\t"));
}

#[test]
fn wait_app_active_succeeds_for_terminal_fixture() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "wait",
            "app-active",
            "--app",
            "Terminal",
            "--timeout-ms",
            "50",
            "--poll-ms",
            "5",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");
}

#[test]
fn wait_app_active_succeeds_for_terminal_bundle_id_fixture() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "wait",
            "app-active",
            "--bundle-id",
            "com.apple.Terminal",
            "--timeout-ms",
            "50",
            "--poll-ms",
            "5",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");
}

#[test]
fn wait_app_active_timeout_is_runtime_error() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "wait",
            "app-active",
            "--app",
            "Finder",
            "--timeout-ms",
            "20",
            "--poll-ms",
            "5",
        ],
    );

    assert_eq!(out.code, 1);
    assert_eq!(out.stdout_text(), "");
    assert!(
        out.stderr_text()
            .contains("timed out waiting for app-active")
    );
}

#[test]
fn wait_policy_flags_aliases_work_for_wait_commands() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "wait",
            "app-active",
            "--app",
            "Terminal",
            "--wait-timeout-ms",
            "60",
            "--wait-poll-ms",
            "5",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");
}

#[test]
fn wait_window_present_succeeds_for_terminal() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "wait",
            "window-present",
            "--app",
            "Terminal",
            "--window-title-contains",
            "Docs",
            "--timeout-ms",
            "50",
            "--poll-ms",
            "5",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");
}

#[test]
fn wait_window_present_rejects_window_title_contains_without_app() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "wait",
            "window-present",
            "--window-title-contains",
            "Docs",
            "--timeout-ms",
            "50",
            "--poll-ms",
            "5",
        ],
    );

    assert_eq!(out.code, 2);
    assert_eq!(out.stdout_text(), "");
    assert!(
        out.stderr_text().contains("requires")
            || out
                .stderr_text()
                .contains("required arguments were not provided")
    );
}

#[test]
fn wait_json_and_tsv_contracts() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let json_out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "wait",
            "app-active",
            "--app",
            "Terminal",
            "--timeout-ms",
            "50",
            "--poll-ms",
            "5",
        ],
    );
    assert_eq!(json_out.code, 0, "stderr: {}", json_out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&json_out.stdout_text()).expect("json payload");
    assert_eq!(payload["command"], serde_json::json!("wait.app-active"));

    let tsv_out = harness.run(
        cwd.path(),
        &["--format", "tsv", "wait", "app-active", "--app", "Terminal"],
    );
    assert_eq!(tsv_out.code, 2);
    assert_eq!(tsv_out.stdout_text(), "");
    assert!(
        tsv_out
            .stderr_text()
            .contains("only supported for `windows list` and `apps list`")
    );
}

#[test]
fn wait_ax_present_reports_matched_count_in_json() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness.cmd_options(cwd.path()).with_env(
        "AGENTS_MACOS_AGENT_AX_LIST_JSON",
        r#"{"nodes":[{"node_id":"1.1","role":"AXButton","enabled":true,"focused":false}],"warnings":[]}"#,
    );

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "wait",
            "ax-present",
            "--role",
            "AXButton",
            "--timeout-ms",
            "50",
            "--poll-ms",
            "5",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(payload["command"], serde_json::json!("wait.ax-present"));
    assert_eq!(payload["result"]["matched_count"], serde_json::json!(1));
}

#[test]
fn wait_ax_unique_timeout_reports_last_match_count_hint() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "AGENTS_MACOS_AGENT_AX_LIST_JSON",
            r#"{"nodes":[{"node_id":"1.1","role":"AXButton","enabled":true,"focused":false},{"node_id":"1.2","role":"AXButton","enabled":true,"focused":false}],"warnings":[]}"#,
        );

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "wait",
            "ax-unique",
            "--role",
            "AXButton",
            "--timeout-ms",
            "20",
            "--poll-ms",
            "5",
        ],
        options,
    );

    assert_eq!(out.code, 1);
    let payload: serde_json::Value =
        serde_json::from_str(&out.stderr_text()).expect("stderr should be json");
    assert_eq!(
        payload["error"]["operation"],
        serde_json::json!("wait.ax-unique")
    );
    let has_hint = payload["error"]["hints"]
        .as_array()
        .map(|hints| {
            hints.iter().any(|hint| {
                hint.as_str()
                    .unwrap_or("")
                    .contains("Last selector match count before timeout: 2")
            })
        })
        .unwrap_or(false);
    assert!(
        has_hint,
        "expected last-match-count hint in wait.ax-unique error"
    );
}

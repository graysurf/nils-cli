use serde_json::json;
use tempfile::TempDir;

use crate::common;
#[test]
fn ax_attr_get_json_supports_override_payload() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness.cmd_options(cwd.path()).with_env(
        "AGENTS_MACOS_AGENT_AX_ATTR_GET_JSON",
        r#"{"node_id":"2.1","matched_count":1,"name":"AXRole","value":{"role":"AXButton"}}"#,
    );

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "ax",
            "attr",
            "get",
            "--node-id",
            "2.1",
            "--name",
            "AXRole",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value = serde_json::from_str(&out.stdout_text()).expect("stdout json");
    assert_eq!(payload["command"], json!("ax.attr.get"));
    assert_eq!(payload["result"]["name"], json!("AXRole"));
    assert_eq!(payload["result"]["value"]["role"], json!("AXButton"));
}

#[test]
fn ax_attr_set_dry_run_bool_value_type_is_reported() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "--dry-run",
            "ax",
            "attr",
            "set",
            "--node-id",
            "1.2",
            "--name",
            "AXEnabled",
            "--value",
            "true",
            "--value-type",
            "bool",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value = serde_json::from_str(&out.stdout_text()).expect("stdout json");
    assert_eq!(payload["command"], json!("ax.attr.set"));
    assert_eq!(payload["result"]["applied"], json!(false));
    assert_eq!(payload["result"]["value_type"], json!("bool"));
}

#[test]
fn ax_attr_set_rejects_invalid_bool_value() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "ax",
            "attr",
            "set",
            "--node-id",
            "1.2",
            "--name",
            "AXEnabled",
            "--value",
            "maybe",
            "--value-type",
            "bool",
        ],
    );

    assert_eq!(out.code, 2);
    assert_eq!(out.stdout_text(), "");
    assert!(out.stderr_text().contains("true or false"));
}

#[test]
fn ax_attr_set_rejects_non_finite_number_value() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "ax",
            "attr",
            "set",
            "--node-id",
            "1.2",
            "--name",
            "AXValue",
            "--value",
            "NaN",
            "--value-type",
            "number",
        ],
    );

    assert_eq!(out.code, 2);
    assert_eq!(out.stdout_text(), "");
    assert!(out.stderr_text().contains("finite number"));
}

#[test]
fn ax_action_perform_text_output_is_emitted() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "ax",
            "action",
            "perform",
            "--node-id",
            "1.1",
            "--name",
            "AXPress",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert!(out.stdout_text().contains("ax.action.perform"));
    assert!(out.stdout_text().contains("performed=true"));
}

#[test]
fn ax_session_start_list_stop_json_contracts() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "AGENTS_MACOS_AGENT_AX_SESSION_START_JSON",
            r#"{"session_id":"axs-demo","app":"Arc","bundle_id":"company.thebrowser.Browser","pid":2001,"window_title_contains":"Inbox","created_at_ms":1700000001111,"created":true}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_SESSION_LIST_JSON",
            r#"{"sessions":[{"session_id":"axs-demo","app":"Arc","bundle_id":"company.thebrowser.Browser","pid":2001,"window_title_contains":"Inbox","created_at_ms":1700000001111}]}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_SESSION_STOP_JSON",
            r#"{"session_id":"axs-demo","removed":true}"#,
        );

    let start_out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "ax",
            "session",
            "start",
            "--app",
            "Arc",
            "--session-id",
            "axs-demo",
            "--window-title-contains",
            "Inbox",
        ],
        options.clone(),
    );
    assert_eq!(start_out.code, 0, "stderr: {}", start_out.stderr_text());
    let start_payload: serde_json::Value =
        serde_json::from_str(&start_out.stdout_text()).expect("stdout json");
    assert_eq!(start_payload["command"], json!("ax.session.start"));
    assert_eq!(start_payload["result"]["session_id"], json!("axs-demo"));
    assert_eq!(start_payload["result"]["created"], json!(true));

    let list_out = harness.run_with_options(
        cwd.path(),
        &["--format", "json", "ax", "session", "list"],
        options.clone(),
    );
    assert_eq!(list_out.code, 0, "stderr: {}", list_out.stderr_text());
    let list_payload: serde_json::Value =
        serde_json::from_str(&list_out.stdout_text()).expect("stdout json");
    assert_eq!(list_payload["command"], json!("ax.session.list"));
    assert_eq!(
        list_payload["result"]["sessions"][0]["session_id"],
        json!("axs-demo")
    );

    let stop_out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "ax",
            "session",
            "stop",
            "--session-id",
            "axs-demo",
        ],
        options,
    );
    assert_eq!(stop_out.code, 0, "stderr: {}", stop_out.stderr_text());
    let stop_payload: serde_json::Value =
        serde_json::from_str(&stop_out.stdout_text()).expect("stdout json");
    assert_eq!(stop_payload["command"], json!("ax.session.stop"));
    assert_eq!(stop_payload["result"]["removed"], json!(true));
}

#[test]
fn ax_session_text_output_redacts_session_ids() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "AGENTS_MACOS_AGENT_AX_SESSION_START_JSON",
            r#"{"session_id":"axs-demo","app":"Arc","bundle_id":"company.thebrowser.Browser","pid":2001,"window_title_contains":"Inbox","created_at_ms":1700000001111,"created":true}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_SESSION_LIST_JSON",
            r#"{"sessions":[{"session_id":"axs-demo","app":"Arc","bundle_id":"company.thebrowser.Browser","pid":2001,"window_title_contains":"Inbox","created_at_ms":1700000001111}]}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_SESSION_STOP_JSON",
            r#"{"session_id":"axs-demo","removed":true}"#,
        );

    let start_out = harness.run_with_options(
        cwd.path(),
        &[
            "ax",
            "session",
            "start",
            "--app",
            "Arc",
            "--session-id",
            "axs-demo",
            "--window-title-contains",
            "Inbox",
        ],
        options.clone(),
    );
    assert_eq!(start_out.code, 0, "stderr: {}", start_out.stderr_text());
    assert!(start_out.stdout_text().contains("ax.session.start"));
    assert!(start_out.stdout_text().contains("session_id=redacted"));
    assert!(!start_out.stdout_text().contains("axs-demo"));

    let list_out =
        harness.run_with_options(cwd.path(), &["ax", "session", "list"], options.clone());
    assert_eq!(list_out.code, 0, "stderr: {}", list_out.stderr_text());
    assert!(list_out.stdout_text().contains("ax.session.list"));
    assert!(list_out.stdout_text().contains("session_id=redacted"));
    assert!(!list_out.stdout_text().contains("axs-demo"));

    let stop_out = harness.run_with_options(
        cwd.path(),
        &["ax", "session", "stop", "--session-id", "axs-demo"],
        options,
    );
    assert_eq!(stop_out.code, 0, "stderr: {}", stop_out.stderr_text());
    assert!(stop_out.stdout_text().contains("ax.session.stop"));
    assert!(stop_out.stdout_text().contains("session_id=redacted"));
    assert!(!stop_out.stdout_text().contains("axs-demo"));
}

#[test]
fn ax_watch_start_poll_stop_json_contracts() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "AGENTS_MACOS_AGENT_AX_WATCH_START_JSON",
            r#"{"watch_id":"axw-demo","session_id":"axs-demo","events":["AXTitleChanged","AXFocusedUIElementChanged"],"max_buffer":64,"started":true}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_WATCH_POLL_JSON",
            r#"{"watch_id":"axw-demo","events":[{"watch_id":"axw-demo","event":"AXTitleChanged","at_ms":1700000002222,"role":"AXButton","title":"Save","identifier":"save-btn","pid":2001}],"dropped":0,"running":true}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_WATCH_STOP_JSON",
            r#"{"watch_id":"axw-demo","stopped":true,"drained":1}"#,
        );

    let start_out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "ax",
            "watch",
            "start",
            "--session-id",
            "axs-demo",
            "--watch-id",
            "axw-demo",
            "--events",
            "AXTitleChanged,AXFocusedUIElementChanged",
            "--max-buffer",
            "64",
        ],
        options.clone(),
    );
    assert_eq!(start_out.code, 0, "stderr: {}", start_out.stderr_text());
    let start_payload: serde_json::Value =
        serde_json::from_str(&start_out.stdout_text()).expect("stdout json");
    assert_eq!(start_payload["command"], json!("ax.watch.start"));
    assert_eq!(start_payload["result"]["watch_id"], json!("axw-demo"));
    assert_eq!(start_payload["result"]["started"], json!(true));

    let poll_out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "ax",
            "watch",
            "poll",
            "--watch-id",
            "axw-demo",
            "--limit",
            "10",
        ],
        options.clone(),
    );
    assert_eq!(poll_out.code, 0, "stderr: {}", poll_out.stderr_text());
    let poll_payload: serde_json::Value =
        serde_json::from_str(&poll_out.stdout_text()).expect("stdout json");
    assert_eq!(poll_payload["command"], json!("ax.watch.poll"));
    assert_eq!(
        poll_payload["result"]["events"][0]["event"],
        json!("AXTitleChanged")
    );

    let stop_out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "ax",
            "watch",
            "stop",
            "--watch-id",
            "axw-demo",
        ],
        options,
    );
    assert_eq!(stop_out.code, 0, "stderr: {}", stop_out.stderr_text());
    let stop_payload: serde_json::Value =
        serde_json::from_str(&stop_out.stdout_text()).expect("stdout json");
    assert_eq!(stop_payload["command"], json!("ax.watch.stop"));
    assert_eq!(stop_payload["result"]["drained"], json!(1));
}

#[test]
fn ax_watch_start_text_output_redacts_session_id() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness.cmd_options(cwd.path()).with_env(
        "AGENTS_MACOS_AGENT_AX_WATCH_START_JSON",
        r#"{"watch_id":"axw-demo","session_id":"axs-demo","events":["AXTitleChanged","AXFocusedUIElementChanged"],"max_buffer":64,"started":true}"#,
    );

    let start_out = harness.run_with_options(
        cwd.path(),
        &[
            "ax",
            "watch",
            "start",
            "--session-id",
            "axs-demo",
            "--watch-id",
            "axw-demo",
            "--events",
            "AXTitleChanged,AXFocusedUIElementChanged",
            "--max-buffer",
            "64",
        ],
        options,
    );
    assert_eq!(start_out.code, 0, "stderr: {}", start_out.stderr_text());
    assert!(start_out.stdout_text().contains("ax.watch.start"));
    assert!(start_out.stdout_text().contains("session_id=redacted"));
    assert!(!start_out.stdout_text().contains("axs-demo"));
}

#[test]
fn ax_commands_can_force_hammerspoon_backend_for_list_click_type() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_AX_BACKEND", "hammerspoon")
        .with_env(
            "AGENTS_MACOS_AGENT_AX_LIST_JSON",
            r#"{"nodes":[{"node_id":"1.1","role":"AXButton","title":"Run","identifier":"run-btn","enabled":true,"focused":false,"actions":["AXPress"],"path":["1","1"]},{"node_id":"1.2","role":"AXTextField","title":"Search","identifier":"search-field","enabled":true,"focused":true,"actions":["AXSetValue"],"path":["1","2"]}],"warnings":[]}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_CLICK_JSON",
            r#"{"node_id":"1.1","matched_count":1,"action":"ax-press","used_coordinate_fallback":false}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_TYPE_JSON",
            r#"{"node_id":"1.2","matched_count":1,"applied_via":"ax-set-value","text_length":4,"submitted":true,"used_keyboard_fallback":false}"#,
        );

    let list_out = harness.run_with_options(
        cwd.path(),
        &["--format", "json", "ax", "list", "--role", "AXButton"],
        options.clone(),
    );
    assert_eq!(list_out.code, 0, "stderr: {}", list_out.stderr_text());
    let list_payload: serde_json::Value =
        serde_json::from_str(&list_out.stdout_text()).expect("stdout json");
    assert_eq!(list_payload["command"], json!("ax.list"));
    assert_eq!(
        list_payload["result"]["nodes"][0]["identifier"],
        json!("run-btn")
    );

    let click_out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "ax",
            "click",
            "--node-id",
            "1.1",
            "--allow-coordinate-fallback",
        ],
        options.clone(),
    );
    assert_eq!(click_out.code, 0, "stderr: {}", click_out.stderr_text());
    let click_payload: serde_json::Value =
        serde_json::from_str(&click_out.stdout_text()).expect("stdout json");
    assert_eq!(click_payload["command"], json!("ax.click"));
    assert_eq!(click_payload["result"]["action"], json!("ax-press"));

    let type_out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "ax",
            "type",
            "--node-id",
            "1.2",
            "--text",
            "test",
            "--submit",
        ],
        options,
    );
    assert_eq!(type_out.code, 0, "stderr: {}", type_out.stderr_text());
    let type_payload: serde_json::Value =
        serde_json::from_str(&type_out.stdout_text()).expect("stdout json");
    assert_eq!(type_payload["command"], json!("ax.type"));
    assert_eq!(type_payload["result"]["submitted"], json!(true));
}

#[test]
fn ax_click_gate_and_postcondition_metadata_are_emitted_in_json() {
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
            r#"{"node_id":"1.1","matched_count":1,"action":"ax-press","used_coordinate_fallback":false}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_ATTR_GET_JSON",
            r#"{"node_id":"1.1","matched_count":1,"name":"AXRole","value":"AXButton"}"#,
        );

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "ax",
            "click",
            "--app",
            "Terminal",
            "--node-id",
            "1.1",
            "--gate-window-present",
            "--gate-ax-present",
            "--gate-ax-unique",
            "--postcondition-focused",
            "false",
            "--postcondition-attribute",
            "AXRole",
            "--postcondition-attribute-value",
            "AXButton",
            "--postcondition-timeout-ms",
            "50",
            "--postcondition-poll-ms",
            "5",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value = serde_json::from_str(&out.stdout_text()).expect("stdout json");
    assert_eq!(payload["command"], json!("ax.click"));
    assert_eq!(
        payload["result"]["gates"]["checks"][0]["gate"],
        json!("window-present")
    );
    assert_eq!(
        payload["result"]["gates"]["checks"][1]["gate"],
        json!("ax-present")
    );
    assert_eq!(
        payload["result"]["gates"]["checks"][2]["gate"],
        json!("ax-unique")
    );
    assert_eq!(
        payload["result"]["postconditions"]["checks"][0]["check"],
        json!("focused=false")
    );
    assert_eq!(
        payload["result"]["postconditions"]["checks"][1]["attribute"],
        json!("AXRole")
    );
}

#[test]
fn ax_type_postcondition_mismatch_has_distinct_operation() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "AGENTS_MACOS_AGENT_AX_LIST_JSON",
            r#"{"nodes":[{"node_id":"1.2","role":"AXTextField","title":"Search","identifier":"search-field","enabled":true,"focused":false,"actions":["AXSetValue"],"path":["1","2"]}],"warnings":[]}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_TYPE_JSON",
            r#"{"node_id":"1.2","matched_count":1,"applied_via":"ax-set-value","text_length":4,"submitted":false,"used_keyboard_fallback":false}"#,
        );

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "ax",
            "type",
            "--node-id",
            "1.2",
            "--text",
            "test",
            "--postcondition-focused",
            "true",
            "--postcondition-timeout-ms",
            "20",
            "--postcondition-poll-ms",
            "5",
        ],
        options,
    );

    assert_eq!(out.code, 1);
    let payload: serde_json::Value = serde_json::from_str(&out.stderr_text()).expect("stderr json");
    assert_eq!(
        payload["error"]["operation"],
        json!("ax.type.postcondition")
    );
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("postcondition mismatch")
    );
}

#[test]
fn ax_click_gate_timeout_reports_actionable_gate_operation() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "ax",
            "click",
            "--app",
            "MissingApp",
            "--node-id",
            "1.1",
            "--gate-window-present",
            "--gate-timeout-ms",
            "20",
            "--gate-poll-ms",
            "5",
        ],
    );

    assert_eq!(out.code, 1);
    let payload: serde_json::Value =
        serde_json::from_str(&out.stderr_text()).expect("stderr should be json");
    assert_eq!(
        payload["error"]["operation"],
        json!("ax.click.gate.window-present")
    );
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("pre-action gate")
    );
}

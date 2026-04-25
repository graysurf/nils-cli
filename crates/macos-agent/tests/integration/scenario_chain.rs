use tempfile::TempDir;

use crate::common;
#[test]
fn scenario_chain_activate_wait_click_type_and_observe_succeeds() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let screenshot_path = cwd.path().join("scenario-chain.png");
    let screenshot_path_text = screenshot_path.to_string_lossy().to_string();

    let activate_out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "--retries",
            "1",
            "window",
            "activate",
            "--app",
            "Terminal",
            "--wait-ms",
            "25",
        ],
    );
    assert_eq!(
        activate_out.code,
        0,
        "stderr: {}",
        activate_out.stderr_text()
    );
    let activate_payload: serde_json::Value =
        serde_json::from_str(&activate_out.stdout_text()).expect("window.activate json");
    assert_eq!(
        activate_payload["command"],
        serde_json::json!("window.activate")
    );
    assert_eq!(
        activate_payload["result"]["selected_app"],
        serde_json::json!("Terminal")
    );
    assert_eq!(
        activate_payload["result"]["policy"]["retries"],
        serde_json::json!(1)
    );

    let wait_out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "wait",
            "app-active",
            "--app",
            "Terminal",
            "--timeout-ms",
            "250",
            "--poll-ms",
            "10",
        ],
    );
    assert_eq!(wait_out.code, 0, "stderr: {}", wait_out.stderr_text());
    let wait_payload: serde_json::Value =
        serde_json::from_str(&wait_out.stdout_text()).expect("wait json");
    assert_eq!(
        wait_payload["command"],
        serde_json::json!("wait.app-active")
    );

    let click_out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "--retries",
            "1",
            "input",
            "click",
            "--x",
            "200",
            "--y",
            "160",
            "--count",
            "1",
            "--pre-wait-ms",
            "1",
            "--post-wait-ms",
            "1",
        ],
    );
    assert_eq!(click_out.code, 0, "stderr: {}", click_out.stderr_text());
    let click_payload: serde_json::Value =
        serde_json::from_str(&click_out.stdout_text()).expect("input.click json");
    assert_eq!(click_payload["command"], serde_json::json!("input.click"));
    assert_eq!(
        click_payload["result"]["policy"]["retries"],
        serde_json::json!(1)
    );

    let type_out = harness.run(
        cwd.path(),
        &["--format", "json", "input", "type", "--text", "hello world"],
    );
    assert_eq!(type_out.code, 0, "stderr: {}", type_out.stderr_text());
    let type_payload: serde_json::Value =
        serde_json::from_str(&type_out.stdout_text()).expect("input.type json");
    assert_eq!(type_payload["command"], serde_json::json!("input.type"));
    assert_eq!(type_payload["result"]["text_length"], serde_json::json!(11));

    let observe_out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "observe",
            "screenshot",
            "--active-window",
            "--path",
            &screenshot_path_text,
        ],
    );
    assert_eq!(observe_out.code, 0, "stderr: {}", observe_out.stderr_text());
    let observe_payload: serde_json::Value =
        serde_json::from_str(&observe_out.stdout_text()).expect("observe json");
    assert_eq!(
        observe_payload["command"],
        serde_json::json!("observe.screenshot")
    );
    assert_eq!(
        observe_payload["result"]["path"],
        serde_json::json!(screenshot_path_text)
    );
    assert!(
        screenshot_path.is_file(),
        "scenario screenshot should exist"
    );
}

#[test]
fn scenario_run_executes_fixture_steps() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("scenario-basic.json");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "scenario",
            "run",
            "--file",
            fixture.to_str().unwrap(),
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("scenario run json");
    assert_eq!(payload["command"], serde_json::json!("scenario.run"));
    assert_eq!(payload["result"]["total_steps"], serde_json::json!(3));
    assert_eq!(payload["result"]["passed_steps"], serde_json::json!(3));
    assert_eq!(payload["result"]["failed_steps"], serde_json::json!(0));
    let steps = payload["result"]["steps"]
        .as_array()
        .expect("steps should be an array");

    let preflight = steps
        .iter()
        .find(|step| step["step_id"] == serde_json::json!("preflight"))
        .expect("preflight step should exist");
    assert_eq!(preflight["operation"], serde_json::json!("preflight"));
    assert_eq!(preflight["ax_path"], serde_json::Value::Null);
    assert_eq!(preflight["fallback_used"], serde_json::Value::Null);

    let activate = steps
        .iter()
        .find(|step| step["step_id"] == serde_json::json!("activate"))
        .expect("activate step should exist");
    assert_eq!(activate["operation"], serde_json::json!("window.activate"));
    assert_eq!(activate["ax_path"], serde_json::Value::Null);
    assert_eq!(activate["fallback_used"], serde_json::Value::Null);

    let click = steps
        .iter()
        .find(|step| step["step_id"] == serde_json::json!("click-dry-run"))
        .expect("click step should exist");
    assert_eq!(click["operation"], serde_json::json!("input.click"));
    assert_eq!(click["ax_path"], serde_json::Value::Null);
    assert_eq!(click["fallback_used"], serde_json::Value::Null);
}

#[test]
fn scenario_steps_report_ax_path_and_fallback_state() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("scenario-ax.json");

    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "AGENTS_MACOS_AGENT_AX_LIST_JSON",
            r#"{"nodes":[{"node_id":"1.1","role":"AXButton","enabled":true,"focused":false,"actions":["AXPress"],"path":["1","1"]}],"warnings":[]}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_CLICK_JSON",
            r#"{"node_id":"1.1","matched_count":1,"action":"ax-press-fallback","used_coordinate_fallback":true,"fallback_x":120,"fallback_y":220}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_TYPE_JSON",
            r#"{"node_id":"1.1","matched_count":1,"applied_via":"keyboard-keystroke-fallback","text_length":11,"submitted":false,"used_keyboard_fallback":true}"#,
        );

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "scenario",
            "run",
            "--file",
            fixture.to_str().unwrap(),
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("scenario run json");
    let steps = payload["result"]["steps"]
        .as_array()
        .expect("steps should be an array");

    let click = steps
        .iter()
        .find(|step| step["step_id"] == serde_json::json!("ax-click"))
        .expect("ax-click step should exist");
    assert_eq!(click["operation"], serde_json::json!("ax.click"));
    assert_eq!(click["ax_path"], serde_json::json!("coordinate-fallback"));
    assert_eq!(click["fallback_used"], serde_json::json!(true));

    let typ = steps
        .iter()
        .find(|step| step["step_id"] == serde_json::json!("ax-type"))
        .expect("ax-type step should exist");
    assert_eq!(typ["operation"], serde_json::json!("ax.type"));
    assert_eq!(typ["ax_path"], serde_json::json!("keyboard-fallback"));
    assert_eq!(typ["fallback_used"], serde_json::json!(true));
}

#[test]
fn scenario_steps_report_ax_native_path_when_fallback_not_used() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("scenario-ax.json");

    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "AGENTS_MACOS_AGENT_AX_LIST_JSON",
            r#"{"nodes":[{"node_id":"1.1","role":"AXButton","enabled":true,"focused":false,"actions":["AXPress"],"path":["1","1"]}],"warnings":[]}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_CLICK_JSON",
            r#"{"node_id":"1.1","matched_count":1,"action":"ax-press","used_coordinate_fallback":false}"#,
        )
        .with_env(
            "AGENTS_MACOS_AGENT_AX_TYPE_JSON",
            r#"{"node_id":"1.1","matched_count":1,"applied_via":"ax-set-value","text_length":11,"submitted":false,"used_keyboard_fallback":false}"#,
        );

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "scenario",
            "run",
            "--file",
            fixture.to_str().unwrap(),
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("scenario run json");
    let steps = payload["result"]["steps"]
        .as_array()
        .expect("steps should be an array");

    let ax_list = steps
        .iter()
        .find(|step| step["step_id"] == serde_json::json!("ax-list"))
        .expect("ax-list step should exist");
    assert_eq!(ax_list["operation"], serde_json::json!("ax.list"));
    assert_eq!(ax_list["ax_path"], serde_json::Value::Null);
    assert_eq!(ax_list["fallback_used"], serde_json::Value::Null);

    let click = steps
        .iter()
        .find(|step| step["step_id"] == serde_json::json!("ax-click"))
        .expect("ax-click step should exist");
    assert_eq!(click["operation"], serde_json::json!("ax.click"));
    assert_eq!(click["ax_path"], serde_json::json!("ax-native"));
    assert_eq!(click["fallback_used"], serde_json::json!(false));

    let typ = steps
        .iter()
        .find(|step| step["step_id"] == serde_json::json!("ax-type"))
        .expect("ax-type step should exist");
    assert_eq!(typ["operation"], serde_json::json!("ax.type"));
    assert_eq!(typ["ax_path"], serde_json::json!("ax-native"));
    assert_eq!(typ["fallback_used"], serde_json::json!(false));
}

#[test]
fn scenario_ax_step_supports_gate_and_postcondition_flags() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let scenario_path = cwd.path().join("scenario-ax-gate-post.json");
    let scenario_json = serde_json::json!({
        "steps": [
            {
                "id": "ax-click-gated",
                "args": [
                    "--format", "json",
                    "--timeout-ms", "15000",
                    "ax", "click",
                    "--app", "Terminal",
                    "--node-id", "1.1",
                    "--gate-app-active",
                    "--gate-window-present",
                    "--gate-ax-present",
                    "--gate-ax-unique",
                    "--postcondition-focused", "false",
                    "--postcondition-attribute", "AXRole",
                    "--postcondition-attribute-value", "AXButton"
                ]
            }
        ]
    });
    std::fs::write(
        &scenario_path,
        serde_json::to_vec_pretty(&scenario_json).expect("scenario json bytes"),
    )
    .expect("write scenario");

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
            "scenario",
            "run",
            "--file",
            scenario_path.to_str().expect("scenario path"),
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("scenario json");
    assert_eq!(payload["result"]["passed_steps"], serde_json::json!(1));
    let step = payload["result"]["steps"][0].clone();
    assert_eq!(step["operation"], serde_json::json!("ax.click"));
    assert_eq!(step["ax_path"], serde_json::json!("ax-native"));

    let step_stdout = step["stdout"].as_str().expect("step stdout");
    let step_payload: serde_json::Value =
        serde_json::from_str(step_stdout).expect("step stdout json");
    assert_eq!(
        step_payload["result"]["gates"]["checks"][0]["gate"],
        serde_json::json!("app-active")
    );
    assert_eq!(
        step_payload["result"]["postconditions"]["checks"][1]["attribute"],
        serde_json::json!("AXRole")
    );
}

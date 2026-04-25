use tempfile::TempDir;

use crate::common;
#[test]
fn success_commands_write_stdout_only() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let cases: Vec<Vec<&str>> = vec![
        vec!["--format", "json", "preflight"],
        vec!["--format", "json", "ax", "list"],
        vec![
            "--format",
            "json",
            "--dry-run",
            "ax",
            "click",
            "--node-id",
            "1.1",
        ],
        vec![
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
        vec![
            "--format", "json", "window", "activate", "--app", "Terminal",
        ],
        vec![
            "--format",
            "json",
            "input",
            "click",
            "--x",
            "10",
            "--y",
            "10",
            "--dry-run",
        ],
        vec![
            "--format",
            "json",
            "input",
            "type",
            "--text",
            "hello",
            "--dry-run",
        ],
        vec![
            "--format",
            "json",
            "input",
            "hotkey",
            "--mods",
            "cmd",
            "--key",
            "4",
            "--dry-run",
        ],
    ];

    for args in cases {
        let out = harness.run(cwd.path(), &args);
        assert_eq!(out.code, 0, "args={args:?}, stderr={}", out.stderr_text());
        assert!(!out.stdout_text().trim().is_empty(), "args={args:?}");
        assert_eq!(out.stderr_text(), "", "args={args:?}");
    }
}

#[test]
fn preflight_permissions_contract_has_stable_keys() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["--format", "json", "preflight"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");

    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("preflight should emit json");
    assert_eq!(payload["command"], serde_json::json!("preflight"));

    let permissions = payload["result"]["permissions"]
        .as_object()
        .expect("result.permissions should be an object");
    let mut keys = permissions
        .keys()
        .map(|key| key.as_str())
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "accessibility",
            "automation",
            "hints",
            "ready",
            "screen_recording"
        ]
    );

    for key in ["screen_recording", "accessibility", "automation"] {
        let state = permissions
            .get(key)
            .and_then(|value| value.as_str())
            .expect("permission state should be a string");
        assert!(
            matches!(state, "ready" | "blocked" | "unknown"),
            "unexpected {key} state: {state}"
        );
    }
    assert!(
        permissions
            .get("ready")
            .and_then(|value| value.as_bool())
            .is_some(),
        "permissions.ready should be bool"
    );
    assert!(
        permissions
            .get("hints")
            .and_then(|value| value.as_array())
            .is_some(),
        "permissions.hints should be array"
    );
}

#[test]
fn mutating_commands_emit_unified_json_envelope_schema() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "window.activate",
            vec![
                "--format",
                "json",
                "--dry-run",
                "--retries",
                "2",
                "--retry-delay-ms",
                "9",
                "--timeout-ms",
                "1234",
                "window",
                "activate",
                "--app",
                "Terminal",
            ],
        ),
        (
            "input.click",
            vec![
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
                "click",
                "--x",
                "10",
                "--y",
                "10",
            ],
        ),
        (
            "input.type",
            vec![
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
        ),
        (
            "input.hotkey",
            vec![
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
                "hotkey",
                "--mods",
                "cmd",
                "--key",
                "4",
            ],
        ),
        (
            "ax.click",
            vec![
                "--format",
                "json",
                "--dry-run",
                "--retries",
                "2",
                "--retry-delay-ms",
                "9",
                "--timeout-ms",
                "1234",
                "ax",
                "click",
                "--node-id",
                "1.1",
            ],
        ),
        (
            "ax.type",
            vec![
                "--format",
                "json",
                "--dry-run",
                "--retries",
                "2",
                "--retry-delay-ms",
                "9",
                "--timeout-ms",
                "1234",
                "ax",
                "type",
                "--node-id",
                "1.1",
                "--text",
                "hello",
            ],
        ),
    ];

    for (command, args) in cases {
        let out = harness.run(cwd.path(), &args);
        assert_eq!(out.code, 0, "args={args:?}, stderr={}", out.stderr_text());
        assert_eq!(out.stderr_text(), "", "args={args:?}");

        let payload: serde_json::Value =
            serde_json::from_str(&out.stdout_text()).expect("mutating command should emit json");
        assert_action_envelope_contract(&payload, command);
    }
}

#[test]
fn error_commands_write_stderr_only_with_error_prefix() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let cases: Vec<Vec<&str>> = vec![
        vec!["input", "hotkey", "--mods", "invalid", "--key", "4"],
        vec!["observe", "screenshot", "--window-id", "999"],
        vec!["input", "type", "--text", ""],
    ];

    for args in cases {
        let out = harness.run(cwd.path(), &args);
        assert!(out.code != 0, "args={args:?}");
        assert_eq!(out.stdout_text(), "", "args={args:?}");
        assert!(out.stderr_text().starts_with("error:"), "args={args:?}");
    }
}

#[test]
fn error_format_json_emits_machine_parseable_payload() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "input",
            "hotkey",
            "--mods",
            "invalid",
            "--key",
            "4",
        ],
    );

    assert_eq!(out.code, 2);
    assert_eq!(out.stdout_text(), "");
    let payload: serde_json::Value =
        serde_json::from_str(&out.stderr_text()).expect("stderr should be json");
    assert_eq!(payload["schema_version"], serde_json::json!(1));
    assert_eq!(payload["ok"], serde_json::json!(false));
    assert_eq!(payload["error"]["category"], serde_json::json!("usage"));
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("invalid modifier")
    );
    assert!(
        payload["error"]["hints"].is_array() || payload["error"]["hints"].is_null(),
        "hints should be an array when present"
    );
}

#[test]
fn trace_writes_artifacts_for_success_and_failure() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let trace_dir = cwd.path().join("trace-out");
    let trace_dir_text = trace_dir.to_string_lossy().to_string();

    let ok = harness.run(
        cwd.path(),
        &[
            "--trace",
            "--trace-dir",
            &trace_dir_text,
            "--format",
            "json",
            "input",
            "click",
            "--x",
            "1",
            "--y",
            "2",
            "--dry-run",
        ],
    );
    assert_eq!(ok.code, 0, "stderr: {}", ok.stderr_text());

    let err = harness.run(
        cwd.path(),
        &[
            "--trace",
            "--trace-dir",
            &trace_dir_text,
            "--error-format",
            "json",
            "input",
            "hotkey",
            "--mods",
            "invalid",
            "--key",
            "4",
        ],
    );
    assert_eq!(err.code, 2);

    let entries = std::fs::read_dir(&trace_dir)
        .expect("trace dir")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .map(|ext| ext == "json")
                .unwrap_or(false)
        })
        .map(|entry| {
            let path = entry.path();
            let raw = std::fs::read_to_string(path).expect("trace payload should be readable");
            serde_json::from_str::<serde_json::Value>(&raw).expect("trace payload should be json")
        })
        .collect::<Vec<_>>();
    assert!(
        entries.len() >= 2,
        "expected at least two trace files, got {}",
        entries.len()
    );

    let success = entries
        .iter()
        .find(|payload| {
            payload["command"] == serde_json::json!("input.click")
                && payload["ok"] == serde_json::json!(true)
        })
        .expect("success trace payload should exist");
    assert_eq!(success["schema_version"], serde_json::json!(1));
    assert!(success["elapsed_ms"].as_u64().is_some());
    assert!(success["args"].is_array());
    assert_eq!(success["error"], serde_json::Value::Null);
    assert_eq!(success["policy"]["dry_run"], serde_json::json!(true));
    assert_eq!(success["policy"]["retries"], serde_json::json!(0));

    let failure = entries
        .iter()
        .find(|payload| {
            payload["command"] == serde_json::json!("input.hotkey")
                && payload["ok"] == serde_json::json!(false)
        })
        .expect("failure trace payload should exist");
    assert_eq!(failure["schema_version"], serde_json::json!(1));
    assert!(failure["elapsed_ms"].as_u64().is_some());
    assert!(failure["args"].is_array());
    assert_eq!(failure["policy"]["dry_run"], serde_json::json!(false));
    assert_eq!(failure["error"]["category"], serde_json::json!("usage"));
    assert!(
        failure["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("invalid modifier")
    );
}

#[test]
fn trace_dir_not_writable_is_actionable_runtime_error() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let trace_file = cwd.path().join("trace-file");
    std::fs::write(&trace_file, b"blocked").expect("write trace blocker");

    let out = harness.run(
        cwd.path(),
        &[
            "--trace",
            "--trace-dir",
            &trace_file.to_string_lossy(),
            "--error-format",
            "json",
            "--format",
            "json",
            "preflight",
        ],
    );

    assert_eq!(out.code, 1, "stderr: {}", out.stderr_text());
    assert_eq!(out.stdout_text(), "");

    let payload: serde_json::Value =
        serde_json::from_str(&out.stderr_text()).expect("stderr should be json");
    assert_eq!(payload["schema_version"], serde_json::json!(1));
    assert_eq!(payload["ok"], serde_json::json!(false));
    assert_eq!(payload["error"]["category"], serde_json::json!("runtime"));
    assert_eq!(
        payload["error"]["operation"],
        serde_json::json!("trace.write")
    );
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("not writable")
    );
    let has_hint = payload["error"]["hints"]
        .as_array()
        .map(|hints| {
            hints
                .iter()
                .any(|hint| hint.as_str().unwrap_or("").contains("writable directory"))
        })
        .unwrap_or(false);
    assert!(
        has_hint,
        "expected writable-directory hint in error payload"
    );
}

#[test]
fn trace_command_labels_include_ax_commands() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let trace_dir = cwd.path().join("trace-ax");
    let trace_dir_text = trace_dir.to_string_lossy().to_string();

    let list = harness.run(
        cwd.path(),
        &[
            "--trace",
            "--trace-dir",
            &trace_dir_text,
            "--format",
            "json",
            "ax",
            "list",
        ],
    );
    assert_eq!(list.code, 0, "stderr: {}", list.stderr_text());

    let click = harness.run(
        cwd.path(),
        &[
            "--trace",
            "--trace-dir",
            &trace_dir_text,
            "--format",
            "json",
            "--dry-run",
            "ax",
            "click",
            "--node-id",
            "1.1",
        ],
    );
    assert_eq!(click.code, 0, "stderr: {}", click.stderr_text());

    let typ = harness.run(
        cwd.path(),
        &[
            "--trace",
            "--trace-dir",
            &trace_dir_text,
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
    assert_eq!(typ.code, 0, "stderr: {}", typ.stderr_text());

    let mut commands = std::fs::read_dir(&trace_dir)
        .expect("trace dir should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().map(|ext| ext == "json").unwrap_or(false))
        .map(|path| {
            let raw = std::fs::read_to_string(path).expect("trace file should be readable");
            let payload: serde_json::Value =
                serde_json::from_str(&raw).expect("trace payload should be json");
            payload["command"].as_str().unwrap_or_default().to_string()
        })
        .collect::<Vec<_>>();

    commands.sort();
    assert!(commands.iter().any(|command| command == "ax.list"));
    assert!(commands.iter().any(|command| command == "ax.click"));
    assert!(commands.iter().any(|command| command == "ax.type"));
}

#[test]
fn debug_bundle_emits_artifact_index_and_partial_failure_entries() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let bundle_dir = cwd.path().join("debug-bundle");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "debug",
            "bundle",
            "--window-id",
            "999",
            "--output-dir",
            bundle_dir.to_str().expect("bundle dir"),
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert_eq!(out.stderr_text(), "");

    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(payload["command"], serde_json::json!("debug.bundle"));
    assert_eq!(
        payload["result"]["partial_failure"],
        serde_json::json!(true)
    );

    let artifacts = payload["result"]["artifacts"]
        .as_array()
        .expect("artifacts should be array");
    let ids = artifacts
        .iter()
        .map(|entry| entry["id"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            "target-window".to_string(),
            "windows-list".to_string(),
            "active-window-screenshot".to_string(),
            "ax-links".to_string(),
            "ax-buttons".to_string(),
            "ax-textfields".to_string(),
            "ax-focused".to_string(),
        ]
    );
    assert!(
        artifacts
            .iter()
            .any(|entry| entry["ok"] == serde_json::json!(false)),
        "expected at least one partial-failure artifact entry"
    );

    let index_path = payload["result"]["artifact_index_path"]
        .as_str()
        .expect("artifact index path should be string");
    let index_raw = std::fs::read_to_string(index_path).expect("read artifact-index.json");
    let index_payload: serde_json::Value =
        serde_json::from_str(&index_raw).expect("artifact index json");
    assert_eq!(
        index_payload["artifacts"][0]["id"],
        serde_json::json!("target-window")
    );
    assert_eq!(
        index_payload["artifacts"][6]["id"],
        serde_json::json!("ax-focused")
    );
}

fn assert_action_envelope_contract(payload: &serde_json::Value, expected_command: &str) {
    assert_eq!(payload["schema_version"], serde_json::json!(1));
    assert_eq!(payload["ok"], serde_json::json!(true));
    assert_eq!(payload["command"], serde_json::json!(expected_command));

    let result = payload["result"]
        .as_object()
        .expect("result should be object");
    assert!(result.contains_key("policy"), "missing policy block");
    assert!(result.contains_key("meta"), "missing meta block");

    let policy = result["policy"]
        .as_object()
        .expect("policy should be object");
    assert_eq!(policy["dry_run"], serde_json::json!(true));
    assert_eq!(policy["retries"], serde_json::json!(2));
    assert_eq!(policy["retry_delay_ms"], serde_json::json!(9));
    assert_eq!(policy["timeout_ms"], serde_json::json!(1234));

    let meta = result["meta"].as_object().expect("meta should be object");
    let action_id = meta["action_id"]
        .as_str()
        .expect("meta.action_id should be string");
    assert!(
        action_id.starts_with(expected_command),
        "action_id `{action_id}` should start with `{expected_command}`"
    );
    assert!(meta["elapsed_ms"].as_u64().is_some());
    assert_eq!(meta["dry_run"], policy["dry_run"]);
    assert_eq!(meta["retries"], policy["retries"]);
    assert_eq!(meta["attempts_used"], serde_json::json!(0));
    assert_eq!(meta["timeout_ms"], policy["timeout_ms"]);
}

use std::env;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, run_with};
use tempfile::TempDir;

fn real_e2e_enabled() -> bool {
    cfg!(target_os = "macos")
        && env::var("MACOS_AGENT_REAL_E2E")
            .ok()
            .map(|value| value == "1")
            .unwrap_or(false)
}

fn real_mutation_enabled() -> bool {
    env::var("MACOS_AGENT_REAL_E2E_MUTATING")
        .ok()
        .map(|value| value == "1")
        .unwrap_or(false)
}

fn base_options(cwd: &std::path::Path) -> CmdOptions {
    CmdOptions::new()
        .with_cwd(cwd)
        .with_env_remove("AGENTS_MACOS_AGENT_TEST_MODE")
        .with_env_remove("AGENTS_MACOS_AGENT_TEST_TIMESTAMP")
        .with_env_remove("AGENTS_MACOS_AGENT_STUB_CLICLICK_MODE")
        .with_env_remove("AGENTS_MACOS_AGENT_STUB_OSASCRIPT_MODE")
}

#[test]
fn real_macos_preflight_reports_tcc_signals_in_json() {
    if !real_e2e_enabled() {
        eprintln!(
            "SKIP[real_macos_preflight_reports_tcc_signals_in_json]: MACOS_AGENT_REAL_E2E is not enabled"
        );
        return;
    }

    let cwd = TempDir::new().expect("tempdir");
    let bin = resolve("macos-agent");
    let options = base_options(cwd.path());
    let out = run_with(&bin, &["--format", "json", "preflight"], &options);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("preflight should emit json");
    assert_eq!(payload["schema_version"], serde_json::json!(1));
    assert_eq!(payload["command"], serde_json::json!("preflight"));

    let checks = payload["result"]["checks"]
        .as_array()
        .expect("checks should be an array");

    for id in ["accessibility", "automation"] {
        let check = checks
            .iter()
            .find(|check| check["id"] == serde_json::json!(id))
            .unwrap_or_else(|| panic!("missing `{id}` check"));
        let status = check["status"].as_str().expect("status should be string");
        assert!(
            ["ok", "fail", "warn"].contains(&status),
            "unexpected status for {id}: {status}"
        );
        if status != "ok" {
            assert!(
                check["hint"]
                    .as_str()
                    .map(|hint| !hint.is_empty())
                    .unwrap_or(false),
                "{id} should include a remediation hint when not ready"
            );
        }
    }
}

#[test]
fn real_macos_activate_and_wait_detects_focus_or_reports_actionable_tcc_error() {
    if !real_e2e_enabled() || !real_mutation_enabled() {
        eprintln!(
            "SKIP[real_macos_activate_and_wait_detects_focus_or_reports_actionable_tcc_error]: real e2e mutating gate is disabled"
        );
        return;
    }

    let app = env::var("MACOS_AGENT_REAL_E2E_APP").unwrap_or_else(|_| "Finder".to_string());
    let cwd = TempDir::new().expect("tempdir");
    let bin = resolve("macos-agent");
    let options = base_options(cwd.path());

    let activate_args = vec![
        "--format",
        "json",
        "window",
        "activate",
        "--app",
        app.as_str(),
        "--wait-ms",
        "1200",
    ];
    let activate_out = run_with(&bin, &activate_args, &options);

    if activate_out.code == 0 {
        let payload: serde_json::Value =
            serde_json::from_str(&activate_out.stdout_text()).expect("activation should emit json");
        assert_eq!(payload["command"], serde_json::json!("window.activate"));
        assert_eq!(
            payload["result"]["selected_app"]
                .as_str()
                .map(|name| name.eq_ignore_ascii_case(&app)),
            Some(true)
        );

        let wait_args = vec![
            "--format",
            "json",
            "wait",
            "app-active",
            "--app",
            app.as_str(),
            "--timeout-ms",
            "1800",
            "--poll-ms",
            "50",
        ];
        let wait_out = run_with(&bin, &wait_args, &options);
        assert_eq!(wait_out.code, 0, "stderr: {}", wait_out.stderr_text());
        let wait_payload: serde_json::Value =
            serde_json::from_str(&wait_out.stdout_text()).expect("wait should emit json");
        assert_eq!(
            wait_payload["command"],
            serde_json::json!("wait.app-active")
        );
    } else {
        assert_eq!(activate_out.code, 1);
        let stderr = activate_out.stderr_text();
        assert!(stderr.starts_with("error:"));
        assert!(
            stderr.contains("Accessibility")
                || stderr.contains("Automation")
                || stderr.contains("System Events")
                || stderr.contains("not authorized")
                || stderr.contains("window activate failed"),
            "expected actionable TCC/focus failure, got: {stderr}"
        );
    }
}

#[test]
fn real_macos_computer_use_input_primitives_execute_against_calculator() {
    if !real_e2e_enabled() || !real_mutation_enabled() {
        eprintln!(
            "SKIP[real_macos_computer_use_input_primitives_execute_against_calculator]: real e2e mutating gate is disabled"
        );
        return;
    }

    let cwd = TempDir::new().expect("tempdir");
    let bin = resolve("macos-agent");
    let options = base_options(cwd.path());

    let activate = run_with(
        &bin,
        &[
            "--format",
            "json",
            "window",
            "activate",
            "--app",
            "Calculator",
            "--wait-ms",
            "1200",
            "--reopen-on-fail",
        ],
        &options,
    );
    assert_eq!(activate.code, 0, "stderr: {}", activate.stderr_text());

    let windows = run_with(
        &bin,
        &[
            "--format",
            "json",
            "windows",
            "list",
            "--app",
            "Calculator",
            "--on-screen-only",
        ],
        &options,
    );
    assert_eq!(windows.code, 0, "stderr: {}", windows.stderr_text());
    let windows_payload: serde_json::Value =
        serde_json::from_str(&windows.stdout_text()).expect("windows list should emit json");
    let window = windows_payload["result"]["windows"]
        .as_array()
        .and_then(|items| items.first())
        .expect("Calculator should have an on-screen window");
    let x = window["x"].as_i64().expect("window x") as i32;
    let y = window["y"].as_i64().expect("window y") as i32;
    let width = window["width"].as_i64().expect("window width") as i32;
    let pointer_x = x + width / 2;
    let pointer_y = y + 100;
    let pointer_x_text = pointer_x.to_string();
    let pointer_y_text = pointer_y.to_string();
    let drag_x_text = (pointer_x + 18).to_string();
    let drag_y_text = (pointer_y + 12).to_string();

    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "input.move",
            vec![
                "--format",
                "json",
                "input",
                "move",
                "--x",
                &pointer_x_text,
                "--y",
                &pointer_y_text,
            ],
        ),
        (
            "input.key",
            vec!["--format", "json", "input", "key", "--key", "escape"],
        ),
        (
            "input.scroll",
            vec![
                "--format",
                "json",
                "input",
                "scroll",
                "--delta-y",
                "-1",
                "--unit",
                "line",
            ],
        ),
        (
            "input.drag",
            vec![
                "--format",
                "json",
                "input",
                "drag",
                "--from-x",
                &pointer_x_text,
                "--from-y",
                &pointer_y_text,
                "--to-x",
                &drag_x_text,
                "--to-y",
                &drag_y_text,
                "--duration-ms",
                "160",
                "--steps",
                "4",
            ],
        ),
    ];

    for (expected_command, args) in cases {
        let out = run_with(&bin, &args, &options);
        assert_eq!(
            out.code,
            0,
            "command={expected_command}, stderr={}",
            out.stderr_text()
        );
        let payload: serde_json::Value =
            serde_json::from_str(&out.stdout_text()).expect("primitive should emit json");
        assert_eq!(payload["command"], serde_json::json!(expected_command));
        assert_eq!(payload["ok"], serde_json::json!(true));
    }
}

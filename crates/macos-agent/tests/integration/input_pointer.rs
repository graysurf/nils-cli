use nils_test_support::StubBinDir;
use tempfile::TempDir;

use crate::common;

#[test]
fn input_move_uses_cliclick_and_reports_position() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let log = cwd.path().join("cliclick.log");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_LOG", &log.to_string_lossy());

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format", "json", "input", "move", "--x", "120", "--y", "240",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(payload["command"], serde_json::json!("input.move"));
    assert_eq!(payload["result"]["x"], serde_json::json!(120));
    assert_eq!(payload["result"]["y"], serde_json::json!(240));
    let recorded = std::fs::read_to_string(log).expect("read cliclick log");
    assert!(recorded.contains("cliclick m:120,240"));
}

#[test]
fn input_drag_uses_bounded_intermediate_steps() {
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
            "drag",
            "--from-x",
            "10",
            "--from-y",
            "20",
            "--to-x",
            "110",
            "--to-y",
            "220",
            "--duration-ms",
            "200",
            "--steps",
            "2",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(payload["command"], serde_json::json!("input.drag"));
    assert_eq!(payload["result"]["duration_ms"], serde_json::json!(200));
    assert_eq!(payload["result"]["steps"], serde_json::json!(2));
    let recorded = std::fs::read_to_string(log).expect("read cliclick log");
    assert!(recorded.contains("dd:10,20"));
    assert!(recorded.contains("dm:60,120"));
    assert!(recorded.contains("dm:110,220"));
    assert!(recorded.contains("du:110,220"));
}

#[test]
fn input_drag_holds_and_releases_modifiers() {
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
            "drag",
            "--from-x",
            "10",
            "--from-y",
            "20",
            "--to-x",
            "30",
            "--to-y",
            "40",
            "--duration-ms",
            "100",
            "--steps",
            "2",
            "--mods",
            "alt,shift",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(
        payload["result"]["mods"],
        serde_json::json!(["alt", "shift"])
    );
    let recorded = std::fs::read_to_string(log).expect("read cliclick log");
    assert!(recorded.contains("kd:alt,shift"), "{recorded}");
    assert!(recorded.contains("ku:alt,shift"), "{recorded}");
    assert!(
        recorded.find("kd:alt,shift") < recorded.find("dd:10,20"),
        "{recorded}"
    );
    assert!(
        recorded.find("du:30,40") < recorded.find("ku:alt,shift"),
        "{recorded}"
    );
}

#[test]
fn input_drag_rejects_duration_that_cannot_fit_action_timeout() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--timeout-ms",
            "200",
            "input",
            "drag",
            "--from-x",
            "10",
            "--from-y",
            "20",
            "--to-x",
            "30",
            "--to-y",
            "40",
            "--duration-ms",
            "200",
        ],
    );

    assert_eq!(out.code, 2);
    assert!(out.stderr_text().contains("--duration-ms"));
    assert!(out.stderr_text().contains("--timeout-ms"));
}

#[test]
fn input_drag_releases_mouse_after_backend_failure() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let log = cwd.path().join("cliclick.log");
    let counter = cwd.path().join("cliclick.count");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_LOG", &log.to_string_lossy())
        .with_env(
            "AGENTS_MACOS_AGENT_STUB_COUNTER_FILE",
            &counter.to_string_lossy(),
        )
        .with_env("AGENTS_MACOS_AGENT_STUB_CLICLICK_FAIL_UNTIL", "1");

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "input", "drag", "--from-x", "10", "--from-y", "20", "--to-x", "30", "--to-y", "40",
        ],
        options,
    );

    assert_eq!(out.code, 1);
    assert_eq!(std::fs::read_to_string(counter).expect("counter"), "2");
    let recorded = std::fs::read_to_string(log).expect("read cliclick log");
    assert!(recorded.contains("cliclick du:."));
}

#[test]
fn input_drag_failure_cleanup_releases_mouse_and_modifiers() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let log = cwd.path().join("cliclick.log");
    let counter = cwd.path().join("cliclick.count");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_LOG", &log.to_string_lossy())
        .with_env(
            "AGENTS_MACOS_AGENT_STUB_COUNTER_FILE",
            &counter.to_string_lossy(),
        )
        .with_env("AGENTS_MACOS_AGENT_STUB_CLICLICK_FAIL_UNTIL", "1");

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "input",
            "drag",
            "--from-x",
            "10",
            "--from-y",
            "20",
            "--to-x",
            "30",
            "--to-y",
            "40",
            "--mods",
            "cmd,shift",
        ],
        options,
    );

    assert_eq!(out.code, 1);
    let recorded = std::fs::read_to_string(log).expect("read cliclick log");
    assert!(
        recorded.contains("cliclick du:. ku:cmd,shift"),
        "{recorded}"
    );
}

#[test]
fn input_drag_interpolates_extreme_coordinates_without_overflow() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "input",
            "drag",
            "--from-x",
            "-2147483648",
            "--from-y",
            "-2147483648",
            "--to-x",
            "2147483647",
            "--to-y",
            "2147483647",
            "--duration-ms",
            "1",
            "--steps",
            "2",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
}

#[test]
fn input_scroll_uses_hammerspoon_and_reports_deltas() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    let log = cwd.path().join("hs.log");
    stub.write_exe(
        "hs",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf 'hs %s\n' "$*" >> "${AGENTS_MACOS_AGENT_STUB_LOG:?}"
printf '%s\n' '{"scrolled":true}'
"#,
    );
    let options = harness
        .cmd_options(cwd.path())
        .with_path_prepend(stub.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_LOG", &log.to_string_lossy());

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "input",
            "scroll",
            "--delta-x",
            "5",
            "--delta-y",
            "-480",
            "--unit",
            "pixel",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(payload["command"], serde_json::json!("input.scroll"));
    assert_eq!(payload["result"]["delta_x"], serde_json::json!(5));
    assert_eq!(payload["result"]["delta_y"], serde_json::json!(-480));
    assert_eq!(payload["result"]["unit"], serde_json::json!("pixel"));
    let recorded = std::fs::read_to_string(log).expect("read hs log");
    assert!(recorded.contains("scrollWheel"));
}

#[test]
fn input_scroll_attaches_modifier_flags_without_leaving_keys_held() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    let log = cwd.path().join("hs.log");
    stub.write_exe(
        "hs",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf 'hs %s\n' "$*" >> "${AGENTS_MACOS_AGENT_STUB_LOG:?}"
printf '%s\n' '{"scrolled":true}'
"#,
    );
    let options = harness
        .cmd_options(cwd.path())
        .with_path_prepend(stub.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_LOG", &log.to_string_lossy());

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "input",
            "scroll",
            "--delta-y",
            "-2",
            "--unit",
            "line",
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
    let recorded = std::fs::read_to_string(log).expect("read hs log");
    assert!(
        recorded.contains(r#"\"mods\":[\"cmd\",\"shift\"]"#)
            || recorded.contains(r#""mods":["cmd","shift"]"#),
        "{recorded}"
    );
}

#[test]
fn input_pointer_dry_run_does_not_execute_backends() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("AGENTS_MACOS_AGENT_STUB_CLICLICK_MODE", "fail");

    for args in [
        vec!["--dry-run", "input", "move", "--x", "1", "--y", "2"],
        vec![
            "--dry-run",
            "input",
            "drag",
            "--from-x",
            "1",
            "--from-y",
            "2",
            "--to-x",
            "3",
            "--to-y",
            "4",
        ],
        vec!["--dry-run", "input", "scroll", "--delta-y", "-20"],
    ] {
        let out = harness.run_with_options(cwd.path(), &args, options.clone());
        assert_eq!(out.code, 0, "args={args:?}, stderr={}", out.stderr_text());
    }
}

#[test]
fn input_scroll_rejects_zero_delta() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["input", "scroll", "--delta-y", "0"]);

    assert_eq!(out.code, 2);
    assert!(out.stderr_text().contains("delta"));
}

#[test]
fn input_scroll_reports_hammerspoon_ipc_remediation() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe(
        "hs",
        r#"#!/usr/bin/env bash
set -euo pipefail
echo "message port cannot connect; is it running?" >&2
exit 1
"#,
    );
    let options = harness
        .cmd_options(cwd.path())
        .with_path_prepend(stub.path());

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "input",
            "scroll",
            "--delta-y",
            "-1",
        ],
        options,
    );

    assert_eq!(out.code, 1);
    let payload: serde_json::Value =
        serde_json::from_str(&out.stderr_text()).expect("stderr should be json");
    let hints = payload["error"]["hints"]
        .as_array()
        .expect("hints should be an array")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(hints.contains("hs.ipc"), "hints={hints}");
    assert!(hints.contains("Hammerspoon"), "hints={hints}");
}

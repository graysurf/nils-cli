use nils_test_support::StubBinDir;
use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn run_with(args: &[&str], options: &CmdOptions) -> CmdOutput {
    let bin = codex_cli_bin();
    cmd::run_with(&bin, args, options)
}

fn stdout(output: &CmdOutput) -> String {
    output.stdout_text()
}

fn stderr(output: &CmdOutput) -> String {
    output.stderr_text()
}

fn codex_stub_script() -> &'static str {
    r#"#!/bin/bash
set -euo pipefail
if [[ -n "${NILS_TEST_STUB_LOG:-}" ]]; then
  echo "$*" >> "${NILS_TEST_STUB_LOG}"
fi
exit "${CODEX_STUB_EXIT_CODE:-0}"
"#
}

#[test]
fn auth_login_default_uses_chatgpt_browser_flow() {
    let stubs = StubBinDir::new();
    stubs.write_exe("codex", codex_stub_script());
    let log = tempfile::NamedTempFile::new().expect("log");

    let options = CmdOptions::default()
        .with_path_prepend(stubs.path())
        .with_env("NILS_TEST_STUB_LOG", log.path().to_string_lossy().as_ref());
    let output = run_with(&["auth", "login"], &options);
    assert_eq!(output.code, 0);
    assert!(stdout(&output).contains("chatgpt-browser"));

    let log_content = fs::read_to_string(log.path()).expect("read log");
    assert!(log_content.contains("login"));
}

#[test]
fn auth_login_device_code_and_api_key_map_to_expected_args() {
    let stubs = StubBinDir::new();
    stubs.write_exe("codex", codex_stub_script());
    let log = tempfile::NamedTempFile::new().expect("log");

    let options = CmdOptions::default()
        .with_path_prepend(stubs.path())
        .with_env("NILS_TEST_STUB_LOG", log.path().to_string_lossy().as_ref());
    let output = run_with(&["auth", "login", "--device-code"], &options);
    assert_eq!(output.code, 0);
    let output = run_with(&["auth", "login", "--api-key"], &options);
    assert_eq!(output.code, 0);

    let log_content = fs::read_to_string(log.path()).expect("read log");
    assert!(log_content.contains("login --device-auth"));
    assert!(log_content.contains("login --with-api-key"));
}

#[test]
fn auth_login_json_success_is_structured() {
    let stubs = StubBinDir::new();
    stubs.write_exe("codex", codex_stub_script());

    let options = CmdOptions::default().with_path_prepend(stubs.path());
    let output = run_with(&["auth", "login", "--json", "--device-code"], &options);
    assert_eq!(output.code, 0);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth login");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["method"], "chatgpt-device-code");
    assert_eq!(payload["result"]["provider"], "chatgpt");
}

#[test]
fn auth_login_rejects_conflicting_flags() {
    let output = run_with(
        &["auth", "login", "--api-key", "--device-code"],
        &CmdOptions::default(),
    );
    assert_eq!(output.code, 64);
    assert!(stderr(&output).contains("--api-key"));
}

#[test]
fn auth_login_json_non_zero_status_is_structured_error() {
    let stubs = StubBinDir::new();
    stubs.write_exe("codex", codex_stub_script());

    let options = CmdOptions::default()
        .with_path_prepend(stubs.path())
        .with_env("CODEX_STUB_EXIT_CODE", "7");
    let output = run_with(&["auth", "login", "--json", "--api-key"], &options);
    assert_eq!(output.code, 7);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "login-failed");
}

#[test]
fn auth_login_json_missing_codex_is_structured_error() {
    let options = CmdOptions::default().with_env("PATH", "");
    let output = run_with(&["auth", "login", "--json"], &options);
    assert_eq!(output.code, 1);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "login-exec-failed");
}

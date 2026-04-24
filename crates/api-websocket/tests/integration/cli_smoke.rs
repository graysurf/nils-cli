use std::path::PathBuf;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOutput, run};
use pretty_assertions::{assert_eq, assert_ne};

fn api_websocket_bin() -> PathBuf {
    resolve("api-websocket")
}

fn run_api_websocket(args: &[&str]) -> CmdOutput {
    run(&api_websocket_bin(), args, &[], None)
}

#[test]
fn help_includes_key_flags() {
    let out = run_api_websocket(&["--help"]);
    assert_eq!(out.code, 0);
    let text = format!("{}{}", out.stdout_text(), out.stderr_text());
    assert!(text.contains("history"));
    assert!(text.contains("report-from-cmd"));
    assert!(text.contains("completion"));
    assert!(text.contains("--config-dir"));
    assert!(text.contains("--format"));
}

#[test]
fn invalid_flag_exits_nonzero() {
    let out = run_api_websocket(&["--definitely-not-a-flag"]);
    assert_ne!(out.code, 0);
}

#[test]
fn report_from_cmd_dry_run_exits_zero_and_prints_report_command() {
    let snippet = "api-websocket call --env staging setup/websocket/requests/health.ws.json";
    let out = run_api_websocket(&["report-from-cmd", "--dry-run", snippet]);
    assert_eq!(out.code, 0);
    assert!(out.stdout_text().contains("api-websocket report"));
    assert!(out.stdout_text().contains("--case"));
    assert!(out.stdout_text().contains("health"));
    assert!(out.stdout_text().contains("staging"));
}

#[test]
fn call_json_failure_uses_contract_envelope() {
    let out = run_api_websocket(&["call", "--format", "json", "does-not-exist.ws.json"]);
    assert_eq!(out.code, 1);

    let json: serde_json::Value = serde_json::from_str(&out.stdout_text()).expect("json stdout");
    assert_eq!(json["schema_version"], "cli.api-websocket.call.v1");
    assert_eq!(json["command"], "api-websocket call");
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "request_not_found");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Request file not found")
    );
}

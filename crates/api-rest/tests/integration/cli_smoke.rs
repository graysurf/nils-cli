use std::path::PathBuf;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOutput, run};
use pretty_assertions::{assert_eq, assert_ne};

fn api_rest_bin() -> PathBuf {
    resolve("api-rest")
}

fn run_api_rest(args: &[&str]) -> CmdOutput {
    run(&api_rest_bin(), args, &[], None)
}

#[test]
fn help_includes_key_flags() {
    let out = run_api_rest(&["--help"]);
    assert_eq!(out.code, 0);
    let text = format!("{}{}", out.stdout_text(), out.stderr_text());
    assert!(text.contains("history"));
    assert!(text.contains("report-from-cmd"));
    assert!(text.contains("completion"));
    assert!(text.contains("--config-dir"));
}

#[test]
fn invalid_flag_exits_nonzero() {
    let out = run_api_rest(&["--definitely-not-a-flag"]);
    assert_ne!(out.code, 0);
}

#[test]
fn report_from_cmd_dry_run_exits_zero_and_prints_report_command() {
    let snippet = "api-rest call --env staging setup/rest/requests/health.request.json";
    let out = run_api_rest(&["report-from-cmd", "--dry-run", snippet]);
    assert_eq!(out.code, 0);
    assert!(out.stdout_text().contains("api-rest report"));
    assert!(out.stdout_text().contains("--case"));
    assert!(out.stdout_text().contains("health"));
    assert!(out.stdout_text().contains("staging"));
}

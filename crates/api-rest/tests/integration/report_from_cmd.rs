use std::path::PathBuf;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run, run_with};
use pretty_assertions::assert_eq;

fn api_rest_bin() -> PathBuf {
    resolve("api-rest")
}

fn run_api_rest(args: &[&str]) -> CmdOutput {
    run(&api_rest_bin(), args, &[], None)
}

fn run_api_rest_with_stdin(args: &[&str], stdin: &[u8]) -> CmdOutput {
    let options = CmdOptions::default().with_stdin_bytes(stdin);
    run_with(&api_rest_bin(), args, &options)
}

const SNIPPET: &str = "api-rest call --config-dir setup/rest --env staging --token service setup/rest/requests/health.request.json | jq .";

#[test]
fn report_from_cmd_dry_run_uses_positional_snippet() {
    let out = run_api_rest(&["report-from-cmd", "--dry-run", SNIPPET]);
    assert_eq!(out.code, 0);

    assert!(out.stdout_text().starts_with("api-rest report"));
    assert!(
        out.stdout_text()
            .contains("--case 'health (staging, token:service)'")
    );
    assert!(
        out.stdout_text()
            .contains("--request 'setup/rest/requests/health.request.json'")
    );
    assert!(out.stdout_text().contains("--config-dir 'setup/rest'"));
    assert!(out.stdout_text().contains("--env 'staging'"));
    assert!(out.stdout_text().contains("--token 'service'"));
    assert!(out.stdout_text().contains(" --run"));
}

#[test]
fn report_from_cmd_dry_run_uses_stdin_snippet() {
    let out = run_api_rest_with_stdin(
        &["report-from-cmd", "--dry-run", "--stdin"],
        format!("{SNIPPET}\n").as_bytes(),
    );
    assert_eq!(out.code, 0);

    assert!(out.stdout_text().starts_with("api-rest report"));
    assert!(
        out.stdout_text()
            .contains("--case 'health (staging, token:service)'")
    );
    assert!(
        out.stdout_text()
            .contains("--request 'setup/rest/requests/health.request.json'")
    );
}

#[test]
fn report_from_cmd_response_stdin_conflicts_with_snippet_stdin() {
    let out = run_api_rest(&["report-from-cmd", "--response", "-", "--stdin", "--dry-run"]);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr_text()
            .contains("When using --response -, stdin is reserved")
    );
}

#[test]
fn report_from_cmd_dry_run_includes_response_dash_and_omits_run() {
    let out = run_api_rest(&["report-from-cmd", "--dry-run", "--response", "-", SNIPPET]);
    assert_eq!(out.code, 0);

    assert!(out.stdout_text().starts_with("api-rest report"));
    assert!(out.stdout_text().contains("--response '-'"));
    assert!(!out.stdout_text().contains(" --run"));
}

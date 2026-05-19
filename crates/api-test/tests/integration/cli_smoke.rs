use std::path::PathBuf;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOutput, run};
use pretty_assertions::{assert_eq, assert_ne};

fn api_test_bin() -> PathBuf {
    resolve("api-test")
}

fn run_api_test(args: &[&str]) -> CmdOutput {
    run(&api_test_bin(), args, &[], None)
}

#[test]
fn help_includes_key_flags() {
    let out = run_api_test(&["--help"]);
    assert_eq!(out.code, 0);
    let text = format!("{}{}", out.stdout_text(), out.stderr_text());
    assert!(text.contains("summary"));
    assert!(text.contains("completion"));
    assert!(text.contains("--suite"));
    assert!(text.contains("--suite-file"));
}

#[test]
fn invalid_flag_exits_nonzero() {
    let out = run_api_test(&["--definitely-not-a-flag"]);
    assert_ne!(out.code, 0);
}

#[test]
fn unknown_arg_returns_usage_exit_code() {
    let out = run_api_test(&["definitely-not-a-real-subcommand"]);
    assert_eq!(out.code, 64, "stderr={}", out.stderr_text());
}

#[test]
fn unknown_flag_emits_json_envelope_when_format_json_present() {
    let out = run_api_test(&["--format", "json", "--definitely-not-a-real-flag"]);
    assert_eq!(out.code, 64, "stderr={}", out.stderr_text());
    let stdout = out.stdout_text();
    assert!(
        stdout.contains("\"schema_version\":\"cli.api-test.error.v1\""),
        "expected error envelope on stdout, got: {stdout}"
    );
    assert!(stdout.contains("\"ok\":false"), "stdout={stdout}");
}

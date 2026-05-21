use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput, run_with};
use std::path::PathBuf;

fn cli_template_bin() -> PathBuf {
    bin::resolve("cli-template")
}

fn run(args: &[&str]) -> CmdOutput {
    let bin = cli_template_bin();
    cmd::run(&bin, args, &[], None)
}

fn run_without_rust_log(args: &[&str]) -> CmdOutput {
    let bin = cli_template_bin();
    let options = CmdOptions::new().with_env_remove("RUST_LOG");
    run_with(&bin, args, &options)
}

#[test]
fn cli_template_runs_without_subcommand() {
    let output = run(&[]);
    assert_eq!(output.code, 0);
    let stdout = output.stdout_text();
    assert_eq!(stdout, "", "no-subcommand path should not print stdout");
    let stderr = output.stderr_text();
    assert!(
        stderr.contains("no subcommand selected"),
        "stderr should log the no-subcommand default path: {stderr}"
    );
}

#[test]
fn cli_template_hello_defaults_to_world() {
    let output = run(&["hello"]);
    assert_eq!(output.code, 0);
    let stdout = output.stdout_text();
    assert!(stdout.contains("Hello, world!"), "stdout={stdout}");
}

#[test]
fn cli_template_hello_accepts_name() {
    let output = run(&["hello", "Nils"]);
    assert_eq!(output.code, 0);
    let stdout = output.stdout_text();
    assert!(stdout.contains("Hello, Nils!"), "stdout={stdout}");
}

#[test]
fn cli_template_progress_demo_prints_done() {
    let output = run(&["progress-demo"]);
    assert_eq!(output.code, 0);
    let stdout = output.stdout_text();
    assert_eq!(stdout.trim(), "done", "stdout={stdout}");
}

#[test]
fn cli_template_invalid_log_level_still_prints_greeting() {
    let output = run_without_rust_log(&["--log-level", "not-a-level", "hello", "Nils"]);
    assert_eq!(output.code, 0);
    let stdout = output.stdout_text();
    assert!(stdout.contains("Hello, Nils!"), "stdout={stdout}");
}

#[test]
fn cli_template_status_format_json_emits_envelope() {
    let output = run(&["--format", "json", "status"]);
    assert_eq!(output.code, 0);
    let stdout = output.stdout_text();
    // Snapshot-pin the literal schema_version per the contract spec.
    assert!(
        stdout.contains("\"schema_version\":\"cli.cli-template.status.v1\""),
        "stdout={stdout}"
    );
    assert!(stdout.contains("\"ok\":true"), "stdout={stdout}");
    assert!(
        stdout.contains("\"binary\":\"cli-template\""),
        "stdout={stdout}"
    );
}

#[test]
fn cli_template_status_json_alias_matches_format_json() {
    let format_run = run(&["--format", "json", "status"]);
    let alias_run = run(&["--json", "status"]);
    assert_eq!(format_run.code, 0);
    assert_eq!(alias_run.code, 0);
    assert_eq!(format_run.stdout_text(), alias_run.stdout_text());
}

#[test]
fn cli_template_status_text_omits_envelope() {
    let output = run(&["status"]);
    assert_eq!(output.code, 0);
    let stdout = output.stdout_text();
    assert!(stdout.contains("cli-template"), "stdout={stdout}");
    assert!(
        !stdout.contains("schema_version"),
        "text mode must not emit JSON envelope: {stdout}"
    );
}

#[test]
fn cli_template_unknown_subcommand_exits_usage() {
    let output = run(&["bogus-subcommand"]);
    assert_eq!(output.code, 64);
}

#[test]
fn cli_template_unknown_subcommand_json_emits_envelope() {
    let output = run(&["--format", "json", "bogus-subcommand"]);
    assert_eq!(output.code, 64);
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("\"schema_version\":\"cli.cli-template.error.v1\""),
        "stdout={stdout}"
    );
    assert!(stdout.contains("\"ok\":false"), "stdout={stdout}");
    assert!(
        stdout.contains("\"code\":\"unknown-subcommand\""),
        "stdout={stdout}"
    );
}

#[test]
fn cli_template_unknown_subcommand_text_prints_error_prefix() {
    let output = run(&["bogus-subcommand"]);
    assert_eq!(output.code, 64);
    let stderr = output.stderr_text();
    assert!(
        stderr.to_ascii_lowercase().starts_with("error:"),
        "stderr should start with 'error:' prefix: {stderr}"
    );
}

#[test]
fn cli_template_help_lists_format_not_json_alias() {
    let output = run(&["--help"]);
    assert_eq!(output.code, 0);
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("--format"),
        "help should advertise --format: {stdout}"
    );
    assert!(
        !stdout.contains("--json"),
        "help should not advertise hidden --json alias: {stdout}"
    );
}

#[test]
fn cli_template_format_and_json_conflict_at_parse_time() {
    let output = run(&["--json", "--format", "text", "status"]);
    assert_eq!(output.code, 64);
}

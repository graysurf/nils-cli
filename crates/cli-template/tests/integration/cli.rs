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

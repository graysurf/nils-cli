//! Coverage for zsh-kit argument-parse handling: unknown subcommands, missing
//! required arguments, both `--format` detection forms used to shape parse
//! errors, and the help/version short-circuits.

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn run(args: &[&str]) -> CmdOutput {
    run_resolved("zsh-kit", args, &CmdOptions::new())
}

#[test]
fn unknown_subcommand_emits_json_parse_error() {
    let output = run(&["definitely-not-a-subcommand", "--format", "json"]);
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    let value: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "unknown-subcommand");
}

#[test]
fn unknown_subcommand_detects_format_equals_form() {
    let output = run(&["definitely-not-a-subcommand", "--format=json"]);
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    let value: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "unknown-subcommand");
}

#[test]
fn missing_required_repo_is_text_parse_error_by_default() {
    // `setup` requires --repo; without --format the error renders as text.
    let output = run(&["setup", "--dry-run"]);
    assert_eq!(output.code, 64, "stdout={}", output.stdout_text());
    assert!(
        output.stderr_text().contains("error:"),
        "stderr={}",
        output.stderr_text()
    );
}

#[test]
fn missing_required_repo_renders_json_parse_error_code() {
    let output = run(&["setup", "--dry-run", "--format", "json"]);
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    let value: Value = serde_json::from_str(&output.stdout_text()).expect("json");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "parse-error");
}

#[test]
fn help_flag_exits_success() {
    let output = run(&["--help"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(output.stdout_text().contains("zsh-kit"));
}

#[test]
fn version_flag_exits_success() {
    let output = run(&["--version"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
}

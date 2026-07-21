#![cfg(target_os = "macos")]

use std::path::Path;

use nils_common::cli_contract::exit;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn run(args: &[&str]) -> CmdOutput {
    run_resolved("agent-run", args, &CmdOptions::new())
}

fn arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[test]
fn inspect_fails_closed_with_actionable_macos_diagnostic() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd_arg = arg(temp.path());
    let output = run(&["inspect", "--cwd", &cwd_arg, "--", "/usr/bin/true"]);

    assert_eq!(output.code, exit::UNAVAILABLE);
    assert_eq!(output.stdout_text(), "");
    assert_eq!(
        output.stderr_text(),
        "agent-run inspect: error[sandbox-backend-unavailable]: strict OS-enforced inspection is unavailable on macOS; use project-dev preparation for the exact target instead\n"
    );
}

#[test]
fn operation_effect_reports_typed_macos_unavailability_without_descriptor() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cwd_arg = arg(temp.path());
    let output = run(&[
        "operation-effect",
        "--format",
        "json",
        "--",
        "inspect",
        "--cwd",
        &cwd_arg,
        "--",
        "rg",
        "TODO",
    ]);

    assert_eq!(output.code, exit::UNAVAILABLE);
    assert_eq!(output.stderr_text(), "");
    let value: Value = serde_json::from_str(&output.stdout_text()).expect("error JSON");
    assert_eq!(value["schema_version"], "cli.agent-run.operation-effect.v1");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "sandbox-backend-unavailable");
    assert_eq!(
        value["error"]["message"],
        "strict OS-enforced inspection is unavailable on macOS; use project-dev preparation for the exact target instead"
    );
    assert!(value.get("data").is_none());
}

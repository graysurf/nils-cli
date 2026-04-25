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

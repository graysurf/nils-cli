use nils_test_support::cmd::{CmdOptions, run_resolved};
use nils_test_support::help::{HelpCase, assert_help_contains};
use pretty_assertions::assert_eq;

#[test]
fn help_snapshot_root_help() {
    assert_help_contains(HelpCase::root(
        "memo-cli",
        &[
            "Capture, search, report",
            "Usage:",
            "Commands:",
            "Options:",
            "EXAMPLES:",
            "ENVIRONMENT:",
            "EXIT CODES:",
            "XDG_DATA_HOME",
            "HOME",
            "-V, --version",
        ],
    ));
}

#[test]
fn help_snapshot_json_format_conflict_is_parse_time() {
    let output = run_resolved(
        "memo-cli",
        &["--json", "--format", "text", "list"],
        &CmdOptions::new(),
    );
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    let combined = format!("{}{}", output.stdout_text(), output.stderr_text());
    assert!(
        combined.contains("cannot be used with"),
        "stdout={}\nstderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
}

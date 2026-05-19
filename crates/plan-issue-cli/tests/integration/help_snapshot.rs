use nils_test_support::cmd::{CmdOptions, run_resolved};
use nils_test_support::help::{HelpCase, assert_help_contains};
use pretty_assertions::assert_eq;

#[test]
fn help_snapshot_root_help() {
    for bin in ["plan-issue", "plan-issue-local"] {
        assert_help_contains(HelpCase::root(
            bin,
            &[
                "Usage:",
                "Commands:",
                "Options:",
                "EXAMPLES:",
                "ENVIRONMENT:",
                "EXIT CODES:",
                "PLAN_ISSUE_HOME",
                "-V, --version",
            ],
        ));
    }
}

#[test]
fn help_snapshot_json_format_conflict_is_parse_time() {
    let output = run_resolved(
        "plan-issue",
        &["--json", "--format", "text", "completion", "zsh"],
        &CmdOptions::new(),
    );
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    assert!(
        output.stderr_text().contains("cannot be used with"),
        "stderr={}",
        output.stderr_text()
    );
}

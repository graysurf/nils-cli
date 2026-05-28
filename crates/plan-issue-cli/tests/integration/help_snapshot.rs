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
fn version_output_uses_binary_name() {
    for bin in ["plan-issue", "plan-issue-local"] {
        let short = run_resolved(bin, &["-V"], &CmdOptions::new());
        assert_eq!(short.code, 0, "stderr={}", short.stderr_text());
        assert_eq!(
            short.stdout_text(),
            format!("{bin} {}\n", env!("CARGO_PKG_VERSION"))
        );

        let long = run_resolved(bin, &["--version"], &CmdOptions::new());
        assert_eq!(long.code, 0, "stderr={}", long.stderr_text());
        assert!(
            long.stdout_text()
                .starts_with(&format!("{bin} {} (", env!("CARGO_PKG_VERSION"))),
            "stdout={}",
            long.stdout_text()
        );
        assert!(
            long.stdout_text().contains("rustc "),
            "{}",
            long.stdout_text()
        );
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

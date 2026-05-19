use nils_test_support::help::{HelpCase, assert_help_contains};

#[test]
fn help_snapshot_root_help() {
    assert_help_contains(HelpCase::root(
        "agent-scope-lock",
        &[
            "Usage:",
            "Commands:",
            "Options:",
            "EXAMPLES:",
            "ENVIRONMENT:",
            "EXIT CODES:",
            "none",
            "-V, --version",
        ],
    ));
}

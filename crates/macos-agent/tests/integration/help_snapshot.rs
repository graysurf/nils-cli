use nils_test_support::help::{HelpCase, assert_help_contains};

#[test]
fn help_snapshot_root_help() {
    assert_help_contains(HelpCase::root(
        "macos-agent",
        &[
            "Usage:",
            "Commands:",
            "Options:",
            "EXAMPLES:",
            "ENVIRONMENT:",
            "EXIT CODES:",
            "AGENT_HOME",
            "AGENTS_MACOS_AGENT_AX_BACKEND",
            "-V, --version",
        ],
    ));
}

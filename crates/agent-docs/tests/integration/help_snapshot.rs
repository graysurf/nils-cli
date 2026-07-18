use nils_test_support::help::{HelpCase, assert_help_contains};

#[test]
fn help_snapshot_root_help() {
    assert_help_contains(HelpCase::root(
        "agent-docs",
        &[
            "Usage:",
            "Commands:",
            "Options:",
            "EXAMPLES:",
            "ENVIRONMENT:",
            "DOCS-HOME RESOLUTION:",
            "EXIT CODES:",
            "AGENT_DOCS_HOME",
            "PROJECT_PATH",
            "XDG_CONFIG_HOME",
            "HOME",
            "audit",
            "preflight",
            "init",
            "explain",
            "list",
            "remove",
            "config",
            "integration",
            "session",
            "completion",
            "--user-config",
            "--integration-fingerprint",
            "-V, --version",
        ],
    ));
}

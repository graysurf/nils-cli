use nils_test_support::help::{HelpCase, assert_help_contains};

#[test]
fn help_snapshot_root_help() {
    assert_help_contains(HelpCase::root(
        "api-websocket",
        &[
            "WebSocket API runner",
            "Usage:",
            "Commands:",
            "EXAMPLES:",
            "ENVIRONMENT:",
            "EXIT CODES:",
            "WS_URL",
            "WS_HISTORY_FILE",
            "WS_REPORT_INCLUDE_COMMAND_ENABLED",
            "ACCESS_TOKEN",
            "-V, --version",
        ],
    ));
}

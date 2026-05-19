use nils_test_support::help::{HelpCase, assert_help_contains};

#[test]
fn help_snapshot_root_help() {
    assert_help_contains(HelpCase::root(
        "api-grpc",
        &[
            "GRPC API runner",
            "Usage:",
            "Commands:",
            "EXAMPLES:",
            "ENVIRONMENT:",
            "EXIT CODES:",
            "GRPC_URL",
            "GRPC_HISTORY_FILE",
            "GRPC_REPORT_INCLUDE_COMMAND_ENABLED",
            "ACCESS_TOKEN",
            "-V, --version",
        ],
    ));
}

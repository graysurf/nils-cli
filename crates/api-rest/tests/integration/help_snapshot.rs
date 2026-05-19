use nils_test_support::help::{HelpCase, assert_help_contains};

#[test]
fn help_snapshot_root_help() {
    assert_help_contains(HelpCase::root(
        "api-rest",
        &[
            "REST API runner",
            "Usage:",
            "Commands:",
            "EXAMPLES:",
            "ENVIRONMENT:",
            "EXIT CODES:",
            "REST_URL",
            "REST_HISTORY_FILE",
            "REST_REPORT_INCLUDE_COMMAND_ENABLED",
            "ACCESS_TOKEN",
            "-V, --version",
        ],
    ));
}

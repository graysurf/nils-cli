use nils_test_support::help::{HelpCase, assert_help_contains};

#[test]
fn help_snapshot_root_help() {
    assert_help_contains(HelpCase::root(
        "api-test",
        &[
            "API suite runner",
            "Usage:",
            "Commands:",
            "EXAMPLES:",
            "ENVIRONMENT:",
            "EXIT CODES:",
            "API_TEST_PROGRESS",
            "API_TEST_ALLOW_WRITES_ENABLED",
            "API_TEST_AUTH_JSON",
            "GITHUB_STEP_SUMMARY",
            "-V, --version",
        ],
    ));
}

use nils_test_support::help::{HelpCase, assert_help_contains};

#[test]
fn help_snapshot_root_help_documents_default_call_and_env() {
    assert_help_contains(HelpCase::root(
        "api-gql",
        &[
            "GraphQL runner",
            "default",
            "call",
            "Usage:",
            "Commands:",
            "EXAMPLES:",
            "ENVIRONMENT:",
            "EXIT CODES:",
            "GQL_HISTORY_FILE",
            "GQL_REPORT_INCLUDE_COMMAND_ENABLED",
            "GQL_VARS_MIN_LIMIT",
            "GQL_SCHEMA_FILE",
            "-V, --version",
        ],
    ));
}

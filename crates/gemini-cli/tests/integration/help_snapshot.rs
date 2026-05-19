use nils_test_support::help::{HelpCase, assert_help_contains};

#[test]
fn help_snapshot_root_help() {
    assert_help_contains(HelpCase::root(
        "gemini-cli",
        &[
            "Usage:",
            "Commands:",
            "Options:",
            "EXAMPLES:",
            "ENVIRONMENT:",
            "EXIT CODES:",
            "GEMINI_SECRET_CACHE_DIR",
            "CODE_ASSIST_ENDPOINT",
            "ZSH_CACHE_DIR",
            "-V, --version",
        ],
    ));
}

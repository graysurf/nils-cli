use nils_test_support::help::{HelpCase, assert_help_contains};

#[test]
fn help_snapshot_root_help() {
    assert_help_contains(HelpCase::root(
        "git-lock",
        &[
            "Usage:",
            "Commands:",
            "EXAMPLES:",
            "ENVIRONMENT:",
            "EXIT CODES:",
            "ZSH_CACHE_DIR",
            "-V, --version",
        ],
    ));
}

use nils_test_support::help::{HelpCase, assert_help_contains};

#[test]
fn help_snapshot_root_help() {
    assert_help_contains(HelpCase::root(
        "codex-cli",
        &[
            "Usage:",
            "Commands:",
            "Options:",
            "EXAMPLES:",
            "ENVIRONMENT:",
            "EXIT CODES:",
            "CODEX_SECRET_CACHE_DIR",
            "CODEX_CHATGPT_BASE_URL",
            "ZSH_CACHE_DIR",
            "-V, --version",
        ],
    ));
}

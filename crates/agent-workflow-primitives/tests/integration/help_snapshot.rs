use nils_test_support::help::{HelpCase, assert_help_contains};

#[test]
fn help_snapshot_all_workflow_primitive_binaries() {
    for (bin, env_needle) in [
        ("browser-session", "none"),
        ("canary-check", "SHELL"),
        ("docs-impact", "none"),
        ("heuristic-inbox", "HOME"),
        ("model-cross-check", "none"),
        ("repo-retro", "HOME"),
        ("review-evidence", "none"),
        ("review-specialists", "none"),
        ("skill-usage", "none"),
        ("test-first-evidence", "none"),
    ] {
        assert_help_contains(HelpCase::root(
            bin,
            &[
                "Usage:",
                "Commands:",
                "Options:",
                "EXAMPLES:",
                "ENVIRONMENT:",
                "EXIT CODES:",
                env_needle,
                "-V, --version",
            ],
        ));
    }
}

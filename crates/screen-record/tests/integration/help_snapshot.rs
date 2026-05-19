use nils_test_support::help::{HelpCase, assert_help_contains};

#[test]
fn help_snapshot_root_help() {
    assert_help_contains(HelpCase::root(
        "screen-record",
        &[
            "Usage:",
            "Options:",
            "EXAMPLES:",
            "ENVIRONMENT:",
            "EXIT CODES:",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "-V, --version",
        ],
    ));
}

use codex_cli::rate_limits::ansi;
use nils_test_support::{EnvGuard, GlobalStateLock};
use pretty_assertions::assert_eq;

#[test]
fn rate_limits_ansi_should_color_respects_no_color() {
    let lock = GlobalStateLock::new();
    let _no_color = EnvGuard::set(&lock, "NO_COLOR", "1");
    assert!(!ansi::should_color());
}

#[test]
fn rate_limits_ansi_format_percent_cell_and_token() {
    assert_eq!(
        ansi::format_percent_cell("5h:94%", 8, Some(false)),
        "  5h:94%"
    );
    assert_eq!(ansi::format_percent_cell("too_long", 3, Some(false)), "too");

    assert_eq!(ansi::format_percent_cell("oops", 4, Some(true)), "oops");

    for raw in ["x:0%", "x:80%", "x:60%", "x:40%", "x:20%", "x:19%"] {
        let rendered = ansi::format_percent_cell(raw, raw.chars().count(), Some(true));
        assert!(rendered.starts_with("\x1b["));
        assert!(rendered.ends_with("\x1b[0m"));
        assert!(rendered.contains(raw));
    }

    assert_eq!(ansi::format_percent_token("", Some(true)), "");
}

#[test]
fn rate_limits_ansi_format_name_cell() {
    assert_eq!(
        ansi::format_name_cell("work", 15, true, Some(false)),
        "work           "
    );
    assert_eq!(
        ansi::format_name_cell("work", 15, false, Some(true)),
        "work           "
    );

    let rendered = ansi::format_name_cell("work", 15, true, Some(true));
    assert!(rendered.starts_with("\x1b[38;2;199;146;234m"));
    assert!(rendered.ends_with("\x1b[0m"));
    assert!(rendered.contains("work"));
}

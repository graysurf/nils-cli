use codex_cli::rate_limits::render;
use nils_test_support::{EnvGuard, GlobalStateLock};
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn rate_limits_render_parses_usage_and_formats_windows() {
    let lock = GlobalStateLock::new();
    let _tz = EnvGuard::set(&lock, "TZ", "UTC");

    let usage = json!({
      "rate_limit": {
        "primary_window": { "limit_window_seconds": 18000, "used_percent": 6.0, "reset_at": 1700003600 },
        "secondary_window": { "limit_window_seconds": 604800, "used_percent": 12.0, "reset_at": 1700600000 }
      }
    });

    let parsed = render::parse_usage(&usage).expect("parse usage");
    let values = render::render_values(&parsed);
    assert_eq!(values.primary_label, "5h");
    assert_eq!(values.secondary_label, "Weekly");
    assert_eq!(values.primary_remaining, 94);
    assert_eq!(values.secondary_remaining, 88);

    let weekly = render::weekly_values(&values);
    assert_eq!(weekly.weekly_remaining, 88);
    assert_eq!(weekly.non_weekly_label, "5h");
    assert_eq!(weekly.non_weekly_remaining, 94);

    assert_eq!(
        render::format_window_seconds(604800).as_deref(),
        Some("Weekly")
    );
    assert_eq!(
        render::format_window_seconds(1209600).as_deref(),
        Some("2w")
    );
    assert_eq!(render::format_window_seconds(86400).as_deref(), Some("1d"));
    assert_eq!(render::format_window_seconds(3600).as_deref(), Some("1h"));
    assert_eq!(render::format_window_seconds(60).as_deref(), Some("1m"));
    assert_eq!(render::format_window_seconds(5).as_deref(), Some("5s"));
    assert_eq!(render::format_window_seconds(0), None);
}

#[test]
fn rate_limits_render_formats_time_and_remaining() {
    let lock = GlobalStateLock::new();
    let _tz = EnvGuard::set(&lock, "TZ", "UTC");

    assert_eq!(render::format_until_epoch_compact(0, 0), None);
    assert_eq!(
        render::format_until_epoch_compact(1, 10).as_deref(),
        Some(" 0h  0m")
    );
    assert_eq!(
        render::format_until_epoch_compact(10 + 86_400 * 2 + 3_600 * 5, 10).as_deref(),
        Some(" 2d  5h")
    );
    assert_eq!(
        render::format_until_epoch_compact(10 + 3_600 * 3 + 60 * 7, 10).as_deref(),
        Some(" 3h  7m")
    );

    assert!(render::format_epoch_local_datetime(1700600000).is_some());
    assert_eq!(
        render::format_epoch_local_datetime_with_offset(1700600000).as_deref(),
        Some("11-21 20:53 +00:00")
    );
    assert!(render::format_epoch_local(1700600000, "%Y").is_some());
}

#[test]
fn rate_limits_render_formats_time_with_local_timezone_offset() {
    let lock = GlobalStateLock::new();
    let _tz = EnvGuard::set(&lock, "TZ", "Asia/Taipei");

    assert_eq!(
        render::format_epoch_local_datetime_with_offset(1700600000).as_deref(),
        Some("11-22 04:53 +08:00")
    );
}

#[test]
fn rate_limits_render_parse_usage_rejects_missing_fields() {
    let missing = json!({});
    assert!(render::parse_usage(&missing).is_none());
}

use gemini_cli::rate_limits::render;

#[test]
fn rate_limits_render_parses_usage_and_formats_windows() {
    let usage = r#"{
  "rate_limit": {
    "primary_window": { "limit_window_seconds": 18000, "used_percent": 6.0, "reset_at": 1700003600 },
    "secondary_window": { "limit_window_seconds": 604800, "used_percent": 12.0, "reset_at": 1700600000 }
  }
}"#;

    let parsed = render::parse_usage(usage).expect("usage");
    let values = render::render_values(&parsed);
    assert_eq!(values.primary_label, "5h");
    assert_eq!(values.secondary_label, "Weekly");
    assert_eq!(values.primary_remaining, 94);
    assert_eq!(values.secondary_remaining, 88);

    let weekly = render::weekly_values(&values);
    assert_eq!(weekly.weekly_remaining, 88);
    assert_eq!(weekly.non_weekly_label, "5h");
    assert_eq!(weekly.non_weekly_remaining, 94);
}

#[test]
fn rate_limits_render_parses_code_assist_buckets() {
    let usage = r#"{
  "buckets": [
    {
      "tokenType": "REQUESTS",
      "remainingFraction": 0.94,
      "resetTime": "2099-01-01T00:00:00Z"
    },
    {
      "tokenType": "REQUESTS",
      "remainingFraction": 0.88,
      "resetTime": "2099-01-08T00:00:00Z"
    }
  ]
}"#;

    let parsed = render::parse_usage(usage).expect("usage");
    let values = render::render_values(&parsed);
    assert_eq!(values.primary_remaining, 94);
    assert_eq!(values.secondary_remaining, 88);

    let weekly = render::weekly_values(&values);
    assert_eq!(weekly.weekly_remaining, 88);
    assert_eq!(weekly.non_weekly_remaining, 94);
}

#[test]
fn rate_limits_render_format_window_seconds_variants() {
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
    assert_eq!(render::format_until_epoch_compact(0, 0), None);
    assert_eq!(
        render::format_until_epoch_compact(1, 10).as_deref(),
        Some(" 0h  0m")
    );
    assert_eq!(
        render::format_until_epoch_compact(10 + 2 * 86_400 + 5 * 3_600, 10).as_deref(),
        Some(" 2d  5h")
    );
    assert_eq!(
        render::format_until_epoch_compact(10 + 3 * 3_600 + 7 * 60, 10).as_deref(),
        Some(" 3h  7m")
    );

    assert_eq!(
        render::format_epoch_local_datetime(1700600000).as_deref(),
        Some("11-21 20:53")
    );
    assert_eq!(
        render::format_epoch_local_datetime_with_offset(1700600000).as_deref(),
        Some("11-21 20:53 +00:00")
    );
    assert_eq!(
        render::format_epoch_local(1700600000, "%Y-%m-%dT%H:%M:%S").as_deref(),
        Some("2023-11-21T20:53:20")
    );
}

#[test]
fn rate_limits_render_rejects_missing_fields() {
    assert!(render::parse_usage("{}").is_none());
    assert!(render::parse_usage(r#"{"rate_limit":{"primary_window":{}}}"#).is_none());
}

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use nils_common::env as shared_env;
use nils_common::rate_limits_ansi as ansi;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub used_percent: f64,
    pub remaining_percent: i64,
    pub resets_at: Option<String>,
}

pub fn render_usage_json(
    raw: &str,
    time_format: &str,
    stale: bool,
    stale_suffix: &str,
) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let usage = parse_usage_value(&value)?;
    render_usage(&usage, time_format, stale, stale_suffix)
}

pub fn parse_usage_value(value: &Value) -> Option<Usage> {
    let usage = value.get("usage").unwrap_or(value);
    let five_hour = parse_window(usage.get("five_hour"));
    let seven_day = parse_window(usage.get("seven_day"));
    if five_hour.is_none() && seven_day.is_none() {
        return None;
    }
    Some(Usage {
        five_hour,
        seven_day,
    })
}

fn parse_window(value: Option<&Value>) -> Option<Window> {
    let value = value?;
    let utilization = value.get("utilization")?.as_f64()?;
    let remaining_percent = remaining_percent(utilization);
    let resets_at = value
        .get("resets_at")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    Some(Window {
        used_percent: utilization.clamp(0.0, 100.0),
        remaining_percent,
        resets_at,
    })
}

fn render_usage(
    usage: &Usage,
    time_format: &str,
    stale: bool,
    stale_suffix: &str,
) -> Option<String> {
    let color_enabled = color_enabled();
    let mut parts = Vec::new();

    if let Some(window) = &usage.five_hour {
        parts.push(ansi::format_percent_token(
            &format!("5h:{}%", window.remaining_percent),
            Some(color_enabled),
        ));
    }

    if let Some(window) = &usage.seven_day {
        parts.push(ansi::format_percent_token(
            &format!("W:{}%", window.remaining_percent),
            Some(color_enabled),
        ));
    }

    if parts.is_empty() {
        return None;
    }

    parts.push(format_reset_time(
        usage
            .seven_day
            .as_ref()
            .and_then(|window| window.resets_at.as_deref()),
        time_format,
    ));

    let mut line = parts.join(" ");
    if stale && !stale_suffix.is_empty() {
        if color_enabled {
            line.push_str(&format!("\x1b[2m{stale_suffix}\x1b[0m"));
        } else {
            line.push_str(stale_suffix);
        }
    }
    Some(line)
}

fn remaining_percent(utilization: f64) -> i64 {
    let remaining = 100.0 - utilization.round();
    remaining.clamp(0.0, 100.0) as i64
}

fn format_reset_time(raw: Option<&str>, fmt: &str) -> String {
    let Some(raw) = raw else {
        return "?".to_string();
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return "?".to_string();
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return dt.with_timezone(&Local).format(fmt).to_string();
    }

    if let Ok(dt) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
        && let Some(local) = Local.from_local_datetime(&dt).single()
    {
        return local.format(fmt).to_string();
    }

    "?".to_string()
}

fn color_enabled() -> bool {
    if shared_env::no_color_enabled() {
        return false;
    }
    if shared_env::env_present("CLAUDE_PROMPT_SEGMENT_COLOR_ENABLED") {
        return shared_env::env_truthy("CLAUDE_PROMPT_SEGMENT_COLOR_ENABLED");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock};

    #[test]
    fn render_usage_json_supports_raw_and_wrapped_payloads() {
        let lock = GlobalStateLock::new();
        let _no_color = EnvGuard::set(&lock, "NO_COLOR", "1");
        let raw = r#"{
          "five_hour": {"utilization": 23.2, "resets_at": "2026-01-01T00:00:00+00:00"},
          "seven_day": {"utilization": 44.1, "resets_at": "2026-01-03T12:30:00+00:00"}
        }"#;
        assert!(
            render_usage_json(raw, "%Y", false, " (stale)")
                .expect("render")
                .starts_with("5h:77% W:56%")
        );

        let wrapped = format!(r#"{{"label":"ignored","usage":{raw}}}"#);
        assert!(
            render_usage_json(&wrapped, "%Y", false, " (stale)")
                .expect("render")
                .starts_with("5h:77% W:56%")
        );
    }

    #[test]
    fn render_usage_json_clamps_remaining_percent() {
        let lock = GlobalStateLock::new();
        let _no_color = EnvGuard::set(&lock, "NO_COLOR", "1");
        let raw = r#"{
          "five_hour": {"utilization": -25.0},
          "seven_day": {"utilization": 105.0}
        }"#;
        assert_eq!(
            render_usage_json(raw, "%Y", false, " (stale)").expect("render"),
            "5h:100% W:0% ?"
        );
    }

    #[test]
    fn render_usage_json_appends_stale_suffix_without_color_when_no_color_is_set() {
        let lock = GlobalStateLock::new();
        let _no_color = EnvGuard::set(&lock, "NO_COLOR", "1");
        let raw = r#"{"seven_day":{"utilization":50,"resets_at":"bad"}}"#;
        assert_eq!(
            render_usage_json(raw, "%Y", true, " (old)").expect("render"),
            "W:50% ? (old)"
        );
    }
}

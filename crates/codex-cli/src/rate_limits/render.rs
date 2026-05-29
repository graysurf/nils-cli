use chrono::{Local, TimeZone};
use serde_json::Value;

pub struct UsageData {
    pub primary: Window,
    pub secondary: Window,
}

pub struct Window {
    pub limit_window_seconds: i64,
    pub used_percent: f64,
    pub reset_at: i64,
}

pub struct RenderValues {
    pub primary_label: String,
    pub secondary_label: String,
    pub primary_remaining: i64,
    pub secondary_remaining: i64,
    pub primary_reset_epoch: i64,
    pub secondary_reset_epoch: i64,
}

pub struct WeeklyValues {
    pub weekly_remaining: i64,
    pub weekly_reset_epoch: i64,
    pub non_weekly_label: String,
    pub non_weekly_remaining: i64,
    pub non_weekly_reset_epoch: Option<i64>,
}

pub fn parse_usage(json: &Value) -> Option<UsageData> {
    let rate_limit = json.get("rate_limit")?;
    let primary = parse_window(rate_limit.get("primary_window")?)?;
    let secondary = parse_window(rate_limit.get("secondary_window")?)?;
    Some(UsageData { primary, secondary })
}

/// True when the usage payload is a valid response that explicitly reports no
/// active rate-limit window (`"rate_limit": null`).
///
/// The ChatGPT backend returns this when there is no usage recorded in the
/// current window (and it transiently returned it for every account during an
/// upstream incident). It is a benign "no data yet" state, not a malformed
/// payload, so callers should degrade gracefully (serve cache / show n/a)
/// rather than reporting an error. This mirrors the official codex client,
/// which maps a null `rate_limit` to empty windows instead of failing.
pub fn rate_limit_is_explicit_null(json: &Value) -> bool {
    matches!(json.get("rate_limit"), Some(Value::Null))
}

fn parse_window(value: &Value) -> Option<Window> {
    let limit_window_seconds = value.get("limit_window_seconds")?.as_i64()?;
    let used_percent = value
        .get("used_percent")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let reset_at = value.get("reset_at")?.as_i64()?;
    Some(Window {
        limit_window_seconds,
        used_percent,
        reset_at,
    })
}

pub fn render_values(data: &UsageData) -> RenderValues {
    let primary_label = format_window_seconds(data.primary.limit_window_seconds)
        .unwrap_or_else(|| "Primary".to_string());
    let secondary_label = format_window_seconds(data.secondary.limit_window_seconds)
        .unwrap_or_else(|| "Secondary".to_string());

    let primary_remaining = remaining_percent(data.primary.used_percent);
    let secondary_remaining = remaining_percent(data.secondary.used_percent);

    RenderValues {
        primary_label,
        secondary_label,
        primary_remaining,
        secondary_remaining,
        primary_reset_epoch: data.primary.reset_at,
        secondary_reset_epoch: data.secondary.reset_at,
    }
}

pub fn weekly_values(values: &RenderValues) -> WeeklyValues {
    let (
        weekly_remaining,
        weekly_reset_epoch,
        non_weekly_label,
        non_weekly_remaining,
        non_weekly_reset_epoch,
    ) = if values.primary_label == "Weekly" {
        (
            values.primary_remaining,
            values.primary_reset_epoch,
            values.secondary_label.clone(),
            values.secondary_remaining,
            Some(values.secondary_reset_epoch),
        )
    } else {
        (
            values.secondary_remaining,
            values.secondary_reset_epoch,
            values.primary_label.clone(),
            values.primary_remaining,
            Some(values.primary_reset_epoch),
        )
    };

    WeeklyValues {
        weekly_remaining,
        weekly_reset_epoch,
        non_weekly_label,
        non_weekly_remaining,
        non_weekly_reset_epoch,
    }
}

pub fn format_window_seconds(raw: i64) -> Option<String> {
    if raw <= 0 {
        return None;
    }
    if raw % 604_800 == 0 {
        let weeks = raw / 604_800;
        if weeks == 1 {
            return Some("Weekly".to_string());
        }
        return Some(format!("{weeks}w"));
    }
    if raw % 86_400 == 0 {
        return Some(format!("{}d", raw / 86_400));
    }
    if raw % 3_600 == 0 {
        return Some(format!("{}h", raw / 3_600));
    }
    if raw % 60 == 0 {
        return Some(format!("{}m", raw / 60));
    }
    Some(format!("{raw}s"))
}

pub fn format_epoch_local_datetime(epoch: i64) -> Option<String> {
    let dt = Local.timestamp_opt(epoch, 0).single()?;
    Some(dt.format("%m-%d %H:%M").to_string())
}

pub fn format_epoch_local_datetime_with_offset(epoch: i64) -> Option<String> {
    let dt = Local.timestamp_opt(epoch, 0).single()?;
    Some(dt.format("%m-%d %H:%M %:z").to_string())
}

pub fn format_epoch_local(epoch: i64, fmt: &str) -> Option<String> {
    let dt = Local.timestamp_opt(epoch, 0).single()?;
    Some(dt.format(fmt).to_string())
}

pub fn format_until_epoch_compact(target_epoch: i64, now_epoch: i64) -> Option<String> {
    if target_epoch <= 0 || now_epoch <= 0 {
        return None;
    }
    let remaining = target_epoch - now_epoch;
    if remaining <= 0 {
        return Some(format!("{:>2}h {:>2}m", 0, 0));
    }

    if remaining >= 86_400 {
        let days = remaining / 86_400;
        let hours = (remaining % 86_400) / 3_600;
        return Some(format!("{:>2}d {:>2}h", days, hours));
    }

    let hours = remaining / 3_600;
    let minutes = (remaining % 3_600) / 60;
    Some(format!("{:>2}h {:>2}m", hours, minutes))
}

fn remaining_percent(used_percent: f64) -> i64 {
    let remaining = 100.0 - used_percent;
    remaining.round() as i64
}

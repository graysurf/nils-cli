use chrono::{Local, TimeZone};
use serde_json::Value;

pub struct UsageData {
    pub primary: Option<Window>,
    pub secondary: Option<Window>,
}

pub struct Window {
    pub limit_window_seconds: i64,
    pub used_percent: f64,
    pub reset_at: i64,
}

#[derive(Clone, Debug)]
pub struct WindowValues {
    pub label: String,
    pub remaining: i64,
    pub reset_epoch: i64,
}

pub struct RenderValues {
    pub primary: Option<WindowValues>,
    pub secondary: Option<WindowValues>,
}

pub struct WeeklyValues {
    pub weekly: Option<WindowValues>,
    pub non_weekly: Option<WindowValues>,
}

pub fn parse_usage(json: &Value) -> Option<UsageData> {
    let rate_limit = json.get("rate_limit")?.as_object()?;
    let primary = parse_optional_window(rate_limit.get("primary_window"))?;
    let secondary = parse_optional_window(rate_limit.get("secondary_window"))?;
    Some(UsageData { primary, secondary })
}

fn parse_optional_window(value: Option<&Value>) -> Option<Option<Window>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(value) => parse_window(value).map(Some),
    }
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
pub fn rate_limit_has_no_windows(json: &Value) -> bool {
    match json.get("rate_limit") {
        Some(Value::Null) => true,
        Some(Value::Object(rate_limit)) => ["primary_window", "secondary_window"]
            .into_iter()
            .all(|key| matches!(rate_limit.get(key), None | Some(Value::Null))),
        _ => false,
    }
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
    RenderValues {
        primary: data
            .primary
            .as_ref()
            .map(|window| render_window_values(window, "Primary")),
        secondary: data
            .secondary
            .as_ref()
            .map(|window| render_window_values(window, "Secondary")),
    }
}

fn render_window_values(window: &Window, fallback_label: &str) -> WindowValues {
    WindowValues {
        label: format_window_seconds(window.limit_window_seconds)
            .unwrap_or_else(|| fallback_label.to_string()),
        remaining: remaining_percent(window.used_percent),
        reset_epoch: window.reset_at,
    }
}

pub fn weekly_values(values: &RenderValues) -> WeeklyValues {
    let mut weekly = None;
    let mut non_weekly = None;
    for window in [&values.primary, &values.secondary].into_iter().flatten() {
        if window.label == "Weekly" {
            weekly = Some(window.clone());
        } else if non_weekly.is_none() {
            non_weekly = Some(window.clone());
        }
    }
    WeeklyValues { weekly, non_weekly }
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

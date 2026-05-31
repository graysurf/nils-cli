pub mod json;
pub mod text;

pub use json::{emit_data, emit_data_with_warnings, emit_json_error};

pub fn format_item_id(item_id: i64) -> String {
    format!("itm_{item_id:08}")
}

pub fn parse_item_id(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(suffix) = trimmed.strip_prefix("itm_") {
        let parsed = suffix.parse::<i64>().ok()?;
        return (parsed > 0).then_some(parsed);
    }

    let parsed = trimmed.parse::<i64>().ok()?;
    (parsed > 0).then_some(parsed)
}

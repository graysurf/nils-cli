use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use crate::errors::AppError;

pub fn parse_rfc3339_utc(raw: &str, flag: &str) -> Result<DateTime<Utc>, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::usage(format!("{flag} must be non-empty")));
    }

    let parsed = chrono::DateTime::parse_from_rfc3339(trimmed).map_err(|err| {
        AppError::usage(format!("{flag} must be valid RFC3339: {err}")).with_code("invalid-time")
    })?;
    Ok(parsed.with_timezone(&Utc))
}

pub fn parse_timezone(raw: &str) -> Result<Tz, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::usage("--tz must be non-empty"));
    }

    trimmed.parse::<Tz>().map_err(|_| {
        AppError::usage("--tz must be a valid IANA timezone").with_code("invalid-timezone")
    })
}

pub fn format_utc(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

pub mod auto_refresh;
pub mod current;
pub mod login;
pub mod output;
pub mod refresh;
pub mod remove;
pub mod save;
pub mod sync;
pub mod use_secret;

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const SECRET_FILE_MODE: u32 = nils_common::fs::SECRET_FILE_MODE;

pub fn identity_from_auth_file(path: &Path) -> io::Result<Option<String>> {
    crate::runtime::auth::identity_from_auth_file(path).map_err(core_error_to_io)
}

pub fn email_from_auth_file(path: &Path) -> io::Result<Option<String>> {
    crate::runtime::auth::email_from_auth_file(path).map_err(core_error_to_io)
}

pub fn account_id_from_auth_file(path: &Path) -> io::Result<Option<String>> {
    crate::runtime::auth::account_id_from_auth_file(path).map_err(core_error_to_io)
}

pub fn last_refresh_from_auth_file(path: &Path) -> io::Result<Option<String>> {
    crate::runtime::auth::last_refresh_from_auth_file(path).map_err(core_error_to_io)
}

pub fn identity_key_from_auth_file(path: &Path) -> io::Result<Option<String>> {
    crate::runtime::auth::identity_key_from_auth_file(path).map_err(core_error_to_io)
}

pub(crate) fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    nils_common::fs::write_atomic(path, contents, mode).map_err(io_error_from_atomic_write)
}

pub(crate) fn write_timestamp(path: &Path, iso: Option<&str>) -> io::Result<()> {
    nils_common::fs::write_timestamp(path, iso).map_err(io_error_from_timestamp_write)
}

pub(crate) fn normalize_iso(raw: &str) -> String {
    let mut trimmed = crate::json::strip_newlines(raw);
    if let Some(dot) = trimmed.find('.')
        && trimmed.ends_with('Z')
    {
        trimmed.truncate(dot);
        trimmed.push('Z');
    }
    trimmed
}

pub(crate) fn parse_rfc3339_epoch(raw: &str) -> Option<i64> {
    let normalized = normalize_iso(raw);
    let (datetime, offset_seconds) = if normalized.ends_with('Z') {
        (&normalized[..normalized.len().saturating_sub(1)], 0i64)
    } else {
        if normalized.len() < 6 {
            return None;
        }
        let tail_index = normalized.len() - 6;
        let sign = normalized.as_bytes().get(tail_index).copied()? as char;
        if sign != '+' && sign != '-' {
            return None;
        }
        if normalized.as_bytes().get(tail_index + 3).copied()? as char != ':' {
            return None;
        }
        let hours = parse_u32(&normalized[tail_index + 1..tail_index + 3])? as i64;
        let minutes = parse_u32(&normalized[tail_index + 4..])? as i64;
        let mut offset = hours * 3600 + minutes * 60;
        if sign == '-' {
            offset = -offset;
        }
        (&normalized[..tail_index], offset)
    };

    if datetime.len() != 19 {
        return None;
    }
    if datetime.as_bytes().get(4).copied()? as char != '-'
        || datetime.as_bytes().get(7).copied()? as char != '-'
        || datetime.as_bytes().get(10).copied()? as char != 'T'
        || datetime.as_bytes().get(13).copied()? as char != ':'
        || datetime.as_bytes().get(16).copied()? as char != ':'
    {
        return None;
    }

    let year = parse_i64(&datetime[0..4])?;
    let month = parse_u32(&datetime[5..7])? as i64;
    let day = parse_u32(&datetime[8..10])? as i64;
    let hour = parse_u32(&datetime[11..13])? as i64;
    let minute = parse_u32(&datetime[14..16])? as i64;
    let second = parse_u32(&datetime[17..19])? as i64;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let local_epoch = days * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(local_epoch - offset_seconds)
}

pub(crate) fn now_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn now_utc_iso() -> String {
    epoch_to_utc_iso(now_epoch_seconds())
}

pub(crate) fn temp_file_path(prefix: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    path.push(format!("{prefix}-{pid}-{nanos}.json"));
    path
}

fn core_error_to_io(err: crate::runtime::CoreError) -> io::Error {
    io::Error::other(err.to_string())
}

fn io_error_from_atomic_write(err: nils_common::fs::AtomicWriteError) -> io::Error {
    match err {
        nils_common::fs::AtomicWriteError::CreateParentDir { source, .. }
        | nils_common::fs::AtomicWriteError::CreateTempFile { source, .. }
        | nils_common::fs::AtomicWriteError::WriteTempFile { source, .. }
        | nils_common::fs::AtomicWriteError::SetPermissions { source, .. }
        | nils_common::fs::AtomicWriteError::ReplaceFile { source, .. }
        | nils_common::fs::AtomicWriteError::SyncTempFile { source, .. } => source,
        nils_common::fs::AtomicWriteError::TempPathExhausted { target, .. } => io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("failed to create unique temp file for {}", target.display()),
        ),
    }
}

fn io_error_from_timestamp_write(err: nils_common::fs::TimestampError) -> io::Error {
    match err {
        nils_common::fs::TimestampError::CreateParentDir { source, .. }
        | nils_common::fs::TimestampError::WriteFile { source, .. }
        | nils_common::fs::TimestampError::RemoveFile { source, .. } => source,
    }
}

fn parse_u32(raw: &str) -> Option<u32> {
    raw.parse::<u32>().ok()
}

fn parse_i64(raw: &str) -> Option<i64> {
    raw.parse::<i64>().ok()
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year / 400
    } else {
        (adjusted_year - 399) / 400
    };
    let year_of_era = adjusted_year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn epoch_to_utc_iso(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let seconds_of_day = epoch.rem_euclid(86_400);

    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let full_year = year + i64::from(month <= 2);

    (full_year, month, day)
}

#[cfg(test)]
mod tests {
    use super::{normalize_iso, parse_rfc3339_epoch};

    #[test]
    fn normalize_iso_removes_fractional_seconds() {
        assert_eq!(
            normalize_iso("2025-01-20T12:34:56.789Z"),
            "2025-01-20T12:34:56Z"
        );
    }

    #[test]
    fn parse_rfc3339_epoch_supports_zulu_and_offsets() {
        assert_eq!(parse_rfc3339_epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_epoch("1970-01-01T01:00:00+01:00"), Some(0));
    }
}

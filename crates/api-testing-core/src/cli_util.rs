use std::io::Write;
use std::path::{Path, PathBuf};

use nils_common::shell::quote_posix_single;

use crate::{Result, env_file};

pub trait WarnSink {
    fn warn(&mut self, message: &str);
}

impl WarnSink for Vec<String> {
    fn warn(&mut self, message: &str) {
        self.push(message.to_string());
    }
}

impl<'a> WarnSink for dyn Write + 'a {
    fn warn(&mut self, message: &str) {
        let _ = writeln!(self, "{message}");
    }
}

pub fn trim_non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

pub fn bool_from_env<S: WarnSink + ?Sized>(
    raw: Option<String>,
    name: &str,
    default: bool,
    tool_label: Option<&str>,
    warnings: &mut S,
) -> bool {
    let raw = raw.unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return default;
    }
    match raw.to_ascii_lowercase().as_str() {
        "true" => true,
        "false" => false,
        _ => {
            let label = tool_label.and_then(|l| (!l.trim().is_empty()).then_some(l));
            let msg = match label {
                Some(label) => format!(
                    "{label}: warning: {name} must be true|false (got: {raw}); treating as false"
                ),
                None => format!("{name} must be true|false (got: {raw}); treating as false"),
            };
            warnings.warn(&msg);
            false
        }
    }
}

pub fn parse_u64_default(raw: Option<String>, default: u64, min: u64) -> u64 {
    let raw = raw.unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return default;
    }
    if !raw.chars().all(|c| c.is_ascii_digit()) {
        return default;
    }
    let Ok(v) = raw.parse::<u64>() else {
        return default;
    };
    v.max(min)
}

pub fn to_env_key(s: &str) -> String {
    env_file::normalize_env_key(s)
}

pub fn slugify(s: &str) -> String {
    let s = s.trim().to_ascii_lowercase();
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        let ok = c.is_ascii_alphanumeric();
        if ok {
            out.push(c);
            prev_dash = false;
            continue;
        }
        if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }

    while out.ends_with('-') {
        out.pop();
    }

    out
}

pub fn maybe_relpath(path: &Path, base: &Path) -> String {
    if path == base {
        return ".".to_string();
    }

    if let Ok(stripped) = path.strip_prefix(base) {
        let s = stripped.to_string_lossy();
        if s.is_empty() {
            return ".".to_string();
        }
        return s.to_string();
    }

    path.to_string_lossy().to_string()
}

pub fn shell_quote(s: &str) -> String {
    quote_posix_single(s)
}

pub fn list_available_suffixes(file: &Path, prefix: &str) -> Vec<String> {
    if !file.is_file() {
        return Vec::new();
    }

    let Ok(content) = std::fs::read_to_string(file) else {
        return Vec::new();
    };

    let mut out: Vec<String> = Vec::new();
    for raw_line in content.lines() {
        let line = raw_line.trim_end_matches('\r');
        let mut line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export")
            && rest.starts_with(char::is_whitespace)
        {
            line = rest.trim();
        }

        let Some((lhs, _rhs)) = line.split_once('=') else {
            continue;
        };
        let key = lhs.trim();
        let Some(suffix) = key.strip_prefix(prefix) else {
            continue;
        };
        if suffix.is_empty()
            || !suffix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            continue;
        }
        out.push(suffix.to_ascii_lowercase());
    }

    out.sort();
    out.dedup();
    out
}

pub fn find_git_root(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent,
            _ => return None,
        }
    }
}

pub fn history_timestamp_now() -> Result<String> {
    let format = time::format_description::parse_borrowed::<2>(
        "[year]-[month]-[day]T[hour]:[minute]:[second][offset_hour sign:mandatory][offset_minute]",
    )?;
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    Ok(now.format(&format)?)
}

pub fn report_stamp_now() -> Result<String> {
    let format =
        time::format_description::parse_borrowed::<2>("[year][month][day]-[hour][minute]")?;
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    Ok(now.format(&format)?)
}

pub fn report_date_now() -> Result<String> {
    let format = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")?;
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    Ok(now.format(&format)?)
}

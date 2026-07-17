//! Content validation for resolved documents.
//!
//! Beyond existence, a required document must be non-empty, contain a declared
//! marker (when one is declared), and — if the catalog declares a freshness
//! window — carry a recent enough `last-reviewed: YYYY-MM-DD` line. A scaffolded
//! placeholder (empty, or missing its marker) therefore fails validation.

use std::path::Path;

use crate::model::{DocumentEntry, DocumentValidation, FreshnessCheck};

/// Validate the content of a resolved document on disk against its catalog
/// entry. `path` is the already-resolved absolute path.
pub fn validate(path: &Path, entry: &DocumentEntry) -> DocumentValidation {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return DocumentValidation::missing();
    };
    validate_content(&raw, entry)
}

pub fn validate_content(raw: &str, entry: &DocumentEntry) -> DocumentValidation {
    let non_empty = !raw.trim().is_empty();

    let marker_present = entry.marker.as_deref().map(|marker| raw.contains(marker));

    let freshness = match entry.freshness_days {
        None => FreshnessCheck::NotDeclared,
        Some(window_days) => evaluate_freshness(raw, window_days),
    };

    let marker_ok = marker_present.unwrap_or(true);
    let valid = non_empty && marker_ok && freshness.passes();

    DocumentValidation {
        exists: true,
        non_empty,
        marker_present,
        freshness,
        valid,
    }
}

fn evaluate_freshness(content: &str, window_days: u64) -> FreshnessCheck {
    let Some(reviewed) = parse_last_reviewed(content) else {
        return FreshnessCheck::Unknown;
    };
    let Some(today) = today_days() else {
        return FreshnessCheck::Unknown;
    };
    if today < reviewed {
        // Reviewed "in the future" — treat as fresh rather than stale.
        return FreshnessCheck::Fresh;
    }
    if today - reviewed <= window_days as i64 {
        FreshnessCheck::Fresh
    } else {
        FreshnessCheck::Stale
    }
}

/// Find a `last-reviewed: YYYY-MM-DD` (or `last_reviewed:`) line and return its
/// date as days since the civil epoch.
fn parse_last_reviewed(content: &str) -> Option<i64> {
    for line in content.lines() {
        let lower = line.to_ascii_lowercase();
        let trimmed = lower.trim_start_matches(['#', '-', '*', ' ', '\t', '<', '!']);
        for key in ["last-reviewed:", "last_reviewed:"] {
            if let Some(rest) = trimmed.strip_prefix(key)
                && let Some(days) = parse_iso_date(rest.trim())
            {
                return Some(days);
            }
        }
    }
    None
}

/// Parse a leading `YYYY-MM-DD` date into days since the civil epoch.
fn parse_iso_date(value: &str) -> Option<i64> {
    let date_part = value.split_whitespace().next().unwrap_or(value);
    let mut parts = date_part.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// Current date as days since the civil epoch. Honors the `AGENT_DOCS_NOW`
/// override (an ISO `YYYY-MM-DD`) so freshness checks are deterministic in
/// tests.
fn today_days() -> Option<i64> {
    if let Ok(raw) = std::env::var("AGENT_DOCS_NOW") {
        return parse_iso_date(raw.trim());
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(secs / 86_400)
}

/// Days since 1970-01-01 for a Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Context, Scope, When};
    use std::path::PathBuf;

    fn entry(marker: Option<&str>, freshness_days: Option<u64>) -> DocumentEntry {
        DocumentEntry {
            context: Context::parse("project-dev").unwrap(),
            scope: Scope::Project,
            path: PathBuf::from("DEVELOPMENT.md"),
            products: Vec::new(),
            required: true,
            when: When::Always,
            when_raw: "always".to_string(),
            marker: marker.map(ToString::to_string),
            freshness_days,
            notes: None,
        }
    }

    fn temp_doc(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "agent-docs-content-{}-{}-{name}",
            std::process::id(),
            name.len()
        ));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn missing_file_is_invalid() {
        let path = std::env::temp_dir().join("agent-docs-content-does-not-exist-xyz");
        let _ = std::fs::remove_file(&path);
        let v = validate(&path, &entry(None, None));
        assert!(!v.exists);
        assert!(!v.valid);
    }

    #[test]
    fn empty_file_is_invalid() {
        let path = temp_doc("empty", "   \n\t\n");
        let v = validate(&path, &entry(None, None));
        assert!(v.exists);
        assert!(!v.non_empty);
        assert!(!v.valid);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn marker_missing_is_invalid() {
        let path = temp_doc("nomarker", "# Dev\n\nsome content without the marker\n");
        let v = validate(&path, &entry(Some("## Validation"), None));
        assert_eq!(v.marker_present, Some(false));
        assert!(!v.valid);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn marker_present_is_valid() {
        let path = temp_doc("marker", "# Dev\n\n## Validation\n\nrun the tests\n");
        let v = validate(&path, &entry(Some("## Validation"), None));
        assert_eq!(v.marker_present, Some(true));
        assert!(v.valid);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn freshness_fresh_vs_stale() {
        // SAFETY: single-threaded test setting a process env override.
        unsafe { std::env::set_var("AGENT_DOCS_NOW", "2026-05-30") };
        let fresh = temp_doc("fresh", "# Dev\n\nlast-reviewed: 2026-05-01\n");
        let stale = temp_doc("stale", "# Dev\n\nlast-reviewed: 2024-01-01\n");
        let unknown = temp_doc("unknownfresh", "# Dev\n\nno date here\n");

        assert_eq!(
            validate(&fresh, &entry(None, Some(180))).freshness,
            FreshnessCheck::Fresh
        );
        assert_eq!(
            validate(&stale, &entry(None, Some(180))).freshness,
            FreshnessCheck::Stale
        );
        assert_eq!(
            validate(&unknown, &entry(None, Some(180))).freshness,
            FreshnessCheck::Unknown
        );
        assert!(!validate(&stale, &entry(None, Some(180))).valid);

        unsafe { std::env::remove_var("AGENT_DOCS_NOW") };
        for p in [fresh, stale, unknown] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn days_from_civil_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
    }
}

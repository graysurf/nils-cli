//! Scrub-log emitter.
//!
//! Writes the `<ISO8601>.scrub.log` sibling file for every snapshot
//! whose payload had at least one secret redacted. The format is
//! line-oriented and stable so reviewers can diff successive snapshot
//! logs by eye and so downstream tooling can grep it without parsing.

use std::fs;
use std::io::Write;
use std::path::Path;

use super::{Match, PATTERN_SET};

/// Format the scrub log body. Stable so fixture goldens can diff.
pub fn format_log(matches: &[Match]) -> String {
    let mut out = String::new();
    out.push_str("# plan-archive scrub log\n");
    out.push_str(&format!("# pattern_set: {PATTERN_SET}\n"));
    for m in matches {
        out.push_str(&format!(
            "match pattern={} offset={} length={} redaction={}\n",
            m.pattern_id, m.offset, m.length, m.redaction_length
        ));
    }
    let total = matches.len();
    let mut triggered: Vec<String> = matches.iter().map(|m| m.pattern_id.clone()).collect();
    triggered.sort();
    triggered.dedup();
    out.push_str(&format!(
        "summary patterns_triggered={} total_matches={}\n",
        if triggered.is_empty() {
            "none".to_string()
        } else {
            triggered.join(",")
        },
        total
    ));
    out
}

/// Write the scrub log to `path` if and only if `matches` is non-empty.
///
/// Returns `Ok(true)` when a log was written, `Ok(false)` when no log
/// was needed.
pub fn write_log_if_any(path: &Path, matches: &[Match]) -> std::io::Result<bool> {
    if matches.is_empty() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = format_log(matches);
    let mut f = fs::File::create(path)?;
    f.write_all(body.as_bytes())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrub::scrub_text;
    use std::fs;

    #[test]
    fn empty_matches_produces_summary_with_none() {
        let body = format_log(&[]);
        assert!(body.contains("# plan-archive scrub log"));
        assert!(body.contains("# pattern_set: v1"));
        assert!(body.contains("summary patterns_triggered=none total_matches=0"));
    }

    #[test]
    fn formats_one_line_per_match_and_a_summary() {
        let result = scrub_text("auth: ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let body = format_log(&result.matches);
        // One match line, plus the two header lines + one summary line.
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 4, "log:\n{body}");
        assert!(lines[2].starts_with("match pattern=github-token offset="));
        assert!(lines[3].starts_with("summary patterns_triggered=github-token"));
    }

    #[test]
    fn write_log_if_any_skips_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snap.scrub.log");
        let wrote = write_log_if_any(&path, &[]).unwrap();
        assert!(!wrote);
        assert!(!path.exists());
    }

    #[test]
    fn write_log_if_any_writes_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snap.scrub.log");
        let result = scrub_text("secret: abcdef1234567890");
        let wrote = write_log_if_any(&path, &result.matches).unwrap();
        assert!(wrote);
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.contains("match pattern=generic-secret-kv"));
        assert!(body.contains("total_matches=1"));
    }

    #[test]
    fn log_never_contains_the_secret_itself() {
        let secret = "topsecretvalue9999";
        let payload = format!("password: {secret}");
        let result = scrub_text(&payload);
        let body = format_log(&result.matches);
        assert!(!body.contains(secret), "log leaked secret: {body}");
    }
}

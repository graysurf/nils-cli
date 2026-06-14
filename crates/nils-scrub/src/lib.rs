//! Secret-scrub library v1.
//!
//! Shared across the nils-cli workspace by any caller that must redact
//! secrets from text before it is persisted (for example
//! `plan-archive refresh` snapshots and `evidence migrate` rollups). The
//! pattern set is intentionally small and stable so the resulting
//! `.scrub.log` sibling stays diffable across writes. Callers label their
//! own scrub-log header via [`log::format_log`] / [`log::write_log_if_any`].

use regex::Regex;
use serde::Serialize;
use std::sync::OnceLock;

pub mod log;

pub use log::{format_log, write_log_if_any};

/// Pattern set identifier embedded in scrub-log headers and the
/// consuming CLI's JSON envelope.
pub const PATTERN_SET: &str = "v1";

/// Placeholder text inserted in place of the matched secret.
pub const REDACTION_TOKEN: &str = "[REDACTED]";

/// A single secret match found in the input payload.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Match {
    /// Stable pattern id (`github-token`, `aws-access-key-id`, ...).
    pub pattern_id: String,
    /// Byte offset in the original input where the redacted span
    /// starts.
    pub offset: usize,
    /// Byte length of the redacted span.
    pub length: usize,
    /// Byte length of the replacement token.
    pub redaction_length: usize,
}

/// Result of running [`scrub_text`] on a payload.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScrubResult {
    /// Payload with every matched span replaced by [`REDACTION_TOKEN`].
    pub redacted: String,
    /// Matches in original-payload byte order.
    pub matches: Vec<Match>,
}

impl ScrubResult {
    /// Distinct pattern ids that triggered at least one match.
    pub fn triggered_patterns(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.matches.iter().map(|m| m.pattern_id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

struct Pattern {
    id: &'static str,
    regex: Regex,
    /// Which capture group to redact. `0` means the entire match.
    capture: usize,
}

fn patterns() -> &'static [Pattern] {
    static P: OnceLock<Vec<Pattern>> = OnceLock::new();
    P.get_or_init(|| {
        vec![
            Pattern {
                id: "github-token",
                regex: Regex::new(r"gh[opusr]_[A-Za-z0-9_]{36,}").unwrap(),
                capture: 0,
            },
            Pattern {
                id: "gitlab-token",
                regex: Regex::new(r"glpat-[A-Za-z0-9_\-]{20,}").unwrap(),
                capture: 0,
            },
            Pattern {
                id: "bitbucket-app-password",
                regex: Regex::new(r"ATBB[A-Za-z0-9]{16,}").unwrap(),
                capture: 0,
            },
            Pattern {
                id: "aws-access-key-id",
                regex: Regex::new(
                    r"(?:AKIA|ASIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|A3T[A-Z0-9])[A-Z0-9]{16}",
                )
                .unwrap(),
                capture: 0,
            },
            Pattern {
                id: "generic-secret-kv",
                regex: Regex::new(
                    r#"(?i)(?:secret|token|password|api[_-]?key)\s*[:=]\s*["']?([A-Za-z0-9_\-\.+/=]{12,})["']?"#,
                )
                .unwrap(),
                capture: 1,
            },
            Pattern {
                id: "pem-private-key",
                regex: Regex::new(
                    r"(?s)-----BEGIN [A-Z ]+PRIVATE KEY-----.*?-----END [A-Z ]+PRIVATE KEY-----",
                )
                .unwrap(),
                capture: 0,
            },
        ]
    })
}

/// Public read-only view of the configured v1 patterns. Stable for
/// downstream `--list-patterns` style flags.
pub fn pattern_ids() -> Vec<&'static str> {
    patterns().iter().map(|p| p.id).collect()
}

/// Scan `input` for secrets in the v1 pattern set, replace every
/// matched span with [`REDACTION_TOKEN`], and return both the
/// redacted text and per-match metadata.
pub fn scrub_text(input: &str) -> ScrubResult {
    let mut spans: Vec<(usize, usize, &'static str)> = Vec::new();
    for p in patterns() {
        for caps in p.regex.captures_iter(input) {
            let Some(m) = caps.get(p.capture) else {
                continue;
            };
            spans.push((m.start(), m.end(), p.id));
        }
    }

    // Earliest span wins on overlap. Order by (offset asc, length
    // desc) so two patterns matching the same offset prefer the
    // wider one.
    spans.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| (b.1 - b.0).cmp(&(a.1 - a.0))));
    let mut kept: Vec<(usize, usize, &'static str)> = Vec::new();
    for span in spans {
        if let Some(last) = kept.last()
            && span.0 < last.1
        {
            continue;
        }
        kept.push(span);
    }

    let matches: Vec<Match> = kept
        .iter()
        .map(|(start, end, id)| Match {
            pattern_id: (*id).to_string(),
            offset: *start,
            length: end - start,
            redaction_length: REDACTION_TOKEN.len(),
        })
        .collect();

    let mut redacted = input.to_string();
    for (start, end, _) in kept.iter().rev() {
        redacted.replace_range(start..end, REDACTION_TOKEN);
    }

    ScrubResult { redacted, matches }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_github_token() {
        let payload = "auth: ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa next";
        let result = scrub_text(payload);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].pattern_id, "github-token");
        assert!(result.redacted.contains(REDACTION_TOKEN));
        assert!(!result.redacted.contains("ghp_"));
    }

    #[test]
    fn detects_gitlab_pat() {
        let payload = "url: https://oauth2:glpat-AAAAAAAAAAAAAAAAAAAA@example.com/x.git";
        let result = scrub_text(payload);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].pattern_id, "gitlab-token");
        assert!(!result.redacted.contains("glpat-"));
    }

    #[test]
    fn detects_bitbucket_app_password() {
        let payload = "pass=ATBB1234567890abcdefXYZ end";
        let result = scrub_text(payload);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].pattern_id, "bitbucket-app-password");
    }

    #[test]
    fn detects_aws_access_key_id() {
        let payload = "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE end";
        let result = scrub_text(payload);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].pattern_id, "aws-access-key-id");
    }

    #[test]
    fn detects_generic_secret_kv_value_only() {
        let payload = "password: \"supersecretvalue12\" trailing";
        let result = scrub_text(payload);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].pattern_id, "generic-secret-kv");
        assert!(result.redacted.starts_with("password: \"[REDACTED]\""));
    }

    #[test]
    fn detects_pem_private_key_block() {
        let payload = "before\n-----BEGIN RSA PRIVATE KEY-----\nlines\nlines\n-----END RSA PRIVATE KEY-----\nafter";
        let result = scrub_text(payload);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].pattern_id, "pem-private-key");
        assert!(result.redacted.contains("before\n[REDACTED]\nafter"));
    }

    #[test]
    fn negative_short_token_not_matched() {
        let payload = "ghp_tooshort and glpat-short and no_aws_here";
        let result = scrub_text(payload);
        assert!(
            result.matches.is_empty(),
            "got matches: {:?}",
            result.matches
        );
    }

    #[test]
    fn negative_random_word_not_secret() {
        let payload = "the password is in the doc";
        let result = scrub_text(payload);
        assert!(
            result.matches.is_empty(),
            "got matches: {:?}",
            result.matches
        );
    }

    #[test]
    fn end_to_end_all_patterns_in_one_payload() {
        let payload = "\
A: ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
B: glpat-AAAAAAAAAAAAAAAAAAAA
C: ATBB1234567890abcdefXYZ
D: AKIAIOSFODNN7EXAMPLE
E: secret=topsecretvalue42
F: -----BEGIN OPENSSH PRIVATE KEY-----\nbody\n-----END OPENSSH PRIVATE KEY-----
";
        let result = scrub_text(payload);
        let ids = result.triggered_patterns();
        assert_eq!(
            ids,
            vec![
                "aws-access-key-id".to_string(),
                "bitbucket-app-password".to_string(),
                "generic-secret-kv".to_string(),
                "github-token".to_string(),
                "gitlab-token".to_string(),
                "pem-private-key".to_string(),
            ]
        );
        assert!(!result.redacted.contains("ghp_"));
        assert!(!result.redacted.contains("glpat-"));
        assert!(!result.redacted.contains("AKIA"));
        assert!(!result.redacted.contains("ATBB"));
        assert!(!result.redacted.contains("topsecretvalue42"));
        assert!(!result.redacted.contains("PRIVATE KEY"));
    }

    #[test]
    fn empty_input_returns_empty_result() {
        let result = scrub_text("");
        assert!(result.matches.is_empty());
        assert_eq!(result.redacted, "");
    }

    #[test]
    fn overlapping_matches_prefer_earliest_widest() {
        // `password: glpat-AAAAAAAAAAAAAAAAAAAA` triggers both
        // generic-secret-kv (value-only) and gitlab-token (full token).
        // Earliest+widest wins → generic-secret-kv covers the same
        // bytes; the result should not double-redact.
        let payload = "password: glpat-AAAAAAAAAAAAAAAAAAAA";
        let result = scrub_text(payload);
        let redaction_count = result.redacted.matches(REDACTION_TOKEN).count();
        assert_eq!(redaction_count, 1, "redacted: {}", result.redacted);
    }

    #[test]
    fn pattern_ids_listing_is_stable() {
        let ids = pattern_ids();
        assert_eq!(
            ids,
            vec![
                "github-token",
                "gitlab-token",
                "bitbucket-app-password",
                "aws-access-key-id",
                "generic-secret-kv",
                "pem-private-key",
            ]
        );
    }
}

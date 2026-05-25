//! Domain-neutral secret redaction shared across evidence-producing CLIs.
//!
//! `redact_text` matches two regex families:
//! - assignment-style secrets (`token=...`, `password: "..."`, etc.) where the
//!   key is preserved and only the value is replaced;
//! - bare token shapes (`sk-...`, `ghp_...`, `github_pat_...`, `xox?-...`,
//!   `bearer ...`) where the entire match is replaced.
//!
//! The returned [`RedactedString`] carries both the rewritten value and the
//! number of replacements applied so callers can report redaction counts in
//! their evidence artifacts.

use std::sync::OnceLock;

use regex::Regex;

/// Output of [`redact_text`]: the rewritten value plus the number of secret
/// replacements that were applied.
#[derive(Debug, Clone)]
pub struct RedactedString {
    pub value: String,
    pub replacements: usize,
}

/// Replace secret-like substrings in `input` with `[REDACTED]` markers.
pub fn redact_text(input: &str) -> RedactedString {
    let mut value = input.to_string();
    let mut replacements = 0usize;

    let assignment_replacements = assignment_secret_regex().captures_iter(&value).count();
    if assignment_replacements > 0 {
        value = assignment_secret_regex()
            .replace_all(&value, "${key}${after_key}[REDACTED]")
            .to_string();
        replacements += assignment_replacements;
    }

    let token_replacements = token_secret_regex().find_iter(&value).count();
    if token_replacements > 0 {
        value = token_secret_regex()
            .replace_all(&value, "[REDACTED]")
            .to_string();
        replacements += token_replacements;
    }

    RedactedString {
        value,
        replacements,
    }
}

/// Regex that matches `key = value` / `key: "value"` style secret assignments.
pub fn assignment_secret_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?ix)
            (?P<key>\b(?:access[_-]?token|refresh[_-]?token|api[_-]?key|apikey|authorization|cookie|password|secret|session[_-]?id|token)\b)
            (?P<after_key>"?\s*[:=]\s*)
            (?P<value>"[^"]*"|'[^']*'|[^\s,;&}\]]+)
            "#,
        )
        .expect("valid assignment secret regex")
    })
}

/// Regex that matches bare token shapes (OpenAI, GitHub, Slack, generic
/// `bearer ...`).
pub fn token_secret_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?ix)
            \b(
                sk-(?:proj-)?[A-Za-z0-9_-]{8,}
                | ghp_[A-Za-z0-9_]{8,}
                | github_pat_[A-Za-z0-9_]{8,}
                | xox[baprs]-[A-Za-z0-9-]{8,}
                | bearer\s+[A-Za-z0-9._~+/=-]{8,}
            )\b
            "#,
        )
        .expect("valid token secret regex")
    })
}

#[cfg(test)]
mod tests {
    use super::redact_text;

    #[test]
    fn redacts_assignment_style_secret() {
        let redacted = redact_text("OPENAI_API_KEY=sk-proj-supersecret");
        assert!(redacted.value.contains("[REDACTED]"));
        assert!(!redacted.value.contains("sk-proj-supersecret"));
        assert!(redacted.replacements >= 1);
    }

    #[test]
    fn redacts_bare_token_shapes() {
        let redacted = redact_text("ghp_abcdefghijklmnop bearer abcdefghijklmnop");
        assert!(!redacted.value.contains("ghp_abcdefghijklmnop"));
        assert!(!redacted.value.contains("bearer abcdefghijklmnop"));
        assert_eq!(redacted.replacements, 2);
    }

    #[test]
    fn returns_input_unchanged_when_no_secret_present() {
        let input = "no secrets here";
        let redacted = redact_text(input);
        assert_eq!(redacted.value, input);
        assert_eq!(redacted.replacements, 0);
    }

    #[test]
    fn counts_combined_assignment_and_token_replacements() {
        let redacted = redact_text("token: abc123 ghp_abcdefghijklmnop");
        assert!(redacted.value.contains("[REDACTED]"));
        assert!(!redacted.value.contains("ghp_abcdefghijklmnop"));
        assert!(redacted.replacements >= 2);
    }
}

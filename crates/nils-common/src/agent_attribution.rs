//! Shared agent self-attribution scan for provider-bound text and commit
//! messages.
//!
//! Coding agents default to stamping their own identity onto the artifacts they
//! produce: a generator marker line in a PR body, a co-author trailer on a
//! commit. This module owns the single definition of what that attribution
//! looks like so every egress layer agrees — `semantic-commit` blocks it on the
//! commit path (see its `claude-coauthor-trailer` / `claude-generated-marker`
//! blocked-message rules) and `forge-cli` blocks it on the provider path
//! (Rule 17). Enforcement lives in the CLIs rather than in an agent-harness
//! hook: a harness hook is per-runtime and absent from any runtime that has not
//! declared it, so the CLI is the layer that holds no matter which runtime, or
//! which harness, is driving the command.
//!
//! The scan is per line and, for markdown payloads, code-segment aware:
//! [`scan_agent_attribution`] strips fenced blocks and inline code spans first,
//! so documenting the rule (`` `Co-Authored-By: Claude ...` ``) is allowed while
//! a bare attribution line is not. Callers that scan structured, non-markdown
//! input use the per-line predicates directly and get no such exemption.

use std::fmt;

use crate::markdown::strip_code_segments;

/// Case-insensitive needles that identify a generator marker. The URL forms are
/// the stable part of the default marker; the prose form catches a marker whose
/// link was stripped.
const GENERATOR_MARKER_NEEDLES: &[&str] = &[
    "generated with claude code",
    "claude.com/claude-code",
    "claude.ai/code",
];

/// Trailer token whose value is checked for agent attribution.
const COAUTHOR_TRAILER_TOKEN: &str = "Co-Authored-By";

/// Blocked co-author value forms: a value whose first word is the model family,
/// or any value carrying the vendor's no-reply address.
const COAUTHOR_BLOCKED_VALUE_WORD: &str = "Claude";
const COAUTHOR_BLOCKED_VALUE_NEEDLE: &str = "noreply@anthropic.com";

/// Cap on enumerated hits in the error `detail`, mirroring
/// [`crate::provider_payload::LOCAL_PATH_MAX_HITS`] so a pathological body
/// cannot produce an unbounded message.
pub const AGENT_ATTRIBUTION_MAX_HITS: usize = 20;

/// Env var that disables the scan after a verified false positive. Deliberately
/// distinct from `FORGE_CLI_ALLOW_LOCAL_PATH` so bypassing one payload rule
/// never silently disables the other.
pub const ALLOW_AGENT_ATTRIBUTION_ENV: &str = "FORGE_CLI_ALLOW_AGENT_ATTRIBUTION";

/// Stable machine-readable error kind for attribution failures.
pub const AGENT_ATTRIBUTION_ERROR_KIND: &str = "agent_attribution_present";

/// Which attribution form a line carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAttributionMarker {
    /// A generator advertisement line, e.g. a `Generated with ...` footer.
    Generator,
    /// A co-author trailer attributing the work to the agent or its vendor.
    Coauthor,
}

impl AgentAttributionMarker {
    /// Short human label naming the offending form. Never echoes the matched
    /// text, so diagnostics cannot reproduce the marker they reject.
    pub fn label(self) -> &'static str {
        match self {
            Self::Generator => "agent generator marker",
            Self::Coauthor => "agent co-author trailer",
        }
    }

    /// Operator-facing remedy for this form.
    pub fn fix(self) -> &'static str {
        match self {
            Self::Generator => "delete the generator marker line",
            Self::Coauthor => "delete the co-author trailer line",
        }
    }
}

impl fmt::Display for AgentAttributionMarker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One attribution marker found in scanned text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAttributionHit {
    /// 1-based line number within the scanned text.
    pub line: usize,
    /// The attribution form found on that line.
    pub marker: AgentAttributionMarker,
}

/// Attribution violation report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAttributionError {
    source: String,
    hits: Vec<AgentAttributionHit>,
}

impl AgentAttributionError {
    pub fn new(source: impl Into<String>, hits: Vec<AgentAttributionHit>) -> Self {
        Self {
            source: source.into(),
            hits,
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn hits(&self) -> &[AgentAttributionHit] {
        &self.hits
    }

    pub fn message(&self) -> String {
        format!(
            "{source} contains {n} agent self-attribution marker(s); \
             deliver the work without agent attribution",
            source = self.source,
            n = self.hits.len()
        )
    }

    pub fn detail(&self) -> String {
        render_agent_attribution_detail(&self.hits)
    }

    pub fn full_message(&self) -> String {
        format!("{}.\n{}", self.message(), self.detail())
    }
}

impl fmt::Display for AgentAttributionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.full_message())
    }
}

impl std::error::Error for AgentAttributionError {}

/// True when `line` advertises the generating agent. Case-insensitive; the
/// needles are ASCII so byte-wise lowercasing is sufficient.
pub fn line_has_generator_marker(line: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    GENERATOR_MARKER_NEEDLES
        .iter()
        .any(|needle| lowered.contains(needle))
}

/// True when `line` is a co-author trailer attributing the work to the agent or
/// its vendor. Accepts both `Token: value` and `Token=value` because
/// `semantic-commit --trailer` accepts either separator.
pub fn line_is_blocked_coauthor_trailer(line: &str) -> bool {
    let Some((token, value)) = split_trailer(line.trim()) else {
        return false;
    };
    if !token.eq_ignore_ascii_case(COAUTHOR_TRAILER_TOKEN) {
        return false;
    }
    starts_with_ascii_word_ignore_case(value, COAUTHOR_BLOCKED_VALUE_WORD)
        || value
            .to_ascii_lowercase()
            .contains(COAUTHOR_BLOCKED_VALUE_NEEDLE)
}

/// Classify a single line, checking the co-author form first so a trailer that
/// also carries a marker URL is reported as the trailer it is.
pub fn classify_line(line: &str) -> Option<AgentAttributionMarker> {
    if line_is_blocked_coauthor_trailer(line) {
        Some(AgentAttributionMarker::Coauthor)
    } else if line_has_generator_marker(line) {
        Some(AgentAttributionMarker::Generator)
    } else {
        None
    }
}

/// Scan a markdown payload for attribution markers. Fenced code blocks and
/// inline code spans are removed first, so text *about* the rule is allowed
/// while an actual attribution line is not. Stripping preserves line counts, so
/// reported line numbers still index the original text. Pure: no env gate and
/// no I/O, so every detection branch is unit-testable.
pub fn scan_agent_attribution(text: &str) -> Vec<AgentAttributionHit> {
    scan_lines(&strip_code_segments(text))
}

/// Scan text verbatim, with no code-segment exemption. For structured,
/// non-markdown input where a backticked value is still a real value.
pub fn scan_agent_attribution_verbatim(text: &str) -> Vec<AgentAttributionHit> {
    scan_lines(text)
}

fn scan_lines(text: &str) -> Vec<AgentAttributionHit> {
    text.lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            classify_line(line).map(|marker| AgentAttributionHit {
                line: idx + 1,
                marker,
            })
        })
        .collect()
}

/// Validate a markdown payload, honoring [`ALLOW_AGENT_ATTRIBUTION_ENV`].
/// `source` names the offending input (`title` / `body` / `comment`).
pub fn validate_no_agent_attribution(
    text: &str,
    source: &str,
) -> Result<(), AgentAttributionError> {
    if agent_attribution_scan_disabled() {
        return Ok(());
    }
    let hits = scan_agent_attribution(text);
    if hits.is_empty() {
        Ok(())
    } else {
        Err(AgentAttributionError::new(source, hits))
    }
}

pub fn agent_attribution_scan_disabled() -> bool {
    matches!(std::env::var(ALLOW_AGENT_ATTRIBUTION_ENV), Ok(v) if v == "1")
}

pub fn render_agent_attribution_detail(hits: &[AgentAttributionHit]) -> String {
    let mut lines: Vec<String> = hits
        .iter()
        .take(AGENT_ATTRIBUTION_MAX_HITS)
        .map(|hit| {
            format!(
                "line {line}: {label} — {fix}",
                line = hit.line,
                label = hit.marker.label(),
                fix = hit.marker.fix(),
            )
        })
        .collect();
    let extra = hits.len().saturating_sub(AGENT_ATTRIBUTION_MAX_HITS);
    if extra > 0 {
        lines.push(format!("... {extra} more marker(s) omitted"));
    }
    lines.push(format!(
        "set {ALLOW_AGENT_ATTRIBUTION_ENV}=1 to bypass after verifying a false positive"
    ));
    lines.join("\n")
}

/// Split `Token: value` / `Token=value` on whichever separator comes first.
/// Mirrors `semantic-commit`'s trailer split so both layers accept the same
/// trailer shapes.
fn split_trailer(line: &str) -> Option<(&str, &str)> {
    let colon = line.find(':');
    let equals = line.find('=');
    let idx = match (colon, equals) {
        (Some(colon), Some(equals)) => colon.min(equals),
        (Some(colon), None) => colon,
        (None, Some(equals)) => equals,
        (None, None) => return None,
    };
    let (token, rest) = line.split_at(idx);
    Some((token.trim(), rest[1..].trim()))
}

/// True when `value` starts with `expected_word` as a whole ASCII word, so
/// `Claudette` does not match `Claude`.
fn starts_with_ascii_word_ignore_case(value: &str, expected_word: &str) -> bool {
    if expected_word.is_empty() {
        return false;
    }
    let value = value.trim_start();
    let Some(prefix) = value.get(..expected_word.len()) else {
        return false;
    };
    if !prefix.eq_ignore_ascii_case(expected_word) {
        return false;
    }
    value[expected_word.len()..]
        .chars()
        .next()
        .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn generator_marker_matches_default_footer() {
        assert!(line_has_generator_marker(
            "🤖 Generated with [Claude Code](https://claude.com/claude-code)"
        ));
    }

    #[test]
    fn generator_marker_matches_legacy_link_and_prose_forms() {
        assert!(line_has_generator_marker("see https://claude.ai/code"));
        assert!(line_has_generator_marker("Generated With Claude Code"));
    }

    #[test]
    fn generator_marker_ignores_unrelated_mentions() {
        assert!(!line_has_generator_marker(
            "reviewed by a coding agent before merge"
        ));
        assert!(!line_has_generator_marker("claude-code-guide agent"));
    }

    #[test]
    fn coauthor_trailer_matches_colon_and_equals_separators() {
        assert!(line_is_blocked_coauthor_trailer(
            "Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
        ));
        assert!(line_is_blocked_coauthor_trailer("co-authored-by=Claude"));
    }

    #[test]
    fn coauthor_trailer_matches_vendor_noreply_for_any_name() {
        assert!(line_is_blocked_coauthor_trailer(
            "Co-Authored-By: Some Agent <noreply@anthropic.com>"
        ));
    }

    #[test]
    fn coauthor_trailer_allows_human_and_longer_word_values() {
        assert!(!line_is_blocked_coauthor_trailer(
            "Co-Authored-By: Jane Dev <jane@example.com>"
        ));
        assert!(!line_is_blocked_coauthor_trailer(
            "Co-Authored-By: Claudette Dev <claudette@example.com>"
        ));
        assert!(!line_is_blocked_coauthor_trailer("Reviewed-By: Claude"));
        assert!(!line_is_blocked_coauthor_trailer("no separator here"));
    }

    #[test]
    fn classify_line_prefers_the_coauthor_form() {
        assert_eq!(
            classify_line("Co-Authored-By: Claude <noreply@anthropic.com>"),
            Some(AgentAttributionMarker::Coauthor)
        );
        assert_eq!(
            classify_line("🤖 Generated with [Claude Code](https://claude.com/claude-code)"),
            Some(AgentAttributionMarker::Generator)
        );
        assert_eq!(classify_line("## Summary"), None);
    }

    #[test]
    fn scan_reports_original_line_numbers() {
        let body = "## Summary\n\nfix the thing\n\n🤖 Generated with Claude Code\n";
        assert_eq!(
            scan_agent_attribution(body),
            vec![AgentAttributionHit {
                line: 5,
                marker: AgentAttributionMarker::Generator,
            }]
        );
    }

    #[test]
    fn scan_exempts_fenced_and_inline_code() {
        let body = "The rule blocks `Co-Authored-By: Claude ...` trailers.\n\
                    \n\
                    ```text\n\
                    🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\
                    ```\n";
        assert_eq!(scan_agent_attribution(body), Vec::new());
    }

    #[test]
    fn verbatim_scan_has_no_code_exemption() {
        let body = "```text\nCo-Authored-By: Claude\n```\n";
        assert_eq!(scan_agent_attribution(body), Vec::new());
        assert_eq!(
            scan_agent_attribution_verbatim(body),
            vec![AgentAttributionHit {
                line: 2,
                marker: AgentAttributionMarker::Coauthor,
            }]
        );
    }

    #[test]
    fn validate_names_the_source_and_enumerates_hits() {
        let err = validate_no_agent_attribution(
            "## Summary\nCo-Authored-By: Claude\n🤖 Generated with Claude Code\n",
            "body",
        )
        .expect_err("attribution present");
        assert_eq!(err.source(), "body");
        assert_eq!(err.hits().len(), 2);
        assert!(err.message().contains("body contains 2"), "{err}");
        assert!(
            err.detail().contains("line 2: agent co-author trailer"),
            "{err}"
        );
        assert!(
            err.detail().contains("line 3: agent generator marker"),
            "{err}"
        );
        assert!(err.detail().contains(ALLOW_AGENT_ATTRIBUTION_ENV), "{err}");
    }

    #[test]
    fn validate_accepts_clean_text() {
        validate_no_agent_attribution("## Summary\n\nfix the thing\n", "body").expect("clean");
    }

    #[test]
    fn detail_caps_enumerated_hits() {
        let hits: Vec<AgentAttributionHit> = (1..=AGENT_ATTRIBUTION_MAX_HITS + 3)
            .map(|line| AgentAttributionHit {
                line,
                marker: AgentAttributionMarker::Generator,
            })
            .collect();
        let detail = render_agent_attribution_detail(&hits);
        assert!(detail.contains("... 3 more marker(s) omitted"), "{detail}");
        assert!(
            !detail.contains(&format!("line {}:", AGENT_ATTRIBUTION_MAX_HITS + 1)),
            "{detail}"
        );
    }
}

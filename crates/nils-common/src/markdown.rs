use std::error::Error;
use std::fmt;

const LITERAL_ESCAPED_CONTROLS: [&str; 3] = [r"\n", r"\r", r"\t"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownPayloadViolation {
    pub sequence: &'static str,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownPayloadError {
    violations: Vec<MarkdownPayloadViolation>,
}

impl MarkdownPayloadError {
    pub fn violations(&self) -> &[MarkdownPayloadViolation] {
        &self.violations
    }
}

impl fmt::Display for MarkdownPayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let details = self
            .violations
            .iter()
            .map(|entry| format!("{} ({})", entry.sequence, entry.count))
            .collect::<Vec<_>>()
            .join(", ");
        write!(
            f,
            "markdown payload contains literal escaped-control artifacts: {details}"
        )
    }
}

impl Error for MarkdownPayloadError {}

pub fn markdown_payload_violations(markdown: &str) -> Vec<MarkdownPayloadViolation> {
    // Literal escaped controls (`\n`, `\r`, `\t`) are legitimate inside code —
    // a shell example like `printf 'a\nb'` is not corruption — so only flag them
    // when they appear in prose / structural markdown. Scan with fenced code
    // blocks and inline code spans removed.
    let scannable = strip_code_segments(markdown);
    let mut violations = Vec::new();

    for sequence in LITERAL_ESCAPED_CONTROLS {
        let count = scannable.match_indices(sequence).count();
        if count > 0 {
            violations.push(MarkdownPayloadViolation { sequence, count });
        }
    }

    violations
}

/// Return `markdown` with fenced code blocks and inline code spans removed so the
/// escaped-control guard only inspects prose / structure. Removed spans are
/// replaced with a space (and skipped fenced lines with a newline) so that
/// neighbouring characters cannot glue into a literal `\n`-style sequence that
/// was not present in the source.
fn strip_code_segments(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut open_fence: Option<(char, usize)> = None;

    for line in markdown.split_inclusive('\n') {
        if let Some((fence_char, fence_len)) = fence_marker(line) {
            match open_fence {
                None => {
                    // Opening fence — start a code block; drop the fence line.
                    open_fence = Some((fence_char, fence_len));
                    out.push('\n');
                    continue;
                }
                Some((open_char, open_len)) if fence_char == open_char && fence_len >= open_len => {
                    // Closing fence — end the block; drop the fence line.
                    open_fence = None;
                    out.push('\n');
                    continue;
                }
                // A fence-looking line of a different kind is block content.
                Some(_) => {
                    out.push('\n');
                    continue;
                }
            }
        }
        if open_fence.is_some() {
            // Inside a fenced block — skip the content line.
            out.push('\n');
            continue;
        }
        out.push_str(&strip_inline_code(line));
    }

    out
}

/// If `line` is a fenced-code delimiter (its first non-whitespace content is a
/// run of three or more backticks or tildes), return the fence character and the
/// run length; otherwise `None`.
fn fence_marker(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let fence_char = trimmed.chars().next()?;
    if fence_char != '`' && fence_char != '~' {
        return None;
    }
    let run = trimmed.chars().take_while(|&c| c == fence_char).count();
    if run >= 3 {
        Some((fence_char, run))
    } else {
        None
    }
}

/// Remove inline code spans (backtick-delimited) from a single line, leaving the
/// surrounding text. An unterminated backtick run is treated as plain text.
fn strip_inline_code(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;

    while i < chars.len() {
        if chars[i] != '`' {
            out.push(chars[i]);
            i += 1;
            continue;
        }

        // Measure the opening backtick run.
        let mut run = 0;
        while i < chars.len() && chars[i] == '`' {
            run += 1;
            i += 1;
        }

        // Find a closing run of exactly the same length.
        let mut j = i;
        let mut closed = false;
        while j < chars.len() {
            if chars[j] == '`' {
                let mut close_run = 0;
                while j < chars.len() && chars[j] == '`' {
                    close_run += 1;
                    j += 1;
                }
                if close_run == run {
                    // [opening run .. closing run] is an inline code span; drop
                    // it, leaving a space so neighbours do not glue together.
                    out.push(' ');
                    i = j;
                    closed = true;
                    break;
                }
            } else {
                j += 1;
            }
        }

        if !closed {
            // No matching close — the run is literal text.
            for _ in 0..run {
                out.push('`');
            }
        }
    }

    out
}

pub fn validate_markdown_payload(markdown: &str) -> Result<(), MarkdownPayloadError> {
    let violations = markdown_payload_violations(markdown);
    if violations.is_empty() {
        Ok(())
    } else {
        Err(MarkdownPayloadError { violations })
    }
}

pub fn canonicalize_table_cell(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_line_break_run = false;

    for ch in value.chars() {
        match ch {
            '\n' | '\r' => {
                if !in_line_break_run {
                    out.push(' ');
                    in_line_break_run = true;
                }
            }
            '|' => {
                out.push('/');
                in_line_break_run = false;
            }
            _ => {
                out.push(ch);
                in_line_break_run = false;
            }
        }
    }

    out
}

fn sort_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                let v = map.get(k).expect("key exists");
                out.insert(k.clone(), sort_json(v));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(sort_json).collect())
        }
        other => other.clone(),
    }
}

/// Format JSON similar to `jq -S .` (stable key order, pretty printed).
pub fn format_json_pretty_sorted(value: &serde_json::Value) -> Result<String, serde_json::Error> {
    let sorted = sort_json(value);
    serde_json::to_string_pretty(&sorted)
}

pub fn heading(level: u8, text: &str) -> String {
    let level = level.clamp(1, 6);
    format!("{} {}\n", "#".repeat(level.into()), text.trim())
}

pub fn code_block(lang: &str, body: &str) -> String {
    let mut out = String::new();
    out.push_str("```");
    out.push_str(lang.trim());
    out.push('\n');
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("```\n");
    out
}

#[cfg(test)]
mod tests {
    use super::{
        canonicalize_table_cell, code_block, format_json_pretty_sorted, heading,
        markdown_payload_violations, validate_markdown_payload,
    };

    #[test]
    fn markdown_payload_validator_accepts_real_control_chars() {
        let payload = "line one\nline two\tvalue\r\n";
        let result = validate_markdown_payload(payload);
        assert!(
            result.is_ok(),
            "unexpected markdown payload error: {result:?}"
        );
    }

    #[test]
    fn markdown_payload_validator_rejects_literal_escaped_controls() {
        let payload = r"line one\nline two\rline three\tvalue";
        let err = validate_markdown_payload(payload).expect_err("expected markdown payload error");

        assert_eq!(err.violations().len(), 3);
        assert!(
            err.to_string().contains(r"\n"),
            "expected escaped-newline mention in {:?}",
            err
        );
        assert!(
            err.to_string().contains(r"\r"),
            "expected escaped-return mention in {:?}",
            err
        );
        assert!(
            err.to_string().contains(r"\t"),
            "expected escaped-tab mention in {:?}",
            err
        );
    }

    #[test]
    fn markdown_payload_violations_reports_counts_per_sequence() {
        let payload = r"one\n two\n three\t";
        let violations = markdown_payload_violations(payload);

        assert_eq!(violations.len(), 2);
        assert_eq!(violations[0].sequence, r"\n");
        assert_eq!(violations[0].count, 2);
        assert_eq!(violations[1].sequence, r"\t");
        assert_eq!(violations[1].count, 1);
    }

    #[test]
    fn markdown_payload_validator_ignores_escaped_controls_in_fenced_code() {
        let payload = "Prose before.\n\n```sh\nprintf 'a\\nb'\n```\n\nProse after.\n";
        assert!(
            validate_markdown_payload(payload).is_ok(),
            "escaped controls inside a fenced code block must not be flagged"
        );
    }

    #[test]
    fn markdown_payload_validator_ignores_escaped_controls_in_inline_code() {
        let payload = r"Run `printf 'a\nb'` to print two lines.";
        assert!(
            validate_markdown_payload(payload).is_ok(),
            "escaped controls inside an inline code span must not be flagged"
        );
    }

    #[test]
    fn markdown_payload_validator_still_flags_escaped_controls_in_prose() {
        // A real escaped newline in prose (outside code) is still corruption.
        let violations = markdown_payload_violations(r"Status: done.\nNext: ship it.");
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].sequence, r"\n");
        assert_eq!(violations[0].count, 1);
    }

    #[test]
    fn markdown_payload_validator_flags_prose_but_not_code_in_mixed_payload() {
        // The prose `\n` is flagged once; the occurrences inside the fenced block
        // and the inline span are ignored.
        let payload = "Bad prose: a\\nb\n\n```\nprintf 'x\\ny'\n```\n\nUse `echo 'p\\nq'` here.\n";
        let violations = markdown_payload_violations(payload);
        assert_eq!(
            violations.len(),
            1,
            "only the prose occurrence counts: {violations:?}"
        );
        assert_eq!(violations[0].sequence, r"\n");
        assert_eq!(violations[0].count, 1);
    }

    #[test]
    fn canonicalize_table_cell_normalizes_markdown_unsafe_chars() {
        let value = "A|B\r\nC\nD\rE";
        assert_eq!(canonicalize_table_cell(value), "A/B C D E");
    }

    #[test]
    fn canonicalize_table_cell_is_idempotent() {
        let first = canonicalize_table_cell("x|y\r\nz");
        let second = canonicalize_table_cell(&first);
        assert_eq!(first, second);
    }

    #[test]
    fn markdown_code_block_is_newline_stable() {
        assert_eq!(code_block("json", "{ }"), "```json\n{ }\n```\n");
        assert_eq!(code_block("json", "{ }\n"), "```json\n{ }\n```\n");
    }

    #[test]
    fn markdown_heading_trims_and_clamps_level() {
        assert_eq!(heading(1, " Title "), "# Title\n");
        assert_eq!(heading(9, "Title"), "###### Title\n");
    }

    #[test]
    fn json_format_sorts_keys_recursively() {
        let v = serde_json::json!({"b": 1, "a": {"d": 4, "c": 3}});
        let s = format_json_pretty_sorted(&v).expect("sorted json");
        assert_eq!(
            s,
            "{\n  \"a\": {\n    \"c\": 3,\n    \"d\": 4\n  },\n  \"b\": 1\n}"
        );
    }
}

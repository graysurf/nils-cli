//! Workspace-wide Markdown helper bridge.
//!
//! `nils-markdown` keeps zero implementation of these helpers: the
//! canonical source is [`nils_common::markdown`]. Re-exporting them
//! at the `nils_markdown::helpers` path means consumer crates can
//! pull the entire Markdown surface (template engine + filter +
//! helpers) through one import in their `.tera`-aware code paths.
//!
//! Decision 6 in the source document pins
//! `nils_common::markdown` as the lowest layer; new helpers land
//! there first and surface here via re-export, never via
//! duplication.

pub use nils_common::markdown::{
    MarkdownPayloadError, MarkdownPayloadViolation, canonicalize_table_cell, code_block,
    format_json_pretty_sorted, heading, markdown_payload_violations, validate_markdown_payload,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_matches_nils_common() {
        let ours = heading(2, "Hello");
        let theirs = nils_common::markdown::heading(2, "Hello");
        assert_eq!(ours, theirs);
        assert_eq!(ours, "## Hello\n");
    }

    #[test]
    fn code_block_matches_nils_common() {
        let body = "let x = 1;";
        let ours = code_block("rust", body);
        let theirs = nils_common::markdown::code_block("rust", body);
        assert_eq!(ours, theirs);
        assert!(ours.starts_with("```rust"));
        assert!(ours.ends_with("```\n"));
    }

    #[test]
    fn canonicalize_table_cell_matches_nils_common() {
        let input = "a|b\nc";
        let ours = canonicalize_table_cell(input);
        let theirs = nils_common::markdown::canonicalize_table_cell(input);
        assert_eq!(ours, theirs);
        assert_eq!(ours, "a/b c");
    }

    #[test]
    fn format_json_pretty_sorted_matches_nils_common() {
        let value = serde_json::json!({"b": 1, "a": 2});
        let ours = format_json_pretty_sorted(&value).unwrap();
        let theirs = nils_common::markdown::format_json_pretty_sorted(&value).unwrap();
        assert_eq!(ours, theirs);
        assert!(ours.contains("\"a\": 2"));
        assert!(ours.contains("\"b\": 1"));
    }

    #[test]
    fn validate_markdown_payload_matches_nils_common() {
        let safe = "hello world\n";
        assert!(validate_markdown_payload(safe).is_ok());
        let violations = markdown_payload_violations(r"literal\n inside");
        assert!(!violations.is_empty());
    }
}

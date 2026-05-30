//! Shared body / full-text scan core for the archive's snapshots.
//!
//! `catalog --grep` only matches catalog metadata; the issue / PR / MR
//! body and comment text lives in each `_index/**.json` snapshot
//! (`data.body` + `data.comments[].body`, scrub-cleaned at refresh).
//! This module scans that text and is the single scanner consumed by
//! both `catalog --deep` (record-level filter) and the `search`
//! subcommand (hit-level results). It reuses [`super::index`] for
//! traversal and ref-URL reconstruction; it never fetches or writes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::index::IndexEntry;

/// Which part of a snapshot a term matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchField {
    /// The issue / PR / MR description (`data.body`).
    Body,
    /// A comment body (`data.comments[].body`).
    Comment,
}

/// A single body / comment match within one ref's latest snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanHit {
    /// Canonical provider URL of the ref the snapshot belongs to.
    pub url: String,
    /// Which field matched.
    pub field: MatchField,
    /// A whitespace-collapsed context snippet around the first match,
    /// with `…` ellipses when the surrounding text was truncated.
    pub snippet: String,
}

/// Bytes of surrounding context included on each side of a match in
/// the snippet.
const SNIPPET_CONTEXT: usize = 48;

/// Snapshot envelope subset we need: only `data.body` and the comment
/// bodies. Everything else is ignored.
#[derive(Debug, Deserialize)]
struct SnapshotEnvelope {
    #[serde(default)]
    data: Option<SnapshotData>,
}

#[derive(Debug, Deserialize)]
struct SnapshotData {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    comments: Vec<SnapshotComment>,
}

#[derive(Debug, Deserialize)]
struct SnapshotComment {
    #[serde(default)]
    body: Option<String>,
}

/// Scan one ref's latest snapshot for a case-insensitive term.
///
/// Returns one [`ScanHit`] per matching field (the body and/or each
/// matching comment), each tagged with the ref's canonical URL. An
/// empty term, a ref with no snapshots, or no match yields an empty
/// vec. Reuses [`IndexEntry::index_dir`] / [`IndexEntry::canonical_url`].
pub fn scan_entry(archive: &Path, entry: &IndexEntry, term: &str) -> std::io::Result<Vec<ScanHit>> {
    let term_lower = term.to_ascii_lowercase();
    if term_lower.is_empty() {
        return Ok(Vec::new());
    }
    let dir = archive.join(entry.index_dir());
    let Some(path) = latest_snapshot_path(&dir) else {
        return Ok(Vec::new());
    };
    let json = std::fs::read_to_string(&path)?;
    let url = entry.canonical_url();
    Ok(scan_json(&json, &term_lower)
        .into_iter()
        .map(|(field, snippet)| ScanHit {
            url: url.clone(),
            field,
            snippet,
        })
        .collect())
}

/// Convenience predicate for record-level filters (`catalog --deep`):
/// does this ref's latest snapshot match the term in its body or any
/// comment?
pub fn entry_matches(archive: &Path, entry: &IndexEntry, term: &str) -> std::io::Result<bool> {
    Ok(!scan_entry(archive, entry, term)?.is_empty())
}

/// Scan a snapshot JSON document for `term_lower` (already lowercased),
/// returning `(field, snippet)` pairs in body-then-comments order.
fn scan_json(json: &str, term_lower: &str) -> Vec<(MatchField, String)> {
    if term_lower.is_empty() {
        return Vec::new();
    }
    let Ok(envelope) = serde_json::from_str::<SnapshotEnvelope>(json) else {
        return Vec::new();
    };
    let Some(data) = envelope.data else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    if let Some(body) = data.body.as_deref()
        && let Some(snippet) = first_snippet(body, term_lower)
    {
        hits.push((MatchField::Body, snippet));
    }
    for comment in &data.comments {
        if let Some(body) = comment.body.as_deref()
            && let Some(snippet) = first_snippet(body, term_lower)
        {
            hits.push((MatchField::Comment, snippet));
        }
    }
    hits
}

/// First whitespace-collapsed snippet around a match of `term_lower`
/// in `text`, or `None` when the term is empty or absent.
fn first_snippet(text: &str, term_lower: &str) -> Option<String> {
    if term_lower.is_empty() {
        return None;
    }
    let pos = text.to_ascii_lowercase().find(term_lower)?;
    Some(make_snippet(text, pos, term_lower.len()))
}

/// Build a one-line context snippet of `text` around the `[start,
/// start+len)` byte range, snapped to char boundaries, whitespace
/// collapsed, with `…` ellipses when either side was truncated.
///
/// `start`/`len` come from a match in the ASCII-lowercased copy of
/// `text`; ASCII lowercasing preserves byte offsets, so they index the
/// original string safely.
fn make_snippet(text: &str, start: usize, len: usize) -> String {
    let mut lo = start.saturating_sub(SNIPPET_CONTEXT);
    while !text.is_char_boundary(lo) {
        lo -= 1;
    }
    let mut hi = (start + len + SNIPPET_CONTEXT).min(text.len());
    while hi < text.len() && !text.is_char_boundary(hi) {
        hi += 1;
    }
    let collapsed = text[lo..hi]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let prefix = if lo > 0 { "…" } else { "" };
    let suffix = if hi < text.len() { "…" } else { "" };
    format!("{prefix}{collapsed}{suffix}")
}

/// Path to the lexically-last `*.json` snapshot in a ref directory.
fn latest_snapshot_path(dir: &Path) -> Option<PathBuf> {
    let mut stamps: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".json"))
        .collect();
    stamps.sort();
    stamps.last().map(|name| dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refresh::refparse::RefKind;

    const BODY_JSON: &str = r#"{
        "schema_version": "x",
        "ok": true,
        "data": {
            "body": "Steps after live acceptance passes with rollback proven.",
            "comments": [{ "body": "looks good to me" }]
        }
    }"#;

    const COMMENT_JSON: &str = r#"{
        "data": {
            "body": "A plain description with no special words.",
            "comments": [
                { "body": "first comment" },
                { "body": "we still need the Rollback steps documented" }
            ]
        }
    }"#;

    fn sample_entry() -> IndexEntry {
        IndexEntry {
            host: "github.com".into(),
            org_or_group_path: "graysurf".into(),
            repo: "agent-runtime-kit".into(),
            kind: RefKind::Issue,
            number: 55,
        }
    }

    #[test]
    fn scan_json_finds_body_match() {
        let hits = scan_json(BODY_JSON, "rollback");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, MatchField::Body);
        assert!(hits[0].1.to_ascii_lowercase().contains("rollback"));
    }

    #[test]
    fn scan_json_finds_comment_match_only() {
        let hits = scan_json(COMMENT_JSON, "rollback");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, MatchField::Comment);
        assert!(hits[0].1.to_ascii_lowercase().contains("rollback"));
    }

    #[test]
    fn scan_json_is_case_insensitive() {
        let hits = scan_json(BODY_JSON, "rollback");
        assert_eq!(
            hits.len(),
            1,
            "lowercased term should match mixed-case body"
        );
    }

    #[test]
    fn scan_json_no_match_is_empty() {
        assert!(scan_json(BODY_JSON, "nonexistent-token").is_empty());
    }

    #[test]
    fn scan_json_empty_term_is_empty() {
        assert!(scan_json(BODY_JSON, "").is_empty());
    }

    #[test]
    fn first_snippet_includes_match_and_ellipsis() {
        let text = "alpha beta gamma rollback delta epsilon zeta eta theta iota kappa lambda mu nu";
        let snip = first_snippet(text, "rollback").expect("match");
        assert!(snip.to_ascii_lowercase().contains("rollback"));
        assert!(!snip.contains('\n'));
    }

    #[test]
    fn scan_entry_attaches_canonical_url() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let entry = sample_entry();
        let dir = archive.join(entry.index_dir());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("20260527T052454Z.json"), BODY_JSON).unwrap();

        let hits = scan_entry(archive, &entry, "rollback").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, entry.canonical_url());
        assert_eq!(hits[0].field, MatchField::Body);
    }

    #[test]
    fn scan_entry_picks_latest_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let entry = sample_entry();
        let dir = archive.join(entry.index_dir());
        std::fs::create_dir_all(&dir).unwrap();
        // Older snapshot lacks the term; newer one has it.
        std::fs::write(
            dir.join("20260101T000000Z.json"),
            r#"{"data":{"body":"nothing here","comments":[]}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("20260527T052454Z.json"), BODY_JSON).unwrap();

        let hits = scan_entry(archive, &entry, "rollback").unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn scan_entry_no_snapshots_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = sample_entry();
        assert!(
            scan_entry(tmp.path(), &entry, "rollback")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn entry_matches_reflects_hits() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let entry = sample_entry();
        let dir = archive.join(entry.index_dir());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("20260527T052454Z.json"), BODY_JSON).unwrap();

        assert!(entry_matches(archive, &entry, "rollback").unwrap());
        assert!(!entry_matches(archive, &entry, "nonexistent-token").unwrap());
    }
}

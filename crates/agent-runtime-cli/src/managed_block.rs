//! Managed-block helper for paired install markers.
//!
//! The installer (Plan 04 Sprint 1 Task 1.2) writes small managed blocks
//! into product config files (`~/.codex/config.toml`,
//! `~/.claude/settings.json`). This module owns the paired-marker contract
//! so install / uninstall / restore-backups stay byte-consistent.
//!
//! Contract:
//!
//! - Markers are paired, comment-prefixed, and live on their own lines:
//!   - `<prefix> >>> agent-runtime-kit:<surface> >>>` (open)
//!   - `<prefix> <<< agent-runtime-kit:<surface> <<<` (close)
//!
//!   `<prefix>` is `#` for TOML surfaces and `//` for JSONC surfaces.
//! - Bytes outside the marker pair are preserved verbatim across read /
//!   write / remove. Inside the markers, the helper canonicalises layout
//!   so repeated writes are idempotent.
//! - Writing into a file with no markers refuses unless the caller passes
//!   `force = true`; this keeps the first install opt-in explicit.
//! - Unbalanced markers (one half present, the other missing) refuse with
//!   a typed error naming the surface.
//! - The `body` passed to `write` must not itself contain a line that
//!   matches our open or close marker — that would write a file that
//!   refuses to round-trip. The helper validates and refuses.
//!
//! Caller contract:
//!
//! - `surface` is a trusted identifier. The helper accepts ASCII
//!   alphanumeric / `-` / `_` only and `debug_assert!`s the shape; release
//!   builds trust the caller. Embedding newlines or marker tokens in the
//!   surface name will produce well-formed but surprising markers.
//! - Inputs are assumed to use LF (`\n`) line endings. CRLF files are out
//!   of scope for Plan 04 and may silently no-op on `read`.
//!
//! Source: `agent-runtime-kit/docs/plans/04-installer-doctor-and-bootstrap/
//! 04-installer-doctor-and-bootstrap-plan.md` Sprint 1 Task 1.1.

use thiserror::Error;

/// Comment syntax used by a surface's file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentStyle {
    /// `#` — TOML (Codex `config.toml`).
    Hash,
    /// `//` — JSONC (Claude `settings.json`).
    DoubleSlash,
}

impl CommentStyle {
    fn prefix(self) -> &'static str {
        match self {
            CommentStyle::Hash => "#",
            CommentStyle::DoubleSlash => "//",
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManagedBlockError {
    /// File has no marker pair, and the caller did not opt in via `force`.
    #[error("managed block for surface `{surface}` not present; pass force=true to add it")]
    NotPresent { surface: String },
    /// Marker counts do not balance (one half present, the other missing,
    /// or more than one of either kind).
    #[error(
        "managed block markers for surface `{surface}` are unbalanced: {open} open / {close} close"
    )]
    Unbalanced {
        surface: String,
        open: usize,
        close: usize,
    },
    /// Close marker precedes its paired open marker.
    #[error("managed block close marker for surface `{surface}` precedes its open marker")]
    OutOfOrder { surface: String },
    /// More than one complete block for the same surface in one file.
    #[error("managed block for surface `{surface}` appears multiple times in one file")]
    Duplicate { surface: String },
    /// The provided body contains a line that matches our own open or
    /// close marker — writing it would corrupt the file so the next read
    /// or write fails with `Unbalanced` or `Duplicate`. Refused at the
    /// write boundary so the file on disk stays intact.
    #[error(
        "managed block body for surface `{surface}` contains its own marker line ({which}); refused"
    )]
    BodyContainsMarker {
        surface: String,
        which: &'static str,
    },
}

/// Owns the marker syntax for one (surface, comment-style) pair.
#[derive(Debug, Clone)]
pub struct ManagedBlock {
    surface: String,
    style: CommentStyle,
}

impl ManagedBlock {
    pub fn new(surface: impl Into<String>, style: CommentStyle) -> Self {
        let surface = surface.into();
        debug_assert!(
            is_trusted_surface(&surface),
            "surface name `{surface}` must be ASCII alphanumeric / `-` / `_` and non-empty",
        );
        Self { surface, style }
    }

    pub fn surface(&self) -> &str {
        &self.surface
    }

    pub fn style(&self) -> CommentStyle {
        self.style
    }

    pub fn open_marker(&self) -> String {
        format!(
            "{prefix} >>> agent-runtime-kit:{surface} >>>",
            prefix = self.style.prefix(),
            surface = self.surface,
        )
    }

    pub fn close_marker(&self) -> String {
        format!(
            "{prefix} <<< agent-runtime-kit:{surface} <<<",
            prefix = self.style.prefix(),
            surface = self.surface,
        )
    }

    /// Return the block body if a complete marker pair is present, else
    /// `Ok(None)`. Errors when markers are unbalanced or duplicated.
    pub fn read(&self, text: &str) -> Result<Option<String>, ManagedBlockError> {
        match self.locate(text)? {
            None => Ok(None),
            Some(span) => Ok(Some(text[span.body_start..span.body_end].to_string())),
        }
    }

    /// Write `body` into the managed block. When markers already exist,
    /// only bytes between the markers are replaced. When markers are
    /// missing, the block is appended to the end of the file — but only
    /// when `force` is true.
    pub fn write(&self, text: &str, body: &str, force: bool) -> Result<String, ManagedBlockError> {
        let body = canonicalize_body(body);
        let open = self.open_marker();
        let close = self.close_marker();
        if !line_anchored_matches(&body, &open).is_empty() {
            return Err(ManagedBlockError::BodyContainsMarker {
                surface: self.surface.clone(),
                which: "open",
            });
        }
        if !line_anchored_matches(&body, &close).is_empty() {
            return Err(ManagedBlockError::BodyContainsMarker {
                surface: self.surface.clone(),
                which: "close",
            });
        }
        match self.locate(text)? {
            None => {
                if !force {
                    return Err(ManagedBlockError::NotPresent {
                        surface: self.surface.clone(),
                    });
                }
                Ok(append_block(text, &open, &close, &body))
            }
            Some(span) => Ok(replace_block(text, &span, &open, &close, &body)),
        }
    }

    /// Remove the managed block in full. No-op when no markers are
    /// present. Errors on unbalanced markers.
    pub fn remove(&self, text: &str) -> Result<String, ManagedBlockError> {
        match self.locate(text)? {
            None => Ok(text.to_string()),
            Some(span) => Ok(remove_block(text, &span)),
        }
    }

    fn locate(&self, text: &str) -> Result<Option<BlockSpan>, ManagedBlockError> {
        let open = self.open_marker();
        let close = self.close_marker();

        let open_hits = line_anchored_matches(text, &open);
        let close_hits = line_anchored_matches(text, &close);

        if open_hits.len() != close_hits.len() {
            return Err(ManagedBlockError::Unbalanced {
                surface: self.surface.clone(),
                open: open_hits.len(),
                close: close_hits.len(),
            });
        }
        if open_hits.is_empty() {
            return Ok(None);
        }
        if open_hits.len() > 1 {
            return Err(ManagedBlockError::Duplicate {
                surface: self.surface.clone(),
            });
        }

        let open_start = open_hits[0];
        let close_start = close_hits[0];
        let open_end = open_start + open.len();
        if close_start < open_end {
            return Err(ManagedBlockError::OutOfOrder {
                surface: self.surface.clone(),
            });
        }
        let close_end = close_start + close.len();

        // Trim one `\n` immediately after the open marker and one `\n`
        // immediately before the close marker so the body is the
        // interior lines without their framing terminators. If those
        // newlines are absent the markers are on the same line as body
        // content — unusual but tolerated.
        let body_start = if text.as_bytes().get(open_end) == Some(&b'\n') {
            open_end + 1
        } else {
            open_end
        };
        let body_end = if close_start > 0 && text.as_bytes()[close_start - 1] == b'\n' {
            close_start - 1
        } else {
            close_start
        };
        let body_end = body_end.max(body_start);

        Ok(Some(BlockSpan {
            open_start,
            body_start,
            body_end,
            close_end,
        }))
    }
}

#[derive(Debug, Clone, Copy)]
struct BlockSpan {
    open_start: usize,
    body_start: usize,
    body_end: usize,
    close_end: usize,
}

/// Locate every position in `text` where `needle` appears at the start of
/// a line (column 0). Anchoring matches to line starts prevents false
/// positives from a marker substring quoted inside string literals.
fn line_anchored_matches(text: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let needle_bytes = needle.as_bytes();
    if needle_bytes.is_empty() {
        return out;
    }
    let mut idx = 0usize;
    while idx + needle_bytes.len() <= bytes.len() {
        if bytes[idx..idx + needle_bytes.len()] == *needle_bytes {
            let at_line_start = idx == 0 || bytes[idx - 1] == b'\n';
            let line_end_pos = idx + needle_bytes.len();
            let line_terminates = line_end_pos == bytes.len() || bytes[line_end_pos] == b'\n';
            if at_line_start && line_terminates {
                out.push(idx);
                idx = line_end_pos;
                continue;
            }
        }
        idx += 1;
    }
    out
}

fn canonicalize_body(body: &str) -> String {
    let trimmed = body.trim_end_matches('\n');
    trimmed.to_string()
}

fn is_trusted_surface(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn append_block(text: &str, open: &str, close: &str, body: &str) -> String {
    let mut out = String::with_capacity(text.len() + open.len() + body.len() + close.len() + 4);
    out.push_str(text);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(open);
    out.push('\n');
    if !body.is_empty() {
        out.push_str(body);
        out.push('\n');
    }
    out.push_str(close);
    out.push('\n');
    out
}

fn replace_block(text: &str, span: &BlockSpan, open: &str, close: &str, body: &str) -> String {
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..span.open_start]);
    out.push_str(open);
    out.push('\n');
    if !body.is_empty() {
        out.push_str(body);
        out.push('\n');
    }
    out.push_str(close);
    out.push_str(&text[span.close_end..]);
    out
}

fn remove_block(text: &str, span: &BlockSpan) -> String {
    let mut start = span.open_start;
    let mut end = span.close_end;
    // Absorb the single newline that follows the close marker so we do
    // not leave a stranded blank line behind. Bytes further out are
    // preserved verbatim.
    if text.as_bytes().get(end) == Some(&b'\n') {
        end += 1;
    } else if start > 0 && text.as_bytes()[start - 1] == b'\n' {
        // The block was the last line and had no trailing newline; pull
        // the leading newline in instead so we still close the gap.
        start -= 1;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&text[end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toml_block() -> ManagedBlock {
        ManagedBlock::new("install", CommentStyle::Hash)
    }

    fn json_block() -> ManagedBlock {
        ManagedBlock::new("install", CommentStyle::DoubleSlash)
    }

    #[test]
    fn marker_strings_match_resolved_contract() {
        let b = toml_block();
        assert_eq!(b.open_marker(), "# >>> agent-runtime-kit:install >>>");
        assert_eq!(b.close_marker(), "# <<< agent-runtime-kit:install <<<");
        let j = json_block();
        assert_eq!(j.open_marker(), "// >>> agent-runtime-kit:install >>>");
        assert_eq!(j.close_marker(), "// <<< agent-runtime-kit:install <<<");
    }

    #[test]
    fn read_returns_none_when_no_markers_present() {
        let b = toml_block();
        assert_eq!(b.read("plain config").unwrap(), None);
        assert_eq!(b.read("").unwrap(), None);
    }

    #[test]
    fn read_returns_body_between_paired_markers() {
        let b = toml_block();
        let text = "leading\n# >>> agent-runtime-kit:install >>>\nfoo = 1\nbar = 2\n# <<< agent-runtime-kit:install <<<\ntrailing\n";
        assert_eq!(b.read(text).unwrap(), Some("foo = 1\nbar = 2".to_string()));
    }

    #[test]
    fn write_with_no_markers_requires_force() {
        let b = toml_block();
        let err = b.write("[plain]\n", "tag = 1", false).unwrap_err();
        assert_eq!(
            err,
            ManagedBlockError::NotPresent {
                surface: "install".to_string()
            }
        );
    }

    #[test]
    fn write_with_force_appends_a_fresh_block() {
        let b = toml_block();
        let out = b.write("[plain]\n", "tag = 1", true).unwrap();
        let expected = "[plain]\n# >>> agent-runtime-kit:install >>>\ntag = 1\n# <<< agent-runtime-kit:install <<<\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn write_preserves_bytes_outside_the_markers() {
        let b = toml_block();
        let before = "alpha\n# >>> agent-runtime-kit:install >>>\nold body\n# <<< agent-runtime-kit:install <<<\nbeta\n";
        let after = b.write(before, "new body", false).unwrap();
        // Outside the markers, both halves must be byte-identical.
        let outside_before = "alpha\n";
        let outside_after_tail = "\nbeta\n";
        assert!(after.starts_with(outside_before));
        assert!(after.ends_with(outside_after_tail));
    }

    #[test]
    fn write_then_write_is_idempotent_on_same_body() {
        let b = toml_block();
        let base = "alpha\n# >>> agent-runtime-kit:install >>>\nold\n# <<< agent-runtime-kit:install <<<\nbeta\n";
        let once = b.write(base, "next", false).unwrap();
        let twice = b.write(&once, "next", false).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn write_then_write_force_creates_block_idempotently() {
        let b = toml_block();
        let base = "[plain]\n";
        let once = b.write(base, "tag = 1", true).unwrap();
        let twice = b.write(&once, "tag = 1", true).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn remove_strips_block_and_collapses_trailing_newline() {
        let b = toml_block();
        let before = "alpha\n# >>> agent-runtime-kit:install >>>\nfoo\n# <<< agent-runtime-kit:install <<<\nbeta\n";
        let after = b.remove(before).unwrap();
        assert_eq!(after, "alpha\nbeta\n");
    }

    #[test]
    fn remove_is_a_noop_when_block_absent() {
        let b = toml_block();
        assert_eq!(b.remove("plain config").unwrap(), "plain config");
    }

    #[test]
    fn unbalanced_markers_refuse() {
        let b = toml_block();
        let text = "alpha\n# >>> agent-runtime-kit:install >>>\nfoo\nbeta\n";
        let err = b.read(text).unwrap_err();
        match err {
            ManagedBlockError::Unbalanced { open, close, .. } => {
                assert_eq!((open, close), (1, 0));
            }
            other => panic!("expected Unbalanced, got {other:?}"),
        }
    }

    #[test]
    fn out_of_order_markers_refuse() {
        let b = toml_block();
        let text =
            "# <<< agent-runtime-kit:install <<<\nfoo\n# >>> agent-runtime-kit:install >>>\n";
        let err = b.read(text).unwrap_err();
        assert!(matches!(err, ManagedBlockError::OutOfOrder { .. }));
    }

    #[test]
    fn duplicate_blocks_refuse() {
        let b = toml_block();
        let text = "# >>> agent-runtime-kit:install >>>\na\n# <<< agent-runtime-kit:install <<<\n# >>> agent-runtime-kit:install >>>\nb\n# <<< agent-runtime-kit:install <<<\n";
        let err = b.read(text).unwrap_err();
        assert!(matches!(err, ManagedBlockError::Duplicate { .. }));
    }

    #[test]
    fn markers_inside_string_literals_are_ignored() {
        // Marker substrings that are not at column 0 must not be picked
        // up — line anchoring is what keeps the helper safe to use
        // against settings.json values that may contain similar text.
        let b = json_block();
        let text = "{\n  \"note\": \"// >>> agent-runtime-kit:install >>> inline string\"\n}\n";
        assert_eq!(b.read(text).unwrap(), None);
    }

    #[test]
    fn write_rejects_body_that_contains_open_marker_line_on_append() {
        let b = toml_block();
        let evil = "# >>> agent-runtime-kit:install >>>";
        let err = b.write("[plain]\n", evil, true).unwrap_err();
        match err {
            ManagedBlockError::BodyContainsMarker { surface, which } => {
                assert_eq!(surface, "install");
                assert_eq!(which, "open");
            }
            other => panic!("expected BodyContainsMarker, got {other:?}"),
        }
    }

    #[test]
    fn write_rejects_body_that_contains_close_marker_line_on_replace() {
        let b = toml_block();
        let base = "alpha\n# >>> agent-runtime-kit:install >>>\nold\n# <<< agent-runtime-kit:install <<<\nbeta\n";
        let evil = "innocent = 1\n# <<< agent-runtime-kit:install <<<\nmore = 2";
        let err = b.write(base, evil, false).unwrap_err();
        match err {
            ManagedBlockError::BodyContainsMarker { surface, which } => {
                assert_eq!(surface, "install");
                assert_eq!(which, "close");
            }
            other => panic!("expected BodyContainsMarker, got {other:?}"),
        }
        // File on disk is untouched after a refused write.
        // (The caller controls disk; this assertion just confirms the
        // helper returns without producing a corrupted string.)
    }

    #[test]
    fn write_accepts_body_containing_marker_substring_off_column_zero() {
        // Same fingerprint, but not at the start of a line — must pass.
        let b = toml_block();
        let body = "    # >>> agent-runtime-kit:install >>> nested";
        let out = b.write("[plain]\n", body, true).unwrap();
        assert_eq!(b.read(&out).unwrap().as_deref(), Some(body));
    }

    #[test]
    fn is_trusted_surface_accepts_canonical_identifiers() {
        assert!(is_trusted_surface("install"));
        assert!(is_trusted_surface("link-map"));
        assert!(is_trusted_surface("agent_runtime_v2"));
        assert!(!is_trusted_surface(""));
        assert!(!is_trusted_surface("install\nmore"));
        assert!(!is_trusted_surface("install >>>"));
        assert!(!is_trusted_surface("install:tag"));
    }

    #[test]
    fn round_trip_preserves_outside_bytes_across_seeds() {
        // Property-style sweep: a handful of deterministic seeds exercise
        // body shapes that have historically tripped marker handlers —
        // empty bodies, multi-line bodies, bodies that themselves contain
        // marker-like substrings, and bodies with trailing newlines.
        let b = toml_block();
        let outside_prefix = "alpha\nbeta\n";
        let outside_suffix = "gamma\ndelta\n";
        let bodies = [
            "",
            "x = 1",
            "x = 1\ny = 2\nz = 3",
            "still text\n", // trailing newline canonicalised
            "marker-like text >>> agent-runtime-kit:other", // not column 0
            "k = \"# >>> agent-runtime-kit:install >>>\"", // marker inside a string
        ];
        for body in bodies {
            let base = format!(
                "{outside_prefix}# >>> agent-runtime-kit:install >>>\nseed\n# <<< agent-runtime-kit:install <<<\n{outside_suffix}"
            );
            let once = b.write(&base, body, false).unwrap();
            let twice = b.write(&once, body, false).unwrap();
            assert_eq!(once, twice, "body={body:?}");
            assert!(once.starts_with(outside_prefix), "body={body:?}");
            assert!(once.ends_with(outside_suffix), "body={body:?}");
            let read_back = b.read(&once).unwrap().unwrap();
            assert_eq!(read_back, canonicalize_body(body), "body={body:?}");
        }
    }
}

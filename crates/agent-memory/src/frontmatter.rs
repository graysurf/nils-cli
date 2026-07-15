//! Shared note-frontmatter parsing and rendering for the memory store.
//!
//! Notes are `*.md` files with a leading `---` fenced YAML block. Only the flat
//! scalar fields the store cares about are read; nested values live under a
//! 2-space `metadata:` map. Parsing is intentionally lenient (it tolerates
//! trailing whitespace and quoted values) so hand-authored notes are accepted.

pub(crate) const VALID_TYPES: [&str; 4] = ["user", "feedback", "project", "reference"];

#[derive(Default)]
pub(crate) struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub node_type: Option<String>,
    pub typ: Option<String>,
    pub origin_session_id: Option<String>,
}

/// Parse the leading `---` fenced YAML block. Returns `None` when the file has
/// no frontmatter block at all.
pub(crate) fn parse(contents: &str) -> Option<Frontmatter> {
    let mut lines = contents.lines();
    if lines.next().map(str::trim) != Some("---") {
        return None;
    }

    let mut frontmatter = Frontmatter::default();
    let mut in_metadata = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if trimmed.is_empty() {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = strip_quotes(value.trim());
        if line.starts_with(char::is_whitespace) {
            if in_metadata {
                match key {
                    "node_type" => frontmatter.node_type = non_empty(value),
                    "type" => frontmatter.typ = non_empty(value),
                    "originSessionId" => frontmatter.origin_session_id = non_empty(value),
                    _ => {}
                }
            }
        } else {
            in_metadata = key == "metadata";
            match key {
                "name" => frontmatter.name = non_empty(value),
                "description" => frontmatter.description = non_empty(value),
                _ => {}
            }
        }
    }
    Some(frontmatter)
}

/// Return the note body after a complete leading `---` fenced block.
///
/// Files without a complete leading block are returned unchanged. One blank
/// separator line is removed because [`render_note`] supplies the canonical
/// separator when the body is promoted into a curated note.
pub(crate) fn body_after_frontmatter(contents: &str) -> &str {
    let Some(end) = frontmatter_end(contents) else {
        return contents;
    };
    strip_optional_separator(&contents[end..])
}

/// Report whether a note starts with two consecutive frontmatter-style blocks.
pub(crate) fn has_duplicate_frontmatter(contents: &str) -> bool {
    let Some(end) = frontmatter_end(contents) else {
        return false;
    };
    frontmatter_end(contents[end..].trim_start()).is_some()
}

/// Render a note file: frontmatter block followed by the body. `origin_session_id`
/// is emitted only when supplied (hand-authored notes may legitimately omit it).
pub(crate) fn render_note(
    name: &str,
    description: &str,
    typ: &str,
    origin_session_id: Option<&str>,
    body: &str,
) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("name: {name}\n"));
    out.push_str(&format!("description: {}\n", quote(description)));
    out.push_str("metadata:\n");
    out.push_str("  node_type: memory\n");
    out.push_str(&format!("  type: {typ}\n"));
    if let Some(session) = origin_session_id {
        out.push_str(&format!("  originSessionId: {session}\n"));
    }
    out.push_str("---\n\n");
    let body = body.trim_end_matches('\n');
    out.push_str(body);
    out.push('\n');
    out
}

/// Validate a note slug: non-empty, kebab/snake ASCII, no path separators.
pub(crate) fn is_valid_slug(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn strip_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn frontmatter_end(contents: &str) -> Option<usize> {
    let mut lines = contents.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim() != "---" {
        return None;
    }

    let mut end = first.len();
    for line in lines {
        end += line.len();
        if line.trim() == "---" {
            return Some(end);
        }
    }
    None
}

fn strip_optional_separator(contents: &str) -> &str {
    contents
        .strip_prefix("\r\n")
        .or_else(|| contents.strip_prefix('\n'))
        .unwrap_or(contents)
}

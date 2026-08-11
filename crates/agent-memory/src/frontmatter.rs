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

/// Parse a complete leading `---` fenced YAML block. Returns `None` when the
/// file has no complete frontmatter block at all.
pub(crate) fn parse(contents: &str) -> Option<Frontmatter> {
    parse_leading_block(contents).map(|(frontmatter, _)| frontmatter)
}

/// Return the note body after recognizable candidate frontmatter.
///
/// Files without a complete, unindented block containing both `name` and
/// `description` are returned unchanged. One blank separator line is removed
/// because [`render_note`] supplies the canonical separator when the body is
/// promoted into a curated note.
pub(crate) fn body_after_frontmatter(contents: &str) -> &str {
    let Some((frontmatter, end)) = parse_leading_block(contents) else {
        return contents;
    };
    if !is_candidate_frontmatter(&frontmatter) {
        return contents;
    }
    strip_optional_separator(&contents[end..])
}

/// Return a useful preview for a candidate with recognizable optional
/// frontmatter.
///
/// Candidate files remain opaque input: a leading thematic `---` block is not
/// treated as metadata unless it carries `name` or `description`. When a
/// description is present it is the best bounded summary; otherwise the first
/// non-empty body line is used.
pub(crate) fn candidate_preview(contents: &str) -> Option<String> {
    let (frontmatter, end) = parse_leading_block(contents)?;
    if frontmatter.name.is_none() && frontmatter.description.is_none() {
        return None;
    }
    if let Some(description) = frontmatter.description {
        return Some(description);
    }
    strip_optional_separator(&contents[end..])
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// Report whether a note starts with two consecutive recognizable blocks.
pub(crate) fn has_duplicate_frontmatter(contents: &str) -> bool {
    let Some((_, end)) = parse_leading_block(contents) else {
        return false;
    };
    let body = strip_optional_separator(&contents[end..]);
    parse_leading_block(body).is_some_and(|(frontmatter, _)| is_candidate_frontmatter(&frontmatter))
}

fn parse_leading_block(contents: &str) -> Option<(Frontmatter, usize)> {
    let end = frontmatter_end(contents)?;
    let mut lines = contents[..end].lines();
    lines.next();

    let mut frontmatter = Frontmatter::default();
    let mut in_metadata = false;
    for line in lines {
        let trimmed = line.trim();
        if is_frontmatter_fence(line) {
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
    Some((frontmatter, end))
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

fn is_candidate_frontmatter(frontmatter: &Frontmatter) -> bool {
    frontmatter.name.is_some() && frontmatter.description.is_some()
}

fn frontmatter_end(contents: &str) -> Option<usize> {
    let mut lines = contents.split_inclusive('\n');
    let first = lines.next()?;
    if !is_frontmatter_fence(first) {
        return None;
    }

    let mut end = first.len();
    for line in lines {
        end += line.len();
        if is_frontmatter_fence(line) {
            return Some(end);
        }
    }
    None
}

fn is_frontmatter_fence(line: &str) -> bool {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    line.starts_with("---") && line.trim_end() == "---"
}

fn strip_optional_separator(contents: &str) -> &str {
    contents
        .strip_prefix("\r\n")
        .or_else(|| contents.strip_prefix('\n'))
        .unwrap_or(contents)
}

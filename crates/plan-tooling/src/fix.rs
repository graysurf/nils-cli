//! Mechanical rewriters for `plan-tooling validate --fix`.
//!
//! Fixers MUST be:
//! - **Idempotent**: `fix_text(fix_text(p)) == fix_text(p)` for every input.
//! - **Lossless**: a fixer never silently drops content.
//! - **Bounded**: only mechanical violations (no ambiguous choices, no
//!   structural edits beyond what the catalog declares).
//!
//! Anything ambiguous (e.g. choosing a file path for a missing `Location`,
//! deciding between directory-mode `/` and file-mode for a path) is left alone
//! so `validate` can surface it as a remaining error after the fix run.

/// Apply every mechanical fixer to the supplied plan / source-doc /
/// execution-state markdown body and return the rewritten text. The function
/// preserves the trailing-newline shape of the input.
pub(crate) fn fix_text(text: &str) -> String {
    let stage_a = fix_label_values(text);
    let stage_b = fix_sprint_metadata_pairs(&stage_a);
    fix_dependencies(&stage_b)
}

/// Strip mechanical wrappers (backticks, single-link `[label](href)`) from the
/// canonical bundle-pointer label values so the strict parser path keeps
/// working without re-running fix.
fn fix_label_values(text: &str) -> String {
    const LABELS: &[&str] = &[
        "Primary source",
        "Recommended plan",
        "Recommended execution state",
        "Source document",
    ];
    let trailing_newline = text.ends_with('\n');
    let mut out_lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut new_line = line.to_string();
        for label in LABELS {
            if let Some((prefix, value)) = split_at_label_colon(line, label) {
                let stripped = strip_value_wrappers(value);
                if stripped != value {
                    new_line = format!("{prefix}{stripped}");
                }
                break;
            }
        }
        out_lines.push(new_line);
    }
    join_lines(&out_lines, trailing_newline)
}

/// Split formatter-collapsed sprint metadata back into the canonical two-line
/// shape. This is intentionally limited to the two canonical sprint metadata
/// labels and only when the line starts with `PR grouping intent`.
fn fix_sprint_metadata_pairs(text: &str) -> String {
    let trailing_newline = text.ends_with('\n');
    let mut out_lines: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(replacement) = split_same_line_sprint_metadata(line) {
            out_lines.extend(replacement);
        } else {
            out_lines.push(line.to_string());
        }
    }
    join_lines(&out_lines, trailing_newline)
}

fn split_same_line_sprint_metadata(line: &str) -> Option<Vec<String>> {
    const INTENT_LABEL: &str = "**PR grouping intent**:";
    const PROFILE_LABEL: &str = "**Execution Profile**:";

    let indent = leading_spaces(line);
    let after_indent = &line[indent..];
    let (marker, after_marker) = split_one_marker(after_indent);
    let after_intent = after_marker.strip_prefix(INTENT_LABEL)?;
    let profile_idx = after_intent.find(PROFILE_LABEL)?;
    let intent_value = clean_sprint_metadata_separator(&after_intent[..profile_idx]);
    let profile_value = after_intent[profile_idx + PROFILE_LABEL.len()..].trim();
    if intent_value.is_empty() || profile_value.is_empty() {
        return None;
    }

    let prefix = format!("{}{}", " ".repeat(indent), marker);
    Some(vec![
        format!("{prefix}{INTENT_LABEL} {intent_value}"),
        format!("{prefix}{PROFILE_LABEL} {profile_value}"),
    ])
}

fn split_one_marker(text: &str) -> (&str, &str) {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = text.strip_prefix(marker) {
            return (marker, rest);
        }
    }
    ("", text)
}

fn clean_sprint_metadata_separator(value: &str) -> &str {
    value.trim().trim_end_matches(['-', '|', ';', ',']).trim()
}

/// Walk Dependencies blocks and rewrite each list item:
/// - `<digits>.<digits>` → `Task <digits>.<digits>`
/// - inline comma list `Task 1.1, Task 1.2` → multi-line bullets at the same
///   indent (also applies the bare-digits → `Task N.M` rewrite per part)
///
/// Leaves annotated entries (`Task 1.1 (only when X)`) untouched.
fn fix_dependencies(text: &str) -> String {
    let trailing_newline = text.ends_with('\n');
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(deps_indent) = dependencies_field_indent(line) {
            out.push(line.to_string());
            i += 1;
            // Consume lines inside the Dependencies block. We leave the block
            // when we hit a line indented at-or-below the field's own indent
            // that is non-empty.
            while i < lines.len() {
                let l = lines[i];
                if l.trim().is_empty() {
                    out.push(l.to_string());
                    i += 1;
                    continue;
                }
                let l_indent = leading_spaces(l);
                if l_indent <= deps_indent {
                    break;
                }
                if let Some(replacement) = fix_dependency_list_item(l) {
                    for new_line in replacement {
                        out.push(new_line);
                    }
                } else {
                    out.push(l.to_string());
                }
                i += 1;
            }
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }
    join_lines(&out, trailing_newline)
}

fn join_lines(lines: &[String], trailing_newline: bool) -> String {
    let mut joined = lines.join("\n");
    if trailing_newline {
        joined.push('\n');
    }
    joined
}

fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

/// Returns the indent (leading spaces) of a line that opens a Dependencies
/// field block, or `None` if the line is not a Dependencies field line.
fn dependencies_field_indent(line: &str) -> Option<usize> {
    let indent = leading_spaces(line);
    let rest = &line[indent..];
    let after_marker = rest.strip_prefix("- ").unwrap_or(rest);
    if after_marker.starts_with("**Dependencies**:") || after_marker.starts_with("Dependencies:") {
        Some(indent)
    } else {
        None
    }
}

/// Apply mechanical rewrites to a single Dependencies list-item line. Returns
/// the replacement lines (one or more) when the line changes, or `None` when
/// no rewrite applies.
fn fix_dependency_list_item(line: &str) -> Option<Vec<String>> {
    let indent = leading_spaces(line);
    let rest = &line[indent..];
    let after_dash = rest.strip_prefix("- ")?;
    let trailing = trailing_whitespace(after_dash);
    let value = &after_dash[..after_dash.len() - trailing.len()];
    let trimmed = value.trim();
    let needs_split = has_inline_comma_task_list(trimmed);
    let needs_prefix = is_bare_digits_form(trimmed);
    if !needs_split && !needs_prefix {
        return None;
    }
    let parts: Vec<String> = if needs_split {
        trimmed
            .split(',')
            .map(|p| normalize_dependency_value(p.trim()))
            .collect()
    } else {
        vec![normalize_dependency_value(trimmed)]
    };
    let prefix = " ".repeat(indent);
    Some(
        parts
            .into_iter()
            .map(|p| format!("{prefix}- {p}"))
            .collect(),
    )
}

fn trailing_whitespace(s: &str) -> &str {
    let end = s.trim_end().len();
    &s[end..]
}

fn normalize_dependency_value(value: &str) -> String {
    if is_bare_digits_form(value) {
        format!("Task {value}")
    } else {
        value.to_string()
    }
}

fn is_bare_digits_form(s: &str) -> bool {
    let trimmed = s.trim();
    let Some((a, b)) = trimmed.split_once('.') else {
        return false;
    };
    !a.is_empty()
        && !b.is_empty()
        && a.chars().all(|c| c.is_ascii_digit())
        && b.chars().all(|c| c.is_ascii_digit())
}

fn has_inline_comma_task_list(s: &str) -> bool {
    if !s.contains(',') {
        return false;
    }
    // Only auto-split when every comma-separated part is itself a recognizable
    // dependency token. This is a conservative gate so we don't accidentally
    // shred annotated entries whose notes happen to contain commas.
    s.split(',').all(|part| {
        let p = part.trim();
        is_bare_digits_form(p)
            || (p.starts_with("Task ") && is_bare_digits_form(p.trim_start_matches("Task ").trim()))
    })
}

/// Locate the colon for a known label on a line. Returns
/// `(prefix_up_to_and_including_label_colon_and_space, value_portion)` when
/// the line matches; `None` otherwise.
///
/// Accepted forms:
/// - `- Label: value`
/// - `* Label: value` / `+ Label: value`
/// - `Label: value`
/// - `- **Label**: value`
/// - `**Label**: value`
fn split_at_label_colon<'a>(line: &'a str, label: &str) -> Option<(&'a str, &'a str)> {
    let indent = leading_spaces(line);
    let after_indent = &line[indent..];
    let (marker_len, after_marker) = if let Some(rest) = strip_one_marker(after_indent) {
        (after_indent.len() - rest.len(), rest)
    } else {
        (0, after_indent)
    };
    let bold = format!("**{label}**:");
    let plain = format!("{label}:");
    let label_len = if after_marker.starts_with(&bold) {
        bold.len()
    } else if after_marker.starts_with(&plain) {
        plain.len()
    } else {
        return None;
    };
    let prefix_end = indent + marker_len + label_len;
    let value_start = line[prefix_end..].chars().take_while(|c| *c == ' ').count();
    let full_prefix_end = prefix_end + value_start;
    let prefix = &line[..full_prefix_end];
    let value = &line[full_prefix_end..];
    Some((prefix, value))
}

fn strip_one_marker(text: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = text.strip_prefix(marker) {
            return Some(rest);
        }
    }
    None
}

fn strip_value_wrappers(value: &str) -> String {
    let trimmed = value.trim_end();
    let mut current = trimmed.to_string();
    if let Some(href) = strip_one_markdown_link(&current) {
        current = href.to_string();
    }
    let t = current.trim();
    if t.len() >= 2 && t.starts_with('`') && t.ends_with('`') {
        current = t[1..t.len() - 1].trim().to_string();
    }
    current
}

fn strip_one_markdown_link(value: &str) -> Option<&str> {
    let s = value.trim();
    if !s.starts_with('[') || !s.ends_with(')') {
        return None;
    }
    let split = s.find("](")?;
    if s[1..split].contains(']') {
        return None;
    }
    let href = &s[split + 2..s.len() - 1];
    if href.contains(')') {
        return None;
    }
    Some(href)
}

#[cfg(test)]
mod tests {
    use super::{
        fix_dependencies, fix_dependency_list_item, fix_label_values, fix_sprint_metadata_pairs,
        fix_text, has_inline_comma_task_list, is_bare_digits_form, normalize_dependency_value,
        strip_value_wrappers,
    };
    use pretty_assertions::assert_eq;

    #[test]
    fn fix_text_is_idempotent_for_fixture_set() {
        for fixture in FIXTURES {
            let once = fix_text(fixture);
            let twice = fix_text(&once);
            assert_eq!(once, twice, "fix not a fixed point for fixture:\n{fixture}");
        }
    }

    #[test]
    fn fix_text_rewrites_bare_digit_dep_to_task_form() {
        let before = "- **Dependencies**:\n  - 1.1\n";
        let after = fix_text(before);
        assert!(
            after.contains("- Task 1.1"),
            "expected `- Task 1.1`, got:\n{after}",
        );
        assert!(!after.contains("- 1.1\n"), "got:\n{after}");
    }

    #[test]
    fn fix_text_splits_inline_comma_dep_list() {
        let before = "- **Dependencies**:\n  - Task 1.1, Task 1.2\n";
        let after = fix_text(before);
        assert!(
            after.contains("  - Task 1.1\n  - Task 1.2"),
            "expected multi-line bullets, got:\n{after}",
        );
    }

    #[test]
    fn fix_text_splits_inline_comma_bare_digits() {
        let before = "- **Dependencies**:\n  - 1.1, 1.2\n";
        let after = fix_text(before);
        assert!(
            after.contains("  - Task 1.1\n  - Task 1.2"),
            "got:\n{after}",
        );
    }

    #[test]
    fn fix_text_strips_backtick_value_on_primary_source() {
        let before = "- Primary source: `docs/source/spec.md`\n";
        let after = fix_text(before);
        assert_eq!(after, "- Primary source: docs/source/spec.md\n");
    }

    #[test]
    fn fix_text_strips_markdown_link_value_on_recommended_plan() {
        let before =
            "- Recommended plan: [docs/plans/demo/demo-plan.md](docs/plans/demo/demo-plan.md)\n";
        let after = fix_text(before);
        assert_eq!(after, "- Recommended plan: docs/plans/demo/demo-plan.md\n");
    }

    #[test]
    fn fix_text_strips_value_under_bold_label_with_backticks() {
        let before = "**Source document**: `docs/plans/demo/demo-plan.md`\n";
        let after = fix_text(before);
        assert_eq!(after, "**Source document**: docs/plans/demo/demo-plan.md\n");
    }

    #[test]
    fn fix_text_preserves_annotated_dependency_entries() {
        // Notes containing commas would be unsafe to comma-split; the gate
        // `has_inline_comma_task_list` must veto this case.
        let before = "- **Dependencies**:\n  - Task 1.1 (only when X, flagged)\n";
        let after = fix_text(before);
        assert_eq!(after, before, "annotated entry must round-trip unchanged");
    }

    #[test]
    fn fix_text_preserves_unrelated_content() {
        let before = "# Plan: Demo\n\nSome prose.\n\n## Sprint 1: First\n";
        let after = fix_text(before);
        assert_eq!(after, before);
    }

    #[test]
    fn fix_text_splits_same_line_sprint_metadata() {
        let before = "- **PR grouping intent**: `group` - **Execution Profile**: `parallel-x2`\n";
        let after = fix_text(before);
        assert_eq!(
            after,
            "- **PR grouping intent**: `group`\n- **Execution Profile**: `parallel-x2`\n"
        );
    }

    #[test]
    fn fix_text_splits_unbulleted_same_line_sprint_metadata() {
        let before = "  **PR grouping intent**: `group` **Execution Profile**: `serial`\n";
        let after = fix_text(before);
        assert_eq!(
            after,
            "  **PR grouping intent**: `group`\n  **Execution Profile**: `serial`\n"
        );
    }

    #[test]
    fn fix_text_preserves_trailing_newline_shape() {
        // No trailing newline.
        let before = "- Primary source: `docs/source/spec.md`";
        let after = fix_text(before);
        assert_eq!(after, "- Primary source: docs/source/spec.md");
        // With trailing newline.
        let before = "- Primary source: `docs/source/spec.md`\n";
        let after = fix_text(before);
        assert_eq!(after, "- Primary source: docs/source/spec.md\n");
    }

    #[test]
    fn is_bare_digits_form_accepts_only_dotted_digit_pairs() {
        assert!(is_bare_digits_form("1.1"));
        assert!(is_bare_digits_form("10.42"));
        assert!(!is_bare_digits_form("1"));
        assert!(!is_bare_digits_form("1.a"));
        assert!(!is_bare_digits_form("1.1.5"));
        assert!(!is_bare_digits_form("Task 1.1"));
    }

    #[test]
    fn has_inline_comma_task_list_is_conservative() {
        assert!(has_inline_comma_task_list("Task 1.1, Task 1.2"));
        assert!(has_inline_comma_task_list("1.1, 1.2"));
        assert!(has_inline_comma_task_list("Task 1.1, 1.2"));
        // Annotated forms are NOT auto-split.
        assert!(!has_inline_comma_task_list(
            "Task 1.1 (only when X, flagged)"
        ));
        assert!(!has_inline_comma_task_list("only on Tuesday, mostly"));
    }

    #[test]
    fn normalize_dependency_value_adds_task_prefix_when_bare() {
        assert_eq!(normalize_dependency_value("1.1"), "Task 1.1");
        assert_eq!(normalize_dependency_value("Task 1.1"), "Task 1.1");
        assert_eq!(normalize_dependency_value("note"), "note");
    }

    #[test]
    fn strip_value_wrappers_chains_backtick_and_link() {
        // We don't expect nested wrappers in practice, but verify single-pass
        // behavior so chained edits stay predictable.
        assert_eq!(strip_value_wrappers("`docs/x.md`"), "docs/x.md");
        assert_eq!(strip_value_wrappers("[docs/x.md](docs/x.md)"), "docs/x.md",);
        assert_eq!(strip_value_wrappers("docs/x.md"), "docs/x.md");
    }

    #[test]
    fn fix_label_values_only_touches_known_labels() {
        // "Some other field" must NOT be rewritten.
        let before = "- Some other field: `value`\n";
        let after = fix_label_values(before);
        assert_eq!(after, before);
    }

    #[test]
    fn fix_dependencies_leaves_non_deps_blocks_alone() {
        let before = "- **Acceptance criteria**:\n  - 1.1\n";
        let after = fix_dependencies(before);
        assert_eq!(
            after, before,
            "Acceptance criteria items must not be rewritten"
        );
    }

    #[test]
    fn fix_sprint_metadata_pairs_leaves_noncanonical_labels_alone() {
        let before = "- **PR Grouping Intent**: `group` - **Execution Profile**: `serial`\n";
        let after = fix_sprint_metadata_pairs(before);
        assert_eq!(after, before);
    }

    #[test]
    fn fix_dependency_list_item_returns_none_for_already_canonical() {
        assert_eq!(fix_dependency_list_item("  - Task 1.1"), None);
        assert_eq!(fix_dependency_list_item("  - Task 1.1 (note)"), None);
        assert_eq!(fix_dependency_list_item("  - none"), None);
    }

    // Property fixture set used by the idempotence test. Each entry exercises
    // a different fixer path.
    const FIXTURES: &[&str] = &[
        // Pre-canonical form (no-op expected).
        "- **Dependencies**:\n  - Task 1.1\n  - Task 1.2\n",
        // Bare digit form needs prefix.
        "- **Dependencies**:\n  - 1.1\n",
        // Inline comma list with bare digits.
        "- **Dependencies**:\n  - 1.1, 1.2\n",
        // Inline comma list with Task form.
        "- **Dependencies**:\n  - Task 1.1, Task 1.2\n",
        // Mixed.
        "- **Dependencies**:\n  - 1.1, Task 1.2\n",
        // Annotated entry must round-trip.
        "- **Dependencies**:\n  - Task 1.1 (only when X flagged)\n",
        // Multiple list items with mixed forms.
        "- **Dependencies**:\n  - 1.1\n  - Task 1.2\n  - Task 1.3 (note)\n",
        // Label value rewrites.
        "- Primary source: `docs/source/spec.md`\n",
        "- Recommended plan: [docs/plans/demo/demo-plan.md](docs/plans/demo/demo-plan.md)\n",
        "**Source document**: `docs/plans/demo/demo-plan.md`\n",
        // Plain plan-only line (no-op expected).
        "- Primary source: plan-only waiver: bounded change\n",
        // Same-line sprint metadata needs canonicalization.
        "- **PR grouping intent**: `group` - **Execution Profile**: `parallel-x2`\n",
        // Empty.
        "",
        // Unrelated content.
        "# Plan: Demo\n\nSome prose.\n\n## Sprint 1: First\n",
    ];
}

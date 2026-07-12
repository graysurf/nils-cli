//! Surgical, byte-preserving sync of the `## Execution State` header bullets.
//!
//! Backs three callers so the durable execution-state Markdown stays in step
//! with the runtime `run-state.json` at the workflow's two transitions:
//!
//! - `plan-issue record open` writes the `- Tracking issue:` URL once the live
//!   issue exists (so `plan-archive discover` can infer the provider ref).
//! - `plan-issue record close` writes the terminal state back (`- Status:`,
//!   task fields, `- Last updated:`, `- Branch/commit/PR:`, and `## Handoff`)
//!   so the in-repo file is coherent after closeout, not transient-stale until
//!   `plan-archive migrate`.
//! - `plan-tooling exec-state-sync` exposes the same routine as an on-demand
//!   repair command for existing bundles.
//!
//! Scope is intentionally narrow, mirroring [`crate::ledger`]: only the named
//! `- <Label>:` bullets inside `## Execution State` and the `## Handoff` body
//! are touched. Structural Markdown headings bound both sections, and every
//! other byte is preserved verbatim. The `## Task Ledger` rows are owned by
//! [`crate::ledger`] and the existing `close-ready` `ledger-rows-pending` gate,
//! so this module never rewrites them.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use nils_common::fs as common_fs;
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use serde::Serialize;

const HEADING: &str = "## Execution State";

/// Canonical bullet labels this module syncs.
pub const TRACKING_ISSUE_LABEL: &str = "Tracking issue";
pub const STATUS_LABEL: &str = "Status";
pub const LAST_UPDATED_LABEL: &str = "Last updated";
pub const BRANCH_LABEL: &str = "Branch/commit/PR";
pub const CURRENT_TASK_LABEL: &str = "Current task";
pub const NEXT_TASK_LABEL: &str = "Next task";
pub const HANDOFF_HEADING: &str = "## Handoff";

/// Placeholder values that mean "not yet recorded" and must be replaced
/// rather than treated as a real value.
const PLACEHOLDERS: &[&str] = &["not yet opened", "tbd", "pending", "none", "n/a", "-"];

#[derive(Debug)]
pub enum ExecStateError {
    ReadFailed {
        path: PathBuf,
        source: io::Error,
    },
    WriteFailed {
        path: PathBuf,
        source: common_fs::AtomicWriteError,
    },
    SectionMissing {
        path: PathBuf,
    },
    DuplicateSection {
        path: PathBuf,
        heading: String,
    },
    InvalidField {
        field: &'static str,
        reason: &'static str,
    },
}

impl ExecStateError {
    pub fn code(&self) -> &'static str {
        match self {
            ExecStateError::ReadFailed { .. } => "exec-state-read-failed",
            ExecStateError::WriteFailed { .. } => "exec-state-write-failed",
            ExecStateError::SectionMissing { .. } => "exec-state-section-missing",
            ExecStateError::DuplicateSection { .. } => "exec-state-duplicate-section",
            ExecStateError::InvalidField { .. } => "exec-state-invalid-field",
        }
    }
}

impl fmt::Display for ExecStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecStateError::ReadFailed { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            ExecStateError::WriteFailed { path, source } => {
                write!(f, "failed to write {}: {source}", path.display())
            }
            ExecStateError::SectionMissing { path } => {
                write!(f, "{}: missing `{HEADING}` section", path.display())
            }
            ExecStateError::DuplicateSection { path, heading } => {
                write!(f, "{}: duplicate `{heading}` sections", path.display())
            }
            ExecStateError::InvalidField { field, reason } => {
                write!(f, "invalid `{field}` value: {reason}")
            }
        }
    }
}

impl std::error::Error for ExecStateError {}

/// What happened to a single bullet during a sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BulletAction {
    /// The bullet already carried the desired value.
    Unchanged,
    /// An existing bullet's value was rewritten.
    Patched,
    /// The bullet was absent and appended to the section.
    Inserted,
}

/// One bullet change record, surfaced in JSON output.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BulletChange {
    pub label: String,
    pub action: BulletAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    pub value: String,
}

/// Aggregate outcome of a sync. `changed` is true when at least one bullet or
/// section was patched or inserted (i.e. the file content differs).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SyncReport {
    pub changed: bool,
    pub bullets: Vec<BulletChange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<SectionChange>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SectionChange {
    pub heading: String,
    pub action: BulletAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    pub value: String,
}

/// Terminal-state fields written back at closeout.
#[derive(Debug, Clone, Default)]
pub struct TerminalState {
    /// Terminal `- Status:` value (must read as terminal to `plan-archive
    /// discover`, e.g. `complete; tracking issue closed`).
    pub status: Option<String>,
    /// `- Last updated:` stamp (caller-supplied; no clock in this crate).
    pub last_updated: Option<String>,
    /// `- Branch/commit/PR:` value, typically the merged PR ref/URL.
    pub branch_commit_pr: Option<String>,
    /// `- Tracking issue:` URL, ensured present (kept from open, or backfilled).
    pub tracking_issue_url: Option<String>,
    /// Terminal `- Current task:` value.
    pub current_task: Option<String>,
    /// Terminal `- Next task:` value.
    pub next_task: Option<String>,
    /// Replacement body for the `## Handoff` section (inserted when absent).
    pub handoff: Option<String>,
}

/// Return the current `- Tracking issue:` value inside `## Execution State`,
/// or `None` when the bullet is absent. The angle-bracket autolink wrapper is
/// stripped so callers compare bare values.
pub fn tracking_issue_value(raw: &str) -> Option<String> {
    bullet_value(raw, TRACKING_ISSUE_LABEL).map(|v| unwrap_autolink(&v))
}

/// True when `value` is empty or a known "not yet recorded" placeholder.
pub fn is_placeholder(value: &str) -> bool {
    let t = value.trim();
    t.is_empty() || PLACEHOLDERS.contains(&t.to_ascii_lowercase().as_str())
}

/// Write-if-missing/placeholder/mismatch the `- Tracking issue:` bullet to the
/// canonical autolinked `url`. Idempotent: a re-run with the same URL is a
/// no-op. Used by `record open` and by the self-heal path.
pub fn sync_tracking_issue(
    path: &Path,
    url: &str,
    dry_run: bool,
) -> Result<SyncReport, ExecStateError> {
    let raw = read(path)?;
    let value = format_autolink(url);
    let (new_text, change) = set_bullet(&raw, path, TRACKING_ISSUE_LABEL, &value)?;
    let changed = change.action != BulletAction::Unchanged;
    if !dry_run {
        write_if_changed(path, &raw, &new_text)?;
    }
    Ok(SyncReport {
        changed,
        bullets: vec![change],
        sections: Vec::new(),
    })
}

/// Write the terminal-state bullets back at closeout. Only the fields present
/// in `state` are touched. Byte-preserving and idempotent. With `dry_run` the
/// change set is computed and reported but the file is left untouched.
pub fn writeback_terminal(
    path: &Path,
    state: &TerminalState,
    dry_run: bool,
) -> Result<SyncReport, ExecStateError> {
    validate_terminal_state(state)?;
    let mut raw = read(path)?;
    require_section(&raw, path, HEADING)?;
    let original = raw.clone();
    let mut bullets = Vec::new();
    let mut sections = Vec::new();

    let mut apply = |raw: &mut String, label: &str, value: &str| -> Result<(), ExecStateError> {
        let (new_text, change) = set_bullet(raw, path, label, value)?;
        *raw = new_text;
        bullets.push(change);
        Ok(())
    };

    if let Some(url) = &state.tracking_issue_url {
        apply(&mut raw, TRACKING_ISSUE_LABEL, &format_autolink(url))?;
    }
    if let Some(status) = &state.status {
        apply(&mut raw, STATUS_LABEL, status)?;
    }
    if let Some(current_task) = &state.current_task {
        apply(&mut raw, CURRENT_TASK_LABEL, current_task)?;
    }
    if let Some(next_task) = &state.next_task {
        apply(&mut raw, NEXT_TASK_LABEL, next_task)?;
    }
    if let Some(branch) = &state.branch_commit_pr {
        apply(&mut raw, BRANCH_LABEL, branch)?;
    }
    if let Some(updated) = &state.last_updated {
        apply(&mut raw, LAST_UPDATED_LABEL, updated)?;
    }
    if let Some(handoff) = &state.handoff {
        let (new_text, change) = set_section_body(&raw, path, HANDOFF_HEADING, handoff)?;
        raw = new_text;
        sections.push(change);
    }

    let changed = raw != original;
    if !dry_run {
        write_if_changed(path, &original, &raw)?;
    }
    Ok(SyncReport {
        changed,
        bullets,
        sections,
    })
}

fn validate_terminal_state(state: &TerminalState) -> Result<(), ExecStateError> {
    for (field, value) in [
        (CURRENT_TASK_LABEL, state.current_task.as_deref()),
        (NEXT_TASK_LABEL, state.next_task.as_deref()),
    ] {
        if value.is_some_and(|value| value.contains(['\n', '\r'])) {
            return Err(ExecStateError::InvalidField {
                field,
                reason: "must be a single line",
            });
        }
    }
    if state
        .handoff
        .as_deref()
        .is_some_and(|body| !structural_h2_headings(body).is_empty())
    {
        return Err(ExecStateError::InvalidField {
            field: "Handoff",
            reason: "must not contain a level-two heading",
        });
    }
    Ok(())
}

fn read(path: &Path) -> Result<String, ExecStateError> {
    std::fs::read_to_string(path).map_err(|source| ExecStateError::ReadFailed {
        path: path.to_path_buf(),
        source,
    })
}

fn write_if_changed(path: &Path, original: &str, new_text: &str) -> Result<(), ExecStateError> {
    if new_text == original {
        return Ok(());
    }
    common_fs::write_atomic(path, new_text.as_bytes(), 0o644).map_err(|source| {
        ExecStateError::WriteFailed {
            path: path.to_path_buf(),
            source,
        }
    })
}

/// Wrap a bare URL in a Markdown autolink (`<url>`); leave already-wrapped or
/// non-URL values untouched. Matches the healthy-bundle convention and keeps
/// rumdl's bare-URL lint happy.
fn format_autolink(value: &str) -> String {
    let t = value.trim();
    if t.starts_with('<') && t.ends_with('>') {
        return t.to_string();
    }
    if t.starts_with("http://") || t.starts_with("https://") {
        return format!("<{t}>");
    }
    t.to_string()
}

fn unwrap_autolink(value: &str) -> String {
    let t = value.trim();
    t.strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(t)
        .to_string()
}

/// Read the value of `- <label>:` inside `## Execution State`. Continuation
/// (wrapped) lines are joined with single spaces.
fn bullet_value(raw: &str, label: &str) -> Option<String> {
    let lines: Vec<&str> = raw.split('\n').collect();
    let (start, end) = section_bounds(raw, HEADING).ok().flatten()?;
    let needle = format!("- {label}:");
    for (idx, line) in lines.iter().enumerate().take(end).skip(start) {
        if line.trim_start().starts_with(&needle) {
            let mut value = line.trim_start()[needle.len()..].trim().to_string();
            // Fold wrapped continuation lines into the value.
            let mut j = idx + 1;
            while j < end && is_continuation(lines[j]) {
                value.push(' ');
                value.push_str(lines[j].trim());
                j += 1;
            }
            return Some(value.trim().to_string());
        }
    }
    None
}

/// Set the value of `- <label>: <value>` inside `## Execution State`. Replaces
/// a present bullet (including any wrapped continuation lines) or appends a new
/// one after the section's last bullet. Returns the new text and the change.
fn set_bullet(
    raw: &str,
    path: &Path,
    label: &str,
    value: &str,
) -> Result<(String, BulletChange), ExecStateError> {
    let trailing_newline = raw.ends_with('\n');
    let lines: Vec<&str> = raw.split('\n').collect();
    let (start, end) = require_section(raw, path, HEADING)?;

    let needle = format!("- {label}:");
    let rendered = format!("- {label}: {}", value.trim());

    // Locate an existing bullet (matched at the start of its trimmed text).
    let mut bullet_idx = None;
    for (idx, line) in lines.iter().enumerate().take(end).skip(start) {
        if line.trim_start().starts_with(&needle) {
            bullet_idx = Some(idx);
            break;
        }
    }

    let mut new_lines: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
    let change;

    if let Some(idx) = bullet_idx {
        // Capture the previous value (folding continuation lines).
        let previous = bullet_value(raw, label).unwrap_or_default();
        // Determine the span of continuation lines to drop.
        let mut last = idx;
        let mut j = idx + 1;
        while j < end && is_continuation(lines[j]) {
            last = j;
            j += 1;
        }
        // Replace [idx..=last] with the single rendered line.
        new_lines.splice(idx..=last, std::iter::once(rendered.clone()));
        let action = if previous.trim() == value.trim() {
            BulletAction::Unchanged
        } else {
            BulletAction::Patched
        };
        change = BulletChange {
            label: label.to_string(),
            action,
            previous: Some(previous),
            value: value.trim().to_string(),
        };
    } else {
        // Insert after the section's last bullet line.
        let mut insert_at = start;
        for (idx, line) in lines.iter().enumerate().take(end).skip(start) {
            if line.trim_start().starts_with("- ") {
                insert_at = idx + 1;
            }
        }
        new_lines.insert(insert_at, rendered.clone());
        change = BulletChange {
            label: label.to_string(),
            action: BulletAction::Inserted,
            previous: None,
            value: value.trim().to_string(),
        };
    }

    let mut new_text = new_lines.join("\n");
    if trailing_newline && !new_text.ends_with('\n') {
        new_text.push('\n');
    } else if !trailing_newline && new_text.ends_with('\n') {
        new_text.pop();
    }
    Ok((new_text, change))
}

/// Replace one second-level section body without touching surrounding bytes,
/// or append the section when it is absent.
fn set_section_body(
    raw: &str,
    path: &Path,
    heading: &str,
    body: &str,
) -> Result<(String, SectionChange), ExecStateError> {
    let trailing_newline = raw.ends_with('\n');
    let normalized = body.trim().to_string();
    let matches = matching_h2_sections(raw, heading);
    if matches.len() > 1 {
        return Err(ExecStateError::DuplicateSection {
            path: path.to_path_buf(),
            heading: heading.to_string(),
        });
    }

    if let Some(section) = matches.first() {
        let previous = raw[section.body_start..section.end].trim().to_string();
        if previous == normalized {
            return Ok((
                raw.to_string(),
                SectionChange {
                    heading: heading.to_string(),
                    action: BulletAction::Unchanged,
                    previous: Some(previous),
                    value: normalized,
                },
            ));
        }
        let heading_has_newline = section.body_start > section.start
            && raw.as_bytes().get(section.body_start - 1) == Some(&b'\n');
        let mut replacement = if heading_has_newline {
            "\n".to_string()
        } else {
            "\n\n".to_string()
        };
        replacement.push_str(&normalized);
        if section.end < raw.len() {
            replacement.push_str("\n\n");
        } else if trailing_newline {
            replacement.push('\n');
        }
        let mut new_text = String::with_capacity(
            raw.len() - (section.end - section.body_start) + replacement.len(),
        );
        new_text.push_str(&raw[..section.body_start]);
        new_text.push_str(&replacement);
        new_text.push_str(&raw[section.end..]);
        return Ok((
            new_text,
            SectionChange {
                heading: heading.to_string(),
                action: BulletAction::Patched,
                previous: Some(previous),
                value: normalized,
            },
        ));
    }

    let mut new_text = raw.trim_end_matches('\n').to_string();
    if !new_text.is_empty() {
        new_text.push_str("\n\n");
    }
    new_text.push_str(heading);
    new_text.push_str("\n\n");
    new_text.push_str(&normalized);
    if trailing_newline {
        new_text.push('\n');
    }
    Ok((
        new_text,
        SectionChange {
            heading: heading.to_string(),
            action: BulletAction::Inserted,
            previous: None,
            value: normalized,
        },
    ))
}

#[derive(Debug)]
struct MarkdownSection {
    start: usize,
    body_start: usize,
    end: usize,
    title: String,
    canonical_atx: bool,
}

#[derive(Debug)]
struct MarkdownHeading {
    start: usize,
    title: String,
    canonical_atx: bool,
}

fn structural_h2_headings(raw: &str) -> Vec<MarkdownHeading> {
    let mut headings = Vec::new();
    let mut current: Option<(usize, String)> = None;
    let mut container_depth = 0usize;
    for (event, range) in Parser::new(raw).into_offset_iter() {
        match event {
            Event::Start(Tag::BlockQuote(_) | Tag::List(_) | Tag::Item) => {
                container_depth += 1;
            }
            Event::Start(Tag::Heading {
                level: HeadingLevel::H2,
                ..
            }) if container_depth == 0 => {
                let line_start = raw[..range.start]
                    .rfind('\n')
                    .map(|offset| offset + 1)
                    .unwrap_or(0);
                current = Some((line_start, String::new()));
            }
            Event::Text(text) | Event::Code(text) if current.is_some() => {
                current.as_mut().expect("checked").1.push_str(&text);
            }
            Event::End(TagEnd::Heading(HeadingLevel::H2)) => {
                if let Some((start, title)) = current.take() {
                    headings.push(MarkdownHeading {
                        start,
                        title: title.trim().to_string(),
                        canonical_atx: is_canonical_atx_h2(raw, start),
                    });
                }
            }
            Event::End(TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item) => {
                container_depth = container_depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    headings
}

fn is_canonical_atx_h2(raw: &str, line_start: usize) -> bool {
    let line = raw[line_start..]
        .split_once('\n')
        .map(|(line, _)| line)
        .unwrap_or(&raw[line_start..])
        .trim_end_matches('\r');
    let indentation = line.bytes().take_while(|byte| *byte == b' ').count();
    if indentation > 3 {
        return false;
    }
    let marker = &line[indentation..];
    marker == "##" || marker.starts_with("## ") || marker.starts_with("##\t")
}

fn markdown_sections(raw: &str) -> Vec<MarkdownSection> {
    let headings = structural_h2_headings(raw);
    headings
        .iter()
        .enumerate()
        .map(|(idx, heading)| {
            let body_start = raw[heading.start..]
                .find('\n')
                .map(|offset| heading.start + offset + 1)
                .unwrap_or(raw.len());
            let end = headings
                .get(idx + 1)
                .map(|next| next.start)
                .unwrap_or(raw.len());
            MarkdownSection {
                start: heading.start,
                body_start,
                end,
                title: heading.title.clone(),
                canonical_atx: heading.canonical_atx,
            }
        })
        .collect()
}

fn heading_title(heading: &str) -> &str {
    heading.strip_prefix("## ").unwrap_or(heading).trim()
}

fn matching_h2_sections(raw: &str, heading: &str) -> Vec<MarkdownSection> {
    let title = heading_title(heading);
    markdown_sections(raw)
        .into_iter()
        .filter(|section| section.canonical_atx && section.title == title)
        .collect()
}

/// `(start, end)` line indices spanning one structural H2 section body.
fn section_bounds(raw: &str, heading: &str) -> Result<Option<(usize, usize)>, &'static str> {
    let matches = matching_h2_sections(raw, heading);
    if matches.len() > 1 {
        return Err("duplicate section");
    }
    Ok(matches.first().map(|section| {
        let unterminated_eof = section.end == raw.len() && !raw.ends_with('\n');
        let start = raw[..section.body_start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + usize::from(unterminated_eof && section.body_start == section.end);
        let end = raw[..section.end]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + usize::from(unterminated_eof);
        (start, end)
    }))
}

fn require_section(
    raw: &str,
    path: &Path,
    heading: &str,
) -> Result<(usize, usize), ExecStateError> {
    section_bounds(raw, heading)
        .map_err(|_| ExecStateError::DuplicateSection {
            path: path.to_path_buf(),
            heading: heading.to_string(),
        })?
        .ok_or_else(|| ExecStateError::SectionMissing {
            path: path.to_path_buf(),
        })
}

/// A wrapped continuation line of a bullet: indented, non-blank, and not the
/// start of a new bullet.
fn is_continuation(line: &str) -> bool {
    if line.trim().is_empty() {
        return false;
    }
    let indented = line.starts_with(' ') || line.starts_with('\t');
    indented && !line.trim_start().starts_with("- ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Plan X Execution State

<!-- plan-issue-record:v2 role=state profile=tracking -->
## Execution State

- Status: tracking issue opened; implementation not yet started.
- Target scope: a long scope value that wraps across two lines here for
  testing continuation folding behavior.
- Last updated: 2026-06-01
- Branch/commit/PR: tracker opened from committed bundle `f34b082`; planned
  implementation branch `feat/x`; no PR opened.
- Tracking issue: not yet opened
- Source snapshot: pending

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | pending | Do the thing |  | note |

## Session Log

- 2026-06-01: authored.
";

    #[test]
    fn syncs_tracking_issue_from_placeholder() {
        let (text, change) = set_bullet(
            SAMPLE,
            Path::new("x.md"),
            TRACKING_ISSUE_LABEL,
            "<https://github.com/o/r/issues/9>",
        )
        .expect("set");
        assert_eq!(change.action, BulletAction::Patched);
        assert_eq!(change.previous.as_deref(), Some("not yet opened"));
        assert!(text.contains("- Tracking issue: <https://github.com/o/r/issues/9>"));
        // Untouched neighbours.
        assert!(text.contains("- Source snapshot: pending"));
        assert!(text.contains("## Task Ledger"));
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn tracking_issue_sync_is_idempotent() {
        let value = "<https://github.com/o/r/issues/9>";
        let (once, _) = set_bullet(SAMPLE, Path::new("x.md"), TRACKING_ISSUE_LABEL, value).unwrap();
        let (twice, change) =
            set_bullet(&once, Path::new("x.md"), TRACKING_ISSUE_LABEL, value).unwrap();
        assert_eq!(change.action, BulletAction::Unchanged);
        assert_eq!(once, twice);
    }

    #[test]
    fn replaces_wrapped_multiline_bullet_with_single_line() {
        let (text, change) =
            set_bullet(SAMPLE, Path::new("x.md"), BRANCH_LABEL, "o/r#10 merged").expect("set");
        assert_eq!(change.action, BulletAction::Patched);
        assert!(text.contains("- Branch/commit/PR: o/r#10 merged"));
        // The old wrapped continuation line must be gone.
        assert!(!text.contains("implementation branch `feat/x`"));
        // The following bullet survives.
        assert!(text.contains("- Tracking issue: not yet opened"));
    }

    #[test]
    fn folds_continuation_lines_when_reading_value() {
        let v = bullet_value(SAMPLE, "Target scope").expect("value");
        assert_eq!(
            v,
            "a long scope value that wraps across two lines here for testing continuation folding behavior."
        );
    }

    #[test]
    fn inserts_missing_bullet_after_last_bullet() {
        let stripped = SAMPLE.replace("- Tracking issue: not yet opened\n", "");
        let (text, change) = set_bullet(
            &stripped,
            Path::new("x.md"),
            TRACKING_ISSUE_LABEL,
            "<https://github.com/o/r/issues/9>",
        )
        .expect("set");
        assert_eq!(change.action, BulletAction::Inserted);
        assert!(text.contains("- Tracking issue: <https://github.com/o/r/issues/9>"));
        // Inserted within the section, before the Task Ledger heading.
        let issue_pos = text.find("- Tracking issue:").unwrap();
        let ledger_pos = text.find("## Task Ledger").unwrap();
        assert!(issue_pos < ledger_pos);
    }

    #[test]
    fn writeback_terminal_sets_only_named_fields() {
        let dir = std::env::temp_dir().join(format!("exec-state-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x-execution-state.md");
        std::fs::write(&path, SAMPLE).unwrap();
        let report = writeback_terminal(
            &path,
            &TerminalState {
                status: Some("complete; tracking issue closed".to_string()),
                current_task: Some("none; tracking issue closed".to_string()),
                next_task: Some("none; tracking issue closed".to_string()),
                last_updated: Some("2026-06-02".to_string()),
                branch_commit_pr: Some("o/r#10 merged".to_string()),
                tracking_issue_url: Some("https://github.com/o/r/issues/9".to_string()),
                handoff: Some("- Tracking issue is closed; no action remains.".to_string()),
            },
            false,
        )
        .expect("writeback");
        assert!(report.changed);
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("- Status: complete; tracking issue closed"));
        assert!(out.contains("- Current task: none; tracking issue closed"));
        assert!(out.contains("- Next task: none; tracking issue closed"));
        assert!(out.contains("- Last updated: 2026-06-02"));
        assert!(out.contains("- Branch/commit/PR: o/r#10 merged"));
        assert!(out.contains("- Tracking issue: <https://github.com/o/r/issues/9>"));
        assert!(out.contains("## Handoff\n\n- Tracking issue is closed; no action remains."));
        // Ledger and session log preserved.
        assert!(out.contains("| 1.1 | pending | Do the thing |  | note |"));
        assert!(out.contains("## Session Log"));
        // Idempotent re-run.
        let again = writeback_terminal(
            &path,
            &TerminalState {
                status: Some("complete; tracking issue closed".to_string()),
                current_task: Some("none; tracking issue closed".to_string()),
                next_task: Some("none; tracking issue closed".to_string()),
                last_updated: Some("2026-06-02".to_string()),
                branch_commit_pr: Some("o/r#10 merged".to_string()),
                tracking_issue_url: Some("https://github.com/o/r/issues/9".to_string()),
                handoff: Some("- Tracking issue is closed; no action remains.".to_string()),
            },
            false,
        )
        .expect("writeback2");
        assert!(!again.changed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn handoff_rewrite_ignores_heading_text_inside_code() {
        let raw = "## Execution State\n\n- Status: active\n\n```markdown\n## Handoff\n\n- fake\n```\n\n## Handoff\n\n- real stale action\n\n## Session Log\n\n- retained\n";
        let (out, _) =
            set_section_body(raw, Path::new("x.md"), HANDOFF_HEADING, "- closed").unwrap();
        assert!(out.contains("```markdown\n## Handoff\n\n- fake\n```"));
        assert!(out.contains("## Handoff\n\n- closed\n\n## Session Log"));
        assert!(!out.contains("real stale action"));
        assert!(out.contains("- retained"));
    }

    #[test]
    fn handoff_rewrite_ignores_heading_text_inside_indented_code_and_raw_html() {
        let raw = "## Execution State\n\n- Status: active\n\nBoundary paragraph.\n\n    ## Handoff\n    - indented fake\n\n<script>\n## Handoff\n- raw HTML fake\n</script>\n\n## Handoff\n\n- stale\n";
        let (out, _) =
            set_section_body(raw, Path::new("x.md"), HANDOFF_HEADING, "- closed").unwrap();
        assert!(out.contains("    ## Handoff\n    - indented fake"));
        assert!(out.contains("<script>\n## Handoff\n- raw HTML fake\n</script>"));
        assert!(out.ends_with("## Handoff\n\n- closed\n"));
    }

    #[test]
    fn handoff_rewrite_ignores_headings_nested_in_quotes_and_lists() {
        let raw = "## Execution State\n\n- Status: active\n\n> ## Handoff\n>\n> - quoted fake\n\n- list item\n\n  ## Handoff\n\n  - list fake\n\n## Handoff\n\n- stale\n";
        let (out, _) =
            set_section_body(raw, Path::new("x.md"), HANDOFF_HEADING, "- closed").unwrap();
        assert!(out.contains("> ## Handoff\n>\n> - quoted fake"));
        assert!(out.contains("  ## Handoff\n\n  - list fake"));
        assert!(out.ends_with("## Handoff\n\n- closed\n"));
    }

    #[test]
    fn handoff_rewrite_preserves_indented_following_h2() {
        // One leading space remains a top-level heading after the preceding
        // list; two spaces would make it part of that list item in CommonMark.
        let raw = "## Execution State\n\n- Status: active\n\n## Handoff\n\n- stale\n\n ## Session Log\n\n- retained\n";
        let (out, _) =
            set_section_body(raw, Path::new("x.md"), HANDOFF_HEADING, "- closed").unwrap();
        assert!(out.contains("## Handoff\n\n- closed\n\n ## Session Log"));
        assert!(out.contains("- retained"));
    }

    #[test]
    fn setext_h2_is_a_boundary_but_not_a_canonical_section_target() {
        let raw = "## Execution State\n\n- Status: active\n\nHandoff\n-------\n\n- setext content\n\n## Session Log\n\n- retained\n";
        let (out, change) =
            set_section_body(raw, Path::new("x.md"), HANDOFF_HEADING, "- closed").unwrap();
        assert_eq!(change.action, BulletAction::Inserted);
        assert!(out.contains("Handoff\n-------\n\n- setext content"));
        assert!(out.contains("## Session Log\n\n- retained"));
        assert!(out.ends_with("## Handoff\n\n- closed\n"));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setext-execution-state.md");
        let setext = "Execution State\n---------------\n\n- Status: active\n";
        std::fs::write(&path, setext).unwrap();
        let err = writeback_terminal(
            &path,
            &TerminalState {
                status: Some("closed".to_string()),
                ..TerminalState::default()
            },
            false,
        )
        .expect_err("only canonical ATX Execution State is writable");
        assert_eq!(err.code(), "exec-state-section-missing");
        assert_eq!(std::fs::read_to_string(path).unwrap(), setext);
    }

    #[test]
    fn writeback_rejects_multiline_task_and_peer_h2_in_handoff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x-execution-state.md");
        std::fs::write(&path, SAMPLE).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let task_err = writeback_terminal(
            &path,
            &TerminalState {
                current_task: Some("closed\n## Task Ledger".to_string()),
                ..TerminalState::default()
            },
            false,
        )
        .expect_err("multiline task must fail closed");
        assert_eq!(task_err.code(), "exec-state-invalid-field");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        let handoff_err = writeback_terminal(
            &path,
            &TerminalState {
                handoff: Some("- closed\n\n## Session Log\n\n- injected".to_string()),
                ..TerminalState::default()
            },
            false,
        )
        .expect_err("peer H2 must fail closed");
        assert_eq!(handoff_err.code(), "exec-state-invalid-field");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn writeback_allows_non_peer_h2_examples_in_handoff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x-execution-state.md");
        std::fs::write(&path, SAMPLE).unwrap();
        let handoff = "> ## Historical example\n>\n> This quoted heading is not a peer section.";
        writeback_terminal(
            &path,
            &TerminalState {
                handoff: Some(handoff.to_string()),
                ..TerminalState::default()
            },
            false,
        )
        .expect("nested heading must remain contained");
        assert!(std::fs::read_to_string(path).unwrap().contains(handoff));
    }

    #[test]
    fn writeback_requires_execution_state_and_rejects_duplicate_handoff() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.md");
        std::fs::write(&missing, "# unrelated\n").unwrap();
        let state = TerminalState {
            handoff: Some("- closed".to_string()),
            ..TerminalState::default()
        };
        let err = writeback_terminal(&missing, &state, false).expect_err("missing guard");
        assert_eq!(err.code(), "exec-state-section-missing");
        assert_eq!(std::fs::read_to_string(&missing).unwrap(), "# unrelated\n");

        let nested = dir.path().join("nested.md");
        let nested_raw = "> ## Execution State\n>\n> - Status: active\n";
        std::fs::write(&nested, nested_raw).unwrap();
        let err = writeback_terminal(&nested, &state, false).expect_err("nested guard");
        assert_eq!(err.code(), "exec-state-section-missing");
        assert_eq!(std::fs::read_to_string(&nested).unwrap(), nested_raw);

        let duplicate = dir.path().join("duplicate.md");
        let raw = "## Execution State\n\n- Status: active\n\n## Handoff\n\n- one\n\n## Handoff\n\n- two\n";
        std::fs::write(&duplicate, raw).unwrap();
        let err = writeback_terminal(&duplicate, &state, false).expect_err("duplicate guard");
        assert_eq!(err.code(), "exec-state-duplicate-section");
        assert_eq!(std::fs::read_to_string(&duplicate).unwrap(), raw);
    }

    #[test]
    fn equivalent_handoff_body_preserves_exact_bytes_and_reports_unchanged() {
        let raw = "## Execution State\n\n- Status: active\n\n## Handoff\n\n\n- closed\n\n\n## Session Log\n\n- retained\n";
        let (out, change) =
            set_section_body(raw, Path::new("x.md"), HANDOFF_HEADING, "- closed").unwrap();
        assert_eq!(out, raw);
        assert_eq!(change.action, BulletAction::Unchanged);
    }

    #[test]
    fn handoff_rewrite_preserves_no_trailing_newline_contract() {
        let raw = "## Execution State\n\n- Status: active\n\n## Handoff\n\n- stale";
        let (out, change) =
            set_section_body(raw, Path::new("x.md"), HANDOFF_HEADING, "- closed").unwrap();
        assert_eq!(
            out,
            "## Execution State\n\n- Status: active\n\n## Handoff\n\n- closed"
        );
        assert_eq!(change.action, BulletAction::Patched);
        let (again, change) =
            set_section_body(&out, Path::new("x.md"), HANDOFF_HEADING, "- closed").unwrap();
        assert_eq!(again, out);
        assert_eq!(change.action, BulletAction::Unchanged);
    }

    #[test]
    fn tracking_issue_value_unwraps_autolink() {
        assert_eq!(
            tracking_issue_value(
                "## Execution State\n\n- Tracking issue: <https://github.com/o/r/issues/9>\n"
            )
            .as_deref(),
            Some("https://github.com/o/r/issues/9")
        );
        assert_eq!(
            tracking_issue_value("## Execution State\n\n- Tracking issue: not yet opened\n")
                .as_deref(),
            Some("not yet opened")
        );
    }

    #[test]
    fn reads_and_updates_last_bullet_without_trailing_newline() {
        let raw = "## Execution State\n\n- Status: active\n- Tracking issue: not yet opened";
        assert_eq!(tracking_issue_value(raw).as_deref(), Some("not yet opened"));

        let (out, change) = set_bullet(
            raw,
            Path::new("x.md"),
            TRACKING_ISSUE_LABEL,
            "<https://github.com/o/r/issues/9>",
        )
        .expect("set");
        assert_eq!(change.action, BulletAction::Patched);
        assert_eq!(out.matches("- Tracking issue:").count(), 1);
        assert_eq!(
            out,
            "## Execution State\n\n- Status: active\n- Tracking issue: <https://github.com/o/r/issues/9>"
        );
    }

    #[test]
    fn inserts_after_heading_only_section_without_trailing_newline() {
        let raw = "## Execution State";
        let value = "<https://github.com/o/r/issues/9>";
        let (once, change) =
            set_bullet(raw, Path::new("x.md"), TRACKING_ISSUE_LABEL, value).expect("set");
        assert_eq!(change.action, BulletAction::Inserted);
        assert_eq!(
            once,
            "## Execution State\n- Tracking issue: <https://github.com/o/r/issues/9>"
        );

        let (twice, change) =
            set_bullet(&once, Path::new("x.md"), TRACKING_ISSUE_LABEL, value).expect("set");
        assert_eq!(change.action, BulletAction::Unchanged);
        assert_eq!(twice, once);
        assert_eq!(twice.matches("- Tracking issue:").count(), 1);
    }

    #[test]
    fn is_placeholder_detects_known_tokens() {
        assert!(is_placeholder("not yet opened"));
        assert!(is_placeholder("TBD"));
        assert!(is_placeholder(""));
        assert!(!is_placeholder("https://github.com/o/r/issues/9"));
    }

    #[test]
    fn missing_section_is_an_error() {
        let err = set_bullet("# no section here\n", Path::new("x.md"), STATUS_LABEL, "x")
            .expect_err("missing");
        assert_eq!(err.code(), "exec-state-section-missing");
    }
}

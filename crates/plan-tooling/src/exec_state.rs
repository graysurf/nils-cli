//! Surgical, byte-preserving sync of the `## Execution State` header bullets.
//!
//! Backs three callers so the durable execution-state Markdown stays in step
//! with the runtime `run-state.json` at the workflow's two transitions:
//!
//! - `plan-issue record open` writes the `- Tracking issue:` URL once the live
//!   issue exists (so `plan-archive discover` can infer the provider ref).
//! - `plan-issue record close` writes the terminal state back (`- Status:`,
//!   `- Last updated:`, `- Branch/commit/PR:`) so the in-repo file is final
//!   after closeout, not transient-stale until `plan-archive migrate`.
//! - `plan-tooling exec-state-sync` exposes the same routine as an on-demand
//!   repair command for existing bundles.
//!
//! Scope is intentionally narrow, mirroring [`crate::ledger`]: only the named
//! `- <Label>:` bullets inside the `## Execution State` section are touched and
//! every other byte of the file is preserved verbatim. The `## Task Ledger`
//! rows are owned by [`crate::ledger`] and the existing `close-ready`
//! `ledger-rows-pending` gate, so this module never rewrites them.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use nils_common::fs as common_fs;
use serde::Serialize;

const HEADING: &str = "## Execution State";

/// Canonical bullet labels this module syncs.
pub const TRACKING_ISSUE_LABEL: &str = "Tracking issue";
pub const STATUS_LABEL: &str = "Status";
pub const LAST_UPDATED_LABEL: &str = "Last updated";
pub const BRANCH_LABEL: &str = "Branch/commit/PR";

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
}

impl ExecStateError {
    pub fn code(&self) -> &'static str {
        match self {
            ExecStateError::ReadFailed { .. } => "exec-state-read-failed",
            ExecStateError::WriteFailed { .. } => "exec-state-write-failed",
            ExecStateError::SectionMissing { .. } => "exec-state-section-missing",
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

/// Aggregate outcome of a sync. `changed` is true when at least one bullet was
/// patched or inserted (i.e. the file content differs).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SyncReport {
    pub changed: bool,
    pub bullets: Vec<BulletChange>,
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
    let mut raw = read(path)?;
    let original = raw.clone();
    let mut bullets = Vec::new();

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
    if let Some(branch) = &state.branch_commit_pr {
        apply(&mut raw, BRANCH_LABEL, branch)?;
    }
    if let Some(updated) = &state.last_updated {
        apply(&mut raw, LAST_UPDATED_LABEL, updated)?;
    }

    let changed = raw != original;
    if !dry_run {
        write_if_changed(path, &original, &raw)?;
    }
    Ok(SyncReport { changed, bullets })
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
    let (start, end) = section_bounds(&lines)?;
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
    let (start, end) = section_bounds(&lines).ok_or_else(|| ExecStateError::SectionMissing {
        path: path.to_path_buf(),
    })?;

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

/// `(start, end)` line indices spanning the body of `## Execution State`
/// (exclusive of the heading line, up to the next `## ` heading or EOF).
fn section_bounds(lines: &[&str]) -> Option<(usize, usize)> {
    let heading_idx = lines.iter().position(|l| l.trim() == HEADING)?;
    let start = heading_idx + 1;
    let mut end = lines.len();
    for (offset, line) in lines.iter().enumerate().skip(start) {
        if line.starts_with("## ") {
            end = offset;
            break;
        }
    }
    Some((start, end))
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
                last_updated: Some("2026-06-02".to_string()),
                branch_commit_pr: Some("o/r#10 merged".to_string()),
                tracking_issue_url: Some("https://github.com/o/r/issues/9".to_string()),
            },
            false,
        )
        .expect("writeback");
        assert!(report.changed);
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("- Status: complete; tracking issue closed"));
        assert!(out.contains("- Last updated: 2026-06-02"));
        assert!(out.contains("- Branch/commit/PR: o/r#10 merged"));
        assert!(out.contains("- Tracking issue: <https://github.com/o/r/issues/9>"));
        // Ledger and session log preserved.
        assert!(out.contains("| 1.1 | pending | Do the thing |  | note |"));
        assert!(out.contains("## Session Log"));
        // Idempotent re-run.
        let again = writeback_terminal(
            &path,
            &TerminalState {
                status: Some("complete; tracking issue closed".to_string()),
                last_updated: Some("2026-06-02".to_string()),
                branch_commit_pr: Some("o/r#10 merged".to_string()),
                tracking_issue_url: Some("https://github.com/o/r/issues/9".to_string()),
            },
            false,
        )
        .expect("writeback2");
        assert!(!again.changed);
        let _ = std::fs::remove_dir_all(&dir);
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

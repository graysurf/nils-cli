//! Minimal pipe-table reader/writer for execution-state Task Ledger sections.
//!
//! Used by `plan-tooling ledger-update` (Task 1.1) and intended to back the
//! follow-up `ledger-sync` + `tracking close-ready ledger-rows-pending`
//! consumers in the same Sprint. Scope is intentionally narrow: only the
//! `## Task Ledger` section is touched, every other byte of the file is
//! preserved verbatim.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use nils_common::fs as common_fs;
use nils_common::markdown::canonicalize_table_cell;

const HEADING: &str = "## Task Ledger";
const COL_ID: &str = "ID";
const COL_STATUS: &str = "Status";
const COL_EVIDENCE: &str = "Evidence";
const COL_NOTES: &str = "Notes";

/// Status vocabulary for `ledger-update`; the completion clap model consumes
/// this so the validator and shell completions cannot drift.
pub const STATUS_VALUES: &[&str] = &[
    "pending",
    "in-progress",
    "done",
    "deferred",
    "blocked",
    "waived",
];

#[derive(Debug)]
pub enum LedgerError {
    ReadFailed {
        path: PathBuf,
        source: io::Error,
    },
    WriteFailed {
        path: PathBuf,
        source: common_fs::AtomicWriteError,
    },
    TableMalformed {
        path: PathBuf,
        reason: String,
    },
    RowNotFound {
        path: PathBuf,
        task_id: String,
    },
    RowAmbiguous {
        path: PathBuf,
        task_id: String,
        occurrences: usize,
    },
    InvalidStatus {
        value: String,
    },
}

impl LedgerError {
    pub fn code(&self) -> &'static str {
        match self {
            LedgerError::ReadFailed { .. } => "ledger-file-read-failed",
            LedgerError::WriteFailed { .. } => "ledger-file-write-failed",
            LedgerError::TableMalformed { .. } => "ledger-table-malformed",
            LedgerError::RowNotFound { .. } => "ledger-row-not-found",
            LedgerError::RowAmbiguous { .. } => "ledger-row-ambiguous",
            LedgerError::InvalidStatus { .. } => "ledger-status-invalid",
        }
    }
}

impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LedgerError::ReadFailed { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            LedgerError::WriteFailed { path, source } => {
                write!(f, "failed to write {}: {source}", path.display())
            }
            LedgerError::TableMalformed { path, reason } => {
                write!(
                    f,
                    "{}: malformed Task Ledger table: {reason}",
                    path.display()
                )
            }
            LedgerError::RowNotFound { path, task_id } => {
                write!(f, "{}: no ledger row with ID `{task_id}`", path.display())
            }
            LedgerError::RowAmbiguous {
                path,
                task_id,
                occurrences,
            } => write!(
                f,
                "{}: ledger row ID `{task_id}` matches {occurrences} rows",
                path.display()
            ),
            LedgerError::InvalidStatus { value } => write!(
                f,
                "invalid --status `{value}`; expected one of {}",
                STATUS_VALUES.join(", ")
            ),
        }
    }
}

impl std::error::Error for LedgerError {}

pub fn validate_status(value: &str) -> Result<&str, LedgerError> {
    if STATUS_VALUES.contains(&value) {
        Ok(value)
    } else {
        Err(LedgerError::InvalidStatus {
            value: value.to_string(),
        })
    }
}

/// One parsed row from the `## Task Ledger` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerRow {
    pub id: String,
    pub status: String,
    pub task: String,
    pub evidence: String,
    pub notes: String,
}

/// Parse the `## Task Ledger` table out of `raw` and return rows.
///
/// Returns `Err(LedgerError::TableMalformed)` when the heading or expected
/// columns (`ID`, `Status`, `Task`, `Evidence`) are missing. `Notes` is
/// optional; rows without a notes column report an empty string.
pub fn read_rows(raw: &str, path: &Path) -> Result<Vec<LedgerRow>, LedgerError> {
    let lines: Vec<&str> = raw.split('\n').collect();
    let heading_idx = lines
        .iter()
        .position(|line| line.trim() == HEADING)
        .ok_or_else(|| LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("missing `{HEADING}` heading"),
        })?;
    let mut header_idx = None;
    for (idx, line) in lines.iter().enumerate().skip(heading_idx + 1) {
        if line.trim().is_empty() {
            continue;
        }
        if line.trim_start().starts_with('|') {
            header_idx = Some(idx);
            break;
        }
        return Err(LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("expected pipe-table header, got `{line}`"),
        });
    }
    let header_idx = header_idx.ok_or_else(|| LedgerError::TableMalformed {
        path: path.to_path_buf(),
        reason: "no table after `## Task Ledger` heading".to_string(),
    })?;
    let header_cells = split_row(lines[header_idx]).ok_or_else(|| LedgerError::TableMalformed {
        path: path.to_path_buf(),
        reason: "header is not a pipe row".to_string(),
    })?;
    let trimmed_header: Vec<String> = header_cells.iter().map(|c| c.trim().to_string()).collect();
    let id_col = trimmed_header
        .iter()
        .position(|c| c == COL_ID)
        .ok_or_else(|| LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("header missing `{COL_ID}` column"),
        })?;
    let status_col = trimmed_header
        .iter()
        .position(|c| c == COL_STATUS)
        .ok_or_else(|| LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("header missing `{COL_STATUS}` column"),
        })?;
    let task_col = trimmed_header
        .iter()
        .position(|c| c == "Task")
        .ok_or_else(|| LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: "header missing `Task` column".to_string(),
        })?;
    let evidence_col = trimmed_header
        .iter()
        .position(|c| c == COL_EVIDENCE)
        .ok_or_else(|| LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("header missing `{COL_EVIDENCE}` column"),
        })?;
    let notes_col = trimmed_header.iter().position(|c| c == COL_NOTES);

    let separator_idx = header_idx + 1;
    if !lines
        .get(separator_idx)
        .map(|line| is_separator_row(line))
        .unwrap_or(false)
    {
        return Err(LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: "expected separator row beneath header".to_string(),
        });
    }
    let mut rows = Vec::new();
    let mut idx = separator_idx + 1;
    while idx < lines.len() {
        let line = lines[idx];
        if line.trim().is_empty() {
            break;
        }
        if !line.trim_start().starts_with('|') {
            break;
        }
        if let Some(cells) = split_row(line) {
            rows.push(LedgerRow {
                id: cells
                    .get(id_col)
                    .map(|c| c.trim().to_string())
                    .unwrap_or_default(),
                status: cells
                    .get(status_col)
                    .map(|c| c.trim().to_string())
                    .unwrap_or_default(),
                task: cells
                    .get(task_col)
                    .map(|c| c.trim().to_string())
                    .unwrap_or_default(),
                evidence: cells
                    .get(evidence_col)
                    .map(|c| c.trim().to_string())
                    .unwrap_or_default(),
                notes: notes_col
                    .and_then(|c| cells.get(c))
                    .map(|c| c.trim().to_string())
                    .unwrap_or_default(),
            });
        }
        idx += 1;
    }
    Ok(rows)
}

/// Outcome of an in-memory ledger patch.
#[derive(Debug, Clone)]
pub struct PatchOutcome {
    pub task_id: String,
    pub previous_status: String,
    pub new_status: String,
    pub previous_evidence: String,
    pub new_evidence: String,
    pub previous_notes: String,
    pub new_notes: String,
    pub notes_changed: bool,
    pub new_text: String,
}

/// Apply a row patch to the file at `path`, write atomically, return outcome.
pub fn update_row(
    path: &Path,
    task_id: &str,
    status: &str,
    evidence: &str,
    notes: Option<&str>,
    dry_run: bool,
) -> Result<PatchOutcome, LedgerError> {
    validate_status(status)?;
    let raw = std::fs::read_to_string(path).map_err(|source| LedgerError::ReadFailed {
        path: path.to_path_buf(),
        source,
    })?;

    let outcome = patch_text(&raw, path, task_id, status, evidence, notes)?;
    if !dry_run && outcome.new_text != raw {
        common_fs::write_atomic(path, outcome.new_text.as_bytes(), 0o644).map_err(|source| {
            LedgerError::WriteFailed {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(outcome)
}

fn patch_text(
    raw: &str,
    path: &Path,
    task_id: &str,
    status: &str,
    evidence: &str,
    notes: Option<&str>,
) -> Result<PatchOutcome, LedgerError> {
    let trailing_newline = raw.ends_with('\n');
    let lines: Vec<&str> = raw.split('\n').collect();
    let heading_idx = lines
        .iter()
        .position(|line| line.trim() == HEADING)
        .ok_or_else(|| LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("missing `{HEADING}` heading"),
        })?;

    let mut header_idx = None;
    for (idx, line) in lines.iter().enumerate().skip(heading_idx + 1) {
        if line.trim().is_empty() {
            continue;
        }
        if line.trim_start().starts_with('|') {
            header_idx = Some(idx);
            break;
        }
        return Err(LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("expected pipe-table header after `{HEADING}`, got `{line}`"),
        });
    }
    let header_idx = header_idx.ok_or_else(|| LedgerError::TableMalformed {
        path: path.to_path_buf(),
        reason: "no table after `## Task Ledger` heading".to_string(),
    })?;

    let header_cells = split_row(lines[header_idx]).ok_or_else(|| LedgerError::TableMalformed {
        path: path.to_path_buf(),
        reason: format!("header line is not a pipe row: `{}`", lines[header_idx]),
    })?;
    let trimmed_header: Vec<String> = header_cells.iter().map(|c| c.trim().to_string()).collect();
    let id_col = trimmed_header
        .iter()
        .position(|c| c == COL_ID)
        .ok_or_else(|| LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("header missing `{COL_ID}` column: {trimmed_header:?}"),
        })?;
    let status_col = trimmed_header
        .iter()
        .position(|c| c == COL_STATUS)
        .ok_or_else(|| LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("header missing `{COL_STATUS}` column: {trimmed_header:?}"),
        })?;
    let evidence_col = trimmed_header
        .iter()
        .position(|c| c == COL_EVIDENCE)
        .ok_or_else(|| LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("header missing `{COL_EVIDENCE}` column: {trimmed_header:?}"),
        })?;
    let notes_col = trimmed_header.iter().position(|c| c == COL_NOTES);

    let separator_idx = header_idx + 1;
    if !lines
        .get(separator_idx)
        .map(|line| is_separator_row(line))
        .unwrap_or(false)
    {
        return Err(LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: "expected `| --- | --- |` separator row beneath header".to_string(),
        });
    }

    let mut matched: Vec<usize> = Vec::new();
    let mut idx = separator_idx + 1;
    while idx < lines.len() {
        let line = lines[idx];
        if line.trim().is_empty() {
            break;
        }
        if !line.trim_start().starts_with('|') {
            break;
        }
        if let Some(cells) = split_row(line)
            && cells.get(id_col).map(|c| c.trim()) == Some(task_id)
        {
            matched.push(idx);
        }
        idx += 1;
    }

    if matched.is_empty() {
        return Err(LedgerError::RowNotFound {
            path: path.to_path_buf(),
            task_id: task_id.to_string(),
        });
    }
    if matched.len() > 1 {
        return Err(LedgerError::RowAmbiguous {
            path: path.to_path_buf(),
            task_id: task_id.to_string(),
            occurrences: matched.len(),
        });
    }
    let row_idx = matched[0];
    let original_cells = split_row(lines[row_idx]).expect("matched row parses");
    let previous_status = original_cells
        .get(status_col)
        .map(|c| c.trim().to_string())
        .unwrap_or_default();
    let previous_evidence = original_cells
        .get(evidence_col)
        .map(|c| c.trim().to_string())
        .unwrap_or_default();
    let previous_notes = notes_col
        .and_then(|c| original_cells.get(c))
        .map(|c| c.trim().to_string())
        .unwrap_or_default();

    let new_evidence_value = canonicalize_table_cell(evidence).trim().to_string();
    let new_evidence = if new_evidence_value.is_empty() {
        previous_evidence.clone()
    } else if previous_evidence.is_empty() || is_evidence_placeholder(&previous_evidence) {
        new_evidence_value
    } else {
        format!("{previous_evidence}; {new_evidence_value}")
    };
    let new_status = status.to_string();
    let new_notes = match notes {
        Some(text) => canonicalize_table_cell(text).trim().to_string(),
        None => previous_notes.clone(),
    };
    let notes_changed = notes.is_some() && new_notes != previous_notes;

    let mut new_cells: Vec<String> = original_cells
        .iter()
        .map(|c| c.trim().to_string())
        .collect();
    if let Some(slot) = new_cells.get_mut(status_col) {
        *slot = new_status.clone();
    }
    if let Some(slot) = new_cells.get_mut(evidence_col) {
        *slot = new_evidence.clone();
    }
    if let (Some(col), Some(_)) = (notes_col, notes)
        && let Some(slot) = new_cells.get_mut(col)
    {
        *slot = new_notes.clone();
    }
    let rendered = render_row(&new_cells);

    let mut new_lines: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
    new_lines[row_idx] = rendered;
    let mut new_text = new_lines.join("\n");
    if trailing_newline && !new_text.ends_with('\n') {
        new_text.push('\n');
    } else if !trailing_newline && new_text.ends_with('\n') {
        new_text.pop();
    }

    Ok(PatchOutcome {
        task_id: task_id.to_string(),
        previous_status,
        new_status,
        previous_evidence,
        new_evidence,
        previous_notes,
        new_notes,
        notes_changed,
        new_text,
    })
}

/// Treat the conventional Task Ledger placeholders as semantically empty so
/// `ledger-update` replaces them with the first real evidence URL instead of
/// joining them with `; ` (which would render `—; https://…`).
fn is_evidence_placeholder(value: &str) -> bool {
    let token = value.trim();
    matches!(token, "—" | "-" | "--" | "–")
        || matches!(
            token.to_ascii_lowercase().as_str(),
            "" | "n/a" | "na" | "none" | "tbd"
        )
}

fn split_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim_end();
    let trimmed = trimmed.strip_prefix('|')?;
    let trimmed = trimmed.strip_suffix('|')?;
    Some(trimmed.split('|').map(|cell| cell.to_string()).collect())
}

fn is_separator_row(line: &str) -> bool {
    let Some(cells) = split_row(line) else {
        return false;
    };
    cells
        .iter()
        .all(|c| !c.trim().is_empty() && c.trim().chars().all(|ch| ch == '-' || ch == ':'))
}

fn render_row(cells: &[String]) -> String {
    let mut out = String::new();
    out.push('|');
    for cell in cells {
        let value = cell.trim();
        if value.is_empty() {
            out.push_str("  ");
        } else {
            out.push(' ');
            out.push_str(value);
            out.push(' ');
        }
        out.push('|');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Demo

## Task Ledger

| ID | Status | Task | Evidence | Notes |
| --- | --- | --- | --- | --- |
| 1.1 | pending | Implement `ledger-update` |  | first row |
| 1.2 | pending | Implement `ledger-sync` |  | second row |
| 2.1 | pending | Release tag |  |  |

## Notes

- trailing section preserved verbatim
";

    #[test]
    fn patches_status_and_evidence_into_empty_cell() {
        let outcome = patch_text(SAMPLE, Path::new("demo.md"), "1.1", "done", "PR #999", None)
            .expect("patch");
        assert_eq!(outcome.previous_status, "pending");
        assert_eq!(outcome.new_status, "done");
        assert_eq!(outcome.previous_evidence, "");
        assert_eq!(outcome.new_evidence, "PR #999");
        assert!(!outcome.notes_changed);
        assert!(
            outcome
                .new_text
                .contains("| 1.1 | done | Implement `ledger-update` | PR #999 | first row |")
        );
        // Ensure other rows untouched.
        assert!(
            outcome
                .new_text
                .contains("| 1.2 | pending | Implement `ledger-sync` |  | second row |")
        );
        assert!(outcome.new_text.contains("## Notes"));
        assert!(outcome.new_text.ends_with('\n'));
    }

    #[test]
    fn appends_to_existing_evidence_with_semicolon() {
        let prefilled = SAMPLE.replace(
            "| 1.1 | pending | Implement `ledger-update` |  | first row |",
            "| 1.1 | in-progress | Implement `ledger-update` | issue#146 | first row |",
        );
        let outcome = patch_text(
            &prefilled,
            Path::new("demo.md"),
            "1.1",
            "done",
            "PR #999",
            None,
        )
        .expect("patch");
        assert_eq!(outcome.previous_evidence, "issue#146");
        assert_eq!(outcome.new_evidence, "issue#146; PR #999");
        assert!(outcome.new_text.contains("issue#146; PR #999"));
    }

    #[test]
    fn empty_evidence_arg_preserves_existing_evidence() {
        let prefilled = SAMPLE.replace(
            "| 1.1 | pending | Implement `ledger-update` |  | first row |",
            "| 1.1 | in-progress | Implement `ledger-update` | issue#146 | first row |",
        );
        let outcome = patch_text(
            &prefilled,
            Path::new("demo.md"),
            "1.1",
            "in-progress",
            "",
            None,
        )
        .expect("patch");
        assert_eq!(outcome.new_evidence, "issue#146");
        assert_eq!(outcome.previous_evidence, "issue#146");
    }

    #[test]
    fn notes_only_changes_when_passed() {
        let outcome = patch_text(SAMPLE, Path::new("demo.md"), "1.1", "done", "PR #1", None)
            .expect("no notes");
        assert!(!outcome.notes_changed);
        assert_eq!(outcome.previous_notes, "first row");
        assert_eq!(outcome.new_notes, "first row");

        let outcome2 = patch_text(
            SAMPLE,
            Path::new("demo.md"),
            "1.1",
            "done",
            "PR #1",
            Some("updated notes"),
        )
        .expect("with notes");
        assert!(outcome2.notes_changed);
        assert_eq!(outcome2.new_notes, "updated notes");
        assert!(outcome2.new_text.contains("| updated notes |"));
    }

    #[test]
    fn missing_id_returns_row_not_found() {
        let err = patch_text(SAMPLE, Path::new("demo.md"), "99.9", "done", "x", None)
            .expect_err("not found");
        assert_eq!(err.code(), "ledger-row-not-found");
    }

    #[test]
    fn duplicate_id_returns_ambiguous() {
        let doubled = SAMPLE.replace(
            "| 1.2 | pending | Implement `ledger-sync` |  | second row |",
            "| 1.1 | pending | dup |  | dup |",
        );
        let err = patch_text(&doubled, Path::new("demo.md"), "1.1", "done", "x", None)
            .expect_err("ambiguous");
        assert_eq!(err.code(), "ledger-row-ambiguous");
    }

    #[test]
    fn missing_heading_returns_malformed() {
        let stripped = "# Empty\n\nNo ledger here.\n";
        let err = patch_text(stripped, Path::new("demo.md"), "1.1", "done", "x", None)
            .expect_err("malformed");
        assert_eq!(err.code(), "ledger-table-malformed");
    }

    #[test]
    fn invalid_status_rejected() {
        let err = patch_text(SAMPLE, Path::new("demo.md"), "1.1", "nope", "x", None);
        // patch_text accepts arbitrary status strings; the public update_row
        // entry point validates first. Exercise validate_status directly.
        let _ = err.expect("patch_text accepts arbitrary status");
        let err = validate_status("nope").expect_err("invalid");
        assert_eq!(err.code(), "ledger-status-invalid");
    }

    #[test]
    fn every_gate_terminal_status_is_a_valid_ledger_status() {
        // plan-tracking-testbed#65: the closeout gates treat
        // done/deferred/waived as terminal, so ledger-update must be able to
        // set each of them.
        for status in ["done", "deferred", "waived"] {
            assert!(
                validate_status(status).is_ok(),
                "`{status}` should be a valid ledger status"
            );
        }
    }

    #[test]
    fn render_row_pads_empty_cells_with_two_spaces() {
        assert_eq!(
            render_row(&["1.1".into(), "done".into(), "".into(), "PR".into()]),
            "| 1.1 | done |  | PR |"
        );
    }

    #[test]
    fn evidence_with_pipe_is_canonicalized() {
        let outcome =
            patch_text(SAMPLE, Path::new("demo.md"), "1.1", "done", "a|b", None).expect("patch");
        assert_eq!(outcome.new_evidence, "a/b");
    }

    #[test]
    fn em_dash_evidence_placeholder_is_replaced_not_appended() {
        // The bundle template ships rows with `—` (em-dash) in the
        // Evidence column to mean "empty". The first real evidence URL
        // must replace that placeholder, not produce `—; <url>`.
        let prefilled = SAMPLE.replace(
            "| 1.1 | pending | Implement `ledger-update` |  | first row |",
            "| 1.1 | pending | Implement `ledger-update` | — | first row |",
        );
        let outcome = patch_text(
            &prefilled,
            Path::new("demo.md"),
            "1.1",
            "done",
            "https://example/c/1",
            None,
        )
        .expect("patch");
        assert_eq!(outcome.previous_evidence, "—");
        assert_eq!(outcome.new_evidence, "https://example/c/1");
        assert!(
            !outcome.new_text.contains("—; https://example/c/1"),
            "evidence column still concatenated em-dash: {}",
            outcome.new_text
        );
        assert!(outcome.new_text.contains(
            "| 1.1 | done | Implement `ledger-update` | https://example/c/1 | first row |"
        ));
    }

    #[test]
    fn evidence_placeholder_variants_all_replace() {
        for placeholder in ["-", "--", "–", "n/a", "N/A", "none", "tbd", "TBD"] {
            let prefilled = SAMPLE.replace(
                "| 1.1 | pending | Implement `ledger-update` |  | first row |",
                &format!(
                    "| 1.1 | pending | Implement `ledger-update` | {placeholder} | first row |"
                ),
            );
            let outcome = patch_text(&prefilled, Path::new("demo.md"), "1.1", "done", "url", None)
                .expect("patch");
            assert_eq!(
                outcome.new_evidence, "url",
                "placeholder {placeholder:?} not replaced"
            );
        }
    }
}

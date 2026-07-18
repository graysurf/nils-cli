//! Minimal pipe-table reader/writer for execution-state Task Ledger sections.
//!
//! Used by `plan-tooling ledger-update` (Task 1.1) and intended to back the
//! follow-up `ledger-sync` + `tracking close-ready ledger-rows-pending`
//! consumers in the same Sprint. Scope is intentionally narrow: only the
//! `## Task Ledger` section is touched, every other byte of the file is
//! preserved verbatim.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Arc, Mutex};

use nils_common::fs as common_fs;
use nils_common::markdown::canonicalize_table_cell;
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag};

use crate::exec_state::{ExecutionStateMutation, MutationLockError};

const HEADING: &str = "## Task Ledger";
const COL_ID: &str = "ID";
const COL_STATUS: &str = "Status";
const COL_EVIDENCE: &str = "Evidence";
const COL_NOTES: &str = "Notes";

#[cfg(test)]
type AfterReadHook = Arc<dyn Fn(&Path) + Send + Sync>;
#[cfg(test)]
static AFTER_READ_HOOK: Mutex<Option<AfterReadHook>> = Mutex::new(None);

#[cfg(test)]
fn run_after_read_hook(path: &Path) {
    let hook = AFTER_READ_HOOK
        .lock()
        .expect("after-read hook lock")
        .clone();
    if let Some(hook) = hook {
        hook(path);
    }
}

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
    LockBusy {
        path: PathBuf,
        lock_path: PathBuf,
    },
    LockFailed {
        path: PathBuf,
        lock_path: PathBuf,
        source: io::Error,
    },
    UnsafeFileAlias {
        path: PathBuf,
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
            LedgerError::LockBusy { .. } => "ledger-update-lock-busy",
            LedgerError::LockFailed { .. } => "ledger-update-lock-failed",
            LedgerError::UnsafeFileAlias { .. } => "ledger-file-unsafe-alias",
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
            LedgerError::LockBusy { path, lock_path } => write!(
                f,
                "{}: execution-state mutation lock is busy at {}; retry after the active mutation finishes (the kernel releases the lock when its process exits)",
                path.display(),
                lock_path.display()
            ),
            LedgerError::LockFailed {
                path,
                lock_path,
                source,
            } => write!(
                f,
                "{}: failed to acquire execution-state mutation lock at {}: {source}",
                path.display(),
                lock_path.display()
            ),
            LedgerError::UnsafeFileAlias { path } => write!(
                f,
                "{}: execution-state file must be a regular file with exactly one hard link",
                path.display()
            ),
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

#[derive(Debug, Clone, Copy)]
struct LedgerColumns {
    id: usize,
    status: usize,
    task: usize,
    evidence: usize,
    notes: Option<usize>,
}

fn required_column_index(
    header: &[String],
    column: &str,
    path: &Path,
) -> Result<usize, LedgerError> {
    let matches = header
        .iter()
        .enumerate()
        .filter_map(|(index, value)| (value == column).then_some(index))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => Err(LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("header missing `{column}` column"),
        }),
        _ => Err(LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("header has duplicate `{column}` columns"),
        }),
    }
}

fn parse_header_columns(
    header_cells: &[String],
    path: &Path,
) -> Result<LedgerColumns, LedgerError> {
    let trimmed_header: Vec<String> = header_cells.iter().map(|c| c.trim().to_string()).collect();
    let id = required_column_index(&trimmed_header, COL_ID, path)?;
    let status = required_column_index(&trimmed_header, COL_STATUS, path)?;
    let task_columns = trimmed_header
        .iter()
        .enumerate()
        .filter_map(|(index, column)| matches!(column.as_str(), "Task" | "Title").then_some(index))
        .collect::<Vec<_>>();
    let task = match task_columns.as_slice() {
        [index] => *index,
        [] => {
            return Err(LedgerError::TableMalformed {
                path: path.to_path_buf(),
                reason: "header missing `Task` or `Title` column".to_string(),
            });
        }
        _ => {
            return Err(LedgerError::TableMalformed {
                path: path.to_path_buf(),
                reason: "header has ambiguous task-description columns (`Task` and `Title`)"
                    .to_string(),
            });
        }
    };
    let evidence = required_column_index(&trimmed_header, COL_EVIDENCE, path)?;
    let notes_columns = trimmed_header
        .iter()
        .enumerate()
        .filter_map(|(index, column)| (column == COL_NOTES).then_some(index))
        .collect::<Vec<_>>();
    let notes = match notes_columns.as_slice() {
        [] => None,
        [index] => Some(*index),
        _ => {
            return Err(LedgerError::TableMalformed {
                path: path.to_path_buf(),
                reason: format!("header has duplicate `{COL_NOTES}` columns"),
            });
        }
    };

    Ok(LedgerColumns {
        id,
        status,
        task,
        evidence,
        notes,
    })
}

fn task_ledger_heading_indices(raw: &str) -> Vec<usize> {
    Parser::new(raw)
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H2,
                ..
            }) => {
                let line_start = raw[..range.start]
                    .rfind('\n')
                    .map(|offset| offset + 1)
                    .unwrap_or(0);
                let line = raw[line_start..]
                    .split_once('\n')
                    .map(|(line, _)| line)
                    .unwrap_or(&raw[line_start..])
                    .trim_end_matches('\r');
                let indentation = line.bytes().take_while(|byte| *byte == b' ').count();
                (indentation <= 3 && line[indentation..].trim_end() == HEADING).then(|| {
                    raw[..line_start]
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count()
                })
            }
            _ => None,
        })
        .collect()
}

fn task_ledger_heading_index(raw: &str, path: &Path) -> Result<usize, LedgerError> {
    let headings = task_ledger_heading_indices(raw);
    match headings.as_slice() {
        [index] => Ok(*index),
        [] => Err(LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("missing `{HEADING}` heading"),
        }),
        _ => Err(LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("multiple `{HEADING}` headings"),
        }),
    }
}

/// Parse the `## Task Ledger` table out of `raw` and return rows.
///
/// Returns `Err(LedgerError::TableMalformed)` when the heading or expected
/// columns (`ID`, `Status`, exactly one of `Task`/`Title`, `Evidence`) are
/// missing. `Notes` is optional; rows without a notes column report an empty
/// string.
pub fn read_rows(raw: &str, path: &Path) -> Result<Vec<LedgerRow>, LedgerError> {
    let lines: Vec<&str> = raw.split('\n').collect();
    let heading_idx = task_ledger_heading_index(raw, path)?;
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
    let columns = parse_header_columns(&header_cells, path)?;

    let separator_idx = header_idx + 1;
    let separator_cells = lines
        .get(separator_idx)
        .and_then(|line| split_row(line))
        .filter(|_| is_separator_row(lines[separator_idx]))
        .ok_or_else(|| LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: "expected separator row beneath header".to_string(),
        })?;
    if separator_cells.len() != header_cells.len() {
        return Err(LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!(
                "separator has {} cells but header has {}",
                separator_cells.len(),
                header_cells.len()
            ),
        });
    }

    let mut rows = Vec::new();
    let mut id_occurrences = HashMap::new();
    let mut first_duplicate_id = None;
    let mut idx = separator_idx + 1;
    while idx < lines.len() {
        let line = lines[idx];
        if line.trim().is_empty() {
            break;
        }
        if !line.trim_start().starts_with('|') {
            return Err(LedgerError::TableMalformed {
                path: path.to_path_buf(),
                reason: format!("row {} is not a complete pipe row", idx + 1),
            });
        }
        let cells = split_row(line).ok_or_else(|| LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: format!("row {} is not a complete pipe row", idx + 1),
        })?;
        if cells.len() != header_cells.len() {
            return Err(LedgerError::TableMalformed {
                path: path.to_path_buf(),
                reason: format!(
                    "row {} has {} cells but header has {}",
                    idx + 1,
                    cells.len(),
                    header_cells.len()
                ),
            });
        }

        let id = cells[columns.id].trim().to_string();
        let status = cells[columns.status].trim().to_string();
        let task = cells[columns.task].trim().to_string();
        if id.is_empty() {
            return Err(LedgerError::TableMalformed {
                path: path.to_path_buf(),
                reason: format!("row {} has an empty task ID", idx + 1),
            });
        }
        let occurrences = id_occurrences.entry(id.clone()).or_insert(0);
        *occurrences += 1;
        if *occurrences == 2 && first_duplicate_id.is_none() {
            first_duplicate_id = Some(id.clone());
        }
        if !STATUS_VALUES.contains(&status.as_str()) {
            return Err(LedgerError::TableMalformed {
                path: path.to_path_buf(),
                reason: format!("row {} has invalid status `{status}`", idx + 1),
            });
        }
        if task.is_empty() {
            return Err(LedgerError::TableMalformed {
                path: path.to_path_buf(),
                reason: format!("row {} has an empty task description", idx + 1),
            });
        }

        rows.push(LedgerRow {
            id,
            status,
            task,
            evidence: cells[columns.evidence].trim().to_string(),
            notes: columns
                .notes
                .map(|column| cells[column].trim().to_string())
                .unwrap_or_default(),
        });
        idx += 1;
    }
    if rows.is_empty() {
        return Err(LedgerError::TableMalformed {
            path: path.to_path_buf(),
            reason: "Task Ledger must contain at least one row".to_string(),
        });
    }
    if let Some(task_id) = first_duplicate_id {
        let occurrences = id_occurrences[&task_id];
        return Err(LedgerError::RowAmbiguous {
            path: path.to_path_buf(),
            task_id,
            occurrences,
        });
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
    pub evidence_appended: bool,
    pub previous_notes: String,
    pub new_notes: String,
    pub notes_changed: bool,
    pub new_text: String,
}

fn map_mutation_lock_error(error: MutationLockError) -> LedgerError {
    match error {
        MutationLockError::Busy { path, lock_path } => LedgerError::LockBusy { path, lock_path },
        MutationLockError::UnsafeFileAlias { path } => LedgerError::UnsafeFileAlias { path },
        MutationLockError::Failed { path, source, .. }
            if source.kind() == io::ErrorKind::NotFound =>
        {
            LedgerError::ReadFailed { path, source }
        }
        MutationLockError::Failed {
            path,
            lock_path,
            source,
        } => LedgerError::LockFailed {
            path,
            lock_path,
            source,
        },
    }
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
    let mutation = ExecutionStateMutation::begin(path).map_err(map_mutation_lock_error)?;
    let raw = mutation
        .read_to_string()
        .map_err(|source| LedgerError::ReadFailed {
            path: path.to_path_buf(),
            source,
        })?;
    #[cfg(test)]
    run_after_read_hook(path);

    let outcome = patch_text(&raw, path, task_id, status, evidence, notes)?;
    if !dry_run && outcome.new_text != raw {
        mutation
            .write_atomic(outcome.new_text.as_bytes())
            .map_err(|source| LedgerError::WriteFailed {
                path: path.to_path_buf(),
                source,
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
    // Mutation is permitted only for a ledger that satisfies the same complete
    // controlled dialect consumed by checkpoint and close-ready readers.
    read_rows(raw, path)?;
    let trailing_newline = raw.ends_with('\n');
    let lines: Vec<&str> = raw.split('\n').collect();
    let heading_idx = task_ledger_heading_index(raw, path)?;

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
    let columns = parse_header_columns(&header_cells, path)?;
    let id_col = columns.id;
    let status_col = columns.status;
    let evidence_col = columns.evidence;
    let notes_col = columns.notes;

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
    let evidence_appended = !new_evidence_value.is_empty()
        && !previous_evidence.is_empty()
        && !is_evidence_placeholder(&previous_evidence);
    let new_evidence = if new_evidence_value.is_empty() {
        previous_evidence.clone()
    } else if evidence_appended {
        format!("{previous_evidence}; {new_evidence_value}")
    } else {
        new_evidence_value
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
        evidence_appended,
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
    let row = trimmed.strip_prefix('|')?;
    if !row.ends_with('|') || pipe_is_escaped(row.as_bytes(), row.len() - 1) {
        return None;
    }
    let row = &row[..row.len() - 1];

    let mut cells = Vec::new();
    let mut cell_start = 0;
    for (offset, byte) in row.bytes().enumerate() {
        if byte == b'|' && !pipe_is_escaped(row.as_bytes(), offset) {
            cells.push(row[cell_start..offset].to_string());
            cell_start = offset + 1;
        }
    }
    cells.push(row[cell_start..].to_string());
    Some(cells)
}

fn pipe_is_escaped(row: &[u8], pipe_offset: usize) -> bool {
    let preceding_backslashes = row[..pipe_offset]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count();
    preceding_backslashes % 2 == 1
}

fn is_separator_cell(cell: &str) -> bool {
    let delimiter = cell.trim();
    let delimiter = delimiter.strip_prefix(':').unwrap_or(delimiter);
    let delimiter = delimiter.strip_suffix(':').unwrap_or(delimiter);

    delimiter.len() >= 3 && delimiter.chars().all(|ch| ch == '-')
}

fn is_separator_row(line: &str) -> bool {
    let Some(cells) = split_row(line) else {
        return false;
    };
    cells.iter().all(|cell| is_separator_cell(cell))
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
    use std::sync::{Barrier, Condvar, mpsc};
    use std::thread;
    use std::time::Duration;

    static AFTER_READ_HOOK_TEST_LOCK: Mutex<()> = Mutex::new(());

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
    fn concurrent_updates_never_lose_a_successful_change() {
        let _hook_test_guard = AFTER_READ_HOOK_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Arc::new(temp.path().join("execution-state.md"));
        std::fs::write(path.as_ref(), SAMPLE).expect("write ledger");

        let arrivals = Arc::new((Mutex::new(0usize), Condvar::new()));
        let hook_arrivals = Arc::clone(&arrivals);
        let hook_path = Arc::clone(&path);
        *AFTER_READ_HOOK.lock().expect("install after-read hook") =
            Some(Arc::new(move |read_path| {
                if read_path != hook_path.as_ref() {
                    return;
                }
                let (lock, ready) = &*hook_arrivals;
                let mut count = lock.lock().expect("arrival count");
                *count += 1;
                ready.notify_all();
                let _ = ready
                    .wait_timeout_while(count, Duration::from_secs(1), |count| *count < 2)
                    .expect("wait for concurrent readers");
            }));

        let start = Arc::new(Barrier::new(3));
        let launch = |task_id: &'static str, evidence: &'static str| {
            let path = Arc::clone(&path);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                update_row(&path, task_id, "done", evidence, None, false)
            })
        };
        let first = launch("1.1", "first-concurrent-update");
        let second = launch("1.2", "second-concurrent-update");
        start.wait();

        let first = first.join();
        let second = second.join();
        *AFTER_READ_HOOK.lock().expect("clear after-read hook") = None;
        let results = [
            (
                "1.1",
                "first-concurrent-update",
                first.expect("first update thread"),
            ),
            (
                "1.2",
                "second-concurrent-update",
                second.expect("second update thread"),
            ),
        ];

        let final_text = std::fs::read_to_string(path.as_ref()).expect("read final ledger");
        let rows = read_rows(&final_text, path.as_ref()).expect("parse final ledger");
        for (task_id, evidence, result) in results {
            match result {
                Ok(_) => {
                    let row = rows
                        .iter()
                        .find(|row| row.id == task_id)
                        .expect("updated row remains present");
                    assert_eq!(
                        (row.status.as_str(), row.evidence.as_str()),
                        ("done", evidence),
                        "successful update to row {task_id} was lost"
                    );
                }
                Err(error) => assert_eq!(error.code(), "ledger-update-lock-busy"),
            }
        }
    }

    fn assert_overlapping_exec_state_mutation_never_loses_success(
        mutate: impl FnOnce(
            &Path,
        )
            -> Result<crate::exec_state::SyncReport, crate::exec_state::ExecStateError>,
        verify_mutation: impl FnOnce(&str),
    ) {
        let _hook_test_guard = AFTER_READ_HOOK_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Arc::new(temp.path().join("execution-state.md"));
        let initial = SAMPLE.replacen(
            "# Demo\n\n",
            "# Demo\n\n## Execution State\n\n- Status: active\n- Tracking issue: not yet opened\n\n",
            1,
        );
        std::fs::write(path.as_ref(), initial).expect("write execution state");

        let (read_tx, read_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let release_rx = Arc::new(Mutex::new(release_rx));
        let hook_path = Arc::clone(&path);
        *AFTER_READ_HOOK.lock().expect("install after-read hook") =
            Some(Arc::new(move |read_path| {
                if read_path != hook_path.as_ref() {
                    return;
                }
                read_tx.send(()).expect("signal ledger read");
                release_rx
                    .lock()
                    .expect("release receiver")
                    .recv()
                    .expect("release ledger update");
            }));

        let update_path = Arc::clone(&path);
        let update = thread::spawn(move || {
            update_row(
                &update_path,
                "1.1",
                "done",
                "ledger-concurrent-update",
                None,
                false,
            )
        });
        read_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("ledger read reached overlap point");

        let mutation_result = mutate(path.as_ref());
        release_tx.send(()).expect("release ledger update");
        let update_result = update.join().expect("ledger update thread");
        *AFTER_READ_HOOK.lock().expect("clear after-read hook") = None;

        update_result.expect("ledger update succeeds");
        let final_text = std::fs::read_to_string(path.as_ref()).expect("read final state");
        let row = read_rows(&final_text, path.as_ref())
            .expect("parse final ledger")
            .into_iter()
            .find(|row| row.id == "1.1")
            .expect("updated row remains present");
        assert_eq!(
            (row.status.as_str(), row.evidence.as_str()),
            ("done", "ledger-concurrent-update")
        );

        match mutation_result {
            Ok(_) => verify_mutation(&final_text),
            Err(error) => assert_eq!(error.code(), "exec-state-mutation-lock-busy"),
        }
    }

    #[test]
    fn ledger_update_overlapping_tracking_sync_never_loses_success() {
        assert_overlapping_exec_state_mutation_never_loses_success(
            |path| {
                crate::exec_state::sync_tracking_issue(path, "https://example.test/issues/1", false)
            },
            |final_text| {
                assert!(
                    final_text.contains("- Tracking issue: <https://example.test/issues/1>"),
                    "successful tracking-issue sync was lost"
                );
            },
        );
    }

    #[test]
    fn ledger_update_overlapping_terminal_writeback_never_loses_success() {
        assert_overlapping_exec_state_mutation_never_loses_success(
            |path| {
                crate::exec_state::writeback_terminal(
                    path,
                    &crate::exec_state::TerminalState {
                        status: Some("complete; tracking issue closed".to_string()),
                        ..crate::exec_state::TerminalState::default()
                    },
                    false,
                )
            },
            |final_text| {
                assert!(
                    final_text.contains("- Status: complete; tracking issue closed"),
                    "successful terminal writeback was lost"
                );
            },
        );
    }

    #[test]
    fn ledger_update_lock_is_released_on_success_dry_run_and_errors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("execution-state.md");
        std::fs::write(&path, SAMPLE).expect("write ledger");
        let lock_path = crate::exec_state::execution_state_mutation_lock_path(&path);
        let assert_released = |state_path: &Path| {
            let lock_path = crate::exec_state::execution_state_mutation_lock_path(state_path);
            drop(
                crate::mutation_lock::OwnedFileLock::acquire(&lock_path)
                    .expect("mutation lock released"),
            );
        };

        update_row(&path, "1.1", "done", "success", None, false).expect("successful update");
        assert!(lock_path.exists(), "stable advisory lock file missing");
        assert_released(&path);

        let before_dry_run = std::fs::read_to_string(&path).expect("read before dry-run");
        update_row(&path, "1.2", "done", "dry-run", None, true).expect("dry-run update");
        assert_released(&path);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read after dry-run"),
            before_dry_run
        );

        let error =
            update_row(&path, "99.9", "done", "missing", None, false).expect_err("missing row");
        assert_eq!(error.code(), "ledger-row-not-found");
        assert_released(&path);

        let missing_path = temp.path().join("missing.md");
        let missing_lock_path =
            crate::exec_state::execution_state_mutation_lock_path(&missing_path);
        let error = update_row(&missing_path, "1.1", "done", "missing", None, false)
            .expect_err("missing ledger");
        assert_eq!(error.code(), "ledger-file-read-failed");
        assert!(
            missing_lock_path.exists(),
            "stable advisory lock file missing after read error"
        );
        assert_released(&missing_path);
    }

    #[test]
    fn reads_title_as_the_task_description_column() {
        let title_dialect = SAMPLE.replace(
            "| ID | Status | Task | Evidence | Notes |",
            "| ID | Status | Title | Evidence | Notes |",
        );

        let rows = read_rows(&title_dialect, Path::new("demo.md")).expect("read Title dialect");

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].task, "Implement `ledger-update`");
    }

    #[test]
    fn reader_and_patcher_preserve_escaped_pipe_cells() {
        let escaped = SAMPLE.replace(
            "| 1.1 | pending | Implement `ledger-update` |  | first row |",
            "| 1.1 | pending | Implement `a \\| b` |  | first \\| row |",
        );

        let rows = read_rows(&escaped, Path::new("demo.md")).expect("read escaped pipes");
        assert_eq!(rows[0].task, "Implement `a \\| b`");
        assert_eq!(rows[0].notes, "first \\| row");

        let outcome = patch_text(
            &escaped,
            Path::new("demo.md"),
            "1.1",
            "done",
            "PR #999",
            None,
        )
        .expect("patch row containing escaped pipes");
        assert!(
            outcome
                .new_text
                .contains("| 1.1 | done | Implement `a \\| b` | PR #999 | first \\| row |"),
            "{}",
            outcome.new_text
        );
    }

    #[test]
    fn rejects_ambiguous_task_description_columns() {
        let ambiguous = SAMPLE
            .replace(
                "| ID | Status | Task | Evidence | Notes |",
                "| ID | Status | Task | Title | Evidence | Notes |",
            )
            .replace(
                "| --- | --- | --- | --- | --- |",
                "| --- | --- | --- | --- | --- | --- |",
            );

        let err = read_rows(&ambiguous, Path::new("demo.md")).expect_err("ambiguous title columns");

        assert_eq!(err.code(), "ledger-table-malformed");
        assert!(err.to_string().contains("ambiguous"), "{err}");
    }

    #[test]
    fn reader_rejects_duplicate_required_columns() {
        for duplicated in ["ID", "Status", "Evidence", "Notes"] {
            let raw = SAMPLE
                .replace(
                    "| ID | Status | Task | Evidence | Notes |",
                    &format!("| ID | Status | Task | Evidence | {duplicated} | Notes |"),
                )
                .replace(
                    "| --- | --- | --- | --- | --- |",
                    "| --- | --- | --- | --- | --- | --- |",
                );

            let err = read_rows(&raw, Path::new("demo.md"))
                .expect_err("duplicate required column must be rejected");
            assert_eq!(err.code(), "ledger-table-malformed");
            assert!(err.to_string().contains("duplicate"), "{duplicated}: {err}");
        }
    }

    #[test]
    fn reader_rejects_multiple_task_ledger_sections() {
        let raw = format!("{SAMPLE}\n{SAMPLE}");

        let err = read_rows(&raw, Path::new("demo.md"))
            .expect_err("multiple Task Ledger sections must be rejected");

        assert_eq!(err.code(), "ledger-table-malformed");
        assert!(err.to_string().contains("multiple"), "{err}");
    }

    #[test]
    fn reader_and_patcher_ignore_fenced_task_ledger_sections() {
        for fence in ["```", "~~~"] {
            let fenced_only = format!("{fence}markdown\n{SAMPLE}\n{fence}\n");
            let err = read_rows(&fenced_only, Path::new("demo.md"))
                .expect_err("fenced Task Ledger must not be structural");
            assert_eq!(err.code(), "ledger-table-malformed", "fence={fence}");

            let raw = format!("{fenced_only}\n{SAMPLE}");
            let rows = read_rows(&raw, Path::new("demo.md"))
                .expect("the real Task Ledger must be selected");
            assert_eq!(rows.len(), 3, "fence={fence}");

            let outcome = patch_text(&raw, Path::new("demo.md"), "1.1", "done", "PR #999", None)
                .expect("patch the real Task Ledger");
            assert_eq!(
                outcome.new_text.matches("PR #999").count(),
                1,
                "fence={fence}"
            );
            assert!(
                outcome
                    .new_text
                    .starts_with(&format!("{fence}markdown\n{SAMPLE}\n{fence}")),
                "fenced example changed for fence={fence}"
            );
        }
    }

    #[test]
    fn reader_accepts_commonmark_indented_task_ledger_heading() {
        for indentation in [" ", "  ", "   "] {
            let raw = SAMPLE.replacen("## Task Ledger", &format!("{indentation}## Task Ledger"), 1);

            let rows = read_rows(&raw, Path::new("demo.md"))
                .expect("one to three spaces remain a structural heading");
            assert_eq!(rows.len(), 3, "indentation={}", indentation.len());
        }
    }

    #[test]
    fn reader_rejects_indented_code_task_ledger_heading() {
        let raw = SAMPLE.replacen("## Task Ledger", "    ## Task Ledger", 1);

        let err = read_rows(&raw, Path::new("demo.md"))
            .expect_err("indented code heading must not be structural");

        assert_eq!(err.code(), "ledger-table-malformed");
    }

    #[test]
    fn reader_rejects_empty_ledger() {
        let raw = "# Demo\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n";

        let err = read_rows(raw, Path::new("demo.md")).expect_err("empty ledger");

        assert_eq!(err.code(), "ledger-table-malformed");
        assert!(err.to_string().contains("at least one"), "{err}");
    }

    #[test]
    fn reader_rejects_duplicate_task_ids_as_ambiguous() {
        let duplicate = SAMPLE.replace(
            "| 1.2 | pending | Implement `ledger-sync` |  | second row |",
            "| 1.1 | pending | Implement `ledger-sync` |  | second row |",
        );

        let err = read_rows(&duplicate, Path::new("demo.md")).expect_err("duplicate");

        assert_eq!(err.code(), "ledger-row-ambiguous");
    }

    #[test]
    fn reader_rejects_empty_task_ids_as_malformed() {
        let empty = SAMPLE.replace(
            "| 1.1 | pending | Implement `ledger-update` |  | first row |",
            "|  | pending | Implement `ledger-update` |  | first row |",
        );

        let err = read_rows(&empty, Path::new("demo.md")).expect_err("empty");

        assert_eq!(err.code(), "ledger-table-malformed");
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn reader_rejects_empty_task_description_and_invalid_status() {
        let empty_task = SAMPLE.replace(
            "| 1.1 | pending | Implement `ledger-update` |  | first row |",
            "| 1.1 | pending |  |  | first row |",
        );
        let empty_status = SAMPLE.replace(
            "| 1.1 | pending | Implement `ledger-update` |  | first row |",
            "| 1.1 |  | Implement `ledger-update` |  | first row |",
        );
        let unknown_status = SAMPLE.replace(
            "| 1.1 | pending | Implement `ledger-update` |  | first row |",
            "| 1.1 | complete | Implement `ledger-update` |  | first row |",
        );

        for raw in [empty_task, empty_status, unknown_status] {
            let err = read_rows(&raw, Path::new("demo.md")).expect_err("invalid row semantics");
            assert_eq!(err.code(), "ledger-table-malformed");
        }
    }

    #[test]
    fn reader_accepts_markdown_separator_alignment_markers() {
        let aligned = SAMPLE.replace(
            "| --- | --- | --- | --- | --- |",
            "| :--- | ---: | :---: | ---- | ----- |",
        );

        let rows = read_rows(&aligned, Path::new("demo.md")).expect("valid aligned separator");

        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn reader_rejects_non_markdown_separator_cells() {
        for invalid in ["-", "--", ":", ":::", "-:-", ":--", "--:", "::---", "---::"] {
            let malformed = SAMPLE.replacen(
                "| --- | --- | --- | --- | --- |",
                &format!("| {invalid} | --- | --- | --- | --- |"),
                1,
            );

            let err = read_rows(&malformed, Path::new("demo.md"))
                .expect_err("invalid separator cell must be rejected");

            assert_eq!(err.code(), "ledger-table-malformed", "cell {invalid:?}");
        }
    }

    #[test]
    fn reader_rejects_separator_or_data_rows_with_wrong_width() {
        let short_separator = SAMPLE.replace(
            "| --- | --- | --- | --- | --- |",
            "| --- | --- | --- | --- |",
        );
        let wide_separator = SAMPLE.replace(
            "| --- | --- | --- | --- | --- |",
            "| --- | --- | --- | --- | --- | --- |",
        );
        let short_row = SAMPLE.replace(
            "| 1.1 | pending | Implement `ledger-update` |  | first row |",
            "| 1.1 | pending | Implement `ledger-update` |  |",
        );
        let wide_row = SAMPLE.replace(
            "| 1.1 | pending | Implement `ledger-update` |  | first row |",
            "| 1.1 | pending | Implement `ledger-update` |  | first row | extra |",
        );
        let missing_trailing_pipe = SAMPLE.replace(
            "| 1.1 | pending | Implement `ledger-update` |  | first row |",
            "| 1.1 | pending | Implement `ledger-update` |  | first row",
        );
        let missing_leading_pipe = SAMPLE.replace(
            "| 1.2 | pending | Implement `ledger-sync` |  | second row |",
            "1.2 | pending | Implement `ledger-sync` |  | second row |",
        );

        for raw in [
            short_separator,
            wide_separator,
            short_row,
            wide_row,
            missing_trailing_pipe,
            missing_leading_pipe,
        ] {
            let err = read_rows(&raw, Path::new("demo.md")).expect_err("wrong-width row");
            assert_eq!(err.code(), "ledger-table-malformed");
        }
    }

    #[test]
    fn patcher_accepts_title_as_the_task_description_column() {
        let title_dialect = SAMPLE.replace(
            "| ID | Status | Task | Evidence | Notes |",
            "| ID | Status | Title | Evidence | Notes |",
        );

        let outcome = patch_text(
            &title_dialect,
            Path::new("demo.md"),
            "1.1",
            "done",
            "PR #999",
            None,
        )
        .expect("patch Title dialect");

        assert!(
            outcome
                .new_text
                .contains("| 1.1 | done | Implement `ledger-update` | PR #999 | first row |")
        );
    }

    #[test]
    fn patcher_rejects_ambiguous_task_description_columns() {
        let ambiguous = SAMPLE
            .replace(
                "| ID | Status | Task | Evidence | Notes |",
                "| ID | Status | Task | Title | Evidence | Notes |",
            )
            .replace(
                "| --- | --- | --- | --- | --- |",
                "| --- | --- | --- | --- | --- | --- |",
            );

        let err = patch_text(
            &ambiguous,
            Path::new("demo.md"),
            "1.1",
            "done",
            "PR #999",
            None,
        )
        .expect_err("ambiguous title columns");

        assert_eq!(err.code(), "ledger-table-malformed");
        assert!(err.to_string().contains("ambiguous"), "{err}");
    }

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
    fn patcher_rejects_duplicate_task_id_as_ambiguous() {
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

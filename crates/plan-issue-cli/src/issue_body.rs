use std::collections::HashMap;

use nils_common::markdown as common_markdown;
use nils_markdown::{Engine, RenderError};
use serde::Serialize;

// Task 2.4 will switch `render::render_plan_issue_body` to call
// `render_task_decomposition_block` instead of stitching the three
// row helpers manually. Until then the template + view types live
// here exercised only by the unit tests, so silence dead_code on
// the new items in the lib target.

/// Template body for the task-decomposition block. Bundled with
/// `include_str!` per Decision 13 in the source document so the
/// asset travels with the binary and no runtime filesystem lookup
/// is required.
#[allow(dead_code)]
const TASK_DECOMPOSITION_TEMPLATE: &str = include_str!("../templates/issue_body.md.tera");
#[allow(dead_code)]
const TASK_DECOMPOSITION_TEMPLATE_NAME: &str = "issue_body";

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
struct TaskTableView<'a> {
    rows: Vec<TaskRowView<'a>>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize)]
struct TaskRowView<'a> {
    task: &'a str,
    summary: &'a str,
    owner: &'a str,
    branch: &'a str,
    worktree: &'a str,
    execution_mode: &'a str,
    pr: &'a str,
    status: &'a str,
    notes: &'a str,
}

impl<'a> From<&'a TaskRow> for TaskRowView<'a> {
    fn from(row: &'a TaskRow) -> Self {
        Self {
            task: &row.task,
            summary: &row.summary,
            owner: &row.owner,
            branch: &row.branch,
            worktree: &row.worktree,
            execution_mode: &row.execution_mode,
            pr: &row.pr,
            status: &row.status,
            notes: &row.notes,
        }
    }
}

/// Render the task-decomposition table block (header row, separator
/// row, and one row per `rows` entry) through the
/// [`nils_markdown::Engine`]. Output is byte-equal to the previous
/// concatenation of [`task_decomposition_header_row`],
/// [`task_decomposition_separator_row`], and
/// [`format_task_decomposition_row`] for the same rows.
///
/// Sprint 2 Task 2.1 ships this function as the migration entry
/// point; Task 2.4 will switch `render::render_plan_issue_body` to
/// call it instead of stitching the three row helpers manually.
#[allow(dead_code)]
pub fn render_task_decomposition_block(rows: &[TaskRow]) -> Result<String, RenderError> {
    let mut engine = Engine::builder().build();
    engine.register_template(
        TASK_DECOMPOSITION_TEMPLATE_NAME,
        TASK_DECOMPOSITION_TEMPLATE,
    )?;
    let view = TaskTableView {
        rows: rows.iter().map(TaskRowView::from).collect(),
    };
    let rendered = engine.render(TASK_DECOMPOSITION_TEMPLATE_NAME, &view)?;
    // Match the `out.join("\n")` contract used by
    // `render::render_plan_issue_body`: the table block lives as
    // elements in a `Vec<String>` and is joined with `\n` later, so
    // it must not carry a trailing newline of its own. The template
    // file ends with the standard EOF newline per workspace
    // convention; strip exactly one trailing `\n` to keep the
    // template human-friendly and the output byte-stable.
    Ok(rendered.strip_suffix('\n').unwrap_or(&rendered).to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub task: String,
    pub summary: String,
    pub owner: String,
    pub branch: String,
    pub worktree: String,
    pub execution_mode: String,
    pub pr: String,
    pub status: String,
    pub notes: String,
    pub line_index: usize,
}

#[derive(Debug, Clone)]
pub struct TaskTable {
    lines: Vec<String>,
    rows: Vec<TaskRow>,
    trailing_newline: bool,
}

pub const TASK_DECOMPOSITION_COLUMNS: [&str; 9] = [
    "Task",
    "Summary",
    "Owner",
    "Branch",
    "Worktree",
    "Execution Mode",
    "PR",
    "Status",
    "Notes",
];

impl TaskTable {
    pub fn rows(&self) -> &[TaskRow] {
        &self.rows
    }

    pub fn rows_mut(&mut self) -> &mut [TaskRow] {
        &mut self.rows
    }

    pub fn sprint_row_indexes(&self, sprint: i32) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(idx, row)| (row_sprint(row) == Some(sprint)).then_some(idx))
            .collect()
    }

    pub fn render(&self) -> String {
        let mut lines = self.lines.clone();
        for row in &self.rows {
            lines[row.line_index] = format_markdown_row(row);
        }

        let mut rendered = lines.join("\n");
        if self.trailing_newline {
            rendered.push('\n');
        }
        rendered
    }
}

pub fn parse_task_table(body: &str) -> Result<TaskTable, String> {
    let trailing_newline = body.ends_with('\n');
    let lines: Vec<String> = body.lines().map(ToString::to_string).collect();

    let section_idx = lines
        .iter()
        .position(|line| line.trim() == "## Task Decomposition")
        .ok_or_else(|| "issue body missing `## Task Decomposition` section".to_string())?;

    let mut header_idx = None;
    for (idx, line) in lines.iter().enumerate().skip(section_idx + 1) {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            break;
        }
        if trimmed.starts_with('|') {
            let cells = split_table_cells(trimmed);
            if normalize_header_cells(&cells) {
                header_idx = Some(idx);
                break;
            }
        }
    }

    let header_idx = header_idx.ok_or_else(|| {
        "task decomposition table header not found or does not match expected columns".to_string()
    })?;

    let separator_idx = header_idx + 1;
    if separator_idx >= lines.len() || !lines[separator_idx].trim().starts_with("| ---") {
        return Err("task decomposition table separator row is missing".to_string());
    }

    let mut rows = Vec::new();
    for (idx, line) in lines.iter().enumerate().skip(separator_idx + 1) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("## ") || !trimmed.starts_with('|') {
            break;
        }

        let cells = split_table_cells(trimmed);
        if cells.len() != TASK_DECOMPOSITION_COLUMNS.len() {
            return Err(format!(
                "task decomposition row has {} columns (expected {}): {}",
                cells.len(),
                TASK_DECOMPOSITION_COLUMNS.len(),
                trimmed
            ));
        }

        rows.push(TaskRow {
            task: cells[0].clone(),
            summary: cells[1].clone(),
            owner: cells[2].clone(),
            branch: cells[3].clone(),
            worktree: cells[4].clone(),
            execution_mode: cells[5].clone(),
            pr: cells[6].clone(),
            status: cells[7].clone(),
            notes: cells[8].clone(),
            line_index: idx,
        });
    }

    if rows.is_empty() {
        return Err("task decomposition table has no task rows".to_string());
    }

    Ok(TaskTable {
        lines,
        rows,
        trailing_newline,
    })
}

pub fn task_decomposition_header_row() -> String {
    format!("| {} |", TASK_DECOMPOSITION_COLUMNS.join(" | "))
}

pub fn task_decomposition_separator_row() -> String {
    let separators = std::iter::repeat_n("---", TASK_DECOMPOSITION_COLUMNS.len())
        .collect::<Vec<_>>()
        .join(" | ");
    format!("| {separators} |")
}

pub fn format_task_decomposition_row(cells: [&str; TASK_DECOMPOSITION_COLUMNS.len()]) -> String {
    let rendered = cells
        .into_iter()
        .map(sanitize_table_value)
        .collect::<Vec<_>>()
        .join(" | ");
    format!("| {rendered} |")
}

pub fn validate_rows(rows: &[TaskRow]) -> Vec<String> {
    let mut errors = Vec::new();

    let mut isolated_branches: HashMap<String, String> = HashMap::new();
    let mut isolated_worktrees: HashMap<String, String> = HashMap::new();
    let mut shared_lane_metadata: HashMap<String, (String, String, String, String)> =
        HashMap::new();
    let mut shared_lane_prs: HashMap<String, (String, String)> = HashMap::new();

    for row in rows {
        let status = row.status.trim().to_ascii_lowercase();
        if !matches!(
            status.as_str(),
            "planned" | "in-progress" | "blocked" | "done"
        ) {
            errors.push(format!(
                "{}: invalid Status `{}`",
                row.task,
                row.status.trim()
            ));
        }

        let execution_mode = row.execution_mode.trim().to_ascii_lowercase();
        if !matches!(
            execution_mode.as_str(),
            "per-sprint" | "pr-isolated" | "pr-shared" | "tbd"
        ) {
            errors.push(format!(
                "{}: invalid Execution Mode `{}`",
                row.task,
                row.execution_mode.trim()
            ));
        }

        if matches!(status.as_str(), "in-progress" | "done") {
            for (label, value) in [
                ("Owner", row.owner.as_str()),
                ("Branch", row.branch.as_str()),
                ("Worktree", row.worktree.as_str()),
                ("Execution Mode", row.execution_mode.as_str()),
                ("PR", row.pr.as_str()),
            ] {
                if is_placeholder(value) {
                    errors.push(format!(
                        "{}: Status `{}` requires non-placeholder {}",
                        row.task, status, label
                    ));
                }
            }
        }

        if !matches!(status.as_str(), "planned" | "blocked") {
            let owner = row.owner.trim().to_ascii_lowercase();
            if !owner.contains("subagent") {
                errors.push(format!(
                    "{}: Owner must include `subagent` for status `{}`",
                    row.task, status
                ));
            }
            if owner.contains("main-agent") {
                errors.push(format!(
                    "{}: Owner cannot reference main-agent for status `{}`",
                    row.task, status
                ));
            }
        }

        if execution_mode == "pr-isolated" {
            if !is_placeholder(&row.branch) {
                let key = row.branch.trim().to_ascii_lowercase();
                if let Some(prev_task) = isolated_branches.insert(key.clone(), row.task.clone()) {
                    errors.push(format!(
                        "{}: pr-isolated Branch `{}` duplicates task {}",
                        row.task,
                        row.branch.trim(),
                        prev_task
                    ));
                }
            }

            if !is_placeholder(&row.worktree) {
                let key = row.worktree.trim().to_ascii_lowercase();
                if let Some(prev_task) = isolated_worktrees.insert(key.clone(), row.task.clone()) {
                    errors.push(format!(
                        "{}: pr-isolated Worktree `{}` duplicates task {}",
                        row.task,
                        row.worktree.trim(),
                        prev_task
                    ));
                }
            }
        }

        if let Some((lane_key, lane_label)) = shared_lane_key(row, &execution_mode)
            && !is_placeholder(&row.owner)
            && !is_placeholder(&row.branch)
            && !is_placeholder(&row.worktree)
        {
            let owner = row.owner.trim().to_string();
            let branch = row.branch.trim().to_string();
            let worktree = row.worktree.trim().to_string();

            if let Some((prev_task, prev_owner, prev_branch, prev_worktree)) =
                shared_lane_metadata.get(&lane_key)
            {
                if prev_owner != &owner || prev_branch != &branch || prev_worktree != &worktree {
                    errors.push(format!(
                        "{}: {} lane `{}` Owner/Branch/Worktree (`{}` / `{}` / `{}`) conflicts with task {} (`{}` / `{}` / `{}`)",
                        row.task,
                        execution_mode,
                        lane_label,
                        owner,
                        branch,
                        worktree,
                        prev_task,
                        prev_owner,
                        prev_branch,
                        prev_worktree
                    ));
                }
            } else {
                shared_lane_metadata.insert(
                    lane_key.clone(),
                    (row.task.clone(), owner, branch, worktree),
                );
            }

            if let Some(pr_key) = canonical_pr_key(&row.pr) {
                let current_pr_display = normalize_pr_display(&row.pr);
                if let Some((prev_task, prev_pr_key)) = shared_lane_prs.get(&lane_key) {
                    if prev_pr_key != &pr_key {
                        errors.push(format!(
                            "{}: {} lane `{}` PR `{}` conflicts with task {} (`{}`)",
                            row.task,
                            execution_mode,
                            lane_label,
                            current_pr_display,
                            prev_task,
                            prev_pr_key
                        ));
                    }
                } else {
                    shared_lane_prs.insert(lane_key, (row.task.clone(), pr_key));
                }
            }
        }
    }

    errors
}

pub fn runtime_pr_sync_lane(row: &TaskRow) -> Option<(String, String)> {
    let execution_mode = row.execution_mode.trim().to_ascii_lowercase();
    shared_lane_key(row, &execution_mode)
}

fn shared_lane_key(row: &TaskRow, execution_mode: &str) -> Option<(String, String)> {
    let sprint = row_sprint(row)
        .map(|value| format!("S{value}"))
        .unwrap_or_else(|| "unknown".to_string());

    match execution_mode {
        "per-sprint" => Some((format!("per-sprint:{sprint}"), sprint)),
        "pr-shared" => {
            let group = note_value(&row.notes, "pr-group")
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "unknown-group".to_string());
            Some((
                format!("pr-shared:{sprint}:{}", group.to_ascii_lowercase()),
                format!("{sprint}/{group}"),
            ))
        }
        _ => None,
    }
}

fn note_value(notes: &str, key: &str) -> Option<String> {
    notes
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(&format!("{key}=")).map(str::to_string))
}

pub fn row_sprint(row: &TaskRow) -> Option<i32> {
    for token in row.notes.split(';').map(str::trim) {
        if let Some(value) = token.strip_prefix("sprint=S")
            && let Ok(number) = value.trim().parse::<i32>()
        {
            return Some(number);
        }
    }

    parse_sprint_from_task_id(&row.task)
}

pub fn parse_pr_number(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if is_placeholder(trimmed) {
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix('#') {
        let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        return digits.parse::<u64>().ok();
    }

    if trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return trimmed.parse::<u64>().ok();
    }

    if let Some((_, tail)) = trimmed.rsplit_once("/pull/") {
        let digits: String = tail.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        return digits.parse::<u64>().ok();
    }

    None
}

fn canonical_pr_key(value: &str) -> Option<String> {
    if is_placeholder(value) {
        return None;
    }

    if let Some(pr) = parse_pr_number(value) {
        return Some(format!("#{pr}"));
    }

    Some(value.trim().to_ascii_lowercase())
}

pub fn normalize_pr_display(value: &str) -> String {
    parse_pr_number(value)
        .map(|pr| format!("#{pr}"))
        .unwrap_or_else(|| value.trim().to_string())
}

pub fn is_placeholder(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return true;
    }

    if matches!(normalized.as_str(), "-" | "none" | "null" | "n/a" | "?") {
        return true;
    }

    normalized.starts_with("tbd")
}

fn normalize_header_cells(cells: &[String]) -> bool {
    if cells.len() != TASK_DECOMPOSITION_COLUMNS.len() {
        return false;
    }

    cells
        .iter()
        .zip(TASK_DECOMPOSITION_COLUMNS)
        .all(|(cell, expected)| cell.trim().eq_ignore_ascii_case(expected))
}

fn split_table_cells(line: &str) -> Vec<String> {
    let mut cells: Vec<String> = line
        .trim()
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect();

    if cells.first().is_some_and(|cell| cell.is_empty()) {
        cells.remove(0);
    }
    if cells.last().is_some_and(|cell| cell.is_empty()) {
        cells.pop();
    }

    cells
}

fn parse_sprint_from_task_id(task: &str) -> Option<i32> {
    let normalized = task.trim();
    if !normalized.starts_with('S') {
        return None;
    }

    let rest = &normalized[1..];
    let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }

    if !rest[digits.len()..].starts_with('T') {
        return None;
    }

    digits.parse::<i32>().ok()
}

fn sanitize_table_value(value: &str) -> String {
    common_markdown::canonicalize_table_cell(value)
}

fn format_markdown_row(row: &TaskRow) -> String {
    format_task_decomposition_row([
        &row.task,
        &row.summary,
        &row.owner,
        &row.branch,
        &row.worktree,
        &row.execution_mode,
        &row.pr,
        &row.status,
        &row.notes,
    ])
}

#[cfg(test)]
mod tests {
    use super::{
        TaskRow, format_task_decomposition_row, is_placeholder, normalize_pr_display,
        parse_pr_number, parse_task_table, render_task_decomposition_block, row_sprint,
        task_decomposition_header_row, task_decomposition_separator_row, validate_rows,
    };

    fn sample_task_rows() -> Vec<TaskRow> {
        vec![
            TaskRow {
                task: "S1T1".to_string(),
                summary: "Add foo".to_string(),
                owner: "subagent".to_string(),
                branch: "issue/s1t1".to_string(),
                worktree: "issue-s1t1".to_string(),
                execution_mode: "pr-isolated".to_string(),
                pr: "#101".to_string(),
                status: "done".to_string(),
                notes: "-".to_string(),
                line_index: 0,
            },
            TaskRow {
                task: "S1T2".to_string(),
                summary: "Pipe|in|summary".to_string(),
                owner: "main".to_string(),
                branch: "issue/s1t2".to_string(),
                worktree: "issue-s1t2".to_string(),
                execution_mode: "per-sprint".to_string(),
                pr: "TBD".to_string(),
                status: "planned".to_string(),
                notes: "sprint=S1; group=A".to_string(),
                line_index: 1,
            },
            TaskRow {
                task: "S2T1".to_string(),
                summary: "Multi\nline\nnotes".to_string(),
                owner: "subagent".to_string(),
                branch: "issue/s2t1".to_string(),
                worktree: "issue-s2t1".to_string(),
                execution_mode: "pr-shared".to_string(),
                pr: "#202".to_string(),
                status: "in-progress".to_string(),
                notes: "Pipe|here".to_string(),
                line_index: 2,
            },
        ]
    }

    fn baseline_via_helpers(rows: &[TaskRow]) -> String {
        let mut out: Vec<String> = vec![
            task_decomposition_header_row(),
            task_decomposition_separator_row(),
        ];
        for row in rows {
            out.push(format_task_decomposition_row([
                &row.task,
                &row.summary,
                &row.owner,
                &row.branch,
                &row.worktree,
                &row.execution_mode,
                &row.pr,
                &row.status,
                &row.notes,
            ]));
        }
        out.join("\n")
    }

    #[test]
    fn render_task_decomposition_block_matches_helper_composition_for_sample_rows() {
        let rows = sample_task_rows();
        let baseline = baseline_via_helpers(&rows);
        let rendered = render_task_decomposition_block(&rows).expect("render block");
        pretty_assertions::assert_eq!(baseline, rendered);
    }

    #[test]
    fn render_task_decomposition_block_matches_helper_composition_for_empty_rows() {
        let baseline = baseline_via_helpers(&[]);
        let rendered = render_task_decomposition_block(&[]).expect("render block");
        pretty_assertions::assert_eq!(baseline, rendered);
    }

    #[test]
    fn render_task_decomposition_block_matches_committed_golden() {
        let rows = sample_task_rows();
        let fixture = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/golden/issue_body/task_table.golden.md"
        ))
        .expect("read golden fixture");
        let rendered = render_task_decomposition_block(&rows).expect("render block");
        pretty_assertions::assert_eq!(fixture, rendered);
    }

    #[test]
    fn parse_task_table_extracts_rows() {
        let body = "## Task Decomposition\n\n| Task | Summary | Owner | Branch | Worktree | Execution Mode | PR | Status | Notes |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- |\n| S4T1 | A | subagent | issue/s4 | issue-s4 | per-sprint | #1 | done | sprint=S4 |\n";
        let table = parse_task_table(body).expect("parse table");
        assert_eq!(table.rows().len(), 1);
        assert_eq!(table.rows()[0].task, "S4T1");
    }

    #[test]
    fn row_sprint_prefers_notes_then_task_id() {
        let row = TaskRow {
            task: "S2T1".to_string(),
            summary: String::new(),
            owner: String::new(),
            branch: String::new(),
            worktree: String::new(),
            execution_mode: String::new(),
            pr: String::new(),
            status: String::new(),
            notes: "x=1; sprint=S9".to_string(),
            line_index: 0,
        };
        assert_eq!(row_sprint(&row), Some(9));
    }

    #[test]
    fn placeholder_and_pr_normalization_cover_common_cases() {
        assert!(is_placeholder("TBD (per-sprint)"));
        assert_eq!(parse_pr_number("#221"), Some(221));
        assert_eq!(
            parse_pr_number("https://github.com/sympoies/nils-cli/pull/221"),
            Some(221)
        );
        assert_eq!(
            normalize_pr_display("https://github.com/x/y/pull/17"),
            "#17"
        );
    }

    #[test]
    fn validate_rows_flags_non_subagent_owner_for_done_rows() {
        let rows = [TaskRow {
            task: "S4T1".to_string(),
            summary: "x".to_string(),
            owner: "main-agent".to_string(),
            branch: "issue/s4".to_string(),
            worktree: "issue-s4".to_string(),
            execution_mode: "per-sprint".to_string(),
            pr: "#1".to_string(),
            status: "done".to_string(),
            notes: "sprint=S4".to_string(),
            line_index: 0,
        }];
        let errs = validate_rows(&rows);
        assert!(!errs.is_empty());
    }

    #[test]
    fn validate_rows_rejects_per_task_execution_mode() {
        let rows = [TaskRow {
            task: "S4T1".to_string(),
            summary: "x".to_string(),
            owner: "subagent-s4-t1".to_string(),
            branch: "issue/s4-t1".to_string(),
            worktree: "issue-s4-t1".to_string(),
            execution_mode: "per-task".to_string(),
            pr: "#1".to_string(),
            status: "in-progress".to_string(),
            notes: "sprint=S4".to_string(),
            line_index: 0,
        }];

        let errs = validate_rows(&rows);
        assert_eq!(errs, vec!["S4T1: invalid Execution Mode `per-task`"]);
    }

    #[test]
    fn validate_rows_requires_unique_branch_and_worktree_for_pr_isolated_rows() {
        let rows = [
            TaskRow {
                task: "S4T1".to_string(),
                summary: "x".to_string(),
                owner: "subagent-s4-t1".to_string(),
                branch: "issue/s4-shared".to_string(),
                worktree: "issue-s4-shared".to_string(),
                execution_mode: "pr-isolated".to_string(),
                pr: "#1".to_string(),
                status: "in-progress".to_string(),
                notes: "sprint=S4".to_string(),
                line_index: 0,
            },
            TaskRow {
                task: "S4T2".to_string(),
                summary: "x".to_string(),
                owner: "subagent-s4-t2".to_string(),
                branch: "issue/s4-shared".to_string(),
                worktree: "issue-s4-shared".to_string(),
                execution_mode: "pr-isolated".to_string(),
                pr: "#2".to_string(),
                status: "in-progress".to_string(),
                notes: "sprint=S4".to_string(),
                line_index: 1,
            },
        ];

        let errs = validate_rows(&rows);
        assert_eq!(
            errs,
            vec![
                "S4T2: pr-isolated Branch `issue/s4-shared` duplicates task S4T1",
                "S4T2: pr-isolated Worktree `issue-s4-shared` duplicates task S4T1",
            ]
        );
    }

    #[test]
    fn validate_rows_detects_conflicting_shared_lane_metadata() {
        let rows = [
            TaskRow {
                task: "S4T1".to_string(),
                summary: "x".to_string(),
                owner: "subagent-s4-lane-a".to_string(),
                branch: "issue/s4-shared-a".to_string(),
                worktree: "issue-s4-shared-a".to_string(),
                execution_mode: "pr-shared".to_string(),
                pr: "#1".to_string(),
                status: "in-progress".to_string(),
                notes: "sprint=S4; pr-group=s4-auto-g1".to_string(),
                line_index: 0,
            },
            TaskRow {
                task: "S4T2".to_string(),
                summary: "x".to_string(),
                owner: "subagent-s4-lane-b".to_string(),
                branch: "issue/s4-shared-b".to_string(),
                worktree: "issue-s4-shared-b".to_string(),
                execution_mode: "pr-shared".to_string(),
                pr: "#1".to_string(),
                status: "in-progress".to_string(),
                notes: "sprint=S4; pr-group=s4-auto-g1".to_string(),
                line_index: 1,
            },
        ];

        let errs = validate_rows(&rows);
        assert!(
            errs.iter()
                .any(|err| err.contains("S4T2: pr-shared lane `S4/s4-auto-g1`")),
            "{errs:?}"
        );
    }

    #[test]
    fn validate_rows_detects_conflicting_per_sprint_lane_metadata() {
        let rows = [
            TaskRow {
                task: "S5T1".to_string(),
                summary: "x".to_string(),
                owner: "subagent-s5-lane-a".to_string(),
                branch: "issue/s5-shared-a".to_string(),
                worktree: "issue-s5-shared-a".to_string(),
                execution_mode: "per-sprint".to_string(),
                pr: "#5".to_string(),
                status: "in-progress".to_string(),
                notes: "sprint=S5; pr-group=s5-auto-g1".to_string(),
                line_index: 0,
            },
            TaskRow {
                task: "S5T2".to_string(),
                summary: "x".to_string(),
                owner: "subagent-s5-lane-b".to_string(),
                branch: "issue/s5-shared-b".to_string(),
                worktree: "issue-s5-shared-b".to_string(),
                execution_mode: "per-sprint".to_string(),
                pr: "#5".to_string(),
                status: "in-progress".to_string(),
                notes: "sprint=S5; pr-group=s5-auto-g1".to_string(),
                line_index: 1,
            },
        ];

        let errs = validate_rows(&rows);
        assert!(
            errs.iter()
                .any(|err| err.contains("S5T2: per-sprint lane `S5`")),
            "{errs:?}"
        );
    }

    #[test]
    fn validate_rows_detects_conflicting_shared_lane_pr_values() {
        let rows = [
            TaskRow {
                task: "S5T1".to_string(),
                summary: "x".to_string(),
                owner: "subagent-s5-lane".to_string(),
                branch: "issue/s5-shared".to_string(),
                worktree: "issue-s5-shared".to_string(),
                execution_mode: "pr-shared".to_string(),
                pr: "#5".to_string(),
                status: "in-progress".to_string(),
                notes: "sprint=S5; pr-group=s5-core".to_string(),
                line_index: 0,
            },
            TaskRow {
                task: "S5T2".to_string(),
                summary: "x".to_string(),
                owner: "subagent-s5-lane".to_string(),
                branch: "issue/s5-shared".to_string(),
                worktree: "issue-s5-shared".to_string(),
                execution_mode: "pr-shared".to_string(),
                pr: "#6".to_string(),
                status: "in-progress".to_string(),
                notes: "sprint=S5; pr-group=s5-core".to_string(),
                line_index: 1,
            },
        ];

        let errs = validate_rows(&rows);
        assert!(
            errs.iter()
                .any(|err| err.contains("S5T2: pr-shared lane `S5/s5-core` PR `#6` conflicts")),
            "{errs:?}"
        );
    }

    #[test]
    fn validate_rows_accepts_equivalent_pr_references_in_shared_lane() {
        let rows = [
            TaskRow {
                task: "S5T1".to_string(),
                summary: "x".to_string(),
                owner: "subagent-s5-lane".to_string(),
                branch: "issue/s5-shared".to_string(),
                worktree: "issue-s5-shared".to_string(),
                execution_mode: "per-sprint".to_string(),
                pr: "#5".to_string(),
                status: "in-progress".to_string(),
                notes: "sprint=S5".to_string(),
                line_index: 0,
            },
            TaskRow {
                task: "S5T2".to_string(),
                summary: "x".to_string(),
                owner: "subagent-s5-lane".to_string(),
                branch: "issue/s5-shared".to_string(),
                worktree: "issue-s5-shared".to_string(),
                execution_mode: "per-sprint".to_string(),
                pr: "https://github.com/x/y/pull/5".to_string(),
                status: "in-progress".to_string(),
                notes: "sprint=S5".to_string(),
                line_index: 1,
            },
        ];

        let errs = validate_rows(&rows);
        assert!(
            !errs
                .iter()
                .any(|err| err.contains("PR") && err.contains("conflicts")),
            "{errs:?}"
        );
    }

    #[test]
    fn task_table_schema_helpers_and_parser_stay_aligned() {
        let body = format!(
            "## Task Decomposition\n\n{}\n{}\n{}\n",
            task_decomposition_header_row(),
            task_decomposition_separator_row(),
            format_task_decomposition_row([
                "S4T1",
                "A | B",
                "subagent",
                "issue/s4",
                "issue-s4",
                "per-sprint",
                "#1",
                "done",
                "sprint=S4"
            ])
        );

        let table = parse_task_table(&body).expect("parse table");
        assert_eq!(table.rows().len(), 1);
        assert_eq!(table.rows()[0].summary, "A / B");
    }
}

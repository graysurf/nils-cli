use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use nils_markdown::Engine;
use serde::Serialize;

use crate::commands::SplitStrategy;
use crate::issue_body;
use crate::task_spec::{
    TaskSpecRow, execution_mode_by_task, runtime_lane_metadata_by_task, state_dir,
};
use nils_common::fs as common_fs;
use nils_common::git as common_git;
use nils_common::markdown as common_markdown;

const PLAN_ISSUE_BODY_TEMPLATE: &str = include_str!("../templates/render/plan_issue_body.md.tera");
const PLAN_ISSUE_BODY_TEMPLATE_NAME: &str = "render_plan_issue_body";

const SPRINT_COMMENT_TEMPLATE: &str = include_str!("../templates/render/sprint_comment.md.tera");
const SPRINT_COMMENT_TEMPLATE_NAME: &str = "render_sprint_comment";

#[derive(Debug, Serialize)]
struct PlanIssueBodyView<'a> {
    pre_table: String,
    task_table_block: String,
    plan_file_display: &'a str,
}

#[derive(Debug, Serialize)]
struct SprintCommentView<'a> {
    heading: String,
    sprint: i32,
    sprint_name: &'a str,
    task_count: usize,
    lead: &'static str,
    mode: &'static str,
    approval_comment_url: Option<&'a str>,
    sprint_section: Option<String>,
    note_text: Option<String>,
    task_rows: Vec<SprintTaskRowView<'a>>,
}

#[derive(Debug, Serialize)]
struct SprintTaskRowView<'a> {
    task: &'a str,
    summary: String,
    third_col: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SprintCommentMode {
    Start,
    Ready,
    Accepted,
}

#[derive(Debug, Clone)]
pub struct SprintCommentInput<'a> {
    pub mode: SprintCommentMode,
    pub plan_file: &'a Path,
    pub sprint: i32,
    pub sprint_name: &'a str,
    pub rows: &'a [TaskSpecRow],
    pub strategy: SplitStrategy,
    pub note_text: Option<&'a str>,
    pub approval_comment_url: Option<&'a str>,
    pub issue_body_text: Option<&'a str>,
}

pub fn default_plan_issue_body_path(plan_file: &Path) -> PathBuf {
    let plan_stem = plan_file
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("plan")
        .to_string();
    state_dir()
        .join("out")
        .join("plan-issue-delivery")
        .join(format!("{plan_stem}-plan-issue-body.md"))
}

pub fn default_sprint_comment_path(
    plan_file: &Path,
    sprint: i32,
    mode: SprintCommentMode,
) -> PathBuf {
    let plan_stem = plan_file
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("plan")
        .to_string();
    let mode_label = match mode {
        SprintCommentMode::Start => "start",
        SprintCommentMode::Ready => "ready",
        SprintCommentMode::Accepted => "accepted",
    };

    state_dir()
        .join("out")
        .join("plan-issue-delivery")
        .join(format!(
            "{plan_stem}-sprint-{sprint}-{mode_label}-comment.md"
        ))
}

pub fn render_plan_issue_body(
    plan_file: &Path,
    plan_file_display: &str,
    plan_title: &str,
    rows: &[TaskSpecRow],
    strategy: SplitStrategy,
) -> String {
    let fallback_title = if plan_title.trim().is_empty() {
        Path::new(plan_file_display)
            .file_stem()
            .and_then(|v| v.to_str())
            .unwrap_or("Plan")
            .to_string()
    } else {
        plan_title.trim().to_string()
    };

    let mut header_lines = load_pre_sprint_plan_lines(plan_file)
        .filter(|lines| !lines.is_empty())
        .unwrap_or_else(|| vec![format!("# {fallback_title}")]);
    while header_lines
        .last()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        header_lines.pop();
    }
    let pre_table = header_lines.join("\n");

    let runtime_lane_metadata = runtime_lane_metadata_by_task(rows, strategy);
    let task_rows: Vec<issue_body::TaskRow> = rows
        .iter()
        .map(|row| {
            let lane = runtime_lane_metadata.get(&row.task_id);
            let owner = lane
                .map(|metadata| metadata.owner.clone())
                .unwrap_or_else(|| row.owner.clone());
            let branch = lane
                .map(|metadata| metadata.branch.clone())
                .unwrap_or_else(|| row.branch.clone());
            let worktree = lane
                .map(|metadata| metadata.worktree.clone())
                .unwrap_or_else(|| row.worktree.clone());
            let execution_mode = lane
                .map(|metadata| metadata.execution_mode.clone())
                .unwrap_or_else(|| "pr-isolated".to_string());
            let notes = lane
                .map(|metadata| metadata.notes.trim().to_string())
                .unwrap_or_else(|| row.notes.trim().to_string());
            let notes = common_markdown::canonicalize_table_cell(&notes);
            let notes = if notes.trim().is_empty() {
                "-".to_string()
            } else {
                notes
            };
            issue_body::TaskRow {
                task: row.task_id.clone(),
                summary: row.summary.clone(),
                owner,
                branch,
                worktree,
                execution_mode,
                pr: "TBD".to_string(),
                status: "planned".to_string(),
                notes,
                line_index: 0,
            }
        })
        .collect();

    let task_table_block = issue_body::render_task_decomposition_block(&task_rows)
        .expect("task-decomposition block renders");

    let view = PlanIssueBodyView {
        pre_table,
        task_table_block,
        plan_file_display,
    };

    let mut engine = Engine::builder().build();
    engine
        .register_template(PLAN_ISSUE_BODY_TEMPLATE_NAME, PLAN_ISSUE_BODY_TEMPLATE)
        .expect("plan_issue_body template registers");
    engine
        .render(PLAN_ISSUE_BODY_TEMPLATE_NAME, &view)
        .expect("plan_issue_body template renders")
}

fn load_pre_sprint_plan_lines(plan_file: &Path) -> Option<Vec<String>> {
    let repo_root = detect_repo_root();
    let resolved = resolve_repo_relative(&repo_root, plan_file);
    let text = fs::read_to_string(&resolved).ok()?;
    let lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();
    if lines.is_empty() {
        return None;
    }

    let mut preface_end = lines.len();
    for (idx, line) in lines.iter().enumerate() {
        if let Some((level, heading)) = parse_heading(line)
            && level == 2
            && parse_sprint_heading_number(&heading) == Some(1)
        {
            preface_end = idx;
            break;
        }
    }

    Some(lines.into_iter().take(preface_end).collect())
}

fn parse_sprint_heading_number(heading: &str) -> Option<i32> {
    let normalized = heading.trim().to_ascii_lowercase();
    let rest = normalized.strip_prefix("sprint ")?;
    let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<i32>().ok()
}

fn parse_heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim();
    if !trimmed.starts_with('#') {
        return None;
    }

    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }

    let heading = trimmed[level..].trim();
    if heading.is_empty() {
        None
    } else {
        Some((level, heading.to_string()))
    }
}

pub fn render_sprint_comment(input: SprintCommentInput<'_>) -> Result<String, String> {
    let SprintCommentInput {
        mode,
        plan_file,
        sprint,
        sprint_name,
        rows,
        strategy,
        note_text,
        approval_comment_url,
        issue_body_text,
    } = input;

    if rows.is_empty() {
        return Err("task spec contains no rows".to_string());
    }

    let execution_modes = execution_mode_by_task(rows, strategy);

    let issue_pr_values = issue_body_text
        .map(parse_issue_pr_values)
        .unwrap_or_default();

    let (heading, lead, mode_token) = match mode {
        SprintCommentMode::Start => (
            format!("## Sprint {sprint} Start"),
            "Main-agent starts this sprint on the plan issue and dispatches implementation to subagents.",
            "start",
        ),
        SprintCommentMode::Ready => (
            format!("## Sprint {sprint} Ready for Review"),
            "Main-agent requests sprint-level review before merge/acceptance on the plan issue (the issue remains open).",
            "ready",
        ),
        SprintCommentMode::Accepted => (
            format!("## Sprint {sprint} Accepted"),
            "Main-agent records sprint acceptance after merge gate passes and sprint rows are synced to done (issue remains open for remaining sprints).",
            "accepted",
        ),
    };

    let approval_url = approval_comment_url
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty());

    let task_rows: Vec<SprintTaskRowView<'_>> = rows
        .iter()
        .map(|row| {
            let summary = if row.summary.is_empty() {
                "-".to_string()
            } else {
                row.summary.clone()
            };
            let third_col = match mode {
                SprintCommentMode::Start => execution_modes
                    .get(&row.task_id)
                    .cloned()
                    .unwrap_or_else(|| "pr-isolated".to_string()),
                SprintCommentMode::Ready | SprintCommentMode::Accepted => {
                    let mut pr_value = issue_pr_values
                        .get(&row.task_id)
                        .map(|v| normalize_pr_display(v))
                        .unwrap_or_default();
                    if pr_value.is_empty() {
                        let execution_mode = execution_modes
                            .get(&row.task_id)
                            .map(String::as_str)
                            .unwrap_or("pr-isolated");
                        pr_value = if execution_mode == "per-sprint" {
                            "TBD (per-sprint)".to_string()
                        } else {
                            format!("TBD (group:{})", row.pr_group)
                        };
                    }
                    pr_value
                }
            };
            SprintTaskRowView {
                task: &row.task_id,
                summary,
                third_col,
            }
        })
        .collect();

    let sprint_section = if mode == SprintCommentMode::Start {
        let section = extract_sprint_section(plan_file, sprint)?;
        if section.is_empty() {
            None
        } else {
            Some(section)
        }
    } else {
        None
    };

    let note_text_owned = note_text
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(str::to_string);

    let view = SprintCommentView {
        heading,
        sprint,
        sprint_name,
        task_count: rows.len(),
        lead,
        mode: mode_token,
        approval_comment_url: approval_url,
        sprint_section,
        note_text: note_text_owned,
        task_rows,
    };

    let mut engine = Engine::builder().build();
    engine
        .register_template(SPRINT_COMMENT_TEMPLATE_NAME, SPRINT_COMMENT_TEMPLATE)
        .map_err(|err| format!("sprint_comment template register failed: {err}"))?;
    engine
        .render(SPRINT_COMMENT_TEMPLATE_NAME, &view)
        .map_err(|err| format!("sprint_comment template render failed: {err}"))
}

pub fn write_rendered(path: &Path, content: &str) -> Result<(), String> {
    common_fs::write_text(path, content).map_err(|err| match err {
        common_fs::WriteTextError::CreateParentDir { path, source } => {
            format!(
                "failed to create output directory {}: {source}",
                path.display()
            )
        }
        common_fs::WriteTextError::WriteFile { source, .. } => {
            format!("failed to write {}: {source}", path.display())
        }
    })
}

fn parse_issue_pr_values(issue_body_text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let lines: Vec<&str> = issue_body_text.lines().collect();

    let Some((start, end)) = section_bounds(&lines, "## Task Decomposition") else {
        return out;
    };

    let table_lines: Vec<&str> = lines[start..end]
        .iter()
        .copied()
        .filter(|line| line.trim().starts_with('|'))
        .collect();

    if table_lines.len() < 3 {
        return out;
    }

    let headers = parse_markdown_row(table_lines[0]);
    let Some(task_idx) = headers.iter().position(|h| h == "Task") else {
        return out;
    };
    let Some(pr_idx) = headers.iter().position(|h| h == "PR") else {
        return out;
    };

    for line in table_lines.iter().skip(2) {
        let cells = parse_markdown_row(line);
        if cells.len() != headers.len() {
            continue;
        }
        let task = cells[task_idx].trim();
        let pr = cells[pr_idx].trim();
        if task.is_empty() {
            continue;
        }
        let normalized = normalize_pr_display(pr);
        if !normalized.is_empty() {
            out.insert(task.to_string(), normalized);
        }
    }

    out
}

fn section_bounds(lines: &[&str], heading: &str) -> Option<(usize, usize)> {
    let mut start = None;
    for (idx, line) in lines.iter().enumerate() {
        if line.trim() == heading {
            start = Some(idx + 1);
            break;
        }
    }
    let start = start?;

    let mut end = lines.len();
    for (idx, line) in lines.iter().enumerate().skip(start) {
        if line.starts_with("## ") {
            end = idx;
            break;
        }
    }

    Some((start, end))
}

fn parse_markdown_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
        return Vec::new();
    }
    trimmed[1..trimmed.len() - 1]
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

fn is_placeholder(value: &str) -> bool {
    let token = value.trim().to_ascii_lowercase();
    if matches!(
        token.as_str(),
        "" | "-" | "tbd" | "none" | "n/a" | "na" | "..."
    ) {
        return true;
    }
    if token.starts_with("tbd") {
        return true;
    }
    if token.starts_with('<') && token.ends_with('>') {
        return true;
    }
    token.contains("task ids")
}

fn parse_digits(token: &str) -> Option<String> {
    if token.is_empty() || !token.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(token.to_string())
}

fn normalize_pr_display(value: &str) -> String {
    let token = value.trim();
    if is_placeholder(token) {
        return String::new();
    }

    if let Some(rest) = token.strip_prefix('#')
        && let Some(num) = parse_digits(rest)
    {
        return format!("#{num}");
    }

    if let Some(rest) = token.to_ascii_lowercase().strip_prefix("pr#")
        && let Some(num) = parse_digits(rest)
    {
        return format!("#{num}");
    }

    if let Some((_, tail)) = token.rsplit_once('#')
        && let Some(num) = parse_digits(tail)
        && token.contains('/')
    {
        return format!("#{num}");
    }

    if let Some(idx) = token.to_ascii_lowercase().find("/pull/") {
        let after = &token[idx + "/pull/".len()..];
        let number: String = after.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        if let Some(num) = parse_digits(&number) {
            return format!("#{num}");
        }
    }

    token.to_string()
}

fn extract_sprint_section(plan_file: &Path, sprint: i32) -> Result<String, String> {
    let repo_root = detect_repo_root();
    let resolved = resolve_repo_relative(&repo_root, plan_file);
    let text = std::fs::read_to_string(&resolved).map_err(|err| {
        format!(
            "failed to read plan file {}: {err}",
            plan_file.to_string_lossy()
        )
    })?;
    let lines: Vec<&str> = text.lines().collect();

    let target_prefix = format!("## Sprint {sprint}");
    let mut start = None;
    for (idx, line) in lines.iter().enumerate() {
        if line.trim().starts_with(&target_prefix) {
            start = Some(idx);
            break;
        }
    }

    let Some(start_idx) = start else {
        return Ok(String::new());
    };

    let mut end_idx = lines.len();
    for (idx, line) in lines.iter().enumerate().skip(start_idx + 1) {
        if line.starts_with("## ") {
            end_idx = idx;
            break;
        }
    }

    Ok(lines[start_idx..end_idx].join("\n").trim().to_string())
}

fn detect_repo_root() -> PathBuf {
    common_git::repo_root_or_cwd()
}

fn resolve_repo_relative(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    repo_root.join(path)
}

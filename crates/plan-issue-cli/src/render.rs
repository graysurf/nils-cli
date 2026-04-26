use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::commands::SplitStrategy;
use crate::issue_body;
use crate::task_spec::{
    TaskSpecRow, execution_mode_by_task, runtime_lane_metadata_by_task, state_dir,
};
use nils_common::fs as common_fs;
use nils_common::git as common_git;
use nils_common::markdown as common_markdown;

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

    let mut out: Vec<String> = load_pre_sprint_plan_lines(plan_file)
        .filter(|lines| !lines.is_empty())
        .unwrap_or_else(|| vec![format!("# {fallback_title}")]);

    if out.last().is_some_and(|line| !line.trim().is_empty()) {
        out.push(String::new());
    }

    out.extend([
        "## Task Decomposition".to_string(),
        String::new(),
        issue_body::task_decomposition_header_row(),
        issue_body::task_decomposition_separator_row(),
    ]);

    let runtime_lane_metadata = runtime_lane_metadata_by_task(rows, strategy);

    for row in rows {
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
        out.push(issue_body::format_task_decomposition_row([
            &row.task_id,
            &row.summary,
            &owner,
            &branch,
            &worktree,
            &execution_mode,
            "TBD",
            "planned",
            &notes,
        ]));
    }

    out.extend([
        String::new(),
        "## Consistency Rules".to_string(),
        String::new(),
        "- `Status` must be one of: `planned`, `in-progress`, `blocked`, `done`.".to_string(),
        "- `Status` = `in-progress` or `done` requires non-`TBD` execution metadata (`Owner`, `Branch`, `Worktree`, `Execution Mode`, `PR`).".to_string(),
        "- `Owner` must be a subagent identifier (contains `subagent`) once the task is assigned; `main-agent` ownership is invalid for implementation tasks.".to_string(),
        "- `Execution Mode` should be one of: `per-sprint`, `pr-isolated`, `pr-shared` (or `TBD` before assignment).".to_string(),
        "- `Branch` and `Worktree` uniqueness is enforced only for rows using `Execution Mode = pr-isolated`.".to_string(),
        String::new(),
        "## Risks / Uncertainties".to_string(),
        String::new(),
        "- Sprint approvals may be recorded before final close; issue stays open until final plan acceptance.".to_string(),
        "- Close gate fails if task statuses or PR merge states in the issue body are incomplete.".to_string(),
        String::new(),
        "## Evidence".to_string(),
        String::new(),
        format!("- Plan source: `{plan_file_display}`"),
        "- Sprint approvals: issue comments (one comment per accepted sprint)".to_string(),
        "- Final approval: issue/pull comment URL passed to `close-plan`".to_string(),
    ]);

    format!("{}\n", out.join("\n"))
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

    let mut out: Vec<String> = Vec::new();
    let (heading, lead) = match mode {
        SprintCommentMode::Start => (
            format!("## Sprint {sprint} Start"),
            "Main-agent starts this sprint on the plan issue and dispatches implementation to subagents.",
        ),
        SprintCommentMode::Ready => (
            format!("## Sprint {sprint} Ready for Review"),
            "Main-agent requests sprint-level review before merge/acceptance on the plan issue (the issue remains open).",
        ),
        SprintCommentMode::Accepted => (
            format!("## Sprint {sprint} Accepted"),
            "Main-agent records sprint acceptance after merge gate passes and sprint rows are synced to done (issue remains open for remaining sprints).",
        ),
    };

    out.push(heading);
    out.push(String::new());
    out.push(format!("- Sprint: {sprint} ({sprint_name})"));
    out.push(format!("- Tasks in sprint: {}", rows.len()));
    out.push(format!("- Note: {lead}"));
    if mode == SprintCommentMode::Start {
        out.push(
            "- Execution Mode comes from current Task Decomposition for each sprint task."
                .to_string(),
        );
    } else {
        out.push(
            "- PR values come from current Task Decomposition; unresolved tasks remain `TBD` until PRs are linked."
                .to_string(),
        );
    }

    if let Some(url) = approval_comment_url {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            out.push(format!("- Approval comment URL: {trimmed}"));
        }
    }

    out.push(String::new());
    match mode {
        SprintCommentMode::Start => {
            out.push("| Task | Summary | Execution Mode |".to_string());
            out.push("| --- | --- | --- |".to_string());
            for row in rows {
                let execution_mode = execution_modes
                    .get(&row.task_id)
                    .map(String::as_str)
                    .unwrap_or("pr-isolated");
                out.push(format!(
                    "| {} | {} | {} |",
                    row.task_id,
                    if row.summary.is_empty() {
                        "-"
                    } else {
                        &row.summary
                    },
                    execution_mode
                ));
            }

            let sprint_section = extract_sprint_section(plan_file, sprint)?;
            if !sprint_section.is_empty() {
                out.push(String::new());
                out.push(sprint_section);
            }
        }
        SprintCommentMode::Ready | SprintCommentMode::Accepted => {
            out.push("| Task | Summary | PR |".to_string());
            out.push("| --- | --- | --- |".to_string());
            for row in rows {
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
                out.push(format!(
                    "| {} | {} | {} |",
                    row.task_id,
                    if row.summary.is_empty() {
                        "-"
                    } else {
                        &row.summary
                    },
                    pr_value
                ));
            }
        }
    }

    if let Some(note) = note_text {
        let trimmed = note.trim();
        if !trimmed.is_empty() {
            out.push(String::new());
            out.push("## Main-Agent Notes".to_string());
            out.push(String::new());
            out.push(trimmed.to_string());
        }
    }

    Ok(format!("{}\n", out.join("\n")))
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

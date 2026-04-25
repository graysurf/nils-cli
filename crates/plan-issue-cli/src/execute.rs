use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nils_common::git as common_git;
use nils_common::markdown as common_markdown;
use plan_tooling::parse::parse_plan_with_display;
use serde_json::{Value, json};

use crate::cli::Cli;
use crate::commands::build::{BuildPlanTaskSpecArgs, BuildTaskSpecArgs};
use crate::commands::plan::{
    CleanupWorktreesArgs, ClosePlanArgs, LinkPrArgs, LinkPrStatus, ReadyPlanArgs, StartPlanArgs,
    StatusPlanArgs,
};
use crate::commands::sprint::{
    AcceptSprintArgs, MultiSprintGuideArgs, ReadySprintArgs, StartSprintArgs,
};
use crate::commands::{Command as CliCommand, SplitStrategy, SummaryArgs};
use crate::dispatch_record::{self, DispatchRecord};
use crate::github::{GhCliAdapter, GitHubAdapter};
use crate::issue_body::{self, TaskRow};
use crate::render::{self, SprintCommentInput, SprintCommentMode};
use crate::runtime_layout::{self, IssueRoot, SprintRoot};
use crate::task_spec::{self, TaskSpecBuildOptions, TaskSpecRow, TaskSpecScope};
use crate::{BinaryFlavor, CommandError};

const LOCAL_ISSUE_PLACEHOLDER: u64 = 999;

pub fn execute(binary: BinaryFlavor, cli: &Cli) -> Result<Value, CommandError> {
    match &cli.command {
        CliCommand::BuildTaskSpec(args) => run_build_task_spec(args),
        CliCommand::BuildPlanTaskSpec(args) => run_build_plan_task_spec(args),
        CliCommand::StartPlan(args) => {
            run_start_plan(binary, cli.dry_run, cli.force, cli.repo.as_deref(), args)
        }
        CliCommand::StatusPlan(args) => {
            run_status_plan(binary, cli.dry_run, cli.force, cli.repo.as_deref(), args)
        }
        CliCommand::LinkPr(args) => {
            run_link_pr(binary, cli.dry_run, cli.force, cli.repo.as_deref(), args)
        }
        CliCommand::ReadyPlan(args) => {
            run_ready_plan(binary, cli.dry_run, cli.force, cli.repo.as_deref(), args)
        }
        CliCommand::ClosePlan(args) => {
            run_close_plan(binary, cli.dry_run, cli.force, cli.repo.as_deref(), args)
        }
        CliCommand::CleanupWorktrees(args) => {
            run_cleanup_worktrees(binary, cli.dry_run, cli.force, cli.repo.as_deref(), args)
        }
        CliCommand::StartSprint(args) => {
            run_start_sprint(binary, cli.dry_run, cli.force, cli.repo.as_deref(), args)
        }
        CliCommand::ReadySprint(args) => {
            run_ready_sprint(binary, cli.dry_run, cli.force, cli.repo.as_deref(), args)
        }
        CliCommand::AcceptSprint(args) => {
            run_accept_sprint(binary, cli.dry_run, cli.force, cli.repo.as_deref(), args)
        }
        CliCommand::MultiSprintGuide(args) => run_multi_sprint_guide(binary, args),
        CliCommand::Completion(_) => Err(CommandError::usage(
            "completion-direct-output-only",
            "completion output is emitted directly; run `<binary> completion <bash|zsh>`",
        )),
    }
}

fn run_build_task_spec(args: &BuildTaskSpecArgs) -> Result<Value, CommandError> {
    let options = to_build_options(
        args.prefixes.owner_prefix.clone(),
        args.prefixes.branch_prefix.clone(),
        args.prefixes.worktree_prefix.clone(),
        args.grouping.pr_grouping,
        args.grouping.default_pr_grouping,
        args.grouping.strategy,
        args.grouping.pr_group.clone(),
    );
    let build = task_spec::build_task_spec(
        &args.plan,
        TaskSpecScope::Sprint(i32::from(args.sprint)),
        &options,
    )
    .map_err(|err| CommandError::runtime("task-spec-generation-failed", err))?;

    let out_path = args.task_spec_out.clone().unwrap_or_else(|| {
        task_spec::default_sprint_task_spec_path(&args.plan, i32::from(args.sprint))
    });
    task_spec::write_tsv(&out_path, &build.rows)
        .map_err(|err| CommandError::runtime("task-spec-write-failed", err))?;

    Ok(json!({
        "scope": "sprint",
        "sprint": args.sprint,
        "task_spec_path": path_text(&out_path),
        "record_count": build.rows.len(),
        "plan_title": build.plan_title,
    }))
}

fn run_build_plan_task_spec(args: &BuildPlanTaskSpecArgs) -> Result<Value, CommandError> {
    let options = to_build_options(
        args.prefixes.owner_prefix.clone(),
        args.prefixes.branch_prefix.clone(),
        args.prefixes.worktree_prefix.clone(),
        args.grouping.pr_grouping,
        args.grouping.default_pr_grouping,
        args.grouping.strategy,
        args.grouping.pr_group.clone(),
    );
    let build = task_spec::build_task_spec(&args.plan, TaskSpecScope::Plan, &options)
        .map_err(|err| CommandError::runtime("task-spec-generation-failed", err))?;

    let out_path = args
        .task_spec_out
        .clone()
        .unwrap_or_else(|| task_spec::default_plan_task_spec_path(&args.plan));
    task_spec::write_tsv(&out_path, &build.rows)
        .map_err(|err| CommandError::runtime("task-spec-write-failed", err))?;

    Ok(json!({
        "scope": "plan",
        "task_spec_path": path_text(&out_path),
        "record_count": build.rows.len(),
        "plan_title": build.plan_title,
    }))
}

fn run_start_plan(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &StartPlanArgs,
) -> Result<Value, CommandError> {
    let options = to_build_options(
        args.prefixes.owner_prefix.clone(),
        args.prefixes.branch_prefix.clone(),
        args.prefixes.worktree_prefix.clone(),
        args.grouping.pr_grouping,
        args.grouping.default_pr_grouping,
        args.grouping.strategy,
        args.grouping.pr_group.clone(),
    );

    let build = task_spec::build_task_spec(&args.plan, TaskSpecScope::Plan, &options)
        .map_err(|err| CommandError::runtime("task-spec-generation-failed", err))?;

    let plan_title = args
        .title
        .clone()
        .unwrap_or_else(|| build.plan_title.clone());

    let issue_body = render::render_plan_issue_body(
        &args.plan,
        &build.display_plan_path,
        &plan_title,
        &build.rows,
        args.grouping.strategy,
    );
    let rendered_table = issue_body::parse_task_table(&issue_body)
        .map_err(|err| CommandError::runtime("issue-body-render-failed", err))?;
    let rendered_errors = issue_body::validate_rows(rendered_table.rows());
    if !rendered_errors.is_empty() {
        return Err(CommandError::runtime(
            "issue-body-invalid",
            rendered_errors.join(" | "),
        ));
    }

    let repo = crate::github::resolve_repo(repo_override)
        .map_err(|err| CommandError::usage("repo-resolution-failed", err))?;
    let repo_slug = runtime_layout::repo_slug(&repo);

    let mut issue_number: Option<u64> = None;
    let mut issue_url: Option<String> = None;
    let mut live_mutations = false;

    if binary == BinaryFlavor::PlanIssue && !dry_run {
        let temp_body = write_temp_markdown("plan-issue-body", &issue_body)
            .map_err(|err| CommandError::runtime("issue-body-write-failed", err))?;
        let adapter = GhCliAdapter::new(force);
        let (number, url) = adapter
            .create_issue(&repo, &plan_title, &temp_body, &args.label)
            .map_err(|err| CommandError::runtime("github-issue-create-failed", err))?;
        issue_number = Some(number);
        issue_url = Some(url);
        live_mutations = true;
    } else if binary == BinaryFlavor::PlanIssueLocal {
        issue_number = Some(LOCAL_ISSUE_PLACEHOLDER);
    }

    let issue_root_number = issue_number.unwrap_or(LOCAL_ISSUE_PLACEHOLDER);
    let issue_root = IssueRoot::new(&repo_slug, issue_root_number)
        .map_err(|err| CommandError::runtime("runtime-layout-failed", err.to_string()))?;

    let task_spec_out = args
        .task_spec_out
        .clone()
        .unwrap_or_else(|| issue_root.plan_task_spec());
    task_spec::write_tsv(&task_spec_out, &build.rows)
        .map_err(|err| CommandError::runtime("task-spec-write-failed", err))?;

    let issue_body_out = args
        .issue_body_out
        .clone()
        .unwrap_or_else(|| issue_root.plan_issue_body());
    render::write_rendered(&issue_body_out, &issue_body)
        .map_err(|err| CommandError::runtime("issue-body-write-failed", err))?;

    let main_agent_init_source = task_spec::agent_home()
        .join("prompts")
        .join("plan-issue-delivery-main-agent-init.md");
    if !main_agent_init_source.exists() {
        return Err(CommandError::runtime(
            "main-agent-init-snapshot-source-missing",
            format!(
                "main-agent init source not found at {}; ensure agent-kit is installed",
                main_agent_init_source.display()
            ),
        ));
    }
    let main_agent_init_target = issue_root.main_agent_init_snapshot();
    if let Some(parent) = main_agent_init_target.parent() {
        runtime_layout::ensure_dir(parent).map_err(|err| {
            CommandError::runtime(
                "runtime-layout-emit-failed",
                format!("failed to create dir {}: {err}", parent.display()),
            )
        })?;
    }
    fs::copy(&main_agent_init_source, &main_agent_init_target).map_err(|err| {
        CommandError::runtime(
            "runtime-layout-emit-failed",
            format!(
                "failed to copy main-agent init snapshot to {}: {err}",
                main_agent_init_target.display()
            ),
        )
    })?;

    let plan_branch_name = format!("plan/issue-{}", issue_root_number);
    let plan_branch_ref = issue_root.plan_branch_ref();
    if let Some(parent) = plan_branch_ref.parent() {
        runtime_layout::ensure_dir(parent).map_err(|err| {
            CommandError::runtime(
                "runtime-layout-emit-failed",
                format!("failed to create dir {}: {err}", parent.display()),
            )
        })?;
    }
    fs::write(&plan_branch_ref, plan_branch_name.as_bytes()).map_err(|err| {
        CommandError::runtime(
            "runtime-layout-emit-failed",
            format!(
                "failed to write plan-branch ref to {}: {err}",
                plan_branch_ref.display()
            ),
        )
    })?;

    Ok(json!({
        "scope": "plan",
        "execution_mode": binary.execution_mode(),
        "dry_run": dry_run,
        "task_spec_path": path_text(&task_spec_out),
        "issue_body_path": path_text(&issue_body_out),
        "issue_root": path_text(issue_root.root()),
        "main_agent_init_snapshot_path": path_text(&main_agent_init_target),
        "plan_branch_ref_path": path_text(&plan_branch_ref),
        "record_count": build.rows.len(),
        "issue_number": issue_number,
        "issue_url": issue_url,
        "labels": args.label,
        "live_mutations_performed": live_mutations,
    }))
}

fn run_status_plan(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &StatusPlanArgs,
) -> Result<Value, CommandError> {
    let adapter = GhCliAdapter::new(force);

    let (body, issue, repo, source) = if let Some(path) = &args.body_file {
        let body = fs::read_to_string(path).map_err(|err| {
            CommandError::runtime(
                "issue-body-read-failed",
                format!("failed to read body file {}: {err}", path.display()),
            )
        })?;
        (body, None, None, format!("body-file:{}", path.display()))
    } else {
        let issue = args
            .issue
            .ok_or_else(|| CommandError::usage("missing-issue", "--issue is required"))?;
        ensure_live_binary_for_command(
            binary,
            "status-plan --issue <number>",
            Some("plan-issue-local status-plan --body-file <path> --dry-run"),
        )?;
        let repo = resolve_repo_for_live(binary, repo_override)?;
        let body = adapter
            .issue_body(&repo, issue)
            .map_err(|err| CommandError::runtime("github-issue-read-failed", err))?;
        (body, Some(issue), Some(repo), format!("issue:{issue}"))
    };

    let table = issue_body::parse_task_table(&body)
        .map_err(|err| CommandError::runtime("issue-body-parse-failed", err))?;

    let structure_errors = issue_body::validate_rows(table.rows());
    if !structure_errors.is_empty() {
        return Err(CommandError::runtime(
            "issue-body-invalid",
            structure_errors.join(" | "),
        ));
    }

    let mut counts: HashMap<String, usize> = HashMap::new();
    for row in table.rows() {
        let status = row.status.trim().to_ascii_lowercase();
        *counts.entry(status).or_insert(0) += 1;
    }

    let should_comment = args.comment_mode.comment && !args.comment_mode.no_comment;
    let comment_text = render_plan_status_comment(table.rows());
    let mut live_mutations = false;

    if should_comment
        && binary == BinaryFlavor::PlanIssue
        && !dry_run
        && let (Some(issue), Some(repo)) = (issue, repo.as_deref())
    {
        let comment_path = write_temp_markdown("status-plan-comment", &comment_text)
            .map_err(|err| CommandError::runtime("comment-write-failed", err))?;
        adapter
            .comment_issue(repo, issue, &comment_path)
            .map_err(|err| CommandError::runtime("github-comment-failed", err))?;
        live_mutations = true;
    }

    Ok(json!({
        "scope": "plan",
        "execution_mode": binary.execution_mode(),
        "dry_run": dry_run,
        "issue_source": source,
        "task_count": table.rows().len(),
        "status_counts": counts,
        "comment_requested": should_comment,
        "comment_preview": should_comment.then_some(comment_text),
        "live_mutations_performed": live_mutations,
    }))
}

#[derive(Debug, Clone)]
struct LinkPrSelection {
    row_indexes: Vec<usize>,
    row_tasks: Vec<String>,
    lane_sync_applied: bool,
    lane_label: Option<String>,
    target_label: String,
}

#[derive(Debug, Clone)]
struct LinkPrScope {
    key: String,
    label: String,
    lane_label: Option<String>,
}

fn run_link_pr(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &LinkPrArgs,
) -> Result<Value, CommandError> {
    let adapter = GhCliAdapter::new(force);

    let (body, issue, repo, source, body_file_path) = if let Some(path) = &args.body_file {
        let body = fs::read_to_string(path).map_err(|err| {
            CommandError::runtime(
                "issue-body-read-failed",
                format!("failed to read body file {}: {err}", path.display()),
            )
        })?;
        (
            body,
            None,
            None,
            format!("body-file:{}", path.display()),
            Some(path.clone()),
        )
    } else {
        let issue = args
            .issue
            .ok_or_else(|| CommandError::usage("missing-issue", "--issue is required"))?;
        ensure_live_binary_for_command(
            binary,
            "link-pr --issue <number> --task <task-id> --pr <ref>",
            Some(
                "plan-issue-local link-pr --body-file <path> --task <task-id> --pr <ref> --dry-run",
            ),
        )?;
        let repo = resolve_repo_for_live(binary, repo_override)?;
        let body = adapter
            .issue_body(&repo, issue)
            .map_err(|err| CommandError::runtime("github-issue-read-failed", err))?;
        (
            body,
            Some(issue),
            Some(repo),
            format!("issue:{issue}"),
            None,
        )
    };

    let mut table = issue_body::parse_task_table(&body)
        .map_err(|err| CommandError::runtime("issue-body-parse-failed", err))?;
    let selection = select_link_pr_rows(table.rows(), args)
        .map_err(|err| CommandError::runtime("link-pr-target-invalid", err))?;

    let normalized_pr = issue_body::normalize_pr_display(&args.pr);
    let status_value = link_pr_status_text(args.status).to_string();
    for idx in &selection.row_indexes {
        let row = &mut table.rows_mut()[*idx];
        row.pr = normalized_pr.clone();
        row.status = status_value.clone();
    }

    let structure_errors = issue_body::validate_rows(table.rows());
    if !structure_errors.is_empty() {
        return Err(CommandError::runtime(
            "issue-body-invalid",
            structure_errors.join(" | "),
        ));
    }

    let updated_body = table.render();
    let mut body_file_updated = false;
    let mut live_mutations = false;

    if let Some(path) = body_file_path.as_ref() {
        if !dry_run {
            fs::write(path, &updated_body).map_err(|err| {
                CommandError::runtime(
                    "issue-body-write-failed",
                    format!("failed to write body file {}: {err}", path.display()),
                )
            })?;
            body_file_updated = true;
        }
    } else if !dry_run {
        let repo = repo.as_deref().ok_or_else(|| {
            CommandError::usage(
                "missing-repo",
                "unable to resolve repository for live link-pr update",
            )
        })?;
        let issue = issue.ok_or_else(|| {
            CommandError::usage("missing-issue", "--issue is required for live link-pr")
        })?;
        let body_path = write_temp_markdown("link-pr-issue-body", &updated_body)
            .map_err(|err| CommandError::runtime("issue-body-write-failed", err))?;
        adapter
            .edit_issue_body(repo, issue, &body_path)
            .map_err(|err| CommandError::runtime("github-issue-update-failed", err))?;
        live_mutations = true;
    }

    Ok(json!({
        "scope": "plan",
        "operation": "link-pr",
        "execution_mode": binary.execution_mode(),
        "dry_run": dry_run,
        "issue_source": source,
        "target": selection.target_label,
        "lane_sync_applied": selection.lane_sync_applied,
        "lane": selection.lane_label,
        "rows_changed": selection.row_indexes.len(),
        "tasks_changed": selection.row_tasks,
        "pr": normalized_pr,
        "status": status_value,
        "body_file_updated": body_file_updated,
        "live_mutations_performed": live_mutations,
    }))
}

fn link_pr_status_text(status: LinkPrStatus) -> &'static str {
    match status {
        LinkPrStatus::Planned => "planned",
        LinkPrStatus::InProgress => "in-progress",
        LinkPrStatus::Blocked => "blocked",
    }
}

fn select_link_pr_rows(rows: &[TaskRow], args: &LinkPrArgs) -> Result<LinkPrSelection, String> {
    if let Some(task_id) = args.task.as_deref() {
        return select_link_pr_rows_by_task(rows, task_id);
    }

    let sprint = args
        .sprint
        .map(i32::from)
        .ok_or_else(|| "missing target selector (`--task` or `--sprint`)".to_string())?;
    select_link_pr_rows_by_sprint(rows, sprint, args.pr_group.as_deref())
}

fn select_link_pr_rows_by_task(rows: &[TaskRow], task_id: &str) -> Result<LinkPrSelection, String> {
    let task_id = task_id.trim();
    let matching_indexes = rows
        .iter()
        .enumerate()
        .filter_map(|(idx, row)| row.task.trim().eq_ignore_ascii_case(task_id).then_some(idx))
        .collect::<Vec<_>>();

    let target_index = match matching_indexes.as_slice() {
        [] => {
            return Err(format!(
                "task `{task_id}` not found in issue Task Decomposition rows"
            ));
        }
        [idx] => *idx,
        _ => {
            return Err(format!(
                "task selector `{task_id}` matched multiple rows; repair duplicate task ids before linking PR"
            ));
        }
    };

    let mut row_indexes = vec![target_index];
    let mut lane_label = None;
    let mut lane_sync_applied = false;
    if let Some((lane_key, label)) = issue_body::runtime_pr_sync_lane(&rows[target_index]) {
        row_indexes = rows
            .iter()
            .enumerate()
            .filter_map(|(idx, row)| {
                issue_body::runtime_pr_sync_lane(row)
                    .and_then(|(candidate_key, _)| (candidate_key == lane_key).then_some(idx))
            })
            .collect();
        lane_sync_applied = row_indexes.len() > 1;
        lane_label = Some(label);
    }

    let row_tasks = row_indexes
        .iter()
        .map(|idx| rows[*idx].task.clone())
        .collect::<Vec<_>>();

    Ok(LinkPrSelection {
        row_indexes,
        row_tasks,
        lane_sync_applied,
        lane_label,
        target_label: format!("task:{task_id}"),
    })
}

fn select_link_pr_rows_by_sprint(
    rows: &[TaskRow],
    sprint: i32,
    pr_group: Option<&str>,
) -> Result<LinkPrSelection, String> {
    let mut row_indexes = rows
        .iter()
        .enumerate()
        .filter_map(|(idx, row)| (issue_body::row_sprint(row) == Some(sprint)).then_some(idx))
        .collect::<Vec<_>>();

    if row_indexes.is_empty() {
        return Err(format!("issue task table has no rows for sprint S{sprint}"));
    }

    let mut target_label = format!("sprint:S{sprint}");
    if let Some(group_raw) = pr_group {
        let group = group_raw.trim();
        row_indexes.retain(|idx| {
            note_value(&rows[*idx].notes, "pr-group")
                .is_some_and(|value| value.trim().eq_ignore_ascii_case(group))
        });
        if row_indexes.is_empty() {
            return Err(format!(
                "sprint S{sprint} has no rows with pr-group `{group}`; use --task or a valid --pr-group"
            ));
        }
        target_label = format!("sprint:S{sprint}/pr-group:{group}");
    }

    let mut scopes: BTreeMap<String, LinkPrScope> = BTreeMap::new();
    for idx in &row_indexes {
        let scope = row_link_pr_scope(&rows[*idx]);
        scopes.entry(scope.key.clone()).or_insert(scope);
    }

    if scopes.len() > 1 {
        let scope_labels = scopes
            .values()
            .map(|scope| scope.label.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let guidance = if pr_group.is_some() {
            "target still spans multiple runtime scopes; narrow with --task"
        } else {
            "use --pr-group to select a shared lane or --task for a single row/lane"
        };
        return Err(format!(
            "sprint S{sprint} target is ambiguous across runtime scopes ({scope_labels}); {guidance}"
        ));
    }

    let lane_label = scopes
        .values()
        .next()
        .and_then(|scope| scope.lane_label.clone());
    let row_tasks = row_indexes
        .iter()
        .map(|idx| rows[*idx].task.clone())
        .collect::<Vec<_>>();

    Ok(LinkPrSelection {
        row_indexes,
        row_tasks,
        lane_sync_applied: false,
        lane_label,
        target_label,
    })
}

fn row_link_pr_scope(row: &TaskRow) -> LinkPrScope {
    if let Some((lane_key, lane_label)) = issue_body::runtime_pr_sync_lane(row) {
        return LinkPrScope {
            key: format!("lane:{lane_key}"),
            label: format!("lane:{lane_label}"),
            lane_label: Some(lane_label),
        };
    }

    let task = row.task.trim().to_string();
    LinkPrScope {
        key: format!("task:{}", task.to_ascii_lowercase()),
        label: format!("task:{task}"),
        lane_label: None,
    }
}

fn run_ready_plan(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &ReadyPlanArgs,
) -> Result<Value, CommandError> {
    let adapter = GhCliAdapter::new(force);

    let (body, issue, repo, source) = if let Some(path) = &args.body_file {
        let body = fs::read_to_string(path).map_err(|err| {
            CommandError::runtime(
                "issue-body-read-failed",
                format!("failed to read body file {}: {err}", path.display()),
            )
        })?;
        (body, None, None, format!("body-file:{}", path.display()))
    } else {
        let issue = args
            .issue
            .ok_or_else(|| CommandError::usage("missing-issue", "--issue is required"))?;
        ensure_live_binary_for_command(
            binary,
            "ready-plan --issue <number>",
            Some("plan-issue-local ready-plan --body-file <path> --summary <text> --dry-run"),
        )?;
        let repo = resolve_repo_for_live(binary, repo_override)?;
        let body = adapter
            .issue_body(&repo, issue)
            .map_err(|err| CommandError::runtime("github-issue-read-failed", err))?;
        (body, Some(issue), Some(repo), format!("issue:{issue}"))
    };

    let table = issue_body::parse_task_table(&body)
        .map_err(|err| CommandError::runtime("issue-body-parse-failed", err))?;
    let structure_errors = issue_body::validate_rows(table.rows());
    if !structure_errors.is_empty() {
        return Err(CommandError::runtime(
            "issue-body-invalid",
            structure_errors.join(" | "),
        ));
    }

    let summary = load_summary(&args.summary)?;
    let should_comment = !args.comment_mode.no_comment;
    let comment_text = summary.unwrap_or_else(|| "Final plan review requested.".to_string());

    let ready_plan_label = args
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .unwrap_or("needs-review")
        .to_string();

    let mut labels_updated = false;
    let mut comment_posted = false;
    let mut live_mutations = false;

    if binary == BinaryFlavor::PlanIssue
        && !dry_run
        && let (Some(issue), Some(repo)) = (issue, repo.as_deref())
    {
        if args.label_update {
            adapter
                .edit_issue_labels(
                    repo,
                    issue,
                    std::slice::from_ref(&ready_plan_label),
                    &args.remove_label,
                )
                .map_err(|err| CommandError::runtime("github-label-update-failed", err))?;
            labels_updated = true;
            live_mutations = true;
        }

        if should_comment {
            let comment_path = write_temp_markdown("ready-plan-comment", &comment_text)
                .map_err(|err| CommandError::runtime("comment-write-failed", err))?;
            adapter
                .comment_issue(repo, issue, &comment_path)
                .map_err(|err| CommandError::runtime("github-comment-failed", err))?;
            comment_posted = true;
            live_mutations = true;
        }
    }

    Ok(json!({
        "scope": "plan",
        "execution_mode": binary.execution_mode(),
        "dry_run": dry_run,
        "issue_source": source,
        "task_count": table.rows().len(),
        "summary": comment_text,
        "label": ready_plan_label,
        "remove_label": args.remove_label,
        "label_update_requested": args.label_update,
        "label_update_applied": labels_updated,
        "comment_requested": should_comment,
        "comment_posted": comment_posted,
        "live_mutations_performed": live_mutations,
    }))
}

fn run_close_plan(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &ClosePlanArgs,
) -> Result<Value, CommandError> {
    if !approval_comment_url_looks_valid(&args.approved_comment_url) {
        return Err(CommandError::usage(
            "invalid-approval-comment-url",
            "--approved-comment-url must be a GitHub issue/pull comment URL",
        ));
    }

    let adapter = GhCliAdapter::new(force);
    let close_comment = load_close_comment(&args.close_comment)?;

    let (body, issue, repo, source) = if let Some(path) = &args.body_file {
        let body = fs::read_to_string(path).map_err(|err| {
            CommandError::runtime(
                "issue-body-read-failed",
                format!("failed to read body file {}: {err}", path.display()),
            )
        })?;
        let repo = (binary == BinaryFlavor::PlanIssue)
            .then(|| resolve_repo_for_live(binary, repo_override))
            .transpose()?;
        (
            body,
            args.issue,
            repo,
            format!("body-file:{}", path.display()),
        )
    } else {
        let issue = args
            .issue
            .ok_or_else(|| CommandError::usage("missing-issue", "--issue is required"))?;
        ensure_live_binary_for_command(
            binary,
            "close-plan --issue <number> --approved-comment-url <url>",
            Some(
                "plan-issue-local close-plan --body-file <path> --approved-comment-url <url> --dry-run",
            ),
        )?;
        let repo = resolve_repo_for_live(binary, repo_override)?;
        let body = adapter
            .issue_body(&repo, issue)
            .map_err(|err| CommandError::runtime("github-issue-read-failed", err))?;
        (body, Some(issue), Some(repo), format!("issue:{issue}"))
    };

    let table = issue_body::parse_task_table(&body)
        .map_err(|err| CommandError::runtime("issue-body-parse-failed", err))?;

    let mut gate_errors = issue_body::validate_rows(table.rows());

    if !args.allow_not_done {
        for row in table.rows() {
            if !row.status.trim().eq_ignore_ascii_case("done") {
                gate_errors.push(format!(
                    "{}: close gate requires Status=done (found `{}`)",
                    row.task,
                    row.status.trim()
                ));
            }
        }
    }

    let required_prs = collect_required_prs(table.rows(), "close-plan")
        .map_err(|err| CommandError::runtime("close-gate-failed", err))?;

    let mut merge_checks_skipped = false;
    if let Some(repo) = repo.as_deref() {
        ensure_prs_merged(&adapter, repo, &required_prs, "close-plan")
            .map_err(|err| CommandError::runtime("close-gate-failed", err))?;
    } else {
        merge_checks_skipped = true;
    }

    if !gate_errors.is_empty() {
        return Err(CommandError::runtime(
            "close-gate-failed",
            gate_errors.join(" | "),
        ));
    }

    let cleanup = cleanup_worktrees_from_rows(table.rows(), dry_run)
        .map_err(|err| CommandError::runtime("worktree-cleanup-failed", err))?;

    let mut issue_closed = false;
    let mut live_mutations = false;

    if binary == BinaryFlavor::PlanIssue && !dry_run {
        let issue = issue.ok_or_else(|| {
            CommandError::usage("missing-issue", "--issue is required for live close-plan")
        })?;
        let repo = repo.as_deref().ok_or_else(|| {
            CommandError::usage(
                "missing-repo",
                "unable to resolve repository for live close-plan",
            )
        })?;

        adapter
            .close_issue(repo, issue, args.reason, close_comment.as_deref())
            .map_err(|err| CommandError::runtime("github-issue-close-failed", err))?;
        issue_closed = true;
        live_mutations = true;
    }

    Ok(json!({
        "scope": "plan",
        "execution_mode": binary.execution_mode(),
        "dry_run": dry_run,
        "issue_source": source,
        "approval_comment_url": args.approved_comment_url,
        "allow_not_done": args.allow_not_done,
        "issue_closed": issue_closed,
        "close_comment_applied": close_comment.as_ref().is_some_and(|v| !v.trim().is_empty()),
        "cleanup": {
            "targeted": cleanup.targeted,
            "removed": cleanup.removed,
            "residual": cleanup.residual,
        },
        "merge_checks_skipped": merge_checks_skipped,
        "live_mutations_performed": live_mutations,
    }))
}

fn run_cleanup_worktrees(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &CleanupWorktreesArgs,
) -> Result<Value, CommandError> {
    ensure_live_binary_for_command(binary, "cleanup-worktrees --issue <number>", None)?;

    let repo = resolve_repo_for_live(binary, repo_override)?;
    let adapter = GhCliAdapter::new(force);
    let body = adapter
        .issue_body(&repo, args.issue)
        .map_err(|err| CommandError::runtime("github-issue-read-failed", err))?;

    let table = issue_body::parse_task_table(&body)
        .map_err(|err| CommandError::runtime("issue-body-parse-failed", err))?;
    let structure_errors = issue_body::validate_rows(table.rows());
    if !structure_errors.is_empty() {
        return Err(CommandError::runtime(
            "issue-body-invalid",
            structure_errors.join(" | "),
        ));
    }

    let cleanup = cleanup_worktrees_from_rows(table.rows(), dry_run)
        .map_err(|err| CommandError::runtime("worktree-cleanup-failed", err))?;

    Ok(json!({
        "scope": "plan",
        "execution_mode": binary.execution_mode(),
        "dry_run": dry_run,
        "issue": args.issue,
        "cleanup": {
            "targeted": cleanup.targeted,
            "removed": cleanup.removed,
            "residual": cleanup.residual,
        },
        "live_mutations_performed": !dry_run && !cleanup.removed.is_empty(),
    }))
}

fn run_start_sprint(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &StartSprintArgs,
) -> Result<Value, CommandError> {
    let options = to_build_options(
        args.prefixes.owner_prefix.clone(),
        args.prefixes.branch_prefix.clone(),
        args.prefixes.worktree_prefix.clone(),
        args.grouping.pr_grouping,
        args.grouping.default_pr_grouping,
        args.grouping.strategy,
        args.grouping.pr_group.clone(),
    );

    let build = task_spec::build_task_spec(
        &args.plan,
        TaskSpecScope::Sprint(i32::from(args.sprint)),
        &options,
    )
    .map_err(|err| CommandError::runtime("task-spec-generation-failed", err))?;
    let mut artifact_rows = build.rows.clone();

    let sprint_name = build
        .sprint_name
        .clone()
        .unwrap_or_else(|| format!("Sprint {}", args.sprint));

    let adapter = GhCliAdapter::new(force);
    let mut issue_body_for_comment: Option<String> = None;
    let mut synced_rows = 0usize;
    let mut live_mutations = false;

    if binary == BinaryFlavor::PlanIssue {
        let repo = resolve_repo_for_live(binary, repo_override)?;
        let body = adapter
            .issue_body(&repo, args.issue)
            .map_err(|err| CommandError::runtime("github-issue-read-failed", err))?;

        let table = issue_body::parse_task_table(&body)
            .map_err(|err| CommandError::runtime("issue-body-parse-failed", err))?;

        let structure_errors = issue_body::validate_rows(table.rows());
        if !structure_errors.is_empty() {
            return Err(CommandError::runtime(
                "issue-body-invalid",
                structure_errors.join(" | "),
            ));
        }

        if args.sprint > 1 {
            enforce_previous_sprint_gate(&adapter, &repo, table.rows(), i32::from(args.sprint))
                .map_err(|err| CommandError::runtime("previous-sprint-gate-failed", err))?;
        }

        artifact_rows = task_spec_rows_from_issue_rows(table.rows(), i32::from(args.sprint))
            .map_err(|err| CommandError::runtime("task-spec-from-issue-rows-failed", err))?;
        synced_rows = artifact_rows.len();
        ensure_start_sprint_runtime_truth_matches_plan(
            table.rows(),
            i32::from(args.sprint),
            &build.rows,
            args.grouping.strategy,
        )
        .map_err(|err| CommandError::runtime("task-sync-drift-detected", err))?;
        issue_body_for_comment = Some(body);
    }

    let repo = crate::github::resolve_repo(repo_override)
        .map_err(|err| CommandError::usage("repo-resolution-failed", err))?;
    let repo_slug = runtime_layout::repo_slug(&repo);
    let issue_root = IssueRoot::new(&repo_slug, args.issue)
        .map_err(|err| CommandError::runtime("runtime-layout-failed", err.to_string()))?;
    let sprint_root = SprintRoot::new(&issue_root, i32::from(args.sprint));

    let task_spec_out = args
        .task_spec_out
        .clone()
        .unwrap_or_else(|| sprint_root.task_spec());
    task_spec::write_tsv(&task_spec_out, &artifact_rows)
        .map_err(|err| CommandError::runtime("task-spec-write-failed", err))?;

    let prompts_dir = args
        .subagent_prompts_out
        .clone()
        .unwrap_or_else(|| sprint_root.prompts_dir());
    runtime_layout::ensure_dir(&prompts_dir).map_err(|err| {
        CommandError::runtime(
            "runtime-layout-emit-failed",
            format!(
                "failed to create prompts dir {}: {err}",
                prompts_dir.display()
            ),
        )
    })?;
    runtime_layout::ensure_dir(&sprint_root.manifests_dir()).map_err(|err| {
        CommandError::runtime(
            "runtime-layout-emit-failed",
            format!(
                "failed to create manifests dir {}: {err}",
                sprint_root.manifests_dir().display()
            ),
        )
    })?;
    runtime_layout::ensure_dir(&sprint_root.specs_dir()).map_err(|err| {
        CommandError::runtime(
            "runtime-layout-emit-failed",
            format!(
                "failed to create specs dir {}: {err}",
                sprint_root.specs_dir().display()
            ),
        )
    })?;

    let plan_snapshot_target = issue_root.plan_snapshot();
    copy_source_into_snapshot(
        &task_spec::resolve_plan_file(&args.plan),
        &plan_snapshot_target,
        "plan-snapshot-source-missing",
    )?;

    let subagent_init_source = task_spec::agent_home()
        .join("prompts")
        .join("plan-issue-delivery-subagent-init.md");
    let subagent_init_target = sprint_root.subagent_init_snapshot();
    copy_source_into_snapshot(
        &subagent_init_source,
        &subagent_init_target,
        "subagent-init-snapshot-source-missing",
    )?;

    let prompt_files = write_subagent_prompts(
        &prompts_dir,
        args.issue,
        i32::from(args.sprint),
        &artifact_rows,
        args.grouping.strategy,
    )
    .map_err(|err| CommandError::runtime("subagent-prompt-write-failed", err))?;

    let plan_branch = read_plan_branch(&issue_root, args.issue);

    let mut dispatch_record_paths: Vec<String> = Vec::new();
    for row in &artifact_rows {
        let task_prompt_path = sprint_root
            .task_prompt(&row.task_id)
            .map_err(|err| CommandError::runtime("runtime-layout-failed", err.to_string()))?;
        let dispatch_path = sprint_root
            .dispatch_record(&row.task_id)
            .map_err(|err| CommandError::runtime("runtime-layout-failed", err.to_string()))?;
        let execution_mode = execution_mode_for_row(row, args.grouping.strategy, &artifact_rows);
        // Canonical contract: dispatch record `worktree` is the absolute
        // assigned path under $WORKTREE_ROOT (RUNTIME_LAYOUT.md "Worktree
        // Layout (Assigned Paths)"), not the short name from the TSV.
        let assigned_worktree = issue_root
            .assigned_worktree(
                &execution_mode,
                &row.task_id,
                &row.pr_group,
                i32::from(args.sprint),
            )
            .map_err(|err| CommandError::runtime("runtime-layout-failed", err.to_string()))?;
        let record = DispatchRecord::implementation(
            row.task_id.clone(),
            path_text(&task_prompt_path),
            path_text(&subagent_init_target),
            path_text(&plan_snapshot_target),
            path_text(&assigned_worktree),
            row.branch.clone(),
            execution_mode,
            row.pr_group.clone(),
            plan_branch.clone(),
        );
        dispatch_record::write_dispatch_record(&dispatch_path, &record).map_err(|err| {
            CommandError::runtime(
                "runtime-layout-emit-failed",
                format!(
                    "failed to write dispatch record {}: {err}",
                    dispatch_path.display()
                ),
            )
        })?;
        dispatch_record_paths.push(path_text(&dispatch_path));
    }
    dispatch_record_paths.sort();

    let prompt_manifest_path = sprint_root.prompt_manifest();
    write_prompt_manifest(
        &prompt_manifest_path,
        &artifact_rows,
        &sprint_root,
        args.grouping.strategy,
    )
    .map_err(|err| {
        CommandError::runtime(
            "runtime-layout-emit-failed",
            format!(
                "failed to write prompt manifest {}: {err}",
                prompt_manifest_path.display()
            ),
        )
    })?;

    let comment = render::render_sprint_comment(SprintCommentInput {
        mode: SprintCommentMode::Start,
        plan_file: &args.plan,
        sprint: i32::from(args.sprint),
        sprint_name: &sprint_name,
        rows: &artifact_rows,
        strategy: args.grouping.strategy,
        note_text: None,
        approval_comment_url: None,
        issue_body_text: issue_body_for_comment.as_deref(),
    })
    .map_err(|err| CommandError::runtime("render-sprint-comment-failed", err))?;

    let comment_out = render::default_sprint_comment_path(
        &args.plan,
        i32::from(args.sprint),
        SprintCommentMode::Start,
    );
    render::write_rendered(&comment_out, &comment)
        .map_err(|err| CommandError::runtime("comment-write-failed", err))?;

    let should_comment = should_emit_comment(&args.comment_mode);
    if binary == BinaryFlavor::PlanIssue && should_comment && !dry_run {
        let repo = resolve_repo_for_live(binary, repo_override)?;
        adapter
            .comment_issue(&repo, args.issue, &comment_out)
            .map_err(|err| CommandError::runtime("github-comment-failed", err))?;
        live_mutations = true;
    }

    Ok(json!({
        "scope": "sprint",
        "sprint": args.sprint,
        "execution_mode": binary.execution_mode(),
        "dry_run": dry_run,
        "task_spec_path": path_text(&task_spec_out),
        "comment_path": path_text(&comment_out),
        "record_count": artifact_rows.len(),
        "subagent_prompts_out": path_text(&prompts_dir),
        "subagent_prompt_files": prompt_files,
        "sprint_root": path_text(sprint_root.root()),
        "plan_snapshot_path": path_text(&plan_snapshot_target),
        "subagent_init_snapshot_path": path_text(&subagent_init_target),
        "prompt_manifest_path": path_text(&prompt_manifest_path),
        "dispatch_record_paths": dispatch_record_paths,
        "synced_issue_rows": synced_rows,
        "comment_requested": should_comment,
        "live_mutations_performed": live_mutations,
    }))
}

fn copy_source_into_snapshot(
    source: &Path,
    target: &Path,
    missing_code: &'static str,
) -> Result<(), CommandError> {
    if !source.exists() {
        return Err(CommandError::runtime(
            missing_code,
            format!("snapshot source not found at {}", source.display()),
        ));
    }
    if let Some(parent) = target.parent() {
        runtime_layout::ensure_dir(parent).map_err(|err| {
            CommandError::runtime(
                "runtime-layout-emit-failed",
                format!("failed to create dir {}: {err}", parent.display()),
            )
        })?;
    }
    fs::copy(source, target).map_err(|err| {
        CommandError::runtime(
            "runtime-layout-emit-failed",
            format!("failed to copy snapshot to {}: {err}", target.display()),
        )
    })?;
    Ok(())
}

fn read_plan_branch(issue_root: &IssueRoot, issue: u64) -> String {
    fs::read_to_string(issue_root.plan_branch_ref())
        .ok()
        .and_then(|raw| {
            let trimmed = raw.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .unwrap_or_else(|| format!("plan/issue-{issue}"))
}

fn execution_mode_for_row(
    row: &TaskSpecRow,
    strategy: SplitStrategy,
    rows: &[TaskSpecRow],
) -> String {
    task_spec::execution_mode_by_task(rows, strategy)
        .get(&row.task_id)
        .cloned()
        .unwrap_or_else(|| "pr-isolated".to_string())
}

fn write_prompt_manifest(
    path: &Path,
    rows: &[TaskSpecRow],
    sprint_root: &SprintRoot,
    strategy: SplitStrategy,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut text = String::from("task_id\tprompt_path\texecution_mode\tworkflow_role\n");
    let modes = task_spec::execution_mode_by_task(rows, strategy);
    let mut sorted: Vec<&TaskSpecRow> = rows.iter().collect();
    sorted.sort_unstable_by(|a, b| a.task_id.cmp(&b.task_id));
    for row in sorted {
        let prompt_path = sprint_root
            .task_prompt(&row.task_id)
            .map_err(|err| std::io::Error::other(err.to_string()))?;
        let execution_mode = modes
            .get(&row.task_id)
            .cloned()
            .unwrap_or_else(|| "pr-isolated".to_string());
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            row.task_id,
            prompt_path.to_string_lossy(),
            execution_mode,
            dispatch_record::WORKFLOW_ROLE_IMPLEMENTATION,
        ));
    }
    fs::write(path, text)
}

fn run_ready_sprint(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &ReadySprintArgs,
) -> Result<Value, CommandError> {
    let options = to_build_options(
        args.prefixes.owner_prefix.clone(),
        args.prefixes.branch_prefix.clone(),
        args.prefixes.worktree_prefix.clone(),
        args.grouping.pr_grouping,
        args.grouping.default_pr_grouping,
        args.grouping.strategy,
        args.grouping.pr_group.clone(),
    );

    let build = task_spec::build_task_spec(
        &args.plan,
        TaskSpecScope::Sprint(i32::from(args.sprint)),
        &options,
    )
    .map_err(|err| CommandError::runtime("task-spec-generation-failed", err))?;

    let task_spec_out = args.task_spec_out.clone().unwrap_or_else(|| {
        task_spec::default_sprint_task_spec_path(&args.plan, i32::from(args.sprint))
    });
    task_spec::write_tsv(&task_spec_out, &build.rows)
        .map_err(|err| CommandError::runtime("task-spec-write-failed", err))?;

    let summary = load_summary(&args.summary)?;
    let sprint_name = build
        .sprint_name
        .clone()
        .unwrap_or_else(|| format!("Sprint {}", args.sprint));

    let adapter = GhCliAdapter::new(force);
    let mut issue_body_for_comment: Option<String> = None;
    let mut live_mutations = false;

    if binary == BinaryFlavor::PlanIssue {
        let repo = resolve_repo_for_live(binary, repo_override)?;
        let body = adapter
            .issue_body(&repo, args.issue)
            .map_err(|err| CommandError::runtime("github-issue-read-failed", err))?;
        let table = issue_body::parse_task_table(&body)
            .map_err(|err| CommandError::runtime("issue-body-parse-failed", err))?;
        let structure_errors = issue_body::validate_rows(table.rows());
        if !structure_errors.is_empty() {
            return Err(CommandError::runtime(
                "issue-body-invalid",
                structure_errors.join(" | "),
            ));
        }
        issue_body_for_comment = Some(body);
    }

    let comment = render::render_sprint_comment(SprintCommentInput {
        mode: SprintCommentMode::Ready,
        plan_file: &args.plan,
        sprint: i32::from(args.sprint),
        sprint_name: &sprint_name,
        rows: &build.rows,
        strategy: args.grouping.strategy,
        note_text: summary.as_deref(),
        approval_comment_url: None,
        issue_body_text: issue_body_for_comment.as_deref(),
    })
    .map_err(|err| CommandError::runtime("render-sprint-comment-failed", err))?;

    let comment_out = render::default_sprint_comment_path(
        &args.plan,
        i32::from(args.sprint),
        SprintCommentMode::Ready,
    );
    render::write_rendered(&comment_out, &comment)
        .map_err(|err| CommandError::runtime("comment-write-failed", err))?;

    let should_comment = should_emit_comment(&args.comment_mode);
    if binary == BinaryFlavor::PlanIssue && should_comment && !dry_run {
        let repo = resolve_repo_for_live(binary, repo_override)?;
        adapter
            .comment_issue(&repo, args.issue, &comment_out)
            .map_err(|err| CommandError::runtime("github-comment-failed", err))?;
        live_mutations = true;
    }

    Ok(json!({
        "scope": "sprint",
        "sprint": args.sprint,
        "execution_mode": binary.execution_mode(),
        "dry_run": dry_run,
        "task_spec_path": path_text(&task_spec_out),
        "comment_path": path_text(&comment_out),
        "record_count": build.rows.len(),
        "comment_requested": should_comment,
        "live_mutations_performed": live_mutations,
    }))
}

fn run_accept_sprint(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &AcceptSprintArgs,
) -> Result<Value, CommandError> {
    if !approval_comment_url_looks_valid(&args.approved_comment_url) {
        return Err(CommandError::usage(
            "invalid-approval-comment-url",
            "--approved-comment-url must be a GitHub issue/pull comment URL",
        ));
    }

    let options = to_build_options(
        args.prefixes.owner_prefix.clone(),
        args.prefixes.branch_prefix.clone(),
        args.prefixes.worktree_prefix.clone(),
        args.grouping.pr_grouping,
        args.grouping.default_pr_grouping,
        args.grouping.strategy,
        args.grouping.pr_group.clone(),
    );

    let build = task_spec::build_task_spec(
        &args.plan,
        TaskSpecScope::Sprint(i32::from(args.sprint)),
        &options,
    )
    .map_err(|err| CommandError::runtime("task-spec-generation-failed", err))?;

    let task_spec_out = args.task_spec_out.clone().unwrap_or_else(|| {
        task_spec::default_sprint_task_spec_path(&args.plan, i32::from(args.sprint))
    });
    task_spec::write_tsv(&task_spec_out, &build.rows)
        .map_err(|err| CommandError::runtime("task-spec-write-failed", err))?;

    let summary = load_summary(&args.summary)?;
    let sprint_name = build
        .sprint_name
        .clone()
        .unwrap_or_else(|| format!("Sprint {}", args.sprint));

    let adapter = GhCliAdapter::new(force);
    let mut issue_body_for_comment: Option<String> = None;
    let mut synced_done_rows = 0usize;
    let mut live_mutations = false;

    if binary == BinaryFlavor::PlanIssue {
        let repo = resolve_repo_for_live(binary, repo_override)?;
        let body = adapter
            .issue_body(&repo, args.issue)
            .map_err(|err| CommandError::runtime("github-issue-read-failed", err))?;

        let mut table = issue_body::parse_task_table(&body)
            .map_err(|err| CommandError::runtime("issue-body-parse-failed", err))?;
        let structure_errors = issue_body::validate_rows(table.rows());
        if !structure_errors.is_empty() {
            return Err(CommandError::runtime(
                "issue-body-invalid",
                structure_errors.join(" | "),
            ));
        }

        let sprint_indexes = table.sprint_row_indexes(i32::from(args.sprint));
        if sprint_indexes.is_empty() {
            return Err(CommandError::runtime(
                "sprint-not-found",
                format!("issue task table has no rows for sprint {}", args.sprint),
            ));
        }

        let sprint_rows: Vec<TaskRow> = sprint_indexes
            .iter()
            .map(|idx| table.rows()[*idx].clone())
            .collect();

        let required_prs = collect_required_prs(&sprint_rows, "accept-sprint")
            .map_err(|err| CommandError::runtime("sprint-acceptance-gate-failed", err))?;
        ensure_prs_merged(&adapter, &repo, &required_prs, "accept-sprint")
            .map_err(|err| CommandError::runtime("sprint-acceptance-gate-failed", err))?;

        for idx in sprint_indexes {
            let row = &mut table.rows_mut()[idx];
            row.status = "done".to_string();
            row.pr = issue_body::normalize_pr_display(&row.pr);
            synced_done_rows += 1;
        }

        let updated_body = table.render();
        issue_body_for_comment = Some(updated_body.clone());

        if !dry_run {
            let body_path = write_temp_markdown("accept-sprint-issue-body", &updated_body)
                .map_err(|err| CommandError::runtime("issue-body-write-failed", err))?;
            adapter
                .edit_issue_body(&repo, args.issue, &body_path)
                .map_err(|err| CommandError::runtime("github-issue-update-failed", err))?;
            live_mutations = true;
        }
    }

    let comment = render::render_sprint_comment(SprintCommentInput {
        mode: SprintCommentMode::Accepted,
        plan_file: &args.plan,
        sprint: i32::from(args.sprint),
        sprint_name: &sprint_name,
        rows: &build.rows,
        strategy: args.grouping.strategy,
        note_text: summary.as_deref(),
        approval_comment_url: Some(&args.approved_comment_url),
        issue_body_text: issue_body_for_comment.as_deref(),
    })
    .map_err(|err| CommandError::runtime("render-sprint-comment-failed", err))?;

    let comment_out = render::default_sprint_comment_path(
        &args.plan,
        i32::from(args.sprint),
        SprintCommentMode::Accepted,
    );
    render::write_rendered(&comment_out, &comment)
        .map_err(|err| CommandError::runtime("comment-write-failed", err))?;

    let should_comment = should_emit_comment(&args.comment_mode);
    if binary == BinaryFlavor::PlanIssue && should_comment && !dry_run {
        let repo = resolve_repo_for_live(binary, repo_override)?;
        adapter
            .comment_issue(&repo, args.issue, &comment_out)
            .map_err(|err| CommandError::runtime("github-comment-failed", err))?;
        live_mutations = true;
    }

    Ok(json!({
        "scope": "sprint",
        "sprint": args.sprint,
        "execution_mode": binary.execution_mode(),
        "dry_run": dry_run,
        "task_spec_path": path_text(&task_spec_out),
        "comment_path": path_text(&comment_out),
        "record_count": build.rows.len(),
        "approval_comment_url": args.approved_comment_url,
        "synced_done_rows": synced_done_rows,
        "comment_requested": should_comment,
        "live_mutations_performed": live_mutations,
    }))
}

fn run_multi_sprint_guide(
    binary: BinaryFlavor,
    args: &MultiSprintGuideArgs,
) -> Result<Value, CommandError> {
    let display_path = args.plan.to_string_lossy().to_string();
    let resolved_plan_path = task_spec::resolve_plan_file(&args.plan);
    if !resolved_plan_path.is_file() {
        return Err(CommandError::runtime(
            "plan-parse-failed",
            format!("plan file not found: {display_path}"),
        ));
    }
    let (plan, parse_errors) = parse_plan_with_display(&resolved_plan_path, &display_path)
        .map_err(|err| CommandError::runtime("plan-parse-failed", err.to_string()))?;

    if !parse_errors.is_empty() {
        return Err(CommandError::runtime(
            "plan-parse-failed",
            parse_errors.join(" | "),
        ));
    }

    let from_sprint = i32::from(args.from_sprint);
    let max_sprint = plan
        .sprints
        .iter()
        .map(|s| s.number)
        .max()
        .unwrap_or(from_sprint);
    let to_sprint = args.to_sprint.map(i32::from).unwrap_or(max_sprint);

    if to_sprint < from_sprint {
        return Err(CommandError::usage(
            "invalid-sprint-range",
            "--to-sprint must be greater than or equal to --from-sprint",
        ));
    }

    let issue_body_path = match crate::github::resolve_repo(None) {
        Ok(repo) => {
            let slug = runtime_layout::repo_slug(&repo);
            match IssueRoot::new(&slug, LOCAL_ISSUE_PLACEHOLDER) {
                Ok(issue_root) => issue_root.plan_issue_body(),
                Err(_) => render::default_plan_issue_body_path(&args.plan),
            }
        }
        Err(_) => render::default_plan_issue_body_path(&args.plan),
    };
    let cli = binary.binary_name();

    let mut lines = vec![
        "MULTI_SPRINT_GUIDE_BEGIN".to_string(),
        "DESIGN=ONE_PLAN_ONE_ISSUE".to_string(),
        format!(
            "MODE={}",
            match binary {
                BinaryFlavor::PlanIssue => "DRY_RUN_LIVE_BINARY",
                BinaryFlavor::PlanIssueLocal => "DRY_RUN_LOCAL",
            }
        ),
        format!("PLAN_FILE={display_path}"),
        format!("PLAN_TITLE={}", plan.title),
        format!("FROM_SPRINT={from_sprint}"),
        format!("TO_SPRINT={to_sprint}"),
        format!("DRY_RUN_PLAN_ISSUE={LOCAL_ISSUE_PLACEHOLDER}"),
        format!("DRY_RUN_ISSUE_BODY={}", issue_body_path.display()),
    ];

    let mut step = 1usize;
    lines.push(format!(
        "STEP_{step}={cli} start-plan --plan {display_path} <grouping-args> --dry-run"
    ));
    step += 1;

    for sprint in from_sprint..=to_sprint {
        lines.push(format!(
            "STEP_{step}={cli} start-sprint --plan {display_path} --issue {LOCAL_ISSUE_PLACEHOLDER} --sprint {sprint} <grouping-args> --no-comment --dry-run"
        ));
        step += 1;

        if sprint < to_sprint {
            lines.push(format!(
                "STEP_{step}={cli} accept-sprint --plan {display_path} --issue {LOCAL_ISSUE_PLACEHOLDER} --sprint {sprint} --approved-comment-url <approval-comment-url-sprint-{sprint}> <grouping-args> --no-comment --dry-run"
            ));
            step += 1;
        }
    }

    lines.push(format!(
        "STEP_{step}={cli} ready-plan --body-file {} --summary Final\\ plan\\ review --no-comment --dry-run",
        issue_body_path.display()
    ));
    step += 1;

    lines.push(format!(
        "STEP_{step}={cli} close-plan --body-file {} --approved-comment-url <final-plan-approval-comment-url> --dry-run",
        issue_body_path.display()
    ));

    lines.extend([
        "NOTE_DRY_RUN=Dry-run guide is local-only and does not call GitHub.".to_string(),
        "GROUPING_ARGS_DETERMINISTIC=--pr-grouping <per-sprint\\|group> [--strategy deterministic]".to_string(),
        "GROUPING_ARGS_AUTO=--strategy auto [--default-pr-grouping <per-sprint\\|group>]".to_string(),
        "NOTE_GROUP_MODE_DETERMINISTIC=When using --pr-grouping group with --strategy deterministic, pass --pr-group for every task in the selected scope.".to_string(),
        "NOTE_GROUP_MODE_AUTO=When using --strategy auto, sprint metadata decides grouping intent and --default-pr-grouping fills metadata gaps.".to_string(),
        "NOTE_SPRINT_GATE=Before starting sprint N+1, sprint N must be reviewed, merged, and accepted.".to_string(),
        "NOTE_ACCEPT_SYNC=accept-sprint enforces merged PRs for the sprint and syncs sprint task Status to done.".to_string(),
        "MULTI_SPRINT_GUIDE_END".to_string(),
    ]);

    Ok(json!({
        "scope": "plan",
        "from_sprint": from_sprint,
        "to_sprint": to_sprint,
        "guide": lines.join("\n"),
    }))
}

fn to_build_options(
    owner_prefix: String,
    branch_prefix: String,
    worktree_prefix: String,
    pr_grouping: Option<crate::commands::PrGrouping>,
    default_pr_grouping: Option<crate::commands::PrGrouping>,
    strategy: crate::commands::SplitStrategy,
    pr_group: Vec<crate::commands::PrGroupMapping>,
) -> TaskSpecBuildOptions {
    TaskSpecBuildOptions {
        owner_prefix,
        branch_prefix,
        worktree_prefix,
        pr_grouping,
        default_pr_grouping,
        strategy,
        pr_group,
    }
}

fn load_summary(summary: &SummaryArgs) -> Result<Option<String>, CommandError> {
    if let Some(inline) = &summary.summary {
        return Ok(Some(inline.to_string()));
    }
    if let Some(path) = &summary.summary_file {
        let text = fs::read_to_string(path).map_err(|err| {
            CommandError::runtime(
                "summary-read-failed",
                format!("failed to read summary file {}: {err}", path.display()),
            )
        })?;
        return Ok(Some(text));
    }
    Ok(None)
}

fn load_close_comment(
    comment: &crate::commands::CommentTextArgs,
) -> Result<Option<String>, CommandError> {
    if let Some(inline) = &comment.comment {
        return Ok(Some(inline.to_string()));
    }
    if let Some(path) = &comment.comment_file {
        let text = fs::read_to_string(path).map_err(|err| {
            CommandError::runtime(
                "close-comment-read-failed",
                format!(
                    "failed to read close comment file {}: {err}",
                    path.display()
                ),
            )
        })?;
        return Ok(Some(text));
    }

    Ok(None)
}

fn approval_comment_url_looks_valid(url: &str) -> bool {
    let trimmed = url.trim();
    if !trimmed.starts_with("https://github.com/") {
        return false;
    }
    let Some((base, suffix)) = trimmed.split_once("#issuecomment-") else {
        return false;
    };
    if !suffix.chars().all(|ch| ch.is_ascii_digit()) {
        return false;
    }
    base.contains("/issues/") || base.contains("/pull/")
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn ensure_live_binary(binary: BinaryFlavor) -> Result<(), CommandError> {
    if binary == BinaryFlavor::PlanIssue {
        Ok(())
    } else {
        Err(CommandError::usage(
            "live-command-unavailable",
            "this command path is not supported in `plan-issue-local`; use `plan-issue <command>` for live GitHub operations, or switch to `--body-file` local rehearsal where supported",
        ))
    }
}

fn ensure_live_binary_for_command(
    binary: BinaryFlavor,
    live_command: &str,
    local_rehearsal_example: Option<&str>,
) -> Result<(), CommandError> {
    if binary == BinaryFlavor::PlanIssue {
        return Ok(());
    }

    let mut message = format!(
        "this command path is not supported in `plan-issue-local`: `{live_command}`; use `plan-issue {live_command}` for live GitHub operations"
    );
    if let Some(example) = local_rehearsal_example {
        message.push_str(&format!(", or use local rehearsal: `{example}`"));
    }

    Err(CommandError::usage("live-command-unavailable", message))
}

fn resolve_repo_for_live(
    binary: BinaryFlavor,
    repo_override: Option<&str>,
) -> Result<String, CommandError> {
    ensure_live_binary(binary)?;
    crate::github::resolve_repo(repo_override)
        .map_err(|err| CommandError::usage("repo-resolution-failed", err))
}

fn render_plan_status_comment(rows: &[TaskRow]) -> String {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for row in rows {
        let status = row.status.trim().to_ascii_lowercase();
        *counts.entry(status).or_insert(0) += 1;
    }

    format!(
        "## Plan Status Snapshot\n\n- Total tasks: {}\n- planned: {}\n- in-progress: {}\n- blocked: {}\n- done: {}\n",
        rows.len(),
        counts.get("planned").copied().unwrap_or(0),
        counts.get("in-progress").copied().unwrap_or(0),
        counts.get("blocked").copied().unwrap_or(0),
        counts.get("done").copied().unwrap_or(0),
    )
}

fn should_emit_comment(comment_mode: &crate::commands::CommentModeArgs) -> bool {
    !comment_mode.no_comment
}

fn write_temp_markdown(stem: &str, content: &str) -> Result<PathBuf, String> {
    let dir = task_spec::agent_home()
        .join("out")
        .join("plan-issue-delivery")
        .join("tmp");
    fs::create_dir_all(&dir).map_err(|err| {
        format!(
            "failed to create temp output directory {}: {err}",
            dir.display()
        )
    })?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("failed to compute timestamp: {err}"))?
        .as_millis();
    let path = dir.join(format!("{stem}-{now}.md"));
    fs::write(&path, content).map_err(|err| {
        format!(
            "failed to write temporary markdown {}: {err}",
            path.display()
        )
    })?;
    Ok(path)
}

fn write_subagent_prompts(
    out_dir: &Path,
    issue: u64,
    sprint: i32,
    rows: &[TaskSpecRow],
    strategy: SplitStrategy,
) -> Result<Vec<String>, String> {
    fs::create_dir_all(out_dir).map_err(|err| {
        format!(
            "failed to create subagent prompt dir {}: {err}",
            out_dir.display()
        )
    })?;

    #[derive(Debug, Clone)]
    struct PromptLane {
        execution_mode: String,
        owner: String,
        branch: String,
        worktree: String,
        notes: String,
        rows: Vec<TaskSpecRow>,
    }

    let runtime_lanes = task_spec::runtime_lane_metadata_by_task(rows, strategy);
    let mut lanes: BTreeMap<String, PromptLane> = BTreeMap::new();

    for row in rows {
        let lane = runtime_lanes.get(&row.task_id);
        let execution_mode = lane
            .map(|metadata| metadata.execution_mode.clone())
            .unwrap_or_else(|| "pr-isolated".to_string());
        let owner = lane
            .map(|metadata| metadata.owner.clone())
            .unwrap_or_else(|| row.owner.clone());
        let branch = lane
            .map(|metadata| metadata.branch.clone())
            .unwrap_or_else(|| row.branch.clone());
        let worktree = lane
            .map(|metadata| metadata.worktree.clone())
            .unwrap_or_else(|| row.worktree.clone());
        let notes = lane
            .map(|metadata| metadata.notes.clone())
            .unwrap_or_else(|| row.notes.clone());
        let lane_key = runtime_lane_key(row, &execution_mode, &notes);
        lanes
            .entry(lane_key)
            .or_insert_with(|| PromptLane {
                execution_mode: execution_mode.clone(),
                owner,
                branch,
                worktree,
                notes: notes.clone(),
                rows: Vec::new(),
            })
            .rows
            .push(row.clone());
    }

    let mut paths = Vec::new();
    for lane in lanes.values_mut() {
        lane.rows
            .sort_unstable_by(|left, right| left.task_id.cmp(&right.task_id));
        let anchor_task = prompt_lane_anchor_task_id(&lane.rows, &lane.notes)?;
        let task_list = lane
            .rows
            .iter()
            .map(|row| row.task_id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let lane_tasks = lane
            .rows
            .iter()
            .map(|row| {
                let summary = if row.summary.trim().is_empty() {
                    "-"
                } else {
                    row.summary.trim()
                };
                format!("- {}: {summary}", row.task_id)
            })
            .collect::<Vec<_>>()
            .join("\n");

        for row in &lane.rows {
            let task_summary = if row.summary.trim().is_empty() {
                "-".to_string()
            } else {
                row.summary.trim().to_string()
            };
            let path = out_dir.join(format!("{}.md", row.task_id));
            let body = format!(
                "# Subagent Task Prompt\n\n- Issue: #{issue}\n- Sprint: S{sprint}\n- Task: {}\n- Anchor: {anchor_task}\n- Tasks: {task_list}\n- Summary: {task_summary}\n- Owner: {}\n- Branch: {}\n- Worktree: {}\n- Execution Mode: {}\n- Notes: {}\n\n## Lane Tasks\n{lane_tasks}\n",
                row.task_id,
                lane.owner,
                lane.branch,
                lane.worktree,
                lane.execution_mode,
                lane.notes
            );
            fs::write(&path, body).map_err(|err| {
                format!("failed to write subagent prompt {}: {err}", path.display())
            })?;
            paths.push(path.to_string_lossy().to_string());
        }
    }

    paths.sort();
    Ok(paths)
}

fn task_spec_rows_from_issue_rows(
    rows: &[TaskRow],
    sprint: i32,
) -> Result<Vec<TaskSpecRow>, String> {
    let mut scoped = Vec::new();
    for row in rows {
        if issue_body::row_sprint(row) != Some(sprint) {
            continue;
        }

        let task_id = row.task.trim();
        if task_id.is_empty() {
            return Err(format!(
                "issue task table contains empty Task id for sprint S{sprint}"
            ));
        }
        if issue_body::is_placeholder(&row.owner)
            || issue_body::is_placeholder(&row.branch)
            || issue_body::is_placeholder(&row.worktree)
            || issue_body::is_placeholder(&row.execution_mode)
        {
            return Err(format!(
                "{task_id}: issue task row must include concrete Owner/Branch/Worktree/Execution Mode before start-sprint dispatch"
            ));
        }

        let execution_mode = row.execution_mode.trim().to_ascii_lowercase();
        let grouping = if execution_mode == "per-sprint" {
            crate::commands::PrGrouping::PerSprint
        } else {
            crate::commands::PrGrouping::Group
        };
        let pr_group = note_value(&row.notes, "pr-group")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| default_pr_group_for_issue_row(task_id, sprint, &execution_mode));

        scoped.push(TaskSpecRow {
            task_id: task_id.to_string(),
            summary: row.summary.clone(),
            branch: row.branch.clone(),
            worktree: row.worktree.clone(),
            owner: row.owner.clone(),
            notes: row.notes.clone(),
            pr_group,
            sprint,
            grouping,
        });
    }

    if scoped.is_empty() {
        return Err(format!(
            "issue task table missing rows for sprint S{sprint}"
        ));
    }

    scoped.sort_unstable_by(|left, right| left.task_id.cmp(&right.task_id));
    Ok(scoped)
}

fn default_pr_group_for_issue_row(task_id: &str, sprint: i32, execution_mode: &str) -> String {
    match execution_mode {
        "per-sprint" => format!("s{sprint}-per-sprint"),
        "pr-shared" => format!("s{sprint}-pr-shared"),
        _ => task_id.to_string(),
    }
}

fn runtime_lane_key(row: &TaskSpecRow, execution_mode: &str, notes: &str) -> String {
    match execution_mode {
        "per-sprint" => format!("per-sprint:S{}", row.sprint),
        "pr-shared" => {
            let pr_group = note_value(notes, "pr-group")
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| row.pr_group.clone());
            format!(
                "pr-shared:S{}:{}",
                row.sprint,
                pr_group.trim().to_ascii_lowercase()
            )
        }
        _ => format!("pr-isolated:{}", row.task_id),
    }
}

fn prompt_lane_anchor_task_id(rows: &[TaskSpecRow], _notes: &str) -> Result<String, String> {
    let task_ids = rows
        .iter()
        .map(|row| row.task_id.clone())
        .collect::<BTreeSet<_>>();
    if task_ids.is_empty() {
        return Err("runtime lane has no task rows".to_string());
    }

    task_ids
        .first()
        .cloned()
        .ok_or_else(|| "runtime lane has no task rows".to_string())
}

fn note_value(notes: &str, key: &str) -> Option<String> {
    notes
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(&format!("{key}=")).map(str::to_string))
}

fn collect_required_prs(rows: &[TaskRow], scope: &str) -> Result<Vec<u64>, String> {
    let mut errors = Vec::new();
    let mut prs = Vec::new();
    let mut seen = HashSet::new();

    for row in rows {
        match issue_body::parse_pr_number(&row.pr) {
            Some(number) => {
                if seen.insert(number) {
                    prs.push(number);
                }
            }
            None => errors.push(format!(
                "{}: {} requires concrete PR reference (found `{}`)",
                row.task,
                scope,
                row.pr.trim()
            )),
        }
    }

    if !errors.is_empty() {
        return Err(errors.join(" | "));
    }

    Ok(prs)
}

fn ensure_prs_merged(
    adapter: &impl GitHubAdapter,
    repo: &str,
    prs: &[u64],
    scope: &str,
) -> Result<(), String> {
    let mut errors = Vec::new();

    for pr in prs {
        let merged = adapter
            .pr_is_merged(repo, *pr)
            .map_err(|err| format!("failed to query PR #{pr}: {err}"))?;
        if !merged {
            errors.push(format!("{scope}: PR #{pr} is not merged"));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join(" | "))
    }
}

fn enforce_previous_sprint_gate(
    adapter: &impl GitHubAdapter,
    repo: &str,
    rows: &[TaskRow],
    sprint: i32,
) -> Result<(), String> {
    let previous = sprint - 1;
    let prev_rows: Vec<TaskRow> = rows
        .iter()
        .filter(|row| issue_body::row_sprint(row) == Some(previous))
        .cloned()
        .collect();

    if prev_rows.is_empty() {
        return Err(format!(
            "start-sprint gate: no rows found for previous sprint S{previous}"
        ));
    }

    let mut errors = Vec::new();
    for row in &prev_rows {
        if !row.status.trim().eq_ignore_ascii_case("done") {
            errors.push(format!(
                "{}: previous sprint gate requires Status=done (found `{}`)",
                row.task,
                row.status.trim()
            ));
        }
    }

    let prs = prev_rows
        .iter()
        .map(|row| {
            issue_body::parse_pr_number(&row.pr).ok_or_else(|| {
                format!(
                    "{}: previous sprint gate requires concrete PR reference (found `{}`)",
                    row.task,
                    row.pr.trim()
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut unique_prs = prs;
    unique_prs.sort_unstable();
    unique_prs.dedup();

    if let Err(err) = ensure_prs_merged(adapter, repo, &unique_prs, "previous-sprint-gate") {
        errors.push(err);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join(" | "))
    }
}

fn ensure_start_sprint_runtime_truth_matches_plan(
    issue_rows: &[TaskRow],
    sprint: i32,
    plan_rows: &[TaskSpecRow],
    strategy: SplitStrategy,
) -> Result<(), String> {
    let mut issue_rows_by_task: HashMap<String, &TaskRow> = HashMap::new();
    let mut issue_duplicates = Vec::new();
    for row in issue_rows {
        if issue_body::row_sprint(row) != Some(sprint) {
            continue;
        }
        let task_id = row.task.trim().to_string();
        if let Some(previous) = issue_rows_by_task.insert(task_id.clone(), row) {
            issue_duplicates.push(format!(
                "{task_id}: duplicate issue rows for sprint S{sprint} (line {} and line {})",
                previous.line_index + 1,
                row.line_index + 1
            ));
        }
    }

    let runtime_lane_metadata = task_spec::runtime_lane_metadata_by_task(plan_rows, strategy);
    let mut expected_by_task: HashMap<String, DriftComparableRow> = HashMap::new();
    for plan_row in plan_rows {
        let lane = runtime_lane_metadata
            .get(&plan_row.task_id)
            .ok_or_else(|| format!("{}: missing runtime lane metadata", plan_row.task_id))?;
        expected_by_task.insert(
            plan_row.task_id.clone(),
            DriftComparableRow {
                summary: plan_row.summary.trim().to_string(),
                owner: lane.owner.trim().to_string(),
                branch: lane.branch.trim().to_string(),
                worktree: lane.worktree.trim().to_string(),
                execution_mode: lane.execution_mode.trim().to_ascii_lowercase(),
                notes: common_markdown::canonicalize_table_cell(lane.notes.trim()),
            },
        );
    }

    let mut errors = issue_duplicates;

    for (task_id, expected) in &expected_by_task {
        let Some(issue_row) = issue_rows_by_task.get(task_id) else {
            errors.push(format!(
                "{task_id}: missing issue row for sprint S{sprint}; rerun start-plan to refresh runtime-truth rows"
            ));
            continue;
        };

        compare_drift_field(
            &mut errors,
            task_id,
            "Summary",
            issue_row.summary.trim(),
            &expected.summary,
        );
        compare_drift_field(
            &mut errors,
            task_id,
            "Owner",
            issue_row.owner.trim(),
            &expected.owner,
        );
        compare_drift_field(
            &mut errors,
            task_id,
            "Branch",
            issue_row.branch.trim(),
            &expected.branch,
        );
        compare_drift_field(
            &mut errors,
            task_id,
            "Worktree",
            issue_row.worktree.trim(),
            &expected.worktree,
        );
        compare_drift_field(
            &mut errors,
            task_id,
            "Execution Mode",
            &issue_row.execution_mode.trim().to_ascii_lowercase(),
            &expected.execution_mode,
        );
        compare_drift_field(
            &mut errors,
            task_id,
            "Notes",
            &common_markdown::canonicalize_table_cell(issue_row.notes.trim()),
            &expected.notes,
        );
    }

    for task_id in issue_rows_by_task.keys() {
        if !expected_by_task.contains_key(task_id) {
            errors.push(format!(
                "{task_id}: issue row exists for sprint S{sprint} but is absent from current plan split output"
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join(" | "))
    }
}

#[derive(Debug)]
struct DriftComparableRow {
    summary: String,
    owner: String,
    branch: String,
    worktree: String,
    execution_mode: String,
    notes: String,
}

fn compare_drift_field(
    errors: &mut Vec<String>,
    task_id: &str,
    field: &str,
    actual: &str,
    expected: &str,
) {
    if actual != expected {
        errors.push(format!(
            "{task_id}: {field} drift (issue `{actual}` != plan `{expected}`)"
        ));
    }
}

#[derive(Debug, Default)]
struct CleanupOutcome {
    targeted: Vec<String>,
    removed: Vec<String>,
    residual: Vec<String>,
}

#[derive(Debug, Clone)]
struct LinkedWorktree {
    path: PathBuf,
    branch: Option<String>,
}

fn cleanup_worktrees_from_rows(rows: &[TaskRow], dry_run: bool) -> Result<CleanupOutcome, String> {
    let repo_root = repo_root()?;
    let cwd = std::env::current_dir()
        .map_err(|err| format!("failed to read current directory: {err}"))?;

    let mut branch_targets: HashSet<String> = HashSet::new();
    let mut path_targets: HashSet<String> = HashSet::new();

    for row in rows {
        if !issue_body::is_placeholder(&row.branch) {
            branch_targets.insert(normalize_branch_name(&row.branch));
        }
        if !issue_body::is_placeholder(&row.worktree) {
            let resolved = resolve_worktree_path(&repo_root, row.worktree.trim());
            path_targets.insert(path_key(&resolved));
        }
    }

    let linked = list_linked_worktrees()?;
    let repo_root_key = path_key(&repo_root);

    let mut outcome = CleanupOutcome::default();

    for worktree in linked {
        let worktree_key = path_key(&worktree.path);
        let branch_key = worktree.branch.as_ref().map(|b| normalize_branch_name(b));

        let targeted = path_targets.contains(&worktree_key)
            || branch_key
                .as_ref()
                .is_some_and(|branch| branch_targets.contains(branch));

        if !targeted {
            continue;
        }

        outcome
            .targeted
            .push(worktree.path.to_string_lossy().to_string());

        if worktree_key == repo_root_key {
            continue;
        }

        if cwd == worktree.path || cwd.starts_with(&worktree.path) {
            outcome
                .residual
                .push(worktree.path.to_string_lossy().to_string());
            continue;
        }

        if dry_run {
            outcome
                .removed
                .push(worktree.path.to_string_lossy().to_string());
            continue;
        }

        let worktree_path = worktree.path.to_string_lossy().to_string();
        let status = common_git::run_status_inherit(&[
            "worktree",
            "remove",
            "--force",
            worktree_path.as_str(),
        ])
        .map_err(|err| format!("failed to execute `git worktree remove`: {err}"))?;

        if status.success() {
            outcome
                .removed
                .push(worktree.path.to_string_lossy().to_string());
        } else {
            outcome
                .residual
                .push(worktree.path.to_string_lossy().to_string());
        }
    }

    if !dry_run {
        let prune_status = common_git::run_status_inherit(&["worktree", "prune"])
            .map_err(|err| format!("failed to execute `git worktree prune`: {err}"))?;
        if !prune_status.success() {
            return Err("git worktree prune failed".to_string());
        }

        let remaining = list_linked_worktrees()?;
        for worktree in remaining {
            let worktree_key = path_key(&worktree.path);
            let branch_key = worktree.branch.as_ref().map(|b| normalize_branch_name(b));
            let targeted = path_targets.contains(&worktree_key)
                || branch_key
                    .as_ref()
                    .is_some_and(|branch| branch_targets.contains(branch));

            if targeted && worktree_key != repo_root_key {
                let path = worktree.path.to_string_lossy().to_string();
                if !outcome.residual.contains(&path) {
                    outcome.residual.push(path);
                }
            }
        }

        if !outcome.residual.is_empty() {
            return Err(format!(
                "cleanup left targeted residual worktrees: {}",
                outcome.residual.join(", ")
            ));
        }
    }

    outcome.targeted.sort();
    outcome.targeted.dedup();
    outcome.removed.sort();
    outcome.removed.dedup();

    Ok(outcome)
}

fn list_linked_worktrees() -> Result<Vec<LinkedWorktree>, String> {
    let output = common_git::run_output(&["worktree", "list", "--porcelain"])
        .map_err(|err| format!("failed to run `git worktree list --porcelain`: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "`git worktree list --porcelain` failed: {}",
            if stderr.is_empty() {
                "unknown error"
            } else {
                &stderr
            }
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut rows = Vec::new();
    let mut current: Option<LinkedWorktree> = None;

    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(prev) = current.take() {
                rows.push(prev);
            }
            current = Some(LinkedWorktree {
                path: PathBuf::from(path.trim()),
                branch: None,
            });
            continue;
        }

        if let Some(branch) = line.strip_prefix("branch ")
            && let Some(current) = current.as_mut()
        {
            current.branch = Some(branch.trim().trim_start_matches("refs/heads/").to_string());
            continue;
        }

        if line.trim().is_empty()
            && let Some(prev) = current.take()
        {
            rows.push(prev);
        }
    }

    if let Some(prev) = current {
        rows.push(prev);
    }

    Ok(rows)
}

fn repo_root() -> Result<PathBuf, String> {
    common_git::repo_root()
        .map_err(|err| format!("failed to run `git rev-parse --show-toplevel`: {err}"))?
        .ok_or_else(|| "unable to resolve repository root".to_string())
}

fn resolve_worktree_path(repo_root: &Path, worktree: &str) -> PathBuf {
    let path = PathBuf::from(worktree);
    if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    }
}

fn normalize_branch_name(branch: &str) -> String {
    branch.trim().trim_start_matches("refs/heads/").to_string()
}

fn path_key(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::commands::plan::CloseReason;
    use crate::commands::{
        CommentModeArgs, CommentTextArgs, PrGroupMapping, PrGrouping, SplitStrategy,
    };
    use nils_test_support::git::{InitRepoOptions, git, init_repo_with};
    use nils_test_support::{CwdGuard, EnvGuard, GlobalStateLock};
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    static LINKED_WORKTREE_SEQ: AtomicU64 = AtomicU64::new(0);

    fn task_row(
        task: &str,
        branch: &str,
        worktree: &str,
        pr: &str,
        status: &str,
        notes: &str,
    ) -> TaskRow {
        TaskRow {
            task: task.to_string(),
            summary: format!("Summary for {task}"),
            owner: "subagent-owner".to_string(),
            branch: branch.to_string(),
            worktree: worktree.to_string(),
            execution_mode: "per-sprint".to_string(),
            pr: pr.to_string(),
            status: status.to_string(),
            notes: notes.to_string(),
            line_index: 0,
        }
    }

    fn note_value(notes: &str, key: &str) -> Option<String> {
        notes
            .split(';')
            .map(str::trim)
            .find_map(|part| part.strip_prefix(&format!("{key}=")).map(str::to_string))
    }

    #[derive(Default)]
    struct MockGitHubAdapter {
        merged: HashMap<u64, Result<bool, String>>,
    }

    impl MockGitHubAdapter {
        fn with_merge(mut self, pr: u64, result: Result<bool, String>) -> Self {
            self.merged.insert(pr, result);
            self
        }
    }

    impl GitHubAdapter for MockGitHubAdapter {
        fn issue_body(&self, _repo: &str, _issue: u64) -> Result<String, String> {
            unreachable!("issue_body is not needed in this test")
        }

        fn create_issue(
            &self,
            _repo: &str,
            _title: &str,
            _body_file: &Path,
            _labels: &[String],
        ) -> Result<(u64, String), String> {
            unreachable!("create_issue is not needed in this test")
        }

        fn edit_issue_body(
            &self,
            _repo: &str,
            _issue: u64,
            _body_file: &Path,
        ) -> Result<(), String> {
            unreachable!("edit_issue_body is not needed in this test")
        }

        fn comment_issue(&self, _repo: &str, _issue: u64, _body_file: &Path) -> Result<(), String> {
            unreachable!("comment_issue is not needed in this test")
        }

        fn edit_issue_labels(
            &self,
            _repo: &str,
            _issue: u64,
            _add_labels: &[String],
            _remove_labels: &[String],
        ) -> Result<(), String> {
            unreachable!("edit_issue_labels is not needed in this test")
        }

        fn close_issue(
            &self,
            _repo: &str,
            _issue: u64,
            _reason: CloseReason,
            _close_comment: Option<&str>,
        ) -> Result<(), String> {
            unreachable!("close_issue is not needed in this test")
        }

        fn pr_is_merged(&self, _repo: &str, pr: u64) -> Result<bool, String> {
            self.merged.get(&pr).cloned().unwrap_or(Ok(true))
        }
    }

    #[test]
    fn helper_url_validation_and_comment_mode_are_stable() {
        assert!(approval_comment_url_looks_valid(
            "https://github.com/sympoies/nils-cli/issues/217#issuecomment-123"
        ));
        assert!(approval_comment_url_looks_valid(
            "https://github.com/sympoies/nils-cli/pull/221#issuecomment-456"
        ));
        assert!(!approval_comment_url_looks_valid(
            "https://example.com/issues/217#issuecomment-123"
        ));
        assert!(!approval_comment_url_looks_valid(
            "https://github.com/sympoies/nils-cli/issues/217#comment-123"
        ));

        assert!(should_emit_comment(&CommentModeArgs {
            comment: false,
            no_comment: false,
        }));
        assert!(!should_emit_comment(&CommentModeArgs {
            comment: true,
            no_comment: true,
        }));
    }

    #[test]
    fn summary_and_close_comment_loaders_cover_inline_file_and_error() {
        let tmp = TempDir::new().expect("tempdir");

        let summary = SummaryArgs {
            summary: Some("inline summary".to_string()),
            summary_file: None,
        };
        assert_eq!(
            load_summary(&summary).expect("inline summary"),
            Some("inline summary".to_string())
        );

        let summary_file = tmp.path().join("summary.md");
        fs::write(&summary_file, "file summary").expect("write summary");
        let from_file = SummaryArgs {
            summary: None,
            summary_file: Some(summary_file.clone()),
        };
        assert_eq!(
            load_summary(&from_file).expect("summary file"),
            Some("file summary".to_string())
        );

        let missing_summary = SummaryArgs {
            summary: None,
            summary_file: Some(tmp.path().join("missing-summary.md")),
        };
        let err = load_summary(&missing_summary).expect_err("missing summary should error");
        assert_eq!(err.code, "summary-read-failed");

        let close_inline = CommentTextArgs {
            comment: Some("inline close".to_string()),
            comment_file: None,
        };
        assert_eq!(
            load_close_comment(&close_inline).expect("inline close comment"),
            Some("inline close".to_string())
        );

        let close_file = tmp.path().join("close.md");
        fs::write(&close_file, "file close").expect("write close");
        let close_from_file = CommentTextArgs {
            comment: None,
            comment_file: Some(close_file),
        };
        assert_eq!(
            load_close_comment(&close_from_file).expect("file close comment"),
            Some("file close".to_string())
        );
    }

    #[test]
    fn resolve_repo_for_live_and_binary_guards_work_as_expected() {
        assert!(ensure_live_binary(BinaryFlavor::PlanIssue).is_ok());
        let local_only = ensure_live_binary(BinaryFlavor::PlanIssueLocal).expect_err("must fail");
        assert_eq!(local_only.code, "live-command-unavailable");
        assert!(
            local_only.message.contains("plan-issue <command>"),
            "{}",
            local_only.message
        );

        let local_status = ensure_live_binary_for_command(
            BinaryFlavor::PlanIssueLocal,
            "status-plan --issue <number>",
            Some("plan-issue-local status-plan --body-file <path> --dry-run"),
        )
        .expect_err("local command-specific guard should fail");
        assert_eq!(local_status.code, "live-command-unavailable");
        assert!(
            local_status
                .message
                .contains("status-plan --issue <number>"),
            "{}",
            local_status.message
        );
        assert!(
            local_status
                .message
                .contains("status-plan --body-file <path> --dry-run"),
            "{}",
            local_status.message
        );

        assert_eq!(
            resolve_repo_for_live(BinaryFlavor::PlanIssue, Some("sympoies/nils-cli"))
                .expect("valid repo"),
            "sympoies/nils-cli"
        );

        let invalid_repo =
            resolve_repo_for_live(BinaryFlavor::PlanIssue, Some("https://example.com/repo"))
                .expect_err("invalid override should fail");
        assert_eq!(invalid_repo.code, "repo-resolution-failed");

        let local_repo = resolve_repo_for_live(BinaryFlavor::PlanIssueLocal, Some("foo/bar"))
            .expect_err("local binary should fail before resolving repo");
        assert_eq!(local_repo.code, "live-command-unavailable");
    }

    #[test]
    fn render_status_and_build_options_helpers_are_deterministic() {
        let rows = vec![
            task_row("S1T1", "issue/s1-t1", "wt-1", "#1", "planned", "sprint=S1"),
            task_row(
                "S1T2",
                "issue/s1-t2",
                "wt-2",
                "#2",
                "in-progress",
                "sprint=S1",
            ),
            task_row("S1T3", "issue/s1-t3", "wt-3", "#3", "done", "sprint=S1"),
        ];
        let comment = render_plan_status_comment(&rows);
        assert!(comment.contains("- Total tasks: 3"), "{comment}");
        assert!(comment.contains("- planned: 1"), "{comment}");
        assert!(comment.contains("- in-progress: 1"), "{comment}");
        assert!(comment.contains("- done: 1"), "{comment}");

        let options = to_build_options(
            "owner".to_string(),
            "branch".to_string(),
            "worktree".to_string(),
            Some(PrGrouping::Group),
            None,
            crate::commands::SplitStrategy::Auto,
            vec![PrGroupMapping {
                task: "S1T1".to_string(),
                group: "g1".to_string(),
            }],
        );
        assert_eq!(options.owner_prefix, "owner");
        assert_eq!(options.branch_prefix, "branch");
        assert_eq!(options.worktree_prefix, "worktree");
        assert_eq!(options.pr_grouping, Some(PrGrouping::Group));
        assert_eq!(options.strategy, SplitStrategy::Auto);
        assert_eq!(options.pr_group.len(), 1);
    }

    #[test]
    fn collect_required_prs_and_merge_checks_cover_success_and_errors() {
        let rows = vec![
            task_row("S1T1", "issue/s1-t1", "wt-1", "#12", "done", "sprint=S1"),
            task_row("S1T2", "issue/s1-t2", "wt-2", "12", "done", "sprint=S1"),
        ];
        assert_eq!(
            collect_required_prs(&rows, "close-plan").expect("dedup"),
            vec![12]
        );

        let bad_rows = vec![task_row(
            "S1T3",
            "issue/s1-t3",
            "wt-3",
            "TBD",
            "done",
            "sprint=S1",
        )];
        let err = collect_required_prs(&bad_rows, "close-plan").expect_err("missing pr");
        assert!(err.contains("requires concrete PR reference"), "{err}");

        let adapter_ok = MockGitHubAdapter::default().with_merge(12, Ok(true));
        ensure_prs_merged(&adapter_ok, "sympoies/nils-cli", &[12], "scope").expect("merged");

        let adapter_unmerged = MockGitHubAdapter::default().with_merge(12, Ok(false));
        let unmerged = ensure_prs_merged(&adapter_unmerged, "sympoies/nils-cli", &[12], "scope")
            .expect_err("unmerged should fail");
        assert!(
            unmerged.contains("scope: PR #12 is not merged"),
            "{unmerged}"
        );

        let adapter_error =
            MockGitHubAdapter::default().with_merge(12, Err("gh failure".to_string()));
        let query_err = ensure_prs_merged(&adapter_error, "sympoies/nils-cli", &[12], "scope")
            .expect_err("query failure should fail");
        assert!(
            query_err.contains("failed to query PR #12: gh failure"),
            "{query_err}"
        );
    }

    #[test]
    fn previous_sprint_gate_enforces_status_pr_and_merge_requirements() {
        let rows_ok = vec![
            task_row("S1T1", "issue/s1-t1", "wt-1", "#11", "done", "sprint=S1"),
            task_row("S2T1", "issue/s2-t1", "wt-2", "#21", "planned", "sprint=S2"),
        ];
        let adapter_ok = MockGitHubAdapter::default().with_merge(11, Ok(true));
        enforce_previous_sprint_gate(&adapter_ok, "sympoies/nils-cli", &rows_ok, 2)
            .expect("gate should pass");

        let no_prev = enforce_previous_sprint_gate(
            &adapter_ok,
            "sympoies/nils-cli",
            &[task_row(
                "S2T1",
                "issue/s2-t1",
                "wt-2",
                "#21",
                "planned",
                "sprint=S2",
            )],
            2,
        )
        .expect_err("missing previous sprint rows");
        assert!(
            no_prev.contains("no rows found for previous sprint S1"),
            "{no_prev}"
        );

        let status_err_rows = vec![task_row(
            "S1T1",
            "issue/s1-t1",
            "wt-1",
            "#11",
            "in-progress",
            "sprint=S1",
        )];
        let status_err =
            enforce_previous_sprint_gate(&adapter_ok, "sympoies/nils-cli", &status_err_rows, 2)
                .expect_err("status gate must fail");
        assert!(status_err.contains("requires Status=done"), "{status_err}");

        let pr_err_rows = vec![task_row(
            "S1T1",
            "issue/s1-t1",
            "wt-1",
            "TBD",
            "done",
            "sprint=S1",
        )];
        let pr_err =
            enforce_previous_sprint_gate(&adapter_ok, "sympoies/nils-cli", &pr_err_rows, 2)
                .expect_err("PR gate must fail");
        assert!(
            pr_err.contains("requires concrete PR reference"),
            "{pr_err}"
        );

        let adapter_unmerged = MockGitHubAdapter::default().with_merge(11, Ok(false));
        let unmerged =
            enforce_previous_sprint_gate(&adapter_unmerged, "sympoies/nils-cli", &rows_ok, 2)
                .expect_err("merge gate must fail");
        assert!(unmerged.contains("PR #11 is not merged"), "{unmerged}");
    }

    fn setup_repo_with_linked_worktree() -> (TempDir, PathBuf) {
        let repo = init_repo_with(InitRepoOptions::new().with_initial_commit());
        git(repo.path(), &["checkout", "-b", "issue/s1-t1"]);
        git(repo.path(), &["checkout", "main"]);

        let unique = LINKED_WORKTREE_SEQ.fetch_add(1, Ordering::Relaxed);
        let linked_path =
            std::env::temp_dir().join(format!("linked-s1-t1-{}-{unique}", std::process::id()));
        let _ = fs::remove_dir_all(&linked_path);
        let linked_s = linked_path.to_string_lossy().to_string();
        git(repo.path(), &["worktree", "add", &linked_s, "issue/s1-t1"]);
        (repo, linked_path)
    }

    #[test]
    fn linked_worktree_listing_and_cleanup_modes_are_covered() {
        let lock = GlobalStateLock::new();
        let (repo, linked_path) = setup_repo_with_linked_worktree();
        let _cwd = CwdGuard::set(&lock, repo.path()).expect("set cwd");

        let listed = list_linked_worktrees().expect("list worktrees");
        let listed_paths = listed
            .iter()
            .map(|entry| path_key(&entry.path))
            .collect::<Vec<_>>();
        assert!(listed_paths.contains(&path_key(repo.path())));
        assert!(listed_paths.contains(&path_key(&linked_path)));

        let linked = linked_path.to_string_lossy().to_string();
        let rows = vec![
            task_row(
                "S1T1",
                "issue/s1-t1",
                &linked,
                "#11",
                "done",
                "sprint=S1; pr-group=s1-auto-g1; shared-pr-anchor=S1T2",
            ),
            task_row(
                "S1T2",
                "issue/s1-t1",
                &linked,
                "#11",
                "done",
                "sprint=S1; pr-group=s1-auto-g1; shared-pr-anchor=S1T2",
            ),
        ];

        let dry_run = cleanup_worktrees_from_rows(&rows, true).expect("dry-run cleanup");
        assert_eq!(
            dry_run
                .targeted
                .iter()
                .filter(|path| path.contains("linked-s1-t1"))
                .count(),
            1
        );
        assert!(dry_run.targeted.iter().any(|p| p.contains("linked-s1-t1")));
        assert!(dry_run.removed.iter().any(|p| p.contains("linked-s1-t1")));
        assert!(linked_path.exists(), "dry-run must not remove worktree");

        let real = cleanup_worktrees_from_rows(&rows, false).expect("real cleanup");
        assert!(real.removed.iter().any(|p| p.contains("linked-s1-t1")));
        assert!(
            !linked_path.exists(),
            "cleanup should remove linked worktree"
        );
    }

    #[test]
    fn cleanup_skips_current_worktree_root_path() {
        let lock = GlobalStateLock::new();
        let (repo, linked_path) = setup_repo_with_linked_worktree();
        let _cwd = CwdGuard::set(&lock, &linked_path).expect("set cwd");

        let rows = vec![task_row(
            "S1T1",
            "issue/s1-t1",
            linked_path.to_string_lossy().as_ref(),
            "#11",
            "done",
            "sprint=S1",
        )];
        let outcome =
            cleanup_worktrees_from_rows(&rows, false).expect("current worktree path is skipped");
        assert!(outcome.targeted.iter().any(|p| p.contains("linked-s1-t1")));
        assert!(outcome.removed.is_empty());
        assert!(outcome.residual.is_empty());

        let _reset = CwdGuard::set(&lock, repo.path()).expect("reset cwd");
        let cleanup = cleanup_worktrees_from_rows(&rows, false).expect("cleanup after reset");
        assert!(cleanup.removed.iter().any(|p| p.contains("linked-s1-t1")));
    }

    #[test]
    fn temp_markdown_and_prompt_outputs_use_agent_home_and_expected_paths() {
        let lock = GlobalStateLock::new();
        let tmp = TempDir::new().expect("tempdir");
        let _agent_home = EnvGuard::set(&lock, "AGENT_HOME", tmp.path().to_string_lossy().as_ref());

        let markdown = write_temp_markdown("status", "hello").expect("write temp markdown");
        assert!(
            markdown
                .to_string_lossy()
                .contains("plan-issue-delivery/tmp")
        );
        assert_eq!(
            fs::read_to_string(&markdown).expect("read markdown"),
            "hello"
        );

        let issue_root = runtime_layout::IssueRoot::new("owner__sample", 7).expect("issue root");
        let sprint_root = runtime_layout::SprintRoot::new(&issue_root, 3);
        let prompts_path = sprint_root.prompts_dir();
        assert!(
            prompts_path
                .to_string_lossy()
                .contains("/owner__sample/issue-7/sprint-3/prompts")
        );

        let out_dir = tmp.path().join("out").join("sprint-3").join("prompts");
        let rows = vec![TaskSpecRow {
            task_id: "S3T1".to_string(),
            summary: "Build feature".to_string(),
            branch: "issue/s3-t1".to_string(),
            worktree: "issue-s3-t1".to_string(),
            owner: "subagent-s3-t1".to_string(),
            notes: "sprint=S3".to_string(),
            pr_group: "s3".to_string(),
            sprint: 3,
            grouping: PrGrouping::PerSprint,
        }];
        let files = write_subagent_prompts(&out_dir, 217, 3, &rows, SplitStrategy::Deterministic)
            .expect("write prompts");
        assert_eq!(files.len(), 1);
        let rendered = fs::read_to_string(&files[0]).expect("read prompt");
        assert!(rendered.contains("Issue: #217"), "{rendered}");
        assert!(rendered.contains("Task: S3T1"), "{rendered}");
        assert!(rendered.contains("Tasks: S3T1"), "{rendered}");
        assert!(
            rendered.contains("Execution Mode: per-sprint"),
            "{rendered}"
        );
        let file_path = std::path::Path::new(&files[0]);
        assert_eq!(
            file_path.file_name().and_then(|name| name.to_str()),
            Some("S3T1.md")
        );
    }

    #[test]
    fn write_subagent_prompts_groups_tasks_by_runtime_lane() {
        let tmp = TempDir::new().expect("tempdir");
        let out_dir = tmp.path().join("subagent-prompts");
        let rows = vec![
            TaskSpecRow {
                task_id: "S3T1".to_string(),
                summary: "First lane task".to_string(),
                branch: "issue/s3-t1".to_string(),
                worktree: "issue-s3-t1".to_string(),
                owner: "subagent-s3-t1".to_string(),
                notes: "sprint=S3; pr-group=s3-auto-g1; shared-pr-anchor=S3T2".to_string(),
                pr_group: "s3-auto-g1".to_string(),
                sprint: 3,
                grouping: PrGrouping::Group,
            },
            TaskSpecRow {
                task_id: "S3T2".to_string(),
                summary: "Second lane task".to_string(),
                branch: "issue/s3-t2".to_string(),
                worktree: "issue-s3-t2".to_string(),
                owner: "subagent-s3-t2".to_string(),
                notes: "sprint=S3; pr-group=s3-auto-g1; shared-pr-anchor=S3T2".to_string(),
                pr_group: "s3-auto-g1".to_string(),
                sprint: 3,
                grouping: PrGrouping::Group,
            },
            TaskSpecRow {
                task_id: "S3T3".to_string(),
                summary: "Isolated task".to_string(),
                branch: "issue/s3-t3".to_string(),
                worktree: "issue-s3-t3".to_string(),
                owner: "subagent-s3-t3".to_string(),
                notes: "sprint=S3; pr-group=s3-auto-g2".to_string(),
                pr_group: "s3-auto-g2".to_string(),
                sprint: 3,
                grouping: PrGrouping::Group,
            },
        ];

        let files = write_subagent_prompts(&out_dir, 217, 3, &rows, SplitStrategy::Auto)
            .expect("write grouped prompts");
        assert_eq!(files.len(), 3, "one prompt file per task: {files:?}");

        for task_id in ["S3T1", "S3T2", "S3T3"] {
            assert!(
                files
                    .iter()
                    .any(|path| path.ends_with(&format!("/{task_id}.md"))),
                "missing prompt for {task_id}: {files:?}"
            );
        }

        let lane_prompt_path = files
            .iter()
            .find(|path| path.ends_with("/S3T1.md"))
            .expect("shared lane prompt for S3T1");
        let lane_prompt = fs::read_to_string(lane_prompt_path).expect("read shared lane prompt");
        assert!(lane_prompt.contains("Task: S3T1"), "{lane_prompt}");
        assert!(lane_prompt.contains("Tasks: S3T1, S3T2"), "{lane_prompt}");
        assert!(
            lane_prompt.contains("Execution Mode: pr-shared"),
            "{lane_prompt}"
        );
        assert!(
            lane_prompt.contains("Owner: subagent-s3-t1"),
            "{lane_prompt}"
        );
        assert!(lane_prompt.contains("Branch: issue/s3-t1"), "{lane_prompt}");
        assert!(
            lane_prompt.contains("Worktree: issue-s3-t1"),
            "{lane_prompt}"
        );
        assert!(
            lane_prompt.contains("- S3T1: First lane task"),
            "{lane_prompt}"
        );
        assert!(
            lane_prompt.contains("- S3T2: Second lane task"),
            "{lane_prompt}"
        );

        let isolated_prompt_path = files
            .iter()
            .find(|path| path.ends_with("/S3T3.md"))
            .expect("isolated lane prompt");
        let isolated_prompt =
            fs::read_to_string(isolated_prompt_path).expect("read isolated lane prompt");
        assert!(isolated_prompt.contains("Tasks: S3T3"), "{isolated_prompt}");
        assert!(
            isolated_prompt.contains("Execution Mode: pr-isolated"),
            "{isolated_prompt}"
        );
    }

    #[test]
    fn task_spec_from_issue_rows_preserves_runtime_truth_metadata() {
        let rows = vec![
            TaskRow {
                task: "S3T1".to_string(),
                summary: "First lane task".to_string(),
                owner: "subagent-s3-anchor".to_string(),
                branch: "issue/s3-shared".to_string(),
                worktree: "issue-s3-shared".to_string(),
                execution_mode: "per-sprint".to_string(),
                pr: "TBD".to_string(),
                status: "planned".to_string(),
                notes: "sprint=S3; plan-task:Task 3.1; pr-group=s3-auto-g1; shared-pr-anchor=S3T2"
                    .to_string(),
                line_index: 0,
            },
            TaskRow {
                task: "S3T2".to_string(),
                summary: "Second lane task".to_string(),
                owner: "subagent-s3-anchor".to_string(),
                branch: "issue/s3-shared".to_string(),
                worktree: "issue-s3-shared".to_string(),
                execution_mode: "per-sprint".to_string(),
                pr: "TBD".to_string(),
                status: "planned".to_string(),
                notes: "sprint=S3; plan-task:Task 3.2; pr-group=s3-auto-g1; shared-pr-anchor=S3T2"
                    .to_string(),
                line_index: 1,
            },
            TaskRow {
                task: "S4T1".to_string(),
                summary: "Other sprint".to_string(),
                owner: "subagent-s4".to_string(),
                branch: "issue/s4".to_string(),
                worktree: "issue-s4".to_string(),
                execution_mode: "pr-isolated".to_string(),
                pr: "TBD".to_string(),
                status: "planned".to_string(),
                notes: "sprint=S4; plan-task:Task 4.1".to_string(),
                line_index: 2,
            },
        ];

        let scoped = task_spec_rows_from_issue_rows(&rows, 3).expect("sprint rows");
        assert_eq!(scoped.len(), 2);
        assert_eq!(scoped[0].task_id, "S3T1");
        assert_eq!(scoped[1].task_id, "S3T2");
        assert_eq!(scoped[0].owner, "subagent-s3-anchor");
        assert_eq!(scoped[1].owner, "subagent-s3-anchor");
        assert_eq!(scoped[0].branch, "issue/s3-shared");
        assert_eq!(scoped[1].branch, "issue/s3-shared");
        assert_eq!(scoped[0].worktree, "issue-s3-shared");
        assert_eq!(scoped[1].worktree, "issue-s3-shared");
        assert_eq!(scoped[0].grouping, PrGrouping::PerSprint);
        assert_eq!(scoped[1].grouping, PrGrouping::PerSprint);
        assert_eq!(scoped[0].pr_group, "s3-auto-g1");
        assert_eq!(scoped[1].pr_group, "s3-auto-g1");
        assert_eq!(
            note_value(&scoped[0].notes, "shared-pr-anchor"),
            Some("S3T2".to_string())
        );
        assert_eq!(
            note_value(&scoped[1].notes, "shared-pr-anchor"),
            Some("S3T2".to_string())
        );
    }

    #[test]
    fn path_normalization_helpers_are_stable() {
        let repo_root = PathBuf::from("/tmp/repo-root");
        assert_eq!(
            resolve_worktree_path(&repo_root, "issue-s1-t1"),
            repo_root.join("issue-s1-t1")
        );
        assert_eq!(
            resolve_worktree_path(&repo_root, "/tmp/issue-s1-t1"),
            PathBuf::from("/tmp/issue-s1-t1")
        );

        assert_eq!(
            normalize_branch_name("refs/heads/issue/s1-t1"),
            "issue/s1-t1"
        );
        assert_eq!(normalize_branch_name(" issue/s1-t2 "), "issue/s1-t2");
    }
}

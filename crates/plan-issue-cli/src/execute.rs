use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use nils_common::git as common_git;
use nils_common::markdown as common_markdown;
use nils_markdown::Engine;
use plan_tooling::parse::parse_plan_with_display;
use serde::Serialize;
use serde_json::{Value, json};

const PLAN_STATUS_COMMENT_TEMPLATE: &str =
    include_str!("../templates/execute/plan_status_comment.md.tera");
const PLAN_STATUS_COMMENT_TEMPLATE_NAME: &str = "execute_plan_status_comment";

const SUBAGENT_PROMPT_TEMPLATE: &str = include_str!("../templates/execute/subagent_prompt.md.tera");
const SUBAGENT_PROMPT_TEMPLATE_NAME: &str = "execute_subagent_prompt";

#[derive(Debug, Serialize)]
struct PlanStatusCommentView {
    total: usize,
    planned: usize,
    in_progress: usize,
    blocked: usize,
    done: usize,
}

#[derive(Debug, Serialize)]
struct SubagentPromptView<'a> {
    issue: u64,
    sprint: i32,
    task: &'a str,
    anchor_task: &'a str,
    task_list: &'a str,
    task_summary: &'a str,
    owner: &'a str,
    branch: &'a str,
    worktree: &'a str,
    execution_mode: &'a str,
    notes: &'a str,
    lane_tasks: &'a str,
}

use crate::cli::Cli;
use crate::commands::build::{BuildPlanTaskSpecArgs, BuildTaskSpecArgs};
use crate::commands::plan::{
    CleanupWorktreesArgs, ClosePlanArgs, LinkPrArgs, LinkPrStatus, ReadyPlanArgs,
    ResolveApprovalArgs, StartPlanArgs, StatusPlanArgs,
};
use crate::commands::record::{
    LifecycleCommentKind, RecordArgs, RecordAttachArgs, RecordAuditArgs, RecordCloseArgs,
    RecordCommand, RecordOpenArgs, RecordPostArgs, RecordRepairDashboardArgs, RecordTemplateArgs,
    TemplateFormatArg,
};
use crate::commands::sprint::{
    AcceptSprintArgs, MultiSprintGuideArgs, ReadySprintArgs, StartSprintArgs,
};
use crate::commands::{Command as CliCommand, SplitStrategy, SummaryArgs};
use crate::dispatch_record::{self, DispatchRecord};
use crate::github::ProviderAdapter;
use crate::issue_body::{self, TaskRow};
use crate::lifecycle_record::{self, DashboardInput};
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
        CliCommand::ResolveApproval(args) => {
            run_resolve_approval_json(binary, cli.force, cli.repo.as_deref(), args)
        }
        CliCommand::Record(args) => {
            run_record(binary, cli.dry_run, cli.force, cli.repo.as_deref(), args)
        }
        CliCommand::Tracking(args) => run_tracking(binary, cli.force, cli.repo.as_deref(), args),
        CliCommand::Completion(_) => Err(CommandError::usage(
            "completion-direct-output-only",
            "completion output is emitted directly; run `<binary> completion <bash|zsh>`",
        )),
    }
}

fn run_record(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &RecordArgs,
) -> Result<Value, CommandError> {
    match &args.command {
        RecordCommand::Open(args) => run_record_open(binary, dry_run, force, repo_override, args),
        RecordCommand::Attach(args) => {
            run_record_attach(binary, dry_run, force, repo_override, args)
        }
        RecordCommand::Post(args) => run_record_post(binary, dry_run, force, repo_override, args),
        RecordCommand::RepairDashboard(args) => {
            run_record_repair_dashboard(binary, dry_run, force, repo_override, args)
        }
        RecordCommand::Close(args) => run_record_close(binary, dry_run, force, repo_override, args),
        RecordCommand::Audit(args) => run_record_audit(args),
        RecordCommand::Template(args) => run_record_template(args),
        RecordCommand::Restore(args) => run_record_restore(force, repo_override, args),
    }
}

#[derive(Debug, Clone)]
struct RecordBundle {
    source_file: PathBuf,
    plan_file: PathBuf,
    /// Resolved when present; `None` only when the caller explicitly opted out.
    #[allow(dead_code)]
    execution_state_file: Option<PathBuf>,
}

struct RecordSeed {
    plan_title: String,
    source_path: String,
    plan_path: String,
    source_commit: String,
    plan_commit: String,
    source_body: String,
    plan_body: String,
    state_body: String,
}

fn resolve_record_bundle(
    bundle: Option<&Path>,
    explicit_source: Option<&Path>,
    explicit_plan: Option<&Path>,
    explicit_execution_state: Option<&Path>,
) -> Result<RecordBundle, CommandError> {
    if let (Some(source), Some(plan)) = (explicit_source, explicit_plan) {
        return Ok(RecordBundle {
            source_file: source.to_path_buf(),
            plan_file: plan.to_path_buf(),
            execution_state_file: explicit_execution_state.map(Path::to_path_buf),
        });
    }

    let bundle_dir = bundle.ok_or_else(|| {
        CommandError::usage(
            "record-open-missing-bundle",
            "either --bundle <dir> or both --source-file and --plan-file are required",
        )
    })?;
    if !bundle_dir.is_dir() {
        return Err(CommandError::usage(
            "record-open-bundle-not-dir",
            format!("--bundle path is not a directory: {}", bundle_dir.display()),
        ));
    }

    let mut plan_file: Option<PathBuf> = None;
    let mut source_file: Option<PathBuf> = None;
    let mut execution_state_file: Option<PathBuf> = None;
    let entries = fs::read_dir(bundle_dir).map_err(|err| {
        CommandError::runtime(
            "record-open-bundle-read-failed",
            format!("failed to read bundle dir {}: {err}", bundle_dir.display()),
        )
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with("-plan.md") {
            plan_file = Some(path.clone());
        } else if name.ends_with("-discussion-source.md") || name.ends_with("-review-source.md") {
            source_file = Some(path.clone());
        } else if name.ends_with("-execution-state.md") {
            execution_state_file = Some(path.clone());
        }
    }

    let plan_file = explicit_plan
        .map(Path::to_path_buf)
        .or(plan_file)
        .ok_or_else(|| {
            CommandError::usage(
                "record-open-missing-plan",
                format!(
                    "no <slug>-plan.md found in bundle {} and no --plan-file provided",
                    bundle_dir.display()
                ),
            )
        })?;
    let source_file = explicit_source
        .map(Path::to_path_buf)
        .or(source_file)
        .ok_or_else(|| {
            CommandError::usage(
                "record-open-missing-source",
                format!(
                    "no <slug>-discussion-source.md or <slug>-review-source.md found in bundle {} and no --source-file provided",
                    bundle_dir.display()
                ),
            )
        })?;
    let execution_state_file = explicit_execution_state
        .map(Path::to_path_buf)
        .or(execution_state_file);

    Ok(RecordBundle {
        source_file,
        plan_file,
        execution_state_file,
    })
}

// Resolve a git working dir + pathspec for `path`.
//
// Running git from the path's parent (rather than the process cwd) lets the
// caller pass an absolute bundle path that lives in a different repo than the
// one they launched the binary from — git walks up to the `.git` toplevel from
// there. The pathspec must then be the bare file name, *relative to that cwd*:
// passing the full path re-anchors it under the subdir cwd (double-prefixed),
// matches nothing, and the empty result was mis-reported as
// `record-open-uncommitted` whenever `--bundle` was a relative path even though
// the bundle files were committed and reachable from HEAD.
fn git_cwd_and_pathspec(path: &Path) -> (&Path, String) {
    let cwd = path.parent().unwrap_or_else(|| Path::new("."));
    let pathspec = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    (cwd, pathspec)
}

fn last_commit_for_path(path: &Path, allow_dirty: bool) -> Result<String, CommandError> {
    let (cwd, pathspec) = git_cwd_and_pathspec(path);
    let output = common_git::run_output_in(
        cwd,
        &["log", "-n", "1", "--format=%H", "--", pathspec.as_str()],
    )
    .map_err(|err| {
        CommandError::runtime(
            "record-open-git-log-failed",
            format!("git log {} failed: {err}", path.display()),
        )
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CommandError::runtime(
            "record-open-git-log-failed",
            format!("git log {}: {stderr}", path.display()),
        ));
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() && !allow_dirty {
        return Err(CommandError::runtime(
            "record-open-uncommitted",
            format!(
                "path {} has no commit in git history; commit it first or pass --allow-dirty",
                path.display()
            ),
        ));
    }
    // With `--allow-dirty` a never-committed bundle file is allowed through with
    // an empty commit; the snapshot renderer omits the `Commit:` line for it,
    // so the error hint above ("pass --allow-dirty") now actually bypasses the
    // check it advertises.
    Ok(sha)
}

fn path_is_dirty(path: &Path) -> Result<bool, CommandError> {
    let (cwd, pathspec) = git_cwd_and_pathspec(path);
    let output =
        common_git::run_output_in(cwd, &["status", "--porcelain", "--", pathspec.as_str()])
            .map_err(|err| {
                CommandError::runtime(
                    "record-open-git-status-failed",
                    format!("git status {} failed: {err}", path.display()),
                )
            })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CommandError::runtime(
            "record-open-git-status-failed",
            format!("git status {}: {stderr}", path.display()),
        ));
    }
    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

fn resolve_bundle_snapshots(
    bundle: &RecordBundle,
    allow_dirty: bool,
) -> Result<
    (
        lifecycle_record::SnapshotData,
        lifecycle_record::SnapshotData,
    ),
    CommandError,
> {
    for (label, path) in [("source", &bundle.source_file), ("plan", &bundle.plan_file)] {
        if !path.exists() {
            return Err(CommandError::usage(
                "record-open-bundle-file-missing",
                format!("{label} file not found: {}", path.display()),
            ));
        }
        if !allow_dirty && path_is_dirty(path)? {
            return Err(CommandError::usage(
                "record-open-bundle-dirty",
                format!(
                    "{label} file {} has uncommitted changes; commit them or pass --allow-dirty",
                    path.display()
                ),
            ));
        }
    }

    let source_commit = last_commit_for_path(&bundle.source_file, allow_dirty)?;
    let plan_commit = last_commit_for_path(&bundle.plan_file, allow_dirty)?;

    let source_snapshot = lifecycle_record::SnapshotData {
        path: relative_repo_path(&bundle.source_file),
        commit: source_commit,
        title: None,
        summary: None,
    };
    let plan_snapshot = lifecycle_record::SnapshotData {
        path: relative_repo_path(&bundle.plan_file),
        commit: plan_commit,
        title: None,
        summary: None,
    };
    Ok((source_snapshot, plan_snapshot))
}

fn relative_repo_path(path: &Path) -> String {
    // Try to express the path relative to the repo root for stable
    // payload identity; otherwise fall back to the display path.
    let cwd = std::env::current_dir().ok();
    if let Some(cwd) = cwd
        && let Ok(stripped) = path.strip_prefix(&cwd)
    {
        return stripped.to_string_lossy().to_string();
    }
    path.to_string_lossy().to_string()
}

fn build_initial_state_payload(plan: &plan_tooling::parse::Plan) -> Value {
    let tasks = plan
        .sprints
        .iter()
        .flat_map(|sprint| {
            sprint.tasks.iter().map(|task| {
                json!({
                    "id": task.id,
                    "status": "pending",
                    "title": task.name,
                })
            })
        })
        .collect::<Vec<_>>();
    json!({
        "status": "in-progress",
        "target_scope": plan.title,
        "current": "Sprint 1 ready",
        "next_action": "execute Sprint 1 tasks",
        "tasks": tasks,
        "prs": [],
        "blockers": [],
        "links": {},
    })
}

fn parse_plan_for_record(plan_file: &Path) -> Result<plan_tooling::parse::Plan, CommandError> {
    let resolved = task_spec::resolve_plan_file(plan_file);
    let display = plan_file.to_string_lossy().to_string();
    let (plan, errors) = parse_plan_with_display(&resolved, &display).map_err(|err| {
        CommandError::runtime(
            "record-open-plan-parse-failed",
            format!("failed to read {}: {err}", plan_file.display()),
        )
    })?;
    if !errors.is_empty() {
        return Err(CommandError::runtime(
            "record-open-plan-invalid",
            errors.join(" | "),
        ));
    }
    Ok(plan)
}

fn record_initial_dashboard(
    profile: crate::commands::record::RecordProfile,
    plan_title: &str,
    issue_url: Option<&str>,
) -> String {
    lifecycle_record::render_dashboard(DashboardInput {
        profile,
        status: "in-progress".to_string(),
        target_scope: plan_title.to_string(),
        current: "Sprint 1 ready".to_string(),
        next_action: "execute Sprint 1 tasks".to_string(),
        validation: "pending".to_string(),
        linked_prs: Vec::new(),
        blockers: Vec::new(),
        approval: "pending".to_string(),
        source_url: None,
        plan_url: None,
        state_url: None,
        session_url: None,
        validation_url: None,
        review_url: None,
        closeout_url: None,
        title: Some(plan_title.to_string()),
        issue_url: issue_url.map(str::to_string),
    })
}

fn build_record_seed(
    profile: crate::commands::record::RecordProfile,
    title: Option<&str>,
    bundle: &RecordBundle,
    allow_dirty: bool,
    state_fallback: &str,
) -> Result<RecordSeed, CommandError> {
    let plan = parse_plan_for_record(&bundle.plan_file)?;
    let plan_title = title
        .map(str::to_string)
        .unwrap_or_else(|| plan.title.clone());

    let (source_snapshot, plan_snapshot) = resolve_bundle_snapshots(bundle, allow_dirty)?;
    let source_content = fs::read_to_string(&bundle.source_file).map_err(|err| {
        CommandError::runtime(
            "record-open-source-read-failed",
            format!("failed to read {}: {err}", bundle.source_file.display()),
        )
    })?;
    let plan_content = fs::read_to_string(&bundle.plan_file).map_err(|err| {
        CommandError::runtime(
            "record-open-plan-read-failed",
            format!("failed to read {}: {err}", bundle.plan_file.display()),
        )
    })?;
    let execution_state_content = bundle
        .execution_state_file
        .as_deref()
        .map(|path| {
            fs::read_to_string(path).map_err(|err| {
                CommandError::runtime(
                    "record-open-execution-state-read-failed",
                    format!("failed to read {}: {err}", path.display()),
                )
            })
        })
        .transpose()?;

    let source_body = lifecycle_record::render_record_snapshot_comment(
        profile,
        crate::commands::record::LifecycleCommentKind::Source,
        &source_snapshot,
        &source_content,
        None,
    )
    .map_err(|err| CommandError::runtime("record-open-source-render-failed", err))?;
    let plan_body = lifecycle_record::render_record_snapshot_comment(
        profile,
        crate::commands::record::LifecycleCommentKind::Plan,
        &plan_snapshot,
        &plan_content,
        None,
    )
    .map_err(|err| CommandError::runtime("record-open-plan-render-failed", err))?;

    let initial_state = build_initial_state_payload(&plan);
    let state_summary = execution_state_content
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(state_fallback);
    // The first Execution State defaults to an open fold (`<details open>`):
    // the toggle stays, but the full Task Ledger is visible when the issue
    // loads. Later checkpoints keep the `auto` default (collapsed while
    // in-progress; expanded raw at the terminal pre-closeout state).
    let state_body = lifecycle_record::render_record_post_comment_with_display(
        profile,
        crate::commands::record::LifecycleCommentKind::State,
        initial_state,
        Some(state_summary),
        None,
        crate::commands::record::TaskLedgerDisplay::Open,
    )
    .map_err(|err| CommandError::runtime("record-open-state-render-failed", err))?;

    Ok(RecordSeed {
        plan_title,
        source_path: path_text(&bundle.source_file),
        plan_path: path_text(&bundle.plan_file),
        source_commit: source_snapshot.commit,
        plan_commit: plan_snapshot.commit,
        source_body,
        plan_body,
        state_body,
    })
}

fn read_fixture_evidence(fixture_dir: &Path) -> Result<(String, String), CommandError> {
    let body_path = fixture_dir.join("issue-body.md");
    let comments_path = fixture_dir.join("comments.json");
    let body = fs::read_to_string(&body_path).map_err(|err| {
        CommandError::runtime(
            "record-fixture-body-read-failed",
            format!("failed to read fixture body {}: {err}", body_path.display()),
        )
    })?;
    let comments = fs::read_to_string(&comments_path).map_err(|err| {
        CommandError::runtime(
            "record-fixture-comments-read-failed",
            format!(
                "failed to read fixture comments {}: {err}",
                comments_path.display()
            ),
        )
    })?;
    Ok((body, comments))
}

fn read_fixture_pr_snapshot(
    fixture_dir: &Path,
    repo: &str,
    pr: u64,
) -> Result<lifecycle_record::LinkedPrEvidence, CommandError> {
    let slug = repo.replace('/', "__");
    let file = fixture_dir.join("prs").join(format!("{slug}__{pr}.json"));
    let raw = fs::read_to_string(&file).map_err(|err| {
        CommandError::runtime(
            "record-fixture-pr-missing",
            format!("failed to read PR fixture {}: {err}", file.display()),
        )
    })?;
    let value: Value = serde_json::from_str(&raw).map_err(|err| {
        CommandError::runtime(
            "record-fixture-pr-invalid",
            format!("failed to parse PR fixture {}: {err}", file.display()),
        )
    })?;
    let state = value
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let merged = state.eq_ignore_ascii_case("merged");
    let merge_sha = value
        .get("mergeCommit")
        .and_then(|v| v.get("oid"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let checks_str = value
        .get("statusCheckRollup")
        .and_then(|rollup| rollup.get("state"))
        .and_then(Value::as_str)
        .map(|s| s.to_ascii_lowercase());
    let checks = check_status_from_state(checks_str.as_deref());
    let required_state = value
        .get("requiredCheckRollup")
        .and_then(|rollup| rollup.get("state"))
        .and_then(Value::as_str)
        .map(|s| s.to_ascii_lowercase());
    let required_count = value
        .get("requiredCheckRollup")
        .and_then(|rollup| rollup.get("count"))
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok());
    let required_state_enum = required_state
        .as_deref()
        .map(|s| check_status_from_state(Some(s)));
    let non_required_failures = value
        .get("nonRequiredFailures")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(lifecycle_record::LinkedPrEvidence {
        pr_ref: format!("{repo}#{pr}"),
        url: value.get("url").and_then(Value::as_str).map(str::to_string),
        merge_sha: if merged { merge_sha } else { None },
        checks,
        required_state: required_state_enum,
        required_count,
        non_required_failures,
    })
}

fn check_status_from_state(state: Option<&str>) -> lifecycle_record::CheckStatus {
    match state.map(str::to_ascii_lowercase).as_deref() {
        Some("success" | "pass") => lifecycle_record::CheckStatus::Pass,
        Some(
            "failure" | "failed" | "error" | "cancelled" | "timed_out" | "action_required"
            | "stale" | "startup_failure",
        ) => lifecycle_record::CheckStatus::Fail,
        _ => lifecycle_record::CheckStatus::None,
    }
}

fn parse_issue_reference(value: &str) -> Result<u64, CommandError> {
    let trimmed = value.trim();
    if let Ok(num) = trimmed.parse::<u64>() {
        return Ok(num);
    }
    if let Some(tail) = trimmed.rsplit('/').next()
        && let Ok(num) = tail.parse::<u64>()
    {
        return Ok(num);
    }
    Err(CommandError::usage(
        "record-invalid-issue",
        format!("--issue must be a number or full URL, got `{value}`"),
    ))
}

fn parse_linked_pr_reference(value: &str) -> Result<(String, u64), CommandError> {
    let trimmed = value.trim();
    if let Some((repo, number_raw)) = trimmed.rsplit_once('#') {
        if !repo.contains('/') {
            return Err(CommandError::usage(
                "record-invalid-linked-pr",
                format!("--linked-pr must be `owner/repo#NN` or a PR URL, got `{value}`"),
            ));
        }
        let number = number_raw.parse::<u64>().map_err(|err| {
            CommandError::usage(
                "record-invalid-linked-pr",
                format!("--linked-pr number is not numeric in `{value}`: {err}"),
            )
        })?;
        return Ok((repo.to_string(), number));
    }
    if trimmed.starts_with("https://github.com/") {
        let tail = trimmed.trim_end_matches('/');
        if let Some(rest) = tail.strip_prefix("https://github.com/") {
            let mut parts = rest.splitn(4, '/');
            let owner = parts.next().unwrap_or("");
            let repo = parts.next().unwrap_or("");
            let kind = parts.next().unwrap_or("");
            let number = parts.next().unwrap_or("");
            if !owner.is_empty()
                && !repo.is_empty()
                && (kind == "pull" || kind == "issues")
                && let Ok(num) = number.parse::<u64>()
            {
                return Ok((format!("{owner}/{repo}"), num));
            }
        }
    }
    Err(CommandError::usage(
        "record-invalid-linked-pr",
        format!("--linked-pr must be `owner/repo#NN` or a PR URL, got `{value}`"),
    ))
}

fn read_payload_data(path: &Path) -> Result<Value, CommandError> {
    let raw = fs::read_to_string(path).map_err(|err| {
        CommandError::runtime(
            "record-post-payload-read-failed",
            format!("failed to read --payload-file {}: {err}", path.display()),
        )
    })?;
    serde_json::from_str(&raw).map_err(|err| {
        CommandError::runtime(
            "record-post-payload-invalid",
            format!("invalid JSON in --payload-file {}: {err}", path.display()),
        )
    })
}

/// Trim, drop empty values, and reject names that appear in both
/// `--add-label` and `--remove-label`. Used by `record post` and
/// `record close` to keep the live `edit_issue_labels` call coherent.
fn normalize_label_mutations(
    add: &[String],
    remove: &[String],
    command_code: &'static str,
) -> Result<(Vec<String>, Vec<String>), CommandError> {
    let normalize = |raw: &[String]| -> Vec<String> {
        raw.iter()
            .map(|label| label.trim().to_string())
            .filter(|label| !label.is_empty())
            .collect()
    };
    let add_clean = normalize(add);
    let remove_clean = normalize(remove);
    let conflicts: Vec<&str> = add_clean
        .iter()
        .filter(|label| remove_clean.iter().any(|other| other == *label))
        .map(String::as_str)
        .collect();
    if !conflicts.is_empty() {
        return Err(CommandError::usage(
            "record-label-mutation-conflict",
            format!(
                "{command_code}: label(s) appear in both --add-label and --remove-label: {}",
                conflicts.join(", ")
            ),
        ));
    }
    Ok((add_clean, remove_clean))
}

fn run_record_open(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &RecordOpenArgs,
) -> Result<Value, CommandError> {
    // Fixture mode is deterministic and never reaches the network.
    if let Some(fixture_dir) = &args.fixture {
        let (body, comments_json) = read_fixture_evidence(fixture_dir)?;
        let audit = lifecycle_record::audit_record(Some(&body), &comments_json, Some(args.profile))
            .map_err(|err| CommandError::runtime("record-open-fixture-audit-failed", err))?;
        let source_url = audit
            .evidence
            .get("source")
            .and_then(|hit| hit.url.clone())
            .unwrap_or_default();
        let plan_url = audit
            .evidence
            .get("plan")
            .and_then(|hit| hit.url.clone())
            .unwrap_or_default();
        let state_url = audit
            .evidence
            .get("state")
            .and_then(|hit| hit.url.clone())
            .unwrap_or_default();
        let plan_title = args
            .title
            .clone()
            .unwrap_or_else(|| "fixture-plan".to_string());
        let dashboard = lifecycle_record::render_dashboard_from_audit(
            &audit,
            Some(&plan_title),
            audit
                .evidence
                .values()
                .next()
                .and_then(|hit| hit.url.as_deref().and_then(|url| url.split('#').next())),
        );
        return Ok(json!({
            "operation": "record.open",
            "execution_mode": binary.execution_mode(),
            "dry_run": true,
            "mode": "fixture",
            "issue": {"number": null, "url": null},
            "comments": {"source": source_url, "plan": plan_url, "state": state_url},
            "dashboard_markdown": dashboard,
        }));
    }

    let bundle = resolve_record_bundle(
        args.bundle.as_deref(),
        args.source_file.as_deref(),
        args.plan_file.as_deref(),
        args.execution_state_file.as_deref(),
    )?;
    let seed = build_record_seed(
        args.profile,
        args.title.as_deref(),
        &bundle,
        args.allow_dirty,
        "Initial execution state seeded by `plan-issue record open`.",
    )?;

    let initial_dashboard = record_initial_dashboard(args.profile, &seed.plan_title, None);

    let normalized_labels: Vec<String> = args
        .labels
        .iter()
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty())
        .collect();

    let preview = json!({
        "issue_body_markdown": initial_dashboard,
        "comments": {
            "source": &seed.source_body,
            "plan": &seed.plan_body,
            "state": &seed.state_body,
        },
        "plan_title": &seed.plan_title,
        "source_path": &seed.source_path,
        "plan_path": &seed.plan_path,
        "source_commit": &seed.source_commit,
        "plan_commit": &seed.plan_commit,
        "labels": normalized_labels.clone(),
    });

    if binary != BinaryFlavor::PlanIssue || dry_run {
        return Ok(json!({
            "operation": "record.open",
            "execution_mode": binary.execution_mode(),
            "dry_run": true,
            "mode": "dry-run",
            "preview": preview,
        }));
    }

    // Live mode. Sprint 2.2 routes `record open` through the provider-aware
    // adapter selector so GitLab repos exercise `forge_cli_adapter::ForgeCliAdapter`
    // (which shells out to `forge-cli`). GitHub repos keep using `GhCliAdapter`
    // unchanged.
    let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
    let adapter = crate::provider::select_adapter(&repo_info, force);
    let repo = repo_info.slug.as_str();

    // Auto-detect an already-open tracker for this bundle before creating a new
    // one. The dedup key is the source snapshot identity (repo-relative path +
    // last-commit SHA) that `record open` embeds in the source lifecycle
    // comment, so a re-run resumes the same tracker instead of duplicating it.
    let identity = BundleIdentity {
        source_path: relative_repo_path(&bundle.source_file),
        source_commit: seed.source_commit.clone(),
    };
    if let Some((issue_number, audit)) = detect_resumable_tracker(
        adapter.as_ref(),
        repo,
        &normalized_labels,
        args.profile,
        &identity,
    )? {
        let issue_url = repo_info.issue_url(issue_number);
        return record_open_resume(
            adapter.as_ref(),
            repo,
            args.profile,
            &seed,
            issue_number,
            &issue_url,
            &audit,
            binary.execution_mode(),
        );
    }

    let body_path = write_temp_markdown("record-open-body", &initial_dashboard)
        .map_err(|err| CommandError::runtime("record-open-body-write-failed", err))?;

    record_open_finalize(
        adapter.as_ref(),
        repo,
        args.profile,
        &body_path,
        &normalized_labels,
        &seed,
        binary.execution_mode(),
    )
}

/// Create the tracking issue, post the initial lifecycle comments, and repair
/// the dashboard.
///
/// The source lifecycle comment is posted **first** because it carries the
/// bundle's snapshot identity (`path` + `commit`) that `record open` auto-detect
/// matches on. Until it exists the tracker is unidentifiable, so a failure
/// *before* the source comment posts best-effort closes (rolls back) the
/// just-created issue — otherwise a re-run could not match the orphan and would
/// create a duplicate. The close is sent without a comment so it still succeeds
/// when the source post itself was the broken step.
///
/// Once the source comment exists the tracker is identifiable, so a later
/// failure (plan/state post or dashboard repair) leaves the partial tracker in
/// place: the next `record open` detects it and attaches only the missing
/// roles. This is the resume-by-default behavior; rollback is narrowed to the
/// pre-identity window.
///
/// Split out of `run_record_open` so both paths are unit-testable with a stub
/// adapter — the live `record open` path is otherwise reachable only through the
/// real `plan-issue` binary.
fn record_open_finalize(
    adapter: &dyn ProviderAdapter,
    repo: &str,
    profile: crate::commands::record::RecordProfile,
    body_path: &Path,
    labels: &[String],
    seed: &RecordSeed,
    execution_mode: &'static str,
) -> Result<Value, CommandError> {
    let (issue_number, issue_url) = adapter
        .create_issue(repo, &seed.plan_title, body_path, labels)
        .map_err(|err| CommandError::runtime("record-open-issue-create-failed", err))?;

    // Post the identity-bearing source comment first; roll back on failure
    // because the orphan would carry no snapshot to resume from.
    let source_comment_path = write_temp_markdown("record-open-source-comment", &seed.source_body)
        .map_err(|err| CommandError::runtime("record-open-source-write-failed", err))?;
    let source_url = match adapter.comment_issue(repo, issue_number, &source_comment_path) {
        Ok(url) => url,
        Err(post_err) => {
            let mut err = CommandError::runtime("record-open-source-post-failed", post_err);
            let rollback_note = match adapter.close_issue(
                repo,
                issue_number,
                crate::commands::plan::CloseReason::NotPlanned,
                None,
            ) {
                Ok(()) => format!(
                    " (rolled back: closed orphaned issue #{issue_number} {issue_url} — it had no \
                     source snapshot to resume from; re-run `plan-issue record open` to recreate \
                     the tracker cleanly)"
                ),
                Err(close_err) => format!(
                    " (rollback FAILED: orphaned issue #{issue_number} {issue_url} is still open \
                     and has no source snapshot to resume from — close it before retrying so a \
                     re-run does not create a duplicate; close error: {close_err})"
                ),
            };
            err.message.push_str(&rollback_note);
            return Err(err);
        }
    };

    // From here the tracker carries its source identity, so a failure leaves it
    // in place for the next `record open` to resume instead of rolling back.
    let populate_rest = || -> Result<Value, CommandError> {
        let plan_path = write_temp_markdown("record-open-plan-comment", &seed.plan_body)
            .map_err(|err| CommandError::runtime("record-open-plan-write-failed", err))?;
        let plan_url = adapter
            .comment_issue(repo, issue_number, &plan_path)
            .map_err(|err| CommandError::runtime("record-open-plan-post-failed", err))?;
        let state_path = write_temp_markdown("record-open-state-comment", &seed.state_body)
            .map_err(|err| CommandError::runtime("record-open-state-write-failed", err))?;
        let state_url = adapter
            .comment_issue(repo, issue_number, &state_path)
            .map_err(|err| CommandError::runtime("record-open-state-post-failed", err))?;

        // Repair dashboard with freshly-created comment URLs through audit.
        let (body_after, comments_json) = adapter
            .issue_evidence(repo, issue_number)
            .map_err(|err| CommandError::runtime("record-open-evidence-read-failed", err))?;
        let audit =
            lifecycle_record::audit_record(Some(&body_after), &comments_json, Some(profile))
                .map_err(|err| CommandError::runtime("record-open-audit-failed", err))?;
        let repaired = lifecycle_record::render_dashboard_from_audit(
            &audit,
            Some(&seed.plan_title),
            Some(&issue_url),
        );
        let repaired_path = write_temp_markdown("record-open-dashboard", &repaired)
            .map_err(|err| CommandError::runtime("record-open-dashboard-write-failed", err))?;
        adapter
            .edit_issue_body(repo, issue_number, &repaired_path)
            .map_err(|err| CommandError::runtime("record-open-dashboard-edit-failed", err))?;

        Ok(json!({
            "operation": "record.open",
            "execution_mode": execution_mode,
            "dry_run": false,
            "mode": "live",
            "issue": {"number": issue_number, "url": issue_url.clone()},
            "comments": {"source": source_url, "plan": plan_url, "state": state_url},
            "labels": labels.to_vec(),
            "dashboard_markdown": repaired,
        }))
    };

    populate_rest().map_err(|mut err| {
        err.message.push_str(&format!(
            " (partial tracker left open as #{issue_number} {issue_url}; re-run \
             `plan-issue record open` to resume and post the missing lifecycle comments)"
        ));
        err
    })
}

/// The deterministic identity used to match a bundle to an already-open tracker:
/// the source snapshot's repo-relative path plus its last-commit SHA, exactly as
/// embedded in the tracker's source lifecycle comment payload. Matching requires
/// the same launch cwd across runs (the path is `relative_repo_path`); a cwd
/// mismatch simply misses detection and falls back to creating a new tracker.
struct BundleIdentity {
    source_path: String,
    source_commit: String,
}

/// Scan the label-scoped open trackers for one whose source lifecycle comment
/// carries the same `(path, commit)` snapshot identity as `identity`. Returns
/// the matching issue number and its audit so the caller can attach only the
/// missing roles. Unreadable or unparseable candidates are skipped rather than
/// failing detection — a single malformed tracker must not block opening a new
/// one.
fn detect_resumable_tracker(
    adapter: &dyn ProviderAdapter,
    repo: &str,
    labels: &[String],
    profile: crate::commands::record::RecordProfile,
    identity: &BundleIdentity,
) -> Result<Option<(u64, lifecycle_record::RecordAudit)>, CommandError> {
    let numbers = adapter
        .list_open_tracker_issues(repo, labels)
        .map_err(|err| CommandError::runtime("record-open-list-failed", err))?;
    for number in numbers {
        let Ok((body, comments_json)) = adapter.issue_evidence(repo, number) else {
            continue;
        };
        let Ok(audit) = lifecycle_record::audit_record(Some(&body), &comments_json, Some(profile))
        else {
            continue;
        };
        let matches = audit
            .evidence
            .get("source")
            .and_then(|hit| hit.payload.as_ref())
            .and_then(|payload| payload.parse_snapshot().ok())
            .is_some_and(|snapshot| {
                snapshot.path == identity.source_path && snapshot.commit == identity.source_commit
            });
        if matches {
            return Ok(Some((number, audit)));
        }
    }
    Ok(None)
}

/// Resume an already-open tracker: post only the lifecycle roles the audit
/// reports missing (`source` / `plan` / `state`), then repair the dashboard. A
/// tracker that already has all three is a no-op (`mode: "already-open"`), so a
/// redundant `record open` neither duplicates the issue nor its comments.
#[allow(clippy::too_many_arguments)]
fn record_open_resume(
    adapter: &dyn ProviderAdapter,
    repo: &str,
    profile: crate::commands::record::RecordProfile,
    seed: &RecordSeed,
    issue_number: u64,
    issue_url: &str,
    audit: &lifecycle_record::RecordAudit,
    execution_mode: &'static str,
) -> Result<Value, CommandError> {
    let missing: std::collections::BTreeSet<&str> =
        audit.missing_required.iter().map(String::as_str).collect();

    let mut attached: Vec<&'static str> = Vec::new();
    for &(role, code, temp_label, fail_code) in &[
        (
            "source",
            "source-missing",
            "record-open-resume-source-comment",
            "record-open-source-post-failed",
        ),
        (
            "plan",
            "plan-missing",
            "record-open-resume-plan-comment",
            "record-open-plan-post-failed",
        ),
        (
            "state",
            "state-missing",
            "record-open-resume-state-comment",
            "record-open-state-post-failed",
        ),
    ] {
        if !missing.contains(code) {
            continue;
        }
        let body = match role {
            "source" => seed.source_body.as_str(),
            "plan" => seed.plan_body.as_str(),
            _ => seed.state_body.as_str(),
        };
        let path = write_temp_markdown(temp_label, body)
            .map_err(|err| CommandError::runtime("record-open-resume-write-failed", err))?;
        adapter
            .comment_issue(repo, issue_number, &path)
            .map_err(|err| CommandError::runtime(fail_code, err))?;
        attached.push(role);
    }

    if attached.is_empty() {
        return Ok(json!({
            "operation": "record.open",
            "execution_mode": execution_mode,
            "dry_run": false,
            "mode": "already-open",
            "issue": {"number": issue_number, "url": issue_url},
            "attached": attached,
        }));
    }

    // Re-read the now-fuller evidence and repair the dashboard so the freshly
    // attached comment URLs are linked.
    let (body_after, comments_json) = adapter
        .issue_evidence(repo, issue_number)
        .map_err(|err| CommandError::runtime("record-open-evidence-read-failed", err))?;
    let refreshed =
        lifecycle_record::audit_record(Some(&body_after), &comments_json, Some(profile))
            .map_err(|err| CommandError::runtime("record-open-audit-failed", err))?;
    let repaired = lifecycle_record::render_dashboard_from_audit(
        &refreshed,
        Some(&seed.plan_title),
        Some(issue_url),
    );
    let repaired_path = write_temp_markdown("record-open-resume-dashboard", &repaired)
        .map_err(|err| CommandError::runtime("record-open-dashboard-write-failed", err))?;
    adapter
        .edit_issue_body(repo, issue_number, &repaired_path)
        .map_err(|err| CommandError::runtime("record-open-dashboard-edit-failed", err))?;

    Ok(json!({
        "operation": "record.open",
        "execution_mode": execution_mode,
        "dry_run": false,
        "mode": "resumed",
        "issue": {"number": issue_number, "url": issue_url},
        "attached": attached,
        "dashboard_markdown": repaired,
    }))
}

fn run_record_attach(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &RecordAttachArgs,
) -> Result<Value, CommandError> {
    let issue_number = parse_issue_reference(&args.issue)?;
    let bundle = resolve_record_bundle(
        args.bundle.as_deref(),
        args.source_file.as_deref(),
        args.plan_file.as_deref(),
        args.execution_state_file.as_deref(),
    )?;
    let seed = build_record_seed(
        args.profile,
        args.title.as_deref(),
        &bundle,
        args.allow_dirty,
        "Initial execution state attached by `plan-issue record attach`.",
    )?;

    let preview = json!({
        "issue": args.issue,
        "issue_number": issue_number,
        "comments": {
            "source": &seed.source_body,
            "plan": &seed.plan_body,
            "state": &seed.state_body,
        },
        "plan_title": &seed.plan_title,
        "source_path": &seed.source_path,
        "plan_path": &seed.plan_path,
        "source_commit": &seed.source_commit,
        "plan_commit": &seed.plan_commit,
    });

    if binary != BinaryFlavor::PlanIssue || dry_run {
        return Ok(json!({
            "operation": "record.attach",
            "execution_mode": binary.execution_mode(),
            "dry_run": true,
            "mode": "dry-run",
            "preview": preview,
        }));
    }

    let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
    let adapter = crate::provider::select_adapter(&repo_info, force);
    let issue_url = repo_info.issue_url(issue_number);
    let repo = repo_info.slug;

    let source_path = write_temp_markdown("record-attach-source-comment", &seed.source_body)
        .map_err(|err| CommandError::runtime("record-attach-source-write-failed", err))?;
    let source_url = adapter
        .comment_issue(&repo, issue_number, &source_path)
        .map_err(|err| CommandError::runtime("record-attach-source-post-failed", err))?;
    let plan_path = write_temp_markdown("record-attach-plan-comment", &seed.plan_body)
        .map_err(|err| CommandError::runtime("record-attach-plan-write-failed", err))?;
    let plan_url = adapter
        .comment_issue(&repo, issue_number, &plan_path)
        .map_err(|err| CommandError::runtime("record-attach-plan-post-failed", err))?;
    let state_path = write_temp_markdown("record-attach-state-comment", &seed.state_body)
        .map_err(|err| CommandError::runtime("record-attach-state-write-failed", err))?;
    let state_url = adapter
        .comment_issue(&repo, issue_number, &state_path)
        .map_err(|err| CommandError::runtime("record-attach-state-post-failed", err))?;

    let (body_after, comments_json) = adapter
        .issue_evidence(&repo, issue_number)
        .map_err(|err| CommandError::runtime("record-attach-evidence-read-failed", err))?;
    let audit =
        lifecycle_record::audit_record(Some(&body_after), &comments_json, Some(args.profile))
            .map_err(|err| CommandError::runtime("record-attach-audit-failed", err))?;
    let repaired = lifecycle_record::render_dashboard_from_audit(
        &audit,
        Some(&seed.plan_title),
        Some(&issue_url),
    );
    let repaired_path = write_temp_markdown("record-attach-dashboard", &repaired)
        .map_err(|err| CommandError::runtime("record-attach-dashboard-write-failed", err))?;
    adapter
        .edit_issue_body(&repo, issue_number, &repaired_path)
        .map_err(|err| CommandError::runtime("record-attach-dashboard-edit-failed", err))?;

    Ok(json!({
        "operation": "record.attach",
        "execution_mode": binary.execution_mode(),
        "dry_run": false,
        "mode": "live",
        "issue": {"number": issue_number, "url": issue_url},
        "comments": {"source": source_url, "plan": plan_url, "state": state_url},
        "dashboard_markdown": repaired,
    }))
}

fn run_record_post(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &RecordPostArgs,
) -> Result<Value, CommandError> {
    match args.kind {
        crate::commands::record::LifecycleCommentKind::Source
        | crate::commands::record::LifecycleCommentKind::Plan => {
            return Err(CommandError::usage(
                "record-post-kind-not-allowed",
                format!(
                    "`record post --kind {}` is rejected; source/plan are owned by `record open`",
                    args.kind.as_str()
                ),
            ));
        }
        crate::commands::record::LifecycleCommentKind::Closeout => {
            return Err(CommandError::usage(
                "record-post-closeout-not-allowed",
                "`record post --kind closeout` is rejected; use `plan-issue record close` which posts the closeout comment after the strict gate passes",
            ));
        }
        _ => {}
    }
    if args.execution_state_file.is_some()
        && args.kind != crate::commands::record::LifecycleCommentKind::State
    {
        return Err(CommandError::usage(
            "record-post-execution-state-file-kind-invalid",
            "`record post --execution-state-file` is only valid with `--kind state`",
        ));
    }
    if args.kind != crate::commands::record::LifecycleCommentKind::State
        && args.task_ledger_display != crate::commands::record::TaskLedgerDisplay::Auto
    {
        return Err(CommandError::usage(
            "record-post-task-ledger-display-kind-invalid",
            "`record post --task-ledger-display` is only configurable with `--kind state`",
        ));
    }

    let payload_data = match &args.payload_file {
        Some(path) => read_payload_data(path)?,
        None => Value::Null,
    };
    lifecycle_record::validate_payload_data_for_kind(args.kind, &payload_data).map_err(|err| {
        CommandError::runtime(
            "record-post-payload-schema-invalid",
            format!(
                "`record post --kind {}` payload does not match the lifecycle record schema: {}",
                args.kind.as_str(),
                err
            ),
        )
    })?;
    let summary = match (&args.execution_state_file, &args.summary_file) {
        (Some(path), None) => {
            let text = read_text_file(path, "record-post-execution-state-read-failed")?;
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Err(CommandError::usage(
                    "record-post-execution-state-empty",
                    format!("execution-state file {} is empty", path.display()),
                ));
            }
            if !trimmed.contains("## Task Ledger") {
                return Err(CommandError::usage(
                    "record-post-execution-state-task-ledger-missing",
                    format!(
                        "execution-state file {} must contain `## Task Ledger`",
                        path.display()
                    ),
                ));
            }
            Some(text)
        }
        (None, Some(path)) => Some(read_text_file(path, "record-post-summary-read-failed")?),
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("clap conflicts_with prevents both summary inputs"),
    };
    let body = lifecycle_record::render_record_post_comment_with_display(
        args.profile,
        args.kind,
        payload_data,
        summary.as_deref(),
        None,
        args.task_ledger_display,
    )
    .map_err(|err| CommandError::runtime("record-post-render-failed", err))?;

    let (add_labels, remove_labels) =
        normalize_label_mutations(&args.add_labels, &args.remove_labels, "record-post")?;
    let label_mutation_planned = !add_labels.is_empty() || !remove_labels.is_empty();
    let labels_preview = json!({
        "add": add_labels.clone(),
        "remove": remove_labels.clone(),
    });

    // Fixture mode: just return the rendered body + simulated URL.
    if let Some(fixture_dir) = &args.fixture {
        let _ = fixture_dir;
        return Ok(json!({
            "operation": "record.post",
            "execution_mode": binary.execution_mode(),
            "dry_run": true,
            "mode": "fixture",
            "issue": args.issue,
            "kind": args.kind.as_str(),
            "comment_body": body,
            "comment_url": null,
            "labels": labels_preview,
        }));
    }

    if binary != BinaryFlavor::PlanIssue || dry_run {
        return Ok(json!({
            "operation": "record.post",
            "execution_mode": binary.execution_mode(),
            "dry_run": true,
            "mode": "dry-run",
            "issue": args.issue,
            "kind": args.kind.as_str(),
            "comment_body": body,
            "labels": labels_preview,
        }));
    }

    let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
    let adapter = crate::provider::select_adapter(&repo_info, force);
    let repo = repo_info.slug;
    let issue_number = parse_issue_reference(&args.issue)?;
    let comment_path = write_temp_markdown("record-post-comment", &body)
        .map_err(|err| CommandError::runtime("record-post-comment-write-failed", err))?;
    let url = adapter
        .comment_issue(&repo, issue_number, &comment_path)
        .map_err(|err| CommandError::runtime("record-post-comment-post-failed", err))?;

    if label_mutation_planned {
        adapter
            .edit_issue_labels(&repo, issue_number, &add_labels, &remove_labels)
            .map_err(|err| CommandError::runtime("record-post-label-edit-failed", err))?;
    }

    Ok(json!({
        "operation": "record.post",
        "execution_mode": binary.execution_mode(),
        "dry_run": false,
        "mode": "live",
        "issue": args.issue,
        "kind": args.kind.as_str(),
        "comment_url": url,
        "labels": labels_preview,
    }))
}

fn run_record_repair_dashboard(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &RecordRepairDashboardArgs,
) -> Result<Value, CommandError> {
    let (body, comments_json, issue_number, repo, issue_url) = if let Some(fixture_dir) =
        &args.fixture
    {
        let (body, comments) = read_fixture_evidence(fixture_dir)?;
        (body, comments, None, None, None)
    } else if let (Some(body_file), Some(comments_path)) = (&args.body_file, &args.comments_json) {
        let body = read_text_file(body_file, "record-repair-body-read-failed")?;
        let comments = read_text_file(comments_path, "record-repair-comments-read-failed")?;
        (body, comments, None, None, None)
    } else {
        let issue_value = args.issue.as_deref().ok_or_else(|| {
                CommandError::usage(
                    "record-repair-missing-issue",
                    "--issue is required for live record repair-dashboard (or pass --body-file + --comments-json / --fixture)",
                )
            })?;
        ensure_live_binary_for_command(
            binary,
            "record repair-dashboard --issue <number>",
            Some(
                "plan-issue-local record repair-dashboard --body-file <path> --comments-json <path>",
            ),
        )?;
        let issue_number = parse_issue_reference(issue_value)?;
        let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
        let adapter = crate::provider::select_adapter(&repo_info, force);
        let issue_url = repo_info.issue_url(issue_number);
        let repo = repo_info.slug;
        let (body, comments) = adapter
            .issue_evidence(&repo, issue_number)
            .map_err(|err| CommandError::runtime("record-repair-evidence-read-failed", err))?;
        (
            body,
            comments,
            Some(issue_number),
            Some(repo),
            Some(issue_url),
        )
    };

    let audit = lifecycle_record::audit_record(Some(&body), &comments_json, None)
        .map_err(|err| CommandError::runtime("record-repair-audit-failed", err))?;
    let dashboard =
        lifecycle_record::render_dashboard_from_audit(&audit, None, issue_url.as_deref());

    if let Some(out) = &args.out {
        render::write_rendered(out, &dashboard)
            .map_err(|err| CommandError::runtime("record-repair-out-write-failed", err))?;
        return Ok(json!({
            "operation": "record.repair-dashboard",
            "execution_mode": binary.execution_mode(),
            "dry_run": true,
            "mode": "local",
            "out_path": path_text(out),
            "dashboard_markdown": dashboard,
        }));
    }

    if dry_run || issue_number.is_none() {
        return Ok(json!({
            "operation": "record.repair-dashboard",
            "execution_mode": binary.execution_mode(),
            "dry_run": true,
            "mode": "dry-run",
            "dashboard_markdown": dashboard,
        }));
    }

    let issue_number = issue_number.expect("live mode has issue number");
    let repo = repo.expect("live mode has repo");
    // Re-resolve provider so this dispatcher works when the early branch
    // populated `repo` from the `--repo` slug alone (no Repo struct kept).
    let repo_info = crate::provider::resolve_repo(Some(&repo))
        .map_err(|err| CommandError::usage("repo-resolution-failed", err))?;
    let adapter = crate::provider::select_adapter(&repo_info, force);
    let dashboard_path = write_temp_markdown("record-repair-dashboard", &dashboard)
        .map_err(|err| CommandError::runtime("record-repair-dashboard-write-failed", err))?;
    adapter
        .edit_issue_body(&repo, issue_number, &dashboard_path)
        .map_err(|err| CommandError::runtime("record-repair-edit-failed", err))?;

    Ok(json!({
        "operation": "record.repair-dashboard",
        "execution_mode": binary.execution_mode(),
        "dry_run": false,
        "mode": "live",
        "issue": {"number": issue_number, "url": issue_url},
        "dashboard_markdown": dashboard,
    }))
}

fn run_record_close(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &RecordCloseArgs,
) -> Result<Value, CommandError> {
    let approval_text = args
        .approval
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            CommandError::usage(
                "record-close-missing-approval",
                "--approval is required and must be a non-empty URL or text",
            )
        })?;

    let override_reason = if args.allow_non_required_check_failure {
        let reason = args
            .allow_non_required_check_failure_reason
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                CommandError::usage(
                    "record-close-override-reason-missing",
                    "--allow-non-required-check-failure requires --allow-non-required-check-failure-reason <text>",
                )
            })?;
        Some(reason.to_string())
    } else {
        None
    };

    // Resolve evidence source.
    let (body, comments_json, repo_for_provider, issue_number) = if let Some(fixture_dir) =
        &args.fixture
    {
        let (body, comments) = read_fixture_evidence(fixture_dir)?;
        let issue_number = parse_issue_reference(&args.issue)?;
        (body, comments, None::<crate::provider::Repo>, issue_number)
    } else if let (Some(body_file), Some(comments_path)) = (&args.body_file, &args.comments_json) {
        let body = read_text_file(body_file, "record-close-body-read-failed")?;
        let comments = read_text_file(comments_path, "record-close-comments-read-failed")?;
        let issue_number = parse_issue_reference(&args.issue)?;
        (body, comments, None, issue_number)
    } else {
        ensure_live_binary_for_command(
            binary,
            "record close --issue <number> --linked-pr <ref> --approval <evidence>",
            Some(
                "plan-issue-local record close --issue <n> --body-file <path> --comments-json <path> --approval <evidence>",
            ),
        )?;
        let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
        let adapter = crate::provider::select_adapter(&repo_info, force);
        let issue_number = parse_issue_reference(&args.issue)?;
        let (body, comments) = adapter
            .issue_evidence(&repo_info.slug, issue_number)
            .map_err(|err| CommandError::runtime("record-close-evidence-read-failed", err))?;
        (body, comments, Some(repo_info), issue_number)
    };

    // Resolve linked PRs through provider/fixture for merge_sha + checks.
    let mut linked_evidence: Vec<lifecycle_record::LinkedPrEvidence> = Vec::new();
    for raw in &args.linked_pr {
        let (pr_repo, pr_number) = parse_linked_pr_reference(raw)?;
        if let Some(fixture_dir) = &args.fixture {
            let pr_ev = read_fixture_pr_snapshot(fixture_dir, &pr_repo, pr_number)?;
            linked_evidence.push(pr_ev);
        } else if let Some(provider_repo) = &repo_for_provider {
            // Pick the adapter from the PR's repo, not the issue's repo —
            // record-close supports cross-repo linked PRs (the PR lives in a
            // different owner/repo from the tracking issue).
            let _ = provider_repo;
            let pr_repo_info = crate::provider::resolve_repo(Some(&pr_repo))
                .map_err(|err| CommandError::usage("repo-resolution-failed", err))?;
            let adapter = crate::provider::select_adapter(&pr_repo_info, force);
            let summary = adapter
                .pr_merge_summary(&pr_repo, pr_number)
                .map_err(|err| CommandError::runtime("record-close-pr-summary-failed", err))?;
            let checks = check_status_from_state(summary.checks.as_deref());
            let required_state = summary
                .required_state
                .as_deref()
                .map(|s| check_status_from_state(Some(s)));
            linked_evidence.push(lifecycle_record::LinkedPrEvidence {
                pr_ref: format!("{pr_repo}#{pr_number}"),
                url: Some(pr_repo_info.pr_url(pr_number)),
                merge_sha: if summary.merged {
                    summary.merge_sha
                } else {
                    None
                },
                checks,
                required_state,
                required_count: summary.required_count,
                non_required_failures: summary.non_required_failures,
            });
            let _ = provider_repo;
        } else {
            // body-file/comments-json mode without fixture: cannot resolve PR
            // state without the provider. Treat as missing merge_sha.
            linked_evidence.push(lifecycle_record::LinkedPrEvidence {
                pr_ref: format!("{pr_repo}#{pr_number}"),
                url: None,
                merge_sha: None,
                checks: lifecycle_record::CheckStatus::None,
                required_state: None,
                required_count: None,
                non_required_failures: Vec::new(),
            });
        }
    }

    let audit = lifecycle_record::audit_record(Some(&body), &comments_json, Some(args.profile))
        .map_err(|err| CommandError::runtime("record-close-audit-failed", err))?;

    // Compute canonical final dashboard from audit.
    let issue_url_hint = repo_for_provider
        .as_ref()
        .map(|repo| repo.issue_url(issue_number));
    let canonical_dashboard =
        lifecycle_record::render_dashboard_from_audit(&audit, None, issue_url_hint.as_deref());

    let gate = lifecycle_record::evaluate_strict_closeout_gate(
        &audit,
        lifecycle_record::StrictCloseoutGateInput {
            profile: args.profile,
            approval: Some(approval_text),
            linked_prs: &linked_evidence,
            // record close repairs the dashboard as part of its own flow
            // (after posting the closeout comment), so the body-vs-canonical
            // diff is not a useful pre-flight signal here. The dashboard
            // failure mode remains available to other callers (e.g. a future
            // `record audit --strict` surface).
            current_body: None,
            expected_dashboard: None,
            allow_non_required_check_failure: args.allow_non_required_check_failure,
        },
    );

    if !gate.ready {
        return Err(CommandError::runtime(
            "record-close-gate-failed",
            format!(
                "strict closeout gate blocked: {} ({})",
                gate.blocked_codes.join(", "),
                gate.checks
                    .iter()
                    .filter(|check| check.status == "fail")
                    .map(|check| format!("{}: {}", check.check, check.detail))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        ));
    }

    // Render closeout comment using the same renderer as record post.
    let check_status_to_str = |status: lifecycle_record::CheckStatus| match status {
        lifecycle_record::CheckStatus::Pass => "pass",
        lifecycle_record::CheckStatus::Fail => "fail",
        lifecycle_record::CheckStatus::None => "none",
    };
    let override_block = override_reason.as_ref().map(|reason| {
        let observed_failures: Vec<String> = linked_evidence
            .iter()
            .flat_map(|pr| {
                pr.non_required_failures
                    .iter()
                    .map(move |name| format!("{}: {name}", pr.pr_ref))
            })
            .collect();
        json!({
            "reason": reason,
            "observed_non_required_failures": observed_failures,
        })
    });
    let final_validation_url = audit
        .evidence
        .get("validation")
        .and_then(|hit| hit.url.clone());
    let closeout_notes = if linked_evidence.is_empty() {
        Some(
            "No linked PRs were provided; closeout relied on issue-visible state, validation, review, and approval evidence.",
        )
    } else {
        None
    };
    let closeout_payload = json!({
        "final_status": "complete",
        "approval": {"comment_url": approval_text},
        "linked_prs": linked_evidence
            .iter()
            .map(|pr| {
                json!({
                    "ref": pr.pr_ref,
                    "url": pr.url,
                    "merge_sha": pr.merge_sha,
                    "checks": check_status_to_str(pr.checks),
                    "required_state": pr.required_state.map(check_status_to_str),
                    "required_count": pr.required_count,
                    "non_required_failures": pr.non_required_failures,
                })
            })
            .collect::<Vec<_>>(),
        "non_required_check_override": override_block,
        "final_validation_url": final_validation_url,
        "notes": closeout_notes,
    });
    let closeout_summary = if override_reason.is_some() {
        "Strict closeout gate passed with non-required-check failure override; record closed by `plan-issue record close`."
    } else {
        "Strict closeout gate passed; record closed by `plan-issue record close`."
    };
    let closeout_body = lifecycle_record::render_record_post_comment(
        args.profile,
        crate::commands::record::LifecycleCommentKind::Closeout,
        closeout_payload.clone(),
        Some(closeout_summary),
        None,
    )
    .map_err(|err| CommandError::runtime("record-close-render-failed", err))?;

    let (add_labels, remove_labels) =
        normalize_label_mutations(&args.add_labels, &args.remove_labels, "record-close")?;
    let label_mutation_planned = !add_labels.is_empty() || !remove_labels.is_empty();
    let labels_preview = json!({
        "add": add_labels.clone(),
        "remove": remove_labels.clone(),
    });

    // Bundle preview for dry-run and fixture modes.
    let preview = json!({
        "closeout_comment_body": closeout_body,
        "final_dashboard": canonical_dashboard,
        "blocked_codes": gate.blocked_codes,
        "checks": gate.checks,
        "labels": labels_preview.clone(),
    });

    if args.fixture.is_some() || dry_run || binary != BinaryFlavor::PlanIssue {
        return Ok(json!({
            "operation": "record.close",
            "execution_mode": binary.execution_mode(),
            "dry_run": true,
            "mode": if args.fixture.is_some() { "fixture" } else { "dry-run" },
            "issue": {"number": issue_number, "url": issue_url_hint},
            "linked_prs": linked_evidence,
            "preview": preview,
        }));
    }

    let repo_info = repo_for_provider.expect("live mode has repo");
    let adapter = crate::provider::select_adapter(&repo_info, force);
    let repo = repo_info.slug.clone();
    let issue_url = repo_info.issue_url(issue_number);
    let closeout_path = write_temp_markdown("record-close-comment", &closeout_body)
        .map_err(|err| CommandError::runtime("record-close-comment-write-failed", err))?;
    let closeout_url = adapter
        .comment_issue(&repo, issue_number, &closeout_path)
        .map_err(|err| CommandError::runtime("record-close-comment-post-failed", err))?;

    // Re-audit so the final dashboard includes the closeout URL.
    let (body_after, comments_after) = adapter
        .issue_evidence(&repo, issue_number)
        .map_err(|err| CommandError::runtime("record-close-evidence-reread-failed", err))?;
    let audit_after =
        lifecycle_record::audit_record(Some(&body_after), &comments_after, Some(args.profile))
            .map_err(|err| CommandError::runtime("record-close-audit-reread-failed", err))?;
    let final_dashboard =
        lifecycle_record::render_dashboard_from_audit(&audit_after, None, Some(&issue_url));
    let dashboard_path = write_temp_markdown("record-close-dashboard", &final_dashboard)
        .map_err(|err| CommandError::runtime("record-close-dashboard-write-failed", err))?;
    adapter
        .edit_issue_body(&repo, issue_number, &dashboard_path)
        .map_err(|err| CommandError::runtime("record-close-dashboard-edit-failed", err))?;
    adapter
        .close_issue(
            &repo,
            issue_number,
            crate::commands::plan::CloseReason::Completed,
            None,
        )
        .map_err(|err| CommandError::runtime("record-close-issue-close-failed", err))?;

    if label_mutation_planned {
        adapter
            .edit_issue_labels(&repo, issue_number, &add_labels, &remove_labels)
            .map_err(|err| CommandError::runtime("record-close-label-edit-failed", err))?;
    }

    Ok(json!({
        "operation": "record.close",
        "execution_mode": binary.execution_mode(),
        "dry_run": false,
        "mode": "live",
        "issue": {
            "number": issue_number,
            "url": issue_url,
        },
        "closeout_url": closeout_url,
        "linked_prs": linked_evidence,
        "final_dashboard": final_dashboard,
        "labels": labels_preview,
    }))
}

fn run_record_template(args: &RecordTemplateArgs) -> Result<Value, CommandError> {
    use crate::lifecycle_record::PayloadRole;
    use crate::lifecycle_vnext::templates;

    let role = match args.kind {
        LifecycleCommentKind::Source => PayloadRole::Source,
        LifecycleCommentKind::Plan => PayloadRole::Plan,
        LifecycleCommentKind::State => PayloadRole::State,
        LifecycleCommentKind::Session => PayloadRole::Session,
        LifecycleCommentKind::Validation => PayloadRole::Validation,
        LifecycleCommentKind::Review => PayloadRole::Review,
        LifecycleCommentKind::Closeout => PayloadRole::Closeout,
    };
    let format = match args.shape {
        TemplateFormatArg::Markdown => templates::TemplateFormat::Markdown,
        TemplateFormatArg::Json => templates::TemplateFormat::Json,
    };
    let template = templates::render_template(args.profile, role, format)
        .map_err(|err| CommandError::runtime(err.code(), err.to_string()))?;

    Ok(json!({
        "operation": "template",
        "profile": args.profile.as_str(),
        "role": match args.kind {
            LifecycleCommentKind::Source => "source",
            LifecycleCommentKind::Plan => "plan",
            LifecycleCommentKind::State => "state",
            LifecycleCommentKind::Session => "session",
            LifecycleCommentKind::Validation => "validation",
            LifecycleCommentKind::Review => "review",
            LifecycleCommentKind::Closeout => "closeout",
        },
        "shape": args.shape.as_str(),
        "template": template,
    }))
}

fn run_record_audit(args: &RecordAuditArgs) -> Result<Value, CommandError> {
    let body = match &args.body_file {
        Some(path) => Some(read_text_file(path, "record-body-read-failed")?),
        None => None,
    };
    let comments_json = read_text_file(&args.comments_json, "record-comments-read-failed")?;
    let audit = lifecycle_record::audit_record(body.as_deref(), &comments_json, args.profile)
        .map_err(|err| CommandError::runtime("record-audit-failed", err))?;

    let mut payload = json!({
        "operation": "audit",
        "audit": audit,
    });

    if args.expect_visible {
        let visible = run_record_audit_visible(&audit, &comments_json, args.profile)?;
        payload["visible"] = visible;
    }

    Ok(payload)
}

fn run_record_audit_visible(
    audit: &lifecycle_record::RecordAudit,
    comments_json: &str,
    profile_filter: Option<crate::commands::record::RecordProfile>,
) -> Result<Value, CommandError> {
    use crate::lifecycle_vnext::registry;
    use crate::lifecycle_vnext::visible_lint;

    let bodies = lifecycle_record::latest_role_bodies(comments_json, profile_filter)
        .map_err(|err| CommandError::runtime("record-audit-visible-failed", err))?;

    let mut role_reports: Vec<Value> = Vec::new();
    let mut all_codes: Vec<&'static str> = Vec::new();
    let mut overall_pass = true;

    // Walk roles in canonical order so the output is deterministic regardless
    // of HashMap iteration order.
    for spec in registry::all_roles() {
        let key = spec.marker_role.to_string();
        let evidence = audit.evidence.get(&key);
        let body = bodies.get(&spec.role);

        let mut report_value = json!({
            "role": spec.marker_role,
            "present": evidence.is_some(),
            "checked": body.is_some(),
        });

        if let Some(body) = body {
            let hints = derive_lint_hints(spec.role, evidence);
            let report = visible_lint::lint_visible(spec.role, body, hints);
            let codes: Vec<&'static str> = report.codes();
            if !report.is_pass() {
                overall_pass = false;
            }
            all_codes.extend(report.codes());
            let findings: Vec<Value> = report
                .findings
                .iter()
                .map(|f| {
                    json!({
                        "code": f.code,
                        "message": f.message,
                    })
                })
                .collect();
            report_value["pass"] = Value::Bool(report.is_pass());
            report_value["codes"] = json!(codes);
            report_value["findings"] = Value::Array(findings);
        } else {
            // No comment for this role; nothing to lint here. The
            // `audit.missing_required` block already names whether the role
            // is mandatory.
            report_value["pass"] = Value::Bool(true);
            report_value["codes"] = json!([] as [&str; 0]);
            report_value["findings"] = Value::Array(Vec::new());
        }

        role_reports.push(report_value);
    }

    Ok(json!({
        "expect_visible": true,
        "overall_pass": overall_pass,
        "codes": all_codes,
        "roles": role_reports,
    }))
}

fn derive_lint_hints(
    role: crate::lifecycle_record::PayloadRole,
    evidence: Option<&crate::lifecycle_record::LifecycleEvidence>,
) -> crate::lifecycle_vnext::visible_lint::LintHints {
    use crate::lifecycle_record::PayloadRole;
    use crate::lifecycle_vnext::visible_lint::LintHints;

    let mut hints = LintHints::default();
    let payload = evidence.and_then(|ev| ev.payload.as_ref());

    match role {
        PayloadRole::State => {
            // Final state when the structured payload reports `complete`.
            if let Some(payload) = payload
                && let Ok(state) = payload.parse_state()
            {
                hints.state_is_final = matches!(
                    state.status,
                    Some(crate::lifecycle_record::StateStatus::Complete)
                );
            }
        }
        PayloadRole::Review => {
            if let Some(payload) = payload
                && let Ok(review) = payload.parse_review()
            {
                hints.review_has_findings = !review.findings.is_empty();
            }
        }
        _ => {}
    }
    hints
}

struct RestorePlan {
    role: &'static str,
    path: String,
    commit: String,
    content: String,
}

fn run_record_restore(
    force: bool,
    repo_override: Option<&str>,
    args: &crate::commands::record::RecordRestoreArgs,
) -> Result<Value, CommandError> {
    use crate::lifecycle_record::{self, PayloadRole};

    // Resolve issue evidence: prefer offline comments JSON; otherwise fetch
    // live through the same provider read path `record audit` / `tracking
    // status` consume.
    let comments_json = if let Some(path) = &args.comments_json {
        read_text_file(path, "record-restore-comments-read-failed")?
    } else if let Some(issue_ref) = &args.issue {
        let repo = repo_override.ok_or_else(|| {
            CommandError::usage(
                "record-restore-missing-repo",
                "online restore requires `--repo owner/repo`; pass --comments-json for offline restore",
            )
        })?;
        let issue = parse_issue_reference(issue_ref)?;
        let repo_info = crate::provider::resolve_repo(Some(repo))
            .map_err(|err| CommandError::usage("repo-resolution-failed", err))?;
        let (_body, comments) =
            auto_fetch_issue_evidence(&repo_info.slug, issue, "record-restore-fetch-failed")?;
        comments
    } else {
        return Err(CommandError::usage(
            "record-restore-missing-input",
            "provide --comments-json <path> for offline restore, or --issue <n> with --repo for live restore",
        ));
    };

    let bodies = lifecycle_record::latest_role_bodies(&comments_json, args.profile)
        .map_err(|err| CommandError::runtime("record-restore-parse-failed", err))?;

    // Only `source` and `plan` embed a verbatim file snapshot; `state` is a
    // rendered lifecycle view with a structured payload (no file path), so it
    // is intentionally out of scope.
    let mut plans: Vec<RestorePlan> = Vec::new();
    let mut missing: Vec<&str> = Vec::new();
    for (role, name) in [(PayloadRole::Source, "source"), (PayloadRole::Plan, "plan")] {
        let Some(body) = bodies.get(&role) else {
            missing.push(name);
            continue;
        };
        let payload = lifecycle_record::extract_payload(body).map_err(|err| {
            CommandError::runtime(
                "record-restore-payload-failed",
                format!("{name} snapshot payload could not be parsed: {err}"),
            )
        })?;
        let snapshot = payload.parse_snapshot().map_err(|err| {
            CommandError::runtime(
                "record-restore-payload-failed",
                format!("{name} snapshot payload is not a source/plan snapshot: {err}"),
            )
        })?;
        let content = lifecycle_record::extract_snapshot_content(body).map_err(|err| {
            CommandError::runtime(
                "record-restore-content-failed",
                format!("{name} snapshot content could not be extracted: {err}"),
            )
        })?;
        plans.push(RestorePlan {
            role: name,
            path: snapshot.path,
            commit: snapshot.commit,
            content,
        });
    }

    if !missing.is_empty() {
        return Err(CommandError::runtime(
            "record-restore-missing-role",
            format!(
                "tracking issue is missing required snapshot role(s): {}",
                missing.join(", ")
            ),
        ));
    }

    // Validate canonical paths and detect overwrite conflicts before writing
    // anything, so a refused restore leaves the output directory untouched.
    let mut targets: Vec<(PathBuf, bool)> = Vec::new();
    for plan in &plans {
        let rel = Path::new(&plan.path);
        if rel.is_absolute()
            || rel
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(CommandError::runtime(
                "record-restore-unsafe-path",
                format!(
                    "refusing to restore `{}` to unsafe path `{}`",
                    plan.role, plan.path
                ),
            ));
        }
        let abs = args.out.join(rel);
        let exists = abs.exists();
        targets.push((abs, exists));
    }

    let conflicts: Vec<&str> = plans
        .iter()
        .zip(&targets)
        .filter(|(_, (_, exists))| *exists)
        .map(|(plan, _)| plan.path.as_str())
        .collect();
    if !conflicts.is_empty() && !force {
        return Err(CommandError::runtime(
            "record-restore-would-overwrite",
            format!(
                "refusing to overwrite existing file(s) without --force: {}",
                conflicts.join(", ")
            ),
        ));
    }

    let mut restored: Vec<Value> = Vec::new();
    for (plan, (abs, existed)) in plans.iter().zip(&targets) {
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                CommandError::runtime(
                    "record-restore-mkdir-failed",
                    format!("failed to create {}: {err}", parent.display()),
                )
            })?;
        }
        fs::write(abs, &plan.content).map_err(|err| {
            CommandError::runtime(
                "record-restore-write-failed",
                format!("failed to write {}: {err}", abs.display()),
            )
        })?;
        restored.push(json!({
            "role": plan.role,
            "path": plan.path,
            "commit": plan.commit,
            "absolute_path": abs.display().to_string(),
            "bytes": plan.content.len(),
            "overwritten": *existed,
        }));
    }

    Ok(json!({
        "operation": "record.restore",
        "out_dir": args.out.display().to_string(),
        "restored": restored,
        "roles": plans.iter().map(|plan| plan.role).collect::<Vec<_>>(),
        "note": "source and plan documents are restored verbatim from their <details> snapshots; the state role is a rendered lifecycle view, not a restorable file snapshot",
    }))
}

fn run_tracking(
    binary: BinaryFlavor,
    force: bool,
    repo_override: Option<&str>,
    args: &crate::commands::tracking::TrackingArgs,
) -> Result<Value, CommandError> {
    use crate::commands::tracking::{TrackingCommand, TrackingRunCommand};
    match &args.command {
        TrackingCommand::Status(status) => run_tracking_status(status),
        TrackingCommand::Run(run) => match &run.command {
            TrackingRunCommand::Init(args) => run_tracking_run_init(repo_override, args),
            TrackingRunCommand::Update(args) => run_tracking_run_update(args),
        },
        TrackingCommand::Checkpoint(args) => {
            run_tracking_checkpoint(binary, force, repo_override, args)
        }
        TrackingCommand::CloseReady(args) => run_tracking_close_ready(args),
    }
}

fn run_tracking_run_init(
    _repo_override: Option<&str>,
    args: &crate::commands::tracking::TrackingRunInitArgs,
) -> Result<Value, CommandError> {
    use crate::runtime_layout;
    use crate::tracking::events::{self, ExecutionEvent, ExecutionEventKind};
    use crate::tracking::run_state::{ExecutionRun, RunPhase, RunRoot, SelectedScope};

    let now = args.now.clone().unwrap_or_else(default_now);
    let run_id = args
        .run_id
        .clone()
        .unwrap_or_else(|| default_run_id(args.issue, &now));
    let mut run = ExecutionRun::new(
        run_id.clone(),
        args.provider_repo.clone(),
        args.issue,
        args.profile.as_str().to_string(),
        RunPhase::Initial,
        now.clone(),
    );
    run.bundle = args.bundle.clone();
    run.execution_state_file = args.execution_state_file.clone();
    if args.task.is_some() || args.sprint.is_some() {
        run.selected_scope = Some(SelectedScope {
            sprint: args.sprint,
            task: args.task.clone(),
            title: None,
        });
    }
    run.branch = args.branch.clone();
    run.worktree = args.worktree.clone();
    if let Some(linked) = &args.linked_pr {
        run.set_linked_pr(crate::tracking::run_state::LinkedPr {
            r#ref: linked.clone(),
            url: None,
            status: None,
        });
    }

    let (run_state_path, events_path) = if let Some(out) = &args.out {
        let events_path = out
            .parent()
            .map(|parent| parent.join("events.jsonl"))
            .unwrap_or_else(|| PathBuf::from("events.jsonl"));
        (out.clone(), events_path)
    } else {
        let repo_slug = runtime_layout::repo_slug(&args.provider_repo);
        let root = RunRoot::new(&repo_slug, args.issue, run_id.clone()).map_err(|err| {
            CommandError::runtime("tracking-run-init-layout-failed", err.to_string())
        })?;
        root.ensure_layout().map_err(|err| {
            CommandError::runtime("tracking-run-init-mkdir-failed", err.to_string())
        })?;
        (root.run_state_path(), root.events_path())
    };

    crate::tracking::run_state::write_run_state(&run_state_path, &run)
        .map_err(|err| CommandError::runtime("tracking-run-init-write-failed", err.to_string()))?;
    let event = ExecutionEvent::new(run_id.clone(), ExecutionEventKind::RunStarted, now.clone())
        .with_detail(serde_json::json!({
            "repo": args.provider_repo,
            "issue": args.issue,
            "profile": args.profile.as_str(),
        }));
    events::append_event(&events_path, &event).map_err(|err| {
        CommandError::runtime("tracking-run-init-event-append-failed", err.to_string())
    })?;

    Ok(json!({
        "operation": "tracking.run.init",
        "run_id": run_id,
        "run_state_path": path_text(&run_state_path),
        "events_path": path_text(&events_path),
        "repo": args.provider_repo,
        "issue": args.issue,
        "profile": args.profile.as_str(),
    }))
}

fn run_tracking_run_update(
    args: &crate::commands::tracking::TrackingRunUpdateArgs,
) -> Result<Value, CommandError> {
    use crate::tracking::events::{self, ExecutionEvent, ExecutionEventKind};
    use crate::tracking::run_state::{
        self, LinkedPr, RunPhase, ValidationCommandRow, ValidationSummary,
    };

    let mut run = run_state::read_run_state(&args.run_state)
        .map_err(|err| CommandError::runtime("tracking-run-update-read-failed", err.to_string()))?;
    let now = args.now.clone().unwrap_or_else(default_now);
    let mut changes = Vec::new();
    if let Some(phase) = args.phase {
        let new_phase = match phase {
            crate::commands::tracking::RunPhaseArg::Initial => RunPhase::Initial,
            crate::commands::tracking::RunPhaseArg::Implementing => RunPhase::Implementing,
            crate::commands::tracking::RunPhaseArg::Validating => RunPhase::Validating,
            crate::commands::tracking::RunPhaseArg::Reviewing => RunPhase::Reviewing,
            crate::commands::tracking::RunPhaseArg::Blocked => RunPhase::Blocked,
            crate::commands::tracking::RunPhaseArg::ReadyForClose => RunPhase::ReadyForClose,
            crate::commands::tracking::RunPhaseArg::Closed => RunPhase::Closed,
        };
        run.phase = new_phase;
        changes.push("phase");
    }
    if let Some(task) = &args.selected_task {
        let mut scope = run.selected_scope.clone().unwrap_or_default();
        scope.task = Some(task.clone());
        run.selected_scope = Some(scope);
        changes.push("selected_task");
    }
    if let Some(branch) = &args.branch {
        run.branch = Some(branch.clone());
        changes.push("branch");
    }
    if let Some(pr) = &args.linked_pr {
        run.set_linked_pr(LinkedPr {
            r#ref: pr.clone(),
            url: None,
            status: None,
        });
        changes.push("linked_pr");
    }
    if args.validation_overall.is_some()
        || args.validation_command.is_some()
        || args.validation_status.is_some()
        || args.validation_evidence.is_some()
    {
        let mut summary = run.validation.clone().unwrap_or_else(|| ValidationSummary {
            overall: "pending".to_string(),
            commands: Vec::new(),
            waiver: None,
            evidence_path: None,
        });
        if let Some(overall) = &args.validation_overall {
            summary.overall = overall.clone();
        }
        if let (Some(command), Some(status)) = (&args.validation_command, &args.validation_status) {
            summary.commands.push(ValidationCommandRow {
                command: command.clone(),
                status: status.clone(),
                evidence: args.validation_evidence.clone(),
            });
        }
        run.validation = Some(summary);
        changes.push("validation");
    }
    if let Some(decision) = &args.review_decision {
        let mut review =
            run.review
                .clone()
                .unwrap_or_else(|| crate::tracking::run_state::ReviewSummary {
                    decision: decision.clone(),
                    findings_disposition: Vec::new(),
                    evidence: None,
                });
        review.decision = decision.clone();
        run.review = Some(review);
        changes.push("review");
    }
    if let Some(note) = &args.note {
        run.notes.push(note.clone());
        changes.push("note");
    }
    run.updated_at = now.clone();

    run_state::write_run_state(&args.run_state, &run).map_err(|err| {
        CommandError::runtime("tracking-run-update-write-failed", err.to_string())
    })?;

    // Append an event next to the run-state file.
    if let Some(parent) = args.run_state.parent() {
        let events_path = parent.join("events.jsonl");
        let detail = serde_json::json!({"changed": changes});
        let event = ExecutionEvent::new(
            run.run_id.clone(),
            ExecutionEventKind::RunUpdated,
            now.clone(),
        )
        .with_detail(detail);
        events::append_event(&events_path, &event).map_err(|err| {
            CommandError::runtime("tracking-run-update-event-append-failed", err.to_string())
        })?;
    }

    Ok(json!({
        "operation": "tracking.run.update",
        "run_id": run.run_id,
        "phase": run.phase.as_str(),
        "changed": changes,
        "updated_at": run.updated_at,
    }))
}

fn run_tracking_checkpoint(
    binary: BinaryFlavor,
    force: bool,
    repo_override: Option<&str>,
    args: &crate::commands::tracking::TrackingCheckpointArgs,
) -> Result<Value, CommandError> {
    use crate::lifecycle_record::{self, PayloadRole};
    use crate::lifecycle_vnext::registry;
    use crate::lifecycle_vnext::visible_lint;
    use crate::tracking::reconcile;
    use crate::tracking::run_state;

    // Parse requested roles from the comma-separated `--post` flag.
    let requested_roles: Vec<PayloadRole> = args
        .post
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|name| match name {
            "state" => Ok(PayloadRole::State),
            "session" => Ok(PayloadRole::Session),
            "validation" => Ok(PayloadRole::Validation),
            "review" => Ok(PayloadRole::Review),
            "source" | "plan" | "closeout" => Err(CommandError::usage(
                "tracking-checkpoint-role-not-allowed",
                format!(
                    "role `{}` is not allowed from tracking checkpoint; use record open/close",
                    name
                ),
            )),
            other => Err(CommandError::usage(
                "tracking-checkpoint-unknown-role",
                format!("unknown lifecycle role `{}`", other),
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;

    if requested_roles.is_empty() {
        return Err(CommandError::usage(
            "tracking-checkpoint-empty-roles",
            "`--post` must name at least one lifecycle role",
        ));
    }

    // Read provider evidence (fixture or explicit files).
    let (body, comments_json) = resolve_checkpoint_inputs(args)?;
    let audit = if let Some(comments) = comments_json.as_deref() {
        Some(
            lifecycle_record::audit_record(body.as_deref(), comments, Some(args.profile))
                .map_err(|err| CommandError::runtime("tracking-checkpoint-audit-failed", err))?,
        )
    } else {
        None
    };

    // Read local run state.
    let run = run_state::read_run_state(&args.run_state).map_err(|err| {
        CommandError::runtime("tracking-checkpoint-run-state-read-failed", err.to_string())
    })?;

    // Reconcile.
    let reconciled = reconcile::reconcile(audit.as_ref(), Some(&run));
    let mut blocked: Vec<Value> = Vec::new();
    if reconciled.is_stale() {
        blocked.push(json!({
            "code": "run-state-stale",
            "message": "provider issue lifecycle evidence is newer than local run state; refuse live mutation",
            "suggested_unblock": "run `plan-issue tracking status` and update run state before checkpoint",
        }));
    }
    if reconciled.recommended_action.as_str() == "open_record" {
        blocked.push(json!({
            "code": "issue-evidence-missing",
            "message": "record has not been opened yet",
            "suggested_unblock": "run `plan-issue record open` first",
        }));
    }

    // Build per-role payloads from run state and render bodies.
    let mut rendered: Vec<Value> = Vec::new();
    let mut visible_failures: Vec<Value> = Vec::new();
    let mut roles_planned: Vec<&'static str> = Vec::new();
    let mut roles_skipped: Vec<Value> = Vec::new();

    for role in &requested_roles {
        let spec = registry::role(*role);
        let body_result = render_checkpoint_role(*role, &run, args.profile)?;
        match body_result {
            CheckpointRoleResult::Empty(reason) => {
                roles_skipped.push(json!({
                    "role": spec.marker_role,
                    "reason": reason,
                }));
                // A `review` role explicitly named in --post but with no
                // decision in run state is a caller error: a review checkpoint
                // with no decision carries no delivery evidence. Surface it as a
                // blocker (code matches the visible-completeness lint) instead
                // of a silent skip, so `--post state,review` does not report a
                // misleading state-only partial success. session/validation can
                // legitimately be empty and keep the skip-empty behavior.
                if matches!(*role, crate::lifecycle_record::PayloadRole::Review) {
                    blocked.push(json!({
                        "code": "review-missing-decision",
                        "role": spec.marker_role,
                        "message": "review role requested but run state has no review decision",
                        "suggested_unblock": "record a decision with `plan-issue tracking run update --review-decision <approve|request-changes|...>` before this checkpoint",
                    }));
                }
            }
            CheckpointRoleResult::Rendered(body) => {
                let hints = checkpoint_lint_hints(*role, &run);
                let report = visible_lint::lint_visible(*role, &body, hints);
                if report.is_pass() {
                    rendered.push(json!({
                        "role": spec.marker_role,
                        "body": body,
                        "lint_pass": true,
                    }));
                    roles_planned.push(spec.marker_role);
                } else {
                    let codes: Vec<&'static str> = report.codes();
                    visible_failures.push(json!({
                        "role": spec.marker_role,
                        "codes": codes,
                    }));
                    blocked.push(json!({
                        "code": "visible-completeness-failed",
                        "role": spec.marker_role,
                        "message": format!(
                            "rendered {} body fails visible-completeness lint",
                            spec.marker_role
                        ),
                        "suggested_unblock": "fix run state / execution-state Markdown before posting",
                    }));
                }
            }
        }
    }

    let dry_run = !args.live;
    // In dry-run mode, write rendered bodies for reproducibility.
    let rendered_out = if dry_run {
        let target_dir = args
            .rendered_out
            .clone()
            .or_else(|| args.run_state.parent().map(|p| p.join("rendered")));
        if let Some(dir) = target_dir.as_ref() {
            let _ = crate::runtime_layout::ensure_dir(dir);
            for entry in &rendered {
                let role_name = entry["role"].as_str().unwrap_or("unknown");
                let body = entry["body"].as_str().unwrap_or_default();
                let path = dir.join(format!("{role_name}-comment.md"));
                let _ = std::fs::write(&path, body);
            }
        }
        target_dir
    } else {
        None
    };

    // Live mode: post each rendered role through the same per-role write hop
    // that `record post` uses. Fixture mode short-circuits the adapter call
    // but otherwise returns the same response shape so deterministic smoke
    // probes can exercise the happy path. Pre-existing blockers (stale run
    // state, missing record, visible-completeness failures) short-circuit
    // posting before any provider mutation.
    let summary = if args.live && blocked.is_empty() && !rendered.is_empty() {
        post_checkpoint_live(
            args,
            &run,
            binary,
            force,
            repo_override,
            &rendered,
            &mut blocked,
        )?
    } else if args.live {
        CheckpointPostSummary {
            posted: Vec::new(),
            repair_dashboard_result: None,
            mode: if args.fixture.is_some() {
                "fixture"
            } else {
                "live"
            },
        }
    } else {
        CheckpointPostSummary {
            posted: Vec::new(),
            repair_dashboard_result: None,
            mode: "dry-run",
        }
    };

    Ok(json!({
        "operation": "tracking.checkpoint",
        "mode": summary.mode,
        "fsm_state": reconciled.state.as_str(),
        "roles_planned": roles_planned,
        "roles_skipped": roles_skipped,
        "rendered": rendered,
        "visible_failures": visible_failures,
        "blocked": blocked,
        "rendered_out": rendered_out.map(|p| p.to_string_lossy().to_string()),
        "repair_dashboard": args.repair_dashboard,
        "posted": summary.posted,
        "repair_dashboard_result": summary.repair_dashboard_result,
    }))
}

/// Summary of the live (or fixture) posting hop, separated out so the
/// `--live` branch can return per-role URLs and an optional dashboard repair
/// result alongside the existing rendered/visible_failures/blocked arrays.
struct CheckpointPostSummary {
    posted: Vec<Value>,
    repair_dashboard_result: Option<Value>,
    mode: &'static str,
}

/// Live (or fixture) per-role posting hop for `tracking checkpoint --live`.
///
/// In **live mode** this mirrors `run_record_post`'s posting hop:
/// `resolve_repo_info_for_live` → `provider::select_adapter` →
/// `write_temp_markdown` → `adapter.comment_issue` per rendered role,
/// preserving `--post` declaration order. On the first per-role failure the
/// function stops, pushes a stable `tracking-checkpoint-live-post-failed`
/// entry into `blocked`, and skips any pending roles plus `--repair-dashboard`
/// so a half-posted issue does not get a stale dashboard rewrite.
///
/// In **fixture mode** (`--fixture <dir>` supplied) the adapter call is
/// skipped entirely and synthesized `fixture://issue/<n>/<role>` URLs are
/// returned. This is the deterministic mode the runtime-smoke happy-path
/// probe relies on.
///
/// `tracking-checkpoint-live-not-implemented` is retained as a stable error
/// code for any future regression that reintroduces a refusal branch on the
/// `--live` path; the live-mode posting branch above no longer emits it.
fn post_checkpoint_live(
    args: &crate::commands::tracking::TrackingCheckpointArgs,
    run: &crate::tracking::run_state::ExecutionRun,
    binary: BinaryFlavor,
    force: bool,
    repo_override: Option<&str>,
    rendered: &[Value],
    blocked: &mut Vec<Value>,
) -> Result<CheckpointPostSummary, CommandError> {
    // Resolve the target issue: prefer the explicit `--issue` flag, then fall
    // back to the issue persisted in the run-state (written by `tracking run
    // init --issue`). The run-state is already this command's source of truth
    // for every other field, and `status` / `close-ready` consume `--issue`
    // the same way, so honoring it here lets the documented dispatch entrypoint
    // (`--run-state <rs>` with no `--provider-repo`/`--issue`) post instead of
    // silently no-opping with status=ok (finding #44). `issue == 0` is the
    // never-written sentinel and is treated as absent so the loud blocker
    // below still fires when nothing can be resolved.
    let issue_number = match args.issue.or((run.issue != 0).then_some(run.issue)) {
        Some(n) => n,
        None => {
            blocked.push(json!({
                "code": "tracking-checkpoint-live-missing-issue",
                "message": "`--issue <number>` is required for live tracking checkpoint and the run-state carries no issue to inherit",
                "suggested_unblock": "pass --issue <number>, or re-run `tracking run init --issue <number>` so the run-state carries it",
            }));
            return Ok(CheckpointPostSummary {
                posted: Vec::new(),
                repair_dashboard_result: None,
                mode: if args.fixture.is_some() {
                    "fixture"
                } else {
                    "live"
                },
            });
        }
    };

    // Fixture mode: synthesize URLs without provider mutation. The smoke
    // probe writes the rendered comment bodies back into its own fixture
    // and re-reads them through `tracking close-ready`; this hop only
    // surfaces the post-attempt shape so the probe can assert it.
    if args.fixture.is_some() {
        let posted: Vec<Value> = rendered
            .iter()
            .map(|entry| {
                let role = entry["role"].as_str().unwrap_or("unknown");
                json!({
                    "role": role,
                    "comment_url": format!("fixture://issue/{}/{}", issue_number, role),
                })
            })
            .collect();
        let repair_dashboard_result = if args.repair_dashboard {
            Some(json!({
                "operation": "record.repair-dashboard",
                "mode": "fixture",
                "dry_run": true,
            }))
        } else {
            None
        };
        return Ok(CheckpointPostSummary {
            posted,
            repair_dashboard_result,
            mode: "fixture",
        });
    }

    // True live mode. Refuse on `plan-issue-local`; the binary boundary
    // mirrors `record post`'s contract.
    ensure_live_binary_for_command(binary, "tracking checkpoint --live", None)?;

    // Mirror the issue fallback: prefer the flag / global `--repo` override,
    // then the run-state `repo` (finding #44). An empty slug is treated as
    // absent so `resolve_repo_info_for_live` can still apply its own discovery.
    let provider_repo_arg = args
        .provider_repo
        .as_deref()
        .or(repo_override)
        .or((!run.repo.is_empty()).then_some(run.repo.as_str()));
    let repo_info = resolve_repo_info_for_live(binary, provider_repo_arg)?;
    let adapter = crate::provider::select_adapter(&repo_info, force);
    let issue_url = repo_info.issue_url(issue_number);
    let repo = repo_info.slug.clone();

    let mut posted: Vec<Value> = Vec::new();
    let mut first_failure: Option<Value> = None;

    for entry in rendered {
        let role = entry["role"].as_str().unwrap_or("unknown");
        let body = entry["body"].as_str().unwrap_or_default();
        let comment_path =
            write_temp_markdown(&format!("tracking-checkpoint-{role}-comment"), body).map_err(
                |err| CommandError::runtime("tracking-checkpoint-comment-write-failed", err),
            )?;
        match adapter.comment_issue(&repo, issue_number, &comment_path) {
            Ok(url) => {
                posted.push(json!({
                    "role": role,
                    "comment_url": url,
                }));
            }
            Err(err) => {
                first_failure = Some(json!({
                    "code": "tracking-checkpoint-live-post-failed",
                    "role": role,
                    "message": format!("failed to post {role} comment: {err}"),
                    "suggested_unblock": "investigate provider error and retry; \
                                          earlier roles already posted are listed under `posted`",
                }));
                break;
            }
        }
    }

    if let Some(failure) = first_failure {
        blocked.push(failure);
        return Ok(CheckpointPostSummary {
            posted,
            repair_dashboard_result: None,
            mode: "live",
        });
    }

    // All requested roles posted. Optionally repair the dashboard against
    // the now-updated provider record. Skipping repair on partial failure
    // avoids overwriting the dashboard with a stale snapshot.
    let repair_dashboard_result = if args.repair_dashboard {
        Some(repair_dashboard_after_checkpoint(
            adapter.as_ref(),
            &repo,
            issue_number,
            &issue_url,
        )?)
    } else {
        None
    };

    Ok(CheckpointPostSummary {
        posted,
        repair_dashboard_result,
        mode: "live",
    })
}

/// Post-checkpoint dashboard repair. Mirrors the live branch of
/// `run_record_repair_dashboard` but kept local so the tracking checkpoint
/// path can stop on partial post failure without dragging the repair caller
/// into the abort logic.
fn repair_dashboard_after_checkpoint(
    adapter: &dyn crate::provider::ProviderAdapter,
    repo: &str,
    issue_number: u64,
    issue_url: &str,
) -> Result<Value, CommandError> {
    let (body, comments) = adapter.issue_evidence(repo, issue_number).map_err(|err| {
        CommandError::runtime("tracking-checkpoint-repair-evidence-read-failed", err)
    })?;
    let audit = crate::lifecycle_record::audit_record(Some(&body), &comments, None)
        .map_err(|err| CommandError::runtime("tracking-checkpoint-repair-audit-failed", err))?;
    let dashboard =
        crate::lifecycle_record::render_dashboard_from_audit(&audit, None, Some(issue_url));
    let dashboard_path = write_temp_markdown("tracking-checkpoint-repair-dashboard", &dashboard)
        .map_err(|err| {
            CommandError::runtime("tracking-checkpoint-repair-dashboard-write-failed", err)
        })?;
    adapter
        .edit_issue_body(repo, issue_number, &dashboard_path)
        .map_err(|err| CommandError::runtime("tracking-checkpoint-repair-edit-failed", err))?;
    Ok(json!({
        "operation": "record.repair-dashboard",
        "mode": "live",
        "issue": {"number": issue_number, "url": issue_url},
    }))
}

enum CheckpointRoleResult {
    Rendered(String),
    Empty(String),
}

fn render_checkpoint_role(
    role: crate::lifecycle_record::PayloadRole,
    run: &crate::tracking::run_state::ExecutionRun,
    profile: crate::commands::record::RecordProfile,
) -> Result<CheckpointRoleResult, CommandError> {
    use crate::commands::record::{LifecycleCommentKind, TaskLedgerDisplay};
    use crate::lifecycle_record::{self, PayloadRole};
    let kind = match role {
        PayloadRole::State => LifecycleCommentKind::State,
        PayloadRole::Session => LifecycleCommentKind::Session,
        PayloadRole::Validation => LifecycleCommentKind::Validation,
        PayloadRole::Review => LifecycleCommentKind::Review,
        _ => unreachable!("filtered by caller"),
    };

    let payload = match role {
        PayloadRole::State => state_checkpoint_payload(run),
        PayloadRole::Session => match synthesize_session_payload(run) {
            Some(value) => value,
            None => {
                return Ok(CheckpointRoleResult::Empty(
                    "no session content in run state; skip empty session checkpoint".to_string(),
                ));
            }
        },
        PayloadRole::Validation => match synthesize_validation_payload(run) {
            Some(value) => value,
            None => {
                return Ok(CheckpointRoleResult::Empty(
                    "no validation content in run state; skip empty validation checkpoint"
                        .to_string(),
                ));
            }
        },
        PayloadRole::Review => match synthesize_review_payload(run) {
            Some(value) => value,
            None => {
                return Ok(CheckpointRoleResult::Empty(
                    "no review content in run state; skip empty review checkpoint".to_string(),
                ));
            }
        },
        _ => unreachable!(),
    };

    // For the `state` role, render from the bundle's canonical
    // `execution-state.md` when one is known. This carries the full
    // per-task ledger (maintained by `plan-tooling ledger-update`)
    // into the lifecycle comment instead of the single-row synthesized
    // baseline. Fall back to the synthesized payload when no execution
    // state file is recorded or the file can't be read — the renderer
    // then continues to emit the deterministic baseline body.
    let summary = if matches!(role, PayloadRole::State) {
        load_state_markdown_summary(run)
    } else {
        None
    };
    let summary_ref = summary.as_deref();

    let body = lifecycle_record::render_record_post_comment_with_display(
        profile,
        kind,
        payload,
        summary_ref,
        Some(run.updated_at.as_str()),
        TaskLedgerDisplay::Auto,
    )
    .map_err(|err| CommandError::runtime("tracking-checkpoint-render-failed", err))?;
    Ok(CheckpointRoleResult::Rendered(body))
}

fn load_state_markdown_summary(run: &crate::tracking::run_state::ExecutionRun) -> Option<String> {
    let path = run.execution_state_file.as_ref()?;
    let content = std::fs::read_to_string(path).ok()?;
    if content.contains("## Task Ledger") {
        Some(content)
    } else {
        None
    }
}

/// Build the `state` checkpoint payload.
///
/// When the run names a canonical execution-state ledger, the hidden payload's
/// `tasks[]` carries the FULL accumulative per-task table (every task known at
/// post-time), so the provider issue is self-contained per-task history that
/// matches the visible Task Ledger. Falls back to the single-current
/// synthesized baseline when no ledger is recorded or it cannot be parsed.
fn state_checkpoint_payload(run: &crate::tracking::run_state::ExecutionRun) -> Value {
    let mut payload = synthesize_state_payload(run);
    if let Some(object) = payload.as_object_mut() {
        if let Some(tasks) = accumulative_state_tasks(run) {
            // The dashboard renders `Current task` / `Next action` straight
            // from this payload, so derive them from the durable ledger rather
            // than the never-advanced `selected_scope`. Otherwise a completed
            // plan still shows the first selected task and an empty next action
            // (graysurf/plan-tracking-testbed#54 / sympoies/nils-cli#700).
            let ready_to_close = matches!(
                run.phase,
                crate::tracking::run_state::RunPhase::ReadyForClose
                    | crate::tracking::run_state::RunPhase::Closed
            );
            let (current, next_action) = derive_ledger_progress(&tasks, ready_to_close);
            object.insert("current".to_string(), Value::String(current));
            object.insert("next_action".to_string(), Value::String(next_action));
            object.insert("tasks".to_string(), Value::Array(tasks));
        }
        // `Target scope` is the issue-backed plan scope, not a status word.
        // Prefer the authored `- Target scope:` line from the execution-state
        // header over the synthesized "in-progress" fallback.
        if let Some(scope) = execution_state_target_scope(run) {
            object.insert("target_scope".to_string(), Value::String(scope));
        }
        // Carry every linked PR so the dashboard (built from the latest state
        // payload's `prs[]`) names all lane PRs, not just the current one.
        let prs = accumulative_state_prs(run);
        if !prs.is_empty() {
            object.insert("prs".to_string(), Value::Array(prs));
        }
    }
    payload
}

/// Derive the dashboard `current` / `next_action` fields from the accumulative
/// ledger `tasks[]`. `current` is the first non-terminal row id, or `complete`
/// when every row is terminal (`done`/`deferred`/`waived`). `next_action` is
/// the next non-terminal row after `current`, or `closeout` once the run is at
/// ready-for-close or no work remains.
fn derive_ledger_progress(tasks: &[Value], ready_to_close: bool) -> (String, String) {
    let is_terminal = |task: &Value| {
        matches!(
            task.get("status").and_then(Value::as_str).unwrap_or(""),
            "done" | "deferred" | "waived"
        )
    };
    let id_of = |task: &Value| {
        task.get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let pending: Vec<&Value> = tasks.iter().filter(|task| !is_terminal(task)).collect();
    let current = match pending.first() {
        Some(task) => id_of(task),
        None => "complete".to_string(),
    };
    let next_action = if ready_to_close || pending.is_empty() {
        "closeout".to_string()
    } else {
        match pending.get(1) {
            Some(task) => id_of(task),
            None => "closeout".to_string(),
        }
    };
    (current, next_action)
}

/// Read the authored `- Target scope:` value from the run's execution-state
/// header, joining wrapped continuation lines into one string. Returns `None`
/// when no execution-state file is recorded, it cannot be read, or it has no
/// scope line. This keeps the dashboard `target_scope` anchored to the durable
/// plan scope instead of a synthesized status word.
fn execution_state_target_scope(run: &crate::tracking::run_state::ExecutionRun) -> Option<String> {
    let path = run.execution_state_file.as_ref()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = raw.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.trim_start().starts_with("- Target scope:"))?;
    let first = lines[start]
        .trim_start()
        .strip_prefix("- Target scope:")?
        .trim();
    let mut scope = first.to_string();
    for line in &lines[start + 1..] {
        let trimmed = line.trim();
        // Stop at a blank line, the next bullet, or the next heading; only an
        // indented wrap of the same bullet continues the value.
        if trimmed.is_empty()
            || trimmed.starts_with("- ")
            || trimmed.starts_with("## ")
            || !line.starts_with(char::is_whitespace)
        {
            break;
        }
        scope.push(' ');
        scope.push_str(trimmed);
    }
    let scope = scope.trim().to_string();
    (!scope.is_empty()).then_some(scope)
}

/// Build the accumulative `prs[]` payload from every linked PR the run has
/// seen (`linked_prs`), falling back to the current `pr` for run states
/// written before lane-PR accumulation. Dedup by ref, first-seen order, so a
/// dispatch dashboard names every lane PR instead of only the latest.
fn accumulative_state_prs(run: &crate::tracking::run_state::ExecutionRun) -> Vec<Value> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for pr in run.linked_prs.iter().chain(run.pr.iter()) {
        if seen.insert(pr.r#ref.clone()) {
            out.push(json!({
                "ref": pr.r#ref,
                "url": pr.url.clone(),
                "status": normalize_pr_status(pr.status.as_deref()),
            }));
        }
    }
    out
}

/// Map a run-state PR status into the `state.prs[].status` enum
/// (`open|merged|closed`); anything unknown degrades to `open` so the
/// synthesized payload always deserializes into `StateData`.
fn normalize_pr_status(status: Option<&str>) -> &'static str {
    match status.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("merged") => "merged",
        Some("closed") => "closed",
        _ => "open",
    }
}

/// Parse the canonical execution-state `## Task Ledger` into the accumulative
/// `tasks[]` payload shape. Returns `None` when no ledger is recorded, it
/// cannot be read, or it has no rows.
fn accumulative_state_tasks(run: &crate::tracking::run_state::ExecutionRun) -> Option<Vec<Value>> {
    let path = run.execution_state_file.as_ref()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let rows = plan_tooling::ledger::read_rows(&raw, path).ok()?;
    if rows.is_empty() {
        return None;
    }
    Some(
        rows.iter()
            .map(|row| {
                json!({
                    "id": row.id,
                    "status": normalize_task_status(&row.status),
                    "title": row.task,
                })
            })
            .collect(),
    )
}

/// Map a ledger Status cell to a valid `state.tasks[].status` value. The
/// execution-state ledger and the state payload share one status vocabulary;
/// unknown or empty cells degrade to `pending` so a malformed row never breaks
/// payload deserialization.
fn normalize_task_status(status: &str) -> &'static str {
    match status.trim() {
        "in-progress" => "in-progress",
        "done" => "done",
        "deferred" => "deferred",
        "blocked" => "blocked",
        "waived" => "waived",
        _ => "pending",
    }
}

fn synthesize_state_payload(run: &crate::tracking::run_state::ExecutionRun) -> Value {
    // Build a placeholder task row from the selected scope so the rendered
    // body carries a `## Task Ledger` section. This synthesizer is the
    // deterministic single-current baseline for `tracking checkpoint`
    // previews; `state_checkpoint_payload` replaces `tasks[]` with the full
    // accumulative ledger when the run names an execution-state file.
    let task_id = run
        .selected_scope
        .as_ref()
        .and_then(|s| s.task.clone())
        .unwrap_or_else(|| "1.1".to_string());
    let task_title = run
        .selected_scope
        .as_ref()
        .and_then(|s| s.title.clone())
        .unwrap_or_else(|| "selected".to_string());
    let task_status = match run.phase {
        crate::tracking::run_state::RunPhase::Closed
        | crate::tracking::run_state::RunPhase::ReadyForClose => "done",
        crate::tracking::run_state::RunPhase::Blocked => "blocked",
        _ => "in-progress",
    };
    json!({
        "status": match run.phase {
            crate::tracking::run_state::RunPhase::Closed
            | crate::tracking::run_state::RunPhase::ReadyForClose => "complete",
            crate::tracking::run_state::RunPhase::Blocked => "blocked",
            _ => "in-progress",
        },
        "target_scope": run
            .selected_scope
            .as_ref()
            .and_then(|s| s.title.clone())
            .unwrap_or_else(|| "in-progress".to_string()),
        "current": run.selected_scope.as_ref().and_then(|s| s.task.clone()).unwrap_or_default(),
        "next_action": "",
        "tasks": [
            {"id": task_id, "status": task_status, "title": task_title}
        ],
        "prs": [],
        "blockers": [],
        "links": {},
    })
}

fn synthesize_session_payload(run: &crate::tracking::run_state::ExecutionRun) -> Option<Value> {
    use crate::tracking::run_state::RunPhase;

    // An explicit note is the authoritative session summary.
    if let Some(summary) = run.notes.last().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        return Some(json!({
            "summary": summary,
            "highlights": run.notes.clone(),
            "links": {},
        }));
    }

    // No explicit note: synthesize a session summary from the run-state
    // activity (selected scope, branch, linked PRs, validation, phase) so a
    // requested `session` role posts instead of being silently dropped
    // (finding #45). This mirrors how `state` already renders straight from
    // run-state fields. A bare run-state with nothing to report — no scope,
    // branch, PR, or validation, still at the initial phase — still yields
    // None so a genuinely empty session checkpoint stays skipped.
    let mut highlights: Vec<String> = Vec::new();
    if let Some(scope) = run.selected_scope.as_ref() {
        match (scope.task.as_deref(), scope.title.as_deref()) {
            (Some(task), Some(title)) => highlights.push(format!("Task {task}: {title}")),
            (Some(task), None) => highlights.push(format!("Task {task}")),
            (None, Some(title)) => highlights.push(format!("Scope: {title}")),
            (None, None) => {}
        }
    }
    if let Some(branch) = run.branch.as_deref() {
        highlights.push(format!("Branch: {branch}"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for pr in run.linked_prs.iter().chain(run.pr.iter()) {
        if seen.insert(pr.r#ref.clone()) {
            highlights.push(format!("PR: {}", pr.r#ref));
        }
    }
    if let Some(validation) = run.validation.as_ref() {
        highlights.push(format!("Validation: {}", validation.overall));
    }

    let phase_has_progress = !matches!(run.phase, RunPhase::Initial);
    if highlights.is_empty() && !phase_has_progress {
        return None;
    }

    let summary = if highlights.is_empty() {
        format!("Session checkpoint at phase {}.", run.phase.as_str())
    } else {
        format!(
            "Session checkpoint at phase {}: {}.",
            run.phase.as_str(),
            highlights.join("; ")
        )
    };

    Some(json!({
        "summary": summary,
        "highlights": highlights,
        "links": {},
    }))
}

fn synthesize_validation_payload(run: &crate::tracking::run_state::ExecutionRun) -> Option<Value> {
    let validation = run.validation.as_ref()?;
    if validation.commands.is_empty() && validation.waiver.is_none() {
        return None;
    }
    let commands: Vec<Value> = validation
        .commands
        .iter()
        .map(|cmd| {
            json!({
                "command": cmd.command,
                "status": cmd.status,
                "evidence": cmd.evidence,
            })
        })
        .collect();
    Some(json!({
        "overall": validation.overall,
        "commands": commands,
        "waivers": validation
            .waiver
            .as_ref()
            .map(|w| vec![json!({"command": "validation", "reason": w})])
            .unwrap_or_default(),
    }))
}

fn synthesize_review_payload(run: &crate::tracking::run_state::ExecutionRun) -> Option<Value> {
    let review = run.review.as_ref()?;
    Some(json!({
        "decision": review.decision,
        "lenses": [],
        "findings": [],
        "outcome_comment_url": review.evidence,
    }))
}

fn checkpoint_lint_hints(
    role: crate::lifecycle_record::PayloadRole,
    run: &crate::tracking::run_state::ExecutionRun,
) -> crate::lifecycle_vnext::visible_lint::LintHints {
    use crate::lifecycle_record::PayloadRole;
    use crate::lifecycle_vnext::visible_lint::LintHints;
    let mut hints = LintHints::default();
    if matches!(role, PayloadRole::State) {
        hints.state_is_final = matches!(
            run.phase,
            crate::tracking::run_state::RunPhase::ReadyForClose
                | crate::tracking::run_state::RunPhase::Closed
        );
    }
    hints
}

fn resolve_checkpoint_inputs(
    args: &crate::commands::tracking::TrackingCheckpointArgs,
) -> Result<(Option<String>, Option<String>), CommandError> {
    if let Some(fixture) = &args.fixture {
        let body_path = fixture.join("body.md");
        let comments_path = fixture.join("comments.json");
        let body = if body_path.exists() {
            Some(read_text_file(
                &body_path,
                "tracking-checkpoint-body-read-failed",
            )?)
        } else {
            None
        };
        let comments = if comments_path.exists() {
            Some(read_text_file(
                &comments_path,
                "tracking-checkpoint-comments-read-failed",
            )?)
        } else {
            None
        };
        return Ok((body, comments));
    }
    let body = match &args.body_file {
        Some(path) => Some(read_text_file(
            path,
            "tracking-checkpoint-body-read-failed",
        )?),
        None => None,
    };
    let comments = match &args.comments_json {
        Some(path) => Some(read_text_file(
            path,
            "tracking-checkpoint-comments-read-failed",
        )?),
        None => None,
    };
    // Auto-fetch live provider evidence when no fixture / explicit files
    // were supplied but the issue is named. The live checkpoint path
    // needs body + comments to determine `RECORD_OPEN_INITIAL` and avoid
    // the `issue-evidence-missing` blocker; without this, callers must
    // pre-snapshot before every checkpoint.
    if body.is_none()
        && comments.is_none()
        && let (Some(repo), Some(issue)) = (args.provider_repo.as_deref(), args.issue)
    {
        let (b, c) =
            auto_fetch_issue_evidence(repo, issue, "tracking-checkpoint-evidence-fetch-failed")?;
        return Ok((Some(b), Some(c)));
    }
    Ok((body, comments))
}

fn run_tracking_close_ready(
    args: &crate::commands::tracking::TrackingCloseReadyArgs,
) -> Result<Value, CommandError> {
    use crate::lifecycle_record::{self};
    use crate::tracking::reconcile;
    use crate::tracking::run_state;

    let (body, comments_json) = resolve_close_ready_inputs(args)?;
    let audit = if let Some(comments) = comments_json.as_deref() {
        Some(
            lifecycle_record::audit_record(body.as_deref(), comments, Some(args.profile))
                .map_err(|err| CommandError::runtime("tracking-close-ready-audit-failed", err))?,
        )
    } else {
        None
    };
    let run = match &args.run_state {
        Some(path) => Some(run_state::read_run_state(path).map_err(|err| {
            CommandError::runtime(
                "tracking-close-ready-run-state-read-failed",
                err.to_string(),
            )
        })?),
        None => None,
    };

    let reconciled = reconcile::reconcile(audit.as_ref(), run.as_ref());
    let mut blockers: Vec<Value> = Vec::new();
    let mut linked_prs: Vec<String> = args.linked_pr.clone();

    if let Some(audit) = audit.as_ref()
        && let Some(state_evidence) = audit.evidence.get("state")
        && let Some(payload) = state_evidence.payload.as_ref()
        && let Ok(state) = payload.parse_state()
    {
        for pr in state.prs {
            linked_prs.push(pr.pr_ref);
        }
    }
    if let Some(run) = run.as_ref()
        && let Some(pr) = run.pr.as_ref()
    {
        linked_prs.push(pr.r#ref.clone());
    }
    linked_prs.sort();
    linked_prs.dedup();

    // Missing roles for closeout block.
    for code in &reconciled.missing_for_closeout {
        blockers.push(json!({
            "code": format!("{code}-missing"),
            "message": format!("closeout requires `{code}` evidence"),
            "suggested_unblock": "post the missing lifecycle evidence before close",
        }));
    }
    if linked_prs.is_empty() && args.approval.is_none() {
        blockers.push(json!({
            "code": "closeout-missing-linked-pr",
            "message": "no linked PR evidence and no `--approval` provided",
            "suggested_unblock": "pass --linked-pr or --approval, or add the evidence to run state",
        }));
    }

    // Ledger-rows-pending blocker (Task 1.3): when phase indicates the lane
    // is ready for close or already closed, every Task Ledger row in the
    // bundle must be done/blocked/waived. Silent-skip when bundle is absent
    // or the file cannot be read so older run-states without a bundle field
    // keep working.
    if let Some(run) = run.as_ref() {
        let phase_gates_ledger = matches!(
            run.phase,
            crate::tracking::run_state::RunPhase::ReadyForClose
                | crate::tracking::run_state::RunPhase::Closed
        );
        if phase_gates_ledger
            && let Some(bundle) = run.bundle.as_ref()
            && let Some(ledger_path) = find_execution_state(bundle)
            && let Ok(raw) = std::fs::read_to_string(&ledger_path)
            && let Ok(rows) = plan_tooling::ledger::read_rows(&raw, &ledger_path)
        {
            for row in &rows {
                if row.status == "pending" || row.status == "in-progress" {
                    blockers.push(json!({
                        "code": "ledger-rows-pending",
                        "task_id": row.id,
                        "status": row.status,
                        "message": "ledger row still pending at phase=ready_for_close",
                        "suggested_unblock": format!(
                            "plan-tooling ledger-update --task '{}' --status done --evidence <evidence>",
                            row.id
                        ),
                    }));
                }
            }
        }
    }

    // Visible completeness check (Task 6.2 deep gate).
    let mut visible_summary = json!({"checked": false});
    if args.expect_visible
        && let (Some(audit), Some(comments)) = (audit.as_ref(), comments_json.as_deref())
    {
        let visible = run_record_audit_visible(audit, comments, Some(args.profile))?;
        let overall_pass = visible["overall_pass"].as_bool().unwrap_or(false);
        visible_summary = json!({
            "checked": true,
            "pass": overall_pass,
            "report": visible,
        });
        if !overall_pass {
            blockers.push(json!({
                "code": "visible-completeness-failed",
                "message": "visible-completeness lint reported missing sections",
                "suggested_unblock": "fix the rendered lifecycle comments before close",
            }));
        }
    }

    let ready = blockers.is_empty()
        && matches!(
            reconciled.state,
            crate::tracking::fsm::RecordState::RecordReadyForClose
        );

    Ok(json!({
        "operation": "tracking.close-ready",
        "ready": ready,
        "fsm_state": reconciled.state.as_str(),
        "blockers": blockers,
        "linked_prs": linked_prs,
        "visible_completeness": visible_summary,
    }))
}

fn find_execution_state(bundle: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(bundle).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.ends_with("-execution-state.md")
        {
            return Some(path);
        }
    }
    None
}

fn resolve_close_ready_inputs(
    args: &crate::commands::tracking::TrackingCloseReadyArgs,
) -> Result<(Option<String>, Option<String>), CommandError> {
    if let Some(fixture) = &args.fixture {
        let body_path = fixture.join("body.md");
        let comments_path = fixture.join("comments.json");
        let body = if body_path.exists() {
            Some(read_text_file(
                &body_path,
                "tracking-close-ready-body-read-failed",
            )?)
        } else {
            None
        };
        let comments = if comments_path.exists() {
            Some(read_text_file(
                &comments_path,
                "tracking-close-ready-comments-read-failed",
            )?)
        } else {
            None
        };
        return Ok((body, comments));
    }
    let body = match &args.body_file {
        Some(path) => Some(read_text_file(
            path,
            "tracking-close-ready-body-read-failed",
        )?),
        None => None,
    };
    let comments = match &args.comments_json {
        Some(path) => Some(read_text_file(
            path,
            "tracking-close-ready-comments-read-failed",
        )?),
        None => None,
    };
    // Fall back to live provider lookup when no fixture / explicit files
    // were given but the caller named the issue. Keeps the close-ready
    // surface usable in the prescribed live `tracking close-ready
    // --provider-repo … --issue …` shape without forcing every caller to
    // pre-snapshot the issue.
    if body.is_none()
        && comments.is_none()
        && let (Some(repo), Some(issue)) = (args.provider_repo.as_deref(), args.issue)
    {
        let (b, c) =
            auto_fetch_issue_evidence(repo, issue, "tracking-close-ready-evidence-fetch-failed")?;
        return Ok((Some(b), Some(c)));
    }
    Ok((body, comments))
}

/// Auto-fetch (body, comments_json) for live tracking calls so callers do
/// not have to pre-snapshot the issue when `--provider-repo` + `--issue`
/// are available. Read-only; uses the default `force=false` adapter.
fn auto_fetch_issue_evidence(
    provider_repo: &str,
    issue: u64,
    error_code: &'static str,
) -> Result<(String, String), CommandError> {
    let repo_info = crate::provider::resolve_repo(Some(provider_repo))
        .map_err(|err| CommandError::usage("repo-resolution-failed", err))?;
    let adapter = crate::provider::select_adapter(&repo_info, false);
    adapter
        .issue_evidence(&repo_info.slug, issue)
        .map_err(|err| CommandError::runtime(error_code, err))
}

fn default_run_id(issue: u64, now: &str) -> String {
    let mut id = String::new();
    let clean: String = now.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    id.push_str(&clean);
    id.push_str(&format!("-issue-{issue}"));
    id
}

fn default_now() -> String {
    // Safe default when no `--now` is supplied: real wall-clock UTC time, so a
    // live `tracking run init`/`update` never records the 1970 epoch placeholder
    // into run-state (issue #588). Tests and deterministic fixtures pass an
    // explicit `--now` to override this.
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn run_tracking_status(
    args: &crate::commands::tracking::TrackingStatusArgs,
) -> Result<Value, CommandError> {
    use crate::lifecycle_record;
    use crate::tracking::reconcile;
    use crate::tracking::run_state;

    let (body, comments_json) = resolve_tracking_status_inputs(args)?;
    let audit = if let Some(comments) = comments_json.as_deref() {
        Some(
            lifecycle_record::audit_record(body.as_deref(), comments, Some(args.profile))
                .map_err(|err| CommandError::runtime("tracking-status-audit-failed", err))?,
        )
    } else {
        None
    };

    let run_state_value = match &args.run_state {
        Some(path) => match run_state::read_run_state(path) {
            Ok(value) => Some(value),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(CommandError::runtime(
                    "tracking-status-run-state-read-failed",
                    err.to_string(),
                ));
            }
        },
        None => None,
    };

    let reconciled = reconcile::reconcile(audit.as_ref(), run_state_value.as_ref());

    let mut payload = serde_json::json!({
        "operation": "tracking.status",
        "fsm_state": reconciled.state.as_str(),
        "recommended_action": reconciled.recommended_action.as_str(),
        "safe_transitions": reconciled.safe_transitions,
        "missing_for_closeout": reconciled.missing_for_closeout,
        "warnings": reconciled.warnings.iter().map(|w| serde_json::json!({
            "code": w.code,
            "message": w.message,
        })).collect::<Vec<_>>(),
        "blocked_reason": reconciled.blocked_reason,
        "issue_truth": tracking_status_audit_summary(audit.as_ref()),
        "run_state": tracking_status_run_state_summary(run_state_value.as_ref()),
    });

    if args.expect_visible
        && let (Some(audit), Some(comments)) = (audit.as_ref(), comments_json.as_deref())
    {
        let visible = run_record_audit_visible(audit, comments, Some(args.profile))?;
        payload["visible"] = visible;
    }

    Ok(payload)
}

fn resolve_tracking_status_inputs(
    args: &crate::commands::tracking::TrackingStatusArgs,
) -> Result<(Option<String>, Option<String>), CommandError> {
    if let Some(fixture) = &args.fixture {
        let body_path = fixture.join("body.md");
        let comments_path = fixture.join("comments.json");
        let body = if body_path.exists() {
            Some(read_text_file(
                &body_path,
                "tracking-status-body-read-failed",
            )?)
        } else {
            None
        };
        let comments = if comments_path.exists() {
            Some(read_text_file(
                &comments_path,
                "tracking-status-comments-read-failed",
            )?)
        } else {
            None
        };
        return Ok((body, comments));
    }
    let body = match &args.body_file {
        Some(path) => Some(read_text_file(path, "tracking-status-body-read-failed")?),
        None => None,
    };
    let comments = match &args.comments_json {
        Some(path) => Some(read_text_file(
            path,
            "tracking-status-comments-read-failed",
        )?),
        None => None,
    };
    if body.is_none() && comments.is_none() {
        // Auto-fetch live provider evidence when the issue is named but no
        // file / fixture inputs were supplied. Keeps `tracking status
        // --provider-repo … --issue …` usable without a preceding
        // `gh issue view …` snapshot.
        if let (Some(repo), Some(issue)) = (args.provider_repo.as_deref(), args.issue) {
            let (b, c) =
                auto_fetch_issue_evidence(repo, issue, "tracking-status-evidence-fetch-failed")?;
            return Ok((Some(b), Some(c)));
        }
        return Err(CommandError::usage(
            "tracking-status-missing-input",
            "tracking status requires --fixture <dir>, --comments-json <path>, --body-file <path>, or `--provider-repo` + `--issue` for live fetch",
        ));
    }
    Ok((body, comments))
}

fn tracking_status_audit_summary(audit: Option<&crate::lifecycle_record::RecordAudit>) -> Value {
    let Some(audit) = audit else {
        return json!({"available": false});
    };
    let latest_roles: Vec<&String> = audit.evidence.keys().collect();
    json!({
        "available": true,
        "recognized_count": audit.recognized_count,
        "latest_roles": latest_roles,
        "missing_required": audit.missing_required,
    })
}

fn tracking_status_run_state_summary(
    run: Option<&crate::tracking::run_state::ExecutionRun>,
) -> Value {
    let Some(run) = run else {
        return json!({"available": false});
    };
    json!({
        "available": true,
        "run_id": run.run_id,
        "phase": run.phase.as_str(),
        "selected_task": run.selected_scope.as_ref().and_then(|s| s.task.clone()),
        "branch": run.branch,
        "pr": run.pr.as_ref().map(|p| p.r#ref.clone()),
        "validation_overall": run.validation.as_ref().map(|v| v.overall.clone()),
    })
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

    let repo = crate::provider::resolve_repo(repo_override)
        .map(|info| info.slug)
        .map_err(|err| CommandError::usage("repo-resolution-failed", err))?;
    let repo_slug = runtime_layout::repo_slug(&repo);

    let mut issue_number: Option<u64> = None;
    let mut issue_url: Option<String> = None;
    let mut live_mutations = false;

    if binary == BinaryFlavor::PlanIssue && !dry_run {
        let temp_body = write_temp_markdown("plan-issue-body", &issue_body)
            .map_err(|err| CommandError::runtime("issue-body-write-failed", err))?;
        let adapter = select_adapter_for_slug(&repo, force)?;
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
        "repo_slug": repo_slug,
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
    // Adapter is constructed lazily once the dispatcher knows whether it is
    // in body-file mode (no adapter) or live mode (adapter selected from
    // resolved repo provider).
    let mut adapter: Option<Box<dyn crate::provider::ProviderAdapter>> = None;

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
        let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
        let live_adapter = crate::provider::select_adapter(&repo_info, force);
        let repo = repo_info.slug;
        let body = live_adapter
            .issue_body(&repo, issue)
            .map_err(|err| CommandError::runtime("github-issue-read-failed", err))?;
        adapter = Some(live_adapter);
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
        let live_adapter = adapter
            .as_ref()
            .expect("status-plan live comment branch ran the issue path that constructed adapter");
        live_adapter
            .comment_issue(repo, issue, &comment_path)
            .map_err(|err| CommandError::runtime("github-comment-failed", err))?;
        live_mutations = true;
    }

    // Repo slug is the runtime fact orchestrators most often rebuild from
    // strings; expose it whenever we know it. For body-file flows where the
    // operator did not pass `--repo`, fall back to whatever repo_override
    // resolves to (None when no remote is detected).
    let repo_slug = match repo.as_deref() {
        Some(repo) => Some(runtime_layout::repo_slug(repo)),
        None => crate::provider::resolve_repo(repo_override)
            .ok()
            .map(|info| runtime_layout::repo_slug(&info.slug)),
    };

    Ok(json!({
        "scope": "plan",
        "execution_mode": binary.execution_mode(),
        "dry_run": dry_run,
        "issue_source": source,
        "repo_slug": repo_slug,
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
    // Adapter is constructed lazily once we know whether we're in body-file
    // mode (no adapter needed) or live mode (adapter selected based on
    // resolved repo provider).
    let mut adapter: Option<Box<dyn crate::provider::ProviderAdapter>> = None;

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
        let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
        let live_adapter = crate::provider::select_adapter(&repo_info, force);
        let repo = repo_info.slug;
        let body = live_adapter
            .issue_body(&repo, issue)
            .map_err(|err| CommandError::runtime("github-issue-read-failed", err))?;
        adapter = Some(live_adapter);
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
        // In live mode the adapter was constructed in the issue branch
        // above; this `expect` is unreachable because that branch is the
        // only way `repo` becomes `Some` without `body_file_path`.
        let live_adapter = adapter
            .as_ref()
            .expect("link-pr live mode constructed adapter alongside repo");
        live_adapter
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
        // Enumerate the actual pr-group values present on this sprint's
        // rows so the error message names the real options. Aligns with
        // start-sprint's `pr_groups` payload (Task 1.3) — orchestrators
        // should pass the same names verbatim.
        let mut available_groups: BTreeSet<String> = BTreeSet::new();
        for idx in &row_indexes {
            if let Some(value) = note_value(&rows[*idx].notes, "pr-group") {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    available_groups.insert(trimmed.to_string());
                }
            }
        }
        row_indexes.retain(|idx| {
            note_value(&rows[*idx].notes, "pr-group")
                .is_some_and(|value| value.trim().eq_ignore_ascii_case(group))
        });
        if row_indexes.is_empty() {
            let suggestion = if available_groups.is_empty() {
                String::from("this sprint has no pr-group rows; use --task instead")
            } else {
                let options = available_groups
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("valid pr-group values for sprint S{sprint}: {options}")
            };
            return Err(format!(
                "sprint S{sprint} has no rows with pr-group `{group}`; {suggestion}"
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
    let mut adapter: Option<Box<dyn crate::provider::ProviderAdapter>> = None;

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
        let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
        let live_adapter = crate::provider::select_adapter(&repo_info, force);
        let repo = repo_info.slug;
        let body = live_adapter
            .issue_body(&repo, issue)
            .map_err(|err| CommandError::runtime("github-issue-read-failed", err))?;
        adapter = Some(live_adapter);
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
        let live_adapter = adapter
            .as_ref()
            .expect("ready-plan live branch ran the issue path that constructed adapter");
        if args.label_update {
            live_adapter
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
            live_adapter
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
            "--approved-comment-url must be a GitHub issue/pull or GitLab issue/MR note comment URL",
        ));
    }

    let mut adapter: Option<Box<dyn crate::provider::ProviderAdapter>> = None;
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
        let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
        let live_adapter = crate::provider::select_adapter(&repo_info, force);
        let repo = repo_info.slug;
        let body = live_adapter
            .issue_body(&repo, issue)
            .map_err(|err| CommandError::runtime("github-issue-read-failed", err))?;
        adapter = Some(live_adapter);
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
        // Construct adapter lazily for the merge-gate check; the close-plan
        // dispatcher may reach this in body-file mode where the issue path
        // never ran (so `adapter` is still None).
        if adapter.is_none() {
            adapter = Some(select_adapter_for_slug(repo, force)?);
        }
        let adapter_ref = adapter.as_ref().unwrap().as_ref();
        ensure_prs_merged(adapter_ref, repo, &required_prs, "close-plan")
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

        let live_adapter = match adapter.as_ref() {
            Some(a) => a,
            None => {
                // body-file mode resolved repo from `--repo` but didn't run
                // through the issue path, so the adapter wasn't constructed
                // yet. Build it now from the resolved slug.
                adapter = Some(select_adapter_for_slug(repo, force)?);
                adapter.as_ref().unwrap()
            }
        };
        live_adapter
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

    let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
    let adapter = crate::provider::select_adapter(&repo_info, force);
    let repo = repo_info.slug;
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
    // Task 1.2: read plan-declared `pr-grouping` for this sprint and
    // default `--strategy` / `--default-pr-grouping` from it when the
    // operator passed neither.
    let mut grouping = args.grouping.clone();
    let mut inferred_defaults_note: Option<String> = None;
    if let Some(inferred) =
        infer_grouping_defaults_from_plan(&args.plan, i32::from(args.sprint), &grouping)
    {
        apply_inferred_grouping_defaults(&mut grouping, &inferred);
        inferred_defaults_note = Some(format!(
            "auto/{}",
            match inferred.default_pr_grouping {
                crate::commands::PrGrouping::PerSprint => "per-sprint",
                crate::commands::PrGrouping::Group => "group",
            }
        ));
    }

    let options = to_build_options(
        args.prefixes.owner_prefix.clone(),
        args.prefixes.branch_prefix.clone(),
        args.prefixes.worktree_prefix.clone(),
        grouping.pr_grouping,
        grouping.default_pr_grouping,
        grouping.strategy,
        grouping.pr_group.clone(),
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

    // Adapter is constructed lazily inside the live `BinaryFlavor::PlanIssue`
    // branch below, where the resolved provider is known. body-file mode
    // doesn't shell out to a provider so it never reaches the adapter.
    let mut issue_body_for_comment: Option<String> = None;
    let mut synced_rows = 0usize;
    let mut live_mutations = false;

    if binary == BinaryFlavor::PlanIssue {
        let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
        let adapter = crate::provider::select_adapter(&repo_info, force);
        let repo = repo_info.slug;
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
            enforce_previous_sprint_gate(
                adapter.as_ref(),
                &repo,
                table.rows(),
                i32::from(args.sprint),
            )
            .map_err(|err| CommandError::runtime("previous-sprint-gate-failed", err))?;
        }

        artifact_rows = task_spec_rows_from_issue_rows(table.rows(), i32::from(args.sprint))
            .map_err(|err| CommandError::runtime("task-spec-from-issue-rows-failed", err))?;
        synced_rows = artifact_rows.len();
        ensure_start_sprint_runtime_truth_matches_plan(
            table.rows(),
            i32::from(args.sprint),
            &build.rows,
            grouping.strategy,
        )
        .map_err(|err| CommandError::runtime("task-sync-drift-detected", err))?;
        issue_body_for_comment = Some(body);
    }

    let repo = crate::provider::resolve_repo(repo_override)
        .map(|info| info.slug)
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

    let prompt_files = write_subagent_prompts(
        &prompts_dir,
        args.issue,
        i32::from(args.sprint),
        &artifact_rows,
        grouping.strategy,
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
        let execution_mode = execution_mode_for_row(row, grouping.strategy, &artifact_rows);
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

    // Aggregate pr_groups for the start-sprint result payload (Task 1.3).
    // Each row carries the resolved `pr_group` string (e.g. `s1`,
    // `s1-auto-g1`, `s2-core`); group rows by that name so orchestrators
    // can pass the value verbatim to `link-pr --pr-group`.
    let pr_groups: Vec<Value> = {
        let mut by_group: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for row in &artifact_rows {
            by_group
                .entry(row.pr_group.clone())
                .or_default()
                .push(row.task_id.clone());
        }
        by_group
            .into_iter()
            .map(|(name, mut task_ids)| {
                task_ids.sort();
                json!({ "name": name, "task_ids": task_ids })
            })
            .collect()
    };

    let prompt_manifest_path = sprint_root.prompt_manifest();
    write_prompt_manifest(
        &prompt_manifest_path,
        &artifact_rows,
        &sprint_root,
        grouping.strategy,
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
        strategy: grouping.strategy,
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
        let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
        let adapter = crate::provider::select_adapter(&repo_info, force);
        let repo = repo_info.slug;
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
        "repo_slug": repo_slug,
        "plan_snapshot_path": path_text(&plan_snapshot_target),
        "prompt_manifest_path": path_text(&prompt_manifest_path),
        "dispatch_record_paths": dispatch_record_paths,
        "pr_groups": pr_groups,
        "synced_issue_rows": synced_rows,
        "inferred_grouping_defaults": inferred_defaults_note,
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
    // Task 1.2: inherit grouping defaults from plan metadata when the
    // operator passed no explicit flags.
    let mut grouping = args.grouping.clone();
    if let Some(inferred) =
        infer_grouping_defaults_from_plan(&args.plan, i32::from(args.sprint), &grouping)
    {
        apply_inferred_grouping_defaults(&mut grouping, &inferred);
    }

    let options = to_build_options(
        args.prefixes.owner_prefix.clone(),
        args.prefixes.branch_prefix.clone(),
        args.prefixes.worktree_prefix.clone(),
        grouping.pr_grouping,
        grouping.default_pr_grouping,
        grouping.strategy,
        grouping.pr_group.clone(),
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

    // Adapter is constructed lazily inside the live `BinaryFlavor::PlanIssue`
    // branch below, where the resolved provider is known. body-file mode
    // doesn't shell out to a provider so it never reaches the adapter.
    let mut issue_body_for_comment: Option<String> = None;
    let mut live_mutations = false;

    if binary == BinaryFlavor::PlanIssue {
        let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
        let adapter = crate::provider::select_adapter(&repo_info, force);
        let repo = repo_info.slug;
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
        strategy: grouping.strategy,
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
        let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
        let adapter = crate::provider::select_adapter(&repo_info, force);
        let repo = repo_info.slug;
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
            "--approved-comment-url must be a GitHub issue/pull or GitLab issue/MR note comment URL",
        ));
    }

    // Task 1.2: inherit grouping defaults from plan metadata when the
    // operator passed no explicit flags.
    let mut grouping = args.grouping.clone();
    if let Some(inferred) =
        infer_grouping_defaults_from_plan(&args.plan, i32::from(args.sprint), &grouping)
    {
        apply_inferred_grouping_defaults(&mut grouping, &inferred);
    }

    let options = to_build_options(
        args.prefixes.owner_prefix.clone(),
        args.prefixes.branch_prefix.clone(),
        args.prefixes.worktree_prefix.clone(),
        grouping.pr_grouping,
        grouping.default_pr_grouping,
        grouping.strategy,
        grouping.pr_group.clone(),
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

    // Adapter is constructed lazily inside the live `BinaryFlavor::PlanIssue`
    // branch below, where the resolved provider is known. body-file mode
    // doesn't shell out to a provider so it never reaches the adapter.
    let mut issue_body_for_comment: Option<String> = None;
    let mut synced_done_rows = 0usize;
    let mut live_mutations = false;

    if binary == BinaryFlavor::PlanIssue {
        let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
        let adapter = crate::provider::select_adapter(&repo_info, force);
        let repo = repo_info.slug;
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
        ensure_prs_merged(adapter.as_ref(), &repo, &required_prs, "accept-sprint")
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
        strategy: grouping.strategy,
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
        let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
        let adapter = crate::provider::select_adapter(&repo_info, force);
        let repo = repo_info.slug;
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

    let issue_body_path = match crate::provider::resolve_repo(None) {
        Ok(info) => {
            let slug = runtime_layout::repo_slug(&info.slug);
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

// ----------------------------------------------------------------------------
// Task 1.5: resolve-approval
// ----------------------------------------------------------------------------

/// One review-evidence comment whose body matched `Decision: merge`.
#[derive(Debug, Clone)]
struct ApprovalCandidate {
    html_url: String,
    created_at: Option<String>,
    body_excerpt: String,
}

#[derive(Debug, Clone)]
struct ResolveApprovalOutcome {
    repo: String,
    pr: u64,
    candidates: Vec<ApprovalCandidate>,
}

impl ResolveApprovalOutcome {
    fn latest_url(&self) -> Option<&str> {
        self.candidates.first().map(|c| c.html_url.as_str())
    }
}

fn collect_approval_candidates(
    binary: BinaryFlavor,
    force: bool,
    repo_override: Option<&str>,
    pr: u64,
) -> Result<ResolveApprovalOutcome, CommandError> {
    ensure_live_binary_for_command(
        binary,
        "resolve-approval --pr <number>",
        Some("plan-issue resolve-approval --pr <number> --repo <owner/repo>"),
    )?;
    let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
    let adapter = crate::provider::select_adapter(&repo_info, force);
    let repo = repo_info.slug;

    let comments = adapter
        .pr_comments(&repo, pr)
        .map_err(|err| CommandError::runtime("github-pr-comments-failed", err))?;

    let mut matched: Vec<ApprovalCandidate> = comments
        .into_iter()
        .filter_map(|comment| {
            let body = comment.get("body").and_then(Value::as_str)?;
            // Match canonical "Decision: merge" line; allow surrounding
            // whitespace/punctuation (Markdown bullet, bold, etc.) so the
            // orchestrator's actual review-evidence comment shape is
            // accepted verbatim.
            if !body.lines().any(|line| line.contains("Decision: merge")) {
                return None;
            }
            let html_url = comment.get("html_url").and_then(Value::as_str)?.to_string();
            let created_at = comment
                .get("created_at")
                .and_then(Value::as_str)
                .map(str::to_string);
            let body_excerpt = body
                .lines()
                .find(|line| line.contains("Decision: merge"))
                .unwrap_or("Decision: merge")
                .chars()
                .take(120)
                .collect::<String>();
            Some(ApprovalCandidate {
                html_url,
                created_at,
                body_excerpt,
            })
        })
        .collect();

    // Latest first by `created_at` (lexicographic on ISO-8601 is correct).
    matched.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    Ok(ResolveApprovalOutcome {
        repo,
        pr,
        candidates: matched,
    })
}

fn run_resolve_approval_json(
    binary: BinaryFlavor,
    force: bool,
    repo_override: Option<&str>,
    args: &ResolveApprovalArgs,
) -> Result<Value, CommandError> {
    let outcome = collect_approval_candidates(binary, force, repo_override, args.pr)?;
    let candidates: Vec<Value> = outcome
        .candidates
        .iter()
        .map(|c| {
            json!({
                "html_url": c.html_url,
                "created_at": c.created_at,
                "body_excerpt": c.body_excerpt,
            })
        })
        .collect();
    Ok(json!({
        "repo": outcome.repo,
        "pr": outcome.pr,
        "count": candidates.len(),
        "url": outcome.latest_url(),
        "candidates": candidates,
    }))
}

/// Text-mode driver for `resolve-approval` — returns the process exit
/// code so the binary can short-circuit the standard text envelope. On
/// exactly one match prints the URL on stdout; otherwise prints a clear
/// stderr message naming the count and exits non-zero.
pub fn run_resolve_approval_text(
    binary: BinaryFlavor,
    repo_override: Option<&str>,
    args: &ResolveApprovalArgs,
) -> i32 {
    // Force flag is irrelevant for read-only `gh api` calls; pass `false`.
    let outcome = match collect_approval_candidates(binary, false, repo_override, args.pr) {
        Ok(outcome) => outcome,
        Err(err) => {
            eprintln!(
                "error[{code}]: {message}",
                code = err.code,
                message = err.message
            );
            return err.exit_code;
        }
    };

    match outcome.candidates.len() {
        0 => {
            eprintln!(
                "no merge-decision review-evidence comment found on PR #{pr} of {repo}",
                pr = outcome.pr,
                repo = outcome.repo
            );
            crate::EXIT_FAILURE
        }
        1 => {
            // SAFETY: count == 1.
            println!("{}", outcome.latest_url().expect("single candidate"));
            crate::EXIT_SUCCESS
        }
        many => {
            eprintln!(
                "found {many} merge-decision review-evidence comments on PR #{pr} of {repo}; pass --format json to inspect candidates and choose explicitly",
                pr = outcome.pr,
                repo = outcome.repo
            );
            crate::EXIT_FAILURE
        }
    }
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

/// Inferred grouping defaults derived from a plan sprint's `pr-grouping`
/// metadata (Task 1.2). Returned only when the operator did NOT pass any of
/// `--strategy` / `--pr-grouping` / `--default-pr-grouping` and the sprint
/// declared an intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InferredGroupingDefaults {
    pub strategy: crate::commands::SplitStrategy,
    pub default_pr_grouping: crate::commands::PrGrouping,
    pub source_sprint: i32,
}

/// Resolve plan-derived defaults for `--strategy` and `--default-pr-grouping`.
///
/// Behaviour (Task 1.2):
///
/// * Returns `Ok(None)` when the operator passed any explicit grouping flag
///   (so CLI flags always win).
/// * Returns `Ok(None)` when the plan parses cleanly but the named sprint
///   has no `pr-grouping` metadata.
/// * Returns `Ok(Some(_))` when the plan declares the sprint's intent and
///   no CLI flag overrode it; the caller mutates `grouping` accordingly
///   and prints the hint to stderr.
/// * Returns `Ok(None)` when the plan cannot be parsed; the downstream
///   `task_spec::build_task_spec` call surfaces the same parse error with
///   a richer message.
pub(crate) fn infer_grouping_defaults_from_plan(
    plan_path: &Path,
    sprint: i32,
    grouping: &crate::commands::GroupingArgs,
) -> Option<InferredGroupingDefaults> {
    // Operator already pinned at least one grouping decision — never
    // override.
    if grouping.pr_grouping.is_some()
        || grouping.default_pr_grouping.is_some()
        || grouping.strategy != crate::commands::SplitStrategy::Deterministic
        || !grouping.pr_group.is_empty()
    {
        return None;
    }

    let resolved = task_spec::resolve_plan_file(plan_path);
    let display = plan_path.to_string_lossy().to_string();
    let (plan, parse_errors) = parse_plan_with_display(&resolved, &display).ok()?;
    if !parse_errors.is_empty() {
        return None;
    }

    let target = plan.sprints.iter().find(|s| s.number == sprint)?;
    let intent = target.metadata.pr_grouping_intent.as_deref()?;
    let pr_grouping = match intent {
        "per-sprint" => crate::commands::PrGrouping::PerSprint,
        "group" => crate::commands::PrGrouping::Group,
        _ => return None,
    };

    Some(InferredGroupingDefaults {
        strategy: crate::commands::SplitStrategy::Auto,
        default_pr_grouping: pr_grouping,
        source_sprint: sprint,
    })
}

/// Apply inferred defaults to a mutable `GroupingArgs` clone and emit the
/// stderr hint required by Task 1.2.
pub(crate) fn apply_inferred_grouping_defaults(
    grouping: &mut crate::commands::GroupingArgs,
    inferred: &InferredGroupingDefaults,
) {
    grouping.strategy = inferred.strategy;
    grouping.default_pr_grouping = Some(inferred.default_pr_grouping);
    let strategy_text = match inferred.strategy {
        crate::commands::SplitStrategy::Auto => "auto",
        crate::commands::SplitStrategy::Deterministic => "deterministic",
    };
    let grouping_text = match inferred.default_pr_grouping {
        crate::commands::PrGrouping::PerSprint => "per-sprint",
        crate::commands::PrGrouping::Group => "group",
    };
    eprintln!(
        "inferred --strategy={strategy_text}; --default-pr-grouping={grouping_text} from plan sprint S{}",
        inferred.source_sprint
    );
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

fn read_text_file(path: &Path, code: &'static str) -> Result<String, CommandError> {
    fs::read_to_string(path).map_err(|err| {
        CommandError::runtime(code, format!("failed to read {}: {err}", path.display()))
    })
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
    if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        let Some((base, suffix)) = rest.split_once("#issuecomment-") else {
            return false;
        };
        if !suffix.chars().all(|ch| ch.is_ascii_digit()) {
            return false;
        }
        return base.contains("/issues/") || base.contains("/pull/");
    }
    if let Some(rest) = trimmed.strip_prefix("https://") {
        let Some((host, path)) = rest.split_once('/') else {
            return false;
        };
        if !(host == "gitlab.com" || host.starts_with("gitlab.") || host.contains(".gitlab.")) {
            return false;
        }
        let Some((base, suffix)) = path.split_once("#note_") else {
            return false;
        };
        if !suffix.chars().all(|ch| ch.is_ascii_digit()) {
            return false;
        }
        return base.contains("/-/issues/")
            || base.contains("/-/merge_requests/")
            || base.contains("/-/work_items/");
    }
    false
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
    // Sprint 2.2: stop early-rejecting GitLab. Dispatchers that still default
    // to `GhCliAdapter` would now silently shell out to `gh` against a GitLab
    // slug and fail with gh's error — that path is acceptable as a transitional
    // failure mode because Sprint 3 / Sprint 4 will refactor those dispatchers
    // to use [`crate::provider::select_adapter`]. The `record open` dispatcher
    // already routes through `resolve_repo_info_for_live` + `select_adapter`,
    // so it lands the GitLab path correctly.
    Ok(resolve_repo_info_for_live(binary, repo_override)?.slug)
}

/// Provider-aware variant of `resolve_repo_for_live`. Used by dispatchers
/// that route through [`crate::provider::select_adapter`] to pick the right
/// `ProviderAdapter` implementation per provider.
fn resolve_repo_info_for_live(
    binary: BinaryFlavor,
    repo_override: Option<&str>,
) -> Result<crate::provider::Repo, CommandError> {
    ensure_live_binary(binary)?;
    crate::provider::resolve_repo(repo_override)
        .map_err(|err| CommandError::usage("repo-resolution-failed", err))
}

/// Pick the right [`crate::provider::ProviderAdapter`] for an already-resolved
/// repo slug. Sprint 4 dispatchers (start-plan, status-plan, ready-plan,
/// close-plan, cleanup-worktrees, start-sprint, ready-sprint, accept-sprint)
/// construct the adapter near the top of the function before they know
/// whether they will hit a live path or a body-file rehearsal path; this
/// helper lets them swap in the GitLab adapter when `repo` resolves to a
/// GitLab slug without restructuring the dispatcher's control flow.
fn select_adapter_for_slug(
    repo_slug: &str,
    force: bool,
) -> Result<Box<dyn crate::provider::ProviderAdapter>, CommandError> {
    let info = crate::provider::resolve_repo(Some(repo_slug))
        .map_err(|err| CommandError::usage("repo-resolution-failed", err))?;
    Ok(crate::provider::select_adapter(&info, force))
}

fn render_subagent_prompt(view: &SubagentPromptView<'_>) -> String {
    let mut engine = Engine::builder().build();
    engine
        .register_template(SUBAGENT_PROMPT_TEMPLATE_NAME, SUBAGENT_PROMPT_TEMPLATE)
        .expect("subagent_prompt template registers");
    engine
        .render(SUBAGENT_PROMPT_TEMPLATE_NAME, view)
        .expect("subagent_prompt template renders")
}

fn render_plan_status_comment(rows: &[TaskRow]) -> String {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for row in rows {
        let status = row.status.trim().to_ascii_lowercase();
        *counts.entry(status).or_insert(0) += 1;
    }

    let view = PlanStatusCommentView {
        total: rows.len(),
        planned: counts.get("planned").copied().unwrap_or(0),
        in_progress: counts.get("in-progress").copied().unwrap_or(0),
        blocked: counts.get("blocked").copied().unwrap_or(0),
        done: counts.get("done").copied().unwrap_or(0),
    };
    let mut engine = Engine::builder().build();
    engine
        .register_template(
            PLAN_STATUS_COMMENT_TEMPLATE_NAME,
            PLAN_STATUS_COMMENT_TEMPLATE,
        )
        .expect("plan_status_comment template registers");
    engine
        .render(PLAN_STATUS_COMMENT_TEMPLATE_NAME, &view)
        .expect("plan_status_comment template renders")
}

fn should_emit_comment(comment_mode: &crate::commands::CommentModeArgs) -> bool {
    !comment_mode.no_comment
}

fn write_temp_markdown(stem: &str, content: &str) -> Result<PathBuf, String> {
    let dir = task_spec::state_dir()
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
            let body = render_subagent_prompt(&SubagentPromptView {
                issue,
                sprint,
                task: &row.task_id,
                anchor_task: &anchor_task,
                task_list: &task_list,
                task_summary: &task_summary,
                owner: &lane.owner,
                branch: &lane.branch,
                worktree: &lane.worktree,
                execution_mode: &lane.execution_mode,
                notes: &lane.notes,
                lane_tasks: &lane_tasks,
            });
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
    adapter: &dyn ProviderAdapter,
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
    adapter: &dyn ProviderAdapter,
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
    struct MockProviderAdapter {
        merged: HashMap<u64, Result<bool, String>>,
    }

    impl MockProviderAdapter {
        fn with_merge(mut self, pr: u64, result: Result<bool, String>) -> Self {
            self.merged.insert(pr, result);
            self
        }
    }

    impl ProviderAdapter for MockProviderAdapter {
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

        fn comment_issue(
            &self,
            _repo: &str,
            _issue: u64,
            _body_file: &Path,
        ) -> Result<String, String> {
            unreachable!("comment_issue is not needed in this test")
        }

        fn issue_evidence(&self, _repo: &str, _issue: u64) -> Result<(String, String), String> {
            unreachable!("issue_evidence is not needed in this test")
        }

        fn list_open_tracker_issues(
            &self,
            _repo: &str,
            _labels: &[String],
        ) -> Result<Vec<u64>, String> {
            unreachable!("list_open_tracker_issues is not needed in this test")
        }

        fn pr_merge_summary(
            &self,
            _repo: &str,
            _pr: u64,
        ) -> Result<crate::github::PrMergeSummary, String> {
            unreachable!("pr_merge_summary is not needed in this test")
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

        fn pr_comments(&self, _repo: &str, _pr: u64) -> Result<Vec<Value>, String> {
            unreachable!("pr_comments is not needed in this test")
        }
    }

    /// Stub adapter that creates issue #1, fails the first comment post, and
    /// records the `close_issue` call so the `record open` rollback can be
    /// asserted without the live `plan-issue` binary.
    struct RollbackProbeAdapter {
        close_ok: bool,
        closed_issue: AtomicU64,
    }

    impl RollbackProbeAdapter {
        fn new(close_ok: bool) -> Self {
            Self {
                close_ok,
                closed_issue: AtomicU64::new(0),
            }
        }
    }

    impl ProviderAdapter for RollbackProbeAdapter {
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
            Ok((1, "https://github.com/owner/repo/issues/1".to_string()))
        }

        fn edit_issue_body(
            &self,
            _repo: &str,
            _issue: u64,
            _body_file: &Path,
        ) -> Result<(), String> {
            unreachable!("edit_issue_body is not needed in this test")
        }

        fn comment_issue(
            &self,
            _repo: &str,
            _issue: u64,
            _body_file: &Path,
        ) -> Result<String, String> {
            Err("simulated comment post failure".to_string())
        }

        fn issue_evidence(&self, _repo: &str, _issue: u64) -> Result<(String, String), String> {
            unreachable!("issue_evidence is not needed in this test")
        }

        fn list_open_tracker_issues(
            &self,
            _repo: &str,
            _labels: &[String],
        ) -> Result<Vec<u64>, String> {
            unreachable!("list_open_tracker_issues is not needed in this test")
        }

        fn pr_merge_summary(
            &self,
            _repo: &str,
            _pr: u64,
        ) -> Result<crate::github::PrMergeSummary, String> {
            unreachable!("pr_merge_summary is not needed in this test")
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
            issue: u64,
            _reason: CloseReason,
            close_comment: Option<&str>,
        ) -> Result<(), String> {
            // Rollback must close without a comment so it still succeeds when the
            // original failure was a broken comment post.
            assert!(
                close_comment.is_none(),
                "rollback close must not post a comment"
            );
            self.closed_issue.store(issue, Ordering::SeqCst);
            if self.close_ok {
                Ok(())
            } else {
                Err("simulated close failure".to_string())
            }
        }

        fn pr_is_merged(&self, _repo: &str, _pr: u64) -> Result<bool, String> {
            unreachable!("pr_is_merged is not needed in this test")
        }

        fn pr_comments(&self, _repo: &str, _pr: u64) -> Result<Vec<Value>, String> {
            unreachable!("pr_comments is not needed in this test")
        }
    }

    fn rollback_probe_seed() -> RecordSeed {
        RecordSeed {
            plan_title: "Rollback Probe Plan".to_string(),
            source_path: "docs/plans/probe/probe-discussion-source.md".to_string(),
            plan_path: "docs/plans/probe/probe-plan.md".to_string(),
            source_commit: "abc".to_string(),
            plan_commit: "def".to_string(),
            source_body: "source body".to_string(),
            plan_body: "plan body".to_string(),
            state_body: "state body".to_string(),
        }
    }

    #[test]
    fn record_open_finalize_rolls_back_orphaned_issue_on_post_failure() {
        let adapter = RollbackProbeAdapter::new(true);
        let seed = rollback_probe_seed();
        let err = record_open_finalize(
            &adapter,
            "owner/repo",
            crate::commands::record::RecordProfile::Tracking,
            Path::new("/tmp/record-open-rollback-body.md"),
            &[],
            &seed,
            "binary",
        )
        .expect_err("a failed comment post must surface an error");

        // The original failure code is preserved.
        assert_eq!(err.code, "record-open-source-post-failed");
        // The orphaned issue was closed and the message says so.
        assert_eq!(adapter.closed_issue.load(Ordering::SeqCst), 1);
        assert!(
            err.message.contains("rolled back") && err.message.contains("#1"),
            "expected a rollback note naming the issue, got: {}",
            err.message
        );
    }

    #[test]
    fn record_open_finalize_reports_when_rollback_close_fails() {
        let adapter = RollbackProbeAdapter::new(false);
        let seed = rollback_probe_seed();
        let err = record_open_finalize(
            &adapter,
            "owner/repo",
            crate::commands::record::RecordProfile::Tracking,
            Path::new("/tmp/record-open-rollback-body.md"),
            &[],
            &seed,
            "binary",
        )
        .expect_err("a failed comment post must surface an error");

        assert_eq!(adapter.closed_issue.load(Ordering::SeqCst), 1);
        assert!(
            err.message.contains("rollback FAILED") && err.message.contains("#1"),
            "expected a rollback-failed note, got: {}",
            err.message
        );
    }

    /// Stateful stub adapter for the `record open` auto-detect / resume tests.
    /// Records comment / edit / close calls and serves scripted issue evidence
    /// so detection and attach-missing are exercisable without the live binary.
    #[derive(Default)]
    struct ResumeFakeAdapter {
        open_issues: Vec<u64>,
        evidence: HashMap<u64, (String, String)>,
        create_number: u64,
        /// When set, `comment_issue` fails on the Nth call (1-based). Used to
        /// simulate a post failure at a precise point without depending on the
        /// (shared, race-prone) temp markdown file's contents.
        fail_on_nth_comment: Option<usize>,
        /// Issue numbers for each successful comment post, in call order.
        comment_calls: std::sync::Mutex<Vec<u64>>,
        edited: std::sync::Mutex<Vec<u64>>,
        closed: std::sync::Mutex<Vec<u64>>,
    }

    impl ProviderAdapter for ResumeFakeAdapter {
        fn issue_body(&self, _repo: &str, issue: u64) -> Result<String, String> {
            self.evidence
                .get(&issue)
                .map(|(body, _)| body.clone())
                .ok_or_else(|| format!("no scripted evidence for issue {issue}"))
        }

        fn issue_evidence(&self, _repo: &str, issue: u64) -> Result<(String, String), String> {
            self.evidence
                .get(&issue)
                .cloned()
                .ok_or_else(|| format!("no scripted evidence for issue {issue}"))
        }

        fn list_open_tracker_issues(
            &self,
            _repo: &str,
            _labels: &[String],
        ) -> Result<Vec<u64>, String> {
            Ok(self.open_issues.clone())
        }

        fn create_issue(
            &self,
            _repo: &str,
            _title: &str,
            _body_file: &Path,
            _labels: &[String],
        ) -> Result<(u64, String), String> {
            Ok((
                self.create_number,
                format!(
                    "https://github.com/owner/repo/issues/{}",
                    self.create_number
                ),
            ))
        }

        fn edit_issue_body(
            &self,
            _repo: &str,
            issue: u64,
            _body_file: &Path,
        ) -> Result<(), String> {
            self.edited.lock().unwrap().push(issue);
            Ok(())
        }

        fn comment_issue(
            &self,
            _repo: &str,
            issue: u64,
            _body_file: &Path,
        ) -> Result<String, String> {
            let mut calls = self.comment_calls.lock().unwrap();
            let nth = calls.len() + 1;
            if self.fail_on_nth_comment == Some(nth) {
                return Err(format!("simulated comment post failure on call {nth}"));
            }
            calls.push(issue);
            Ok(format!(
                "https://github.com/owner/repo/issues/{issue}#issuecomment-{nth}"
            ))
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
            issue: u64,
            _reason: CloseReason,
            _close_comment: Option<&str>,
        ) -> Result<(), String> {
            self.closed.lock().unwrap().push(issue);
            Ok(())
        }

        fn pr_is_merged(&self, _repo: &str, _pr: u64) -> Result<bool, String> {
            unreachable!("pr_is_merged is not needed in this test")
        }

        fn pr_merge_summary(
            &self,
            _repo: &str,
            _pr: u64,
        ) -> Result<crate::github::PrMergeSummary, String> {
            unreachable!("pr_merge_summary is not needed in this test")
        }

        fn pr_comments(&self, _repo: &str, _pr: u64) -> Result<Vec<Value>, String> {
            unreachable!("pr_comments is not needed in this test")
        }
    }

    /// Render a real source lifecycle comment carrying the snapshot identity
    /// (`path` + `commit`) so `audit_record` parses it exactly as in live mode.
    fn source_identity_comment(path: &str, commit: &str) -> String {
        lifecycle_record::render_record_snapshot_comment(
            crate::commands::record::RecordProfile::Tracking,
            crate::commands::record::LifecycleCommentKind::Source,
            &lifecycle_record::SnapshotData {
                path: path.to_string(),
                commit: commit.to_string(),
                title: None,
                summary: None,
            },
            "source content",
            None,
        )
        .expect("render source comment")
    }

    /// Minimal v2 marker comment (no payload) — enough for `audit_record` to
    /// record the role as present when only presence matters.
    fn v2_marker_comment(role: &str) -> String {
        format!("<!-- plan-issue-record:v2 role={role} profile=tracking -->\n")
    }

    fn comments_envelope(bodies: &[&str]) -> String {
        let items: Vec<Value> = bodies
            .iter()
            .enumerate()
            .map(|(idx, body)| {
                json!({
                    "body": body,
                    "url": format!("https://github.com/owner/repo/issues/1#c{idx}"),
                    "created_at": format!("2026-01-{:02}T00:00:00Z", idx + 1),
                })
            })
            .collect();
        serde_json::to_string(&json!({ "comments": items })).unwrap()
    }

    fn resume_seed() -> RecordSeed {
        RecordSeed {
            plan_title: "Resume Plan".to_string(),
            source_path: "docs/plans/x/x-discussion-source.md".to_string(),
            plan_path: "docs/plans/x/x-plan.md".to_string(),
            source_commit: "abc123".to_string(),
            plan_commit: "abc123".to_string(),
            source_body: "SRC-BODY".to_string(),
            plan_body: "PLN-BODY".to_string(),
            state_body: "STA-BODY".to_string(),
        }
    }

    #[test]
    fn detect_resumable_tracker_matches_source_snapshot_identity() {
        let other = source_identity_comment("docs/plans/y/y-discussion-source.md", "zzz999");
        let wanted = source_identity_comment("docs/plans/x/x-discussion-source.md", "abc123");
        let mut evidence = HashMap::new();
        evidence.insert(2, ("body-2".to_string(), comments_envelope(&[&other])));
        evidence.insert(7, ("body-7".to_string(), comments_envelope(&[&wanted])));
        let adapter = ResumeFakeAdapter {
            open_issues: vec![2, 7],
            evidence,
            ..Default::default()
        };
        let identity = BundleIdentity {
            source_path: "docs/plans/x/x-discussion-source.md".to_string(),
            source_commit: "abc123".to_string(),
        };

        let found = detect_resumable_tracker(
            &adapter,
            "owner/repo",
            &[],
            crate::commands::record::RecordProfile::Tracking,
            &identity,
        )
        .expect("detection must not error");
        let (number, audit) = found.expect("a matching tracker must be found");
        assert_eq!(number, 7);
        // Only the source role is present on the matched tracker.
        assert!(audit.missing_required.iter().any(|c| c == "plan-missing"));
        assert!(audit.missing_required.iter().any(|c| c == "state-missing"));
    }

    #[test]
    fn detect_resumable_tracker_returns_none_when_no_identity_matches() {
        let other = source_identity_comment("docs/plans/y/y-discussion-source.md", "zzz999");
        let mut evidence = HashMap::new();
        evidence.insert(2, ("body-2".to_string(), comments_envelope(&[&other])));
        let adapter = ResumeFakeAdapter {
            open_issues: vec![2],
            evidence,
            ..Default::default()
        };
        let identity = BundleIdentity {
            source_path: "docs/plans/x/x-discussion-source.md".to_string(),
            source_commit: "abc123".to_string(),
        };

        let found = detect_resumable_tracker(
            &adapter,
            "owner/repo",
            &[],
            crate::commands::record::RecordProfile::Tracking,
            &identity,
        )
        .expect("detection must not error");
        assert!(found.is_none(), "no bundle identity matches, expected None");
    }

    #[test]
    fn record_open_resume_attaches_only_missing_roles() {
        let wanted = source_identity_comment("docs/plans/x/x-discussion-source.md", "abc123");
        let comments_json = comments_envelope(&[&wanted]);
        let audit = lifecycle_record::audit_record(
            Some("body-7"),
            &comments_json,
            Some(crate::commands::record::RecordProfile::Tracking),
        )
        .expect("audit");
        // Sanity: source present, plan + state missing.
        assert!(audit.evidence.contains_key("source"));
        assert!(audit.missing_required.iter().any(|c| c == "plan-missing"));

        let mut evidence = HashMap::new();
        evidence.insert(7, ("body-7".to_string(), comments_json));
        let adapter = ResumeFakeAdapter {
            evidence,
            ..Default::default()
        };
        let seed = resume_seed();

        let result = record_open_resume(
            &adapter,
            "owner/repo",
            crate::commands::record::RecordProfile::Tracking,
            &seed,
            7,
            "https://github.com/owner/repo/issues/7",
            &audit,
            "binary",
        )
        .expect("resume must succeed");

        assert_eq!(result["mode"], "resumed");
        // The result's `attached` carries the role identity (source is skipped,
        // only plan + state are posted); the call log confirms exactly two posts
        // to issue 7.
        assert_eq!(result["attached"], json!(["plan", "state"]));
        assert_eq!(*adapter.comment_calls.lock().unwrap(), vec![7, 7]);
        assert_eq!(*adapter.edited.lock().unwrap(), vec![7]);
        assert!(adapter.closed.lock().unwrap().is_empty());
    }

    #[test]
    fn record_open_resume_is_noop_when_all_roles_present() {
        let source = source_identity_comment("docs/plans/x/x-discussion-source.md", "abc123");
        let plan = v2_marker_comment("plan");
        let state = v2_marker_comment("state");
        let comments_json = comments_envelope(&[&source, &plan, &state]);
        let audit = lifecycle_record::audit_record(
            Some("body-7"),
            &comments_json,
            Some(crate::commands::record::RecordProfile::Tracking),
        )
        .expect("audit");
        assert!(
            audit.missing_required.is_empty(),
            "fixture must have all required roles present, got {:?}",
            audit.missing_required
        );

        let adapter = ResumeFakeAdapter::default();
        let seed = resume_seed();

        let result = record_open_resume(
            &adapter,
            "owner/repo",
            crate::commands::record::RecordProfile::Tracking,
            &seed,
            7,
            "https://github.com/owner/repo/issues/7",
            &audit,
            "binary",
        )
        .expect("resume must succeed");

        assert_eq!(result["mode"], "already-open");
        assert!(adapter.comment_calls.lock().unwrap().is_empty());
        assert!(adapter.edited.lock().unwrap().is_empty());
    }

    #[test]
    fn record_open_finalize_leaves_partial_tracker_after_source_posts() {
        let adapter = ResumeFakeAdapter {
            create_number: 1,
            // Fail the 2nd comment post (plan), i.e. after the source comment.
            fail_on_nth_comment: Some(2),
            ..Default::default()
        };
        let seed = resume_seed();

        let err = record_open_finalize(
            &adapter,
            "owner/repo",
            crate::commands::record::RecordProfile::Tracking,
            Path::new("/tmp/record-open-partial-body.md"),
            &[],
            &seed,
            "binary",
        )
        .expect_err("a failed plan post must surface an error");

        assert_eq!(err.code, "record-open-plan-post-failed");
        assert!(
            err.message.contains("partial tracker left open") && err.message.contains("#1"),
            "expected a leave-partial note, got: {}",
            err.message
        );
        // The source comment posted, so the tracker is identifiable and must NOT
        // be rolled back — the next run resumes it.
        assert!(
            adapter.closed.lock().unwrap().is_empty(),
            "must not roll back once the source comment has posted"
        );
        // Exactly one successful post (source) happened before the plan failure.
        assert_eq!(*adapter.comment_calls.lock().unwrap(), vec![1]);
    }

    #[test]
    fn helper_url_validation_and_comment_mode_are_stable() {
        assert!(approval_comment_url_looks_valid(
            "https://github.com/sympoies/nils-cli/issues/217#issuecomment-123"
        ));
        assert!(approval_comment_url_looks_valid(
            "https://github.com/sympoies/nils-cli/pull/221#issuecomment-456"
        ));
        assert!(approval_comment_url_looks_valid(
            "https://gitlab.com/group/project/-/issues/12#note_789"
        ));
        assert!(approval_comment_url_looks_valid(
            "https://gitlab.com/terrylin/agent-runtime-testing/-/merge_requests/7#note_321"
        ));
        assert!(approval_comment_url_looks_valid(
            "https://gitlab.com/terrylin/agent-runtime-testing/-/work_items/13#note_654"
        ));
        assert!(!approval_comment_url_looks_valid(
            "https://example.com/issues/217#issuecomment-123"
        ));
        assert!(!approval_comment_url_looks_valid(
            "https://github.com/sympoies/nils-cli/issues/217#comment-123"
        ));
        assert!(!approval_comment_url_looks_valid(
            "https://gitlab.com/terrylin/agent-runtime-testing/-/merge_requests/7#note_abc"
        ));
        assert!(!approval_comment_url_looks_valid(
            "https://gitlab.com/terrylin/agent-runtime-testing/-/merge_requests/7"
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
    fn state_checkpoint_payload_carries_full_accumulative_ledger() {
        use crate::tracking::run_state::{ExecutionRun, RunPhase};

        let tmp = TempDir::new().expect("tempdir");
        let ledger = tmp.path().join("slug-execution-state.md");
        std::fs::write(
            &ledger,
            concat!(
                "# Execution State\n\n",
                "## Task Ledger\n\n",
                "| ID | Status | Task | Evidence | Notes |\n",
                "| --- | --- | --- | --- | --- |\n",
                "| 1.1 | done | Append line A | log | first |\n",
                "| 1.2 | in-progress | Append line B | — | second |\n",
                "| 1.3 | blocked | Append line C | — | third |\n",
                "| 1.4 | waived | Skip D | — | fourth |\n",
            ),
        )
        .expect("write ledger");

        let mut run = ExecutionRun::new(
            "run-1",
            "owner/repo",
            1,
            "tracking",
            RunPhase::Implementing,
            "2026-05-29T00:00:00Z",
        );
        run.execution_state_file = Some(ledger.clone());

        let payload = state_checkpoint_payload(&run);
        let tasks = payload["tasks"].as_array().expect("tasks array");
        assert_eq!(tasks.len(), 4, "hidden payload must carry the full ledger");
        let ids: Vec<&str> = tasks.iter().map(|t| t["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["1.1", "1.2", "1.3", "1.4"]);
        let statuses: Vec<&str> = tasks
            .iter()
            .map(|t| t["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, ["done", "in-progress", "blocked", "waived"]);
        assert_eq!(tasks[0]["title"].as_str().unwrap(), "Append line A");

        // The accumulative payload must round-trip through the typed StateData
        // reader (record audit path), exercising the blocked/waived variants.
        let state = serde_json::from_value::<crate::lifecycle_record::StateData>(payload.clone())
            .expect("accumulative payload deserializes into StateData");
        assert_eq!(state.tasks.len(), 4);

        // Fallback: no execution-state ledger -> single-current baseline.
        let mut bare = ExecutionRun::new(
            "run-2",
            "owner/repo",
            1,
            "tracking",
            RunPhase::Implementing,
            "2026-05-29T00:00:00Z",
        );
        bare.execution_state_file = None;
        let baseline = state_checkpoint_payload(&bare);
        assert_eq!(
            baseline["tasks"].as_array().expect("tasks array").len(),
            1,
            "without a ledger the payload stays single-current"
        );
    }

    #[test]
    fn state_checkpoint_payload_accumulates_linked_prs() {
        use crate::tracking::run_state::{ExecutionRun, LinkedPr, RunPhase};

        let mut run = ExecutionRun::new(
            "run-prs",
            "owner/repo",
            1,
            "dispatch",
            RunPhase::Implementing,
            "2026-05-29T00:00:00Z",
        );
        // Two lanes each link their PR; the most-recent stays `pr`, both
        // accumulate into `linked_prs`. A repeated ref must not duplicate.
        run.set_linked_pr(LinkedPr {
            r#ref: "owner/repo#1".to_string(),
            url: None,
            status: None,
        });
        run.set_linked_pr(LinkedPr {
            r#ref: "owner/repo#2".to_string(),
            url: None,
            status: None,
        });
        run.set_linked_pr(LinkedPr {
            r#ref: "owner/repo#1".to_string(),
            url: None,
            status: None,
        });
        assert_eq!(run.pr.as_ref().unwrap().r#ref, "owner/repo#1");
        assert_eq!(run.linked_prs.len(), 2, "ref dedup keeps two lane PRs");

        let payload = state_checkpoint_payload(&run);
        let refs: Vec<&str> = payload["prs"]
            .as_array()
            .expect("prs array")
            .iter()
            .map(|p| p["ref"].as_str().unwrap())
            .collect();
        assert_eq!(
            refs,
            ["owner/repo#1", "owner/repo#2"],
            "dashboard prs[] names every lane PR in first-seen order"
        );
        assert_eq!(payload["prs"][0]["status"].as_str().unwrap(), "open");
        let state = serde_json::from_value::<crate::lifecycle_record::StateData>(payload)
            .expect("payload with prs[] deserializes into StateData");
        assert_eq!(state.prs.len(), 2);

        // No linked PR -> prs[] stays empty (no regression for the
        // ledger-only accumulative path).
        let bare = ExecutionRun::new(
            "run-bare",
            "owner/repo",
            1,
            "tracking",
            RunPhase::Implementing,
            "2026-05-29T00:00:00Z",
        );
        let bare_prs = state_checkpoint_payload(&bare)["prs"].clone();
        assert!(
            bare_prs.as_array().map(|a| a.is_empty()).unwrap_or(true),
            "no linked PR -> prs stays empty"
        );
    }

    #[test]
    fn state_checkpoint_payload_derives_progress_and_scope_from_ledger() {
        // graysurf/plan-tracking-testbed#54 (sympoies/nils-cli#700): the
        // dashboard reads `current` / `next_action` / `target_scope` from the
        // latest state payload, so the checkpoint payload must derive them from
        // the durable `## Task Ledger` + execution-state scope header, not from
        // the never-advanced `selected_scope` or the "in-progress" fallback.
        use crate::tracking::run_state::{ExecutionRun, RunPhase};

        let tmp = TempDir::new().expect("tempdir");

        // Completed plan at ready-for-close: every row terminal.
        let done = tmp.path().join("done-execution-state.md");
        std::fs::write(
            &done,
            concat!(
                "## Execution State\n\n",
                "- Status: ready-to-start\n",
                "- Target scope: two append-only commits to `notes.md`,\n",
                "  one per task, with full lifecycle evidence\n",
                "- Current task: none\n\n",
                "## Task Ledger\n\n",
                "| ID | Status | Task | Evidence | Notes |\n",
                "| --- | --- | --- | --- | --- |\n",
                "| 1.1 | done | Append line A | log | first |\n",
                "| 1.2 | done | Append line B | log | second |\n",
            ),
        )
        .expect("write done ledger");
        let mut run = ExecutionRun::new(
            "run-done",
            "owner/repo",
            1,
            "tracking",
            RunPhase::ReadyForClose,
            "2026-05-30T00:00:00Z",
        );
        run.execution_state_file = Some(done);
        let payload = state_checkpoint_payload(&run);
        assert_eq!(
            payload["current"], "complete",
            "all rows terminal -> current=complete"
        );
        assert_eq!(
            payload["next_action"], "closeout",
            "ready-for-close -> next_action=closeout"
        );
        let scope = payload["target_scope"].as_str().expect("scope");
        assert!(
            scope.starts_with("two append-only commits"),
            "target_scope must carry the authored scope, got {scope:?}"
        );
        assert!(
            !scope.eq_ignore_ascii_case("in-progress"),
            "target_scope must never be a lifecycle status word"
        );

        // Mid-flight plan: derive the in-progress row and the next pending row.
        let mid = tmp.path().join("mid-execution-state.md");
        std::fs::write(
            &mid,
            concat!(
                "## Execution State\n\n",
                "- Target scope: demo scope\n\n",
                "## Task Ledger\n\n",
                "| ID | Status | Task | Evidence | Notes |\n",
                "| --- | --- | --- | --- | --- |\n",
                "| 1.1 | done | A | log | first |\n",
                "| 1.2 | in-progress | B | — | second |\n",
                "| 1.3 | pending | C | — | third |\n",
            ),
        )
        .expect("write mid ledger");
        let mut run_mid = ExecutionRun::new(
            "run-mid",
            "owner/repo",
            1,
            "tracking",
            RunPhase::Implementing,
            "2026-05-30T00:00:00Z",
        );
        run_mid.execution_state_file = Some(mid);
        let mid_payload = state_checkpoint_payload(&run_mid);
        assert_eq!(
            mid_payload["current"], "1.2",
            "current = first non-terminal ledger row"
        );
        assert_eq!(
            mid_payload["next_action"], "1.3",
            "next_action = next non-terminal ledger row"
        );
        assert_eq!(mid_payload["target_scope"], "demo scope");
    }

    #[test]
    fn dispatch_profile_state_checkpoint_payload_is_also_accumulative() {
        // The state-payload builder is profile-agnostic: `tracking checkpoint
        // --profile dispatch --post state` shares `state_checkpoint_payload`,
        // so a dispatch run with an execution-state ledger gets the same
        // accumulative `tasks[]` as the tracking profile. This locks that
        // parity so a future profile split cannot silently regress dispatch.
        use crate::tracking::run_state::{ExecutionRun, RunPhase};

        let tmp = TempDir::new().expect("tempdir");
        let ledger = tmp.path().join("slug-execution-state.md");
        std::fs::write(
            &ledger,
            concat!(
                "## Task Ledger\n\n",
                "| ID | Status | Task | Evidence | Notes |\n",
                "| --- | --- | --- | --- | --- |\n",
                "| 1.1 | done | Lane A | log | a |\n",
                "| 1.2 | in-progress | Lane B | — | b |\n",
                "| 2.1 | pending | Lane C | — | c |\n",
            ),
        )
        .expect("write ledger");

        let mut run = ExecutionRun::new(
            "run-d",
            "owner/repo",
            7,
            "dispatch",
            RunPhase::Implementing,
            "2026-05-29T00:00:00Z",
        );
        run.execution_state_file = Some(ledger);

        let payload = state_checkpoint_payload(&run);
        let tasks = payload["tasks"].as_array().expect("tasks array");
        assert_eq!(
            tasks.len(),
            3,
            "dispatch checkpoint must carry the full accumulative ledger"
        );
        let ids: Vec<&str> = tasks.iter().map(|t| t["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["1.1", "1.2", "2.1"]);
    }

    #[test]
    fn checkpoint_marker_reflects_selected_profile() {
        use crate::commands::record::RecordProfile;
        use crate::lifecycle_record::PayloadRole;
        use crate::tracking::run_state::{ExecutionRun, RunPhase};

        let run = ExecutionRun::new(
            "run-p",
            "owner/repo",
            9,
            "dispatch",
            RunPhase::Implementing,
            "2026-05-29T00:00:00Z",
        );

        let marker = |profile| match render_checkpoint_role(PayloadRole::State, &run, profile)
            .expect("render")
        {
            CheckpointRoleResult::Rendered(body) => body,
            CheckpointRoleResult::Empty(reason) => panic!("unexpected empty: {reason}"),
        };

        let dispatch_body = marker(RecordProfile::Dispatch);
        assert!(
            dispatch_body.contains("role=state profile=dispatch"),
            "dispatch checkpoint must mark profile=dispatch, got: {dispatch_body}"
        );
        let tracking_body = marker(RecordProfile::Tracking);
        assert!(
            tracking_body.contains("role=state profile=tracking"),
            "tracking checkpoint must mark profile=tracking, got: {tracking_body}"
        );
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

        let adapter_ok = MockProviderAdapter::default().with_merge(12, Ok(true));
        ensure_prs_merged(&adapter_ok, "sympoies/nils-cli", &[12], "scope").expect("merged");

        let adapter_unmerged = MockProviderAdapter::default().with_merge(12, Ok(false));
        let unmerged = ensure_prs_merged(&adapter_unmerged, "sympoies/nils-cli", &[12], "scope")
            .expect_err("unmerged should fail");
        assert!(
            unmerged.contains("scope: PR #12 is not merged"),
            "{unmerged}"
        );

        let adapter_error =
            MockProviderAdapter::default().with_merge(12, Err("gh failure".to_string()));
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
        let adapter_ok = MockProviderAdapter::default().with_merge(11, Ok(true));
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

        let adapter_unmerged = MockProviderAdapter::default().with_merge(11, Ok(false));
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
    fn temp_markdown_and_prompt_outputs_use_state_dir_and_expected_paths() {
        let lock = GlobalStateLock::new();
        let tmp = TempDir::new().expect("tempdir");
        crate::state::set_state_dir_override(None);
        let _state_dir = EnvGuard::set(
            &lock,
            "PLAN_ISSUE_HOME",
            tmp.path().to_string_lossy().as_ref(),
        );

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

    fn execute_golden_fixture(name: &str) -> String {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("golden")
            .join("execute")
            .join(name);
        if std::env::var_os("BLESS_EXECUTE_GOLDEN").is_some() {
            return path.to_string_lossy().into_owned();
        }
        std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()))
    }

    fn assert_or_bless_execute(name: &str, actual: &str) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("golden")
            .join("execute")
            .join(name);
        if std::env::var_os("BLESS_EXECUTE_GOLDEN").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir fixture dir");
            std::fs::write(&path, actual).expect("write fixture");
            return;
        }
        let expected = execute_golden_fixture(name);
        assert_eq!(expected, actual, "golden mismatch for {name}");
    }

    #[test]
    fn plan_status_comment_matches_golden_empty() {
        let comment = render_plan_status_comment(&[]);
        assert_or_bless_execute("plan_status_comment_empty.md", &comment);
    }

    #[test]
    fn plan_status_comment_matches_golden_mixed() {
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
        assert_or_bless_execute("plan_status_comment_mixed.md", &comment);
    }

    #[test]
    fn subagent_prompt_matches_golden() {
        let view = SubagentPromptView {
            issue: 541,
            sprint: 2,
            task: "2.6",
            anchor_task: "2.6",
            task_list: "2.6",
            task_summary: "Migrate execute.rs Markdown emitters",
            owner: "subagent-alpha",
            branch: "issue/2-6",
            worktree: "issue-2-6",
            execution_mode: "pr-isolated",
            notes: "depends on Task 2.5b",
            lane_tasks: "- 2.6: Migrate execute.rs Markdown emitters",
        };
        let body = render_subagent_prompt(&view);
        assert_or_bless_execute("subagent_prompt.md", &body);
    }
}

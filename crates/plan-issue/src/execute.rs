use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use nils_common::git as common_git;
use nils_common::markdown as common_markdown;
use nils_markdown::Engine;
use plan_tooling::parse::parse_plan_with_display;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

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

use crate::adapter::ProviderAdapter;
use crate::cli::Cli;
use crate::commands::build::{BuildPlanTaskSpecArgs, BuildTaskSpecArgs};
use crate::commands::plan::{
    CleanupWorktreesArgs, ClosePlanArgs, LinkPrArgs, LinkPrStatus, ReadyPlanArgs,
    ResolveApprovalArgs, StartPlanArgs, StatusPlanArgs,
};
use crate::commands::record::{
    LifecycleCommentKind, RecordArgs, RecordAttachArgs, RecordAuditArgs, RecordCloseArgs,
    RecordCommand, RecordOpenArgs, RecordPostArgs, RecordProfile, RecordRepairDashboardArgs,
    RecordTemplateArgs, TemplateFormatArg,
};
use crate::commands::sprint::{
    AcceptSprintArgs, MultiSprintGuideArgs, ReadySprintArgs, StartSprintArgs,
};
use crate::commands::{Command as CliCommand, SplitStrategy, SummaryArgs};
use crate::dispatch_record::{self, DispatchRecord};
use crate::issue_body::{self, TaskRow};
use crate::lifecycle_record::{self, DashboardInput};
use crate::record_open_intent::{RecordOpenIntentState, RecordOpenIntentStore};
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
        CliCommand::Tracking(args) => {
            run_tracking(binary, cli.dry_run, cli.force, cli.repo.as_deref(), args)
        }
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
    if allow_dirty && path_is_dirty(path)? {
        let contents = fs::read(path).map_err(|err| {
            CommandError::runtime(
                "record-open-dirty-snapshot-read-failed",
                format!("failed to read dirty snapshot {}: {err}", path.display()),
            )
        })?;
        let digest = Sha256::digest(contents)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        return Ok(format!("dirty-sha256:{digest}"));
    }
    if sha.is_empty() {
        return Err(CommandError::runtime(
            "record-open-uncommitted",
            format!(
                "path {} has no commit in git history; commit it first or pass --allow-dirty",
                path.display()
            ),
        ));
    }
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
        path: relative_repo_path(&bundle.source_file)?,
        commit: source_commit,
        title: None,
        summary: None,
    };
    let plan_snapshot = lifecycle_record::SnapshotData {
        path: relative_repo_path(&bundle.plan_file)?,
        commit: plan_commit,
        title: None,
        summary: None,
    };
    Ok((source_snapshot, plan_snapshot))
}

fn relative_repo_path(path: &Path) -> Result<String, CommandError> {
    let repo_root = repository_root_for_confined_path(path).ok_or_else(|| {
        CommandError::runtime(
            "record-open-repo-root-failed",
            format!(
                "failed to resolve the repository root containing {}",
                path.display()
            ),
        )
    })?;
    let relative = repo_relative_identity(path, &repo_root).ok_or_else(|| {
        CommandError::runtime(
            "record-open-repo-relative-path-failed",
            format!(
                "failed to derive a repository-relative identity for {}",
                path.display()
            ),
        )
    })?;
    Ok(relative.to_string_lossy().to_string())
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

    let (source_snapshot, mut plan_snapshot) = resolve_bundle_snapshots(bundle, allow_dirty)?;
    plan_snapshot.title = Some(plan_title.clone());
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
        None,
        Some(state_summary),
        None,
        crate::commands::record::TaskLedgerDisplay::Open,
    )
    .map_err(|err| CommandError::runtime("record-open-state-render-failed", err))?;

    Ok(RecordSeed {
        plan_title,
        source_path: source_snapshot.path,
        plan_path: plan_snapshot.path,
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

#[derive(Debug, Clone)]
struct ParsedLinkedPrReference {
    slug: String,
    number: u64,
    authority: Option<(crate::provider::Provider, Option<String>)>,
}

fn invalid_linked_pr(_value: &str) -> CommandError {
    CommandError::usage(
        "record-invalid-linked-pr",
        "--linked-pr must be `owner/repo#NN`, a GitHub PR URL, or a GitLab MR URL",
    )
}

fn linked_pr_host_provider_hint(host: &str) -> Option<crate::provider::Provider> {
    let host = host
        .rsplit_once(':')
        .filter(|(_, port)| port.chars().all(|ch| ch.is_ascii_digit()))
        .map_or(host, |(name, _)| name)
        .to_ascii_lowercase();
    if host == "github.com" || host.ends_with(".github.com") || host.ends_with(".ghe.com") {
        Some(crate::provider::Provider::GitHub)
    } else if host == "gitlab.com" || host.starts_with("gitlab.") || host.contains(".gitlab.") {
        Some(crate::provider::Provider::GitLab)
    } else {
        None
    }
}

fn is_linked_pr_slug(value: &str) -> bool {
    let segments = value.split('/').collect::<Vec<_>>();
    segments.len() >= 2
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        })
}

fn parse_linked_pr_reference_full(value: &str) -> Result<ParsedLinkedPrReference, CommandError> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.contains("://") && trimmed.contains(['?', '#']) {
        return Err(invalid_linked_pr(value));
    }
    if let Some((repo, number_raw)) = trimmed.rsplit_once('#') {
        if !is_linked_pr_slug(repo) {
            return Err(invalid_linked_pr(value));
        }
        let number = number_raw
            .parse::<u64>()
            .map_err(|_| invalid_linked_pr(value))?;
        return Ok(ParsedLinkedPrReference {
            slug: repo.to_string(),
            number,
            authority: None,
        });
    }

    let (scheme, rest) = trimmed
        .split_once("://")
        .ok_or_else(|| invalid_linked_pr(value))?;
    if scheme.eq_ignore_ascii_case("local") {
        let (slug, number_raw) = rest
            .rsplit_once("/pull/")
            .ok_or_else(|| invalid_linked_pr(value))?;
        if !is_linked_pr_slug(slug) {
            return Err(invalid_linked_pr(value));
        }
        let number = number_raw
            .split(['?', '#'])
            .next()
            .and_then(|raw| raw.parse::<u64>().ok())
            .ok_or_else(|| invalid_linked_pr(value))?;
        return Ok(ParsedLinkedPrReference {
            slug: slug.to_string(),
            number,
            authority: Some((crate::provider::Provider::Local, None)),
        });
    }
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return Err(invalid_linked_pr(value));
    }
    let (host, path) = rest
        .split_once('/')
        .ok_or_else(|| invalid_linked_pr(value))?;
    if host.is_empty() || host.contains('@') {
        return Err(invalid_linked_pr(value));
    }
    let path = path.trim_end_matches('/');
    let (provider, slug, number_raw) = if let Some((slug, number)) = path.rsplit_once("/pull/") {
        (crate::provider::Provider::GitHub, slug, number)
    } else if let Some((slug, number)) = path.rsplit_once("/issues/") {
        // Reader-side compatibility for historical execution-run v1 records,
        // which accepted GitHub PR URLs in the issue-shaped form.
        (crate::provider::Provider::GitHub, slug, number)
    } else if let Some((slug, number)) = path.rsplit_once("/-/merge_requests/") {
        (crate::provider::Provider::GitLab, slug, number)
    } else {
        return Err(invalid_linked_pr(value));
    };
    if !is_linked_pr_slug(slug) {
        return Err(invalid_linked_pr(value));
    }
    if linked_pr_host_provider_hint(host).is_some_and(|hint| hint != provider) {
        return Err(CommandError::usage(
            "record-linked-pr-authority-ambiguous",
            "--linked-pr URL authority conflicts with its provider path shape; refusing cross-forge routing",
        ));
    }
    let number = number_raw
        .split(['?', '#'])
        .next()
        .and_then(|raw| raw.parse::<u64>().ok())
        .ok_or_else(|| invalid_linked_pr(value))?;
    Ok(ParsedLinkedPrReference {
        slug: slug.to_string(),
        number,
        authority: Some((
            provider,
            Some(crate::provider::canonical_provider_host(provider, host)),
        )),
    })
}

fn canonical_linked_pr_reference(value: &str) -> Result<String, CommandError> {
    let parsed = parse_linked_pr_reference_full(value)?;
    match parsed.authority {
        None => Ok(format!("{}#{}", parsed.slug, parsed.number)),
        Some((crate::provider::Provider::GitHub, Some(host))) => Ok(format!(
            "https://{host}/{}/pull/{}",
            parsed.slug, parsed.number
        )),
        Some((crate::provider::Provider::GitLab, Some(host))) => Ok(format!(
            "https://{host}/{}/-/merge_requests/{}",
            parsed.slug, parsed.number
        )),
        Some((crate::provider::Provider::Local, None)) => {
            Ok(format!("local://{}/pull/{}", parsed.slug, parsed.number))
        }
        Some(_) => Err(invalid_linked_pr(value)),
    }
}

fn canonicalize_execution_run_linked_prs(
    run: &mut crate::tracking::run_state::ExecutionRun,
) -> Result<(), CommandError> {
    for linked in run.linked_prs.iter_mut().chain(run.pr.iter_mut()) {
        linked.r#ref = canonical_linked_pr_reference(&linked.r#ref)?;
        linked.url = linked
            .url
            .as_deref()
            .and_then(|url| canonical_linked_pr_reference(url).ok());
    }
    Ok(())
}

fn parse_linked_pr_reference(value: &str) -> Result<(String, u64), CommandError> {
    let parsed = parse_linked_pr_reference_full(value)?;
    Ok((parsed.slug, parsed.number))
}

fn resolve_linked_pr_target(
    value: &str,
    tracking_repo: &crate::provider::Repo,
) -> Result<(crate::provider::Repo, u64), CommandError> {
    let parsed = parse_linked_pr_reference_full(value)?;
    if let Some((provider, host)) = &parsed.authority
        && (*provider != tracking_repo.provider
            || !crate::provider::optional_authorities_equal(
                *provider,
                host.as_deref(),
                tracking_repo.host.as_deref(),
            ))
    {
        return Err(CommandError::usage(
            "record-linked-pr-authority-mismatch",
            "--linked-pr URL authority does not match the tracking repository authority",
        ));
    }
    let repo = if let Some((provider, host)) = parsed.authority {
        crate::provider::Repo {
            provider,
            slug: parsed.slug,
            host,
        }
    } else {
        crate::provider::Repo {
            provider: tracking_repo.provider,
            slug: parsed.slug,
            host: tracking_repo.host.clone(),
        }
    };
    Ok((repo, parsed.number))
}

fn read_live_linked_pr_evidence(
    linked_prs: &[String],
    tracking_repo: &crate::provider::Repo,
    force: bool,
) -> Result<Vec<lifecycle_record::LinkedPrEvidence>, CommandError> {
    linked_prs
        .iter()
        .map(|raw| {
            let (pr_repo_info, pr_number) = resolve_linked_pr_target(raw, tracking_repo)?;
            let adapter = crate::provider::select_adapter(&pr_repo_info, force);
            let summary = adapter
                .pr_merge_summary(&pr_repo_info.slug, pr_number)
                .map_err(|err| CommandError::runtime("record-close-pr-summary-failed", err))?;
            Ok(lifecycle_record::LinkedPrEvidence {
                pr_ref: format!("{}#{pr_number}", pr_repo_info.slug),
                url: Some(pr_repo_info.pr_url(pr_number)),
                merge_sha: summary.merged.then_some(summary.merge_sha).flatten(),
                checks: check_status_from_state(summary.checks.as_deref()),
                required_state: summary
                    .required_state
                    .as_deref()
                    .map(|state| check_status_from_state(Some(state))),
                required_count: summary.required_count,
                non_required_failures: summary.non_required_failures,
            })
        })
        .collect()
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

#[derive(Debug, Clone)]
struct CloseLabelPlan {
    requested_add: Vec<String>,
    requested_remove: Vec<String>,
    add: Vec<String>,
    remove: Vec<String>,
    current: Option<Vec<String>>,
    final_labels: Option<Vec<String>>,
    catalog_checked: bool,
    missing_additions: Vec<String>,
}

impl CloseLabelPlan {
    fn preview(&self, confirmed: Option<&[String]>) -> Value {
        json!({
            "requested": {
                "add": self.requested_add,
                "remove": self.requested_remove,
            },
            "add": self.add,
            "remove": self.remove,
            "current": self.current,
            "final": self.final_labels,
            "availability": {
                "checked": self.catalog_checked,
                "missing_additions": self.missing_additions,
            },
            "confirmed": confirmed,
        })
    }
}

fn sorted_unique_labels_for_provider(
    labels: impl IntoIterator<Item = String>,
    provider: Option<crate::provider::Provider>,
) -> Vec<String> {
    let mut unique: Vec<String> = Vec::new();
    for label in labels {
        if !unique
            .iter()
            .any(|existing| label_matches(provider, existing, &label))
        {
            unique.push(label);
        }
    }
    if provider == Some(crate::provider::Provider::GitHub) {
        unique.sort_by(|left, right| {
            left.to_ascii_lowercase()
                .cmp(&right.to_ascii_lowercase())
                .then_with(|| left.cmp(right))
        });
    } else {
        unique.sort();
    }
    unique
}

fn label_matches(provider: Option<crate::provider::Provider>, left: &str, right: &str) -> bool {
    if provider == Some(crate::provider::Provider::GitHub) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

fn render_label_diagnostic(labels: &[String]) -> String {
    let quoted = labels
        .iter()
        .map(|label| serde_json::to_string(label).unwrap_or_else(|_| "\"<invalid>\"".to_string()))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{quoted}]")
}

fn build_close_label_plan(
    requested_add: Vec<String>,
    requested_remove: Vec<String>,
    current: Option<Vec<String>>,
    catalog: Option<Vec<String>>,
    provider: Option<crate::provider::Provider>,
) -> Result<CloseLabelPlan, CommandError> {
    let requested_add = sorted_unique_labels_for_provider(requested_add, provider);
    let requested_remove = sorted_unique_labels_for_provider(requested_remove, provider);
    let conflicts = requested_add
        .iter()
        .filter(|added| {
            requested_remove
                .iter()
                .any(|removed| label_matches(provider, added, removed))
        })
        .cloned()
        .collect::<Vec<_>>();
    if !conflicts.is_empty() {
        return Err(CommandError::usage(
            "record-label-mutation-conflict",
            format!(
                "record-close: label(s) appear in both --add-label and --remove-label: {}",
                render_label_diagnostic(&conflicts)
            ),
        ));
    }
    let state_additions: Vec<&str> = requested_add
        .iter()
        .map(String::as_str)
        .filter(|label| label.to_ascii_lowercase().starts_with("state::"))
        .collect();
    if state_additions.len() > 1 {
        return Err(CommandError::usage(
            "record-close-state-label-conflict",
            format!(
                "record-close: multiple mutually exclusive state labels requested: {}",
                render_label_diagnostic(
                    &state_additions
                        .iter()
                        .map(|label| (*label).to_string())
                        .collect::<Vec<_>>()
                )
            ),
        ));
    }

    let current = current.map(|labels| sorted_unique_labels_for_provider(labels, provider));
    let catalog = catalog.map(|labels| sorted_unique_labels_for_provider(labels, provider));
    let mut remove = requested_remove.clone();
    if let (Some(target), Some(current_labels)) = (state_additions.first(), current.as_ref()) {
        remove.extend(
            current_labels
                .iter()
                .filter(|label| label.to_ascii_lowercase().starts_with("state::"))
                .filter(|label| !label_matches(provider, label, target))
                .cloned(),
        );
    }
    let remove = sorted_unique_labels_for_provider(remove, provider);

    let missing_additions = catalog
        .as_ref()
        .map(|available| {
            requested_add
                .iter()
                .filter(|requested| {
                    !available
                        .iter()
                        .any(|actual| label_matches(provider, actual, requested))
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let final_labels = current.as_ref().map(|current_labels| {
        let mut final_labels: Vec<String> = current_labels
            .iter()
            .filter(|label| {
                !remove
                    .iter()
                    .any(|removed| label_matches(provider, label, removed))
            })
            .cloned()
            .collect();
        for added in &requested_add {
            if !final_labels
                .iter()
                .any(|current| label_matches(provider, current, added))
            {
                final_labels.push(added.clone());
            }
        }
        sorted_unique_labels_for_provider(final_labels, provider)
    });

    Ok(CloseLabelPlan {
        requested_add: requested_add.clone(),
        requested_remove,
        add: requested_add,
        remove,
        current,
        final_labels,
        catalog_checked: catalog.is_some(),
        missing_additions,
    })
}

fn close_label_plan_converged(
    provider: crate::provider::Provider,
    plan: &CloseLabelPlan,
    actual: &[String],
) -> bool {
    let additions_present = plan.add.iter().all(|expected_label| {
        actual
            .iter()
            .any(|actual_label| label_matches(Some(provider), expected_label, actual_label))
    });
    let removals_absent = plan.remove.iter().all(|removed_label| {
        !actual
            .iter()
            .any(|actual_label| label_matches(Some(provider), removed_label, actual_label))
    });
    let state_target = plan
        .add
        .iter()
        .find(|label| label.to_ascii_lowercase().starts_with("state::"));
    let state_exclusive = state_target.is_none_or(|target| {
        let actual_states: Vec<&String> = actual
            .iter()
            .filter(|label| label.to_ascii_lowercase().starts_with("state::"))
            .collect();
        actual_states.len() == 1 && label_matches(Some(provider), actual_states[0], target)
    });
    additions_present && removals_absent && state_exclusive
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

    let identity_marker = lifecycle_record::render_record_identity_marker(
        args.profile,
        &seed.source_path,
        &seed.source_commit,
    );
    lifecycle_record::extract_record_identity(&identity_marker)
        .and_then(|identity| {
            identity
                .filter(|identity| {
                    identity.matches(args.profile, &seed.source_path, &seed.source_commit)
                })
                .ok_or_else(|| "rendered record identity did not round-trip".to_string())
        })
        .map_err(|err| CommandError::runtime("record-open-identity-invalid", err))?;
    let initial_dashboard = format!(
        "{identity_marker}\n\n{}",
        record_initial_dashboard(args.profile, &seed.plan_title, None)
    );

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

    // Live mode. The provider-aware adapter selector routes `record open`
    // through `forge_cli_adapter::ForgeCliAdapter` (which shells out to
    // `forge-cli`) for every provider — GitHub, GitLab, and Local.
    let repo_info = resolve_repo_info_for_live(binary, repo_override)?;
    let adapter = crate::provider::select_adapter(&repo_info, force);
    let repo = repo_info.slug.as_str();

    // Serialize one canonical bundle identity locally. The reservation remains
    // held across journal reconciliation, provider reads, and all mutations.
    let identity = BundleIdentity {
        source_path: seed.source_path.clone(),
        source_commit: seed.source_commit.clone(),
    };
    let _record_open_reservation = crate::lifecycle_lock::acquire_record_open(
        &repo_info,
        args.profile,
        &identity.source_path,
        &identity.source_commit,
    )?;
    let body_path = write_temp_markdown("record-open-body", &initial_dashboard)
        .map_err(|err| CommandError::runtime("record-open-body-write-failed", err))?;
    let intent = RecordOpenIntentStore::new(
        &repo_info,
        args.profile,
        &identity.source_path,
        &identity.source_commit,
    );
    let pending = intent.load()?;
    let result = match pending {
        Some(pending) => record_open_continue_from_intent(
            adapter.as_ref(),
            &repo_info,
            args.profile,
            &body_path,
            &normalized_labels,
            &seed,
            &identity,
            &intent,
            pending,
            false,
            binary.execution_mode(),
        )?,
        None => {
            if let Some(issue_number) =
                detect_resumable_tracker(adapter.as_ref(), repo, args.profile, &identity)?
            {
                let issue_url = repo_info.issue_url(issue_number);
                intent.persist_issue_known(issue_number)?;
                record_open_resume_journaled(
                    adapter.as_ref(),
                    &repo_info,
                    args.profile,
                    &seed,
                    issue_number,
                    &issue_url,
                    &identity,
                    &intent,
                    None,
                    false,
                    &normalized_labels,
                    binary.execution_mode(),
                )?
            } else {
                intent.persist_create_in_flight()?;
                record_open_continue_from_intent(
                    adapter.as_ref(),
                    &repo_info,
                    args.profile,
                    &body_path,
                    &normalized_labels,
                    &seed,
                    &identity,
                    &intent,
                    RecordOpenIntentState::CreateInFlight,
                    true,
                    binary.execution_mode(),
                )?
            }
        }
    };

    // Mirror the live tracking-issue URL into the bundle's durable
    // `*-execution-state.md` so `plan-archive discover` can infer the provider
    // ref offline. The issue already exists, so a sync failure is reported in
    // the result rather than rolled back.
    Ok(attach_open_exec_state_sync(
        result,
        bundle.execution_state_file.as_deref(),
    ))
}

/// Patch the bundle's execution-state `Tracking issue` bullet to the URL the
/// `record open` result reports, then annotate the result with an
/// `execution_state_sync` object (action + whether a follow-up commit is
/// needed). Non-fatal: the tracker already exists, so a write error is reported
/// instead of failing the command.
fn attach_open_exec_state_sync(mut result: Value, execution_state_file: Option<&Path>) -> Value {
    let issue_url = result
        .get("issue")
        .and_then(|issue| issue.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|url| !url.is_empty());
    let sync = match (execution_state_file, issue_url) {
        (Some(path), Some(url)) => {
            match plan_tooling::exec_state::sync_tracking_issue(path, &url, false) {
                Ok(report) => json!({
                    "file": path.display().to_string(),
                    "changed": report.changed,
                    "followup_commit_required": report.changed,
                    "report": serde_json::to_value(&report).unwrap_or(Value::Null),
                }),
                Err(err) => json!({
                    "file": path.display().to_string(),
                    "ok": false,
                    "error": {"code": err.code(), "message": err.to_string()},
                }),
            }
        }
        (None, _) => json!({"skipped": true, "reason": "no execution-state file in bundle"}),
        (_, None) => json!({"skipped": true, "reason": "no issue url available"}),
    };
    if let Value::Object(map) = &mut result {
        map.insert("execution_state_sync".to_string(), sync);
    }
    result
}

/// Create or reconcile the issue named by the durable record-open intent, then
/// converge its source, plan, state, and dashboard surfaces. Kept as a wrapper
/// for focused adapter tests; the live command performs trusted auto-detection
/// before first entering this path.
#[cfg(test)]
fn record_open_finalize(
    adapter: &dyn ProviderAdapter,
    repo_info: &crate::provider::Repo,
    profile: crate::commands::record::RecordProfile,
    body_path: &Path,
    labels: &[String],
    seed: &RecordSeed,
    execution_mode: &'static str,
) -> Result<Value, CommandError> {
    let identity = BundleIdentity {
        source_path: seed.source_path.clone(),
        source_commit: seed.source_commit.clone(),
    };
    let intent = RecordOpenIntentStore::new(
        repo_info,
        profile,
        &identity.source_path,
        &identity.source_commit,
    );
    let pending = match intent.load()? {
        Some(pending) => (pending, false),
        None => {
            intent.persist_create_in_flight()?;
            (RecordOpenIntentState::CreateInFlight, true)
        }
    };
    record_open_continue_from_intent(
        adapter,
        repo_info,
        profile,
        body_path,
        labels,
        seed,
        &identity,
        &intent,
        pending.0,
        pending.1,
        execution_mode,
    )
}

#[allow(clippy::too_many_arguments)]
fn record_open_continue_from_intent(
    adapter: &dyn ProviderAdapter,
    repo_info: &crate::provider::Repo,
    profile: RecordProfile,
    body_path: &Path,
    labels: &[String],
    seed: &RecordSeed,
    identity: &BundleIdentity,
    intent: &RecordOpenIntentStore,
    pending: RecordOpenIntentState,
    attempt_create: bool,
    execution_mode: &'static str,
) -> Result<Value, CommandError> {
    let repo = repo_info.slug.as_str();
    match pending {
        RecordOpenIntentState::CreateInFlight => {
            let (issue_number, issue_url) = if attempt_create {
                match adapter.create_issue(repo, &seed.plan_title, body_path, labels) {
                    Ok((issue_number, _provider_url)) => {
                        (issue_number, repo_info.issue_url(issue_number))
                    }
                    Err(create_error) => reconcile_unknown_record_open_create(
                        adapter,
                        repo_info,
                        profile,
                        identity,
                        Some(&create_error),
                    )?,
                }
            } else {
                reconcile_unknown_record_open_create(adapter, repo_info, profile, identity, None)?
            };
            intent.persist_issue_known(issue_number)?;
            record_open_resume_journaled(
                adapter,
                repo_info,
                profile,
                seed,
                issue_number,
                &issue_url,
                identity,
                intent,
                None,
                attempt_create,
                labels,
                execution_mode,
            )
        }
        RecordOpenIntentState::IssueKnown { issue } => {
            let issue_url = repo_info.issue_url(issue);
            record_open_resume_journaled(
                adapter,
                repo_info,
                profile,
                seed,
                issue,
                &issue_url,
                identity,
                intent,
                None,
                false,
                labels,
                execution_mode,
            )
        }
        RecordOpenIntentState::CommentInFlight {
            issue,
            role,
            expected_payload,
            expected_fingerprint: _,
        } => {
            let current_payload = expected_record_open_payload(seed, role)?;
            if !expected_payload.semantically_matches(&current_payload) {
                return Err(CommandError::runtime(
                    "record-open-intent-invalid",
                    format!(
                        "record-open intent for {} comment does not match the current bundle snapshot",
                        role.as_str()
                    ),
                ));
            }
            let issue_url = repo_info.issue_url(issue);
            record_open_resume_journaled(
                adapter,
                repo_info,
                profile,
                seed,
                issue,
                &issue_url,
                identity,
                intent,
                Some((role, expected_payload)),
                false,
                labels,
                execution_mode,
            )
        }
    }
}

fn reconcile_unknown_record_open_create(
    adapter: &dyn ProviderAdapter,
    repo_info: &crate::provider::Repo,
    profile: RecordProfile,
    identity: &BundleIdentity,
    immediate_error: Option<&str>,
) -> Result<(u64, String), CommandError> {
    let repo = repo_info.slug.as_str();
    let numbers = adapter
        .list_open_tracker_issues(repo, &[])
        .map_err(|err| record_open_outcome_unknown("issue create", immediate_error, err))?;
    let mut matching = Vec::new();
    let mut historical_only = Vec::new();
    for number in numbers {
        let (body, comments_json) = adapter
            .issue_evidence(repo, number)
            .map_err(|err| record_open_outcome_unknown("issue create", immediate_error, err))?;
        let direct = lifecycle_record::extract_record_identity(&body)
            .map_err(|err| record_open_outcome_unknown("issue create", immediate_error, err))?;
        let historical = lifecycle_record::audit_record(None, &comments_json, Some(profile))
            .map_err(|err| record_open_outcome_unknown("issue create", immediate_error, err))?
            .record_identity;
        if let (Some(direct), Some(historical)) = (direct.as_ref(), historical.as_ref())
            && direct != historical
        {
            return Err(record_open_outcome_unknown(
                "issue create",
                immediate_error,
                format!(
                    "candidate issue #{number} has conflicting trusted issue-body and historical source-comment identities"
                ),
            ));
        }
        if direct.as_ref().is_some_and(|candidate| {
            candidate.matches(profile, &identity.source_path, &identity.source_commit)
        }) {
            matching.push(number);
        } else if historical.as_ref().is_some_and(|candidate| {
            candidate.matches(profile, &identity.source_path, &identity.source_commit)
        }) {
            historical_only.push(number);
        }
    }
    matching.sort_unstable();
    matching.dedup();
    historical_only.sort_unstable();
    historical_only.dedup();
    match (matching.as_slice(), historical_only.is_empty()) {
        ([issue], true) => Ok((*issue, repo_info.issue_url(*issue))),
        _ => Err(record_open_outcome_unknown(
            "issue create",
            immediate_error,
            format!(
                "broad provider scan found {} exact trusted issue-body identity markers and {} historical-only matching identities",
                matching.len(),
                historical_only.len()
            ),
        )),
    }
}

fn record_open_outcome_unknown(
    operation: &str,
    immediate_error: Option<&str>,
    proof_detail: impl Into<String>,
) -> CommandError {
    let immediate = immediate_error
        .map(|error| format!("; provider call reported: {error}"))
        .unwrap_or_default();
    CommandError::runtime(
        "record-open-outcome-unknown",
        format!(
            "record-open {operation} outcome is unknown{immediate}; proof readback did not establish exactly one matching provider result: {}. The durable local intent was retained and no duplicate mutation will be attempted automatically",
            proof_detail.into()
        ),
    )
}

/// Deterministic bundle identity carried by the trusted issue-body marker and
/// reused in the local record-open reservation/journal key.
struct BundleIdentity {
    source_path: String,
    source_commit: String,
}

/// Broad-scan open trackers for a unique exact bundle identity stored in the
/// issue body. Historical source-comment identity remains readable by audit
/// and repair, but it is never trusted for automatic resume. Any historical-only
/// match, malformed body marker, or trusted/historical ambiguity fails closed so
/// `record open` cannot create or mutate an arbitrary tracker.
fn detect_resumable_tracker(
    adapter: &dyn ProviderAdapter,
    repo: &str,
    profile: crate::commands::record::RecordProfile,
    identity: &BundleIdentity,
) -> Result<Option<u64>, CommandError> {
    let numbers = adapter
        .list_open_tracker_issues(repo, &[])
        .map_err(|err| CommandError::runtime("record-open-list-failed", err))?;
    let mut trusted = Vec::new();
    let mut historical = Vec::new();
    for number in numbers {
        let (body, comments_json) = adapter.issue_evidence(repo, number).map_err(|err| {
            CommandError::runtime(
                "record-open-evidence-read-failed",
                format!("failed to read candidate issue {number}: {err}"),
            )
        })?;
        let direct = lifecycle_record::extract_record_identity(&body).map_err(|err| {
            record_open_identity_repair_required(format!(
                "candidate issue #{number} has an invalid issue-body identity marker: {err}"
            ))
        })?;
        let historical_identity = lifecycle_record::audit_record(
            None,
            &comments_json,
            Some(profile),
        )
        .map_err(|err| {
            record_open_identity_repair_required(format!(
                "candidate issue #{number} has unreadable historical lifecycle identity evidence: {err}"
            ))
        })?
        .record_identity;

        if let (Some(direct), Some(historical_identity)) =
            (direct.as_ref(), historical_identity.as_ref())
            && direct != historical_identity
        {
            return Err(record_open_identity_repair_required(format!(
                "candidate issue #{number} has conflicting trusted issue-body and historical source-comment identities"
            )));
        }

        if direct.as_ref().is_some_and(|candidate| {
            candidate.matches(profile, &identity.source_path, &identity.source_commit)
        }) {
            trusted.push(number);
        } else if historical_identity.as_ref().is_some_and(|candidate| {
            candidate.matches(profile, &identity.source_path, &identity.source_commit)
        }) {
            historical.push(number);
        }
    }
    trusted.sort_unstable();
    trusted.dedup();
    historical.sort_unstable();
    historical.dedup();

    if trusted.len() > 1 || !historical.is_empty() {
        let mut ambiguous = trusted.clone();
        ambiguous.extend(historical.iter().copied());
        ambiguous.sort_unstable();
        ambiguous.dedup();
        return Err(record_open_identity_repair_required(format!(
            "open lifecycle tracker identity requires repair before automatic resume (candidate issues: {}); every resumable tracker must have one unique valid exact issue-body identity marker",
            ambiguous
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    Ok(trusted.first().copied())
}

fn record_open_identity_repair_required(message: impl Into<String>) -> CommandError {
    CommandError::runtime("record-open-identity-repair-required", message)
}

/// Resume an already-open tracker through the same journaled convergence path
/// used after issue creation. This wrapper is retained for focused tests.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn record_open_resume(
    adapter: &dyn ProviderAdapter,
    repo_info: &crate::provider::Repo,
    profile: crate::commands::record::RecordProfile,
    seed: &RecordSeed,
    issue_number: u64,
    _issue_url: &str,
    identity: &BundleIdentity,
    execution_mode: &'static str,
) -> Result<Value, CommandError> {
    let intent = RecordOpenIntentStore::new(
        repo_info,
        profile,
        &identity.source_path,
        &identity.source_commit,
    );
    let issue_url = repo_info.issue_url(issue_number);
    let pending_comment = match intent.load()? {
        None => {
            intent.persist_issue_known(issue_number)?;
            None
        }
        Some(RecordOpenIntentState::IssueKnown { issue }) if issue == issue_number => None,
        Some(RecordOpenIntentState::CommentInFlight {
            issue,
            role,
            expected_payload,
            expected_fingerprint: _,
        }) if issue == issue_number => {
            let current_payload = expected_record_open_payload(seed, role)?;
            if !expected_payload.semantically_matches(&current_payload) {
                return Err(CommandError::runtime(
                    "record-open-intent-invalid",
                    format!(
                        "record-open intent for {} comment does not match the current bundle snapshot",
                        role.as_str()
                    ),
                ));
            }
            Some((role, expected_payload))
        }
        Some(_) => {
            return Err(CommandError::runtime(
                "record-open-intent-invalid",
                "record-open intent does not name the tracker selected for resume",
            ));
        }
    };
    record_open_resume_journaled(
        adapter,
        repo_info,
        profile,
        seed,
        issue_number,
        &issue_url,
        identity,
        &intent,
        pending_comment,
        false,
        &[],
        execution_mode,
    )
}

#[allow(clippy::too_many_arguments)]
fn record_open_resume_journaled(
    adapter: &dyn ProviderAdapter,
    repo_info: &crate::provider::Repo,
    profile: RecordProfile,
    seed: &RecordSeed,
    issue_number: u64,
    issue_url: &str,
    identity: &BundleIdentity,
    intent: &RecordOpenIntentStore,
    pending_comment: Option<(
        lifecycle_record::PayloadRole,
        lifecycle_record::RecordPayload,
    )>,
    created_this_run: bool,
    labels: &[String],
    execution_mode: &'static str,
) -> Result<Value, CommandError> {
    let _lifecycle_lock = crate::lifecycle_lock::acquire(repo_info, issue_number, profile)?;
    let repo = repo_info.slug.as_str();
    let pending_operation = pending_comment
        .as_ref()
        .map(|(role, _)| format!("{} comment", role.as_str()));
    let (mut current_body, current_comments) =
        adapter.issue_evidence(repo, issue_number).map_err(|err| {
            if let Some(operation) = pending_operation.as_deref() {
                record_open_outcome_unknown(
                    operation,
                    None,
                    format!("provider evidence read failed for issue #{issue_number}: {err}"),
                )
            } else {
                CommandError::runtime("record-open-evidence-read-failed", err)
            }
        })?;
    validate_record_open_direct_identity(&current_body, issue_number, profile, identity).map_err(
        |err| {
            if let Some(operation) = pending_operation.as_deref() {
                record_open_outcome_unknown(operation, None, err.message)
            } else {
                err
            }
        },
    )?;
    validate_record_open_historical_identity(&current_comments, issue_number, profile, identity)
        .map_err(|err| {
            if let Some(operation) = pending_operation.as_deref() {
                record_open_outcome_unknown(operation, None, err.message)
            } else {
                err
            }
        })?;
    let mut audit =
        lifecycle_record::audit_record(Some(&current_body), &current_comments, Some(profile))
            .map_err(|err| {
                if let Some(operation) = pending_operation.as_deref() {
                    record_open_outcome_unknown(
                        operation,
                        None,
                        format!("provider lifecycle evidence is unreadable: {err}"),
                    )
                } else {
                    CommandError::runtime("record-open-audit-failed", err)
                }
            })?;

    if let Some((role, expected_payload)) = pending_comment {
        if matching_record_open_comment_url(&audit, role, &expected_payload).is_none() {
            return Err(record_open_outcome_unknown(
                &format!("{} comment", role.as_str()),
                None,
                format!(
                    "provider evidence for issue #{issue_number} has no same-role semantically matching payload"
                ),
            ));
        }
        intent.persist_issue_known(issue_number)?;
    }

    let missing: std::collections::BTreeSet<&str> =
        audit.missing_required.iter().map(String::as_str).collect();
    let mut attached: Vec<&'static str> = Vec::new();
    for &(role, code, temp_label, body) in &[
        (
            "source",
            "source-missing",
            "record-open-source-comment",
            seed.source_body.as_str(),
        ),
        (
            "plan",
            "plan-missing",
            "record-open-plan-comment",
            seed.plan_body.as_str(),
        ),
        (
            "state",
            "state-missing",
            "record-open-state-comment",
            seed.state_body.as_str(),
        ),
    ] {
        if !missing.contains(code) {
            continue;
        }
        let expected_payload = lifecycle_record::extract_payload(body).map_err(|err| {
            CommandError::runtime(
                "record-open-comment-payload-invalid",
                format!("rendered {role} comment has invalid lifecycle payload: {err}"),
            )
        })?;
        let path = write_temp_markdown(temp_label, body)
            .map_err(|err| CommandError::runtime("record-open-comment-write-failed", err))?;
        intent.persist_comment_in_flight(issue_number, &expected_payload)?;
        let post_error = adapter.comment_issue(repo, issue_number, &path).err();
        prove_record_open_comment(
            adapter,
            repo,
            issue_number,
            profile,
            identity,
            expected_payload.role,
            &expected_payload,
        )
        .map_err(|proof_detail| {
            record_open_outcome_unknown(
                &format!("{role} comment"),
                post_error.as_deref(),
                proof_detail,
            )
        })?;
        intent.persist_issue_known(issue_number)?;
        attached.push(role);
    }

    if !attached.is_empty() {
        let (body_after, comments_json) = adapter
            .issue_evidence(repo, issue_number)
            .map_err(|err| CommandError::runtime("record-open-evidence-read-failed", err))?;
        validate_record_open_direct_identity(&body_after, issue_number, profile, identity)?;
        validate_record_open_historical_identity(&comments_json, issue_number, profile, identity)?;
        audit = lifecycle_record::audit_record(Some(&body_after), &comments_json, Some(profile))
            .map_err(|err| CommandError::runtime("record-open-audit-failed", err))?;
        current_body = body_after;
    }
    if !audit.missing_required.is_empty() {
        return Err(record_open_outcome_unknown(
            "comment convergence",
            None,
            format!(
                "provider evidence still reports missing roles: {}",
                audit.missing_required.join(", ")
            ),
        ));
    }

    let repaired = lifecycle_record::render_dashboard_from_audit(
        &audit,
        Some(&seed.plan_title),
        Some(issue_url),
    );
    if current_body != repaired {
        let repaired_path = write_temp_markdown("record-open-dashboard", &repaired)
            .map_err(|err| CommandError::runtime("record-open-dashboard-write-failed", err))?;
        adapter
            .edit_issue_body(repo, issue_number, &repaired_path)
            .map_err(|err| CommandError::runtime("record-open-dashboard-edit-failed", err))?;
    }
    intent.clear()?;

    let mode = if created_this_run {
        "live"
    } else if attached.is_empty() {
        "already-open"
    } else {
        "resumed"
    };
    Ok(json!({
        "operation": "record.open",
        "execution_mode": execution_mode,
        "dry_run": false,
        "mode": mode,
        "issue": {"number": issue_number, "url": issue_url},
        "comments": {
            "source": audit.evidence.get("source").and_then(|hit| hit.url.clone()),
            "plan": audit.evidence.get("plan").and_then(|hit| hit.url.clone()),
            "state": audit.evidence.get("state").and_then(|hit| hit.url.clone()),
        },
        "attached": attached,
        "labels": labels,
        "dashboard_markdown": repaired,
    }))
}

fn expected_record_open_payload(
    seed: &RecordSeed,
    role: lifecycle_record::PayloadRole,
) -> Result<lifecycle_record::RecordPayload, CommandError> {
    let body = match role {
        lifecycle_record::PayloadRole::Source => seed.source_body.as_str(),
        lifecycle_record::PayloadRole::Plan => seed.plan_body.as_str(),
        lifecycle_record::PayloadRole::State => seed.state_body.as_str(),
        _ => {
            return Err(CommandError::runtime(
                "record-open-intent-invalid",
                format!(
                    "record-open intent contains unsupported {} comment role",
                    role.as_str()
                ),
            ));
        }
    };
    lifecycle_record::extract_payload(body).map_err(|err| {
        CommandError::runtime(
            "record-open-comment-payload-invalid",
            format!(
                "current rendered {} comment has invalid lifecycle payload: {err}",
                role.as_str()
            ),
        )
    })
}

fn validate_record_open_direct_identity(
    body: &str,
    issue_number: u64,
    profile: RecordProfile,
    identity: &BundleIdentity,
) -> Result<(), CommandError> {
    let direct_identity = lifecycle_record::extract_record_identity(body).map_err(|err| {
        CommandError::runtime(
            "record-open-identity-changed",
            format!(
                "issue #{issue_number} has an invalid direct issue-body identity marker after acquiring its lifecycle lock: {err}"
            ),
        )
    })?;
    if !direct_identity.as_ref().is_some_and(|candidate| {
        candidate.matches(profile, &identity.source_path, &identity.source_commit)
    }) {
        return Err(CommandError::runtime(
            "record-open-identity-changed",
            format!(
                "issue #{issue_number} no longer has the unique exact bundle identity marker in its body after acquiring its lifecycle lock"
            ),
        ));
    }
    Ok(())
}

fn validate_record_open_historical_identity(
    comments_json: &str,
    issue_number: u64,
    profile: RecordProfile,
    identity: &BundleIdentity,
) -> Result<(), CommandError> {
    let historical_identity = lifecycle_record::audit_record(None, comments_json, Some(profile))
        .map_err(|err| {
            record_open_identity_repair_required(format!(
                "issue #{issue_number} has unreadable historical lifecycle identity evidence after acquiring its lifecycle lock: {err}"
            ))
        })?
        .record_identity;
    if historical_identity.as_ref().is_some_and(|candidate| {
        !candidate.matches(profile, &identity.source_path, &identity.source_commit)
    }) {
        return Err(record_open_identity_repair_required(format!(
            "issue #{issue_number} has conflicting trusted issue-body and historical source-comment identities after acquiring its lifecycle lock"
        )));
    }
    Ok(())
}

fn matching_record_open_comment_url(
    audit: &lifecycle_record::RecordAudit,
    role: lifecycle_record::PayloadRole,
    expected: &lifecycle_record::RecordPayload,
) -> Option<String> {
    let evidence = audit.evidence.get(role.as_str())?;
    evidence
        .payload
        .as_ref()
        .filter(|payload| payload.semantically_matches(expected))?;
    evidence.url.clone()
}

fn prove_record_open_comment(
    adapter: &dyn ProviderAdapter,
    repo: &str,
    issue: u64,
    profile: RecordProfile,
    identity: &BundleIdentity,
    role: lifecycle_record::PayloadRole,
    expected: &lifecycle_record::RecordPayload,
) -> Result<String, String> {
    let (body, comments_json) = adapter
        .issue_evidence(repo, issue)
        .map_err(|err| format!("provider evidence read failed: {err}"))?;
    validate_record_open_direct_identity(&body, issue, profile, identity)
        .map_err(|err| err.message)?;
    validate_record_open_historical_identity(&comments_json, issue, profile, identity)
        .map_err(|err| err.message)?;
    let audit = lifecycle_record::audit_record(Some(&body), &comments_json, Some(profile))
        .map_err(|err| format!("provider lifecycle evidence is unreadable: {err}"))?;
    matching_record_open_comment_url(&audit, role, expected).ok_or_else(|| {
        format!(
            "provider evidence for issue #{issue} has no same-role semantically matching payload"
        )
    })
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
    let _lifecycle_lock = crate::lifecycle_lock::acquire(&repo_info, issue_number, args.profile)?;
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
    let execution_state = match &args.execution_state_file {
        Some(path) => {
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
        None => None,
    };
    let summary = match &args.summary_file {
        Some(path) => Some(read_text_file(path, "record-post-summary-read-failed")?),
        None => None,
    };
    let body = lifecycle_record::render_record_post_comment_with_display(
        args.profile,
        args.kind,
        payload_data,
        execution_state.as_deref(),
        summary.as_deref(),
        None,
        args.task_ledger_display,
    )
    .map_err(|err| CommandError::runtime("record-post-render-failed", err))?;
    validate_record_post_payload_carrier(&body)?;

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
    let issue_number = parse_issue_reference(&args.issue)?;
    let comment_path = write_temp_markdown("record-post-comment", &body)
        .map_err(|err| CommandError::runtime("record-post-comment-write-failed", err))?;
    let _lifecycle_lock = crate::lifecycle_lock::acquire(&repo_info, issue_number, args.profile)?;
    let adapter = crate::provider::select_adapter(&repo_info, force);
    let repo = repo_info.slug;
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

fn validate_record_post_payload_carrier(body: &str) -> Result<(), CommandError> {
    let marker_count = lifecycle_record::raw_payload_marker_count(body);
    if marker_count != 1 {
        let message = if marker_count == 0 {
            "no plan-issue-record-payload carrier or fence in comment body".to_string()
        } else {
            "multiple plan-issue-record-payload carriers or fences in comment body".to_string()
        };
        return Err(CommandError::usage(
            "record-post-payload-carrier-conflict",
            format!(
                "`record post` rendered body must contain exactly one plan-issue-record-payload carrier; {message}"
            ),
        ));
    }
    lifecycle_record::extract_payload(body)
        .map(|_| ())
        .map_err(|err| {
            CommandError::usage(
                "record-post-payload-carrier-conflict",
                format!(
                    "`record post` rendered body must contain exactly one plan-issue-record-payload carrier; {err}"
                ),
            )
        })
}

fn run_record_repair_dashboard(
    binary: BinaryFlavor,
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &RecordRepairDashboardArgs,
) -> Result<Value, CommandError> {
    let mut _record_repair_lifecycle_lock = None;
    let (body, comments_json, issue_number, repo_info, issue_url) = if let Some(fixture_dir) =
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
        if !dry_run && args.out.is_none() {
            _record_repair_lifecycle_lock = Some(crate::lifecycle_lock::acquire(
                &repo_info,
                issue_number,
                RecordProfile::Tracking,
            )?);
        }
        let adapter = crate::provider::select_adapter(&repo_info, force);
        let issue_url = repo_info.issue_url(issue_number);
        let repo = repo_info.slug.clone();
        let (body, comments) = adapter
            .issue_evidence(&repo, issue_number)
            .map_err(|err| CommandError::runtime("record-repair-evidence-read-failed", err))?;
        (
            body,
            comments,
            Some(issue_number),
            Some(repo_info),
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
    let repo_info = repo_info.expect("live mode has resolved repo");
    let adapter = crate::provider::select_adapter(&repo_info, force);
    let dashboard_path = write_temp_markdown("record-repair-dashboard", &dashboard)
        .map_err(|err| CommandError::runtime("record-repair-dashboard-write-failed", err))?;
    adapter
        .edit_issue_body(&repo_info.slug, issue_number, &dashboard_path)
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

fn matching_closeout_url(
    audit: &lifecycle_record::RecordAudit,
    expected: &lifecycle_record::RecordPayload,
) -> Option<String> {
    let closeout = audit.evidence.get("closeout")?;
    closeout
        .payload
        .as_ref()
        .filter(|payload| payload.semantically_matches(expected))?;
    closeout.url.clone()
}

fn resolve_closeout_comment_url(
    adapter: &dyn ProviderAdapter,
    repo: &str,
    issue: u64,
    profile: RecordProfile,
    audit: &lifecycle_record::RecordAudit,
    expected: &lifecycle_record::RecordPayload,
    closeout_body: &str,
) -> Result<String, CommandError> {
    if let Some(url) = matching_closeout_url(audit, expected) {
        return Ok(url);
    }

    let closeout_path = write_temp_markdown("record-close-comment", closeout_body)
        .map_err(|err| CommandError::runtime("record-close-comment-write-failed", err))?;
    match adapter.comment_issue(repo, issue, &closeout_path) {
        Ok(url) => Ok(url),
        Err(post_error) => {
            let recovered_url = adapter
                .issue_evidence(repo, issue)
                .ok()
                .and_then(|(body, comments)| {
                    lifecycle_record::audit_record(Some(&body), &comments, Some(profile)).ok()
                })
                .and_then(|audit| matching_closeout_url(&audit, expected));
            recovered_url.ok_or_else(|| {
                CommandError::runtime("record-close-comment-post-failed", post_error)
            })
        }
    }
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

    // Provider-backed live close holds the issue-scoped lifecycle lock from
    // before the first gate-bearing evidence read through the terminal
    // mutation and local writeback. Offline/fixture previews never mutate the
    // provider and therefore do not contend for this lock.
    let mut _record_close_lifecycle_lock = None;

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
        let issue_number = parse_issue_reference(&args.issue)?;
        if !dry_run {
            _record_close_lifecycle_lock = Some(crate::lifecycle_lock::acquire(
                &repo_info,
                issue_number,
                args.profile,
            )?);
        }
        let adapter = crate::provider::select_adapter(&repo_info, force);
        let (body, comments) = adapter
            .issue_evidence(&repo_info.slug, issue_number)
            .map_err(|err| CommandError::runtime("record-close-evidence-read-failed", err))?;
        (body, comments, Some(repo_info), issue_number)
    };

    // Resolve linked PRs through provider/fixture for merge_sha + checks.
    let linked_evidence: Vec<lifecycle_record::LinkedPrEvidence> =
        if let Some(fixture_dir) = &args.fixture {
            args.linked_pr
                .iter()
                .map(|raw| {
                    let (pr_repo, pr_number) = parse_linked_pr_reference(raw)?;
                    read_fixture_pr_snapshot(fixture_dir, &pr_repo, pr_number)
                })
                .collect::<Result<_, _>>()?
        } else if let Some(tracking_repo) = repo_for_provider.as_ref() {
            read_live_linked_pr_evidence(&args.linked_pr, tracking_repo, force)?
        } else {
            args.linked_pr
                .iter()
                .map(|raw| {
                    let (pr_repo, pr_number) = parse_linked_pr_reference(raw)?;
                    // body-file/comments-json mode without fixture cannot resolve
                    // provider state, so the strict gate sees a missing merge SHA.
                    Ok(lifecycle_record::LinkedPrEvidence {
                        pr_ref: format!("{pr_repo}#{pr_number}"),
                        url: None,
                        merge_sha: None,
                        checks: lifecycle_record::CheckStatus::None,
                        required_state: None,
                        required_count: None,
                        non_required_failures: Vec::new(),
                    })
                })
                .collect::<Result<_, CommandError>>()?
        };

    let audit = lifecycle_record::audit_record(Some(&body), &comments_json, Some(args.profile))
        .map_err(|err| CommandError::runtime("record-close-audit-failed", err))?;

    // Audit the provider evidence before constructing the closeout preview.
    let issue_url_hint = repo_for_provider
        .as_ref()
        .map(|repo| repo.issue_url(issue_number));

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

    // Dry-run and fixture previews have not posted the closeout comment yet, but
    // their `final_dashboard` must represent the evidence that this command
    // would append. Add the rendered payload to an in-memory audit instead of
    // treating a pre-close `state.status=complete` comment as terminal.
    let preview_closeout_payload = lifecycle_record::extract_payload(&closeout_body)
        .map_err(|err| CommandError::runtime("record-close-render-failed", err.to_string()))?;
    let preview_closeout_status = preview_closeout_payload
        .parse_closeout()
        .map_err(|err| CommandError::runtime("record-close-render-failed", err.to_string()))?
        .final_status;
    let mut preview_audit = audit.clone();
    preview_audit.evidence.insert(
        "closeout".to_string(),
        lifecycle_record::LifecycleEvidence {
            role: lifecycle_record::PayloadRole::Closeout,
            profile: args.profile.into(),
            url: None,
            created_at: None,
            status: Some(preview_closeout_status.clone()),
            payload: Some(preview_closeout_payload.clone()),
            plan_title: None,
        },
    );
    let canonical_dashboard = lifecycle_record::render_dashboard_from_audit(
        &preview_audit,
        None,
        issue_url_hint.as_deref(),
    );

    let (requested_add_labels, requested_remove_labels) =
        normalize_label_mutations(&args.add_labels, &args.remove_labels, "record-close")?;
    let label_mutation_requested =
        !requested_add_labels.is_empty() || !requested_remove_labels.is_empty();
    let mut label_plan = build_close_label_plan(
        requested_add_labels,
        requested_remove_labels,
        None,
        None,
        repo_for_provider.as_ref().map(|repo| repo.provider),
    )?;
    if label_mutation_requested && let Some(repo_info) = repo_for_provider.as_ref() {
        let adapter = crate::provider::select_adapter(repo_info, force);
        let current = adapter
            .issue_labels(&repo_info.slug, issue_number)
            .map_err(|err| CommandError::runtime("record-close-label-state-read-failed", err))?;
        let catalog = if repo_info.provider == crate::provider::Provider::Local {
            None
        } else {
            Some(adapter.repository_labels(&repo_info.slug).map_err(|err| {
                CommandError::runtime("record-close-label-catalog-read-failed", err)
            })?)
        };
        label_plan = build_close_label_plan(
            label_plan.requested_add,
            label_plan.requested_remove,
            Some(current),
            catalog,
            Some(repo_info.provider),
        )?;
    }
    let add_labels = label_plan.add.clone();
    let remove_labels = label_plan.remove.clone();
    let label_mutation_planned = !add_labels.is_empty() || !remove_labels.is_empty();
    let labels_preview = label_plan.preview(None);
    let execution_state_writeback = repo_for_provider
        .as_ref()
        .map(|repo| {
            prepare_close_exec_state_writeback(
                args.bundle.as_deref(),
                repo,
                &repo.issue_url(issue_number),
                &linked_evidence,
            )
        })
        .transpose()?;

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

    if !label_plan.missing_additions.is_empty() {
        return Err(CommandError::runtime(
            "record-close-label-preflight-failed",
            format!(
                "requested label additions are unavailable in the repository: {}",
                render_label_diagnostic(&label_plan.missing_additions)
            ),
        ));
    }

    let repo_info = repo_for_provider.expect("live mode has repo");
    let repo = repo_info.slug.clone();
    let issue_url = repo_info.issue_url(issue_number);
    let _execution_state_writeback_preflight = execution_state_writeback
        .expect("provider-backed close preflights execution-state writeback");
    let adapter = crate::provider::select_adapter(&repo_info, force);
    let revalidate_close_gate = || -> Result<
        (
            lifecycle_record::RecordAudit,
            Vec<lifecycle_record::LinkedPrEvidence>,
        ),
        CommandError,
    > {
        let (latest_body, latest_comments) = adapter
            .issue_evidence(&repo, issue_number)
            .map_err(|err| CommandError::runtime("record-close-gate-reread-failed", err))?;
        let latest_audit = lifecycle_record::audit_record(
            Some(&latest_body),
            &latest_comments,
            Some(args.profile),
        )
        .map_err(|err| CommandError::runtime("record-close-gate-reaudit-failed", err))?;
        let latest_linked_evidence =
            read_live_linked_pr_evidence(&args.linked_pr, &repo_info, force)?;
        let latest_gate = lifecycle_record::evaluate_strict_closeout_gate(
            &latest_audit,
            lifecycle_record::StrictCloseoutGateInput {
                profile: args.profile,
                approval: Some(approval_text),
                linked_prs: &latest_linked_evidence,
                current_body: None,
                expected_dashboard: None,
                allow_non_required_check_failure: args.allow_non_required_check_failure,
            },
        );
        if latest_gate.ready {
            Ok((latest_audit, latest_linked_evidence))
        } else {
            Err(CommandError::runtime(
                "record-close-gate-changed",
                format!(
                    "strict closeout gate changed after preflight: {} ({})",
                    latest_gate.blocked_codes.join(", "),
                    latest_gate
                        .checks
                        .iter()
                        .filter(|check| check.status == "fail")
                        .map(|check| format!("{}: {}", check.check, check.detail))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            ))
        }
    };

    // Re-fetch gate-bearing issue evidence under the lifecycle lock directly
    // before provider closure. Closing is the first mutation: a failed close
    // must not leave terminal labels, closeout evidence, or a final dashboard
    // on an issue that is still open.
    let (final_audit, linked_evidence) = revalidate_close_gate()?;

    // The final, still-passing refresh is the evidence snapshot that is
    // terminalized. Rebuild every closeout projection before the close, but do
    // not mutate the provider until `close_issue` succeeds.
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
    let final_validation_url = final_audit
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
    let closeout_body = lifecycle_record::render_record_post_comment(
        args.profile,
        crate::commands::record::LifecycleCommentKind::Closeout,
        closeout_payload,
        Some(closeout_summary),
        None,
    )
    .map_err(|err| CommandError::runtime("record-close-render-failed", err))?;
    let preview_closeout_payload = lifecycle_record::extract_payload(&closeout_body)
        .map_err(|err| CommandError::runtime("record-close-render-failed", err.to_string()))?;
    let preview_closeout_status = preview_closeout_payload
        .parse_closeout()
        .map_err(|err| CommandError::runtime("record-close-render-failed", err.to_string()))?
        .final_status;
    let execution_state_writeback = prepare_close_exec_state_writeback(
        args.bundle.as_deref(),
        &repo_info,
        &issue_url,
        &linked_evidence,
    )?;

    if let Err(close_error) = adapter.close_issue(
        &repo,
        issue_number,
        crate::commands::plan::CloseReason::Completed,
        None,
    ) {
        let confirmed_closed = adapter
            .issue_state(&repo, issue_number)
            .is_ok_and(|state| state.trim().eq_ignore_ascii_case("closed"));
        if !confirmed_closed {
            return Err(CommandError::runtime(
                "record-close-issue-close-failed",
                close_error,
            ));
        }
    }

    let confirmed_labels = if label_mutation_planned {
        adapter
            .edit_issue_labels(&repo, issue_number, &add_labels, &remove_labels)
            .map_err(|err| CommandError::runtime("record-close-label-edit-failed", err))?;
        let observed = adapter
            .issue_labels(&repo, issue_number)
            .map_err(|err| CommandError::runtime("record-close-label-readback-failed", err))?;
        let observed = sorted_unique_labels_for_provider(observed, Some(repo_info.provider));
        if !close_label_plan_converged(repo_info.provider, &label_plan, &observed) {
            let expected = label_plan.final_labels.as_deref().unwrap_or_default();
            return Err(CommandError::runtime(
                "record-close-label-convergence-failed",
                format!(
                    "provider label read-back differs from the preflighted final set; expected {}, observed {}",
                    render_label_diagnostic(expected),
                    render_label_diagnostic(&observed)
                ),
            ));
        }
        Some(observed)
    } else {
        None
    };
    let labels_result = label_plan.preview(confirmed_labels.as_deref());

    let closeout_url = resolve_closeout_comment_url(
        adapter.as_ref(),
        &repo,
        issue_number,
        args.profile,
        &final_audit,
        &preview_closeout_payload,
        &closeout_body,
    )?;

    // The final gate audit is the authoritative pre-close snapshot. Provider
    // reads can lag a successful comment write, so overlay the returned
    // closeout URL and the freshly rendered payload directly onto that audit.
    let mut audit_after = final_audit;
    audit_after.evidence.insert(
        "closeout".to_string(),
        lifecycle_record::LifecycleEvidence {
            role: lifecycle_record::PayloadRole::Closeout,
            profile: args.profile.into(),
            url: Some(closeout_url.clone()),
            created_at: None,
            status: Some(preview_closeout_status),
            payload: Some(preview_closeout_payload),
            plan_title: None,
        },
    );
    let final_dashboard =
        lifecycle_record::render_dashboard_from_audit(&audit_after, None, Some(&issue_url));
    let dashboard_path = write_temp_markdown("record-close-dashboard", &final_dashboard)
        .map_err(|err| CommandError::runtime("record-close-dashboard-write-failed", err))?;
    adapter
        .edit_issue_body(&repo, issue_number, &dashboard_path)
        .map_err(|err| CommandError::runtime("record-close-dashboard-edit-failed", err))?;

    // Apply the preflighted terminal writeback only after the provider close
    // succeeds. Repository, parent, and target descriptors were pinned before
    // provider mutation; apply revalidates their identities and replaces the
    // file relative to the pinned parent descriptor.
    let execution_state_sync = apply_close_exec_state_writeback(execution_state_writeback)?;

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
        "labels": labels_result,
        "execution_state_sync": execution_state_sync,
    }))
}

/// Terminal-state writeback for `record close`. Patches the bundle's
/// execution-state fields (`Status`, `Current task`, `Next task`, `Last
/// updated`, `Branch/commit/PR`, `Tracking issue`, and `Handoff`) to coherent
/// final values. A provider-close success followed by a local writeback failure
/// returns a nonzero command error instead of embedding failure in a success
/// envelope. The `## Task Ledger` rows
/// are owned by the existing per-task `ledger-update` + `close-ready`
/// `ledger-rows-pending` gate, so this writeback never rewrites them.
enum CloseExecStateWritebackPlan {
    Skipped(&'static str),
    Apply {
        exec_state: plan_tooling::exec_state::PinnedExecutionState,
        expected_contents: String,
        state: Box<plan_tooling::exec_state::TerminalState>,
    },
}

fn close_exec_state_terminal_state(
    issue_url: &str,
    linked_prs: &[lifecycle_record::LinkedPrEvidence],
) -> plan_tooling::exec_state::TerminalState {
    let branch_commit_pr = if linked_prs.is_empty() {
        None
    } else {
        Some(
            linked_prs
                .iter()
                .map(|pr| match &pr.url {
                    // Angle-bracket the URL so the written execution-state passes
                    // markdown lint (MD034 forbids bare URLs), matching the
                    // `Tracking issue` bullet's autolink treatment (#1006).
                    Some(url) => format!("{} merged (<{url}>)", pr.pr_ref),
                    None => format!("{} merged", pr.pr_ref),
                })
                .collect::<Vec<_>>()
                .join("; "),
        )
    };
    plan_tooling::exec_state::TerminalState {
        status: Some("complete".to_string()),
        current_task: Some("complete".to_string()),
        next_task: Some("none".to_string()),
        last_updated: Some(chrono::Utc::now().format("%Y-%m-%d").to_string()),
        branch_commit_pr,
        tracking_issue_url: Some(issue_url.to_string()),
        handoff: Some(format!(
            "- Tracking issue <{issue_url}> is closed; terminal execution state is synchronized. No closeout or merge action remains."
        )),
    }
}

fn prepare_close_exec_state_writeback(
    bundle: Option<&Path>,
    repo: &crate::provider::Repo,
    issue_url: &str,
    linked_prs: &[lifecycle_record::LinkedPrEvidence],
) -> Result<CloseExecStateWritebackPlan, CommandError> {
    let Some(bundle) = bundle else {
        return Ok(CloseExecStateWritebackPlan::Skipped("no --bundle provided"));
    };
    let allow_any_git_repo = matches!(repo.provider, crate::provider::Provider::Local);
    let repo_root = verified_current_repo_root_for_identity(Some(repo), allow_any_git_repo)
        .ok_or_else(|| {
            CommandError::usage(
                "record-close-execution-state-path-invalid",
                "--bundle requires a verified matching Git repository",
            )
        })?;
    let bundle =
        confined_existing_path(&repo_root, &absolutize(bundle), false).ok_or_else(|| {
            CommandError::usage(
                "record-close-execution-state-path-invalid",
                "--bundle must be a real directory inside the verified repository",
            )
        })?;
    let exec_state = unique_confined_execution_state(&repo_root, &bundle).map_err(|err| {
        CommandError::usage(
            match err {
                StateLedgerResolutionError::Ambiguous => "record-close-execution-state-ambiguous",
                StateLedgerResolutionError::Unresolved => {
                    "record-close-execution-state-path-invalid"
                }
            },
            "--bundle must contain at most one real *-execution-state.md file",
        )
    })?;
    let Some(exec_state) = exec_state else {
        return Ok(CloseExecStateWritebackPlan::Skipped(
            "no *-execution-state.md in bundle",
        ));
    };
    let exec_state = confined_existing_path(&repo_root, &exec_state, true).ok_or_else(|| {
        CommandError::usage(
            "record-close-execution-state-path-invalid",
            "execution-state file must be a real file inside the verified repository",
        )
    })?;
    let exec_state = plan_tooling::exec_state::PinnedExecutionState::pin(&repo_root, &exec_state)
        .map_err(|err| {
        CommandError::usage(
            "record-close-execution-state-path-invalid",
            format!(
                "execution-state file could not be pinned inside the verified repository: {err}"
            ),
        )
    })?;
    let state = close_exec_state_terminal_state(issue_url, linked_prs);
    let expected_contents = exec_state.read_to_string().map_err(|err| {
        CommandError::runtime(
            "record-close-execution-state-preflight-failed",
            err.to_string(),
        )
    })?;
    let ledger_rows = plan_tooling::ledger::read_rows(&expected_contents, exec_state.path())
        .map_err(|err| {
            let code = if err.code() == "ledger-row-ambiguous" {
                "record-close-execution-state-ledger-ambiguous"
            } else {
                "record-close-execution-state-ledger-malformed"
            };
            CommandError::usage(code, err.to_string())
        })?;
    let nonterminal = ledger_rows
        .iter()
        .filter(|row| !matches!(row.status.as_str(), "done" | "deferred" | "waived"))
        .map(|row| format!("{} ({})", row.id, row.status))
        .collect::<Vec<_>>();
    if !nonterminal.is_empty() {
        return Err(CommandError::usage(
            "record-close-execution-state-ledger-pending",
            format!(
                "execution-state Task Ledger contains nonterminal rows: {}",
                nonterminal.join(", ")
            ),
        ));
    }
    plan_tooling::exec_state::writeback_terminal_pinned_if_unchanged(
        &exec_state,
        &expected_contents,
        &state,
        true,
    )
    .map_err(|err| match err {
        plan_tooling::exec_state::ExecStateError::ExpectedContentsChanged { .. }
        | plan_tooling::exec_state::ExecStateError::ExpectedPathChanged { .. } => {
            CommandError::runtime(
                "record-close-execution-state-path-changed",
                "execution-state path or contents changed during preflight",
            )
        }
        err => CommandError::runtime(
            "record-close-execution-state-preflight-failed",
            err.to_string(),
        ),
    })?;
    Ok(CloseExecStateWritebackPlan::Apply {
        exec_state,
        expected_contents,
        state: Box::new(state),
    })
}

fn apply_close_exec_state_writeback(
    plan: CloseExecStateWritebackPlan,
) -> Result<Value, CommandError> {
    let (exec_state, expected_contents, state) = match plan {
        CloseExecStateWritebackPlan::Skipped(reason) => {
            return Ok(json!({"skipped": true, "reason": reason}));
        }
        CloseExecStateWritebackPlan::Apply {
            exec_state,
            expected_contents,
            state,
        } => (exec_state, expected_contents, state),
    };
    match plan_tooling::exec_state::writeback_terminal_pinned_if_unchanged(
        &exec_state,
        &expected_contents,
        &state,
        false,
    ) {
        Ok(report) => Ok(json!({
            "file": exec_state.path().display().to_string(),
            "changed": report.changed,
            "followup_commit_required": report.changed,
            "report": serde_json::to_value(&report).unwrap_or(Value::Null),
        })),
        Err(plan_tooling::exec_state::ExecStateError::ExpectedPathChanged { .. }) => {
            Err(CommandError::runtime(
                "record-close-execution-state-writeback-failed",
                "provider issue is closed, but durable execution-state writeback failed: execution-state path changed after preflight",
            ))
        }
        Err(plan_tooling::exec_state::ExecStateError::ExpectedContentsChanged { .. }) => {
            Err(CommandError::runtime(
                "record-close-execution-state-writeback-failed",
                "provider issue is closed, but durable execution-state writeback failed: execution-state contents changed after preflight",
            ))
        }
        Err(err) => Err(CommandError::runtime(
            "record-close-execution-state-writeback-failed",
            format!(
                "provider issue is closed, but durable execution-state writeback failed ({}): {err}",
                err.code()
            ),
        )),
    }
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

    let state_is_closed = audit
        .evidence
        .get("closeout")
        .and_then(|hit| hit.payload.as_ref())
        .and_then(|payload| payload.parse_closeout().ok())
        .is_some_and(|closeout| closeout.final_status.eq_ignore_ascii_case("complete"));

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
            let hints = derive_lint_hints(spec.role, evidence, state_is_closed);
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
    state_is_closed: bool,
) -> crate::lifecycle_vnext::visible_lint::LintHints {
    use crate::lifecycle_record::PayloadRole;
    use crate::lifecycle_vnext::visible_lint::LintHints;

    let mut hints = LintHints::default();
    let payload = evidence.and_then(|ev| ev.payload.as_ref());

    match role {
        PayloadRole::State => {
            hints.state_is_closed = state_is_closed;
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
    dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &crate::commands::tracking::TrackingArgs,
) -> Result<Value, CommandError> {
    use crate::commands::tracking::{TrackingCommand, TrackingRunCommand};
    match &args.command {
        TrackingCommand::Status(status) => run_tracking_status(status),
        TrackingCommand::Run(run) => match &run.command {
            TrackingRunCommand::Init(args) => run_tracking_run_init(repo_override, dry_run, args),
            TrackingRunCommand::Update(args) => run_tracking_run_update(args),
        },
        TrackingCommand::Checkpoint(args) => {
            run_tracking_checkpoint(binary, dry_run, force, repo_override, args)
        }
        TrackingCommand::CloseReady(args) => run_tracking_close_ready(args),
    }
}

/// Normalize a path to an absolute, cwd-independent form so a later
/// `tracking checkpoint` (run from any directory) can still resolve a bundle or
/// execution-state ref recorded at init time. `std::path::absolute` only
/// prepends the current directory for relative inputs — it does not require the
/// path to exist or resolve symlinks. Falls back to the original path when the
/// current directory cannot be read, so a recorded ref is never dropped.
fn absolutize(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn is_safe_repo_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn repository_identity(
    repo: &str,
    repo_provider: Option<&str>,
    repo_host: Option<&str>,
) -> Option<crate::provider::Repo> {
    let qualified = repo.contains("://")
        || repo.contains('@')
        || repo.ends_with(".git")
        || repo.starts_with("local:");
    let parsed = qualified
        .then(|| crate::provider::resolve_repo(Some(repo)).ok())
        .flatten();
    let provider = match repo_provider {
        Some("github") => crate::provider::Provider::GitHub,
        Some("gitlab") => crate::provider::Provider::GitLab,
        Some("local") => crate::provider::Provider::Local,
        Some(_) => return None,
        None => parsed.as_ref()?.provider,
    };
    let slug = parsed
        .as_ref()
        .map(|parsed| parsed.slug.clone())
        .unwrap_or_else(|| repo.trim().trim_end_matches('/').to_string());
    if slug.is_empty() {
        return None;
    }
    Some(crate::provider::Repo {
        provider,
        slug,
        host: repo_host
            .map(|host| crate::provider::canonical_provider_host(provider, host))
            .or_else(|| parsed.and_then(|parsed| parsed.host)),
    })
}

fn repository_slug_matches(
    provider: crate::provider::Provider,
    expected: &str,
    actual: &str,
) -> bool {
    match provider {
        crate::provider::Provider::GitHub => expected.eq_ignore_ascii_case(actual),
        crate::provider::Provider::GitLab | crate::provider::Provider::Local => expected == actual,
    }
}

fn repository_identity_matches(
    expected: &crate::provider::Repo,
    actual: &crate::provider::Repo,
) -> bool {
    expected.provider == actual.provider
        && repository_slug_matches(expected.provider, &expected.slug, &actual.slug)
        && match (expected.host.as_deref(), actual.host.as_deref()) {
            (Some(expected_host), Some(actual_host)) => {
                crate::provider::authorities_equal(expected.provider, expected_host, actual_host)
            }
            (None, None) => true,
            _ => false,
        }
}

fn verified_current_repo_root(
    repo: &str,
    repo_provider: Option<&str>,
    repo_host: Option<&str>,
) -> Option<PathBuf> {
    let expected = repository_identity(repo, repo_provider, repo_host)?;
    if matches!(expected.provider, crate::provider::Provider::Local) {
        return common_git::repo_root().ok().flatten();
    }
    let actual = crate::provider::resolve_repo(None).ok()?;
    if !repository_identity_matches(&expected, &actual) {
        return None;
    }
    common_git::repo_root().ok().flatten()
}

fn verified_current_repo_root_for_identity(
    expected: Option<&crate::provider::Repo>,
    allow_any_git_repo: bool,
) -> Option<PathBuf> {
    let root = common_git::repo_root().ok().flatten()?;
    if allow_any_git_repo {
        return Some(root);
    }
    let actual = crate::provider::resolve_repo(None).ok()?;
    expected
        .is_some_and(|expected| repository_identity_matches(expected, &actual))
        .then_some(root)
}

fn git_output_at(path: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn repository_root_for_confined_path(path: &Path) -> Option<PathBuf> {
    let anchor = if path.is_dir() { path } else { path.parent()? };
    Some(PathBuf::from(git_output_at(
        anchor,
        &["rev-parse", "--show-toplevel"],
    )?))
}

fn verified_recorded_path(
    expected: Option<&crate::provider::Repo>,
    historical_slug: Option<&str>,
    allow_any_git_repo: bool,
    path: &Path,
    want_file: bool,
) -> Option<PathBuf> {
    let link_metadata = std::fs::symlink_metadata(path).ok()?;
    if link_metadata.file_type().is_symlink() {
        return None;
    }
    let anchor = if want_file { path.parent()? } else { path };
    let root = PathBuf::from(git_output_at(anchor, &["rev-parse", "--show-toplevel"])?);
    if allow_any_git_repo {
        return confined_existing_path(&root, path, want_file);
    }
    let remote = git_output_at(&root, &["remote", "get-url", "origin"])?;
    let actual = crate::provider::resolve_repo(Some(&remote)).ok()?;
    let identity_matches = expected
        .is_some_and(|expected| repository_identity_matches(expected, &actual))
        || (expected.is_none()
            && historical_slug.is_some_and(|slug| {
                repository_slug_matches(
                    actual.provider,
                    slug.trim().trim_end_matches('/'),
                    &actual.slug,
                )
            }));
    if !identity_matches {
        return None;
    }
    confined_existing_path(&root, path, want_file)
}

fn repo_relative_identity(path: &Path, repo_root: &Path) -> Option<PathBuf> {
    let canonical_path = fs::canonicalize(absolutize(path)).ok()?;
    let canonical_root = fs::canonicalize(repo_root).ok()?;
    let relative = canonical_path.strip_prefix(canonical_root).ok()?;
    is_safe_repo_relative(relative).then(|| relative.to_path_buf())
}

fn run_tracking_run_init(
    repo_override: Option<&str>,
    dry_run: bool,
    args: &crate::commands::tracking::TrackingRunInitArgs,
) -> Result<Value, CommandError> {
    use crate::runtime_layout;
    use crate::tracking::events::{self, ExecutionEvent, ExecutionEventKind};
    use crate::tracking::run_state::{ExecutionRun, RunPhase, RunRoot, SelectedScope};

    let canonical_linked_pr = args
        .linked_pr
        .as_deref()
        .map(canonical_linked_pr_reference)
        .transpose()?;

    let now = args.now.clone().unwrap_or_else(default_now);
    let run_id = args
        .run_id
        .clone()
        .unwrap_or_else(|| default_run_id(args.issue, &now));
    let requested_is_qualified = checkpoint_repo_arg_is_qualified(&args.provider_repo);
    let global_is_qualified = repo_override.is_some_and(checkpoint_repo_arg_is_qualified);
    let global_repo = repo_override
        .map(|raw| {
            crate::provider::resolve_repo(Some(raw)).map_err(|_| {
                CommandError::usage(
                    "tracking-run-init-repo-identity-invalid",
                    "global repository override is invalid",
                )
            })
        })
        .transpose()?;
    let requested_repo = if !requested_is_qualified && global_is_qualified {
        let global = global_repo.as_ref().expect("qualified override resolved");
        if !repository_slug_matches(global.provider, &global.slug, &args.provider_repo) {
            return Err(CommandError::usage(
                "tracking-run-init-repo-mismatch",
                "global repository override does not match --provider-repo",
            ));
        }
        global.clone()
    } else {
        crate::provider::resolve_repo(Some(&args.provider_repo)).map_err(|_| {
            CommandError::usage(
                "tracking-run-init-repo-identity-invalid",
                "--provider-repo is not a valid repository identity",
            )
        })?
    };
    let repo_slug = requested_repo.slug.clone();
    if let Some(global) = global_repo.as_ref()
        && ((requested_is_qualified && global.provider != requested_repo.provider)
            || !repository_slug_matches(global.provider, &global.slug, &requested_repo.slug))
    {
        return Err(CommandError::usage(
            "tracking-run-init-repo-mismatch",
            "global repository override does not match --provider-repo",
        ));
    }
    let bound_repo = if requested_is_qualified {
        if let Some(global) = global_repo.as_ref()
            && checkpoint_repo_arg_is_qualified(repo_override.unwrap_or_default())
            && !checkpoint_repo_identities_match(&requested_repo, global)
        {
            return Err(CommandError::usage(
                "tracking-run-init-repo-mismatch",
                "global repository override does not match --provider-repo",
            ));
        }
        Some(requested_repo.clone())
    } else if repo_override.is_some_and(checkpoint_repo_arg_is_qualified) {
        global_repo
    } else {
        crate::provider::resolve_repo(None).ok().filter(|current| {
            current.provider == requested_repo.provider
                && repository_slug_matches(requested_repo.provider, &current.slug, &repo_slug)
        })
    };
    let root = RunRoot::new(
        &runtime_layout::repo_slug(&repo_slug),
        args.issue,
        run_id.clone(),
    )
    .map_err(|err| CommandError::runtime("tracking-run-init-layout-failed", err.to_string()))?;
    let mut run = ExecutionRun::new(
        run_id.clone(),
        repo_slug,
        args.issue,
        args.profile.as_str().to_string(),
        RunPhase::Initial,
        now.clone(),
    );
    if let Some(bound) = bound_repo {
        run.repo_provider = Some(bound.provider.as_str().to_string());
        run.repo_host = bound.host;
    }
    let source_repo_root = if args.bundle.is_some() || args.execution_state_file.is_some() {
        Some(
            verified_current_repo_root(
                &run.repo,
                run.repo_provider.as_deref(),
                run.repo_host.as_deref(),
            )
            .ok_or_else(|| {
                CommandError::usage(
                    "tracking-run-init-source-path-invalid",
                    "source paths require a verified matching Git repository",
                )
            })?,
        )
    } else {
        None
    };
    // Persist bundle / execution-state refs as absolute, cwd-independent paths.
    // `tracking run init` resolves a relative `--bundle` against the current
    // directory, but a later `tracking checkpoint` may run from elsewhere; a
    // verbatim relative ref then fails to resolve and the state checkpoint
    // silently degrades to the single-row baseline
    // (graysurf/plan-tracking-testbed#55).
    run.bundle = args
        .bundle
        .as_deref()
        .map(|path| {
            let absolute = absolutize(path);
            confined_existing_path(
                source_repo_root.as_deref().expect("source root"),
                &absolute,
                false,
            )
            .ok_or_else(|| {
                CommandError::usage(
                    "tracking-run-init-source-path-invalid",
                    "--bundle must be a real directory inside the verified repository",
                )
            })
        })
        .transpose()?;
    run.execution_state_file = args
        .execution_state_file
        .as_deref()
        .map(|path| {
            let absolute = absolutize(path);
            confined_existing_path(
                source_repo_root.as_deref().expect("source root"),
                &absolute,
                true,
            )
            .ok_or_else(|| {
                CommandError::usage(
                    "tracking-run-init-source-path-invalid",
                    "--execution-state-file must be a real file inside the verified repository",
                )
            })
        })
        .transpose()?;
    if let Some(repo_root) = source_repo_root {
        run.bundle_repo_relative = args
            .bundle
            .as_deref()
            .and_then(|path| repo_relative_identity(path, &repo_root));
        run.execution_state_repo_relative = args
            .execution_state_file
            .as_deref()
            .and_then(|path| repo_relative_identity(path, &repo_root));
    }
    if args.task.is_some() || args.sprint.is_some() {
        run.selected_scope = Some(SelectedScope {
            sprint: args.sprint,
            task: args.task.clone(),
            title: None,
        });
    }
    run.branch = args.branch.clone();
    run.worktree = args.worktree.clone();
    if let Some(linked) = &canonical_linked_pr {
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
        if !dry_run {
            root.ensure_layout().map_err(|err| {
                CommandError::runtime("tracking-run-init-mkdir-failed", err.to_string())
            })?;
        }
        (root.run_state_path(), root.events_path())
    };

    let run_init_lock = (!dry_run)
        .then(|| TrackingRunUpdateLock::acquire(&run_state_path))
        .transpose()?;
    if !dry_run {
        run_init_lock
            .as_ref()
            .expect("live init owns run-state lock")
            .write_run_state(&run)
            .map_err(|err| {
                CommandError::runtime("tracking-run-init-write-failed", err.to_string())
            })?;
        let event =
            ExecutionEvent::new(run_id.clone(), ExecutionEventKind::RunStarted, now.clone())
                .with_detail(serde_json::json!({
                    "repo": run.repo.as_str(),
                    "repo_provider": run.repo_provider.as_deref(),
                    "repo_host": run.repo_host.as_deref(),
                    "issue": args.issue,
                    "profile": args.profile.as_str(),
                }));
        events::append_event(&events_path, &event).map_err(|err| {
            CommandError::runtime("tracking-run-init-event-append-failed", err.to_string())
        })?;
    }

    Ok(json!({
        "operation": "tracking.run.init",
        "dry_run": dry_run,
        "run_id": run_id,
        "run_state_path": path_text(&run_state_path),
        "events_path": path_text(&events_path),
        "repo": run.repo.as_str(),
        "repo_provider": run.repo_provider.as_deref(),
        "repo_host": run.repo_host.as_deref(),
        "issue": args.issue,
        "profile": args.profile.as_str(),
    }))
}

#[derive(Debug)]
struct TrackingRunUpdateLock {
    run_state: PathBuf,
    target_identity: Option<(u64, u64)>,
    target: Option<plan_tooling::mutation_lock::OwnedFileLock>,
    _path_lock: plan_tooling::mutation_lock::OwnedFileLock,
}

impl TrackingRunUpdateLock {
    fn acquire(run_state: &Path) -> Result<Self, CommandError> {
        let parent = run_state.parent().unwrap_or_else(|| Path::new("."));
        let file_name = run_state
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("run-state.json");
        let path = parent.join(format!(".{file_name}.update.lock"));
        let path_lock = match plan_tooling::mutation_lock::OwnedFileLock::acquire(&path) {
            Ok(lock) => lock,
            Err(plan_tooling::mutation_lock::OwnedFileLockError::Busy) => {
                return Err(CommandError::runtime(
                    "tracking-run-update-lock-busy",
                    format!(
                        "another update is already in progress for {}; retry after it finishes (the kernel releases lock {} when its process exits)",
                        run_state.display(),
                        path.display()
                    ),
                ));
            }
            Err(plan_tooling::mutation_lock::OwnedFileLockError::Failed(err)) => {
                return Err(CommandError::runtime(
                    "tracking-run-update-lock-acquire-failed",
                    format!(
                        "failed to acquire run update lock {}: {err}",
                        path.display()
                    ),
                ));
            }
        };

        let visible = match fs::symlink_metadata(run_state) {
            Ok(metadata) => Some(metadata),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(CommandError::runtime(
                    "tracking-run-update-lock-acquire-failed",
                    format!("failed to inspect {}: {err}", run_state.display()),
                ));
            }
        };
        if visible
            .as_ref()
            .is_some_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
        {
            return Err(Self::unsafe_target(run_state));
        }

        let target = if visible.is_some() {
            match plan_tooling::mutation_lock::OwnedFileLock::acquire_existing(run_state) {
                Ok(lock) => Some(lock),
                Err(plan_tooling::mutation_lock::OwnedFileLockError::Busy) => {
                    return Err(CommandError::runtime(
                        "tracking-run-update-lock-busy",
                        format!(
                            "another update is already in progress for {}; retry after it finishes",
                            run_state.display()
                        ),
                    ));
                }
                Err(plan_tooling::mutation_lock::OwnedFileLockError::Failed(_)) => {
                    return Err(Self::unsafe_target(run_state));
                }
            }
        } else {
            None
        };
        let target_identity = target
            .as_ref()
            .map(|target| target.file().metadata())
            .transpose()
            .map_err(|err| {
                CommandError::runtime(
                    "tracking-run-update-lock-acquire-failed",
                    format!("failed to inspect {}: {err}", run_state.display()),
                )
            })?
            .map(|metadata| {
                if metadata.nlink() != 1 || !metadata.is_file() {
                    return Err(Self::unsafe_target(run_state));
                }
                Ok((metadata.dev(), metadata.ino()))
            })
            .transpose()?;

        Ok(Self {
            run_state: run_state.to_path_buf(),
            target_identity,
            target,
            _path_lock: path_lock,
        })
    }

    fn unsafe_target(run_state: &Path) -> CommandError {
        CommandError::runtime(
            "tracking-run-update-target-unsafe",
            format!(
                "run-state target {} must be a single-link regular file, not a symlink, hard-link alias, or special file",
                run_state.display()
            ),
        )
    }

    fn read_run_state(&self) -> std::io::Result<crate::tracking::run_state::ExecutionRun> {
        let target = self.target.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} does not exist", self.run_state.display()),
            )
        })?;
        crate::tracking::run_state::read_run_state_file(target.file())
    }

    fn verify_target_identity(&self) -> std::io::Result<()> {
        match self.target_identity {
            Some((expected_dev, expected_ino)) => {
                let metadata = fs::symlink_metadata(&self.run_state)?;
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.nlink() != 1
                    || metadata.dev() != expected_dev
                    || metadata.ino() != expected_ino
                {
                    return Err(std::io::Error::other(
                        "run-state target changed after lock acquisition",
                    ));
                }
            }
            None => match fs::symlink_metadata(&self.run_state) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Ok(_) => {
                    return Err(std::io::Error::other(
                        "run-state target appeared after lock acquisition",
                    ));
                }
                Err(err) => return Err(err),
            },
        }
        Ok(())
    }

    fn write_run_state(
        &self,
        run: &crate::tracking::run_state::ExecutionRun,
    ) -> std::io::Result<()> {
        self.verify_target_identity()?;
        crate::tracking::run_state::write_run_state(&self.run_state, run)
    }
}

const TRACKING_RUN_PENDING_UPDATE_SCHEMA: &str = "plan-issue.pending-run-update-event.v1";

#[derive(Debug, Serialize, Deserialize)]
struct PendingRunUpdateEvent {
    schema: String,
    post_state: Value,
    changed: Vec<String>,
    update_id: String,
    event: crate::tracking::events::ExecutionEvent,
}

fn tracking_run_pending_update_path(run_state: &Path) -> PathBuf {
    let parent = run_state.parent().unwrap_or_else(|| Path::new("."));
    let file_name = run_state
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("run-state.json");
    parent.join(format!(".{file_name}.pending-update-event.json"))
}

fn clear_tracking_run_pending_update(path: &Path) -> Result<(), CommandError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(CommandError::runtime(
            "tracking-run-update-pending-cleanup-failed",
            format!("failed to remove {}: {err}", path.display()),
        )),
    }
}

fn recover_tracking_run_pending_event(
    run_state_path: &Path,
    run: &crate::tracking::run_state::ExecutionRun,
) -> Result<Option<Vec<String>>, CommandError> {
    use crate::tracking::events;

    let pending_path = tracking_run_pending_update_path(run_state_path);
    let raw = match fs::read(&pending_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(CommandError::runtime(
                "tracking-run-update-pending-read-failed",
                format!("failed to read {}: {err}", pending_path.display()),
            ));
        }
    };
    let pending: PendingRunUpdateEvent = serde_json::from_slice(&raw).map_err(|err| {
        CommandError::runtime(
            "tracking-run-update-pending-parse-failed",
            format!("failed to parse {}: {err}", pending_path.display()),
        )
    })?;
    if pending.schema != TRACKING_RUN_PENDING_UPDATE_SCHEMA {
        return Err(CommandError::runtime(
            "tracking-run-update-pending-parse-failed",
            format!(
                "unsupported pending update schema `{}` in {}",
                pending.schema,
                pending_path.display()
            ),
        ));
    }
    let current_state = serde_json::to_value(run).map_err(|err| {
        CommandError::runtime("tracking-run-update-compare-failed", err.to_string())
    })?;
    if current_state != pending.post_state {
        clear_tracking_run_pending_update(&pending_path)?;
        return Ok(None);
    }

    let events_path = run_state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("events.jsonl");
    let existing = match events::read_events(&events_path) {
        Ok(existing) => existing,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            return Err(CommandError::runtime(
                "tracking-run-update-event-read-failed",
                err.to_string(),
            ));
        }
    };
    let already_appended = existing.iter().any(|event| {
        event.detail.get("update_id").and_then(Value::as_str) == Some(pending.update_id.as_str())
    });
    if !already_appended {
        events::append_event(&events_path, &pending.event).map_err(|err| {
            CommandError::runtime("tracking-run-update-event-append-failed", err.to_string())
        })?;
    }
    clear_tracking_run_pending_update(&pending_path)?;
    Ok(Some(pending.changed))
}

fn persist_tracking_run_pending_event(
    run_state_path: &Path,
    pending: &PendingRunUpdateEvent,
) -> Result<(), CommandError> {
    let path = tracking_run_pending_update_path(run_state_path);
    let raw = serde_json::to_vec_pretty(pending).map_err(|err| {
        CommandError::runtime(
            "tracking-run-update-pending-serialize-failed",
            err.to_string(),
        )
    })?;
    nils_common::fs::write_atomic(&path, &raw, nils_common::fs::SECRET_FILE_MODE).map_err(|err| {
        CommandError::runtime(
            "tracking-run-update-pending-write-failed",
            format!("failed to write {}: {err}", path.display()),
        )
    })
}

fn validate_tracking_run_update_validation_args(
    args: &crate::commands::tracking::TrackingRunUpdateArgs,
) -> Result<(), CommandError> {
    if args.validation_evidence.is_some() && args.validation_command.is_none() {
        return Err(CommandError::usage(
            "tracking-run-update-validation-evidence-requires-command",
            "--validation-evidence requires a complete --validation-command/--validation-status pair",
        ));
    }
    if args.validation_command.is_some() != args.validation_status.is_some() {
        return Err(CommandError::usage(
            "tracking-run-update-validation-command-status-required",
            "--validation-command and --validation-status must be provided together",
        ));
    }
    if let Some(linked_pr) = args.linked_pr.as_deref() {
        parse_linked_pr_reference_full(linked_pr)?;
    }
    Ok(())
}

fn run_tracking_run_update(
    args: &crate::commands::tracking::TrackingRunUpdateArgs,
) -> Result<Value, CommandError> {
    use crate::tracking::events::{self, ExecutionEvent, ExecutionEventKind};
    use crate::tracking::run_state::{LinkedPr, RunPhase, ValidationCommandRow, ValidationSummary};

    validate_tracking_run_update_validation_args(args)?;
    let run_update_lock = TrackingRunUpdateLock::acquire(&args.run_state)?;
    let mut run = run_update_lock
        .read_run_state()
        .map_err(|err| CommandError::runtime("tracking-run-update-read-failed", err.to_string()))?;
    canonicalize_execution_run_linked_prs(&mut run)?;
    let recovered_changes = recover_tracking_run_pending_event(&args.run_state, &run)?;
    let now = args.now.clone().unwrap_or_else(default_now);
    let requested_phase = args.phase.map(|phase| match phase {
        crate::commands::tracking::RunPhaseArg::Initial => RunPhase::Initial,
        crate::commands::tracking::RunPhaseArg::Implementing => RunPhase::Implementing,
        crate::commands::tracking::RunPhaseArg::Validating => RunPhase::Validating,
        crate::commands::tracking::RunPhaseArg::Reviewing => RunPhase::Reviewing,
        crate::commands::tracking::RunPhaseArg::Blocked => RunPhase::Blocked,
        crate::commands::tracking::RunPhaseArg::ReadyForClose => RunPhase::ReadyForClose,
        crate::commands::tracking::RunPhaseArg::Closed => RunPhase::Closed,
    });
    if matches!(run.phase, RunPhase::Closed)
        && requested_phase.is_some_and(|phase| !matches!(phase, RunPhase::Closed))
    {
        return Err(CommandError::usage(
            "tracking-run-update-closed-transition",
            "a closed run cannot transition to another phase without a governed reopen operation",
        ));
    }
    let resulting_phase = requested_phase.unwrap_or(run.phase);
    if matches!(resulting_phase, RunPhase::Closed) && args.selected_task.is_some() {
        return Err(CommandError::usage(
            "tracking-run-update-closed-selected-task",
            "--selected-task cannot be set while the run phase is closed",
        ));
    }
    let has_ordinary_mutation = args.branch.is_some()
        || args.linked_pr.is_some()
        || args.validation_overall.is_some()
        || args.validation_command.is_some()
        || args.validation_status.is_some()
        || args.validation_evidence.is_some()
        || args.review_decision.is_some()
        || !args.review_lens.is_empty()
        || args.review_outcome_comment.is_some()
        || args.review_findings_file.is_some()
        || args.note.is_some();
    if matches!(run.phase, RunPhase::Closed) && has_ordinary_mutation {
        return Err(CommandError::usage(
            "tracking-run-update-closed-immutable",
            "a closed run is immutable; only `--phase closed` may repair a historical stale selected task",
        ));
    }
    let explicit_closed = matches!(requested_phase, Some(RunPhase::Closed));
    let mut changes = Vec::new();
    if let Some(new_phase) = requested_phase
        && new_phase != run.phase
    {
        run.phase = new_phase;
        changes.push("phase");
    }
    if let Some(task) = &args.selected_task
        && run
            .selected_scope
            .as_ref()
            .and_then(|scope| scope.task.as_deref())
            != Some(task.as_str())
    {
        let mut scope = run.selected_scope.clone().unwrap_or_default();
        scope.task = Some(task.clone());
        run.selected_scope = Some(scope);
        changes.push("selected_task");
    }
    if explicit_closed
        && let Some(scope) = run.selected_scope.as_mut()
        && scope.task.take().is_some()
    {
        changes.push("selected_task");
    }
    if let Some(branch) = &args.branch
        && run.branch.as_ref() != Some(branch)
    {
        run.branch = Some(branch.clone());
        changes.push("branch");
    }
    if let Some(pr) = &args.linked_pr {
        let canonical_pr = canonical_linked_pr_reference(pr)?;
        if run.pr.as_ref().map(|linked| linked.r#ref.as_str()) != Some(canonical_pr.as_str()) {
            run.set_linked_pr(LinkedPr {
                r#ref: canonical_pr,
                url: None,
                status: None,
            });
            changes.push("linked_pr");
        }
    }
    if args.validation_overall.is_some()
        || args.validation_command.is_some()
        || args.validation_status.is_some()
        || args.validation_evidence.is_some()
    {
        let previous = run.validation.clone();
        let mut summary = previous.clone().unwrap_or_else(|| ValidationSummary {
            overall: "pending".to_string(),
            commands: Vec::new(),
            waiver: None,
            evidence_path: None,
        });
        let mut effective_input = false;
        if let Some(overall) = &args.validation_overall {
            summary.overall = overall.clone();
            effective_input = true;
        }
        if let (Some(command), Some(status)) = (&args.validation_command, &args.validation_status) {
            let row = ValidationCommandRow {
                command: command.clone(),
                status: status.clone(),
                evidence: args.validation_evidence.clone(),
            };
            if !summary.commands.iter().any(|existing| {
                existing.command == row.command
                    && existing.status == row.status
                    && existing.evidence == row.evidence
            }) {
                summary.commands.push(row);
            }
            effective_input = true;
        }
        let previous_value = previous
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|err| {
                CommandError::runtime("tracking-run-update-compare-failed", err.to_string())
            })?;
        let next_value = serde_json::to_value(&summary).map_err(|err| {
            CommandError::runtime("tracking-run-update-compare-failed", err.to_string())
        })?;
        if effective_input && previous_value.as_ref() != Some(&next_value) {
            run.validation = Some(summary);
            changes.push("validation");
        }
    }
    if args.review_decision.is_some()
        || !args.review_lens.is_empty()
        || args.review_outcome_comment.is_some()
        || args.review_findings_file.is_some()
    {
        let previous = run.review.clone();
        let mut review = if let Some(existing) = previous.clone() {
            existing
        } else {
            let decision = args.review_decision.clone().ok_or_else(|| {
                CommandError::usage(
                    "tracking-run-update-review-decision-required",
                    "record --review-decision before adding review lenses, findings, or outcome evidence",
                )
            })?;
            crate::tracking::run_state::ReviewSummary {
                decision,
                lenses: Vec::new(),
                findings: Vec::new(),
                findings_disposition: Vec::new(),
                evidence: None,
            }
        };
        if let Some(decision) = &args.review_decision {
            review.decision = decision.clone();
        }
        if !args.review_lens.is_empty() {
            review.lenses = args.review_lens.clone();
        }
        if let Some(outcome) = &args.review_outcome_comment {
            review.evidence = Some(outcome.clone());
        }
        if let Some(findings_file) = &args.review_findings_file {
            review.findings = read_review_findings_file(findings_file)?;
        }
        let previous_value = previous
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|err| {
                CommandError::runtime("tracking-run-update-compare-failed", err.to_string())
            })?;
        let next_value = serde_json::to_value(&review).map_err(|err| {
            CommandError::runtime("tracking-run-update-compare-failed", err.to_string())
        })?;
        if previous_value.as_ref() != Some(&next_value) {
            run.review = Some(review);
            changes.push("review");
        }
    }
    if let Some(note) = &args.note {
        run.notes.push(note.clone());
        changes.push("note");
    }
    if changes.is_empty() {
        return Ok(json!({
            "operation": "tracking.run.update",
            "run_id": run.run_id,
            "phase": run.phase.as_str(),
            "changed": recovered_changes.unwrap_or_default(),
            "updated_at": run.updated_at,
        }));
    }
    run.updated_at = now.clone();

    let events_path = args
        .run_state
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("events.jsonl");
    let update_nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let update_id = format!(
        "{}:{}:{}:{update_nonce}",
        run.run_id,
        now,
        std::process::id()
    );
    let changed = changes
        .iter()
        .map(|change| (*change).to_string())
        .collect::<Vec<_>>();
    let event = ExecutionEvent::new(
        run.run_id.clone(),
        ExecutionEventKind::RunUpdated,
        now.clone(),
    )
    .with_detail(serde_json::json!({
        "changed": changed,
        "update_id": update_id,
    }));
    let post_state = serde_json::to_value(&run).map_err(|err| {
        CommandError::runtime("tracking-run-update-compare-failed", err.to_string())
    })?;
    let pending = PendingRunUpdateEvent {
        schema: TRACKING_RUN_PENDING_UPDATE_SCHEMA.to_string(),
        post_state,
        changed: changed.clone(),
        update_id,
        event,
    };
    persist_tracking_run_pending_event(&args.run_state, &pending)?;

    run_update_lock.write_run_state(&run).map_err(|err| {
        CommandError::runtime("tracking-run-update-write-failed", err.to_string())
    })?;
    events::append_event(&events_path, &pending.event).map_err(|err| {
        CommandError::runtime("tracking-run-update-event-append-failed", err.to_string())
    })?;
    clear_tracking_run_pending_update(&tracking_run_pending_update_path(&args.run_state))?;

    Ok(json!({
        "operation": "tracking.run.update",
        "run_id": run.run_id,
        "phase": run.phase.as_str(),
        "changed": changed,
        "updated_at": run.updated_at,
    }))
}

fn read_review_findings_file(
    path: &Path,
) -> Result<Vec<crate::tracking::run_state::ReviewFindingSummary>, CommandError> {
    let raw = fs::read_to_string(path).map_err(|err| {
        CommandError::runtime(
            "tracking-run-update-review-findings-read-failed",
            err.to_string(),
        )
    })?;
    let findings: Vec<crate::tracking::run_state::ReviewFindingSummary> =
        serde_json::from_str(&raw).map_err(|err| {
            CommandError::usage(
                "tracking-run-update-review-findings-parse-failed",
                err.to_string(),
            )
        })?;
    for finding in &findings {
        if finding.id.trim().is_empty()
            || finding.severity.trim().is_empty()
            || finding.disposition.trim().is_empty()
            || finding.summary.trim().is_empty()
        {
            return Err(CommandError::usage(
                "tracking-run-update-review-findings-empty-field",
                "review finding rows require non-empty id, severity, disposition, and summary",
            ));
        }
    }
    Ok(findings)
}

struct CheckpointTarget {
    repo: crate::provider::Repo,
    issue: u64,
}

fn checkpoint_repo_arg_is_qualified(repo: &str) -> bool {
    repo.contains("://")
        || repo.contains('@')
        || repo.ends_with(".git")
        || repo.starts_with("local:")
}

fn checkpoint_repo_identities_match(
    expected: &crate::provider::Repo,
    actual: &crate::provider::Repo,
) -> bool {
    repository_identity_matches(expected, actual)
}

fn is_default_provider_host(repo: &crate::provider::Repo) -> bool {
    match (repo.provider, repo.host.as_deref()) {
        (crate::provider::Provider::GitHub, Some(host)) => {
            crate::provider::canonical_provider_host(repo.provider, host) == "github.com"
        }
        (crate::provider::Provider::GitLab, Some(host)) => {
            crate::provider::canonical_provider_host(repo.provider, host) == "gitlab.com"
        }
        (crate::provider::Provider::Local, None) => true,
        _ => false,
    }
}

#[derive(Debug)]
enum PersistedCheckpointRepo {
    Bound(crate::provider::Repo),
    Unbound,
}

fn persisted_checkpoint_repo(
    run: &crate::tracking::run_state::ExecutionRun,
) -> Result<PersistedCheckpointRepo, ()> {
    use crate::provider::{Provider, Repo};

    let qualified = checkpoint_repo_arg_is_qualified(&run.repo);
    let parsed = qualified
        .then(|| crate::provider::resolve_repo(Some(&run.repo)).map_err(|_| ()))
        .transpose()?;
    match (run.repo_provider.as_deref(), run.repo_host.as_deref()) {
        (None, None) => Ok(parsed
            .map(PersistedCheckpointRepo::Bound)
            .unwrap_or(PersistedCheckpointRepo::Unbound)),
        (Some("local"), None) => {
            if parsed
                .as_ref()
                .is_some_and(|repo| !matches!(repo.provider, Provider::Local))
            {
                return Err(());
            }
            let slug = parsed
                .as_ref()
                .map(|repo| repo.slug.clone())
                .unwrap_or_else(|| run.repo.trim().trim_start_matches("local:").to_string());
            if slug.is_empty() {
                return Err(());
            }
            Ok(PersistedCheckpointRepo::Bound(Repo {
                provider: Provider::Local,
                slug,
                host: None,
            }))
        }
        (Some(provider), Some(host)) if matches!(provider, "github" | "gitlab") => {
            let provider = if provider == "github" {
                Provider::GitHub
            } else {
                Provider::GitLab
            };
            let slug = parsed
                .as_ref()
                .map(|repo| repo.slug.clone())
                .unwrap_or_else(|| run.repo.trim().trim_end_matches('/').to_string());
            if slug.is_empty() {
                return Err(());
            }
            let host = crate::provider::canonical_provider_host(provider, host);
            let bound = Repo {
                provider,
                slug,
                host: Some(host.clone()),
            };
            let qualified_bound = format!("https://{host}/{}", bound.slug);
            let validated =
                crate::provider::resolve_repo(Some(&qualified_bound)).map_err(|_| ())?;
            if !checkpoint_repo_identities_match(&bound, &validated)
                || parsed
                    .as_ref()
                    .is_some_and(|parsed| !checkpoint_repo_identities_match(&bound, parsed))
            {
                return Err(());
            }
            Ok(PersistedCheckpointRepo::Bound(bound))
        }
        _ => Err(()),
    }
}

#[derive(Debug)]
enum ExplicitCheckpointRepo {
    Bound(crate::provider::Repo),
    Slug(String),
}

fn parse_explicit_checkpoint_repo(raw: &str) -> Result<ExplicitCheckpointRepo, CommandError> {
    if checkpoint_repo_arg_is_qualified(raw) {
        return crate::provider::resolve_repo(Some(raw))
            .map(ExplicitCheckpointRepo::Bound)
            .map_err(|_| {
                CommandError::usage(
                    "tracking-checkpoint-live-repo-identity-invalid",
                    "explicit repository identity is invalid",
                )
            });
    }
    let slug = raw.trim().trim_end_matches('/');
    if slug.is_empty() || !slug.contains('/') {
        return Err(CommandError::usage(
            "tracking-checkpoint-live-repo-identity-invalid",
            "explicit repository identity is invalid",
        ));
    }
    Ok(ExplicitCheckpointRepo::Slug(slug.to_string()))
}

fn explicit_checkpoint_repos_match(
    left: &ExplicitCheckpointRepo,
    right: &ExplicitCheckpointRepo,
) -> bool {
    match (left, right) {
        (ExplicitCheckpointRepo::Bound(left), ExplicitCheckpointRepo::Bound(right)) => {
            checkpoint_repo_identities_match(left, right)
        }
        (ExplicitCheckpointRepo::Bound(bound), ExplicitCheckpointRepo::Slug(slug))
        | (ExplicitCheckpointRepo::Slug(slug), ExplicitCheckpointRepo::Bound(bound)) => {
            repository_slug_matches(bound.provider, &bound.slug, slug)
        }
        (ExplicitCheckpointRepo::Slug(left), ExplicitCheckpointRepo::Slug(right)) => left == right,
    }
}

fn explicit_checkpoint_repo_matches_bound(
    explicit: &ExplicitCheckpointRepo,
    bound: &crate::provider::Repo,
) -> bool {
    match explicit {
        ExplicitCheckpointRepo::Bound(explicit) => {
            checkpoint_repo_identities_match(explicit, bound)
        }
        ExplicitCheckpointRepo::Slug(slug) => {
            repository_slug_matches(bound.provider, slug, &bound.slug)
        }
    }
}

fn resolve_checkpoint_live_target(
    repo_override: Option<&str>,
    args: &crate::commands::tracking::TrackingCheckpointArgs,
    run: &crate::tracking::run_state::ExecutionRun,
) -> Result<CheckpointTarget, CommandError> {
    let issue = match (
        args.issue.filter(|issue| *issue != 0),
        (run.issue != 0).then_some(run.issue),
    ) {
        (Some(explicit), Some(recorded)) if explicit != recorded => {
            return Err(CommandError::usage(
                "tracking-checkpoint-live-target-mismatch",
                format!("explicit issue #{explicit} does not match run-state issue #{recorded}"),
            ));
        }
        (Some(explicit), _) => explicit,
        (None, Some(recorded)) if args.issue.is_none() => recorded,
        _ => {
            return Err(CommandError::usage(
                "tracking-checkpoint-live-missing-issue",
                "a non-zero `--issue <number>` is required because the run-state carries no issue to inherit",
            ));
        }
    };

    let persisted = persisted_checkpoint_repo(run).map_err(|_| {
        CommandError::usage(
            "tracking-checkpoint-live-repo-identity-invalid",
            "persisted repository identity metadata is malformed or contradictory",
        )
    })?;
    let global = repo_override
        .map(parse_explicit_checkpoint_repo)
        .transpose()?;
    let checkpoint = args
        .provider_repo
        .as_deref()
        .map(parse_explicit_checkpoint_repo)
        .transpose()?;
    if let (Some(global), Some(checkpoint)) = (&global, &checkpoint)
        && !explicit_checkpoint_repos_match(global, checkpoint)
    {
        return Err(CommandError::usage(
            "tracking-checkpoint-live-target-mismatch",
            "global and checkpoint repository overrides do not identify the same repository",
        ));
    }
    let repo = match persisted {
        PersistedCheckpointRepo::Bound(persisted) => {
            if global
                .iter()
                .chain(checkpoint.iter())
                .any(|explicit| !explicit_checkpoint_repo_matches_bound(explicit, &persisted))
            {
                return Err(CommandError::usage(
                    "tracking-checkpoint-live-target-mismatch",
                    format!(
                        "explicit repository does not match the persisted {} repository identity for `{}`",
                        persisted.provider, persisted.slug
                    ),
                ));
            }
            let qualified_override_confirms =
                global.iter().chain(checkpoint.iter()).any(|explicit| {
                    matches!(
                        explicit,
                        ExplicitCheckpointRepo::Bound(repo)
                            if checkpoint_repo_identities_match(repo, &persisted)
                    )
                });
            let checkout_confirms = crate::provider::current_remote_matches(&persisted);
            if !is_default_provider_host(&persisted)
                && !qualified_override_confirms
                && !checkout_confirms
            {
                return Err(CommandError::usage(
                    "tracking-checkpoint-live-repo-identity-required",
                    "a persisted self-hosted repository requires a matching checkout or an explicit qualified repository URL for this invocation",
                ));
            }
            persisted
        }
        PersistedCheckpointRepo::Unbound => {
            let resolved = checkpoint
                .as_ref()
                .and_then(|explicit| match explicit {
                    ExplicitCheckpointRepo::Bound(repo) => Some(repo),
                    ExplicitCheckpointRepo::Slug(_) => None,
                })
                .or_else(|| {
                    global.as_ref().and_then(|explicit| match explicit {
                        ExplicitCheckpointRepo::Bound(repo) => Some(repo),
                        ExplicitCheckpointRepo::Slug(_) => None,
                    })
                })
                .ok_or_else(|| {
                    CommandError::usage(
                        "tracking-checkpoint-live-repo-identity-required",
                        "live checkpoint requires a persisted provider/host identity or an explicit qualified repository URL",
                    )
                })?;
            if global
                .iter()
                .chain(checkpoint.iter())
                .any(|explicit| !explicit_checkpoint_repo_matches_bound(explicit, resolved))
            {
                return Err(CommandError::usage(
                    "tracking-checkpoint-live-target-mismatch",
                    "explicit repository overrides do not identify the same repository",
                ));
            }
            let recorded_slug = run.repo.trim().trim_end_matches('/');
            if !recorded_slug.is_empty()
                && !repository_slug_matches(resolved.provider, recorded_slug, &resolved.slug)
            {
                return Err(CommandError::usage(
                    "tracking-checkpoint-live-target-mismatch",
                    "explicit repository does not match the run-state repository slug",
                ));
            }
            resolved.clone()
        }
    };

    Ok(CheckpointTarget { repo, issue })
}

fn run_tracking_checkpoint(
    binary: BinaryFlavor,
    global_dry_run: bool,
    force: bool,
    repo_override: Option<&str>,
    args: &crate::commands::tracking::TrackingCheckpointArgs,
) -> Result<Value, CommandError> {
    use crate::lifecycle_record::{self, PayloadRole};
    use crate::lifecycle_vnext::registry;
    use crate::lifecycle_vnext::visible_lint;
    use crate::tracking::reconcile;

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
    if global_dry_run && args.live {
        return Err(CommandError::usage(
            "tracking-checkpoint-dry-run-live-conflict",
            "global --dry-run cannot be combined with tracking checkpoint --live",
        ));
    }
    if args.live
        && args.fixture.is_none()
        && (args.body_file.is_some() || args.comments_json.is_some())
    {
        return Err(CommandError::usage(
            "tracking-checkpoint-live-offline-evidence-conflict",
            "provider-live checkpoint must fetch issue evidence from its bound target; --body-file and --comments-json are offline-only",
        ));
    }

    // Serialize this command with every run-state writer before taking the
    // authoritative local snapshot. The guard remains alive through provider
    // publication and optional dashboard repair.
    let checkpoint_run_lock = TrackingRunUpdateLock::acquire(&args.run_state)?;

    // Read local run state before resolving any provider target. The persisted
    // repository/issue identity is the command's authority for every live hop.
    let mut run = checkpoint_run_lock.read_run_state().map_err(|err| {
        CommandError::runtime("tracking-checkpoint-run-state-read-failed", err.to_string())
    })?;
    canonicalize_execution_run_linked_prs(&mut run)?;
    let provider_live = args.live && args.fixture.is_none();
    if provider_live {
        ensure_live_binary_for_command(binary, "tracking checkpoint --live", None)?;
    }
    let checkpoint_target = provider_live
        .then(|| resolve_checkpoint_live_target(repo_override, args, &run))
        .transpose()?;
    let checkpoint_repo = match checkpoint_target.as_ref() {
        Some(target) => Some(target.repo.clone()),
        None => match persisted_checkpoint_repo(&run) {
            Ok(PersistedCheckpointRepo::Bound(repo)) => Some(repo),
            Ok(PersistedCheckpointRepo::Unbound) | Err(()) => None,
        },
    };
    let checkpoint_adapter = checkpoint_target
        .as_ref()
        .map(|target| crate::provider::select_adapter(&target.repo, force));
    // Hold the issue-scoped lifecycle lock across local snapshot acquisition,
    // evidence fetch, reconciliation, rendering, posting, and optional dashboard
    // repair. Fixture-live mode uses the same lock lifetime as provider-live
    // mode instead of acquiring it only at the posting hop.
    let _checkpoint_lifecycle_lock = if let Some(target) = checkpoint_target.as_ref() {
        Some(crate::lifecycle_lock::acquire(
            &target.repo,
            target.issue,
            args.profile,
        )?)
    } else if args.live && args.fixture.is_some() {
        args.issue
            .or((run.issue != 0).then_some(run.issue))
            .map(|issue| {
                crate::lifecycle_lock::acquire(
                    &checkpoint_fixture_lock_repo(&run),
                    issue,
                    args.profile,
                )
            })
            .transpose()?
    } else {
        None
    };

    // Resolve, lock, and read the execution-state Markdown before provider
    // access. The owning guard remains alive through publication so every
    // rendered role is derived from one coherent local generation.
    let has_recorded_source = run.bundle.is_some()
        || run.execution_state_file.is_some()
        || run.bundle_repo_relative.is_some()
        || run.execution_state_repo_relative.is_some();
    let mut execution_state_guard = None;
    let snapshot_result = match state_ledger_path(&run, checkpoint_repo.as_ref()) {
        Ok(Some(path)) => {
            let repo_root = repository_root_for_confined_path(&path).ok_or_else(|| {
                CommandError::runtime(
                    "state-ledger-unresolved",
                    "execution-state file is not confined to a repository checkout",
                )
            })?;
            let pinned = plan_tooling::exec_state::PinnedExecutionState::pin(&repo_root, &path)
                .map_err(|err| {
                    let code = if err.kind() == std::io::ErrorKind::InvalidInput {
                        "exec-state-unsafe-file-alias"
                    } else {
                        "exec-state-read-failed"
                    };
                    CommandError::runtime(code, err.to_string())
                })?;
            let guard = plan_tooling::exec_state::ExecutionStateGuard::acquire_pinned(&pinned)
                .map_err(|err| CommandError::runtime(err.code(), err.to_string()))?;
            match execution_state_snapshot_from_guard(path, &guard) {
                Ok(snapshot) => {
                    execution_state_guard = Some(guard);
                    Ok(Some(snapshot))
                }
                Err(_) => Err(StateLedgerResolutionError::Unresolved),
            }
        }
        Ok(None) => Ok(None),
        Err(err) => Err(err),
    };

    // Read provider evidence through the already-bound target and adapter.
    let (body, comments_json) = resolve_checkpoint_inputs(
        args,
        checkpoint_target.as_ref(),
        checkpoint_adapter.as_deref(),
    )?;
    let audit = if let Some(comments) = comments_json.as_deref() {
        Some(
            lifecycle_record::audit_record(body.as_deref(), comments, Some(args.profile))
                .map_err(|err| CommandError::runtime("tracking-checkpoint-audit-failed", err))?,
        )
    } else {
        None
    };

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

    // Payload, visible Markdown, ledger preflight, target scope, and Tracking
    // issue reconciliation all consume the guarded snapshot captured above.
    let mut state_source_failure: Option<&'static str> = None;
    let mut execution_state = match snapshot_result {
        Ok(snapshot) => snapshot,
        Err(err) => {
            if requested_roles.contains(&PayloadRole::State) {
                state_source_failure = Some(err.code());
                blocked.push(json!({
                    "code": err.code(),
                    "role": "state",
                    "message": match err {
                        StateLedgerResolutionError::Unresolved => "run records a bundle / execution-state ref but no safe readable Task Ledger can be resolved; refusing to post a degraded single-row state comment",
                        StateLedgerResolutionError::Ambiguous => "run bundle contains multiple *-execution-state.md candidates; refusing to choose one implicitly",
                    },
                    "suggested_unblock": "restore one unique repository-relative execution-state file or re-run `tracking run init` with an exact --execution-state-file",
                }));
            }
            None
        }
    };
    if requested_roles.contains(&PayloadRole::State) && has_recorded_source {
        match execution_state.as_ref() {
            None if !blocked.iter().any(|entry| {
                matches!(
                    entry["code"].as_str(),
                    Some("state-ledger-unresolved" | "state-ledger-ambiguous")
                )
            }) =>
            {
                state_source_failure = Some("state-ledger-unresolved");
                blocked.push(json!({
                    "code": "state-ledger-unresolved",
                    "role": "state",
                    "message": "run records a bundle / execution-state ref but its Task Ledger could not be resolved; refusing to post a degraded single-row state comment",
                    "suggested_unblock": "restore the recorded execution-state file or re-run `tracking run init` with an exact --execution-state-file",
                }));
            }
            Some(snapshot) => {
                if let Err(message) = &snapshot.rows {
                    state_source_failure = Some("state-ledger-malformed");
                    blocked.push(json!({
                        "code": "state-ledger-malformed",
                        "role": "state",
                        "message": message,
                        "suggested_unblock": "repair the `## Task Ledger` table before posting a state checkpoint",
                    }));
                }
            }
            None => {}
        }
    }

    // Reconcile the durable execution-state `Tracking issue` bullet from the
    // same snapshot. A live self-heal is attempted only when no other blocker
    // exists; after the write, refresh the command snapshot exactly once.
    let mut exec_state_reconcile = json!({"applicable": false});
    let checkpoint_issue = checkpoint_target
        .as_ref()
        .map(|target| target.issue)
        .unwrap_or(run.issue);
    if checkpoint_issue != 0
        && let Some(snapshot) = execution_state.as_ref()
    {
        let issue_class = classify_exec_state_issue(
            plan_tooling::exec_state::tracking_issue_value(&snapshot.raw).as_deref(),
            checkpoint_repo.as_ref(),
            checkpoint_issue,
        );
        let es_path = snapshot.path.clone();
        match issue_class {
            ExecStateIssueClass::Consistent => {
                exec_state_reconcile = json!({"applicable": true, "status": "consistent"});
            }
            ExecStateIssueClass::Mismatch(found) => {
                exec_state_reconcile =
                    json!({"applicable": true, "status": "mismatch", "found": found});
                blocked.push(json!({
                    "code": "execution-state-issue-mismatch",
                    "message": format!(
                        "durable execution-state tracking issue `{found}` does not match checkpoint issue #{}",
                        checkpoint_issue
                    ),
                    "suggested_unblock": format!(
                        "run `plan-tooling exec-state-sync --execution-state {} --issue-url <correct-url>` or fix the file",
                        es_path.display()
                    ),
                }));
            }
            ExecStateIssueClass::Invalid => {
                exec_state_reconcile = json!({"applicable": true, "status": "invalid"});
                blocked.push(json!({
                    "code": "execution-state-issue-invalid",
                    "message": "durable execution-state tracking issue is not a valid issue URL",
                    "suggested_unblock": format!(
                        "run `plan-tooling exec-state-sync --execution-state {} --issue-url <correct-url>` or fix the file",
                        es_path.display()
                    ),
                }));
            }
            ExecStateIssueClass::Missing => {
                let mut healed_url = None;
                let mut heal_failure = None;
                let issue_url = checkpoint_repo
                    .as_ref()
                    .map(|repo| repo.issue_url(checkpoint_issue));
                if args.live
                    && blocked.is_empty()
                    && let (Some(url), Some(guard)) = (issue_url, execution_state_guard.as_ref())
                {
                    match guard.sync_tracking_issue(&url, false) {
                        Ok(_) => {
                            match execution_state_snapshot_from_guard(es_path.clone(), guard) {
                                Ok(refreshed) => {
                                    execution_state = Some(refreshed);
                                    healed_url = Some(url);
                                }
                                Err(err) => {
                                    heal_failure = Some((err.code(), err.to_string()));
                                }
                            }
                        }
                        Err(err) => {
                            heal_failure = Some((err.code(), err.to_string()));
                        }
                    }
                }
                if let Some(url) = healed_url {
                    exec_state_reconcile = json!({
                        "applicable": true,
                        "status": "self-healed",
                        "issue_url": url,
                        "file": es_path.display().to_string(),
                    });
                } else if let Some((code, message)) = heal_failure {
                    exec_state_reconcile = json!({
                        "applicable": true,
                        "status": "self-heal-failed",
                        "file": es_path.display().to_string(),
                    });
                    blocked.push(json!({
                        "code": code,
                        "message": message,
                        "suggested_unblock": format!(
                            "repair {} and retry the live checkpoint",
                            es_path.display()
                        ),
                    }));
                } else {
                    exec_state_reconcile = json!({"applicable": true, "status": "missing"});
                    blocked.push(json!({
                        "code": "execution-state-issue-missing",
                        "message": "durable execution-state has no tracking issue URL; archive discovery would block with no-provider-refs",
                        "suggested_unblock": format!(
                            "run `plan-tooling exec-state-sync --execution-state {} --issue-url <url>`",
                            es_path.display()
                        ),
                    }));
                }
            }
        }
    }

    // Build per-role payloads from run state and render bodies.
    let mut rendered: Vec<Value> = Vec::new();
    let mut visible_failures: Vec<Value> = Vec::new();
    let mut roles_planned: Vec<&'static str> = Vec::new();
    let mut roles_skipped: Vec<Value> = Vec::new();

    for role in &requested_roles {
        let spec = registry::role(*role);
        if matches!(*role, PayloadRole::State)
            && let Some(code) = state_source_failure
        {
            roles_skipped.push(json!({
                "role": spec.marker_role,
                "reason": format!("{code}: recorded execution-state source failed closed"),
            }));
            continue;
        }
        let body_result =
            render_checkpoint_role(*role, &run, args.profile, execution_state.as_ref())?;
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
            checkpoint_target.as_ref(),
            checkpoint_adapter.as_deref(),
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
        "execution_state_reconcile": exec_state_reconcile,
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
/// In **live mode** this reuses the provider target, adapter, and lifecycle
/// lock held by the caller since before the command fetched evidence. It writes
/// each role with `adapter.comment_issue` and optionally repairs the same issue
/// dashboard. On the first per-role failure the function stops, pushes a stable `tracking-checkpoint-live-post-failed` entry into `blocked`,
/// and skips any pending roles plus `--repair-dashboard` so a half-posted issue
/// does not get a stale dashboard rewrite.
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
    target: Option<&CheckpointTarget>,
    adapter: Option<&dyn crate::provider::ProviderAdapter>,
    rendered: &[Value],
    blocked: &mut Vec<Value>,
) -> Result<CheckpointPostSummary, CommandError> {
    // Fixture mode preserves the explicit issue override used by deterministic
    // smoke probes. True provider mode receives an already-validated target.
    let issue_number = match target
        .map(|target| target.issue)
        .or(args.issue)
        .or((run.issue != 0).then_some(run.issue))
    {
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

    let target = target.ok_or_else(|| {
        CommandError::runtime(
            "tracking-checkpoint-live-target-missing",
            "provider checkpoint target was not bound before posting",
        )
    })?;
    let adapter = adapter.ok_or_else(|| {
        CommandError::runtime(
            "tracking-checkpoint-live-adapter-missing",
            "provider checkpoint adapter was not selected before posting",
        )
    })?;
    let issue_url = target.repo.issue_url(target.issue);
    let repo = target.repo.slug.as_str();

    let mut posted: Vec<Value> = Vec::new();
    let mut first_failure: Option<Value> = None;

    for entry in rendered {
        let role = entry["role"].as_str().unwrap_or("unknown");
        let body = entry["body"].as_str().unwrap_or_default();
        let comment_path =
            write_temp_markdown(&format!("tracking-checkpoint-{role}-comment"), body).map_err(
                |err| CommandError::runtime("tracking-checkpoint-comment-write-failed", err),
            )?;
        match adapter.comment_issue(repo, issue_number, &comment_path) {
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
            adapter,
            repo,
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

fn checkpoint_fixture_lock_repo(
    run: &crate::tracking::run_state::ExecutionRun,
) -> crate::provider::Repo {
    let slug = if run.repo.trim().is_empty() {
        "fixture".to_string()
    } else {
        run.repo.clone()
    };
    crate::provider::Repo {
        provider: crate::provider::Provider::Local,
        slug,
        host: None,
    }
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
    execution_state: Option<&ExecutionStateSnapshot>,
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
        PayloadRole::State => state_checkpoint_payload(run, execution_state),
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
        load_state_markdown_summary(execution_state)
    } else {
        None
    };

    let body = lifecycle_record::render_record_post_comment_with_display_mode(
        profile,
        kind,
        payload,
        None,
        summary,
        Some(run.updated_at.as_str()),
        TaskLedgerDisplay::Auto,
        // The checkpoint controller derives live state from run-state, so the
        // visible Execution State header is re-rendered from the payload rather
        // than echoed from the (possibly stale) execution-state.md header
        // (graysurf/plan-tracking-testbed#54 / sympoies/nils-cli#700).
        lifecycle_record::StateHeaderMode::DeriveFromPayload,
    )
    .map_err(|err| CommandError::runtime("tracking-checkpoint-render-failed", err))?;
    Ok(CheckpointRoleResult::Rendered(body))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateLedgerResolutionError {
    Unresolved,
    Ambiguous,
}

impl StateLedgerResolutionError {
    fn code(self) -> &'static str {
        match self {
            Self::Unresolved => "state-ledger-unresolved",
            Self::Ambiguous => "state-ledger-ambiguous",
        }
    }
}

fn recorded_repo_relative(
    stored: Option<&Path>,
    absolute: Option<&Path>,
    worktree: Option<&Path>,
) -> Option<PathBuf> {
    stored
        .filter(|path| is_safe_repo_relative(path))
        .map(Path::to_path_buf)
        .or_else(|| {
            let relative = absolute?.strip_prefix(worktree?).ok()?;
            is_safe_repo_relative(relative).then(|| relative.to_path_buf())
        })
}

fn state_path_repository_binding(
    run: &crate::tracking::run_state::ExecutionRun,
    checkpoint_repo: Option<&crate::provider::Repo>,
) -> (Option<crate::provider::Repo>, bool) {
    let bound = checkpoint_repo
        .cloned()
        .or_else(|| match persisted_checkpoint_repo(run).ok()? {
            PersistedCheckpointRepo::Bound(repo) => Some(repo),
            PersistedCheckpointRepo::Unbound => None,
        });
    let allow_any_git_repo = bound
        .as_ref()
        .is_some_and(|repo| matches!(repo.provider, crate::provider::Provider::Local));
    let expected = bound.filter(|repo| !matches!(repo.provider, crate::provider::Provider::Local));
    (expected, allow_any_git_repo)
}

#[cfg(target_os = "macos")]
fn normalize_platform_path_alias(path: PathBuf) -> PathBuf {
    for alias in ["var", "tmp", "etc"] {
        let source = Path::new("/").join(alias);
        if let Ok(relative) = path.strip_prefix(&source) {
            return Path::new("/private").join(alias).join(relative);
        }
    }
    path
}

#[cfg(not(target_os = "macos"))]
fn normalize_platform_path_alias(path: PathBuf) -> PathBuf {
    path
}

fn has_symlinked_path_component(repo_root: &Path, path: &Path) -> bool {
    // macOS exposes system temporary paths through `/var` while Git commonly
    // reports the same checkout through `/private/var`. Normalize only these
    // platform aliases before checking repository-internal path components.
    let repo_root = normalize_platform_path_alias(absolutize(repo_root));
    let path = normalize_platform_path_alias(absolutize(path));
    let Ok(relative) = path.strip_prefix(&repo_root) else {
        return true;
    };
    let mut current = repo_root;
    for component in relative.components() {
        match component {
            std::path::Component::CurDir => continue,
            std::path::Component::Normal(part) => current.push(part),
            _ => return true,
        }
        if std::fs::symlink_metadata(&current)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return true;
        }
    }
    false
}

fn confined_existing_path(repo_root: &Path, path: &Path, want_file: bool) -> Option<PathBuf> {
    let canonical_root = std::fs::canonicalize(repo_root).ok()?;
    if has_symlinked_path_component(repo_root, path) {
        return None;
    }
    let link_metadata = std::fs::symlink_metadata(path).ok()?;
    if link_metadata.file_type().is_symlink() {
        return None;
    }
    let candidate = std::fs::canonicalize(path).ok()?;
    if !candidate.starts_with(&canonical_root) {
        return None;
    }
    let metadata = std::fs::metadata(&candidate).ok()?;
    ((want_file && metadata.is_file()) || (!want_file && metadata.is_dir())).then_some(candidate)
}

fn relocated_repo_path(repo_root: &Path, relative: &Path, want_file: bool) -> Option<PathBuf> {
    if !is_safe_repo_relative(relative) {
        return None;
    }
    confined_existing_path(repo_root, &repo_root.join(relative), want_file)
}

fn unique_confined_execution_state(
    repo_root: &Path,
    bundle: &Path,
) -> Result<Option<PathBuf>, StateLedgerResolutionError> {
    let entries = std::fs::read_dir(bundle).map_err(|_| StateLedgerResolutionError::Unresolved)?;
    let mut candidate = None;
    for entry in entries {
        let path = entry
            .map_err(|_| StateLedgerResolutionError::Unresolved)?
            .path();
        let matches_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-execution-state.md"));
        if !matches_name {
            continue;
        }
        let confined = confined_existing_path(repo_root, &path, true)
            .ok_or(StateLedgerResolutionError::Unresolved)?;
        if candidate.is_some() {
            return Err(StateLedgerResolutionError::Ambiguous);
        }
        candidate = Some(confined);
    }
    Ok(candidate)
}

/// Resolve the execution-state Markdown that backs the `state` checkpoint
/// ledger. Every candidate must be confined to the verified current checkout.
/// An explicit file identity wins over bundle discovery, including when the
/// original managed-worktree path has to be relocated.
fn state_ledger_path(
    run: &crate::tracking::run_state::ExecutionRun,
    checkpoint_repo: Option<&crate::provider::Repo>,
) -> Result<Option<PathBuf>, StateLedgerResolutionError> {
    let (expected_repo, allow_any_git_repo) = state_path_repository_binding(run, checkpoint_repo);
    let historical_slug = (expected_repo.is_none()
        && !allow_any_git_repo
        && !checkpoint_repo_arg_is_qualified(&run.repo))
    .then_some(run.repo.as_str());
    let has_exact_identity =
        run.execution_state_file.is_some() || run.execution_state_repo_relative.is_some();
    let has_bundle_identity = run.bundle.is_some() || run.bundle_repo_relative.is_some();
    if !has_exact_identity && !has_bundle_identity {
        return Ok(None);
    }

    // An existing exact path is authoritative even when the command runs from
    // another checkout. Verify it against the containing repository's own
    // origin rather than the ambient cwd before considering relocation.
    if let Some(path) = run.execution_state_file.as_deref().and_then(|path| {
        verified_recorded_path(
            expected_repo.as_ref(),
            historical_slug,
            allow_any_git_repo,
            path,
            true,
        )
    }) {
        return Ok(Some(path));
    }

    let repo_root =
        verified_current_repo_root_for_identity(expected_repo.as_ref(), allow_any_git_repo);
    let execution_state_relative = recorded_repo_relative(
        run.execution_state_repo_relative.as_deref(),
        run.execution_state_file.as_deref(),
        run.worktree.as_deref(),
    );
    if let Some(path) = repo_root.as_deref().and_then(|repo_root| {
        execution_state_relative
            .as_deref()
            .and_then(|relative| relocated_repo_path(repo_root, relative, true))
    }) {
        return Ok(Some(path));
    }

    // Once an exact execution-state identity was recorded, failure to resolve
    // that identity is terminal. Never substitute a different unique file from
    // the bundle: doing so can project unrelated task state as terminal proof.
    if has_exact_identity {
        return Err(StateLedgerResolutionError::Unresolved);
    }

    if let Some(bundle) = run.bundle.as_deref().and_then(|path| {
        verified_recorded_path(
            expected_repo.as_ref(),
            historical_slug,
            allow_any_git_repo,
            path,
            false,
        )
    }) {
        return unique_confined_execution_state(
            repository_root_for_confined_path(&bundle)
                .as_deref()
                .ok_or(StateLedgerResolutionError::Unresolved)?,
            &bundle,
        )?
        .map(Some)
        .ok_or(StateLedgerResolutionError::Unresolved);
    }

    let bundle_relative = recorded_repo_relative(
        run.bundle_repo_relative.as_deref(),
        run.bundle.as_deref(),
        run.worktree.as_deref(),
    );
    if let Some(repo_root) = repo_root.as_deref()
        && let Some(relative) = bundle_relative.as_deref()
        && let Some(bundle) = relocated_repo_path(repo_root, relative, false)
    {
        return unique_confined_execution_state(repo_root, &bundle)?
            .map(Some)
            .ok_or(StateLedgerResolutionError::Unresolved);
    }

    Err(StateLedgerResolutionError::Unresolved)
}

#[derive(Debug)]
struct ExecutionStateSnapshot {
    path: PathBuf,
    raw: String,
    rows: Result<Vec<plan_tooling::ledger::LedgerRow>, String>,
}

fn execution_state_snapshot_from_guard(
    path: PathBuf,
    guard: &plan_tooling::exec_state::ExecutionStateGuard,
) -> Result<ExecutionStateSnapshot, plan_tooling::exec_state::ExecStateError> {
    let raw = guard.read_to_string()?;
    let rows = plan_tooling::ledger::read_rows(&raw, &path).map_err(|err| err.to_string());
    Ok(ExecutionStateSnapshot { path, raw, rows })
}

fn execution_state_snapshot(
    run: &crate::tracking::run_state::ExecutionRun,
) -> Result<Option<ExecutionStateSnapshot>, StateLedgerResolutionError> {
    execution_state_snapshot_for_repo(run, None)
}

fn execution_state_snapshot_for_repo(
    run: &crate::tracking::run_state::ExecutionRun,
    checkpoint_repo: Option<&crate::provider::Repo>,
) -> Result<Option<ExecutionStateSnapshot>, StateLedgerResolutionError> {
    let Some(path) = state_ledger_path(run, checkpoint_repo)? else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(&path).map_err(|_| StateLedgerResolutionError::Unresolved)?;
    let rows = plan_tooling::ledger::read_rows(&raw, &path).map_err(|err| err.to_string());
    Ok(Some(ExecutionStateSnapshot { path, raw, rows }))
}

fn load_state_markdown_summary(snapshot: Option<&ExecutionStateSnapshot>) -> Option<&str> {
    let snapshot = snapshot?;
    snapshot
        .raw
        .contains("## Task Ledger")
        .then_some(snapshot.raw.as_str())
}

/// Build the `state` checkpoint payload.
///
/// When the run names a canonical execution-state ledger, the hidden payload's
/// `tasks[]` carries the FULL accumulative per-task table (every task known at
/// post-time), so the provider issue is self-contained per-task history that
/// matches the visible Task Ledger. Falls back to the single-current
/// synthesized baseline when no ledger is recorded or it cannot be parsed.
fn state_checkpoint_payload(
    run: &crate::tracking::run_state::ExecutionRun,
    execution_state: Option<&ExecutionStateSnapshot>,
) -> Value {
    let mut payload = synthesize_state_payload(run);
    if let Some(object) = payload.as_object_mut() {
        if let Some(tasks) = accumulative_state_tasks(execution_state) {
            // The dashboard renders `Current task` / `Next action` straight
            // from this payload, so derive them from the durable ledger rather
            // than the never-advanced `selected_scope`. Otherwise a completed
            // plan still shows the first selected task and an empty next action
            // (graysurf/plan-tracking-testbed#54 / sympoies/nils-cli#700).
            let (current, next_action) = derive_ledger_progress(&tasks, run.phase);
            object.insert("current".to_string(), Value::String(current));
            object.insert("next_action".to_string(), Value::String(next_action));
            object.insert("tasks".to_string(), Value::Array(tasks));
        }
        // `Target scope` is the issue-backed plan scope, not a status word.
        // Prefer the authored `- Target scope:` line from the execution-state
        // header over the synthesized "in-progress" fallback.
        if let Some(scope) = execution_state_target_scope(execution_state) {
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
/// ledger `tasks[]`. Active phases select the first two non-terminal rows.
/// Ready-for-close and closed phases render distinct canonical terminal values.
fn derive_ledger_progress(
    tasks: &[Value],
    phase: crate::tracking::run_state::RunPhase,
) -> (String, String) {
    use crate::tracking::run_state::RunPhase;

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

    if pending.is_empty() && matches!(phase, RunPhase::Closed) {
        return ("complete".to_string(), "none".to_string());
    }
    if pending.is_empty() && matches!(phase, RunPhase::ReadyForClose) {
        return ("complete".to_string(), "closeout".to_string());
    }

    let current = match pending.first() {
        Some(task) => id_of(task),
        None => "complete".to_string(),
    };
    let next_action = if pending.is_empty() {
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
/// header, joining wrapped continuation lines into one string. The controlled
/// `# Execution State: <scope>` heading is a fallback for older bundles that do
/// not carry the bullet.
fn execution_state_target_scope(snapshot: Option<&ExecutionStateSnapshot>) -> Option<String> {
    let raw = snapshot?.raw.as_str();
    let lines: Vec<&str> = raw.lines().collect();
    if let Some(start) = lines
        .iter()
        .position(|line| line.trim_start().starts_with("- Target scope:"))
    {
        let first = lines[start]
            .trim_start()
            .strip_prefix("- Target scope:")?
            .trim();
        let mut scope = first.to_string();
        for line in &lines[start + 1..] {
            let trimmed = line.trim();
            // Stop at a blank line, the next bullet, or the next heading; only
            // an indented wrap of the same bullet continues the value.
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
        if !scope.is_empty() {
            return Some(scope);
        }
    }

    raw.lines().find_map(|line| {
        line.trim()
            .strip_prefix("# Execution State:")
            .map(str::trim)
            .filter(|scope| !scope.is_empty())
            .map(str::to_string)
    })
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
fn accumulative_state_tasks(snapshot: Option<&ExecutionStateSnapshot>) -> Option<Vec<Value>> {
    let rows = snapshot?.rows.as_ref().ok()?;
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
    let (status, current, next_action, fallback_scope) = match run.phase {
        crate::tracking::run_state::RunPhase::Closed => {
            ("complete", "complete", "none", "execution plan")
        }
        crate::tracking::run_state::RunPhase::ReadyForClose => {
            ("complete", "complete", "closeout", "execution plan")
        }
        crate::tracking::run_state::RunPhase::Blocked => (
            "blocked",
            run.selected_scope
                .as_ref()
                .and_then(|scope| scope.task.as_deref())
                .unwrap_or_default(),
            "",
            "in-progress",
        ),
        _ => (
            "in-progress",
            run.selected_scope
                .as_ref()
                .and_then(|scope| scope.task.as_deref())
                .unwrap_or_default(),
            "",
            "in-progress",
        ),
    };
    json!({
        "status": status,
        "target_scope": run
            .selected_scope
            .as_ref()
            .and_then(|s| s.title.clone())
            .unwrap_or_else(|| fallback_scope.to_string()),
        "current": current,
        "next_action": next_action,
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
        "decision": review.decision.clone(),
        "lenses": review.lenses.clone(),
        "findings": review.findings.clone(),
        "outcome_comment_url": review.evidence.clone(),
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
        hints.state_is_closed = matches!(run.phase, crate::tracking::run_state::RunPhase::Closed);
    }
    if matches!(role, PayloadRole::Review) {
        hints.review_has_findings = run
            .review
            .as_ref()
            .map(|review| !review.findings.is_empty())
            .unwrap_or(false);
    }
    hints
}

fn resolve_checkpoint_inputs(
    args: &crate::commands::tracking::TrackingCheckpointArgs,
    target: Option<&CheckpointTarget>,
    adapter: Option<&dyn crate::provider::ProviderAdapter>,
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
    // Auto-fetch provider evidence through the same bound target/adapter that
    // live posting and dashboard repair will reuse. Preserve the older
    // explicit dry-run lookup for callers that are not entering live mode.
    if body.is_none() && comments.is_none() {
        if let (Some(target), Some(adapter)) = (target, adapter) {
            let (body, comments) = adapter
                .issue_evidence(&target.repo.slug, target.issue)
                .map_err(|err| {
                    CommandError::runtime("tracking-checkpoint-evidence-fetch-failed", err)
                })?;
            return Ok((Some(body), Some(comments)));
        }
        if let (Some(repo), Some(issue)) = (args.provider_repo.as_deref(), args.issue) {
            let (body, comments) = auto_fetch_issue_evidence(
                repo,
                issue,
                "tracking-checkpoint-evidence-fetch-failed",
            )?;
            return Ok((Some(body), Some(comments)));
        }
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
    let mut run = match &args.run_state {
        Some(path) => Some(run_state::read_run_state(path).map_err(|err| {
            CommandError::runtime(
                "tracking-close-ready-run-state-read-failed",
                err.to_string(),
            )
        })?),
        None => None,
    };
    if let Some(run) = run.as_mut() {
        canonicalize_execution_run_linked_prs(run)?;
    }

    let reconciled = reconcile::reconcile(audit.as_ref(), run.as_ref());
    let mut blockers: Vec<Value> = Vec::new();
    let execution_state = match run.as_ref().map(execution_state_snapshot) {
        Some(Ok(snapshot)) => snapshot,
        Some(Err(err)) => {
            blockers.push(json!({
                "code": err.code(),
                "message": match err {
                    StateLedgerResolutionError::Unresolved => "run records a bundle / execution-state ref but no safe readable Task Ledger can be resolved",
                    StateLedgerResolutionError::Ambiguous => "run bundle contains multiple *-execution-state.md candidates; refusing to choose one implicitly",
                },
                "suggested_unblock": "restore one unique repository-relative execution-state file or re-run `tracking run init` with an exact --execution-state-file",
            }));
            None
        }
        None => None,
    };
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
    if let Some(run) = run.as_ref() {
        for pr in run.linked_prs.iter().chain(run.pr.iter()) {
            linked_prs.push(pr.r#ref.clone());
        }
    }
    linked_prs = linked_prs
        .iter()
        .map(|linked_pr| canonical_linked_pr_reference(linked_pr))
        .collect::<Result<Vec<_>, _>>()?;
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
    if let lifecycle_record::ApprovalCloseoutOutcome::Blocked { detail, .. } =
        lifecycle_record::evaluate_approval_closeout(args.approval.as_deref())
    {
        blockers.push(json!({
            // Match the public `record close` input guard, which rejects a
            // missing/blank --approval before entering the strict gate.
            "code": "record-close-missing-approval",
            "message": detail,
            "suggested_unblock": "pass explicit approval evidence with --approval before close",
        }));
    }

    // Ledger-rows-pending blocker (Task 1.3): when phase indicates the lane
    // is ready for close or already closed, every Task Ledger row in the
    // bundle must use a terminal status (done/deferred/waived). Silent-skip when
    // or the file cannot be read so older run-states without a bundle field
    // keep working.
    if let Some(run) = run.as_ref() {
        let phase_gates_ledger = matches!(
            run.phase,
            crate::tracking::run_state::RunPhase::ReadyForClose
                | crate::tracking::run_state::RunPhase::Closed
        );
        if phase_gates_ledger && let Some(snapshot) = execution_state.as_ref() {
            match &snapshot.rows {
                Ok(rows) => {
                    for row in rows {
                        if !matches!(row.status.as_str(), "done" | "deferred" | "waived") {
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
                Err(message) => blockers.push(json!({
                    "code": "state-ledger-malformed",
                    "message": message,
                    "suggested_unblock": "repair the `## Task Ledger` table before close",
                })),
            }
        }
    }

    // Task 1.4: the durable execution-state `Tracking issue` bullet must be
    // consistent with run-state before close / archive handoff. Non-mutating
    // probe — block (do not self-heal) and point at the repair command or a
    // live checkpoint.
    if let Some(run) = run.as_ref()
        && run.issue != 0
        && let Some(snapshot) = execution_state.as_ref()
    {
        let expected_repo = match persisted_checkpoint_repo(run) {
            Ok(PersistedCheckpointRepo::Bound(repo)) => Some(repo),
            _ => None,
        };
        match classify_exec_state_issue(
            plan_tooling::exec_state::tracking_issue_value(&snapshot.raw).as_deref(),
            expected_repo.as_ref(),
            run.issue,
        ) {
            ExecStateIssueClass::Consistent => {}
            ExecStateIssueClass::Missing => blockers.push(json!({
                "code": "execution-state-issue-missing",
                "message": "durable execution-state has no tracking issue URL; archive discovery would block with no-provider-refs",
                "suggested_unblock": format!(
                    "run `plan-tooling exec-state-sync --execution-state {} --issue-url <url>` (or `tracking checkpoint --live` to self-heal)",
                    snapshot.path.display()
                ),
            })),
            ExecStateIssueClass::Invalid => blockers.push(json!({
                "code": "execution-state-issue-invalid",
                "message": "durable execution-state tracking issue is not a valid issue URL",
                "suggested_unblock": format!(
                    "run `plan-tooling exec-state-sync --execution-state {} --issue-url <correct-url>`",
                    snapshot.path.display()
                ),
            })),
            ExecStateIssueClass::Mismatch(found) => blockers.push(json!({
                "code": "execution-state-issue-mismatch",
                "message": format!(
                    "durable execution-state tracking issue `{found}` does not match run-state issue #{}",
                    run.issue
                ),
                "suggested_unblock": format!(
                    "run `plan-tooling exec-state-sync --execution-state {} --issue-url <correct-url>`",
                    snapshot.path.display()
                ),
            })),
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

    // Provider state is authoritative for terminal task completion. Evaluate
    // the same rule as `record close`; a local bundle ledger is supplemental
    // evidence and cannot make a pending provider task disappear.
    if let Some(audit) = audit.as_ref()
        && let Some(state) = audit.evidence.get("state")
        && let lifecycle_record::StateCloseoutOutcome::Blocked { code, detail } =
            lifecycle_record::evaluate_state_closeout(Some(state))
    {
        blockers.push(json!({
            "code": code,
            "message": detail,
            "suggested_unblock": "record a complete provider state whose task ledger contains only terminal task statuses",
        }));
    }

    // Provider-latest validation is the canonical closeout result. Reuse the
    // same strict evaluation as `record close` so `partial`, `fail`, and
    // malformed evidence cannot pass this non-mutating probe. Absence is
    // already reported by reconciliation, so evaluate only a present role to
    // avoid a duplicate `validation-missing` blocker.
    if let Some(audit) = audit.as_ref()
        && let Some(validation) = audit.evidence.get("validation")
        && let lifecycle_record::ValidationCloseoutOutcome::Blocked { code, detail } =
            lifecycle_record::evaluate_validation_closeout(Some(validation))
    {
        blockers.push(json!({
            "code": code,
            "message": detail,
            "suggested_unblock": "record provider validation evidence with overall=pass before close",
        }));
    }

    // Strict review-finding gate (plan-tracking-testbed#79): the non-mutating
    // probe must evaluate the SAME review rule as `record close`, otherwise a
    // residual blocker/major finding (or a `request-changes` decision) passes
    // close-ready and is then rejected by the mutating gate with
    // `review-unresolved-findings`, stranding the closeout skill in its close
    // window. A fully missing `review` role is already reported via the
    // reconcile step's `review-missing` blocker above, so only evaluate when
    // the evidence is present to avoid emitting the code twice.
    if let Some(audit) = audit.as_ref()
        && let Some(review) = audit.evidence.get("review")
        && let lifecycle_record::ReviewCloseoutOutcome::Blocked { code, detail } =
            lifecycle_record::evaluate_review_closeout(Some(review))
    {
        // Guidance depends on which strict rule fired, so the operator reads a
        // hint that matches the actual blocker rather than a generic one.
        let suggested_unblock = match code {
            "review-rejected" => {
                "resolve the review decision (request-changes) or malformed payload and record an approving review before close"
            }
            "review-missing" => {
                "record review evidence with a machine-readable payload before close"
            }
            _ => {
                "resolve the blocking review findings (record a review with no residual blocker/major findings) before close"
            }
        };
        blockers.push(json!({
            "code": code,
            "message": detail,
            "suggested_unblock": suggested_unblock,
        }));
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

/// How the durable `Tracking issue` bullet relates to the bound checkpoint.
#[derive(Debug, PartialEq, Eq)]
enum ExecStateIssueClass {
    /// Bullet absent or a `not yet opened`/placeholder value.
    Missing,
    /// Bullet names the same issue.
    Consistent,
    /// Bullet is present but does not contain a valid issue URL.
    Invalid,
    /// Bullet names a different provider, host, repository, or issue.
    Mismatch(String),
}

fn issue_identity_from_url(value: &str) -> Option<CheckpointTarget> {
    if value.contains(['?', '#']) {
        return None;
    }
    if let Some(local) = value.strip_prefix("local://") {
        let marker = "/issues/";
        let idx = local.rfind(marker)?;
        let slug = local[..idx].trim_matches('/');
        let issue_text = local[idx + marker.len()..]
            .strip_suffix('/')
            .unwrap_or(&local[idx + marker.len()..]);
        if slug.is_empty()
            || issue_text.is_empty()
            || !issue_text.chars().all(|ch| ch.is_ascii_digit())
        {
            return None;
        }
        let issue = issue_text.parse().ok()?;
        if issue == 0 {
            return None;
        }
        return Some(CheckpointTarget {
            repo: crate::provider::Repo {
                provider: crate::provider::Provider::Local,
                slug: slug.to_string(),
                host: None,
            },
            issue,
        });
    }

    let without_scheme = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))?;
    let authority = without_scheme.split('/').next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let parsed = common_git::parse_git_remote_url(value)?;
    let (marker, idx) = ["/-/issues/", "/issues/"]
        .into_iter()
        .find_map(|marker| parsed.path.rfind(marker).map(|idx| (marker, idx)))?;
    let slug = parsed.path[..idx].trim_matches('/');
    let issue_text = parsed.path[idx + marker.len()..]
        .strip_suffix('/')
        .unwrap_or(&parsed.path[idx + marker.len()..]);
    if slug.is_empty() || issue_text.is_empty() || !issue_text.chars().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    let issue = issue_text.parse().ok()?;
    if issue == 0 {
        return None;
    }
    let repo = crate::provider::resolve_repo(Some(&format!("https://{authority}/{slug}"))).ok()?;
    Some(CheckpointTarget { repo, issue })
}

/// Classify the current `Tracking issue` value against the expected provider
/// target. Pure: callers own the IO and any self-heal write. A present value
/// that is not a parseable issue URL is invalid and blocks publication.
fn classify_exec_state_issue(
    current: Option<&str>,
    expected_repo: Option<&crate::provider::Repo>,
    expected_issue: u64,
) -> ExecStateIssueClass {
    match current {
        None => ExecStateIssueClass::Missing,
        Some(value) if plan_tooling::exec_state::is_placeholder(value) => {
            ExecStateIssueClass::Missing
        }
        Some(value) => match issue_identity_from_url(value) {
            Some(found)
                if found.issue == expected_issue
                    && expected_repo.is_none_or(|expected| {
                        checkpoint_repo_identities_match(expected, &found.repo)
                    }) =>
            {
                ExecStateIssueClass::Consistent
            }
            Some(found) => ExecStateIssueClass::Mismatch(found.repo.issue_url(found.issue)),
            None => ExecStateIssueClass::Invalid,
        },
    }
}

/// Extract the issue number from a GitHub, GitLab, or local issue URL.
#[cfg(test)]
fn issue_number_from_url(value: &str) -> Option<u64> {
    issue_identity_from_url(value).map(|identity| identity.issue)
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

    let mut run_state_value = match &args.run_state {
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
    if let Some(run) = run_state_value.as_mut() {
        canonicalize_execution_run_linked_prs(run)?;
    }

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
    // Every provider routes through `forge_cli_adapter::ForgeCliAdapter` via
    // [`crate::provider::select_adapter`], so a resolved slug of any provider
    // is handled uniformly by the forge-cli subprocess rail.
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

static TEMP_MARKDOWN_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const STALE_TEMP_MARKDOWN_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
const STALE_TEMP_MARKDOWN_SCAN_LIMIT: usize = 256;

#[derive(Debug)]
struct TempMarkdownDir {
    path: PathBuf,
    descriptor: File,
}

#[derive(Debug)]
struct TempMarkdown {
    provider_path: PathBuf,
    display_path: PathBuf,
    parent: File,
    file_name: OsString,
    file: File,
}

impl PartialEq for TempMarkdown {
    fn eq(&self, other: &Self) -> bool {
        self.display_path == other.display_path
    }
}

impl Eq for TempMarkdown {}

impl TempMarkdown {
    #[cfg(test)]
    fn physical_path(&self) -> &Path {
        &self.display_path
    }
}

impl std::ops::Deref for TempMarkdown {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.provider_path
    }
}

impl AsRef<Path> for TempMarkdown {
    fn as_ref(&self) -> &Path {
        &self.provider_path
    }
}

impl Drop for TempMarkdown {
    fn drop(&mut self) {
        let Ok(name) = CString::new(self.file_name.as_bytes()) else {
            return;
        };
        // SAFETY: the retained parent descriptor and NUL-terminated file name
        // identify the exact directory entry created by this guard.
        let _ = unsafe { libc::unlinkat(self.parent.as_raw_fd(), name.as_ptr(), 0) };
    }
}

fn descriptor_path(descriptor: &File) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from(format!("/proc/self/fd/{}", descriptor.as_raw_fd()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        PathBuf::from(format!("/dev/fd/{}", descriptor.as_raw_fd()))
    }
}

fn ensure_private_temp_markdown_dir(dir: &Path) -> Result<TempMarkdownDir, String> {
    let parent = dir.parent().ok_or_else(|| {
        format!(
            "temporary markdown directory has no parent: {}",
            dir.display()
        )
    })?;
    fs::create_dir_all(parent).map_err(|err| {
        format!(
            "failed to create temporary markdown parent {}: {err}",
            parent.display()
        )
    })?;
    match fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "temporary markdown directory must be a real directory, not a symlink: {}",
                dir.display()
            ));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(dir).map_err(|err| {
                format!(
                    "failed to create temporary markdown directory {}: {err}",
                    dir.display()
                )
            })?;
        }
        Err(err) => {
            return Err(format!(
                "failed to inspect temporary markdown directory {}: {err}",
                dir.display()
            ));
        }
    }
    let descriptor = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)
        .map_err(|err| {
            format!(
                "failed to open temporary markdown directory {}: {err}",
                dir.display()
            )
        })?;
    if !descriptor
        .metadata()
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(format!(
            "temporary markdown directory changed during setup: {}",
            dir.display()
        ));
    }
    // SAFETY: `descriptor` owns a live directory descriptor and 0700 is a
    // valid Unix mode.
    if unsafe { libc::fchmod(descriptor.as_raw_fd(), 0o700) } != 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!(
            "failed to secure temporary markdown directory {}: {err}",
            dir.display()
        ));
    }
    Ok(TempMarkdownDir {
        path: dir.to_path_buf(),
        descriptor,
    })
}

fn temp_markdown_entry_names(dir: &TempMarkdownDir) -> Vec<OsString> {
    let current = CString::new(".").expect("current directory name is NUL-free");
    // Open a fresh stream relative to the retained descriptor. Reopening a
    // directory through `/dev/fd` is not portable to macOS.
    // SAFETY: the parent descriptor is live and `current` is NUL-terminated.
    let descriptor = unsafe {
        libc::openat(
            dir.descriptor.as_raw_fd(),
            current.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Vec::new();
    }
    // SAFETY: `descriptor` uniquely owns a freshly opened directory stream.
    let stream = unsafe { libc::fdopendir(descriptor) };
    if stream.is_null() {
        // SAFETY: `fdopendir` failed, so ownership remains with this function.
        let _ = unsafe { libc::close(descriptor) };
        return Vec::new();
    }

    let mut names = Vec::new();
    while names.len() < STALE_TEMP_MARKDOWN_SCAN_LIMIT {
        // SAFETY: `stream` remains live until `closedir` below.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: `readdir` returned a live entry with a NUL-terminated name.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        names.push(OsStr::from_bytes(name).to_os_string());
    }
    // SAFETY: `stream` is live and owns `descriptor`.
    let _ = unsafe { libc::closedir(stream) };
    names
}

fn cleanup_stale_temp_markdown(dir: &TempMarkdownDir) {
    for file_name in temp_markdown_entry_names(dir) {
        if Path::new(&file_name)
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("md")
        {
            continue;
        }
        let Ok(name) = CString::new(file_name.as_bytes()) else {
            continue;
        };
        // Inspect the entry through the retained directory descriptor without
        // following a symlink or blocking on a non-regular special file.
        // SAFETY: the parent descriptor is live and `name` is NUL-terminated.
        let descriptor = unsafe {
            libc::openat(
                dir.descriptor.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            continue;
        }
        // SAFETY: `descriptor` was returned uniquely by `openat` above.
        let entry = unsafe { File::from_raw_fd(descriptor) };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age >= STALE_TEMP_MARKDOWN_AGE);
        if stale {
            // SAFETY: deletion is relative to the retained directory descriptor,
            // so a pathname swap cannot redirect cleanup outside this directory.
            let _ = unsafe { libc::unlinkat(dir.descriptor.as_raw_fd(), name.as_ptr(), 0) };
        }
    }
}

fn create_temp_markdown_file(dir: &TempMarkdownDir, file_name: &OsStr) -> std::io::Result<File> {
    let name = CString::new(file_name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "temporary markdown file name contains a NUL byte",
        )
    })?;
    // Deliberately omit O_CLOEXEC: provider subprocesses receive a descriptor
    // path for this exact file instead of reopening a swappable directory path.
    // SAFETY: the parent descriptor is live and `name` is NUL-terminated.
    let descriptor = unsafe {
        libc::openat(
            dir.descriptor.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `descriptor` was returned uniquely by openat above.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn write_temp_markdown_in_with<F>(
    dir: &TempMarkdownDir,
    stem: &str,
    content: &str,
    timestamp: u128,
    process_id: u32,
    sequence: &AtomicU64,
    mut write: F,
) -> Result<TempMarkdown, String>
where
    F: FnMut(&mut File, &[u8]) -> std::io::Result<()>,
{
    loop {
        let sequence = sequence.fetch_add(1, Ordering::Relaxed);
        let file_name = OsString::from(format!("{stem}-{timestamp}-{process_id}-{sequence}.md"));
        let display_path = dir.path.join(&file_name);
        match create_temp_markdown_file(dir, &file_name) {
            Ok(file) => {
                let parent = dir.descriptor.try_clone().map_err(|err| {
                    format!(
                        "failed to retain temporary markdown directory {}: {err}",
                        dir.path.display()
                    )
                })?;
                let provider_path = descriptor_path(&file);
                let mut markdown = TempMarkdown {
                    provider_path,
                    display_path: display_path.clone(),
                    parent,
                    file_name,
                    file,
                };
                write(&mut markdown.file, content.as_bytes()).map_err(|err| {
                    format!(
                        "failed to write temporary markdown {}: {err}",
                        display_path.display()
                    )
                })?;
                markdown.file.flush().map_err(|err| {
                    format!(
                        "failed to flush temporary markdown {}: {err}",
                        display_path.display()
                    )
                })?;
                markdown.file.seek(SeekFrom::Start(0)).map_err(|err| {
                    format!(
                        "failed to rewind temporary markdown {}: {err}",
                        display_path.display()
                    )
                })?;
                return Ok(markdown);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(format!(
                    "failed to create temporary markdown {}: {err}",
                    display_path.display()
                ));
            }
        }
    }
}

fn write_temp_markdown_in(
    dir: &TempMarkdownDir,
    stem: &str,
    content: &str,
    timestamp: u128,
    process_id: u32,
    sequence: &AtomicU64,
) -> Result<TempMarkdown, String> {
    write_temp_markdown_in_with(
        dir,
        stem,
        content,
        timestamp,
        process_id,
        sequence,
        |file, bytes| file.write_all(bytes),
    )
}

fn write_temp_markdown(stem: &str, content: &str) -> Result<TempMarkdown, String> {
    let dir = task_spec::state_dir()
        .join("out")
        .join("plan-issue-delivery")
        .join("tmp");
    let dir = ensure_private_temp_markdown_dir(&dir)?;
    cleanup_stale_temp_markdown(&dir);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| format!("failed to compute timestamp: {err}"))?
        .as_nanos();
    write_temp_markdown_in(
        &dir,
        stem,
        content,
        now,
        std::process::id(),
        &TEMP_MARKDOWN_SEQUENCE,
    )
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
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;
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

    fn execution_state_snapshot_for_test(path: &Path) -> ExecutionStateSnapshot {
        let raw = std::fs::read_to_string(path).expect("read execution state");
        let rows = plan_tooling::ledger::read_rows(&raw, path).map_err(|err| err.to_string());
        ExecutionStateSnapshot {
            path: path.to_path_buf(),
            raw,
            rows,
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
        comment_results: Mutex<VecDeque<Result<String, String>>>,
        evidence_results: Mutex<VecDeque<Result<(String, String), String>>>,
        comment_calls: AtomicU64,
    }

    impl MockProviderAdapter {
        fn with_merge(mut self, pr: u64, result: Result<bool, String>) -> Self {
            self.merged.insert(pr, result);
            self
        }

        fn with_comment_result(mut self, result: Result<String, String>) -> Self {
            self.comment_results
                .get_mut()
                .expect("comment result queue")
                .push_back(result);
            self
        }

        fn with_evidence_result(mut self, result: Result<(String, String), String>) -> Self {
            self.evidence_results
                .get_mut()
                .expect("evidence result queue")
                .push_back(result);
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
            self.comment_calls.fetch_add(1, Ordering::SeqCst);
            self.comment_results
                .lock()
                .expect("comment result queue")
                .pop_front()
                .expect("queued comment result")
        }

        fn issue_evidence(&self, _repo: &str, _issue: u64) -> Result<(String, String), String> {
            self.evidence_results
                .lock()
                .expect("evidence result queue")
                .pop_front()
                .expect("queued evidence result")
        }

        fn issue_labels(&self, _repo: &str, _issue: u64) -> Result<Vec<String>, String> {
            unreachable!("issue_labels is not needed in this test")
        }

        fn repository_labels(&self, _repo: &str) -> Result<Vec<String>, String> {
            unreachable!("repository_labels is not needed in this test")
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
        ) -> Result<crate::adapter::PrMergeSummary, String> {
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
        created_body: Mutex<Option<String>>,
    }

    impl RollbackProbeAdapter {
        fn new(close_ok: bool) -> Self {
            Self {
                close_ok,
                closed_issue: AtomicU64::new(0),
                created_body: Mutex::new(None),
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
            body_file: &Path,
            _labels: &[String],
        ) -> Result<(u64, String), String> {
            *self.created_body.lock().unwrap() = Some(
                fs::read_to_string(body_file)
                    .map_err(|error| format!("failed to read issue body: {error}"))?,
            );
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
            self.created_body
                .lock()
                .unwrap()
                .clone()
                .map(|body| (body, comments_envelope(&[])))
                .ok_or_else(|| "issue has not been created".to_string())
        }

        fn issue_labels(&self, _repo: &str, _issue: u64) -> Result<Vec<String>, String> {
            unreachable!("issue_labels is not needed in this test")
        }

        fn repository_labels(&self, _repo: &str) -> Result<Vec<String>, String> {
            unreachable!("repository_labels is not needed in this test")
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
        ) -> Result<crate::adapter::PrMergeSummary, String> {
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
        resume_seed()
    }

    fn record_open_test_repo(slug: &str) -> crate::provider::Repo {
        crate::provider::Repo {
            provider: crate::provider::Provider::GitHub,
            slug: slug.to_string(),
            host: Some("github.com".to_string()),
        }
    }

    #[test]
    fn record_open_finalize_leaves_marker_identified_issue_open_on_source_post_failure() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let adapter = RollbackProbeAdapter::new(true);
        let seed = rollback_probe_seed();
        let body_path = record_open_body_file(state.path(), &seed);
        let repo = record_open_test_repo("owner/marker-source-failure");
        let err = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("an unproven comment post must surface an outcome-unknown error");

        assert_eq!(err.code, "record-open-outcome-unknown");
        assert_eq!(adapter.closed_issue.load(Ordering::SeqCst), 0);
        assert!(
            err.message.contains("durable local intent was retained"),
            "expected a retained-intent note, got: {}",
            err.message
        );
    }

    #[test]
    fn record_open_finalize_does_not_attempt_close_when_source_post_fails() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let adapter = RollbackProbeAdapter::new(false);
        let seed = rollback_probe_seed();
        let body_path = record_open_body_file(state.path(), &seed);
        let repo = record_open_test_repo("owner/no-close-on-source-failure");
        let err = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("an unproven comment post must surface an error");

        assert_eq!(err.code, "record-open-outcome-unknown");
        assert_eq!(adapter.closed_issue.load(Ordering::SeqCst), 0);
        assert!(!err.message.contains("rollback"), "{}", err.message);
    }

    /// Stateful stub adapter for the `record open` auto-detect / resume tests.
    /// Records comment / edit / close calls and serves scripted issue evidence
    /// so detection and attach-missing are exercisable without the live binary.
    #[derive(Default)]
    struct ResumeFakeAdapter {
        open_issues: Vec<u64>,
        evidence: HashMap<u64, (String, String)>,
        create_number: u64,
        create_calls: AtomicU64,
        created_issue: Mutex<Option<(u64, String)>>,
        create_store_then_error: bool,
        create_unproven_error: bool,
        listed_labels: Mutex<Vec<Vec<String>>>,
        hide_posted_comments: bool,
        intent_probe: Option<RecordOpenIntentStore>,
        observed_create_intent: Mutex<Option<bool>>,
        observed_comment_intents: Mutex<Vec<bool>>,
        require_lifecycle_lock_on_evidence: Option<(
            crate::provider::Repo,
            crate::commands::record::RecordProfile,
        )>,
        break_intent_cleanup: bool,
        /// When set, `comment_issue` fails on the Nth call (1-based). Used to
        /// simulate a post failure at a precise point without depending on the
        /// (shared, race-prone) temp markdown file's contents.
        fail_on_nth_comment: Option<usize>,
        store_then_fail_on_nth_comment: Option<usize>,
        comment_attempts: AtomicU64,
        /// Issue numbers for each successful comment post, in call order.
        comment_calls: std::sync::Mutex<Vec<u64>>,
        /// Provider-visible body for each successful comment post, in call order.
        comment_bodies: std::sync::Mutex<Vec<(u64, String)>>,
        edited: std::sync::Mutex<Vec<u64>>,
        closed: std::sync::Mutex<Vec<u64>>,
    }

    impl ProviderAdapter for ResumeFakeAdapter {
        fn issue_body(&self, _repo: &str, issue: u64) -> Result<String, String> {
            self.evidence
                .get(&issue)
                .map(|(body, _)| body.clone())
                .or_else(|| {
                    self.created_issue
                        .lock()
                        .unwrap()
                        .as_ref()
                        .filter(|(created_issue, _)| *created_issue == issue)
                        .map(|(_, body)| body.clone())
                })
                .ok_or_else(|| format!("no scripted evidence for issue {issue}"))
        }

        fn issue_evidence(&self, _repo: &str, issue: u64) -> Result<(String, String), String> {
            if let Some((repo, profile)) = &self.require_lifecycle_lock_on_evidence {
                match crate::lifecycle_lock::acquire(repo, issue, *profile) {
                    Err(error) if error.code == "plan-issue-lifecycle-lock-busy" => {}
                    Ok(_lock) => {
                        return Err(
                            "provider evidence was read before acquiring the lifecycle lock"
                                .to_string(),
                        );
                    }
                    Err(error) => {
                        return Err(format!(
                            "failed to probe lifecycle lock during provider evidence read: {}",
                            error.message
                        ));
                    }
                }
            }
            let (body, comments_json) = self
                .evidence
                .get(&issue)
                .cloned()
                .or_else(|| {
                    self.created_issue
                        .lock()
                        .unwrap()
                        .as_ref()
                        .filter(|(created_issue, _)| *created_issue == issue)
                        .map(|(_, body)| (body.clone(), comments_envelope(&[])))
                })
                .ok_or_else(|| format!("no scripted evidence for issue {issue}"))?;
            let mut comments: Value = serde_json::from_str(&comments_json)
                .map_err(|error| format!("invalid scripted comments: {error}"))?;
            let items = comments
                .get_mut("comments")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| "scripted comments envelope has no comments array".to_string())?;
            if !self.hide_posted_comments {
                for (index, (_, posted_body)) in self
                    .comment_bodies
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(posted_issue, _)| *posted_issue == issue)
                    .enumerate()
                {
                    items.push(json!({
                        "body": posted_body,
                        "url": format!("https://github.com/owner/repo/issues/{issue}#posted-{index}"),
                        "created_at": format!("2026-02-{:02}T00:00:00Z", index + 1),
                    }));
                }
            }
            Ok((body, comments.to_string()))
        }

        fn issue_labels(&self, _repo: &str, _issue: u64) -> Result<Vec<String>, String> {
            unreachable!("issue_labels is not needed in this test")
        }

        fn repository_labels(&self, _repo: &str) -> Result<Vec<String>, String> {
            unreachable!("repository_labels is not needed in this test")
        }

        fn list_open_tracker_issues(
            &self,
            _repo: &str,
            labels: &[String],
        ) -> Result<Vec<u64>, String> {
            self.listed_labels.lock().unwrap().push(labels.to_vec());
            let mut issues = self.open_issues.clone();
            if let Some((issue, _)) = self.created_issue.lock().unwrap().as_ref()
                && !issues.contains(issue)
            {
                issues.push(*issue);
            }
            Ok(issues)
        }

        fn create_issue(
            &self,
            _repo: &str,
            _title: &str,
            body_file: &Path,
            _labels: &[String],
        ) -> Result<(u64, String), String> {
            self.create_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(intent) = &self.intent_probe {
                let observed = matches!(
                    intent.load(),
                    Ok(Some(RecordOpenIntentState::CreateInFlight))
                );
                *self.observed_create_intent.lock().unwrap() = Some(observed);
            }
            if self.create_unproven_error {
                return Err("simulated unproven issue create failure".to_string());
            }
            let body = fs::read_to_string(body_file)
                .map_err(|error| format!("failed to read issue body: {error}"))?;
            *self.created_issue.lock().unwrap() = Some((self.create_number, body));
            if self.create_store_then_error {
                return Err("simulated store-then-error issue create failure".to_string());
            }
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
            if self.break_intent_cleanup
                && let Some(intent) = &self.intent_probe
            {
                fs::remove_file(intent.path()).map_err(|error| {
                    format!("failed to remove intent before cleanup probe: {error}")
                })?;
                fs::create_dir(intent.path()).map_err(|error| {
                    format!("failed to replace intent with cleanup-blocking directory: {error}")
                })?;
            }
            Ok(())
        }

        fn comment_issue(
            &self,
            _repo: &str,
            issue: u64,
            body_file: &Path,
        ) -> Result<String, String> {
            let body = fs::read_to_string(body_file)
                .map_err(|error| format!("failed to read comment body: {error}"))?;
            if let Some(intent) = &self.intent_probe {
                let posted_payload = lifecycle_record::extract_payload(&body).ok();
                let observed = match (intent.load(), posted_payload) {
                    (
                        Ok(Some(RecordOpenIntentState::CommentInFlight {
                            issue: pending_issue,
                            role,
                            expected_payload,
                            ..
                        })),
                        Some(posted_payload),
                    ) => {
                        pending_issue == issue
                            && role == posted_payload.role
                            && expected_payload.semantically_matches(&posted_payload)
                    }
                    _ => false,
                };
                self.observed_comment_intents.lock().unwrap().push(observed);
            }
            let nth = self.comment_attempts.fetch_add(1, Ordering::SeqCst) as usize + 1;
            if self.store_then_fail_on_nth_comment == Some(nth) {
                self.comment_bodies.lock().unwrap().push((issue, body));
                return Err(format!("simulated store-then-error on comment call {nth}"));
            }
            if self.fail_on_nth_comment == Some(nth) {
                return Err(format!("simulated comment post failure on call {nth}"));
            }
            self.comment_calls.lock().unwrap().push(issue);
            self.comment_bodies.lock().unwrap().push((issue, body));
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
        ) -> Result<crate::adapter::PrMergeSummary, String> {
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
        let profile = crate::commands::record::RecordProfile::Tracking;
        let source_path = "docs/plans/x/x-discussion-source.md".to_string();
        let plan_path = "docs/plans/x/x-plan.md".to_string();
        let commit = "abc123".to_string();
        let source_body = source_identity_comment(&source_path, &commit);
        let plan_body = lifecycle_record::render_record_snapshot_comment(
            profile,
            crate::commands::record::LifecycleCommentKind::Plan,
            &lifecycle_record::SnapshotData {
                path: plan_path.clone(),
                commit: commit.clone(),
                title: Some("Resume Plan".to_string()),
                summary: None,
            },
            "plan content",
            None,
        )
        .expect("render plan comment");
        let state_body = lifecycle_record::render_record_post_comment(
            profile,
            crate::commands::record::LifecycleCommentKind::State,
            json!({
                "status": "in-progress",
                "target_scope": "Resume Plan",
                "current": "1.1",
                "next_action": "Continue",
                "tasks": [],
                "prs": [],
                "blockers": [],
                "links": {}
            }),
            Some("initial state"),
            None,
        )
        .expect("render state comment");
        RecordSeed {
            plan_title: "Resume Plan".to_string(),
            source_path,
            plan_path,
            source_commit: commit.clone(),
            plan_commit: commit,
            source_body,
            plan_body,
            state_body,
        }
    }

    fn record_identity_body(seed: &RecordSeed) -> String {
        format!(
            "{}\n\n## Current Dashboard\n",
            lifecycle_record::render_record_identity_marker(
                crate::commands::record::RecordProfile::Tracking,
                &seed.source_path,
                &seed.source_commit,
            )
        )
    }

    fn isolate_record_open_state(lock: &GlobalStateLock) -> (TempDir, EnvGuard) {
        crate::state::set_state_dir_override(None);
        let state = TempDir::new().expect("record-open state");
        let env = EnvGuard::set(lock, "PLAN_ISSUE_HOME", &state.path().to_string_lossy());
        (state, env)
    }

    fn record_open_body_file(dir: &Path, seed: &RecordSeed) -> PathBuf {
        let path = dir.join("record-open-body.md");
        fs::write(&path, record_identity_body(seed)).expect("record-open body");
        path
    }

    #[test]
    fn bundle_identity_is_repo_relative_from_any_launch_directory() {
        let lock = GlobalStateLock::new();
        let repo = init_repo_with(InitRepoOptions::new().with_initial_commit());
        let nested = repo.path().join("docs/plans/demo");
        fs::create_dir_all(&nested).expect("nested bundle directory");
        let source = nested.join("demo-discussion-source.md");
        fs::write(&source, "# Source\n").expect("source file");

        let from_root = {
            let _cwd = CwdGuard::set(&lock, repo.path()).expect("repo root cwd");
            relative_repo_path(&source).expect("repository-relative source identity")
        };
        let from_nested = {
            let _cwd = CwdGuard::set(&lock, &nested).expect("nested cwd");
            relative_repo_path(&source).expect("repository-relative source identity")
        };

        assert_eq!(from_root, "docs/plans/demo/demo-discussion-source.md");
        assert_eq!(from_nested, from_root);
    }

    #[test]
    fn detect_resumable_tracker_source_comment_only_requires_repair() {
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

        let error = detect_resumable_tracker(
            &adapter,
            "owner/repo",
            crate::commands::record::RecordProfile::Tracking,
            &identity,
        )
        .expect_err("historical source-comment identity must not be auto-resumable");
        assert_eq!(error.code, "record-open-identity-repair-required");
        assert!(
            error.message.contains("issue-body identity marker"),
            "{error:?}"
        );
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn detect_resumable_tracker_matches_issue_body_identity_before_comments() {
        let marker = lifecycle_record::render_record_identity_marker(
            crate::commands::record::RecordProfile::Tracking,
            "docs/plans/x/x-discussion-source.md",
            "abc123",
        );
        let mut evidence = HashMap::new();
        evidence.insert(
            7,
            (
                format!("{marker}\n\n## Current Dashboard\n"),
                comments_envelope(&[]),
            ),
        );
        let adapter = ResumeFakeAdapter {
            open_issues: vec![7],
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
            crate::commands::record::RecordProfile::Tracking,
            &identity,
        )
        .expect("body marker detection must not error");
        assert_eq!(found, Some(7));
    }

    #[test]
    fn detect_resumable_tracker_scans_all_open_issues_regardless_requested_labels() {
        let seed = resume_seed();
        let mut evidence = HashMap::new();
        evidence.insert(7, (record_identity_body(&seed), comments_envelope(&[])));
        let adapter = ResumeFakeAdapter {
            open_issues: vec![7],
            evidence,
            ..Default::default()
        };
        let identity = BundleIdentity {
            source_path: seed.source_path.clone(),
            source_commit: seed.source_commit.clone(),
        };

        let found = detect_resumable_tracker(
            &adapter,
            "owner/repo",
            crate::commands::record::RecordProfile::Tracking,
            &identity,
        )
        .expect("dedup scan must not depend on creation labels");

        assert_eq!(found, Some(7));
        assert_eq!(
            *adapter.listed_labels.lock().unwrap(),
            vec![Vec::<String>::new()],
            "identity discovery must broad-scan all open issues"
        );
    }

    #[test]
    fn detect_resumable_tracker_refuses_multiple_matching_issues() {
        let marker = lifecycle_record::render_record_identity_marker(
            crate::commands::record::RecordProfile::Tracking,
            "docs/plans/x/x-discussion-source.md",
            "abc123",
        );
        let source = source_identity_comment("docs/plans/x/x-discussion-source.md", "abc123");
        let mut evidence = HashMap::new();
        evidence.insert(
            2,
            (
                format!("{marker}\n\n## Current Dashboard\n"),
                comments_envelope(&[]),
            ),
        );
        evidence.insert(
            7,
            ("historical body".to_string(), comments_envelope(&[&source])),
        );
        let adapter = ResumeFakeAdapter {
            open_issues: vec![7, 2, 7],
            evidence,
            ..Default::default()
        };
        let identity = BundleIdentity {
            source_path: "docs/plans/x/x-discussion-source.md".to_string(),
            source_commit: "abc123".to_string(),
        };

        let error = detect_resumable_tracker(
            &adapter,
            "owner/repo",
            crate::commands::record::RecordProfile::Tracking,
            &identity,
        )
        .expect_err("trusted plus historical matching trackers must fail closed");
        assert_eq!(error.code, "record-open-identity-repair-required");
        assert!(error.message.contains("2, 7"), "{error:?}");
    }

    #[test]
    fn detect_resumable_tracker_rejects_malformed_or_duplicate_body_markers() {
        let valid = lifecycle_record::render_record_identity_marker(
            crate::commands::record::RecordProfile::Tracking,
            "docs/plans/x/x-discussion-source.md",
            "abc123",
        );
        let identity = BundleIdentity {
            source_path: "docs/plans/x/x-discussion-source.md".to_string(),
            source_commit: "abc123".to_string(),
        };

        for (case, body) in [
            (
                "malformed",
                "<!-- plan-issue-record-identity:v1:hex:not-hex -->".to_string(),
            ),
            ("duplicate", format!("{valid}\n{valid}")),
        ] {
            let mut evidence = HashMap::new();
            evidence.insert(7, (body, comments_envelope(&[])));
            let adapter = ResumeFakeAdapter {
                open_issues: vec![7],
                evidence,
                ..Default::default()
            };

            let error = detect_resumable_tracker(
                &adapter,
                "owner/repo",
                crate::commands::record::RecordProfile::Tracking,
                &identity,
            )
            .expect_err("invalid candidate body marker must fail closed");
            assert_eq!(
                error.code, "record-open-identity-repair-required",
                "{case}: {error:?}"
            );
        }
    }

    #[test]
    fn detect_resumable_tracker_returns_none_when_no_identity_matches() {
        let other = source_identity_comment("docs/plans/y/y-discussion-source.md", "zzz999");
        let other_marker = lifecycle_record::render_record_identity_marker(
            crate::commands::record::RecordProfile::Tracking,
            "docs/plans/y/y-discussion-source.md",
            "zzz999",
        );
        let mut evidence = HashMap::new();
        evidence.insert(
            2,
            (
                format!("{other_marker}\n\n## Other tracker\n"),
                comments_envelope(&[&other]),
            ),
        );
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
            crate::commands::record::RecordProfile::Tracking,
            &identity,
        )
        .expect("detection must not error");
        assert!(found.is_none(), "no bundle identity matches, expected None");
    }

    #[test]
    fn detect_resumable_tracker_propagates_candidate_evidence_read_failures() {
        let adapter = ResumeFakeAdapter {
            open_issues: vec![7],
            ..Default::default()
        };
        let identity = BundleIdentity {
            source_path: "docs/plans/x/x-discussion-source.md".to_string(),
            source_commit: "abc123".to_string(),
        };

        let error = detect_resumable_tracker(
            &adapter,
            "owner/repo",
            crate::commands::record::RecordProfile::Tracking,
            &identity,
        )
        .expect_err("provider read failure must stop duplicate tracker creation");

        assert_eq!(error.code, "record-open-evidence-read-failed");
        assert!(error.message.contains("candidate issue 7"), "{error:?}");
    }

    #[test]
    fn record_open_resume_attaches_only_missing_roles() {
        let state_lock = GlobalStateLock::new();
        let (_state, _state_env) = isolate_record_open_state(&state_lock);
        let seed = resume_seed();
        let wanted = seed.source_body.clone();
        let comments_json = comments_envelope(&[&wanted]);
        let identity_body = record_identity_body(&seed);
        let audit = lifecycle_record::audit_record(
            Some(&identity_body),
            &comments_json,
            Some(crate::commands::record::RecordProfile::Tracking),
        )
        .expect("audit");
        // Sanity: source present, plan + state missing.
        assert!(audit.evidence.contains_key("source"));
        assert!(audit.missing_required.iter().any(|c| c == "plan-missing"));

        let mut evidence = HashMap::new();
        evidence.insert(7, (identity_body, comments_json));
        let adapter = ResumeFakeAdapter {
            evidence,
            ..Default::default()
        };
        let identity = BundleIdentity {
            source_path: seed.source_path.clone(),
            source_commit: seed.source_commit.clone(),
        };
        let repo = record_open_test_repo("owner/resume-missing-roles");

        let result = record_open_resume(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed,
            7,
            "https://github.com/owner/repo/issues/7",
            &identity,
            "binary",
        )
        .expect("resume must succeed");

        assert_eq!(result["mode"], "resumed");
        // The result's `attached` carries the role identity (source is skipped,
        // only plan + state are posted); the call log confirms exactly two posts
        // to issue 7.
        assert_eq!(result["attached"], json!(["plan", "state"]));
        assert_eq!(*adapter.comment_calls.lock().unwrap(), vec![7, 7]);
        let (body, comments_json) = adapter
            .issue_evidence("owner/repo", 7)
            .expect("provider-visible posted comments");
        let converged = lifecycle_record::audit_record(
            Some(&body),
            &comments_json,
            Some(crate::commands::record::RecordProfile::Tracking),
        )
        .expect("audit provider-visible comments");
        assert!(
            converged.missing_required.is_empty(),
            "missing={:?} evidence={:?}",
            converged.missing_required,
            converged.evidence.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            converged
                .evidence
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["plan", "source", "state"]
        );
        assert_eq!(*adapter.edited.lock().unwrap(), vec![7]);
        assert!(adapter.closed.lock().unwrap().is_empty());
    }

    #[test]
    fn record_open_resume_posts_only_state_for_source_plan_partial_tracker() {
        let state_lock = GlobalStateLock::new();
        let (_state, _state_env) = isolate_record_open_state(&state_lock);
        let seed = resume_seed();
        let comments_json = comments_envelope(&[&seed.source_body, &seed.plan_body]);
        let identity_body = record_identity_body(&seed);
        let audit = lifecycle_record::audit_record(
            Some(&identity_body),
            &comments_json,
            Some(crate::commands::record::RecordProfile::Tracking),
        )
        .expect("audit");
        assert_eq!(audit.missing_required, vec!["state-missing"]);
        let mut evidence = HashMap::new();
        evidence.insert(7, (identity_body, comments_json));
        let adapter = ResumeFakeAdapter {
            evidence,
            ..Default::default()
        };
        let identity = BundleIdentity {
            source_path: seed.source_path.clone(),
            source_commit: seed.source_commit.clone(),
        };
        let repo = record_open_test_repo("owner/resume-state-only");

        let result = record_open_resume(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed,
            7,
            "https://github.com/owner/repo/issues/7",
            &identity,
            "binary",
        )
        .expect("resume must succeed");

        assert_eq!(result["attached"], json!(["state"]));
        let bodies = adapter.comment_bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        let posted = comments_envelope(&[bodies[0].1.as_str()]);
        let posted_audit = lifecycle_record::audit_record(
            None,
            &posted,
            Some(crate::commands::record::RecordProfile::Tracking),
        )
        .expect("audit posted state body");
        assert!(posted_audit.evidence.contains_key("state"));
        assert!(!posted_audit.evidence.contains_key("source"));
        assert!(!posted_audit.evidence.contains_key("plan"));
    }

    #[test]
    fn record_open_resume_converges_dashboard_when_all_roles_present() {
        let state_lock = GlobalStateLock::new();
        let (_state, _state_env) = isolate_record_open_state(&state_lock);
        let seed = resume_seed();
        let identity_body = record_identity_body(&seed);
        let source = seed.source_body.clone();
        let plan = v2_marker_comment("plan");
        let state = v2_marker_comment("state");
        let comments_json = comments_envelope(&[&source, &plan, &state]);
        let audit = lifecycle_record::audit_record(
            Some(&identity_body),
            &comments_json,
            Some(crate::commands::record::RecordProfile::Tracking),
        )
        .expect("audit");
        assert!(
            audit.missing_required.is_empty(),
            "fixture must have all required roles present, got {:?}",
            audit.missing_required
        );

        let mut evidence = HashMap::new();
        evidence.insert(7, (identity_body, comments_json));
        let adapter = ResumeFakeAdapter {
            evidence,
            ..Default::default()
        };
        let identity = BundleIdentity {
            source_path: seed.source_path.clone(),
            source_commit: seed.source_commit.clone(),
        };
        let repo = record_open_test_repo("owner/resume-complete");

        let result = record_open_resume(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed,
            7,
            "https://github.com/owner/repo/issues/7",
            &identity,
            "binary",
        )
        .expect("resume must succeed");

        assert_eq!(result["mode"], "already-open");
        assert!(adapter.comment_calls.lock().unwrap().is_empty());
        assert_eq!(*adapter.edited.lock().unwrap(), vec![7]);
    }

    #[test]
    fn record_open_resume_revalidates_direct_body_marker_after_issue_lock() {
        let state_lock = GlobalStateLock::new();
        let (_state, _state_env) = isolate_record_open_state(&state_lock);
        let seed = resume_seed();
        let comments_json = comments_envelope(&[&seed.source_body]);
        let mut evidence = HashMap::new();
        evidence.insert(7, ("body marker removed".to_string(), comments_json));
        let repo = record_open_test_repo("owner/resume-revalidation");
        let adapter = ResumeFakeAdapter {
            evidence,
            require_lifecycle_lock_on_evidence: Some((
                repo.clone(),
                crate::commands::record::RecordProfile::Tracking,
            )),
            ..Default::default()
        };
        let identity = BundleIdentity {
            source_path: seed.source_path.clone(),
            source_commit: seed.source_commit.clone(),
        };

        let error = record_open_resume(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed,
            7,
            "https://github.com/owner/repo/issues/7",
            &identity,
            "binary",
        )
        .expect_err("historical source-comment fallback must not pass resume revalidation");

        assert_eq!(error.code, "record-open-identity-changed");
        assert!(adapter.comment_calls.lock().unwrap().is_empty());
        assert!(adapter.edited.lock().unwrap().is_empty());
    }

    #[test]
    fn record_open_resume_rejects_post_lock_historical_identity_conflict() {
        let state_lock = GlobalStateLock::new();
        let (_state, _state_env) = isolate_record_open_state(&state_lock);
        let seed = resume_seed();
        let conflicting_source =
            source_identity_comment("docs/plans/other/source.md", "other-commit");
        let mut evidence = HashMap::new();
        evidence.insert(
            7,
            (
                record_identity_body(&seed),
                comments_envelope(&[&conflicting_source]),
            ),
        );
        let adapter = ResumeFakeAdapter {
            evidence,
            ..Default::default()
        };
        let identity = BundleIdentity {
            source_path: seed.source_path.clone(),
            source_commit: seed.source_commit.clone(),
        };
        let repo = record_open_test_repo("owner/resume-historical-conflict");
        let issue_url = repo.issue_url(7);

        let error = record_open_resume(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed,
            7,
            &issue_url,
            &identity,
            "binary",
        )
        .expect_err("trusted and historical identity conflict must fail closed");

        assert_eq!(error.code, "record-open-identity-repair-required");
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 0);
        assert!(adapter.edited.lock().unwrap().is_empty());
    }

    #[test]
    fn record_open_unproven_comment_error_persists_intent_and_retry_does_not_repost() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let adapter = ResumeFakeAdapter {
            create_number: 1,
            fail_on_nth_comment: Some(2),
            ..Default::default()
        };
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/unproven-comment");
        let body_path = record_open_body_file(state.path(), &seed);

        let first = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("unproven plan comment outcome must stop");
        assert_eq!(first.code, "record-open-outcome-unknown");
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(*adapter.comment_calls.lock().unwrap(), vec![1]);

        let intent = RecordOpenIntentStore::new(
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed.source_path,
            &seed.source_commit,
        );
        assert!(matches!(
            intent.load().expect("persisted intent"),
            Some(RecordOpenIntentState::CommentInFlight {
                role: lifecycle_record::PayloadRole::Plan,
                ..
            })
        ));

        let retry = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("identical retry must remain outcome-unknown without reposting");
        assert_eq!(retry.code, "record-open-outcome-unknown");
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn record_open_success_without_visible_comment_retains_intent_and_never_reposts() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let adapter = ResumeFakeAdapter {
            create_number: 7,
            hide_posted_comments: true,
            ..Default::default()
        };
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/success-with-stale-readback");
        let body_path = record_open_body_file(state.path(), &seed);

        let first = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("a success response without semantic evidence remains uncertain");
        assert_eq!(first.code, "record-open-outcome-unknown");
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 1);

        let intent = RecordOpenIntentStore::new(
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed.source_path,
            &seed.source_commit,
        );
        assert!(matches!(
            intent.load().expect("pending source intent"),
            Some(RecordOpenIntentState::CommentInFlight {
                role: lifecycle_record::PayloadRole::Source,
                ..
            })
        ));

        let retry = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("retry must reconcile the retained source intent without reposting");
        assert_eq!(retry.code, "record-open-outcome-unknown");
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn record_open_pending_comment_read_uncertainty_stays_outcome_unknown_without_repost() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let adapter = ResumeFakeAdapter::default();
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/pending-comment-read-uncertainty");
        let body_path = record_open_body_file(state.path(), &seed);
        let expected_payload =
            lifecycle_record::extract_payload(&seed.source_body).expect("rendered source payload");
        let intent = RecordOpenIntentStore::new(
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed.source_path,
            &seed.source_commit,
        );
        intent
            .persist_comment_in_flight(7, &expected_payload)
            .expect("persist pending source comment");

        let error = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("uncertain readback must retain the pending comment intent");

        assert_eq!(error.code, "record-open-outcome-unknown");
        assert!(error.message.contains("source comment"), "{error:?}");
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 0);
        assert!(matches!(
            intent.load().expect("retained pending intent"),
            Some(RecordOpenIntentState::CommentInFlight {
                role: lifecycle_record::PayloadRole::Source,
                ..
            })
        ));
    }

    #[test]
    fn record_open_corrupt_intent_stops_before_provider_writes() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let adapter = ResumeFakeAdapter::default();
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/corrupt-intent");
        let body_path = record_open_body_file(state.path(), &seed);
        let intent = RecordOpenIntentStore::new(
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed.source_path,
            &seed.source_commit,
        );
        fs::create_dir_all(intent.path().parent().expect("intent parent"))
            .expect("create intent parent");
        fs::write(intent.path(), b"{corrupt").expect("write corrupt intent");

        let error = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("corrupt journal must fail closed");

        assert_eq!(error.code, "record-open-intent-invalid");
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 0);
        assert!(adapter.edited.lock().unwrap().is_empty());
    }

    #[test]
    fn record_open_create_store_then_error_recovers_without_second_create() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let adapter = ResumeFakeAdapter {
            create_number: 7,
            create_store_then_error: true,
            ..Default::default()
        };
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/create-store-then-error");
        let body_path = record_open_body_file(state.path(), &seed);

        let result = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &["requested-label".to_string()],
            &seed,
            "binary",
        )
        .expect("visible exact identity marker must prove the ambiguous create");

        assert_eq!(result["issue"]["number"], 7);
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 3);
        let intent = RecordOpenIntentStore::new(
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed.source_path,
            &seed.source_commit,
        );
        assert!(intent.load().expect("intent cleared").is_none());
    }

    #[test]
    fn record_open_unproven_create_error_persists_intent_and_retry_does_not_create() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let adapter = ResumeFakeAdapter {
            create_number: 7,
            create_unproven_error: true,
            ..Default::default()
        };
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/unproven-create");
        let body_path = record_open_body_file(state.path(), &seed);

        let first = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("zero trusted markers cannot prove an ambiguous create");
        assert_eq!(first.code, "record-open-outcome-unknown");
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 0);

        let intent = RecordOpenIntentStore::new(
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed.source_path,
            &seed.source_commit,
        );
        assert!(matches!(
            intent.load().expect("persisted create intent"),
            Some(RecordOpenIntentState::CreateInFlight)
        ));

        let retry = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("retry must reconcile instead of creating again");
        assert_eq!(retry.code, "record-open-outcome-unknown");
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn record_open_comment_store_then_error_recovers_without_second_append() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let adapter = ResumeFakeAdapter {
            create_number: 7,
            store_then_fail_on_nth_comment: Some(1),
            ..Default::default()
        };
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/comment-store-then-error");
        let body_path = record_open_body_file(state.path(), &seed);

        let result = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect("semantic provider evidence must prove the ambiguous source append");

        assert_eq!(result["issue"]["number"], 7);
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 3);
        let source_appends = adapter
            .comment_bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, body)| {
                lifecycle_record::extract_payload(body)
                    .is_ok_and(|payload| payload.role == lifecycle_record::PayloadRole::Source)
            })
            .count();
        assert_eq!(source_appends, 1);
        let intent = RecordOpenIntentStore::new(
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed.source_path,
            &seed.source_commit,
        );
        assert!(intent.load().expect("intent cleared").is_none());
    }

    #[test]
    fn record_open_persists_write_ahead_intents_before_provider_calls() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/write-ahead-intents");
        let body_path = record_open_body_file(state.path(), &seed);
        let adapter = ResumeFakeAdapter {
            create_number: 7,
            intent_probe: Some(RecordOpenIntentStore::new(
                &repo,
                crate::commands::record::RecordProfile::Tracking,
                &seed.source_path,
                &seed.source_commit,
            )),
            ..Default::default()
        };

        record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect("record open must converge");

        assert_eq!(*adapter.observed_create_intent.lock().unwrap(), Some(true));
        assert_eq!(
            *adapter.observed_comment_intents.lock().unwrap(),
            vec![true, true, true]
        );
    }

    #[test]
    fn record_open_rejects_stale_pending_payload_before_provider_writes() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/stale-pending-payload");
        let body_path = record_open_body_file(state.path(), &seed);
        let intent = RecordOpenIntentStore::new(
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed.source_path,
            &seed.source_commit,
        );
        let mut stale_payload =
            lifecycle_record::extract_payload(&seed.source_body).expect("source payload");
        stale_payload.data["summary"] = json!("stale local snapshot");
        intent
            .persist_comment_in_flight(7, &stale_payload)
            .expect("persist stale pending payload");
        let adapter = ResumeFakeAdapter::default();

        let error = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("stale pending payload must fail closed");

        assert_eq!(error.code, "record-open-intent-invalid");
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 0);
        assert!(adapter.edited.lock().unwrap().is_empty());
    }

    #[test]
    fn record_open_pending_comment_semantic_mismatch_never_reposts() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/pending-semantic-mismatch");
        let body_path = record_open_body_file(state.path(), &seed);
        let expected_payload =
            lifecycle_record::extract_payload(&seed.source_body).expect("source payload");
        let mut snapshot = expected_payload.parse_snapshot().expect("source snapshot");
        snapshot.summary = Some("different provider-visible snapshot".to_string());
        let mismatched_source = lifecycle_record::render_record_snapshot_comment(
            crate::commands::record::RecordProfile::Tracking,
            LifecycleCommentKind::Source,
            &snapshot,
            "different source contents",
            Some("2026-07-18T00:00:00Z"),
        )
        .expect("mismatched source comment");
        let mut evidence = HashMap::new();
        evidence.insert(
            7,
            (
                record_identity_body(&seed),
                comments_envelope(&[&mismatched_source]),
            ),
        );
        let adapter = ResumeFakeAdapter {
            evidence,
            ..Default::default()
        };
        let intent = RecordOpenIntentStore::new(
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed.source_path,
            &seed.source_commit,
        );
        intent
            .persist_comment_in_flight(7, &expected_payload)
            .expect("persist pending source payload");

        let error = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("same-role semantic mismatch must remain outcome-unknown");

        assert_eq!(error.code, "record-open-outcome-unknown");
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 0);
        assert!(matches!(
            intent.load().expect("retained pending intent"),
            Some(RecordOpenIntentState::CommentInFlight { .. })
        ));
    }

    #[test]
    fn record_open_intent_cleanup_failure_is_an_error() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/cleanup-failure");
        let body_path = record_open_body_file(state.path(), &seed);
        let adapter = ResumeFakeAdapter {
            create_number: 7,
            intent_probe: Some(RecordOpenIntentStore::new(
                &repo,
                crate::commands::record::RecordProfile::Tracking,
                &seed.source_path,
                &seed.source_commit,
            )),
            break_intent_cleanup: true,
            ..Default::default()
        };

        let error = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("journal cleanup failure must fail the operation");

        assert_eq!(error.code, "record-open-intent-cleanup-failed");
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn record_open_mismatched_intent_stops_before_provider_writes() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/mismatched-intent");
        let body_path = record_open_body_file(state.path(), &seed);
        let intent = RecordOpenIntentStore::new(
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed.source_path,
            &seed.source_commit,
        );
        intent
            .persist_create_in_flight()
            .expect("persist valid intent");
        let mut raw: Value = serde_json::from_slice(
            &fs::read(intent.path()).expect("read persisted intent for tampering"),
        )
        .expect("parse persisted intent");
        raw["identity"]["source_path"] = json!("docs/plans/other/source.md");
        fs::write(
            intent.path(),
            serde_json::to_vec(&raw).expect("serialize mismatched intent"),
        )
        .expect("write mismatched intent");
        let adapter = ResumeFakeAdapter::default();

        let error = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("mismatched journal must fail closed");

        assert_eq!(error.code, "record-open-intent-invalid");
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 0);
        assert!(adapter.edited.lock().unwrap().is_empty());
    }

    #[test]
    fn record_open_unreadable_intent_stops_before_provider_writes() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/unreadable-intent");
        let body_path = record_open_body_file(state.path(), &seed);
        let intent = RecordOpenIntentStore::new(
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &seed.source_path,
            &seed.source_commit,
        );
        fs::create_dir_all(intent.path()).expect("replace journal file with unreadable directory");
        let adapter = ResumeFakeAdapter::default();

        let error = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &[],
            &seed,
            "binary",
        )
        .expect_err("unreadable journal must fail closed");

        assert_eq!(error.code, "record-open-intent-read-failed");
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 0);
        assert!(adapter.edited.lock().unwrap().is_empty());
    }

    #[test]
    fn record_open_unknown_create_rejects_historical_only_ambiguity() {
        let state_lock = GlobalStateLock::new();
        let (state, _state_env) = isolate_record_open_state(&state_lock);
        let seed = resume_seed();
        let repo = record_open_test_repo("owner/create-historical-ambiguity");
        let body_path = record_open_body_file(state.path(), &seed);
        let adapter = ResumeFakeAdapter {
            open_issues: vec![2],
            evidence: HashMap::from([(
                2,
                (
                    "historical tracker".to_string(),
                    comments_envelope(&[&seed.source_body]),
                ),
            )]),
            create_number: 7,
            create_store_then_error: true,
            ..Default::default()
        };

        let error = record_open_finalize(
            &adapter,
            &repo,
            crate::commands::record::RecordProfile::Tracking,
            &body_path,
            &["creation-label".to_string()],
            &seed,
            "binary",
        )
        .expect_err("historical-only create reconciliation ambiguity must fail closed");

        assert_eq!(error.code, "record-open-outcome-unknown");
        assert_eq!(adapter.create_calls.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.comment_attempts.load(Ordering::SeqCst), 0);
        assert_eq!(
            *adapter.listed_labels.lock().unwrap(),
            vec![Vec::<String>::new()]
        );
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
            "https://gitlab.com/example-user/agent-runtime-testing/-/merge_requests/7#note_321"
        ));
        assert!(approval_comment_url_looks_valid(
            "https://gitlab.com/example-user/agent-runtime-testing/-/work_items/13#note_654"
        ));
        assert!(!approval_comment_url_looks_valid(
            "https://example.com/issues/217#issuecomment-123"
        ));
        assert!(!approval_comment_url_looks_valid(
            "https://github.com/sympoies/nils-cli/issues/217#comment-123"
        ));
        assert!(!approval_comment_url_looks_valid(
            "https://gitlab.com/example-user/agent-runtime-testing/-/merge_requests/7#note_abc"
        ));
        assert!(!approval_comment_url_looks_valid(
            "https://gitlab.com/example-user/agent-runtime-testing/-/merge_requests/7"
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

        let snapshot = execution_state_snapshot_for_test(&ledger);
        let payload = state_checkpoint_payload(&run, Some(&snapshot));
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
        let baseline = state_checkpoint_payload(&bare, None);
        assert_eq!(
            baseline["tasks"].as_array().expect("tasks array").len(),
            1,
            "without a ledger the payload stays single-current"
        );
    }

    #[test]
    fn state_checkpoint_payload_consumes_bundle_discovered_snapshot() {
        // graysurf/plan-tracking-testbed#55: the canonical `tracking run init
        // --bundle <dir>` flow records only `bundle` (no `execution_state_file`).
        // Once the command resolver discovers the bundle's unique state file,
        // the payload must consume that snapshot and carry the FULL ledger.
        use crate::tracking::run_state::{ExecutionRun, RunPhase};

        let tmp = TempDir::new().expect("tempdir");
        let bundle = tmp.path().join("docs/plans/slug");
        std::fs::create_dir_all(&bundle).expect("bundle dir");
        std::fs::write(
            bundle.join("slug-execution-state.md"),
            concat!(
                "# Execution State\n\n",
                "## Task Ledger\n\n",
                "| ID | Status | Task | Evidence | Notes |\n",
                "| --- | --- | --- | --- | --- |\n",
                "| 1.1 | done | Append line A | log | first |\n",
                "| 1.2 | in-progress | Append line B | — | second |\n",
                "| 1.3 | pending | Append line C | — | third |\n",
            ),
        )
        .expect("write ledger");

        let mut run = ExecutionRun::new(
            "run-bundle",
            "owner/repo",
            1,
            "tracking",
            RunPhase::Implementing,
            "2026-05-29T00:00:00Z",
        );
        run.bundle = Some(bundle.clone());
        assert!(
            run.execution_state_file.is_none(),
            "this case covers --bundle with no explicit execution-state file"
        );

        let snapshot = execution_state_snapshot_for_test(&bundle.join("slug-execution-state.md"));
        let payload = state_checkpoint_payload(&run, Some(&snapshot));
        let tasks = payload["tasks"].as_array().expect("tasks array");
        assert_eq!(
            tasks.len(),
            3,
            "state payload must resolve the bundle's *-execution-state.md ledger, not the single-row baseline"
        );
        let ids: Vec<&str> = tasks.iter().map(|t| t["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["1.1", "1.2", "1.3"]);
    }

    #[test]
    fn absolutize_makes_relative_paths_absolute_and_leaves_absolute_paths_absolute() {
        use std::path::{Path, PathBuf};

        // A relative ref (what `tracking run init --bundle docs/plans/x` records
        // today) becomes absolute so a later checkpoint resolves it regardless
        // of its working directory (graysurf/plan-tracking-testbed#55).
        let rel = absolutize(Path::new("docs/plans/slug"));
        assert!(
            rel.is_absolute(),
            "relative ref must be absolutized: {rel:?}"
        );
        assert!(rel.ends_with("docs/plans/slug"));

        // An already-absolute ref stays absolute and idempotent.
        let abs_in = PathBuf::from("/tmp/bundle/slug-execution-state.md");
        let abs_out = absolutize(&abs_in);
        assert!(abs_out.is_absolute());
        assert_eq!(abs_out, abs_in);
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

        let payload = state_checkpoint_payload(&run, None);
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
        let bare_prs = state_checkpoint_payload(&bare, None)["prs"].clone();
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
        run.execution_state_file = Some(done.clone());
        let done_snapshot = execution_state_snapshot_for_test(&done);
        let payload = state_checkpoint_payload(&run, Some(&done_snapshot));
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
        run_mid.execution_state_file = Some(mid.clone());
        let mid_snapshot = execution_state_snapshot_for_test(&mid);
        let mid_payload = state_checkpoint_payload(&run_mid, Some(&mid_snapshot));
        assert_eq!(
            mid_payload["current"], "1.2",
            "current = first non-terminal ledger row"
        );
        assert_eq!(
            mid_payload["next_action"], "1.3",
            "next_action = next non-terminal ledger row"
        );
        assert_eq!(mid_payload["target_scope"], "demo scope");

        // A premature terminal phase must not hide unfinished ledger rows behind
        // the canonical `complete` / `closeout` projection.
        let mut premature = run_mid.clone();
        premature.phase = RunPhase::ReadyForClose;
        let premature_payload = state_checkpoint_payload(&premature, Some(&mid_snapshot));
        assert_eq!(premature_payload["current"], "1.2");
        assert_eq!(premature_payload["next_action"], "1.3");
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
        run.execution_state_file = Some(ledger.clone());

        let snapshot = execution_state_snapshot_for_test(&ledger);
        let payload = state_checkpoint_payload(&run, Some(&snapshot));
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

        let marker = |profile| match render_checkpoint_role(PayloadRole::State, &run, profile, None)
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
        let second_markdown =
            write_temp_markdown("status", "world").expect("write second temp markdown");
        assert_ne!(
            markdown, second_markdown,
            "temporary bodies with one stem must never overwrite each other"
        );
        assert!(
            markdown
                .physical_path()
                .to_string_lossy()
                .contains("plan-issue-delivery/tmp")
        );
        assert_eq!(
            fs::read_to_string(&markdown).expect("read markdown"),
            "hello"
        );
        assert_eq!(
            fs::read_to_string(&second_markdown).expect("read second markdown"),
            "world"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                fs::metadata(&markdown)
                    .expect("temporary markdown metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let markdown_path = markdown.physical_path().to_path_buf();
        let second_markdown_path = second_markdown.physical_path().to_path_buf();
        drop(markdown);
        drop(second_markdown);
        assert!(!markdown_path.exists(), "temporary body must be unlinked");
        assert!(
            !second_markdown_path.exists(),
            "second temporary body must be unlinked"
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
    fn temp_markdown_retries_create_new_collision_without_overwrite() {
        let dir = TempDir::new().expect("tempdir");
        let dir_handle =
            ensure_private_temp_markdown_dir(dir.path()).expect("secure temp directory");
        let timestamp = 123_u128;
        let process_id = 456_u32;
        let sequence = AtomicU64::new(0);
        let collision = dir.path().join("status-123-456-0.md");
        fs::write(&collision, "sentinel").expect("seed collision");

        let markdown = write_temp_markdown_in(
            &dir_handle,
            "status",
            "provider body",
            timestamp,
            process_id,
            &sequence,
        )
        .expect("retry after collision");

        assert_eq!(
            markdown
                .physical_path()
                .file_name()
                .and_then(|name| name.to_str()),
            Some("status-123-456-1.md")
        );
        assert_eq!(
            fs::read_to_string(&collision).expect("sentinel"),
            "sentinel"
        );
        assert_eq!(
            fs::read_to_string(&markdown).expect("provider body"),
            "provider body"
        );
        let markdown_path = markdown.physical_path().to_path_buf();
        drop(markdown);
        assert!(!markdown_path.exists());
        assert_eq!(fs::read_to_string(collision).expect("sentinel"), "sentinel");
    }

    #[cfg(unix)]
    #[test]
    fn temp_markdown_directory_swap_cannot_redirect_create_cleanup_or_drop() {
        let root = TempDir::new().expect("root");
        let outside = TempDir::new().expect("outside");
        let dir_path = root.path().join("tmp");
        fs::create_dir(&dir_path).expect("temp markdown dir");
        let stale = dir_path.join("stale.md");
        fs::write(&stale, "stale").expect("stale body");
        File::open(&stale)
            .expect("open stale")
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(UNIX_EPOCH)
                    .set_accessed(UNIX_EPOCH),
            )
            .expect("age stale body");
        let dir = ensure_private_temp_markdown_dir(&dir_path).expect("pin temp directory");
        let retained = root.path().join("retained");
        fs::rename(&dir_path, &retained).expect("rename pinned directory");
        std::os::unix::fs::symlink(outside.path(), &dir_path).expect("replace with symlink");
        fs::write(outside.path().join("sentinel.md"), "outside").expect("outside sentinel");

        cleanup_stale_temp_markdown(&dir);
        assert!(!retained.join("stale.md").exists());
        assert_eq!(
            fs::read_to_string(outside.path().join("sentinel.md")).expect("outside sentinel"),
            "outside"
        );
        let markdown = write_temp_markdown_in(
            &dir,
            "status",
            "provider body",
            123,
            456,
            &AtomicU64::new(0),
        )
        .expect("descriptor-relative create");
        assert_eq!(
            fs::read_to_string(&markdown).expect("descriptor body"),
            "provider body"
        );
        let retained_body = retained.join(&markdown.file_name);
        assert!(retained_body.exists());
        assert_eq!(
            fs::read_dir(outside.path())
                .expect("outside entries")
                .count(),
            1,
            "directory swap redirected temporary body creation"
        );

        drop(markdown);
        assert!(
            !retained_body.exists(),
            "drop must unlink from pinned directory"
        );
        assert_eq!(
            fs::read_to_string(outside.path().join("sentinel.md")).expect("outside sentinel"),
            "outside"
        );
    }

    #[test]
    fn temp_markdown_partial_write_failure_unlinks_created_file() {
        let dir_path = TempDir::new().expect("tempdir");
        let dir = ensure_private_temp_markdown_dir(dir_path.path()).expect("pin temp directory");
        let error = write_temp_markdown_in_with(
            &dir,
            "status",
            "provider body",
            123,
            456,
            &AtomicU64::new(0),
            |file, _| {
                file.write_all(b"partial")?;
                Err(std::io::Error::other("injected write failure"))
            },
        )
        .expect_err("partial write must fail");

        assert!(error.contains("injected write failure"), "{error}");
        assert_eq!(
            fs::read_dir(dir_path.path())
                .expect("temp directory")
                .count(),
            0,
            "partially written provider body was orphaned"
        );
    }

    #[cfg(unix)]
    #[test]
    fn temp_markdown_rejects_symlinked_delivery_directory() {
        let lock = GlobalStateLock::new();
        let state = TempDir::new().expect("state dir");
        let outside = TempDir::new().expect("outside dir");
        crate::state::set_state_dir_override(None);
        let _state_dir = EnvGuard::set(
            &lock,
            "PLAN_ISSUE_HOME",
            state.path().to_string_lossy().as_ref(),
        );
        let delivery = state.path().join("out/plan-issue-delivery");
        fs::create_dir_all(&delivery).expect("delivery parent");
        std::os::unix::fs::symlink(outside.path(), delivery.join("tmp"))
            .expect("symlink temp directory");

        let error = write_temp_markdown("status", "private body")
            .expect_err("symlinked temp directory must be rejected");

        assert!(error.contains("temporary markdown directory"), "{error}");
        assert_eq!(
            fs::read_dir(outside.path())
                .expect("outside directory")
                .count(),
            0,
            "provider body must not be redirected outside the state directory"
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

    #[test]
    fn attach_open_exec_state_sync_writes_tracking_url_and_reports() {
        let tmp = TempDir::new().expect("tempdir");
        let state = tmp.path().join("demo-execution-state.md");
        std::fs::write(
            &state,
            "## Execution State\n\n- Status: tracking issue opened\n- Tracking issue: not yet opened\n",
        )
        .unwrap();
        let result = json!({
            "operation": "record.open",
            "issue": {"number": 738, "url": "https://github.com/o/r/issues/738"},
        });
        let out = attach_open_exec_state_sync(result, Some(state.as_path()));
        let sync = out.get("execution_state_sync").expect("sync field");
        assert_eq!(sync.get("changed").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            sync.get("followup_commit_required")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        let written = std::fs::read_to_string(&state).unwrap();
        assert!(written.contains("- Tracking issue: <https://github.com/o/r/issues/738>"));
    }

    #[test]
    fn attach_open_exec_state_sync_skips_without_bundle_state() {
        let result = json!({"issue": {"url": "https://github.com/o/r/issues/1"}});
        let out = attach_open_exec_state_sync(result, None);
        assert_eq!(
            out.get("execution_state_sync")
                .and_then(|s| s.get("skipped"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn close_exec_state_writeback_sets_terminal_header_fields() {
        let tmp = TempDir::new().expect("tempdir");
        let state = tmp.path().join("demo-execution-state.md");
        std::fs::write(
            &state,
            "## Execution State\n\n- Status: tracking issue opened; implementation not yet started.\n- Current task: merge PR #42 and close the tracker.\n- Next task: run record close after the merge.\n- Last updated: 2026-06-01\n- Branch/commit/PR: no PR opened.\n- Tracking issue: <https://github.com/o/r/issues/738>\n\n## Task Ledger\n\n| ID | Status | Task | Evidence | Notes |\n| --- | --- | --- | --- | --- |\n| 1.1 | done | x | y | z |\n\n## Handoff\n\n- Merge PR #42.\n- Run strict close-ready and record close.\n\n## Session Log\n\n- Preserve this unrelated section verbatim.\n",
        )
        .unwrap();
        let linked = vec![lifecycle_record::LinkedPrEvidence {
            pr_ref: "o/r#42".to_string(),
            url: Some("https://github.com/o/r/pull/42".to_string()),
            merge_sha: Some("abc".to_string()),
            checks: lifecycle_record::CheckStatus::Pass,
            required_state: None,
            required_count: None,
            non_required_failures: Vec::new(),
        }];
        let terminal =
            close_exec_state_terminal_state("https://github.com/o/r/issues/738", &linked);
        let report = plan_tooling::exec_state::writeback_terminal(&state, &terminal, false)
            .expect("write terminal state");
        assert!(report.changed);
        let written = std::fs::read_to_string(&state).unwrap();
        assert!(written.contains("- Status: complete\n"));
        // The PR URL must be an angle-bracket autolink so the written
        // execution-state passes markdown lint (MD034 forbids bare URLs), matching
        // the `Tracking issue` bullet's treatment. Regression guard for #1006.
        assert!(
            written
                .contains("- Branch/commit/PR: o/r#42 merged (<https://github.com/o/r/pull/42>)")
        );
        assert!(
            !written.contains("merged (https://"),
            "PR URL must not be written as a bare URL (MD034)"
        );
        assert!(written.contains("- Tracking issue: <https://github.com/o/r/issues/738>"));
        assert!(written.contains("- Current task: complete\n"));
        assert!(written.contains("- Next task: none\n"));
        assert!(written.contains(
            "- Tracking issue <https://github.com/o/r/issues/738> is closed; terminal execution state is synchronized. No closeout or merge action remains."
        ));
        assert!(!written.contains("merge PR #42"));
        assert!(!written.contains("run record close"));
        // Task Ledger row preserved verbatim.
        assert!(written.contains("| 1.1 | done | x | y | z |"));
        assert!(written.contains("- Preserve this unrelated section verbatim."));
    }

    #[test]
    fn close_exec_state_writeback_acquires_lock_before_expected_contents_comparison() {
        let tmp = TempDir::new().expect("tempdir");
        let state = tmp.path().join("demo-execution-state.md");
        let concurrent_contents = "## Execution State\n\n- Status: concurrent writer\n";
        std::fs::write(&state, concurrent_contents).expect("write concurrent state");
        let lock_path = state.with_file_name("demo-execution-state.md.lock");
        let _active_lock = plan_tooling::mutation_lock::OwnedFileLock::acquire(&lock_path)
            .expect("hold execution-state lock");
        let exec_state = plan_tooling::exec_state::PinnedExecutionState::pin(tmp.path(), &state)
            .expect("pin execution state");
        let plan = CloseExecStateWritebackPlan::Apply {
            exec_state,
            expected_contents: "## Execution State\n\n- Status: preflight snapshot\n".to_string(),
            state: Box::new(close_exec_state_terminal_state(
                "https://github.com/o/r/issues/738",
                &[],
            )),
        };

        let err = apply_close_exec_state_writeback(plan).expect_err("busy lock must fail");

        assert_eq!(err.code, "record-close-execution-state-writeback-failed");
        assert!(err.message.contains("exec-state-mutation-lock-busy"));
        assert_eq!(
            std::fs::read_to_string(&state).expect("read unchanged state"),
            concurrent_contents
        );
        assert!(lock_path.exists(), "stable advisory lock path missing");
    }

    #[test]
    fn close_exec_state_writeback_rejects_byte_identical_inode_replacement() {
        const CONTENTS: &str = "## Execution State\n\n- Status: active\n- Current task: close\n- Next task: none\n\n## Handoff\n\n- Close the tracker.\n";

        let tmp = TempDir::new().expect("tempdir");
        let state = tmp.path().join("demo-execution-state.md");
        let displaced = tmp.path().join("displaced-execution-state.md");
        std::fs::write(&state, CONTENTS).expect("write original state");
        let exec_state = plan_tooling::exec_state::PinnedExecutionState::pin(tmp.path(), &state)
            .expect("pin execution state");
        let plan = CloseExecStateWritebackPlan::Apply {
            exec_state,
            expected_contents: CONTENTS.to_string(),
            state: Box::new(close_exec_state_terminal_state(
                "https://github.com/o/r/issues/738",
                &[],
            )),
        };
        std::fs::rename(&state, &displaced).expect("displace preflight inode");
        std::fs::write(&state, CONTENTS).expect("write byte-identical replacement");

        let err = apply_close_exec_state_writeback(plan)
            .expect_err("a byte-identical inode replacement must fail");

        assert_eq!(err.code, "record-close-execution-state-writeback-failed");
        assert!(err.message.contains("path changed after preflight"));
        assert_eq!(
            std::fs::read_to_string(&state).expect("replacement after failed apply"),
            CONTENTS
        );
        assert_eq!(
            std::fs::read_to_string(&displaced).expect("original after failed apply"),
            CONTENTS
        );
    }

    #[test]
    fn close_exec_state_writeback_skips_without_bundle() {
        let repo = crate::provider::Repo {
            provider: crate::provider::Provider::GitHub,
            slug: "o/r".to_string(),
            host: Some("github.com".to_string()),
        };
        let plan =
            prepare_close_exec_state_writeback(None, &repo, "https://github.com/o/r/issues/1", &[])
                .expect("prepare skipped writeback");
        let out = apply_close_exec_state_writeback(plan).expect("skipped writeback");
        assert_eq!(out.get("skipped").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn close_label_convergence_allows_unrelated_automation_labels() {
        let plan = build_close_label_plan(
            vec!["state::closed".into()],
            vec![],
            Some(vec!["state::ready".into(), "workflow::tracking".into()]),
            Some(vec!["state::ready".into(), "state::closed".into()]),
            Some(crate::provider::Provider::GitHub),
        )
        .expect("label plan");
        assert!(close_label_plan_converged(
            crate::provider::Provider::GitHub,
            &plan,
            &[
                "automation::complete".into(),
                "State::Closed".into(),
                "workflow::tracking".into(),
            ]
        ));
        assert!(!close_label_plan_converged(
            crate::provider::Provider::GitHub,
            &plan,
            &[
                "state::closed".into(),
                "state::ready".into(),
                "workflow::tracking".into(),
            ]
        ));
    }

    #[test]
    fn github_close_label_plan_rejects_case_equivalent_add_remove_conflicts() {
        let error = build_close_label_plan(
            vec!["state::closed".into()],
            vec!["State::Closed".into()],
            Some(vec!["state::ready".into()]),
            Some(vec!["state::ready".into(), "state::closed".into()]),
            Some(crate::provider::Provider::GitHub),
        )
        .expect_err("GitHub labels are case-insensitive");
        assert_eq!(error.code, "record-label-mutation-conflict");
    }

    #[test]
    fn close_label_plan_deduplicates_with_provider_identity_rules() {
        let github = build_close_label_plan(
            vec!["Bug".into(), "bug".into()],
            vec![],
            Some(vec![]),
            Some(vec!["bug".into()]),
            Some(crate::provider::Provider::GitHub),
        )
        .expect("GitHub semantic duplicate");
        assert_eq!(github.add, vec!["Bug"]);

        let gitlab = build_close_label_plan(
            vec!["Bug".into(), "bug".into()],
            vec!["State::Closed".into()],
            Some(vec![]),
            Some(vec!["Bug".into(), "bug".into()]),
            Some(crate::provider::Provider::GitLab),
        )
        .expect("GitLab labels are case-sensitive");
        assert_eq!(gitlab.add, vec!["Bug", "bug"]);
        assert_eq!(gitlab.remove, vec!["State::Closed"]);

        let gitlab_case_distinct = build_close_label_plan(
            vec!["state::closed".into()],
            vec!["State::Closed".into()],
            Some(vec![]),
            Some(vec!["state::closed".into()]),
            Some(crate::provider::Provider::GitLab),
        )
        .expect("GitLab case-distinct add and remove are not contradictory");
        assert_eq!(gitlab_case_distinct.add, vec!["state::closed"]);
        assert_eq!(gitlab_case_distinct.remove, vec!["State::Closed"]);
    }

    #[test]
    fn label_diagnostics_escape_provider_control_characters() {
        let rendered =
            render_label_diagnostic(&["safe".into(), "bad\n\u{1b}]8;;https://x\u{7}".into()]);

        assert_eq!(rendered, r#"["safe", "bad\n\u001b]8;;https://x\u0007"]"#);
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\u{7}'));
    }

    #[test]
    fn issue_number_from_url_parses_github_and_gitlab() {
        assert_eq!(
            issue_number_from_url("https://github.com/o/r/issues/738"),
            Some(738)
        );
        assert_eq!(
            issue_number_from_url("https://gitlab.example.com/g/p/-/issues/42"),
            Some(42)
        );
        assert_eq!(issue_number_from_url("https://github.com/o/r/pull/9"), None);
        assert_eq!(issue_number_from_url("not yet opened"), None);
    }

    #[test]
    fn classify_exec_state_issue_covers_missing_consistent_mismatch_and_invalid() {
        assert_eq!(
            classify_exec_state_issue(None, None, 738),
            ExecStateIssueClass::Missing
        );
        assert_eq!(
            classify_exec_state_issue(Some("not yet opened"), None, 738),
            ExecStateIssueClass::Missing
        );
        assert_eq!(
            classify_exec_state_issue(Some("https://github.com/o/r/issues/738"), None, 738),
            ExecStateIssueClass::Consistent
        );
        assert_eq!(
            classify_exec_state_issue(Some("https://github.com/o/r/issues/716"), None, 738),
            ExecStateIssueClass::Mismatch("https://github.com/o/r/issues/716".to_string())
        );
        assert_eq!(
            classify_exec_state_issue(Some("https://github.com/o/r/pull/9"), None, 738),
            ExecStateIssueClass::Invalid
        );
        assert_eq!(
            classify_exec_state_issue(Some("hand-authored tracking note"), None, 738),
            ExecStateIssueClass::Invalid
        );
    }

    fn closeout_body_for_test(notes: &str) -> String {
        lifecycle_record::render_record_post_comment(
            RecordProfile::Tracking,
            crate::commands::record::LifecycleCommentKind::Closeout,
            json!({
                "final_status": "complete",
                "approval": {"comment_url": "https://example.test/approval"},
                "linked_prs": [],
                "notes": notes,
            }),
            Some("Closeout summary."),
            None,
        )
        .expect("closeout body")
    }

    fn closeout_audit_for_test(comments: Value) -> lifecycle_record::RecordAudit {
        lifecycle_record::audit_record(
            None,
            &json!({"comments": comments}).to_string(),
            Some(RecordProfile::Tracking),
        )
        .expect("closeout audit")
    }

    #[test]
    fn closeout_retry_reuses_matching_latest_comment() {
        let body = closeout_body_for_test("closed");
        let expected = lifecycle_record::extract_payload(&body).expect("closeout payload");
        let existing_url = "https://example.test/issues/42#issuecomment-existing";
        let audit = closeout_audit_for_test(json!([{
            "body": body,
            "url": existing_url,
            "created_at": "2026-07-18T01:00:00Z",
        }]));
        let adapter = MockProviderAdapter::default().with_comment_result(Ok(
            "https://example.test/issues/42#issuecomment-duplicate".to_string(),
        ));

        let actual = resolve_closeout_comment_url(
            &adapter,
            "owner/repo",
            42,
            RecordProfile::Tracking,
            &audit,
            &expected,
            &body,
        )
        .expect("reuse matching closeout");

        assert_eq!(actual, existing_url);
        assert_eq!(adapter.comment_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn closeout_post_error_recovers_latest_matching_comment_without_repost() {
        let body = closeout_body_for_test("closed");
        let expected = lifecycle_record::extract_payload(&body).expect("closeout payload");
        let initial_audit = closeout_audit_for_test(json!([]));
        let recovered_url = "https://example.test/issues/42#issuecomment-recovered";
        let recovery_comments = json!({
            "comments": [{
                "body": body,
                "url": recovered_url,
                "created_at": "2026-07-18T01:00:00Z",
            }]
        })
        .to_string();
        let adapter = MockProviderAdapter::default()
            .with_comment_result(Err("ambiguous transport failure".to_string()))
            .with_evidence_result(Ok((String::new(), recovery_comments)));

        let actual = resolve_closeout_comment_url(
            &adapter,
            "owner/repo",
            42,
            RecordProfile::Tracking,
            &initial_audit,
            &expected,
            &body,
        )
        .expect("recover accepted closeout");

        assert_eq!(actual, recovered_url);
        assert_eq!(adapter.comment_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn closeout_post_error_rejects_latest_mismatch_without_repost() {
        let body = closeout_body_for_test("closed");
        let expected = lifecycle_record::extract_payload(&body).expect("closeout payload");
        let initial_audit = closeout_audit_for_test(json!([]));
        let matching_older = closeout_body_for_test("closed");
        let mismatching_latest = closeout_body_for_test("different closeout");
        let recovery_comments = json!({
            "comments": [
                {
                    "body": matching_older,
                    "url": "https://example.test/issues/42#issuecomment-older",
                    "created_at": "2026-07-18T01:00:00Z",
                },
                {
                    "body": mismatching_latest,
                    "url": "https://example.test/issues/42#issuecomment-latest",
                    "created_at": "2026-07-18T02:00:00Z",
                }
            ]
        })
        .to_string();
        let adapter = MockProviderAdapter::default()
            .with_comment_result(Err("ambiguous transport failure".to_string()))
            .with_evidence_result(Ok((String::new(), recovery_comments)));

        let error = resolve_closeout_comment_url(
            &adapter,
            "owner/repo",
            42,
            RecordProfile::Tracking,
            &initial_audit,
            &expected,
            &body,
        )
        .expect_err("latest mismatched closeout must not recover the post");

        assert_eq!(error.code, "record-close-comment-post-failed");
        assert_eq!(adapter.comment_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn historical_transport_alias_identities_normalize_in_memory() {
        let cases = [
            (
                "github",
                crate::provider::Provider::GitHub,
                "ssh.github.com",
                "github.com",
            ),
            (
                "gitlab",
                crate::provider::Provider::GitLab,
                "altssh.gitlab.com",
                "gitlab.com",
            ),
        ];

        for (provider_name, provider, alias, canonical) in cases {
            let historical = repository_identity("owner/repo", Some(provider_name), Some(alias))
                .expect("historical identity");
            let current = crate::provider::Repo {
                provider,
                slug: "owner/repo".to_string(),
                host: Some(canonical.to_string()),
            };
            assert_eq!(historical.host.as_deref(), Some(canonical));
            assert!(repository_identity_matches(&historical, &current));
            assert!(is_default_provider_host(&historical));
        }
    }

    #[test]
    fn linked_pr_reference_accepts_provider_transport_aliases() {
        let cases = [
            (
                crate::provider::Provider::GitHub,
                "github.com",
                "https://ssh.github.com/owner/repo/pull/7",
            ),
            (
                crate::provider::Provider::GitLab,
                "gitlab.com",
                "https://altssh.gitlab.com/owner/repo/-/merge_requests/7",
            ),
        ];

        for (provider, canonical, linked_pr) in cases {
            let tracking_repo = crate::provider::Repo {
                provider,
                slug: "owner/repo".to_string(),
                host: Some(canonical.to_string()),
            };
            let (target, number) = resolve_linked_pr_target(linked_pr, &tracking_repo)
                .expect("transport alias matches tracking authority");
            assert_eq!(target.host.as_deref(), Some(canonical));
            assert_eq!(number, 7);
            assert!(target.pr_url(number).contains(canonical));
        }
    }

    #[test]
    fn linked_pr_reference_matches_explicit_and_implicit_default_authorities() {
        for (provider, default_host, linked_pr) in [
            (
                crate::provider::Provider::GitHub,
                "github.com",
                "https://github.com/owner/repo/pull/7",
            ),
            (
                crate::provider::Provider::GitLab,
                "gitlab.com",
                "https://gitlab.com/owner/repo/-/merge_requests/7",
            ),
        ] {
            let implicit = crate::provider::Repo {
                provider,
                slug: "owner/repo".to_string(),
                host: None,
            };
            let (target, number) = resolve_linked_pr_target(linked_pr, &implicit)
                .expect("explicit default host matches implicit repository authority");
            assert_eq!(target.host.as_deref(), Some(default_host));
            assert_eq!(number, 7);
        }
    }

    #[test]
    fn historical_github_issues_pr_url_canonicalizes_to_pull_url() {
        let historical = "https://github.com/owner/repo/issues/7";
        assert_eq!(
            canonical_linked_pr_reference(historical).expect("historical v1 PR URL"),
            "https://github.com/owner/repo/pull/7"
        );

        let tracking_repo = crate::provider::Repo {
            provider: crate::provider::Provider::GitHub,
            slug: "owner/repo".to_string(),
            host: None,
        };
        let (target, number) = resolve_linked_pr_target(historical, &tracking_repo)
            .expect("historical PR URL remains readable");
        assert_eq!(target.slug, "owner/repo");
        assert_eq!(number, 7);
    }

    #[test]
    fn linked_pr_reference_rejects_untrusted_authority_and_sensitive_url_parts() {
        let tracking_repo = crate::provider::Repo {
            provider: crate::provider::Provider::GitHub,
            slug: "owner/repo".to_string(),
            host: Some("github.example.test".to_string()),
        };

        let authority_error =
            resolve_linked_pr_target("https://attacker.example/owner/repo/pull/7", &tracking_repo)
                .expect_err("cross-authority linked PR must fail");
        assert_eq!(authority_error.code, "record-linked-pr-authority-mismatch");

        for value in [
            "https://operator:secret@github.example.test/owner/repo/pull/7",
            "https://github.example.test/owner/repo/pull/7?token=secret",
            "https://github.example.test/owner/repo/pull/7#secret",
            "operator:secret@host/owner/repo#7",
        ] {
            let error = resolve_linked_pr_target(value, &tracking_repo)
                .expect_err("credential-bearing linked PR must fail");
            let rendered = &error.message;
            assert!(!rendered.contains("operator"), "{rendered}");
            assert!(!rendered.contains("secret"), "{rendered}");
        }
    }
}

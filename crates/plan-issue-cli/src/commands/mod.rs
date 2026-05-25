pub mod build;
pub mod completion;
pub mod plan;
pub mod record;
pub mod sprint;
pub mod tracking;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use crate::{ValidationError, issue_body};

use self::build::{BuildPlanTaskSpecArgs, BuildTaskSpecArgs};
use self::completion::CompletionArgs;
use self::plan::{
    CleanupWorktreesArgs, ClosePlanArgs, LinkPrArgs, ReadyPlanArgs, ResolveApprovalArgs,
    StartPlanArgs, StatusPlanArgs,
};
use self::record::RecordArgs;
use self::sprint::{AcceptSprintArgs, MultiSprintGuideArgs, ReadySprintArgs, StartSprintArgs};
use self::tracking::TrackingArgs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
pub enum PrGrouping {
    #[value(name = "per-sprint", alias = "per-spring")]
    PerSprint,
    #[value(name = "group")]
    Group,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
pub enum SplitStrategy {
    #[value(name = "deterministic")]
    Deterministic,
    #[value(name = "auto")]
    Auto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrGroupMapping {
    pub task: String,
    pub group: String,
}

fn parse_pr_group_mapping(raw: &str) -> Result<PrGroupMapping, String> {
    let (task_raw, group_raw) = raw
        .split_once('=')
        .ok_or_else(|| "expected format <task>=<group>".to_string())?;

    let task = task_raw.trim();
    let group = group_raw.trim();

    if task.is_empty() {
        return Err("task key in --pr-group cannot be empty".to_string());
    }
    if group.is_empty() {
        return Err("group name in --pr-group cannot be empty".to_string());
    }

    Ok(PrGroupMapping {
        task: task.to_string(),
        group: group.to_string(),
    })
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct PrefixArgs {
    /// Task owner prefix.
    #[arg(long, default_value = "subagent", value_name = "text")]
    pub owner_prefix: String,

    /// Branch prefix. Defaults to `feat` so dispatch lane branches pass the
    /// `forge-cli` Conventional Commits prefix rule
    /// (`feat|fix|chore|docs|ci|refactor`).
    #[arg(long, default_value = "feat", value_name = "text")]
    pub branch_prefix: String,

    /// Worktree prefix.
    #[arg(long, default_value = "feat__", value_name = "text")]
    pub worktree_prefix: String,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct GroupingArgs {
    /// Split strategy for group assignment.
    #[arg(
        long,
        value_enum,
        default_value_t = SplitStrategy::Deterministic,
        value_name = "strategy"
    )]
    pub strategy: SplitStrategy,

    /// PR grouping mode (deterministic only).
    #[arg(long, value_enum, value_name = "mode")]
    pub pr_grouping: Option<PrGrouping>,

    /// Auto fallback when sprint metadata omits grouping intent.
    #[arg(long = "default-pr-grouping", value_enum, value_name = "mode")]
    pub default_pr_grouping: Option<PrGrouping>,

    /// Explicit task->group mapping (`<task>=<group>`). Repeatable.
    #[arg(
        long = "pr-group",
        value_name = "task=group",
        value_parser = parse_pr_group_mapping
    )]
    pub pr_group: Vec<PrGroupMapping>,
}

#[derive(Debug, Clone, Args, Default, Serialize)]
pub struct SummaryArgs {
    /// Inline review summary text.
    #[arg(long, conflicts_with = "summary_file", value_name = "text")]
    pub summary: Option<String>,

    /// Path to markdown/text review summary.
    #[arg(long, conflicts_with = "summary", value_name = "path")]
    pub summary_file: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Args, Default, Serialize)]
pub struct CommentModeArgs {
    /// Emit comment output.
    #[arg(long, conflicts_with = "no_comment")]
    pub comment: bool,

    /// Disable comment output.
    #[arg(long = "no-comment", conflicts_with = "comment")]
    pub no_comment: bool,
}

#[derive(Debug, Clone, Args, Default, Serialize)]
pub struct CommentTextArgs {
    /// Inline close comment.
    #[arg(long, conflicts_with = "comment_file", value_name = "text")]
    pub comment: Option<String>,

    /// Path to close comment markdown/text.
    #[arg(long, conflicts_with = "comment", value_name = "path")]
    pub comment_file: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Build sprint-scoped task-spec TSV from a plan.
    BuildTaskSpec(BuildTaskSpecArgs),

    /// Build plan-scoped task-spec TSV (all sprints) for the single plan issue.
    BuildPlanTaskSpec(BuildPlanTaskSpecArgs),

    /// Open one plan issue with all plan tasks in Task Decomposition.
    StartPlan(StartPlanArgs),

    /// Wrapper of issue-delivery-loop status for the plan issue.
    StatusPlan(StatusPlanArgs),

    /// Link PR to task rows and set runtime status (default: in-progress).
    LinkPr(LinkPrArgs),

    /// Wrapper of issue-delivery-loop ready-for-review for final plan review.
    ReadyPlan(ReadyPlanArgs),

    /// Close the single plan issue after final approval + merged PR gates, then enforce worktree cleanup.
    ClosePlan(ClosePlanArgs),

    /// Enforce cleanup of all issue-assigned task worktrees.
    CleanupWorktrees(CleanupWorktreesArgs),

    /// Start sprint from Task Decomposition runtime truth after previous sprint merge+done gate passes.
    StartSprint(StartSprintArgs),

    /// Post sprint-ready comment for main-agent review before merge.
    ReadySprint(ReadySprintArgs),

    /// Enforce merged-PR gate, sync sprint status=done, then post accepted comment.
    AcceptSprint(AcceptSprintArgs),

    /// Print the full repeated command flow for a plan (1 plan = 1 issue).
    MultiSprintGuide(MultiSprintGuideArgs),

    /// Resolve the URL of the most recent `Decision: merge` review-evidence
    /// comment on a PR, suitable for `accept-sprint --approved-comment-url`.
    ResolveApproval(ResolveApprovalArgs),

    /// Render and audit issue-backed plan record dashboards and comments.
    Record(RecordArgs),

    /// Run-state controller for the plan-tracking issue workflow
    /// (`status`, `run init`, `run update`, `checkpoint`, `close-ready`).
    Tracking(TrackingArgs),

    /// Export shell completion script.
    Completion(CompletionArgs),
}

impl Command {
    pub fn command_id(&self) -> &'static str {
        match self {
            Self::BuildTaskSpec(_) => "build-task-spec",
            Self::BuildPlanTaskSpec(_) => "build-plan-task-spec",
            Self::StartPlan(_) => "start-plan",
            Self::StatusPlan(_) => "status-plan",
            Self::LinkPr(_) => "link-pr",
            Self::ReadyPlan(_) => "ready-plan",
            Self::ClosePlan(_) => "close-plan",
            Self::CleanupWorktrees(_) => "cleanup-worktrees",
            Self::StartSprint(_) => "start-sprint",
            Self::ReadySprint(_) => "ready-sprint",
            Self::AcceptSprint(_) => "accept-sprint",
            Self::MultiSprintGuide(_) => "multi-sprint-guide",
            Self::ResolveApproval(_) => "resolve-approval",
            Self::Record(args) => args.command_id(),
            Self::Tracking(args) => args.command_id(),
            Self::Completion(_) => "completion",
        }
    }

    pub fn schema_version(&self) -> String {
        // Most commands stay on `.v1`. Commands whose `result` payload picked
        // up new orchestrator-friendly fields in Sprint 1 (`repo_slug`,
        // `pr_groups`, `worktree_abs_path`) bump to `.v2`. Existing v1
        // readers that read only the older fields are still compatible —
        // the new fields are additive — but should be considered deprecated
        // and updated to the v2 schema.
        let suffix = match self {
            // Result now exposes `repo_slug` (Task 1.1).
            Self::StartPlan(_) => "v2",
            // Result now exposes `repo_slug` (Task 1.1).
            Self::StatusPlan(_) => "v2",
            // Result now exposes `repo_slug` (Task 1.1) + `pr_groups`
            // (Task 1.3); dispatch records gain `worktree_abs_path`
            // (Task 1.4).
            Self::StartSprint(_) => "v2",
            // v2 lifecycle record subcommand surface (Sprint 3): live
            // open/post/repair-dashboard/close, structured payloads, and
            // strict closeout gate. The result envelope grew live operation
            // fields (`issue.url`, `comments.*`, `closeout_url`,
            // `final_dashboard`).
            Self::Record(_) => "v2",
            Self::Tracking(_) => "v1",
            _ => "v1",
        };
        format!(
            "plan-issue-cli.{}.{suffix}",
            self.command_id().replace('-', ".")
        )
    }

    pub fn payload(&self) -> Value {
        let payload = match self {
            Self::BuildTaskSpec(args) => serde_json::to_value(args),
            Self::BuildPlanTaskSpec(args) => serde_json::to_value(args),
            Self::StartPlan(args) => serde_json::to_value(args),
            Self::StatusPlan(args) => serde_json::to_value(args),
            Self::LinkPr(args) => serde_json::to_value(args),
            Self::ReadyPlan(args) => serde_json::to_value(args),
            Self::ClosePlan(args) => serde_json::to_value(args),
            Self::CleanupWorktrees(args) => serde_json::to_value(args),
            Self::StartSprint(args) => serde_json::to_value(args),
            Self::ReadySprint(args) => serde_json::to_value(args),
            Self::AcceptSprint(args) => serde_json::to_value(args),
            Self::MultiSprintGuide(args) => serde_json::to_value(args),
            Self::ResolveApproval(args) => serde_json::to_value(args),
            Self::Record(args) => serde_json::to_value(args),
            Self::Tracking(args) => serde_json::to_value(args),
            Self::Completion(args) => serde_json::to_value(args),
        };

        payload.unwrap_or(Value::Null)
    }

    pub fn validate(&self, dry_run: bool) -> Result<(), ValidationError> {
        match self {
            Self::BuildTaskSpec(args) => validate_grouping(&args.grouping),
            Self::BuildPlanTaskSpec(args) => validate_grouping(&args.grouping),
            Self::StartPlan(args) => validate_grouping(&args.grouping),
            // Sprint commands may infer `--strategy` / `--default-pr-grouping`
            // from the plan markdown's `pr-grouping` metadata at runtime
            // (Task 1.2), so the no-flag deterministic path is intentionally
            // permitted here; the runtime resolver enforces the real
            // requirement once the plan has been read.
            Self::StartSprint(args) => validate_grouping_with_plan_inference(&args.grouping),
            Self::ReadySprint(args) => validate_grouping_with_plan_inference(&args.grouping),
            Self::AcceptSprint(args) => validate_grouping_with_plan_inference(&args.grouping),
            Self::ClosePlan(args) => validate_close_plan_args(args, dry_run),
            Self::LinkPr(args) => validate_link_pr_args(args),
            Self::MultiSprintGuide(args) => validate_multi_sprint_guide_args(args),
            Self::Record(_) => Ok(()),
            Self::Tracking(_) => Ok(()),
            Self::Completion(_)
            | Self::StatusPlan(_)
            | Self::ReadyPlan(_)
            | Self::ResolveApproval(_)
            | Self::CleanupWorktrees(_) => Ok(()),
        }
    }
}

impl RecordArgs {
    pub fn command_id(&self) -> &'static str {
        match &self.command {
            record::RecordCommand::Open(_) => "record.open",
            record::RecordCommand::Attach(_) => "record.attach",
            record::RecordCommand::Post(_) => "record.post",
            record::RecordCommand::RepairDashboard(_) => "record.repair-dashboard",
            record::RecordCommand::Close(_) => "record.close",
            record::RecordCommand::Audit(_) => "record.audit",
            record::RecordCommand::Template(_) => "record.template",
        }
    }
}

impl TrackingArgs {
    pub fn command_id(&self) -> &'static str {
        match &self.command {
            tracking::TrackingCommand::Status(_) => "tracking.status",
        }
    }
}

fn validate_grouping(grouping: &GroupingArgs) -> Result<(), ValidationError> {
    match grouping.strategy {
        SplitStrategy::Deterministic => {
            let Some(pr_grouping) = grouping.pr_grouping else {
                return Err(ValidationError::new(
                    "invalid-pr-grouping",
                    "--strategy deterministic requires --pr-grouping <per-sprint|group>",
                ));
            };
            if grouping.default_pr_grouping.is_some() {
                return Err(ValidationError::new(
                    "invalid-pr-grouping",
                    "--default-pr-grouping is only valid when --strategy auto",
                ));
            }
            match (pr_grouping, grouping.pr_group.is_empty()) {
                (PrGrouping::PerSprint, false) => Err(ValidationError::new(
                    "invalid-pr-grouping",
                    "--pr-group is only valid when --pr-grouping group",
                )),
                (PrGrouping::Group, true) => Err(ValidationError::new(
                    "invalid-pr-grouping",
                    "--pr-grouping group with --strategy deterministic requires --pr-group mappings",
                )),
                _ => Ok(()),
            }
        }
        SplitStrategy::Auto => {
            if grouping.pr_grouping.is_some() {
                return Err(ValidationError::new(
                    "invalid-pr-grouping",
                    "--pr-grouping cannot be used with --strategy auto; use sprint metadata or --default-pr-grouping",
                ));
            }
            Ok(())
        }
    }
}

/// Validate grouping for sprint commands that may infer flags from the
/// plan's per-sprint `pr-grouping` metadata (Task 1.2). Identical to
/// `validate_grouping` except the "deterministic with no `--pr-grouping`"
/// case is permitted: the runtime resolver in `execute::run_*_sprint`
/// either substitutes plan-derived defaults or surfaces a richer error.
fn validate_grouping_with_plan_inference(grouping: &GroupingArgs) -> Result<(), ValidationError> {
    // No-flag path that downstream inference will fill in.
    if grouping.strategy == SplitStrategy::Deterministic
        && grouping.pr_grouping.is_none()
        && grouping.default_pr_grouping.is_none()
        && grouping.pr_group.is_empty()
    {
        return Ok(());
    }
    validate_grouping(grouping)
}

fn validate_close_plan_args(args: &ClosePlanArgs, dry_run: bool) -> Result<(), ValidationError> {
    if args.issue.is_some() && args.body_file.is_some() {
        return Err(ValidationError::new(
            "conflicting-issue-source",
            "use either --issue or --body-file for close-plan, not both",
        ));
    }

    if dry_run && args.body_file.is_none() {
        return Err(ValidationError::new(
            "missing-body-file",
            "--body-file is required for close-plan --dry-run",
        ));
    }

    if !dry_run && args.issue.is_none() {
        return Err(ValidationError::new(
            "missing-issue",
            "--issue is required for close-plan",
        ));
    }

    if !dry_run && args.body_file.is_some() {
        return Err(ValidationError::new(
            "invalid-body-file-mode",
            "--body-file is only supported with --dry-run",
        ));
    }

    Ok(())
}

fn validate_link_pr_args(args: &LinkPrArgs) -> Result<(), ValidationError> {
    let pr = args.pr.trim();
    if issue_body::parse_pr_number(pr).is_none() {
        return Err(ValidationError::new(
            "invalid-pr-reference",
            "--pr must be a concrete PR reference (`#123`, `123`, or GitHub pull URL)",
        ));
    }

    if let Some(task) = args.task.as_deref()
        && task.trim().is_empty()
    {
        return Err(ValidationError::new(
            "invalid-task-id",
            "--task cannot be empty",
        ));
    }

    if let Some(group) = args.pr_group.as_deref()
        && group.trim().is_empty()
    {
        return Err(ValidationError::new(
            "invalid-pr-group",
            "--pr-group cannot be empty",
        ));
    }

    Ok(())
}

fn validate_multi_sprint_guide_args(args: &MultiSprintGuideArgs) -> Result<(), ValidationError> {
    if let Some(to_sprint) = args.to_sprint
        && to_sprint < args.from_sprint
    {
        return Err(ValidationError::new(
            "invalid-sprint-range",
            "--from-sprint must be <= --to-sprint",
        ));
    }
    Ok(())
}

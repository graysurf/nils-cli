use std::path::PathBuf;

use clap::{ArgGroup, Args, ValueEnum};
use serde::Serialize;

use super::{CommentModeArgs, CommentTextArgs, GroupingArgs, PrefixArgs, SummaryArgs};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
pub enum CloseReason {
    Completed,
    #[value(name = "not-planned")]
    NotPlanned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
pub enum LinkPrStatus {
    #[value(name = "planned")]
    Planned,
    #[value(name = "in-progress")]
    InProgress,
    #[value(name = "blocked")]
    Blocked,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct StartPlanArgs {
    /// Plan markdown path.
    #[arg(long, value_name = "path")]
    pub plan: PathBuf,

    /// Override plan issue title.
    #[arg(long, value_name = "text")]
    pub title: Option<String>,

    /// Plan task-spec output path override.
    #[arg(long, value_name = "path")]
    pub task_spec_out: Option<PathBuf>,

    /// Rendered plan issue body output path override.
    #[arg(long, value_name = "path")]
    pub issue_body_out: Option<PathBuf>,

    #[command(flatten)]
    pub prefixes: PrefixArgs,

    #[command(flatten)]
    pub grouping: GroupingArgs,

    /// Labels to add at issue creation time.
    #[arg(long = "label", value_name = "name", default_values = ["issue", "plan"])]
    pub label: Vec<String>,
}

#[derive(Debug, Clone, Args, Serialize)]
#[command(group(
    ArgGroup::new("issue_source")
        .required(true)
        .args(["issue", "body_file"])
))]
#[command(group(
    ArgGroup::new("target_scope")
        .required(true)
        .args(["task", "sprint"])
))]
pub struct LinkPrArgs {
    /// Plan issue number (live `plan-issue` path only).
    #[arg(long, value_name = "number")]
    pub issue: Option<u64>,

    /// Offline issue body path.
    #[arg(long, value_name = "path")]
    pub body_file: Option<PathBuf>,

    /// Target task row id (for example `S2T3`). Shared-lane rows sync automatically.
    #[arg(long, value_name = "task-id")]
    pub task: Option<String>,

    /// Target sprint rows. If multiple shared lanes exist, use `--pr-group`.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..), value_name = "number")]
    pub sprint: Option<u16>,

    /// Target PR group within the sprint (`pr-shared` lane selector).
    #[arg(
        long = "pr-group",
        value_name = "group",
        requires = "sprint",
        conflicts_with = "task"
    )]
    pub pr_group: Option<String>,

    /// PR reference (`#123`, `123`, or GitHub pull URL).
    #[arg(long, value_name = "pr")]
    pub pr: String,

    /// Row status to apply after linking PR (default: `in-progress`).
    #[arg(long, value_enum, default_value_t = LinkPrStatus::InProgress)]
    pub status: LinkPrStatus,
}

#[derive(Debug, Clone, Args, Serialize)]
#[command(group(
    ArgGroup::new("issue_source")
        .required(true)
        .args(["issue", "body_file"])
))]
pub struct StatusPlanArgs {
    /// Plan issue number (live `plan-issue` path only).
    #[arg(long, value_name = "number")]
    pub issue: Option<u64>,

    /// Offline issue body path.
    #[arg(long, value_name = "path")]
    pub body_file: Option<PathBuf>,

    #[command(flatten)]
    pub comment_mode: CommentModeArgs,
}

#[derive(Debug, Clone, Args, Serialize)]
#[command(group(
    ArgGroup::new("issue_source")
        .required(true)
        .args(["issue", "body_file"])
))]
pub struct ReadyPlanArgs {
    /// Plan issue number (live `plan-issue` path only).
    #[arg(long, value_name = "number")]
    pub issue: Option<u64>,

    /// Offline issue body path.
    #[arg(long, value_name = "path")]
    pub body_file: Option<PathBuf>,

    #[command(flatten)]
    pub summary: SummaryArgs,

    /// Apply label updates in live mode.
    #[arg(long)]
    pub label_update: bool,

    /// Review label to add when `--label-update` is set.
    #[arg(long = "label", value_name = "name", requires = "label_update")]
    pub label: Option<String>,

    /// Labels to remove when `--label-update` is set.
    #[arg(long = "remove-label", value_name = "name", requires = "label_update")]
    pub remove_label: Vec<String>,

    #[command(flatten)]
    pub comment_mode: CommentModeArgs,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct ClosePlanArgs {
    /// Plan issue number (`--issue`-only path is live `plan-issue` only).
    #[arg(long, value_name = "number")]
    pub issue: Option<u64>,

    /// Local issue body path (dry-run mode).
    #[arg(long, value_name = "path")]
    pub body_file: Option<PathBuf>,

    /// Final approval comment URL.
    #[arg(long = "approved-comment-url", value_name = "url")]
    pub approved_comment_url: String,

    /// Close reason.
    #[arg(long, value_enum, default_value_t = CloseReason::Completed)]
    pub reason: CloseReason,

    #[command(flatten)]
    pub close_comment: CommentTextArgs,

    /// Allow closing when some tasks are not done.
    #[arg(long)]
    pub allow_not_done: bool,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct CleanupWorktreesArgs {
    /// Plan issue number (live `plan-issue` only).
    #[arg(long, value_name = "number")]
    pub issue: u64,
}

/// Args for the `resolve-approval` subcommand (Task 1.5).
///
/// Wraps the orchestrator pattern of finding the latest review-evidence
/// comment whose body contains `Decision: merge` on a PR and returning its
/// `html_url`.
#[derive(Debug, Clone, Args, Serialize)]
pub struct ResolveApprovalArgs {
    /// PR number (live `plan-issue` path only).
    #[arg(long, value_name = "number")]
    pub pr: u64,
}

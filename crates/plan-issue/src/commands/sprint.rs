use std::path::PathBuf;

use clap::Args;
use serde::Serialize;

use super::{CommentModeArgs, GroupingArgs, PrefixArgs, SummaryArgs};

#[derive(Debug, Clone, Args, Serialize)]
pub struct StartSprintArgs {
    /// Plan markdown path.
    #[arg(long, value_name = "path")]
    pub plan: PathBuf,

    /// Plan issue number.
    #[arg(long, value_name = "number")]
    pub issue: u64,

    /// Sprint number.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..), value_name = "number")]
    pub sprint: u16,

    /// Sprint task-spec output path override.
    #[arg(long, value_name = "path")]
    pub task_spec_out: Option<PathBuf>,

    /// start-sprint output directory for subagent task prompts.
    #[arg(long, value_name = "path")]
    pub subagent_prompts_out: Option<PathBuf>,

    #[command(flatten)]
    pub prefixes: PrefixArgs,

    #[command(flatten)]
    pub grouping: GroupingArgs,

    #[command(flatten)]
    pub comment_mode: CommentModeArgs,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct ReadySprintArgs {
    /// Plan markdown path.
    #[arg(long, value_name = "path")]
    pub plan: PathBuf,

    /// Plan issue number.
    #[arg(long, value_name = "number")]
    pub issue: u64,

    /// Sprint number.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..), value_name = "number")]
    pub sprint: u16,

    /// Sprint task-spec output path override.
    #[arg(long, value_name = "path")]
    pub task_spec_out: Option<PathBuf>,

    #[command(flatten)]
    pub prefixes: PrefixArgs,

    #[command(flatten)]
    pub grouping: GroupingArgs,

    #[command(flatten)]
    pub summary: SummaryArgs,

    #[command(flatten)]
    pub comment_mode: CommentModeArgs,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct AcceptSprintArgs {
    /// Plan markdown path.
    #[arg(long, value_name = "path")]
    pub plan: PathBuf,

    /// Plan issue number.
    #[arg(long, value_name = "number")]
    pub issue: u64,

    /// Sprint number.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..), value_name = "number")]
    pub sprint: u16,

    /// Sprint task-spec output path override.
    #[arg(long, value_name = "path")]
    pub task_spec_out: Option<PathBuf>,

    #[command(flatten)]
    pub prefixes: PrefixArgs,

    #[command(flatten)]
    pub grouping: GroupingArgs,

    /// Review approval comment URL.
    #[arg(long = "approved-comment-url", value_name = "url")]
    pub approved_comment_url: String,

    #[command(flatten)]
    pub summary: SummaryArgs,

    #[command(flatten)]
    pub comment_mode: CommentModeArgs,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct MultiSprintGuideArgs {
    /// Plan markdown path.
    #[arg(long, value_name = "path")]
    pub plan: PathBuf,

    /// First sprint to include.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..), default_value_t = 1)]
    pub from_sprint: u16,

    /// Last sprint to include.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..), value_name = "number")]
    pub to_sprint: Option<u16>,
}

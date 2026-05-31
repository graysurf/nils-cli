use std::path::PathBuf;

use clap::Args;
use serde::Serialize;

use super::{GroupingArgs, PrefixArgs};

#[derive(Debug, Clone, Args, Serialize)]
pub struct BuildTaskSpecArgs {
    /// Plan markdown path.
    #[arg(long, value_name = "path")]
    pub plan: PathBuf,

    /// Sprint number.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..), value_name = "number")]
    pub sprint: u16,

    /// Output task-spec TSV path.
    #[arg(long, value_name = "path")]
    pub task_spec_out: Option<PathBuf>,

    #[command(flatten)]
    pub prefixes: PrefixArgs,

    #[command(flatten)]
    pub grouping: GroupingArgs,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct BuildPlanTaskSpecArgs {
    /// Plan markdown path.
    #[arg(long, value_name = "path")]
    pub plan: PathBuf,

    /// Output task-spec TSV path.
    #[arg(long, value_name = "path")]
    pub task_spec_out: Option<PathBuf>,

    #[command(flatten)]
    pub prefixes: PrefixArgs,

    #[command(flatten)]
    pub grouping: GroupingArgs,
}

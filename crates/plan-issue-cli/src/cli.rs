use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use crate::ValidationError;
use crate::commands::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Parser)]
#[command(
    version,
    about = "Rust implementation of the plan-issue orchestration workflow.",
    after_help = "Usage paths:\n  - plan-issue: live GitHub-backed orchestration\n  - plan-issue-local: local-first rehearsal and dry-run flow\n\nUnsupported in plan-issue-local:\n  - Any --issue path that requires live GitHub reads/writes (for example: status-plan/ready-plan with --issue, close-plan with --issue-only, cleanup-worktrees).\n\nUse instead:\n  - plan-issue <command> ...        (live GitHub path)\n  - --body-file + --dry-run flows   (local rehearsal path where supported)\n\nRuntime workspace:\n  - Pass --state-dir <PATH> to override the workspace root, or export PLAN_ISSUE_HOME.\n  - Default: ${XDG_STATE_HOME:-$HOME/.local/state}/plan-issue.\n\nBoth binaries share the same typed command contract.",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Pass-through repository target for GitHub operations.
    #[arg(long, global = true, value_name = "owner/repo")]
    pub repo: Option<String>,

    /// Print write actions without mutating GitHub state.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Bypass markdown payload guard for GitHub body/comment writes.
    #[arg(short = 'f', long, global = true)]
    pub force: bool,

    /// Hidden alias for `--format json` (kept for backwards compatibility).
    #[arg(long, global = true, hide = true)]
    pub json: bool,

    /// Output format.
    #[arg(long, global = true, value_enum)]
    pub format: Option<OutputFormat>,

    /// Runtime workspace root. Overrides $PLAN_ISSUE_HOME and the
    /// $XDG_STATE_HOME/plan-issue default. Artefacts land under
    /// `<state-dir>/out/plan-issue-delivery/...`.
    #[arg(long, global = true, value_name = "PATH")]
    pub state_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    pub fn resolve_output_format(&self) -> Result<OutputFormat, ValidationError> {
        if self.json && matches!(self.format, Some(OutputFormat::Text)) {
            return Err(ValidationError::new(
                "invalid-output-mode",
                "--json cannot be combined with --format text",
            ));
        }

        if self.json || matches!(self.format, Some(OutputFormat::Json)) {
            return Ok(OutputFormat::Json);
        }

        Ok(OutputFormat::Text)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        self.command.validate(self.dry_run)
    }
}

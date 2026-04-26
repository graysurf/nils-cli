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
    after_help = "Usage paths:\n  - plan-issue: live GitHub-backed orchestration\n  - plan-issue-local: local-first rehearsal and dry-run flow\n\nUnsupported in plan-issue-local:\n  - Any --issue path that requires live GitHub reads/writes (for example: status-plan/ready-plan with --issue, close-plan with --issue-only, cleanup-worktrees).\n\nUse instead:\n  - plan-issue <command> ...        (live GitHub path)\n  - --body-file + --dry-run flows   (local rehearsal path where supported)\n\nAdapter opt-outs:\n  - PLAN_ISSUE_SKIP_INIT_SNAPSHOT=1 makes start-plan / start-sprint skip the canonical *-init.snapshot.md copy entirely; intended for runtime adapters (such as Claude Code) that ship their own role/protocol prompts. Codex / opencode adapters must not set this.\n\nBoth binaries share the same typed command contract.",
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

    /// Output machine-readable JSON (alias for --format json).
    #[arg(long, global = true)]
    pub json: bool,

    /// Output format.
    #[arg(long, global = true, value_enum)]
    pub format: Option<OutputFormat>,

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

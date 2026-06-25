use clap::{Args, Parser, Subcommand, ValueHint};
use nils_common::cli_contract::OutputFormat;

#[derive(Debug, Parser)]
#[command(
    name = "agent-memory",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Resolve and manage local agent memory directories.",
    long_about = "Resolve and manage the git-backed local agent memory store used by Claude Code personas and per-agent memory scopes.",
    after_help = "SCOPE VALUES:\n  root          AGENT_MEMORY_HOME itself\n  global        shared memory store\n  <id>          shorthand for agents/<id>\n  agents/<id>   per-agent memory store\n  personas/<id> persona launchpad directory\n\nENVIRONMENT:\n  AGENT_MEMORY_HOME  Override memory-store root.\n  XDG_CONFIG_HOME    Parent for the default agent-memory root.\n  HOME               Fallback parent when XDG_CONFIG_HOME is unset.\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error",
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print directory for a scope.
    Path(ScopeArgs),
    /// List markdown files in a scope.
    #[command(alias = "ls")]
    List(ScopeArgs),
    /// Print MEMORY.md from a scope.
    #[command(alias = "idx")]
    Index(ScopeArgs),
    /// List registered per-agent scopes.
    Agents,
    /// List registered Claude Code personas.
    Personas,
    /// Create a per-agent memory scope.
    InitAgent(IdArgs),
    /// Scaffold a Claude Code persona launchpad.
    InitPersona(IdArgs),
    /// Print global and per-agent paths.
    Resolve(IdArgs),
    /// Emit shell exports for the resolved layout.
    Env,
    /// Verify memory-store layout.
    Doctor,
    /// Check structural integrity of a memory scope.
    Check(CheckArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
    /// Print help.
    Help,
}

#[derive(Debug, Args)]
pub struct ScopeArgs {
    /// Scope to resolve.
    #[arg(value_name = "SCOPE", value_hint = ValueHint::DirPath)]
    pub scope: Option<String>,
}

#[derive(Debug, Args)]
pub struct IdArgs {
    /// Agent or persona ID.
    #[arg(value_name = "ID")]
    pub id: String,
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Scope to check (default: global).
    #[arg(value_name = "SCOPE", value_hint = ValueHint::DirPath)]
    pub scope: Option<String>,
    /// Check every memory scope (global, agents, personas).
    #[arg(long)]
    pub all: bool,
    /// Promote warn-level findings to failures.
    #[arg(long)]
    pub strict: bool,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json` (kept for convenience).
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    #[arg(value_enum)]
    pub shell: crate::completion::CompletionShell,
}

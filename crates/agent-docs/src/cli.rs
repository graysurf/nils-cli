use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::model::{AuditTarget, FallbackMode, OutputFormat, Product, Scope};

#[derive(Debug, Parser)]
#[command(
    name = "agent-docs",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Data-driven required-doc resolution and auditing for agent workflows",
    long_about = "Resolve and audit the documents and validation contract a repository declares in its AGENT_DOCS.toml catalog. Policy is data the repo owns; this binary is a generic resolver and auditor.",
    after_help = "EXAMPLES:\n  agent-docs audit --target all --strict\n  agent-docs preflight --intent project-dev --format json\n  agent-docs init --print\n  agent-docs list\n  agent-docs explain --intent project-dev\n  agent-docs completion zsh\n\nENVIRONMENT:\n  AGENT_DOCS_HOME  Docs-home fallback when --docs-home is omitted and no\n                   install symlink resolves.\n  PROJECT_PATH     Default project root when --project-path is omitted.\n\nDOCS-HOME RESOLUTION:\n  --docs-home flag, else the install symlink (dirname of\n  ~/.claude/CLAUDE.md or ~/.codex/AGENTS.md), else AGENT_DOCS_HOME.\n\nEXIT CODES:\n  0   success\n  1   strict failure (unsatisfied required docs / audit problems)\n  3   catalog (config) error\n  4   runtime error\n  64  command-line usage error\n  65  undeclared intent when preflight --require-declared-intent is set",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Override the docs-home root (otherwise derived from the install symlink)"
    )]
    pub docs_home: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Override the project root path"
    )]
    pub project_path: Option<PathBuf>,

    #[arg(
        long = "worktree-fallback",
        global = true,
        value_enum,
        default_value_t = FallbackMode::Auto,
        value_name = "MODE",
        help = "Project worktree fallback mode",
        long_help = "Project worktree fallback mode. auto enables linked-worktree fallback to the primary worktree; local-only disables fallback and enforces local project files only."
    )]
    pub worktree_fallback: FallbackMode,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Audit repo health: symlink wiring, declared-doc validity, catalog validity.
    Audit(AuditArgs),
    /// Resolve the doc set and validation contract for an intent (for hooks).
    Preflight(PreflightArgs),
    /// Emit an annotated project-local override stub.
    Init(InitArgs),
    /// Explain what an intent resolves to and why.
    Explain(ExplainArgs),
    /// List the declared documents, validation contracts, and intents.
    List(ListArgs),
    /// Remove a `[[document]]` entry from the project catalog.
    Remove(RemoveArgs),
    /// Generate shell completion scripts.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
pub struct AuditArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = AuditTarget::All,
        help = "Audit scope target"
    )]
    pub target: AuditTarget,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Output format"
    )]
    pub format: OutputFormat,

    #[arg(long, help = "Exit non-zero when the audit finds problems")]
    pub strict: bool,

    #[arg(long, value_enum, help = "Filter catalog documents by product")]
    pub product: Option<Product>,
}

#[derive(Debug, Args)]
pub struct PreflightArgs {
    #[arg(
        long,
        value_name = "INTENT",
        help = "Intent to resolve (for example project-dev)"
    )]
    pub intent: String,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Output format"
    )]
    pub format: OutputFormat,

    #[arg(long, help = "Exit non-zero when required docs are unsatisfied")]
    pub strict: bool,

    #[arg(long, value_enum, help = "Filter documents and validation by product")]
    pub product: Option<Product>,

    #[arg(
        long,
        help = "Exit non-zero when the requested intent is not declared by applicable documents or validation"
    )]
    pub require_declared_intent: bool,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(long, help = "Print the stub to stdout without writing (default)")]
    pub print: bool,

    #[arg(
        long = "dry-run",
        help = "Report the target path and stub without writing"
    )]
    pub dry_run: bool,

    #[arg(
        long,
        help = "Write the stub, overwriting any existing AGENT_DOCS.toml"
    )]
    pub force: bool,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Output format for dry-run / write reports"
    )]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct ExplainArgs {
    #[arg(
        long,
        value_name = "INTENT",
        help = "Intent to explain; omit to list available intents"
    )]
    pub intent: Option<String>,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Output format"
    )]
    pub format: OutputFormat,

    #[arg(long, value_enum, help = "Filter documents and validation by product")]
    pub product: Option<Product>,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Output format"
    )]
    pub format: OutputFormat,

    #[arg(long, value_enum, help = "Filter documents and validation by product")]
    pub product: Option<Product>,
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    #[arg(
        long,
        value_name = "INTENT",
        help = "Context/intent of the entry to remove"
    )]
    pub context: String,

    #[arg(long, value_enum, help = "Scope of the entry to remove")]
    pub scope: Scope,

    #[arg(
        long,
        value_name = "PATH",
        help = "Document path of the entry to remove"
    )]
    pub path: PathBuf,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Output format"
    )]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    #[arg(value_enum)]
    pub shell: crate::completion::CompletionShell,
}

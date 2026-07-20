use std::ffi::OsString;
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
    after_help = "EXAMPLES:\n  agent-docs audit --target all --strict\n  agent-docs preflight --intent project-dev --format json\n  agent-docs integration resolve --product claude --format json\n  agent-docs config enroll --catalog /abs/private/catalog.toml\n  agent-docs init --print\n  agent-docs list\n  agent-docs explain --intent project-dev\n  agent-docs completion zsh\n\nENVIRONMENT:\n  AGENT_DOCS_HOME  Docs-home fallback when --docs-home is omitted and no\n                   install symlink resolves.\n  PROJECT_PATH     Default project root when --project-path is omitted.\n  XDG_CONFIG_HOME  Absolute root containing agent-docs/config.toml.\n  HOME             Absolute fallback root for .config/agent-docs/config.toml.\n\nDOCS-HOME RESOLUTION:\n  --docs-home flag, else the install symlink (dirname of\n  ~/.claude/CLAUDE.md or ~/.codex/AGENTS.md), else AGENT_DOCS_HOME.\n\nEXIT CODES:\n  0   success\n  1   strict failure (unsatisfied required docs / audit problems)\n  3   catalog/config error\n  4   runtime or invariant error\n  64  command-line usage error\n  65  stale bound data or required undeclared intent",
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

    #[arg(
        long,
        global = true,
        help = "Select the effective private user catalog for this operation"
    )]
    pub user_config: bool,

    #[arg(
        long,
        global = true,
        value_name = "SHA256",
        requires = "user_config",
        help = "Require the current integration decision to match this fingerprint"
    )]
    pub integration_fingerprint: Option<String>,

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
    /// Manage exact user-local project enrollment and exclusion rules.
    Config(ConfigArgs),
    /// Resolve the typed automatic integration decision for this checkout.
    Integration(IntegrationArgs),
    /// Manage durable selective intent activation scoped to a session, project, and product.
    Session(SessionArgs),
    /// Describe the effect of one exact typed agent-docs invocation.
    #[command(hide = true)]
    OperationEffect(OperationEffectArgs),
    /// Generate shell completion scripts.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
pub struct OperationEffectArgs {
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    pub command: Vec<OsString>,
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
        value_name = "PHASE",
        help = "Filter documents to a workflow phase (no-phase docs always apply)"
    )]
    pub phase: Option<String>,

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

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
pub enum ConfigCommand {
    /// Enroll an external private project catalog for this checkout.
    Enroll(ConfigEnrollArgs),
    /// Exclude this checkout from automatic agent-docs integration.
    Exclude(ConfigRuleArgs),
    /// Show rules matching this checkout.
    Show(ConfigFormatArgs),
    /// List every user-local project rule.
    List(ConfigFormatArgs),
    /// Remove the exact rule for this checkout.
    Remove(ConfigRemoveArgs),
}

#[derive(Debug, Args)]
pub struct ConfigEnrollArgs {
    #[arg(
        long,
        value_name = "PATH",
        help = "Absolute private catalog path outside every target Git worktree"
    )]
    pub catalog: PathBuf,
    #[arg(long, help = "Match every worktree belonging to this local clone")]
    pub all_worktrees: bool,
    #[arg(long, value_name = "TEXT", help = "Optional local explanation")]
    pub reason: Option<String>,
    #[arg(long, help = "Apply the proposed user-config update")]
    pub apply: bool,
    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Output format"
    )]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct ConfigRuleArgs {
    #[arg(long, help = "Match every worktree belonging to this local clone")]
    pub all_worktrees: bool,
    #[arg(long, value_name = "TEXT", help = "Optional local explanation")]
    pub reason: Option<String>,
    #[arg(long, help = "Apply the proposed user-config update")]
    pub apply: bool,
    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Output format"
    )]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct ConfigRemoveArgs {
    #[arg(long, help = "Match every worktree belonging to this local clone")]
    pub all_worktrees: bool,
    #[arg(long, help = "Apply the proposed user-config update")]
    pub apply: bool,
    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Output format"
    )]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct ConfigFormatArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Output format"
    )]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct IntegrationArgs {
    #[command(subcommand)]
    pub command: IntegrationCommand,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
pub enum IntegrationCommand {
    /// Resolve the automatic integration action and decision fingerprint.
    Resolve(IntegrationResolveArgs),
}

#[derive(Debug, Args)]
pub struct IntegrationResolveArgs {
    #[arg(
        long,
        value_enum,
        help = "Product whose automatic integration decision is being resolved"
    )]
    pub product: Product,
    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Output format"
    )]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct SessionArgs {
    #[command(subcommand)]
    pub command: SessionCommand,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
pub enum SessionCommand {
    /// Strictly preflight and atomically activate one or more declared intents.
    Activate(SessionActivateArgs),
    /// Atomically prepare one or more declared intents (activate + strict
    /// preflight) and report a stable JSON result usable by a runtime hook.
    Prepare(SessionActivateArgs),
    /// Show active intents for the current session/project/product scope.
    Status(SessionCommonArgs),
    /// Re-resolve the catalog and verify required intents are active and fresh.
    Verify(SessionVerifyArgs),
}

#[derive(Debug, Args)]
pub struct SessionCommonArgs {
    #[arg(long = "session-id", value_name = "ID")]
    pub session_id: String,
    #[arg(long, value_enum)]
    pub product: Product,
    #[arg(long = "state-home", value_name = "DIR")]
    pub state_home: PathBuf,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct SessionActivateArgs {
    #[command(flatten)]
    pub common: SessionCommonArgs,
    #[arg(long, value_name = "INTENT", required = true)]
    pub intent: Vec<String>,
    #[arg(
        long,
        value_name = "PHASE",
        help = "Scope preparation to a workflow phase (no-phase docs always apply)"
    )]
    pub phase: Option<String>,
}

#[derive(Debug, Args)]
pub struct SessionVerifyArgs {
    #[command(flatten)]
    pub common: SessionCommonArgs,
    #[arg(long = "require-intent", value_name = "INTENT", required = true)]
    pub require_intent: Vec<String>,
    #[arg(
        long,
        value_name = "PHASE",
        help = "Verify a phase-scoped or full preparation for the required intents"
    )]
    pub phase: Option<String>,
}

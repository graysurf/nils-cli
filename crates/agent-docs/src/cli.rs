use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::model::{
    BaselineTarget, Context, DocumentWhen, FallbackMode, OutputFormat, ResolveFormat, Scope,
};

#[derive(Debug, Parser)]
#[command(
    name = "agent-docs",
    version,
    about = "Deterministic required-doc discovery for agent workflows",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Override AGENT_DOCS_HOME root path"
    )]
    pub docs_home: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Override project root path"
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
    Resolve(ResolveArgs),
    Contexts(ContextsArgs),
    Add(AddArgs),
    ScaffoldAgents(ScaffoldAgentsArgs),
    Baseline(BaselineArgs),
    ScaffoldBaseline(ScaffoldBaselineArgs),
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
pub struct ResolveArgs {
    #[arg(long, value_enum, help = "Context to resolve")]
    pub context: Context,

    #[arg(
        long,
        value_enum,
        default_value_t = ResolveFormat::Text,
        help = "Output format"
    )]
    pub format: ResolveFormat,

    #[arg(long, help = "Fail when required docs are missing")]
    pub strict: bool,
}

#[derive(Debug, Args)]
pub struct ContextsArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = ResolveFormat::Text,
        help = "Output format"
    )]
    pub format: ResolveFormat,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[arg(long, value_enum, help = "Config target to update")]
    pub target: Scope,

    #[arg(long, value_enum, help = "Context that uses this document")]
    pub context: Context,

    #[arg(long, value_enum, help = "Scope used to resolve relative paths")]
    pub scope: Scope,

    #[arg(long, value_name = "PATH", help = "Document path to register")]
    pub path: PathBuf,

    #[arg(long, help = "Mark this document as required")]
    pub required: bool,

    #[arg(
        long,
        value_enum,
        default_value_t = DocumentWhen::Always,
        help = "Condition for when the document is required"
    )]
    pub when: DocumentWhen,

    #[arg(long, help = "Optional notes for the document entry")]
    pub notes: Option<String>,
}

#[derive(Debug, Args)]
pub struct ScaffoldAgentsArgs {
    #[arg(long, value_enum, help = "Scaffold target scope")]
    pub target: Scope,

    #[arg(long, value_name = "PATH", help = "Explicit output path")]
    pub output: Option<PathBuf>,

    #[arg(long, help = "Overwrite an existing output file")]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct BaselineArgs {
    #[arg(long, help = "Run baseline check mode")]
    pub check: bool,

    #[arg(
        long,
        value_enum,
        default_value_t = BaselineTarget::All,
        help = "Baseline scope target"
    )]
    pub target: BaselineTarget,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Output format"
    )]
    pub format: OutputFormat,

    #[arg(long, help = "Fail when required baseline docs are missing")]
    pub strict: bool,
}

#[derive(Debug, Args)]
pub struct ScaffoldBaselineArgs {
    #[arg(
        long,
        value_enum,
        default_value_t = BaselineTarget::All,
        help = "Baseline scope target"
    )]
    pub target: BaselineTarget,

    #[arg(long, help = "Create only missing baseline files")]
    pub missing_only: bool,

    #[arg(long, help = "Overwrite existing baseline files")]
    pub force: bool,

    #[arg(long, help = "Preview planned changes without writing files")]
    pub dry_run: bool,

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

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};

#[derive(Debug, Parser)]
#[command(
    name = "agent-out",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Generate and audit canonical AGENT_HOME/out artifact paths.",
    long_about = "Generate canonical project-scoped AGENT_HOME/out run directories and audit existing out entries for workflow artifact hygiene.",
    after_help = "EXAMPLES:\n  agent-out project --topic browser-qa --mkdir\n  agent-out project --repo . --topic release-notes --format json\n  agent-out audit --strict\n  agent-out completion zsh\n\nENVIRONMENT:\n  AGENT_HOME  Default agent home root when --agent-home is omitted.\n  AGENT_OUT_PATH, AGENT_OUT_ROOT, AGENT_OUT_PROJECT_SLUG, AGENT_OUT_TOPIC, AGENT_OUT_RUN_ID  Exported by --format env.\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Generate a canonical project-scoped artifact directory path.
    Project(ProjectArgs),
    /// Audit top-level AGENT_HOME/out entries.
    Audit(AuditArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
pub struct ProjectArgs {
    /// Human-readable topic for this run directory.
    #[arg(long, value_name = "TOPIC")]
    pub topic: String,

    /// Repository path used for slug discovery. Defaults to the current directory.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub repo: Option<PathBuf>,

    /// Explicit repository slug, preferably owner/repo.
    #[arg(long = "repo-slug", value_name = "OWNER/REPO")]
    pub repo_slug: Option<String>,

    /// Agent home root. Defaults to AGENT_HOME.
    #[arg(long = "agent-home", value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub agent_home: Option<PathBuf>,

    /// Create the generated directory.
    #[arg(long)]
    pub mkdir: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = ProjectFormat::Path)]
    pub format: ProjectFormat,
}

#[derive(Debug, Args)]
pub struct AuditArgs {
    /// Agent home root. Defaults to AGENT_HOME.
    #[arg(long = "agent-home", value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub agent_home: Option<PathBuf>,

    /// Exit non-zero when noncanonical entries are present.
    #[arg(long)]
    pub strict: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = AuditFormat::Text)]
    pub format: AuditFormat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ProjectFormat {
    Path,
    Json,
    Env,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum AuditFormat {
    Text,
    Json,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    #[arg(value_enum)]
    pub shell: crate::completion::CompletionShell,
}

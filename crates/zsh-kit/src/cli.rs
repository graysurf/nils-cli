use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum, ValueHint};
use nils_common::cli_contract::OutputFormat;

#[derive(Debug, Parser)]
#[command(
    name = "zsh-kit",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Bootstrap an operator-supplied Zsh repository at runtime.",
    long_about = "Clone or update an operator-supplied Zsh repository, validate its setup hook, optionally write a ZDOTDIR bootstrap, and dispatch shell-specific setup back to the repository.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  zsh-kit setup --repo https://github.com/example/zsh-config.git --dry-run\n  zsh-kit setup --repo git@github.com:example/zsh-config.git --dest ~/.config/zsh --apply\n  zsh-kit setup --repo ./fixtures/zsh --dry-run --format json\n  zsh-kit completion zsh\n\nENVIRONMENT:\n  HOME      Used for the default destination and .zshenv path.\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data\n  69  required resource unavailable"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Clone/update a Zsh repository and dispatch its setup hook.
    Setup(SetupArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("mode")
        .required(true)
        .multiple(false)
        .args(["dry_run", "apply"])
))]
pub struct SetupArgs {
    /// Git repository URL or local path.
    #[arg(long, value_name = "URL_OR_PATH")]
    pub repo: String,

    /// Destination directory. Defaults to $HOME/.config/zsh.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub dest: Option<PathBuf>,

    /// Branch to checkout after clone/update.
    #[arg(long, value_name = "NAME", conflicts_with = "ref_name")]
    pub branch: Option<String>,

    /// Revision, tag, or commit to checkout after clone/update.
    #[arg(long = "ref", value_name = "REV", conflicts_with = "branch")]
    pub ref_name: Option<String>,

    /// Write a managed $HOME/.zshenv that exports ZDOTDIR and setup features.
    #[arg(long)]
    pub write_zshenv: bool,

    /// Comma-separated feature list forwarded to the repo setup hook.
    #[arg(long, value_name = "CSV", value_delimiter = ',')]
    pub features: Vec<String>,

    /// Tool-install policy forwarded to the repo setup hook.
    #[arg(long, value_enum, default_value_t = InstallTools::Skip)]
    pub install_tools: InstallTools,

    /// Preview intended actions without filesystem, git, bootstrap, or hook mutation.
    #[arg(long)]
    pub dry_run: bool,

    /// Mutate the destination and dispatch the repo setup hook.
    #[arg(long)]
    pub apply: bool,

    /// Allow guarded overwrite/update paths such as mismatched remotes or .zshenv replacement.
    #[arg(long)]
    pub force: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum)]
    pub shell: crate::completion::CompletionShell,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum, serde::Serialize)]
#[value(rename_all = "kebab-case")]
#[serde(rename_all = "kebab-case")]
pub enum InstallTools {
    Skip,
    Repo,
}

impl InstallTools {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Repo => "repo",
        }
    }
}

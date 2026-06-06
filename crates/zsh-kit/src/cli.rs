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
    after_help = "EXAMPLES:\n  zsh-kit setup --repo https://github.com/example/zsh-config.git --dry-run\n  zsh-kit setup --repo git@github.com:example/zsh-config.git --dest ~/.config/zsh --apply\n  zsh-kit plugin fetch --entry 'zsh-abbr::zsh-abbr.plugin.zsh::git=https://github.com/olets/zsh-abbr.git'\n  zsh-kit plugin status\n  zsh-kit completion zsh\n\nENVIRONMENT:\n  HOME                    Used for default destination and fallback paths.\n  ZSH_PLUGINS_DIR         Default plugin directory for plugin commands.\n  ZDOTDIR                 Fallback root used to derive the plugin directory.\n  ZSH_CACHE_DIR           Default cache directory for plugin timestamp state.\n  PLUGIN_UPDATE_FILE      Default plugin timestamp file.\n  PLUGIN_UPDATE_INTERVAL_DAYS  Default plugin auto-update interval.\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data\n  69  required resource unavailable"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Clone/update a Zsh repository and dispatch its setup hook.
    Setup(SetupArgs),
    /// Manage zsh-kit plugin fetch/update helpers.
    Plugin(PluginArgs),
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

#[derive(Debug, Args)]
pub struct PluginArgs {
    #[command(subcommand)]
    pub command: PluginCommand,
}

#[derive(Debug, Subcommand)]
pub enum PluginCommand {
    /// Ensure one plugins.list entry exists under the plugin directory.
    Fetch(PluginFetchArgs),
    /// Fast-forward all git plugins under the plugin directory.
    Update(PluginUpdateArgs),
    /// Run update when the timestamp is older than the configured interval.
    MaybeUpdate(PluginMaybeUpdateArgs),
    /// Print plugin auto-update status.
    Status(PluginStatusArgs),
}

#[derive(Debug, Args)]
pub struct PluginFetchArgs {
    /// Raw config/plugins.list entry to fetch.
    #[arg(long, value_name = "ENTRY")]
    pub entry: String,

    /// Plugin base directory. Defaults to $ZSH_PLUGINS_DIR or $ZDOTDIR/plugins.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub plugins_dir: Option<PathBuf>,

    /// Print intended actions without cloning, deleting, or updating submodules.
    #[arg(long)]
    pub dry_run: bool,

    /// Delete an existing plugin directory before cloning.
    #[arg(long)]
    pub force: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct PluginUpdateArgs {
    /// Plugin base directory. Defaults to $ZSH_PLUGINS_DIR or $ZDOTDIR/plugins.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub plugins_dir: Option<PathBuf>,

    /// Print intended git pull commands without mutating repositories.
    #[arg(long)]
    pub dry_run: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct PluginMaybeUpdateArgs {
    /// Plugin base directory. Defaults to $ZSH_PLUGINS_DIR or $ZDOTDIR/plugins.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub plugins_dir: Option<PathBuf>,

    /// Timestamp file used to track the last auto-update.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub timestamp_file: Option<PathBuf>,

    /// Auto-update interval in whole days.
    #[arg(long, value_name = "DAYS")]
    pub interval_days: Option<u64>,

    /// Print intended git pull commands without mutating repositories.
    #[arg(long)]
    pub dry_run: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct PluginStatusArgs {
    /// Timestamp file used to track the last auto-update.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub timestamp_file: Option<PathBuf>,

    /// Auto-update interval in whole days.
    #[arg(long, value_name = "DAYS")]
    pub interval_days: Option<u64>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
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

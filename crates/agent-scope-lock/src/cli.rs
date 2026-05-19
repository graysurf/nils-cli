use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};

#[derive(Debug, Parser)]
#[command(
    name = "agent-scope-lock",
    version,
    about = "Create and validate deterministic agent edit-scope locks.",
    long_about = "Create, read, validate, and clear repository edit-scope locks so agent changes stay within declared path prefixes.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  agent-scope-lock create --path crates/agent-scope-lock --path Cargo.toml --owner T3\n  agent-scope-lock read\n  agent-scope-lock validate --changes all\n  agent-scope-lock validate --format json\n  agent-scope-lock clear\n\nENVIRONMENT:\n  none\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a lock with allowed repo-relative path prefixes.
    Create(CreateArgs),
    /// Read the current lock.
    Read(CommonArgs),
    /// Validate changed paths against the current lock.
    Validate(ValidateArgs),
    /// Remove the current lock if present.
    Clear(CommonArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
pub struct CommonArgs {
    /// Lock file path. Defaults to `git rev-parse --git-path agent-scope-lock.json`.
    #[arg(long = "lock-file", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub lock_file: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Allowed repo-relative path or prefix. Repeat for multiple scopes.
    #[arg(long = "path", value_name = "PATH", value_hint = ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,

    /// Optional lock owner label.
    #[arg(long, value_name = "OWNER")]
    pub owner: Option<String>,

    /// Optional lock note.
    #[arg(long, value_name = "NOTE")]
    pub note: Option<String>,

    /// Overwrite an existing lock file.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct ValidateArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Changed path set to validate.
    #[arg(long, value_enum, default_value_t = ChangeMode::All)]
    pub changes: ChangeMode,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum)]
    pub shell: crate::completion::CompletionShell,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ChangeMode {
    All,
    Staged,
    Unstaged,
}

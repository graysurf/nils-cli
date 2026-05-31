use clap::{Args, ValueEnum};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum, value_name = "shell")]
    pub shell: CompletionShell,
}

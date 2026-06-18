//! Shell completion generation (clap-first).
//!
//! Completion is generated directly from the clap command model and emitted
//! verbatim; the committed assets in `completions/{bash,zsh}` are the exact
//! output of `github-app-cli completion <shell>` and verified by
//! `scripts/ci/completion-freshness-audit.sh`.

use std::io;

use clap::CommandFactory;
use clap_complete::{Shell, generate};
use nils_common::cli_contract::exit;

/// Shells for which a completion script can be generated.
#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum CompletionShell {
    Bash,
    Zsh,
}

/// Emit the completion script for `shell` to stdout.
pub fn run(shell: CompletionShell) -> i32 {
    let generator = match shell {
        CompletionShell::Bash => Shell::Bash,
        CompletionShell::Zsh => Shell::Zsh,
    };
    let mut command = crate::cli::Cli::command();
    let bin_name = command.get_name().to_string();
    generate(generator, &mut command, bin_name, &mut io::stdout());
    exit::SUCCESS
}

use clap::CommandFactory;
use clap_complete::{Shell, generate};

use crate::cli::{Cli, CompletionShell};

pub fn run(shell: CompletionShell) -> i32 {
    let mut command = Cli::command();
    let shell = match shell {
        CompletionShell::Bash => Shell::Bash,
        CompletionShell::Zsh => Shell::Zsh,
    };
    generate(shell, &mut command, "agent-hook", &mut std::io::stdout());
    0
}

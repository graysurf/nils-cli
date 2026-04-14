use clap::{CommandFactory, ValueEnum};
use clap_complete::{Shell, generate};
use std::io::{self, Write};

use crate::cli::Cli;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
}

pub fn run(shell: CompletionShell) -> i32 {
    let mut command = Cli::command();
    let bin_name = command.get_name().to_string();

    match shell {
        CompletionShell::Bash => print_completion(Shell::Bash, &mut command, &bin_name),
        CompletionShell::Zsh => print_completion(Shell::Zsh, &mut command, &bin_name),
    }

    0
}

fn print_completion(generator: Shell, command: &mut clap::Command, bin_name: &str) {
    if matches!(generator, Shell::Bash) {
        let mut output = Vec::new();
        generate(generator, command, bin_name, &mut output);
        let normalized = normalize_bash_completion(
            String::from_utf8(output).expect("bash completion should be valid UTF-8"),
        );
        io::stdout()
            .write_all(normalized.as_bytes())
            .expect("failed to write bash completion");
        return;
    }

    generate(generator, command, bin_name, &mut io::stdout());
}

fn normalize_bash_completion(script: String) -> String {
    script.replace("__subcmd__", "__")
}

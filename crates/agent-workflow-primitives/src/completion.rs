use std::io;

use clap::{CommandFactory, ValueEnum};
use clap_complete::{Shell, generate};

use crate::common::EXIT_OK;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CompletionShell {
    Bash,
    Zsh,
}

pub fn run<C: CommandFactory>(shell: CompletionShell, bin_name: &'static str) -> i32 {
    let mut command = C::command();
    match shell {
        CompletionShell::Bash => print_completion(Shell::Bash, &mut command, bin_name),
        CompletionShell::Zsh => print_completion(Shell::Zsh, &mut command, bin_name),
    }
    EXIT_OK
}

fn print_completion(generator: Shell, command: &mut clap::Command, bin_name: &'static str) {
    let mut output = Vec::new();
    generate(generator, command, bin_name, &mut output);
    match generator {
        Shell::Bash => {
            let normalized = normalize_bash_completion(
                String::from_utf8(output).expect("bash completion should be valid UTF-8"),
            );
            io::Write::write_all(&mut io::stdout(), normalized.as_bytes())
                .expect("failed to write bash completion");
        }
        _ => io::Write::write_all(&mut io::stdout(), &output).expect("failed to write completion"),
    }
}

fn normalize_bash_completion(script: String) -> String {
    script
        .replace("__subcmd__", "__")
        .replace("complete -F _", "complete -o nospace -F _")
}

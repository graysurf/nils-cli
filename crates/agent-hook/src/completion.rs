use std::io::{self, Write};

use clap::CommandFactory;
use clap_complete::{Shell, generate};

use crate::cli::{Cli, CompletionShell};

pub fn run(shell: CompletionShell) -> i32 {
    let mut command = Cli::command();
    let bin_name = command.get_name().to_string();
    match shell {
        CompletionShell::Bash => {
            let mut output = Vec::new();
            generate(Shell::Bash, &mut command, &bin_name, &mut output);
            let script = String::from_utf8(output).expect("bash completion should be UTF-8");
            io::stdout()
                .write_all(script.replace("__subcmd__", "__").as_bytes())
                .expect("failed to write bash completion");
        }
        CompletionShell::Zsh => generate(Shell::Zsh, &mut command, &bin_name, &mut io::stdout()),
    }
    0
}

use clap::CommandFactory;
use clap_complete::{Shell, generate};
use std::io::{self, Write};

pub fn run(shell: crate::CompletionShell) -> i32 {
    match shell {
        crate::CompletionShell::Bash => generate_script(Shell::Bash),
        crate::CompletionShell::Zsh => generate_script(Shell::Zsh),
    }
}

fn generate_script(generator: Shell) -> i32 {
    let mut command = crate::Cli::command();
    let bin_name = command.get_name().to_string();
    if matches!(generator, Shell::Bash) {
        let mut output = Vec::new();
        generate(generator, &mut command, bin_name.clone(), &mut output);
        let normalized = normalize_bash_completion(
            String::from_utf8(output).expect("bash completion should be valid UTF-8"),
        );
        io::stdout()
            .write_all(normalized.as_bytes())
            .expect("failed to write bash completion");
        return 0;
    }

    generate(generator, &mut command, bin_name, &mut io::stdout());
    0
}

fn normalize_bash_completion(script: String) -> String {
    script.replace("__subcmd__", "__")
}

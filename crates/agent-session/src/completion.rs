use std::io::{self, Write};

use clap::{CommandFactory, ValueEnum};
use clap_complete::{Shell, generate};

use crate::cli::Cli;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
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

pub(crate) fn print_completion(generator: Shell, command: &mut clap::Command, bin_name: &str) {
    if matches!(generator, Shell::Bash | Shell::Zsh) {
        let mut output = Vec::new();
        generate(generator, command, bin_name, &mut output);
        let script = String::from_utf8(output).expect("completion should be valid UTF-8");
        let normalized = match generator {
            Shell::Bash => normalize_bash_completion(script),
            Shell::Zsh => normalize_zsh_completion(script),
            _ => unreachable!("only bash and zsh are buffered"),
        };
        io::stdout()
            .write_all(normalized.as_bytes())
            .expect("failed to write completion");
        return;
    }

    generate(generator, command, bin_name, &mut io::stdout());
}

fn normalize_bash_completion(script: String) -> String {
    script
        .replace("__subcmd__", "__")
        .replace(" --authenticated", "")
}

fn normalize_zsh_completion(script: String) -> String {
    script
        .split_inclusive('\n')
        .filter(|line| !line.contains("'--authenticated["))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{normalize_bash_completion, normalize_zsh_completion};

    #[test]
    fn bash_completion_hides_the_private_authenticated_broker_status_flag() {
        let script = "opts=\"--session --authenticated --format\"\nagent__subcmd__status\n";
        let normalized = normalize_bash_completion(script.to_string());

        assert_eq!(normalized, "opts=\"--session --format\"\nagent__status\n");
    }

    #[test]
    fn zsh_completion_hides_the_private_authenticated_broker_status_flag() {
        let script = concat!(
            "'--session=[Managed session]:SESSION:_default' \\\n",
            "'--authenticated[Require private authentication]' \\\n",
            "'--format=[Output format]:FORMAT:_default' \\\n",
        );
        let normalized = normalize_zsh_completion(script.to_string());

        assert_eq!(
            normalized,
            concat!(
                "'--session=[Managed session]:SESSION:_default' \\\n",
                "'--format=[Output format]:FORMAT:_default' \\\n",
            )
        );
    }
}

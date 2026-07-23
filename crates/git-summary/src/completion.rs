use clap::{Arg, ArgAction, Command};
use clap_complete::{Shell, generate};
use std::io::{self, Write};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum CompletionShell {
    Bash,
    Zsh,
}

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        None => {
            eprintln!("usage: git-summary completion <bash|zsh>");
            1
        }
        Some("bash") if args.len() == 1 => run_shell(CompletionShell::Bash),
        Some("zsh") if args.len() == 1 => run_shell(CompletionShell::Zsh),
        Some(shell) if args.len() == 1 => {
            eprintln!("git-summary: error: unsupported completion shell '{shell}'");
            eprintln!("usage: git-summary completion <bash|zsh>");
            1
        }
        _ => {
            eprintln!("git-summary: error: expected `git-summary completion <bash|zsh>`");
            1
        }
    }
}

fn run_shell(shell: CompletionShell) -> i32 {
    match shell {
        CompletionShell::Bash => generate_script(Shell::Bash),
        CompletionShell::Zsh => generate_script(Shell::Zsh),
    }
}

fn generate_script(generator: Shell) -> i32 {
    let mut command = build_completion_command();
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

fn build_completion_command() -> Command {
    Command::new("git-summary")
        .version(env!("CARGO_PKG_VERSION"))
        .long_version(nils_build_info::long_version(env!("CARGO_PKG_VERSION")))
        .about("Git history summary CLI")
        .disable_help_subcommand(true)
        .arg(
            Arg::new("format")
                .long("format")
                .value_name("FORMAT")
                .num_args(1)
                .value_parser(["text", "json"])
                .default_value("text")
                .global(true)
                .help("Output format"),
        )
        .arg(
            Arg::new("no-mailmap")
                .long("no-mailmap")
                .action(ArgAction::SetTrue)
                .global(true)
                .help("Show raw commit identities instead of canonical mailmap identities"),
        )
        .arg(
            Arg::new("from")
                .value_name("from")
                .help("Custom range start date (YYYY-MM-DD)")
                .required(false),
        )
        .arg(
            Arg::new("to")
                .value_name("to")
                .help("Custom range end date (YYYY-MM-DD)")
                .required(false),
        )
        .subcommand(Command::new("all").about("Entire history"))
        .subcommand(Command::new("today").about("Today only"))
        .subcommand(Command::new("yesterday").about("Yesterday only"))
        .subcommand(Command::new("this-month").about("1st to today"))
        .subcommand(Command::new("last-month").about("1st to end of last month"))
        .subcommand(Command::new("this-week").about("This Mon-Sun"))
        .subcommand(Command::new("last-week").about("Last Mon-Sun"))
        .subcommand(Command::new("help").about("Display help message for git-summary"))
        .subcommand(
            Command::new("completion")
                .about("Export shell completion script")
                .arg(
                    Arg::new("shell")
                        .value_name("shell")
                        .value_parser(["bash", "zsh"])
                        .required(true),
                ),
        )
}

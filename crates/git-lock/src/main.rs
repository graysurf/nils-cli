use clap::{Parser, Subcommand, ValueEnum};
use nils_common::cli_contract::exit;

mod completion;
mod copy;
mod delete;
mod diff;
mod fs;
mod git;
mod list;
mod lock;
mod lock_view;
mod messages;
mod prompt;
mod store;
mod tag;
mod unlock;

#[derive(Parser)]
#[command(
    name = "git-lock",
    version,
    about = "Save and restore named Git commit locks.",
    long_about = "Save, list, copy, diff, tag, and restore named Git commit locks using a repository-local cache.",
    disable_help_flag = true,
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  git-lock lock release-point\n  git-lock list\n  git-lock diff before after\n  git-lock completion zsh\n\nENVIRONMENT:\n  ZSH_CACHE_DIR  Base cache directory for lock storage.\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Lock {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Unlock {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    List {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Copy {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Delete {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Diff {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Tag {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Completion {
        /// Shell to generate completion script for
        #[arg(value_enum, value_name = "shell")]
        shell: CompletionShell,
    },
    Help,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum CompletionShell {
    Bash,
    Zsh,
}

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 && is_help(&args[1]) {
        messages::print_help();
        return exit::SUCCESS;
    }

    if args.len() > 1 && is_version(&args[1]) {
        println!("git-lock {}", env!("CARGO_PKG_VERSION"));
        return exit::SUCCESS;
    }

    if args.len() > 2
        && is_known_command(&args[1])
        && is_help(&args[2])
        && let Some(text) = messages::subcommand_help(&args[1])
    {
        println!("{text}");
        return exit::SUCCESS;
    }

    if args.len() > 1 && args[1] == "completion" {
        let cli = Cli::parse_from(&args);
        let command = cli.command.unwrap_or(Command::Help);
        return match command {
            Command::Completion { shell } => completion::run(shell),
            _ => {
                messages::print_help();
                exit::USAGE
            }
        };
    }

    if !nils_common::git::is_git_repo().unwrap_or(false) {
        println!("{}", messages::NOT_GIT_REPO);
        return exit::RUNTIME;
    }

    if args.len() <= 1 {
        messages::print_help();
        return exit::SUCCESS;
    }

    if !is_known_command(&args[1]) {
        println!("{}", messages::unknown_command(&args[1]));
        println!("{}", messages::UNKNOWN_COMMAND_HINT);
        return exit::USAGE;
    }

    let cli = Cli::parse_from(&args);

    let result = match cli.command.unwrap_or(Command::Help) {
        Command::Lock { args } => lock::run(&args),
        Command::Unlock { args } => unlock::run(&args),
        Command::List { args } => list::run(&args),
        Command::Copy { args } => copy::run(&args),
        Command::Delete { args } => delete::run(&args),
        Command::Diff { args } => diff::run(&args),
        Command::Tag { args } => tag::run(&args),
        Command::Completion { shell } => Ok(completion::run(shell)),
        Command::Help => {
            messages::print_help();
            Ok(exit::SUCCESS)
        }
    };

    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{err:#}");
            exit::RUNTIME
        }
    }
}

fn is_help(arg: &str) -> bool {
    matches!(arg, "help" | "--help" | "-h")
}

fn is_version(arg: &str) -> bool {
    matches!(arg, "--version" | "-V")
}

fn is_known_command(arg: &str) -> bool {
    matches!(
        arg,
        "lock" | "unlock" | "list" | "copy" | "delete" | "diff" | "tag" | "completion" | "help"
    )
}

#[cfg(test)]
mod tests {
    use super::{is_help, is_known_command, is_version};

    #[test]
    fn is_help_matches_expected_flags() {
        assert!(is_help("help"));
        assert!(is_help("--help"));
        assert!(is_help("-h"));
        assert!(!is_help("lock"));
    }

    #[test]
    fn is_version_matches_expected_flags() {
        assert!(is_version("--version"));
        assert!(is_version("-V"));
        assert!(!is_version("lock"));
    }

    #[test]
    fn is_known_command_accepts_known() {
        assert!(is_known_command("lock"));
        assert!(is_known_command("unlock"));
        assert!(is_known_command("list"));
        assert!(is_known_command("copy"));
        assert!(is_known_command("delete"));
        assert!(is_known_command("diff"));
        assert!(is_known_command("tag"));
        assert!(is_known_command("completion"));
        assert!(is_known_command("help"));
        assert!(!is_known_command("nope"));
    }
}

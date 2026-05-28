use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use nils_common::cli_contract::exit;

mod completion;
mod copy;
mod delete;
mod diff;
mod errors;
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
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Save and restore named Git commit locks.",
    long_about = "Save, list, copy, diff, tag, and restore named Git commit locks using a repository-local cache.",
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
    let parsed = match Cli::try_parse_from(&args) {
        Ok(cli) => Some(cli),
        Err(err) => {
            if matches!(
                err.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                return print_parse_error(err);
            }
            if args.get(1).is_some_and(|arg| arg == "completion") {
                return print_parse_error(err);
            }
            None
        }
    };

    if let Some(Command::Completion { shell }) =
        parsed.as_ref().and_then(|cli| cli.command.as_ref())
    {
        return completion::run(*shell);
    }

    if let Some(Command::Help) = parsed.as_ref().and_then(|cli| cli.command.as_ref()) {
        return print_root_help();
    }

    if !nils_common::git::is_git_repo().unwrap_or(false) {
        println!("{}", messages::NOT_GIT_REPO);
        return exit::RUNTIME;
    }

    if args.len() <= 1 {
        return print_root_help();
    }

    if !is_known_command(&args[1]) {
        println!("{}", messages::unknown_command(&args[1]));
        println!("{}", messages::UNKNOWN_COMMAND_HINT);
        return exit::USAGE;
    }

    let cli = parsed.unwrap_or_else(|| Cli::parse_from(&args));

    let result = match cli.command.unwrap_or(Command::Help) {
        Command::Lock { args } => lock::run(&args),
        Command::Unlock { args } => unlock::run(&args),
        Command::List { args } => list::run(&args),
        Command::Copy { args } => copy::run(&args),
        Command::Delete { args } => delete::run(&args),
        Command::Diff { args } => diff::run(&args),
        Command::Tag { args } => tag::run(&args),
        Command::Completion { shell } => Ok(completion::run(shell)),
        Command::Help => Ok(print_root_help()),
    };

    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{}", errors::format_error(&err));
            exit::RUNTIME
        }
    }
}

fn print_parse_error(err: clap::Error) -> i32 {
    let kind = err.kind();
    if let Err(print_err) = err.print() {
        eprintln!("{print_err}");
        return exit::RUNTIME;
    }

    if matches!(kind, ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        exit::SUCCESS
    } else {
        exit::USAGE
    }
}

fn print_root_help() -> i32 {
    let mut command = Cli::command();
    if let Err(err) = command.print_help() {
        eprintln!("{err}");
        return exit::RUNTIME;
    }
    println!();
    exit::SUCCESS
}

fn is_known_command(arg: &str) -> bool {
    matches!(
        arg,
        "lock" | "unlock" | "list" | "copy" | "delete" | "diff" | "tag" | "completion" | "help"
    )
}

#[cfg(test)]
mod tests {
    use super::is_known_command;

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

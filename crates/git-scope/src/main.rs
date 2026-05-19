use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use nils_common::cli_contract::exit;
use nils_common::env as shared_env;
use std::process;

mod change;
mod commit;
mod completion;
mod git;
mod git_cmd;
mod print;
mod progress;
mod render;
mod tree;

#[derive(Parser)]
#[command(
    name = "git-scope",
    version,
    about = "Inspect Git-tracked file scopes and selected content.",
    long_about = "Inspect tracked, staged, unstaged, untracked, and commit-scoped paths with optional file-content printing.",
    disable_help_flag = true,
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  git-scope tracked crates/agent-docs\n  git-scope staged -p\n  git-scope commit HEAD~1\n  git-scope completion zsh\n\nENVIRONMENT:\n  GIT_SCOPE_PROGRESS  Opt in or out of progress output.\n  NO_COLOR            Disable ANSI colors.\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error"
)]
struct Cli {
    /// Disable ANSI colors (also via NO_COLOR)
    #[arg(long, global = true)]
    no_color: bool,

    /// Display help message for git-scope
    #[arg(short = 'h', long = "help", global = true)]
    help: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show files tracked by Git (prefix filter optional)
    Tracked {
        /// Print the contents of each file
        #[arg(short = 'p', long = "print")]
        print: bool,
        /// Optional path prefixes to filter tracked files
        #[arg(value_name = "prefix", num_args = 0..)]
        prefixes: Vec<String>,
    },
    /// Show files staged for commit
    Staged {
        /// Print the contents of each file (index)
        #[arg(short = 'p', long = "print")]
        print: bool,
    },
    /// Show modified files not yet staged
    Unstaged {
        /// Print the contents of each file (worktree)
        #[arg(short = 'p', long = "print")]
        print: bool,
    },
    /// Show all changes (staged + unstaged)
    All {
        /// Print the contents of each file
        #[arg(short = 'p', long = "print")]
        print: bool,
    },
    /// Show untracked files
    Untracked {
        /// Print the contents of each file (worktree)
        #[arg(short = 'p', long = "print")]
        print: bool,
    },
    /// Show commit details (use -p to print content)
    Commit {
        /// Print file contents for the commit file list
        #[arg(short = 'p', long = "print")]
        print: bool,
        /// For merge commits: show diff against parent <n>
        #[arg(long = "parent", short = 'P')]
        parent: Option<String>,
        /// Commit-ish (hash, HEAD, etc.)
        commit: Option<String>,
    },
    /// Display help message for git-scope
    Help,
    /// Export shell completion script
    Completion {
        /// Shell to generate completion script for
        #[arg(value_enum, value_name = "shell")]
        shell: CompletionShell,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum CompletionShell {
    Bash,
    Zsh,
}

fn print_help() {
    println!("Inspect Git-tracked file scopes and selected content.");
    println!();
    println!("Usage: git-scope <command> [args]");
    println!();
    println!("Commands:");
    println!(
        "  {:<16}  Show files tracked by Git (prefix filter optional)",
        "tracked"
    );
    println!("  {:<16}  Show files staged for commit", "staged");
    println!("  {:<16}  Show modified files not yet staged", "unstaged");
    println!("  {:<16}  Show all changes (staged and unstaged)", "all");
    println!("  {:<16}  Show untracked files", "untracked");
    println!(
        "  {:<16}  Show commit details (use -p to print content)",
        "commit <id>"
    );
    println!(
        "  {:<16}  Export shell completion script",
        "completion <shell>"
    );
    println!();
    println!("Options:");
    println!(
        "  {:<16}  Disable ANSI colors (also via NO_COLOR)",
        "--no-color"
    );
    println!("  {:<16}  Print help", "-h, --help");
    println!("  {:<16}  Show version", "-V, --version");
    println!();
    println!("EXAMPLES:");
    println!("  git-scope tracked crates/agent-docs");
    println!("  git-scope staged -p");
    println!("  git-scope commit HEAD~1");
    println!("  git-scope completion zsh");
    println!();
    println!("ENVIRONMENT:");
    println!("  GIT_SCOPE_PROGRESS  Opt in or out of progress output.");
    println!("  NO_COLOR            Disable ANSI colors.");
    println!();
    println!("EXIT CODES:");
    println!("  0   success");
    println!("  1   runtime error");
    println!("  64  command-line usage error");
}

fn print_subcommand_help(command: &Command) -> bool {
    let subcommand = match command {
        Command::Tracked { .. } => "tracked",
        Command::Staged { .. } => "staged",
        Command::Unstaged { .. } => "unstaged",
        Command::All { .. } => "all",
        Command::Untracked { .. } => "untracked",
        Command::Commit { .. } => "commit",
        Command::Completion { .. } => "completion",
        Command::Help => return false,
    };

    let mut root = Cli::command();
    let Some(subcommand) = root.find_subcommand_mut(subcommand) else {
        return false;
    };
    if subcommand.print_help().is_err() {
        return false;
    }
    println!();
    true
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err:#}");
        process::exit(exit::RUNTIME);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    if cli.help {
        if let Some(command) = cli.command.as_ref()
            && print_subcommand_help(command)
        {
            return Ok(());
        }
        print_help();
        return Ok(());
    }

    let command = cli.command.unwrap_or(Command::Help);
    match command {
        Command::Help => {
            print_help();
            return Ok(());
        }
        Command::Completion { shell } => {
            process::exit(completion::run(shell));
        }
        _ => {}
    }

    if !git::is_git_repo() {
        println!("⚠️ Not a Git repository. Run this command inside a Git project.");
        process::exit(exit::RUNTIME);
    }

    let no_color = cli.no_color || shared_env::no_color_enabled();
    let progress_opt_in = git_scope_progress_opt_in();

    match command {
        Command::Tracked { print, prefixes } => {
            let lines = git::collect_tracked(&prefixes)?;
            render::render_with_type(
                &lines,
                no_color,
                render::PrintMode::Worktree,
                print,
                progress_opt_in,
            )?;
        }
        Command::Staged { print } => {
            let lines = git::collect_staged()?;
            render::render_with_type(
                &lines,
                no_color,
                render::PrintMode::Index,
                print,
                progress_opt_in,
            )?;
        }
        Command::Unstaged { print } => {
            let lines = git::collect_unstaged()?;
            render::render_with_type(
                &lines,
                no_color,
                render::PrintMode::Worktree,
                print,
                progress_opt_in,
            )?;
        }
        Command::All { print } => {
            let (combined, staged, unstaged) = git::collect_all()?;
            let files = render::render_with_type(
                &combined,
                no_color,
                render::PrintMode::Worktree,
                false,
                progress_opt_in,
            )?;
            if print {
                render::print_all_files(&files, &staged, &unstaged, progress_opt_in)?;
            }
        }
        Command::Untracked { print } => {
            let lines = git::collect_untracked()?;
            render::render_with_type(
                &lines,
                no_color,
                render::PrintMode::Worktree,
                print,
                progress_opt_in,
            )?;
        }
        Command::Commit {
            print,
            parent,
            commit,
        } => {
            let Some(commit) = commit else {
                eprintln!("error: the following required arguments were not provided:");
                eprintln!("  <COMMIT>");
                eprintln!();
                eprintln!("Usage: git-scope commit [OPTIONS] <COMMIT>");
                eprintln!();
                eprintln!("For more information, try '--help commit <COMMIT>'.");
                process::exit(exit::USAGE);
            };
            commit::render_commit(&commit, parent.as_deref(), no_color, print, progress_opt_in)
                .with_context(|| format!("git-scope commit {commit}"))?;
        }
        Command::Help | Command::Completion { .. } => {
            unreachable!("Help/Completion handled by the early-return dispatch above")
        }
    }

    Ok(())
}

fn git_scope_progress_opt_in() -> bool {
    let Some(value) = std::env::var_os("GIT_SCOPE_PROGRESS") else {
        return false;
    };
    let value = value.to_string_lossy();
    shared_env::is_truthy(value.trim())
}

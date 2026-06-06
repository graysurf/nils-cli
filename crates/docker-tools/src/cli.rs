use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use nils_common::cli_contract::exit;
use std::ffi::OsString;

use crate::{completion, runtime};

const ROOT_AFTER_HELP: &str = "\
EXAMPLES:
  docker-tools container sh my-container
  docker-tools container rm --no-force old-container
  docker-tools compose down --all --yes
  docker-tools run zsh ubuntu:latest
  docker-tools completion zsh

ENVIRONMENT:
  ZSH_DOCKER_COMPOSE_CMD  Override compose command, for example 'docker compose' or docker-compose.

EXIT CODES:
  0    success
  1    runtime error or cancelled interactive action
  2    zsh-kit-compatible usage guard for migrated helper commands
  64   command-line usage error
  127  required Docker executable is unavailable";

#[derive(Debug, Parser)]
#[command(
    name = "docker-tools",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Docker helper CLI",
    long_about = "Run Docker helpers migrated out of zsh-kit. Shell alias mutation remains owned by zsh-kit.",
    after_help = ROOT_AFTER_HELP
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Container helper commands
    Container(ContainerArgs),
    /// Docker Compose helper commands
    Compose(ComposeArgs),
    /// Interactive docker run helpers
    Run(RunArgs),
    /// Export shell completion script
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
pub struct ContainerArgs {
    #[command(subcommand)]
    pub command: Option<ContainerCommand>,
}

#[derive(Debug, Subcommand)]
pub enum ContainerCommand {
    /// Exec into a container with zsh -> bash -> sh fallback
    Sh(ContainerShellArgs),
    /// Alias for container sh
    Zsh(ContainerShellArgs),
    /// Remove one or more containers
    Rm(ContainerRmArgs),
}

#[derive(Debug, Args)]
pub struct ContainerShellArgs {
    /// User inside the container
    #[arg(
        short = 'u',
        long = "user",
        value_name = "user",
        conflicts_with = "root"
    )]
    pub user: Option<String>,
    /// Exec as root
    #[arg(short = 'r', long = "root")]
    pub root: bool,
    /// Container name or id
    #[arg(value_name = "container")]
    pub container: String,
}

#[derive(Debug, Args)]
pub struct ContainerRmArgs {
    /// Do not force remove containers
    #[arg(long = "no-force")]
    pub no_force: bool,
    /// Remove anonymous volumes
    #[arg(short = 'v', long = "volumes")]
    pub volumes: bool,
    /// Container names or ids
    #[arg(value_name = "container", required = true, num_args = 1..)]
    pub containers: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ComposeArgs {
    #[command(subcommand)]
    pub command: Option<ComposeCommand>,
}

#[derive(Debug, Subcommand)]
pub enum ComposeCommand {
    /// Run docker compose down
    Down(ComposeDownArgs),
}

#[derive(Debug, Args)]
pub struct ComposeDownArgs {
    /// Add --remove-orphans --volumes --rmi all
    #[arg(short = 'a', long = "all")]
    pub all: bool,
    /// Skip confirmation prompts for --all
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,
    /// Extra arguments passed after docker compose down
    #[arg(value_name = "compose-arg", num_args = 0.., trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[command(subcommand)]
    pub command: Option<RunCommand>,
}

#[derive(Debug, Subcommand)]
pub enum RunCommand {
    /// Run an image with zsh -> bash -> sh fallback
    Zsh(RunZshArgs),
}

#[derive(Debug, Args)]
pub struct RunZshArgs {
    /// Do not mount the current working directory to /work
    #[arg(long = "no-mount")]
    pub no_mount: bool,
    /// Container workdir
    #[arg(short = 'w', long = "workdir", value_name = "path")]
    pub workdir: Option<String>,
    /// Container name
    #[arg(short = 'n', long = "name", value_name = "name")]
    pub name: Option<String>,
    /// User inside the container
    #[arg(
        short = 'u',
        long = "user",
        value_name = "user",
        conflicts_with = "root"
    )]
    pub user: Option<String>,
    /// Run as root
    #[arg(short = 'r', long = "root")]
    pub root: bool,
    /// Image to run
    #[arg(value_name = "image")]
    pub image: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    /// Shell to generate completion script for
    #[arg(value_enum, value_name = "shell")]
    pub shell: CompletionShell,
}

pub fn run() -> i32 {
    run_from(std::env::args_os())
}

pub fn run_from<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => return print_parse_error(err),
    };

    match cli.command {
        None => print_help_stdout(),
        Some(Command::Container(args)) => match args.command {
            None => print_subcommand_help("container"),
            Some(ContainerCommand::Sh(args) | ContainerCommand::Zsh(args)) => {
                runtime::container_shell(args)
            }
            Some(ContainerCommand::Rm(args)) => runtime::container_rm(args),
        },
        Some(Command::Compose(args)) => match args.command {
            None => print_subcommand_help("compose"),
            Some(ComposeCommand::Down(args)) => runtime::compose_down(args),
        },
        Some(Command::Run(args)) => match args.command {
            None => print_subcommand_help("run"),
            Some(RunCommand::Zsh(args)) => runtime::run_zsh(args),
        },
        Some(Command::Completion(args)) => completion::run(args.shell),
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

fn print_help_stdout() -> i32 {
    let mut command = Cli::command();
    if let Err(err) = command.print_help() {
        eprintln!("{err}");
        return exit::RUNTIME;
    }
    println!();
    exit::SUCCESS
}

fn print_subcommand_help(name: &str) -> i32 {
    let mut command = Cli::command();
    let Some(subcommand) = command.find_subcommand_mut(name) else {
        return exit::SOFTWARE;
    };
    if let Err(err) = subcommand.print_help() {
        eprintln!("{err}");
        return exit::RUNTIME;
    }
    println!();
    exit::SUCCESS
}

use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::engine::ArgValueCandidates;
use nils_common::cli_contract::{OutputFormat, emit_parse_error, exit};
use std::ffi::OsString;

use crate::completion::name_candidates;
use crate::{completion, runtime};

pub const BINARY: &str = "secrets";

const ROOT_AFTER_HELP: &str = "\
EXAMPLES:
  secrets pull                 # decrypt this repo's store entry -> ./.env
  secrets pull my-stack        # decrypt a named entry instead
  secrets add                  # encrypt ./.env into the store, commit, push
  secrets which                # print the store path this repo maps to
  secrets list                 # list every store entry name
  secrets edit my-stack        # sops-edit a store entry
  secrets completion zsh

ENVIRONMENT:
  SECRETS_REPO  Override the store path (default: ~/Project/graysurf/secrets).

SECURITY:
  stdout and --format json carry only METADATA (store paths, entry names,
  counts). Decrypted secret VALUES are written to ./.env (mode 600) and are
  never echoed to stdout or the JSON envelope.

EXIT CODES:
  0    success
  1    runtime error
  64   command-line usage error
  65   no store entry for the requested target
  69   the store, sops, or git is unavailable";

#[derive(Debug, Parser)]
#[command(
    name = "secrets",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Pull / add a repo's .env from the central SOPS store",
    long_about = "Thin wrapper over sops and git: decrypt a repo's store entry into ./.env, or encrypt ./.env back into the central store. Secret values never touch stdout.",
    after_help = ROOT_AFTER_HELP
)]
pub struct Cli {
    /// Output format
    #[arg(long, value_enum, global = true, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    pub fn output_format(&self) -> OutputFormat {
        self.format
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Decrypt the store entry into ./.env (mode 600)
    Pull(NameArgs),
    /// Encrypt a plaintext env file into the store, commit, and push
    Add(AddArgs),
    /// List every store entry name
    List,
    /// Print the store path the current repo maps to
    Which(NameArgs),
    /// Open a store entry in sops for editing
    Edit(NameArgs),
    /// Export shell completion script
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
pub struct NameArgs {
    /// Override the auto-detected entry: bare name, `repos/<o>/<r>`, or `stacks/<x>`
    #[arg(value_name = "name", add = ArgValueCandidates::new(name_candidates))]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Plaintext env file to encrypt into the store
    #[arg(value_name = "file", default_value = ".env")]
    pub file: String,
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
    // Short-circuit `COMPLETE=<shell> secrets ...` dynamic-completion requests
    // before the normal parse. No-op when `COMPLETE` is unset, so ordinary
    // invocations are unaffected.
    completion::complete_env();

    let argv: Vec<OsString> = args.into_iter().map(|arg| arg.into()).collect();

    let cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(err) => return print_parse_error(err, &argv),
    };

    let format = cli.output_format();

    match cli.command {
        None => print_help_stdout(),
        Some(Command::Pull(args)) => runtime::pull(args.name.as_deref(), format),
        Some(Command::Add(args)) => runtime::add(&args.file, format),
        Some(Command::List) => runtime::list(format),
        Some(Command::Which(args)) => runtime::which(args.name.as_deref(), format),
        Some(Command::Edit(args)) => runtime::edit(args.name.as_deref(), format),
        Some(Command::Completion(args)) => completion::run(args.shell),
    }
}

fn print_parse_error(err: clap::Error, argv: &[OsString]) -> i32 {
    let kind = err.kind();
    // Help/version are not failures: print them as clap rendered and exit 0.
    if matches!(kind, ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        if let Err(print_err) = err.print() {
            eprintln!("{print_err}");
            return exit::RUNTIME;
        }
        return exit::SUCCESS;
    }

    // For genuine usage errors, route through the shared contract so a
    // `--format json` caller gets a machine-readable error envelope instead of
    // clap's prose on stderr.
    let format = detect_format_from_argv(argv);
    if format.is_json() {
        return emit_parse_error(BINARY, format, "invalid-arguments", &err.to_string());
    }
    if let Err(print_err) = err.print() {
        eprintln!("{print_err}");
        return exit::RUNTIME;
    }
    exit::USAGE
}

/// Peek at argv for `--format json` so a parse failure (which never produced a
/// parsed `Cli`) can still honor the requested machine-readable output mode.
fn detect_format_from_argv(argv: &[OsString]) -> OutputFormat {
    let mut iter = argv.iter().map(|arg| arg.to_string_lossy());
    while let Some(arg) = iter.next() {
        if arg == "--format" {
            if let Some(value) = iter.next()
                && value == "json"
            {
                return OutputFormat::Json;
            }
        } else if arg == "--format=json" {
            return OutputFormat::Json;
        }
    }
    OutputFormat::Text
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use pretty_assertions::assert_eq;

    #[test]
    fn format_defaults_to_text_and_parses_json() {
        let cli = Cli::parse_from(["secrets", "list"]);
        assert_eq!(cli.output_format(), OutputFormat::Text);
        let cli = Cli::parse_from(["secrets", "--format", "json", "list"]);
        assert_eq!(cli.output_format(), OutputFormat::Json);
    }

    #[test]
    fn detect_format_from_argv_finds_json() {
        let argv: Vec<OsString> = ["secrets", "--format", "json", "bogus"]
            .iter()
            .map(OsString::from)
            .collect();
        assert!(detect_format_from_argv(&argv).is_json());
        let argv: Vec<OsString> = ["secrets", "--format=json", "bogus"]
            .iter()
            .map(OsString::from)
            .collect();
        assert!(detect_format_from_argv(&argv).is_json());
        let argv: Vec<OsString> = ["secrets", "bogus"].iter().map(OsString::from).collect();
        assert!(detect_format_from_argv(&argv).is_text());
    }

    #[test]
    fn add_defaults_file_to_dotenv() {
        let cli = Cli::parse_from(["secrets", "add"]);
        match cli.command {
            Some(Command::Add(args)) => assert_eq!(args.file, ".env"),
            other => panic!("expected add, got {other:?}"),
        }
    }
}

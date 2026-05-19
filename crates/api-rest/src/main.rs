mod cli;
mod commands;
mod completion;

use std::io::IsTerminal;
use std::path::PathBuf;

use std::ffi::OsString;

use api_testing_core::cli_contract::handle_parse_error;
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::commands::{cmd_call, cmd_history, cmd_report, cmd_report_from_cmd};
use crate::completion::run as run_completion;

fn argv_with_default_command(raw_args: &[String]) -> Vec<String> {
    let mut argv = vec!["api-rest".to_string()];
    if raw_args.is_empty() {
        return argv;
    }

    let first = raw_args[0].as_str();
    let is_root_help = first == "-h" || first == "--help";
    let is_root_version = first == "-V" || first == "--version";

    let is_explicit_command = matches!(
        first,
        "call" | "history" | "report" | "report-from-cmd" | "completion"
    );
    if !is_explicit_command && !is_root_help && !is_root_version {
        argv.push("call".to_string());
    }

    argv.extend_from_slice(raw_args);
    argv
}

fn print_root_help() {
    println!("Usage: api-rest <command> [args]");
    println!();
    println!("Commands:");
    println!("  call      Execute a request file and print the response body to stdout (default)");
    println!("  history   Print the last (or last N) history entries");
    println!("  report    Generate a Markdown API test report");
    println!("  report-from-cmd  Generate a report from a saved `call` snippet");
    println!("  completion      Print shell completion script");
    println!();
    println!("Common options (see subcommand help for full details):");
    println!("  --config-dir <dir>   Seed setup/rest discovery (call/history/report)");
    println!("  -h, --help           Print help");
    println!();
    println!("Examples:");
    println!("  api-rest --help");
    println!("  api-rest call --help");
    println!("  api-rest report --help");
    println!("  api-rest report-from-cmd --help");
    println!("  api-rest completion zsh");
}

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let is_root_help = raw_args.len() == 1 && (raw_args[0] == "-h" || raw_args[0] == "--help");
    if raw_args.is_empty() || is_root_help {
        print_root_help();
        return 0;
    }

    let argv = argv_with_default_command(&raw_args);
    let argv_os: Vec<OsString> = argv.iter().map(OsString::from).collect();

    let cli = match Cli::try_parse_from(argv_os.iter()) {
        Ok(v) => v,
        Err(err) => return handle_parse_error("api-rest", argv_os, err),
    };

    let invocation_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let invocation_dir = std::fs::canonicalize(&invocation_dir).unwrap_or(invocation_dir);

    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let stdout_is_tty = std::io::stdout().is_terminal();

    match cli.command {
        None => {
            print_root_help();
            0
        }
        Some(Command::Call(args)) => cmd_call(
            &args,
            &invocation_dir,
            stdout_is_tty,
            &mut stdout,
            &mut stderr,
        ),
        Some(Command::History(args)) => {
            cmd_history(&args, &invocation_dir, &mut stdout, &mut stderr)
        }
        Some(Command::Report(args)) => cmd_report(&args, &invocation_dir, &mut stdout, &mut stderr),
        Some(Command::ReportFromCmd(args)) => {
            cmd_report_from_cmd(&args, &invocation_dir, &mut stdout, &mut stderr)
        }
        Some(Command::Completion(args)) => run_completion(args.shell),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn argv_with_default_command_inserts_call() {
        let argv = argv_with_default_command(&[]);
        assert_eq!(argv, vec!["api-rest".to_string()]);

        let argv = argv_with_default_command(&["--help".to_string()]);
        assert_eq!(argv, vec!["api-rest".to_string(), "--help".to_string()]);

        let argv = argv_with_default_command(&["history".to_string()]);
        assert_eq!(argv, vec!["api-rest".to_string(), "history".to_string()]);

        let argv = argv_with_default_command(&["completion".to_string(), "zsh".to_string()]);
        assert_eq!(
            argv,
            vec![
                "api-rest".to_string(),
                "completion".to_string(),
                "zsh".to_string()
            ]
        );

        let argv = argv_with_default_command(&["requests/health.request.json".to_string()]);
        assert_eq!(
            argv,
            vec![
                "api-rest".to_string(),
                "call".to_string(),
                "requests/health.request.json".to_string()
            ]
        );
    }
}

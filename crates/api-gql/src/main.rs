mod cli;
mod commands;
mod completion;

use std::io::IsTerminal;
use std::path::PathBuf;

use std::ffi::OsString;

use api_testing_core::cli_contract::handle_parse_error;
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::commands::{cmd_call, cmd_history, cmd_report, cmd_report_from_cmd, cmd_schema};
use crate::completion::run as run_completion;

fn argv_with_default_command(raw_args: &[String]) -> Vec<String> {
    let mut argv = vec!["api-gql".to_string()];
    if raw_args.is_empty() {
        return argv;
    }

    let first = raw_args[0].as_str();
    let is_root_help = first == "-h" || first == "--help";
    let is_root_version = first == "-V" || first == "--version";

    let is_explicit_command = matches!(
        first,
        "call" | "history" | "report" | "report-from-cmd" | "schema" | "completion"
    );
    if !is_explicit_command && !is_root_help && !is_root_version {
        argv.push("call".to_string());
    }

    argv.extend_from_slice(raw_args);
    argv
}

fn print_root_help() {
    println!("Usage: api-gql <command> [args]");
    println!();
    println!("Commands:");
    println!(
        "  call     Execute an operation (and optional variables) and print response JSON (default)"
    );
    println!("  history  Print the last (or last N) history entries");
    println!("  report   Generate a Markdown API test report");
    println!("  report-from-cmd  Generate a report from a command snippet (arg or stdin)");
    println!("  schema   Resolve a schema file path (or print schema contents)");
    println!("  completion  Print shell completion script");
    println!();
    println!("Common options (see subcommand help for full details):");
    println!("  --config-dir <dir>   Seed setup/graphql discovery (call/history/report/schema)");
    println!("  --list-envs          List available endpoint presets and exit 0 (call)");
    println!("  --list-jwts          List available JWT profiles and exit 0 (call)");
    println!("  -h, --help           Print help");
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
        Err(err) => return handle_parse_error("api-gql", argv_os, err),
    };

    let invocation_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let invocation_dir = std::fs::canonicalize(&invocation_dir).unwrap_or(invocation_dir);

    let stdout_is_tty = std::io::stdout().is_terminal();
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();

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
        Some(Command::Schema(args)) => cmd_schema(&args, &invocation_dir, &mut stdout, &mut stderr),
        Some(Command::Completion(args)) => run_completion(args.shell),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{CallArgs, ReportArgs};
    use crate::commands::call::resolve_endpoint_for_call;
    use crate::commands::report_from_cmd::build_api_gql_report_dry_run_command;
    use pretty_assertions::assert_eq;
    use std::path::Path;
    use tempfile::TempDir;

    fn write_file(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    #[test]
    fn argv_with_default_command_inserts_call() {
        let argv = argv_with_default_command(&[]);
        assert_eq!(argv, vec!["api-gql".to_string()]);

        let argv = argv_with_default_command(&["--help".to_string()]);
        assert_eq!(argv, vec!["api-gql".to_string(), "--help".to_string()]);

        let argv = argv_with_default_command(&["history".to_string()]);
        assert_eq!(argv, vec!["api-gql".to_string(), "history".to_string()]);

        let argv = argv_with_default_command(&["completion".to_string(), "zsh".to_string()]);
        assert_eq!(
            argv,
            vec![
                "api-gql".to_string(),
                "completion".to_string(),
                "zsh".to_string()
            ]
        );

        let argv = argv_with_default_command(&["ops/health.graphql".to_string()]);
        assert_eq!(
            argv,
            vec![
                "api-gql".to_string(),
                "call".to_string(),
                "ops/health.graphql".to_string()
            ]
        );
    }

    #[test]
    fn build_report_command_includes_expected_flags() {
        let args = ReportArgs {
            case: "Health".to_string(),
            op: "ops/health.graphql".to_string(),
            vars: Some("vars.json".to_string()),
            out: Some("docs/report.md".to_string()),
            env: Some("staging".to_string()),
            url: None,
            jwt: Some("svc".to_string()),
            run: true,
            response: None,
            allow_empty: true,
            no_redact: false,
            no_command: false,
            no_command_url: false,
            project_root: None,
            config_dir: Some("setup/graphql".to_string()),
        };

        let cmd = build_api_gql_report_dry_run_command(&args);
        assert!(cmd.contains("--case 'Health'"));
        assert!(cmd.contains("--op 'ops/health.graphql'"));
        assert!(cmd.contains("--vars 'vars.json'"));
        assert!(cmd.contains("--out 'docs/report.md'"));
        assert!(cmd.contains("--config-dir 'setup/graphql'"));
        assert!(cmd.contains("--env 'staging'"));
        assert!(cmd.contains("--jwt 'svc'"));
        assert!(cmd.contains("--run"));
        assert!(cmd.contains("--allow-empty"));
    }

    #[test]
    fn resolve_endpoint_for_call_honors_url_and_env() {
        let tmp = TempDir::new().unwrap();
        let setup_dir = tmp.path().join("setup/graphql");
        std::fs::create_dir_all(&setup_dir).unwrap();
        write_file(
            &setup_dir.join("endpoints.env"),
            "GQL_ENV_DEFAULT=prod\nGQL_URL_PROD=http://prod\nGQL_URL_STAGING=http://staging\n",
        );
        let setup = api_testing_core::config::ResolvedSetup::graphql(setup_dir, None);

        let args = CallArgs {
            env: None,
            url: Some("http://explicit".to_string()),
            jwt: None,
            config_dir: None,
            list_envs: false,
            list_jwts: false,
            no_history: false,
            operation: Some("ops/health.graphql".to_string()),
            variables: None,
        };
        let sel = resolve_endpoint_for_call(&args, &setup).unwrap();
        assert_eq!(sel.gql_url, "http://explicit");
        assert_eq!(sel.endpoint_label_used, "url");

        let args = CallArgs {
            env: Some("staging".to_string()),
            url: None,
            jwt: None,
            config_dir: None,
            list_envs: false,
            list_jwts: false,
            no_history: false,
            operation: Some("ops/health.graphql".to_string()),
            variables: None,
        };
        let sel = resolve_endpoint_for_call(&args, &setup).unwrap();
        assert_eq!(sel.gql_url, "http://staging");
        assert_eq!(sel.endpoint_label_used, "env");

        let args = CallArgs {
            env: Some("https://example.test/graphql".to_string()),
            url: None,
            jwt: None,
            config_dir: None,
            list_envs: false,
            list_jwts: false,
            no_history: false,
            operation: Some("ops/health.graphql".to_string()),
            variables: None,
        };
        let sel = resolve_endpoint_for_call(&args, &setup).unwrap();
        assert_eq!(sel.gql_url, "https://example.test/graphql");
        assert_eq!(sel.endpoint_label_used, "url");
    }

    #[test]
    fn resolve_endpoint_for_call_unknown_env_lists_available() {
        let tmp = TempDir::new().unwrap();
        let setup_dir = tmp.path().join("setup/graphql");
        std::fs::create_dir_all(&setup_dir).unwrap();
        write_file(
            &setup_dir.join("endpoints.env"),
            "GQL_URL_PROD=http://prod\nGQL_URL_DEV=http://dev\n",
        );
        let setup = api_testing_core::config::ResolvedSetup::graphql(setup_dir, None);

        let args = CallArgs {
            env: Some("missing".to_string()),
            url: None,
            jwt: None,
            config_dir: None,
            list_envs: false,
            list_jwts: false,
            no_history: false,
            operation: Some("ops/health.graphql".to_string()),
            variables: None,
        };

        let err = resolve_endpoint_for_call(&args, &setup).unwrap_err();
        assert!(err.to_string().contains("Unknown --env 'missing'"));
        assert!(err.to_string().contains("prod"));
    }
}

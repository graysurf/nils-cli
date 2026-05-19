mod cli;
pub mod commands;
mod completion;
pub mod config;
pub mod env;
pub mod model;
pub mod output;
pub mod paths;
pub mod resolver;

use clap::Parser;

use cli::{Cli, Command};
use env::{PathOverrides, resolve_roots};
use model::{ConfigErrorKind, OutputFormat};
use output::{
    render_baseline, render_contexts, render_resolve, render_scaffold_baseline, render_stub,
};

use nils_common::cli_contract::exit;

const EXIT_OK: i32 = exit::SUCCESS;
const EXIT_STRICT_MISSING_REQUIRED: i32 = exit::RUNTIME;
const EXIT_USAGE: i32 = exit::USAGE;
const EXIT_CONFIG: i32 = 3;
const EXIT_RUNTIME: i32 = 4;

pub fn run() -> i32 {
    run_with_args(std::env::args_os())
}

pub fn run_with_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(parsed) => parsed,
        Err(err) => {
            use clap::error::ErrorKind;
            let code = match err.kind() {
                ErrorKind::DisplayHelp
                | ErrorKind::DisplayVersion
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => err.exit_code(),
                _ => EXIT_USAGE,
            };
            let _ = err.print();
            return code;
        }
    };

    dispatch(cli)
}

fn dispatch(cli: Cli) -> i32 {
    let fallback_mode = cli.worktree_fallback;
    let overrides = PathOverrides {
        docs_home: cli.docs_home,
        project_path: cli.project_path,
    };

    match cli.command {
        Command::Contexts(args) => print_rendered(
            render_contexts(args.format, resolver::supported_contexts()),
            EXIT_OK,
        ),
        Command::Resolve(args) => {
            let roots = match resolve_roots_or_exit(&overrides) {
                Ok(roots) => roots,
                Err(code) => return code,
            };

            let report =
                match resolver::resolve_with_mode(args.context, &roots, args.strict, fallback_mode)
                {
                    Ok(report) => report,
                    Err(err) => {
                        eprintln!("error: {err}");
                        return config_error_exit_code(&err);
                    }
                };
            let exit_code = if args.strict && report.has_missing_required() {
                EXIT_STRICT_MISSING_REQUIRED
            } else {
                EXIT_OK
            };
            print_rendered(render_resolve(args.format, &report), exit_code)
        }
        Command::Add(args) => {
            let roots = match resolve_roots_or_exit(&overrides) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let request = commands::add::AddDocumentRequest {
                target: args.target,
                context: args.context,
                scope: args.scope,
                path: args.path,
                required: args.required,
                when: args.when,
                notes: args.notes,
            };

            match commands::add::upsert_document(&roots, request) {
                Ok(report) => {
                    let message = format!(
                        "target={} action={} config={} entries={}",
                        report.target,
                        report.action,
                        report.config_path.display(),
                        report.document_count
                    );
                    print_rendered(render_stub(OutputFormat::Text, "add", message), EXIT_OK)
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    config_error_exit_code(&err)
                }
            }
        }
        Command::ScaffoldAgents(args) => {
            let roots = match resolve_roots_or_exit(&overrides) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let request = commands::scaffold_agents::ScaffoldAgentsRequest {
                target: args.target,
                output: args.output,
                force: args.force,
            };

            match commands::scaffold_agents::scaffold_agents(&request, &roots) {
                Ok(report) => {
                    let message = format!(
                        "target={} mode={} output={}",
                        report.target,
                        report.write_mode,
                        report.output_path.display()
                    );
                    print_rendered(
                        render_stub(OutputFormat::Text, "scaffold-agents", message),
                        EXIT_OK,
                    )
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    match err.kind {
                        commands::scaffold_agents::ScaffoldAgentsErrorKind::AlreadyExists => {
                            EXIT_STRICT_MISSING_REQUIRED
                        }
                        commands::scaffold_agents::ScaffoldAgentsErrorKind::Io => EXIT_RUNTIME,
                    }
                }
            }
        }
        Command::Baseline(args) => {
            if !args.check {
                eprintln!("error: baseline currently supports only --check");
                return EXIT_USAGE;
            }

            let roots = match resolve_roots_or_exit(&overrides) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let report = match commands::baseline::check_builtin_baseline_with_mode(
                args.target,
                &roots,
                args.strict,
                fallback_mode,
            ) {
                Ok(report) => report,
                Err(err) => {
                    eprintln!("error: {err}");
                    return config_error_exit_code(&err);
                }
            };
            let exit_code = if args.strict && report.has_missing_required() {
                EXIT_STRICT_MISSING_REQUIRED
            } else {
                EXIT_OK
            };
            print_rendered(render_baseline(args.format, &report), exit_code)
        }
        Command::ScaffoldBaseline(args) => {
            let roots = match resolve_roots_or_exit(&overrides) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let request = commands::scaffold_baseline::ScaffoldBaselineRequest {
                target: args.target,
                missing_only: args.missing_only,
                force: args.force,
                dry_run: args.dry_run,
            };

            match commands::scaffold_baseline::scaffold_baseline(&request, &roots) {
                Ok(report) => {
                    print_rendered(render_scaffold_baseline(args.format, &report), EXIT_OK)
                }
                Err(err) => {
                    eprintln!("error: {err}");
                    EXIT_RUNTIME
                }
            }
        }
        Command::Completion(args) => completion::run(args.shell),
    }
}

fn resolve_roots_or_exit(overrides: &PathOverrides) -> Result<env::ResolvedRoots, i32> {
    resolve_roots(overrides).map_err(|err| {
        eprintln!("error: {err:#}");
        EXIT_RUNTIME
    })
}

fn config_error_exit_code(err: &model::ConfigLoadError) -> i32 {
    match err.kind {
        ConfigErrorKind::Validation | ConfigErrorKind::Parse => EXIT_CONFIG,
        ConfigErrorKind::Io => EXIT_RUNTIME,
    }
}

fn print_rendered(rendered: anyhow::Result<String>, success_exit_code: i32) -> i32 {
    match rendered {
        Ok(body) => {
            println!("{body}");
            success_exit_code
        }
        Err(err) => {
            eprintln!("error: {err:#}");
            EXIT_RUNTIME
        }
    }
}

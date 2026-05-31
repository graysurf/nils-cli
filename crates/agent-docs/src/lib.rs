mod cli;
pub mod commands;
mod completion;
pub mod config;
pub mod content;
pub mod env;
pub mod model;
pub mod output;
pub mod paths;
pub mod predicate;
pub mod resolver;

use clap::Parser;

use cli::{Cli, Command};
use config::load_catalog_from_roots;
use env::{PathOverrides, ResolvedRoots, resolve_roots};
use model::{ConfigErrorKind, ConfigLoadError, Context, InitMode, ListReport};
use output::{
    ExplainIntent, ExplainIntents, render_audit, render_explain_intent, render_explain_intents,
    render_init, render_list, render_preflight, render_remove, render_undeclared_intent_error,
};

use nils_common::cli_contract::exit;

const EXIT_OK: i32 = exit::SUCCESS;
const EXIT_STRICT: i32 = exit::RUNTIME;
const EXIT_DATA: i32 = exit::DATA;
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
        Command::Audit(args) => {
            let roots = match resolve_roots_or_exit(&overrides) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let report =
                match commands::audit::run_audit(args.target, &roots, args.strict, fallback_mode) {
                    Ok(report) => report,
                    Err(err) => {
                        eprintln!("error: {err}");
                        return config_error_exit_code(&err);
                    }
                };
            let exit_code = if args.strict && report.has_problems() {
                EXIT_STRICT
            } else {
                EXIT_OK
            };
            print_rendered(render_audit(args.format, &report), exit_code)
        }
        Command::Preflight(args) => {
            let intent = match parse_intent(&args.intent) {
                Ok(intent) => intent,
                Err(code) => return code,
            };
            let roots = match resolve_roots_or_exit(&overrides) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let catalog = match load_catalog_from_roots(&roots) {
                Ok(catalog) => catalog,
                Err(err) => {
                    eprintln!("error: {err}");
                    return config_error_exit_code(&err);
                }
            };
            let report = resolver::resolve_intent_with_catalog(
                &intent,
                &roots,
                args.strict,
                fallback_mode,
                true,
                &catalog,
            );
            if args.require_declared_intent
                && report.documents.is_empty()
                && !report.validation.declared
            {
                let available_intents = resolver::declared_intents(&roots, fallback_mode, &catalog);
                return print_failure_rendered(
                    args.format,
                    render_undeclared_intent_error(
                        args.format,
                        intent.as_str(),
                        &available_intents,
                    ),
                    EXIT_DATA,
                );
            }
            let exit_code = if args.strict && report.has_unsatisfied_required() {
                EXIT_STRICT
            } else {
                EXIT_OK
            };
            print_rendered(render_preflight(args.format, &report), exit_code)
        }
        Command::Init(args) => {
            let roots = match resolve_roots_or_exit(&overrides) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let (mode, force) = init_mode(&args);
            match commands::init::run_init(&roots, mode, force) {
                Ok(report) => print_rendered(render_init(args.format, &report), EXIT_OK),
                Err(err) => {
                    eprintln!("error: {err}");
                    match err {
                        commands::init::InitError::AlreadyExists(_) => EXIT_STRICT,
                        commands::init::InitError::Io(_) => EXIT_RUNTIME,
                    }
                }
            }
        }
        Command::Explain(args) => {
            let roots = match resolve_roots_or_exit(&overrides) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let catalog = match load_catalog_from_roots(&roots) {
                Ok(catalog) => catalog,
                Err(err) => {
                    eprintln!("error: {err}");
                    return config_error_exit_code(&err);
                }
            };
            match args.intent {
                Some(raw) => {
                    let intent = match parse_intent(&raw) {
                        Ok(intent) => intent,
                        Err(code) => return code,
                    };
                    let report = resolver::resolve_intent_with_catalog(
                        &intent,
                        &roots,
                        false,
                        fallback_mode,
                        false,
                        &catalog,
                    );
                    let payload = ExplainIntent {
                        intent: intent.as_str(),
                        documents: &report.documents,
                        validation: &report.validation,
                    };
                    print_rendered(render_explain_intent(args.format, &payload), EXIT_OK)
                }
                None => {
                    let intents = resolver::available_intents(&catalog);
                    let payload = ExplainIntents { intents: &intents };
                    print_rendered(render_explain_intents(args.format, &payload), EXIT_OK)
                }
            }
        }
        Command::List(args) => {
            let roots = match resolve_roots_or_exit(&overrides) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let catalog = match load_catalog_from_roots(&roots) {
                Ok(catalog) => catalog,
                Err(err) => {
                    eprintln!("error: {err}");
                    return config_error_exit_code(&err);
                }
            };
            let documents = resolver::resolve_all_documents(&roots, fallback_mode, &catalog);
            let validations = resolver::all_validation_contracts(&roots, &catalog);
            let intents = resolver::available_intents(&catalog);
            let report = ListReport {
                docs_home: roots.docs_home.clone(),
                project_path: roots.project_path.clone(),
                intents,
                documents,
                validations,
            };
            print_rendered(render_list(args.format, &report), EXIT_OK)
        }
        Command::Remove(args) => {
            let intent = match parse_intent(&args.context) {
                Ok(intent) => intent,
                Err(code) => return code,
            };
            let roots = match resolve_roots_or_exit(&overrides) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let request = commands::remove::RemoveRequest {
                context: intent.as_str().to_string(),
                scope: args.scope,
                path: args.path,
            };
            match commands::remove::run_remove(&roots, request) {
                Ok(report) => print_rendered(render_remove(args.format, &report), EXIT_OK),
                Err(err) => {
                    eprintln!("error: {err}");
                    match err {
                        commands::remove::RemoveError::Parse(_) => EXIT_CONFIG,
                        commands::remove::RemoveError::Io(_) => EXIT_RUNTIME,
                    }
                }
            }
        }
        Command::Completion(args) => completion::run(args.shell),
    }
}

fn init_mode(args: &cli::InitArgs) -> (InitMode, bool) {
    if args.print {
        (InitMode::Print, false)
    } else if args.dry_run {
        (InitMode::DryRun, false)
    } else if args.force {
        (InitMode::Write, true)
    } else {
        // Default is non-mutating: print the stub to stdout.
        (InitMode::Print, false)
    }
}

fn parse_intent(raw: &str) -> Result<Context, i32> {
    Context::parse(raw).map_err(|message| {
        eprintln!("error: invalid --intent/--context: {message}");
        EXIT_USAGE
    })
}

fn resolve_roots_or_exit(overrides: &PathOverrides) -> Result<ResolvedRoots, i32> {
    resolve_roots(overrides).map_err(|err| {
        eprintln!("error: {err:#}");
        EXIT_RUNTIME
    })
}

fn config_error_exit_code(err: &ConfigLoadError) -> i32 {
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

fn print_failure_rendered(
    format: model::OutputFormat,
    rendered: anyhow::Result<String>,
    failure_exit_code: i32,
) -> i32 {
    match rendered {
        Ok(body) => {
            match format {
                model::OutputFormat::Json => println!("{body}"),
                model::OutputFormat::Text => eprintln!("{body}"),
            }
            failure_exit_code
        }
        Err(err) => {
            eprintln!("error: {err:#}");
            EXIT_RUNTIME
        }
    }
}

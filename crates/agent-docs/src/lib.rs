mod cli;
pub mod commands;
mod completion;
pub mod config;
pub mod content;
pub mod env;
mod integration;
pub mod model;
pub mod output;
pub mod path_classes;
pub mod paths;
pub mod predicate;
pub mod resolver;
mod session;
mod user_config;

use clap::Parser;

use cli::{Cli, Command};
use config::load_catalog_from_roots;
use env::{PathOverrides, ResolvedRoots, resolve_roots};
use model::{ConfigErrorKind, ConfigLoadError, Context, InitMode, ListReport};
use output::{
    ExplainIntent, ExplainIntents, render_audit, render_explain_intent, render_explain_intents,
    render_init, render_list, render_preflight, render_remove, render_undeclared_intent_error,
};

use nils_common::cli_contract::{Envelope, EnvelopeError, exit, schema_version_for};

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
    let args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
    let output_format = detect_cli_output_format(&args);
    let cli = match Cli::try_parse_from(args.iter().cloned()) {
        Ok(parsed) => parsed,
        Err(err) => {
            use clap::error::ErrorKind;
            let kind = err.kind();
            if matches!(
                kind,
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                let code = err.exit_code();
                let _ = err.print();
                return code;
            }
            let code = match kind {
                ErrorKind::InvalidSubcommand => "unknown-subcommand",
                _ => "parse-error",
            };
            return nils_common::cli_contract::emit_parse_error(
                "agent-docs",
                output_format,
                code,
                &render_clap_message(&err),
            );
        }
    };

    dispatch(cli, output_format)
}

fn detect_cli_output_format(
    args: &[std::ffi::OsString],
) -> nils_common::cli_contract::OutputFormat {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        let arg = arg.to_string_lossy();
        if arg == "--format"
            && let Some(next) = iter.next()
            && next.to_string_lossy().eq_ignore_ascii_case("json")
        {
            return nils_common::cli_contract::OutputFormat::Json;
        }
        if let Some(value) = arg.strip_prefix("--format=")
            && value.eq_ignore_ascii_case("json")
        {
            return nils_common::cli_contract::OutputFormat::Json;
        }
    }
    nils_common::cli_contract::OutputFormat::Text
}

fn render_clap_message(err: &clap::Error) -> String {
    err.to_string()
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| {
            let line = line.trim();
            line.strip_prefix("error:")
                .map(str::trim)
                .unwrap_or(line)
                .to_string()
        })
        .unwrap_or_else(|| "command-line parse failed".to_string())
}

fn dispatch(cli: Cli, output_format: nils_common::cli_contract::OutputFormat) -> i32 {
    if cli.user_config
        && !matches!(
            &cli.command,
            Command::Preflight(_) | Command::Explain(_) | Command::List(_) | Command::Session(_)
        )
    {
        return nils_common::cli_contract::emit_parse_error(
            "agent-docs",
            output_format,
            "invalid-user-config-command",
            "--user-config is supported only by preflight, explain, list, and session",
        );
    }

    let fallback_mode = cli.worktree_fallback;
    let use_user_config = cli.user_config;
    let integration_fingerprint = cli.integration_fingerprint;
    let overrides = PathOverrides {
        docs_home: cli.docs_home,
        project_path: cli.project_path,
    };

    match cli.command {
        Command::Audit(args) => {
            let roots = match resolve_roots_or_exit(
                &overrides,
                CatalogCommandContract {
                    format: args.format,
                    command: "audit",
                    schema_version: 2,
                },
            ) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let report = match commands::audit::run_audit(
                args.target,
                &roots,
                args.product,
                args.strict,
                fallback_mode,
            ) {
                Ok(report) => report,
                Err(err) => {
                    let exit_code = config_error_exit_code(&err);
                    return render_command_failure(
                        args.format,
                        "audit",
                        2,
                        "catalog-load-failed",
                        &err.to_string(),
                        exit_code,
                    );
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
            let roots = match resolve_roots_or_exit(
                &overrides,
                CatalogCommandContract {
                    format: args.format,
                    command: "preflight",
                    schema_version: 2,
                },
            ) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let catalog = match load_effective_catalog(
                &roots,
                use_user_config,
                args.product,
                fallback_mode,
                integration_fingerprint.as_deref(),
                CatalogCommandContract {
                    format: args.format,
                    command: "preflight",
                    schema_version: 2,
                },
            ) {
                Ok(catalog) => catalog,
                Err(code) => return code,
            };
            let report = resolver::resolve_intent_with_effective_catalog_for_product(
                &intent,
                &roots,
                args.product,
                args.strict,
                fallback_mode,
                true,
                &catalog,
            );
            if args.require_declared_intent {
                let available_intents = resolver::declared_intents_for_product(
                    &roots,
                    args.product,
                    fallback_mode,
                    &catalog.catalog,
                );
                if !available_intents.iter().any(|name| name == intent.as_str()) {
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
            }
            let exit_code = if args.strict && report.has_unsatisfied_required() {
                EXIT_STRICT
            } else {
                EXIT_OK
            };
            print_rendered(render_preflight(args.format, &report), exit_code)
        }
        Command::Init(args) => {
            let roots = match resolve_roots_or_exit(
                &overrides,
                CatalogCommandContract {
                    format: args.format,
                    command: "init",
                    schema_version: 1,
                },
            ) {
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
            let roots = match resolve_roots_or_exit(
                &overrides,
                CatalogCommandContract {
                    format: args.format,
                    command: "explain",
                    schema_version: 1,
                },
            ) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let catalog = match load_effective_catalog(
                &roots,
                use_user_config,
                args.product,
                fallback_mode,
                integration_fingerprint.as_deref(),
                CatalogCommandContract {
                    format: args.format,
                    command: "explain",
                    schema_version: 1,
                },
            ) {
                Ok(catalog) => catalog,
                Err(code) => return code,
            };
            match args.intent {
                Some(raw) => {
                    let intent = match parse_intent(&raw) {
                        Ok(intent) => intent,
                        Err(code) => return code,
                    };
                    let report = resolver::resolve_intent_with_effective_catalog_for_product(
                        &intent,
                        &roots,
                        args.product,
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
                    let intents = resolver::available_intents_for_product_in_roots(
                        &roots,
                        args.product,
                        &catalog.catalog,
                    );
                    let payload = ExplainIntents { intents: &intents };
                    print_rendered(render_explain_intents(args.format, &payload), EXIT_OK)
                }
            }
        }
        Command::List(args) => {
            let roots = match resolve_roots_or_exit(
                &overrides,
                CatalogCommandContract {
                    format: args.format,
                    command: "list",
                    schema_version: 1,
                },
            ) {
                Ok(roots) => roots,
                Err(code) => return code,
            };
            let catalog = match load_effective_catalog(
                &roots,
                use_user_config,
                args.product,
                fallback_mode,
                integration_fingerprint.as_deref(),
                CatalogCommandContract {
                    format: args.format,
                    command: "list",
                    schema_version: 1,
                },
            ) {
                Ok(catalog) => catalog,
                Err(code) => return code,
            };
            let documents = resolver::resolve_all_documents_for_product_policy(
                &roots,
                args.product,
                fallback_mode,
                &catalog.catalog,
                &catalog.private_allowed_roots,
            );
            let validations = resolver::all_validation_contracts_for_product(
                &roots,
                args.product,
                &catalog.catalog,
            );
            let intents = resolver::available_intents_for_product_in_roots(
                &roots,
                args.product,
                &catalog.catalog,
            );
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
            let roots = match resolve_roots_or_exit(
                &overrides,
                CatalogCommandContract {
                    format: args.format,
                    command: "remove",
                    schema_version: 1,
                },
            ) {
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
        Command::Config(args) => user_config::run(args, &overrides),
        Command::Integration(args) => integration::run(args, &overrides, fallback_mode),
        Command::Session(args) => session::run(
            args,
            overrides,
            fallback_mode,
            use_user_config,
            integration_fingerprint,
        ),
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

fn resolve_roots_or_exit(
    overrides: &PathOverrides,
    contract: CatalogCommandContract,
) -> Result<ResolvedRoots, i32> {
    resolve_roots(overrides).map_err(|err| {
        render_command_failure(
            contract.format,
            contract.command,
            contract.schema_version,
            "root-resolution-failed",
            &format!("{err:#}"),
            EXIT_RUNTIME,
        )
    })
}

#[derive(Clone, Copy)]
struct CatalogCommandContract {
    format: model::OutputFormat,
    command: &'static str,
    schema_version: u32,
}

fn load_effective_catalog(
    roots: &ResolvedRoots,
    use_user_config: bool,
    product: Option<model::Product>,
    fallback_mode: model::FallbackMode,
    integration_fingerprint: Option<&str>,
    contract: CatalogCommandContract,
) -> Result<integration::EffectiveCatalog, i32> {
    if use_user_config {
        let Some(product) = product else {
            return Err(nils_common::cli_contract::emit_parse_error(
                "agent-docs",
                contract_output_format(contract.format),
                "user-config-requires-product",
                "--user-config requires --product",
            ));
        };
        return integration::load_bound_catalog(
            roots,
            product,
            fallback_mode,
            integration_fingerprint,
        )
        .map_err(|err| {
            let exit_code = match err.kind() {
                integration::BoundCatalogErrorKind::Config => EXIT_CONFIG,
                integration::BoundCatalogErrorKind::Data => EXIT_DATA,
                integration::BoundCatalogErrorKind::Runtime => EXIT_RUNTIME,
            };
            render_command_failure(
                contract.format,
                contract.command,
                contract.schema_version,
                err.code(),
                &err.to_string(),
                exit_code,
            )
        });
    }
    load_catalog_from_roots(roots)
        .map(|catalog| integration::EffectiveCatalog {
            catalog,
            private_project_catalog: false,
            private_allowed_roots: Vec::new(),
        })
        .map_err(|err| {
            let exit_code = config_error_exit_code(&err);
            render_command_failure(
                contract.format,
                contract.command,
                contract.schema_version,
                "catalog-load-failed",
                &err.to_string(),
                exit_code,
            )
        })
}

fn contract_output_format(format: model::OutputFormat) -> nils_common::cli_contract::OutputFormat {
    match format {
        model::OutputFormat::Text => nils_common::cli_contract::OutputFormat::Text,
        model::OutputFormat::Json => nils_common::cli_contract::OutputFormat::Json,
    }
}

fn render_command_failure(
    format: model::OutputFormat,
    command: &str,
    schema_version: u32,
    code: &str,
    message: &str,
    exit_code: i32,
) -> i32 {
    match format {
        model::OutputFormat::Json => {
            let envelope: Envelope<()> = Envelope::failure(
                schema_version_for("agent-docs", command, schema_version),
                EnvelopeError::new(code, message),
            );
            match serde_json::to_string(&envelope) {
                Ok(serialized) => println!("{serialized}"),
                Err(err) => eprintln!("error: {err:#}"),
            }
        }
        model::OutputFormat::Text => eprintln!("error: {message}"),
    }
    exit_code
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

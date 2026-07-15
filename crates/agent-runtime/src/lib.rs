//! `agent-runtime` CLI library. Render / install / doctor /
//! audit-drift for graysurf/agent-runtime-kit.
//!
//! ## Determinism contract (Resolved Decision #9)
//!
//! Render output must be a pure function of the source-root contents:
//! no wall-clock time, no hash-randomized iteration. The crate-wide
//! `#![deny(...)]` attribute below pairs with `clippy.toml` to make
//! `std::collections::HashMap`, `std::time::SystemTime::now`, and
//! `chrono::Utc::now` build failures. The single sanctioned time
//! value is `render::time::source_commit_timestamp()`. Helpers under
//! `render::helpers` are the only sanctioned `HashMap` site — Tera's
//! `Function` trait forces the signature, and the lint is silenced
//! exactly there.
//!
//! Source: `agent-runtime-kit/docs/source/inventory-target-architecture.md`
//! → Resolved Decision #9.
#![deny(clippy::disallowed_types, clippy::disallowed_methods)]

use clap::{Arg, ArgAction, CommandFactory, FromArgMatches, Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

pub mod audit_drift;
pub mod commands;
pub mod doctor;
pub mod gc_backups;
pub mod install;
pub mod live_surface;
pub mod managed_block;
pub mod prune_stale;
pub mod purge_state;
pub mod render;
pub mod restore_backups;
pub mod uninstall;

#[derive(Parser)]
#[command(
    name = "agent-runtime",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Render / install / doctor / audit-drift for graysurf/agent-runtime-kit."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Render `core/` + `targets/<product>/` into `build/<product>/`.
    Render(commands::render::RenderArgs),
    /// Activate rendered output against a product's runtime home.
    Install(commands::install::InstallArgs),
    /// Remove installed renderer output from a product's runtime home.
    Uninstall(commands::uninstall::UninstallArgs),
    /// Diagnose host setup, runtime roots, and required CLI floors.
    Doctor(commands::doctor::DoctorArgs),
    /// Preview or apply the host bootstrap phases for Codex and Claude.
    BootstrapHost(commands::bootstrap_host::BootstrapHostArgs),
    /// Detect source-vs-rendered, rendered-vs-live, and unsafe drift.
    AuditDrift(commands::audit_drift::AuditDriftArgs),
    /// Prune old backups under `<state_home>/backups/`.
    GcBackups(commands::gc_backups::GcBackupsArgs),
    /// List the skills an `install` would activate for a product.
    ListSkills(commands::list_skills::ListSkillsArgs),
    /// Render standardized PR / MR bodies for forge-cli create flows.
    PrBody(commands::pr_body::PrBodyArgs),
    /// Remove stale managed runtime-home surfaces.
    PruneStale(commands::prune_stale::PruneStaleArgs),
    /// Restore a runtime home from a recorded backup snapshot.
    RestoreBackups(commands::restore_backups::RestoreBackupsArgs),
    /// Purge runtime-managed state (use with caution).
    PurgeState(commands::purge_state::PurgeStateArgs),
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::Render(_) => "render",
            Command::Install(_) => "install",
            Command::Uninstall(_) => "uninstall",
            Command::Doctor(_) => "doctor",
            Command::BootstrapHost(_) => "bootstrap-host",
            Command::AuditDrift(_) => "audit-drift",
            Command::GcBackups(_) => "gc-backups",
            Command::ListSkills(_) => "list-skills",
            Command::PrBody(_) => "pr-body",
            Command::PruneStale(_) => "prune-stale",
            Command::RestoreBackups(_) => "restore-backups",
            Command::PurgeState(_) => "purge-state",
        }
    }
}

pub fn run() -> ExitCode {
    run_cli(Cli::parse(), &[])
}

pub fn binary_command() -> clap::Command {
    Cli::command().mut_subcommand("prune-stale", |command| {
        command.arg(
            Arg::new("owned_source_root")
                .long("owned-source-root")
                .value_name("ABSOLUTE_PATH")
                .help("Trust an explicit prior runtime-kit source root; repeatable")
                .action(ArgAction::Append)
                .value_parser(clap::value_parser!(PathBuf)),
        )
    })
}

pub fn run_binary() -> ExitCode {
    let matches = binary_command().get_matches();
    let (cli, owned_source_roots) =
        cli_and_owned_source_roots(matches).unwrap_or_else(|error| error.exit());
    run_cli(cli, &owned_source_roots)
}

fn cli_and_owned_source_roots(
    matches: clap::ArgMatches,
) -> Result<(Cli, Vec<PathBuf>), clap::Error> {
    let owned_source_roots = match matches.subcommand() {
        Some(("prune-stale", matches)) => matches
            .get_many::<PathBuf>("owned_source_root")
            .map(|roots| roots.cloned().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    Cli::from_arg_matches(&matches).map(|cli| (cli, owned_source_roots))
}

fn run_cli(cli: Cli, owned_source_roots: &[PathBuf]) -> ExitCode {
    let name = cli.command.name();
    match cli.command {
        Command::Render(args) => match commands::render::run(args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("agent-runtime render: {err:#}");
                ExitCode::from(2)
            }
        },
        Command::AuditDrift(args) => match commands::audit_drift::run(args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("agent-runtime audit-drift: {err:#}");
                ExitCode::from(2)
            }
        },
        Command::BootstrapHost(args) => match commands::bootstrap_host::run(args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("agent-runtime bootstrap-host: {err:#}");
                ExitCode::from(2)
            }
        },
        Command::Install(args) => match commands::install::run(args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("agent-runtime install: {err:#}");
                ExitCode::from(2)
            }
        },
        Command::Uninstall(args) => match commands::uninstall::run(args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("agent-runtime uninstall: {err:#}");
                ExitCode::from(2)
            }
        },
        Command::RestoreBackups(args) => match commands::restore_backups::run(args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("agent-runtime restore-backups: {err:#}");
                ExitCode::from(2)
            }
        },
        Command::PurgeState(args) => match commands::purge_state::run(args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("agent-runtime purge-state: {err:#}");
                ExitCode::from(2)
            }
        },
        Command::GcBackups(args) => match commands::gc_backups::run(args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("agent-runtime gc-backups: {err:#}");
                ExitCode::from(2)
            }
        },
        Command::ListSkills(args) => match commands::list_skills::run(args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("agent-runtime list-skills: {err:#}");
                ExitCode::from(2)
            }
        },
        Command::PrBody(args) => match commands::pr_body::run(args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("agent-runtime pr-body: {err:#}");
                ExitCode::from(2)
            }
        },
        Command::PruneStale(args) => {
            match commands::prune_stale::run_with_owned_source_roots(args, owned_source_roots) {
                Ok(code) => ExitCode::from(code),
                Err(err) => {
                    eprintln!("agent-runtime prune-stale: {err:#}");
                    ExitCode::from(2)
                }
            }
        }
        Command::Doctor(args) => match commands::doctor::run(args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("agent-runtime {name}: {err:#}");
                ExitCode::from(2)
            }
        },
    }
}

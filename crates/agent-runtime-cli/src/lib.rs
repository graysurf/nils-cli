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

use clap::{Parser, Subcommand};
use std::process::ExitCode;

pub mod audit_drift;
pub mod commands;
pub mod install;
pub mod managed_block;
pub mod render;

#[derive(Parser)]
#[command(
    name = "agent-runtime",
    version,
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
    Uninstall,
    /// Diagnose host setup, runtime roots, and required CLI floors.
    Doctor,
    /// Detect source-vs-rendered, rendered-vs-live, and unsafe drift.
    AuditDrift(commands::audit_drift::AuditDriftArgs),
    /// Prune old backups under `<state_home>/backups/`.
    GcBackups,
    /// Restore a runtime home from a recorded backup snapshot.
    RestoreBackups,
    /// Purge runtime-managed state (use with caution).
    PurgeState,
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::Render(_) => "render",
            Command::Install(_) => "install",
            Command::Uninstall => "uninstall",
            Command::Doctor => "doctor",
            Command::AuditDrift(_) => "audit-drift",
            Command::GcBackups => "gc-backups",
            Command::RestoreBackups => "restore-backups",
            Command::PurgeState => "purge-state",
        }
    }
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
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
        Command::Install(args) => match commands::install::run(args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("agent-runtime install: {err:#}");
                ExitCode::from(2)
            }
        },
        _ => {
            eprintln!("agent-runtime {name}: not implemented");
            ExitCode::from(1)
        }
    }
}

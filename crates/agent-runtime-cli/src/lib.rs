use clap::{Parser, Subcommand};
use std::process::ExitCode;

pub mod commands;
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
    Install,
    /// Remove installed renderer output from a product's runtime home.
    Uninstall,
    /// Diagnose host setup, runtime roots, and required CLI floors.
    Doctor,
    /// Detect source-vs-rendered, rendered-vs-live, and unsafe drift.
    AuditDrift,
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
            Command::Install => "install",
            Command::Uninstall => "uninstall",
            Command::Doctor => "doctor",
            Command::AuditDrift => "audit-drift",
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
        _ => {
            eprintln!("agent-runtime {name}: not implemented");
            ExitCode::from(1)
        }
    }
}

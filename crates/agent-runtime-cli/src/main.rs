use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "agent-runtime",
    version,
    about = "Render / install / doctor / audit-drift for graysurf/agent-runtime-kit (Plan 01 stub — every subcommand exits 1)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render `core/` + `targets/<product>/` into `build/<product>/`.
    Render,
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
            Command::Render => "render",
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

fn main() -> ExitCode {
    let cli = Cli::parse();
    eprintln!("agent-runtime {}: not implemented", cli.command.name());
    ExitCode::from(1)
}

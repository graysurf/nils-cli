use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};

#[derive(Debug, Parser)]
#[command(
    name = "test-first-evidence",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Record test-first evidence and waivers for agent workflows.",
    long_about = "Create and verify test-first evidence records that capture failing tests, waivers, and final validation.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  test-first-evidence init --out /tmp/evidence --classification behavior-change --production-path src/lib.rs\n  test-first-evidence record-failing --out /tmp/evidence --command 'cargo test bug_repro' --exit-code 101 --summary 'bug reproduced'\n  test-first-evidence record-waiver --out /tmp/evidence --reason 'docs-only change'\n  test-first-evidence record-final --out /tmp/evidence --command 'cargo test bug_repro' --status pass\n  test-first-evidence verify --out /tmp/evidence --format json\n  test-first-evidence completion zsh\n\nENVIRONMENT:\n  none\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
#[command(rename_all = "kebab-case")]
pub enum Command {
    /// Create a deterministic evidence record.
    Init(InitArgs),
    /// Record a failing test or reproducible failure before the fix.
    RecordFailing(RecordFailingArgs),
    /// Record an explicit waiver when failing evidence is not practical.
    RecordWaiver(RecordWaiverArgs),
    /// Record final validation after the implementation.
    RecordFinal(RecordFinalArgs),
    /// Verify the evidence record is complete enough for delivery.
    Verify(CommonArgs),
    /// Print the current evidence record.
    Show(CommonArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
pub struct CommonArgs {
    /// Evidence artifact directory containing `test-first-evidence.json`.
    #[arg(long = "out", value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub out_dir: PathBuf,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Change classification such as behavior-change, bug-fix, docs-only, or generated-only.
    #[arg(long, value_name = "TEXT")]
    pub classification: String,

    /// Production path affected by the change. Repeat for multiple paths.
    #[arg(long = "production-path", value_name = "PATH", value_hint = ValueHint::AnyPath)]
    pub production_paths: Vec<PathBuf>,

    /// Optional note to keep with the record. Repeat for multiple notes.
    #[arg(long = "note", value_name = "TEXT")]
    pub notes: Vec<String>,

    /// Overwrite an existing record.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct RecordFailingArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Command or manual step that produced the before-fix failure.
    #[arg(long, value_name = "TEXT")]
    pub command: String,

    /// Exit code from the failing command.
    #[arg(long, value_name = "CODE")]
    pub exit_code: i32,

    /// Concise failure summary.
    #[arg(long, value_name = "TEXT")]
    pub summary: String,

    /// Optional test name or scenario identifier.
    #[arg(long = "test-name", value_name = "TEXT")]
    pub test_name: Option<String>,

    /// Optional evidence artifact path. Repeat for multiple artifacts.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    pub artifact: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RecordWaiverArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Reason failing-test evidence was not practical.
    #[arg(long, value_name = "TEXT")]
    pub reason: String,

    /// Substitute validation captured before editing. Repeat for multiple items.
    #[arg(long = "substitute-validation", value_name = "TEXT")]
    pub substitute_validation: Vec<String>,
}

#[derive(Debug, Args)]
pub struct RecordFinalArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Final validation command or manual validation step.
    #[arg(long, value_name = "TEXT")]
    pub command: String,

    /// Final validation status.
    #[arg(long, value_enum)]
    pub status: ValidationStatus,

    /// Optional final validation summary.
    #[arg(long, value_name = "TEXT")]
    pub summary: Option<String>,

    /// Optional validation artifact path. Repeat for multiple artifacts.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    pub artifact: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum)]
    pub shell: crate::completion::CompletionShell,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ValidationStatus {
    Pass,
    Fail,
}

impl ValidationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

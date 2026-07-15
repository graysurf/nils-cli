use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ErrorFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceMode {
    Minimal,
    Debug,
    Sensitive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    App,
    Daemon,
    Auto,
    Process,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProfile {
    Observe,
    Interact,
    Extended,
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "macos-agent",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Run a pinned Peekaboo backend through a guarded macOS adapter.",
    long_about = "Install and verify one immutable Peekaboo release, execute it locally or over SSH, enforce MCP tool profiles, and retain privacy-preserving execution journals.",
    after_help = "EXAMPLES:\n  macos-agent backend status --format json\n  macos-agent doctor --strict --format json\n  macos-agent exec --out-dir ./run --intent 'Inspect Calculator' -- see --app Calculator --json\n  macos-agent scenario --out-dir ./run --file ./flow.peekaboo.json\n  macos-agent mcp --out-dir ./run --tool-profile interact\n  macos-agent journal review --out-dir ./run --format json\n\nEXIT CODES:\n  0   success\n  64  usage\n  69  backend unavailable or invalid\n  70  upstream failure\n  74  journal or artifact failure\n  75  transport failure\n  77  permission failure\n  78  policy refusal",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Human-readable or versioned JSON output.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text, global = true)]
    pub format: OutputFormat,

    /// Human-readable or versioned JSON errors on stderr.
    #[arg(long, value_enum, default_value_t = ErrorFormat::Text, global = true)]
    pub error_format: ErrorFormat,

    #[command(subcommand)]
    pub command: CommandGroup,
}

#[derive(Debug, Clone, Subcommand)]
pub enum CommandGroup {
    /// Install, inspect, verify, or roll back the locked Peekaboo backend.
    Backend {
        #[command(subcommand)]
        command: BackendCommand,
    },
    /// Verify backend integrity, runtime readiness, Bridge state, and permissions.
    Doctor(DoctorArgs),
    /// Print the adapter capability and safety ceiling.
    Capabilities(CapabilitiesArgs),
    /// Execute Peekaboo arguments verbatim after `--`.
    Exec(ExecArgs),
    /// Run a local `.peekaboo.json` scenario through the locked backend.
    Scenario(ScenarioArgs),
    /// Proxy Peekaboo's stdio MCP server with a strict tool profile.
    Mcp(McpArgs),
    /// Summarize, review, plan replay, or replay one guarded journal step.
    Journal {
        #[command(subcommand)]
        command: JournalCommand,
    },
    /// Print generated shell completion.
    Completion(CompletionArgs),
    #[command(name = "__remote", hide = true)]
    Remote,
    #[command(name = "__remote-mcp", hide = true)]
    RemoteMcp(RemoteSessionArgs),
    #[command(name = "__remote-cleanup", hide = true)]
    RemoteCleanup(RemoteSessionArgs),
}

#[derive(Debug, Clone, Subcommand)]
pub enum BackendCommand {
    /// Download, verify, and atomically activate the locked release.
    Install(BackendMutationArgs),
    /// Report the current and previous receipts without exposing local paths.
    Status(BackendStatusArgs),
    /// Verify the active receipt, assets, signatures, version, and strict assessments.
    Verify(BackendVerifyArgs),
    /// Atomically select the verified previous receipt.
    Rollback(BackendMutationArgs),
}

#[derive(Debug, Clone, Args)]
pub struct BackendMutationArgs {
    /// Runtime-only trusted SSH target; never persisted.
    #[arg(long)]
    pub host: Option<String>,
    /// Show the planned operation without changing backend state.
    #[arg(long)]
    pub dry_run: bool,
    /// Enforce the locked trust policy and disclose any exact notarization waiver.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Clone, Args)]
pub struct BackendStatusArgs {
    /// Runtime-only trusted SSH target; never persisted.
    #[arg(long)]
    pub host: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct BackendVerifyArgs {
    /// Runtime-only trusted SSH target; never persisted.
    #[arg(long)]
    pub host: Option<String>,
    /// Enforce the locked trust policy and disclose any exact notarization waiver.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Clone, Args)]
pub struct DoctorArgs {
    /// Runtime-only trusted SSH target; never persisted.
    #[arg(long)]
    pub host: Option<String>,
    /// Fail when any required backend, Bridge, permission, or capability check is not ready.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Clone, Args)]
pub struct CapabilitiesArgs {
    /// Runtime-only trusted SSH target; never persisted.
    #[arg(long)]
    pub host: Option<String>,
    /// Include live backend probes in addition to the static matrix.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Clone, Args)]
pub struct ExecArgs {
    /// Runtime-only trusted SSH target; never persisted.
    #[arg(long)]
    pub host: Option<String>,
    /// Caller-owned directory for the structural execution journal.
    #[arg(long)]
    pub out_dir: PathBuf,
    /// Concise operator intent; private values are redacted before persistence.
    #[arg(long)]
    pub intent: Option<String>,
    /// Observable postcondition expected by the caller.
    #[arg(long)]
    pub expected: Option<String>,
    /// Evidence retention and suppression mode.
    #[arg(long, value_enum, default_value_t = EvidenceMode::Minimal)]
    pub evidence_mode: EvidenceMode,
    /// Permission/runtime authority used for this invocation.
    #[arg(long, value_enum, default_value_t = RuntimeMode::App)]
    pub runtime: RuntimeMode,
    /// Wall-clock bound for the upstream process.
    #[arg(long, default_value_t = 60)]
    pub timeout_seconds: u64,
    /// Peekaboo arguments, passed without grammar translation.
    #[arg(last = true, required = true, allow_hyphen_values = true, num_args = 1..)]
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct ScenarioArgs {
    /// Runtime-only trusted SSH target; never persisted.
    #[arg(long)]
    pub host: Option<String>,
    /// Caller-owned directory for the structural execution journal.
    #[arg(long)]
    pub out_dir: PathBuf,
    /// Local scenario file staged read-only for execution.
    #[arg(long)]
    pub file: PathBuf,
    /// Evidence retention and suppression mode.
    #[arg(long, value_enum, default_value_t = EvidenceMode::Minimal)]
    pub evidence_mode: EvidenceMode,
    /// Permission/runtime authority used for this invocation.
    #[arg(long, value_enum, default_value_t = RuntimeMode::App)]
    pub runtime: RuntimeMode,
    /// Wall-clock bound for the upstream scenario.
    #[arg(long, default_value_t = 300)]
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Args)]
pub struct McpArgs {
    /// Runtime-only trusted SSH target; never persisted.
    #[arg(long)]
    pub host: Option<String>,
    /// Caller-owned directory for the structural MCP journal.
    #[arg(long)]
    pub out_dir: PathBuf,
    /// Exact admitted tool family.
    #[arg(long, value_enum, default_value_t = ToolProfile::Interact)]
    pub tool_profile: ToolProfile,
    /// Permission/runtime authority used for this server.
    #[arg(long, value_enum, default_value_t = RuntimeMode::App)]
    pub runtime: RuntimeMode,
}

#[derive(Debug, Clone, Subcommand)]
pub enum JournalCommand {
    /// Rebuild and print a deterministic run summary.
    Summarize(JournalReadArgs),
    /// Cluster failures and propose explicit defect owners.
    Review(JournalReadArgs),
    /// Print guarded replay eligibility without executing anything.
    ReplayPlan(JournalReplayPlanArgs),
    /// Replay exactly one eligible step after current-state checks.
    ReplayStep(JournalReplayStepArgs),
}

#[derive(Debug, Clone, Args)]
pub struct JournalReadArgs {
    /// Existing structural execution journal directory.
    #[arg(long)]
    pub out_dir: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct JournalReplayPlanArgs {
    /// Existing structural execution journal directory.
    #[arg(long)]
    pub out_dir: PathBuf,
    /// Optional journal step identifier to inspect.
    #[arg(long)]
    pub step: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct JournalReplayStepArgs {
    /// Existing structural execution journal directory.
    #[arg(long)]
    pub out_dir: PathBuf,
    /// Journal step identifier to replay.
    #[arg(long)]
    pub step: String,
    /// Acknowledge conditional replay after current-state checks.
    #[arg(long)]
    pub confirm_conditional: bool,
    /// Current caller-observed snapshot/state reference for conditional replay.
    #[arg(long)]
    pub current_snapshot: Option<String>,
    /// Fresh caller-observed postcondition required for conditional replay.
    #[arg(long)]
    pub expected: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct CompletionArgs {
    #[arg(value_enum)]
    pub shell: crate::completion::CompletionShell,
}

#[derive(Debug, Clone, Args)]
pub struct RemoteSessionArgs {
    #[arg(long)]
    pub token: String,
}

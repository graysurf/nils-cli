use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};
use nils_common::cli_contract::OutputFormat;

use crate::model::{Product, RecoveryScope};

#[derive(Debug, Parser)]
#[command(
    name = "agent-hook",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Dispatch shared agent policy through provider-native hook ingress.",
    long_about = "Validate and dispatch one strict cross-provider hook policy, manage its exact provider ingress, inspect convergence, and operate governed recovery.",
    arg_required_else_help = true,
    disable_help_subcommand = true,
    after_help = "ENVIRONMENT:\n  XDG_CONFIG_HOME  Parent for agent-hook/config.toml.\n  XDG_DATA_HOME    Parent for installed policy bundles.\n  XDG_STATE_HOME   Parent for private trace, setup, and recovery state.\n  HOME             Absolute fallback for XDG roots.\n  AGENT_SESSION_STATE_DIR  Optional #676 coordination state root.\n\nEXIT CODES:\n  0   success or provider-native decision rendered\n  1   runtime failure or service-format provider block\n  2   provider-native blocking fallback when the event cannot be recovered\n  64  invalid command usage\n  65  invalid config, policy, input, drift, or recovery data\n  69  required provider/setup resource or lock is unavailable\n  75  concurrent state mutation; bounded retry may succeed"
)]
pub struct Cli {
    /// Override the strict user config path.
    #[arg(long, global = true, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub config: Option<PathBuf>,

    /// Override the selected versioned policy bundle path.
    #[arg(long, global = true, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub policy: Option<PathBuf>,

    /// Override the private state root.
    #[arg(long, global = true, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub state_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Normalize one provider request, evaluate policy, and render one decision.
    Dispatch(DispatchArgs),
    /// Validate strict config, selected policy, overrides, and digests.
    Validate(FormatArgs),
    /// List public rule metadata and effective modes.
    Inventory(FormatArgs),
    /// Inspect policy, provider registrations, compatibility residue, and recovery state.
    Doctor(DoctorArgs),
    /// Preview, apply, repair, or remove exact owned provider ingress.
    Setup(SetupArgs),
    /// Create, authorize, inspect, consume, or revoke governed recovery.
    Recovery(RecoveryArgs),
    /// Print a shell completion script.
    Completion(CompletionArgs),
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum DispatchFormat {
    /// Provider-native output for hook ingress.
    #[default]
    Provider,
    /// Human-readable normalized decision.
    Text,
    /// Versioned service JSON envelope.
    Json,
}

#[derive(Debug, Args)]
pub struct DispatchArgs {
    /// Provider whose bounded JSON request is read from stdin.
    #[arg(long, value_enum)]
    pub product: Product,

    /// Provider event when the request does not carry one.
    #[arg(long, value_name = "EVENT")]
    pub event: Option<String>,

    /// Evaluate every selected rule as side-effect-free shadow.
    #[arg(long)]
    pub shadow: bool,

    /// Append a redacted bounded trace entry.
    #[arg(long)]
    pub trace: bool,

    /// Private exact recovery capability file.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,

    /// Output contract. Provider is the hook-ingress default.
    #[arg(long, value_enum, default_value_t = DispatchFormat::Provider)]
    pub format: DispatchFormat,
}

#[derive(Debug, Args)]
pub struct FormatArgs {
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Limit provider diagnostics.
    #[arg(long, value_enum, conflicts_with = "all")]
    pub product: Option<Product>,

    /// Inspect every provider, including truthful unsupported surfaces.
    #[arg(long)]
    pub all: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Provider whose exact owned ingress is managed.
    #[arg(long, value_enum)]
    pub product: Product,

    /// Preview the exact plan without changing files.
    #[arg(
        long,
        required_unless_present_any = ["apply", "repair", "remove"],
        conflicts_with_all = ["apply", "repair"]
    )]
    pub dry_run: bool,

    /// Apply an additive or reviewed migration plan.
    #[arg(long, conflicts_with_all = ["dry_run", "repair", "remove"])]
    pub apply: bool,

    /// Restore missing exact owned ingress.
    #[arg(long, conflicts_with_all = ["dry_run", "apply", "remove"])]
    pub repair: bool,

    /// Remove only exact agent-hook-owned ingress.
    #[arg(long, conflicts_with_all = ["apply", "repair"])]
    pub remove: bool,

    /// Digest from the reviewed preview; required for drift, compatibility, or dual state.
    #[arg(long, value_name = "SHA256")]
    pub expected_plan_digest: Option<String>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct RecoveryArgs {
    #[command(subcommand)]
    pub command: RecoveryCommand,
}

#[derive(Debug, Subcommand)]
pub enum RecoveryCommand {
    /// Create a private exact recovery challenge.
    Challenge(RecoveryChallengeArgs),
    /// Authorize a reviewed challenge for the current OS principal.
    Authorize(RecoveryAuthorizeArgs),
    /// Validate and atomically consume one exact capability.
    Consume(RecoveryConsumeArgs),
    /// Inspect redacted recovery state.
    Status(RecoveryStatusArgs),
    /// Revoke a capability or repair window by public ID.
    Revoke(RecoveryRevokeArgs),
}

#[derive(Debug, Args)]
pub struct RecoveryChallengeArgs {
    #[arg(long, value_enum)]
    pub product: Product,
    #[arg(long)]
    pub event: String,
    #[arg(long)]
    pub target_digest: String,
    #[arg(long)]
    pub command_digest: String,
    #[arg(long)]
    pub snapshot_digest: String,
    #[arg(long, value_enum, default_value_t = RecoveryScope::OneShot)]
    pub scope: RecoveryScope,
    #[arg(long = "rule", required = true)]
    pub rules: Vec<String>,
    #[arg(long, default_value_t = 300)]
    pub ttl_seconds: u64,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub out: PathBuf,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct RecoveryAuthorizeArgs {
    /// Private challenge file produced by `recovery challenge`.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub challenge_file: PathBuf,
    #[arg(long, value_name = "SHA256")]
    pub expected_challenge_digest: String,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub out: PathBuf,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct RecoveryConsumeArgs {
    /// Private authorized capability file to validate and consume.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: PathBuf,
    #[arg(long, value_enum)]
    pub product: Product,
    #[arg(long)]
    pub event: String,
    #[arg(long)]
    pub target_digest: String,
    #[arg(long)]
    pub command_digest: String,
    #[arg(long)]
    pub snapshot_digest: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct RecoveryStatusArgs {
    /// Inspect one public capability identifier instead of the full redacted list.
    #[arg(long)]
    pub capability_id: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct RecoveryRevokeArgs {
    /// Public capability identifier to revoke.
    #[arg(long)]
    pub capability_id: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    pub shell: CompletionShell,
}

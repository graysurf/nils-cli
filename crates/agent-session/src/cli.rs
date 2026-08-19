use std::path::PathBuf;

use crate::SessionTitleState;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};
use nils_common::cli_contract::OutputFormat;
use serde::{Deserialize, Serialize};

/// Default delay before pasting the initial prompt (ms). Shared by `start`'s
/// `--paste-delay-ms` default and the serve create endpoint.
pub const DEFAULT_PASTE_DELAY_MS: u64 = 1200;
/// Default number of pane lines captured by `glance` (CLI and serve).
pub const DEFAULT_GLANCE_TAIL: usize = 40;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum CoordinationMode {
    /// Warn about overlapping managed sessions without blocking work.
    #[default]
    Advisory,
    /// Require authenticated claims and mutation admission leases.
    Enforce,
    /// Disable agent-session collision warnings and admission.
    Off,
}

impl CoordinationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::Enforce => "enforce",
            Self::Off => "off",
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "agent-session",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Start and manage tmux-backed Codex, Claude Code, and Hermes sessions.",
    long_about = "Start and manage tmux-backed Codex, Claude Code, and Hermes sessions for mobile handoff workflows.",
    disable_help_subcommand = true,
    after_help = "EXAMPLES:\n  agent-session start --agent codex --cwd ~/Project/app --prompt-file prompt.md\n  agent-session start --agent hermes --cwd ~\n  agent-session list\n  agent-session glance <id> --tail 40\n  agent-session send <id> --text yes --key enter\n  agent-session send <id> --key c-c\n  agent-session resume <id>\n  agent-session command <id>\n  agent-session attach <id>\n  agent-session delete <id>\n\nENVIRONMENT:\n  AGENT_SESSION_HOST       Hostname used in generated ssh attach commands.\n  AGENT_SESSION_STATE_DIR  Default state directory override.\n  AGENT_SESSION_TMUX_BIN   tmux binary override.\n  AGENT_SESSION_CODEX_BIN  codex binary override.\n  AGENT_SESSION_CLAUDE_BIN claude binary override.\n  AGENT_SESSION_HERMES_BIN hermes binary override.\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error"
)]
pub struct Cli {
    /// State directory. Defaults to AGENT_SESSION_STATE_DIR, XDG_STATE_HOME/agent-session, or ~/.local/state/agent-session.
    #[arg(long = "state-dir", global = true, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub state_dir: Option<PathBuf>,

    /// Hostname used in generated ssh attach commands.
    #[arg(long, global = true, value_name = "HOST")]
    pub host: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start an interactive tmux-backed agent session.
    Start(Box<StartArgs>),
    /// Run a one-shot agent task in a tmux session and write output to a log file.
    Run(RunArgs),
    /// List recorded agent sessions.
    List(ListArgs),
    /// Print the attach command for a session.
    #[command(name = "command")]
    Show(SessionRefArgs),
    /// Attach to a tmux session from the current terminal.
    Attach(AttachArgs),
    /// Print captured tmux pane output or a one-shot run log.
    Logs(LogsArgs),
    /// Print the redacted control-plane diagnostic bundle.
    Diagnose(DiagnoseArgs),
    /// Send input (literal text and/or special keys) to a live session.
    Send(SendArgs),
    /// Capture the recent pane tail plus live status as a dashboard glance.
    Glance(GlanceArgs),
    /// Recreate a missing tmux runtime from exact provider resume metadata.
    Resume(ResumeArgs),
    /// Inspect or ingest metadata-only agent turn lifecycle events.
    Activity(ActivityArgs),
    /// Manage authenticated structured work context and mutation leases.
    #[command(name = "work-context")]
    WorkContext(WorkContextArgs),
    /// Inspect or recover the per-session coordination broker.
    Broker(BrokerArgs),
    /// Exchange bounded private coordination mailbox messages.
    Message(MessageArgs),
    /// Serve the control plane (HTTP) and PTY attach (WebSocket) over loopback.
    Serve(ServeArgs),
    /// Internal metadata-only bridge for a managed Codex remote TUI.
    #[command(name = "codex-app-server-proxy", hide = true)]
    CodexAppServerProxy(CodexAppServerProxyArgs),
    /// Internal exact-child supervisor for an explicitly armed Main Agent canary.
    #[command(name = "provider-stop-canary-supervisor", hide = true)]
    ProviderStopCanarySupervisor(ProviderStopCanarySupervisorArgs),
    /// Internal crash guardian for the exact provider-owned process session.
    #[command(name = "provider-stop-canary-guardian", hide = true)]
    ProviderStopCanaryGuardian(ProviderStopCanarySupervisorArgs),
    /// Delete session state and kill the tmux session if it is still alive.
    Delete(DeleteArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
pub struct CodexAppServerProxyArgs {
    /// Managed session id whose runtime owns this bridge.
    #[arg(long)]
    pub id: String,

    /// Private app-server Unix socket.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub upstream: PathBuf,

    /// Private Unix socket exposed only to the visible TUI.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub listen: PathBuf,
}

#[derive(Debug, Args)]
pub struct ProviderStopCanarySupervisorArgs {
    /// Managed session id whose exact Codex child this supervisor owns.
    #[arg(long)]
    pub id: String,

    /// Internal pinned cgroup device passed only from supervisor to guardian.
    #[arg(long, hide = true)]
    pub cgroup_device: Option<u64>,

    /// Internal pinned cgroup inode passed only from supervisor to guardian.
    #[arg(long, hide = true)]
    pub cgroup_inode: Option<u64>,
}

#[derive(Debug, Args)]
pub struct StartArgs {
    /// Internal: the serve daemon owns the Codex app-server control connection.
    #[arg(skip)]
    pub app_server_managed: bool,

    /// Internal: arm the exact-child provider stop canary wrapper.
    #[arg(skip)]
    pub provider_stop_canary: bool,

    /// Internal: assignment selector for the armed canary reservation sidecar.
    #[arg(skip)]
    pub provider_stop_canary_assignment_id: Option<String>,

    /// Internal: account nickname selected by the serve daemon.
    #[arg(skip)]
    pub initial_codex_account: Option<String>,

    /// Internal: structured title provenance supplied by the serve daemon.
    #[arg(skip)]
    pub initial_title_state: Option<SessionTitleState>,

    /// Internal: server-owned launch profile id selected by the serve daemon.
    #[arg(skip)]
    pub initial_agent_profile: Option<String>,

    /// Internal: provider config root owned by the selected launch profile.
    #[arg(skip)]
    pub initial_provider_config_dir: Option<PathBuf>,

    /// Internal: whether the selected profile has authoritative auto-resume usage.
    #[arg(skip)]
    pub initial_profile_auto_resume_supported: Option<bool>,

    /// Internal: authoritative Codex usage account nickname for a `claude`
    /// launch profile that actually runs on a Codex/GPT backend. When set, the
    /// auto-resume loop keys off this Codex account's rate limits instead of
    /// native Claude usage.
    #[arg(skip)]
    pub initial_codex_usage_account: Option<String>,

    /// Agent to run.
    #[arg(long, value_enum)]
    pub agent: AgentKind,

    /// Working directory for the agent session. Defaults to the current directory.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub cwd: Option<PathBuf>,

    /// Human-readable session title.
    #[arg(long, value_name = "TITLE")]
    pub title: Option<String>,

    /// Short explicit session id. Usually auto-generated.
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,

    /// Prompt text. Prefer --prompt-file or --prompt-stdin for long prompts.
    #[arg(long, value_name = "TEXT")]
    pub prompt: Option<String>,

    /// Prompt file path, or '-' to read stdin.
    #[arg(long = "prompt-file", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub prompt_file: Option<PathBuf>,

    /// Read prompt from stdin.
    #[arg(long)]
    pub prompt_stdin: bool,

    /// tmux binary override.
    #[arg(long = "tmux-bin", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub tmux_bin: Option<PathBuf>,

    /// Agent binary override.
    #[arg(long = "agent-bin", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub agent_bin: Option<PathBuf>,

    /// Extra argument passed to the underlying agent command.
    #[arg(long = "agent-arg", value_name = "ARG")]
    pub agent_args: Vec<String>,

    /// Session collision coordination mode.
    #[arg(long, value_enum, default_value_t = CoordinationMode::Advisory)]
    pub coordination_mode: CoordinationMode,

    /// Delay before pasting the initial prompt into the tmux pane.
    #[arg(long = "paste-delay-ms", default_value_t = DEFAULT_PASTE_DELAY_MS)]
    pub paste_delay_ms: u64,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Agent to run.
    #[arg(long, value_enum)]
    pub agent: AgentKind,

    /// Working directory for the agent session. Defaults to the current directory.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub cwd: Option<PathBuf>,

    /// Human-readable session title.
    #[arg(long, value_name = "TITLE")]
    pub title: Option<String>,

    /// Short explicit session id. Usually auto-generated.
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,

    /// Prompt text. Prefer --prompt-file or --prompt-stdin for long prompts.
    #[arg(long, value_name = "TEXT")]
    pub prompt: Option<String>,

    /// Prompt file path, or '-' to read stdin.
    #[arg(long = "prompt-file", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub prompt_file: Option<PathBuf>,

    /// Read prompt from stdin.
    #[arg(long)]
    pub prompt_stdin: bool,

    /// tmux binary override.
    #[arg(long = "tmux-bin", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub tmux_bin: Option<PathBuf>,

    /// Agent binary override.
    #[arg(long = "agent-bin", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub agent_bin: Option<PathBuf>,

    /// Extra argument passed to the underlying agent command.
    #[arg(long = "agent-arg", value_name = "ARG")]
    pub agent_args: Vec<String>,

    /// Session collision coordination mode.
    #[arg(long, value_enum, default_value_t = CoordinationMode::Advisory)]
    pub coordination_mode: CoordinationMode,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct WorkContextArgs {
    #[command(subcommand)]
    pub command: WorkContextCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkContextCommand {
    /// Show automatic presence, mode, and the optional declared context for this session.
    Status(WorkContextStatusArgs),
    /// Declare or replace this session's optional context using inferred identity and repository data.
    Set(WorkContextSetArgs),
    /// Clear this session's optional declared context.
    Clear(WorkContextClearArgs),
    /// Evaluate privacy-safe managed-session overlap without blocking work.
    Advise(WorkContextAdviseArgs),
    /// Suppress repeated advisory warnings for a bounded period.
    Acknowledge(WorkContextAcknowledgeArgs),
    /// Atomically evaluate conflicts and acquire a structured work claim.
    Claim(WorkContextClaimArgs),
    /// Show the authenticated session's current public work context.
    Show(WorkContextShowArgs),
    /// Evaluate a candidate or the authenticated session without acquiring.
    Check(WorkContextCheckArgs),
    /// Renew an active work claim using revision compare-and-swap.
    Renew(WorkContextRenewArgs),
    /// Release an active work claim using revision compare-and-swap.
    Release(WorkContextReleaseArgs),
    /// Admit one mutation operation whose targets are covered by the claim.
    Admit(WorkContextAdmitArgs),
    /// Complete an admitted mutation operation.
    Complete(WorkContextCompleteArgs),
    /// Reconcile a missed operation completion from bounded proof.
    Reconcile(WorkContextReconcileArgs),
}

#[derive(Debug, Args)]
pub struct WorkContextStatusArgs {
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct WorkContextSetArgs {
    /// Short public description of the current work.
    #[arg(long, value_name = "TEXT")]
    pub summary: Option<String>,
    /// Work intent label.
    #[arg(long, default_value = "implementation")]
    pub intent: String,
    /// Tracking tier (L0, L1, L2, or L3).
    #[arg(long, default_value = "L0")]
    pub tier: String,
    /// Canonical owner/repository. Defaults to the current checkout's origin.
    #[arg(long, value_name = "OWNER/REPO")]
    pub repository: Option<String>,
    /// Repository-relative exact or prefix path to declare. Repeatable.
    #[arg(long = "path", value_name = "PATH")]
    pub paths: Vec<String>,
    /// Issue number in the inferred repository. Repeatable.
    #[arg(long, value_name = "NUMBER")]
    pub issue: Vec<u64>,
    /// Pull/merge request number in the inferred repository. Repeatable.
    #[arg(long, value_name = "NUMBER")]
    pub pr: Vec<u64>,
    /// Public plan reference. Repeatable.
    #[arg(long = "plan-ref", value_name = "REF")]
    pub plan_refs: Vec<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct WorkContextClearArgs {
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct WorkContextAdviseArgs {
    /// Optional operation-targets JSON used to refine this advisory check.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub targets_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct WorkContextAcknowledgeArgs {
    /// Bounded suppression duration such as 30m or 2h (maximum 8h).
    #[arg(long = "for", default_value = "30m", value_name = "DURATION")]
    pub duration: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct WorkContextClaimArgs {
    /// Managed session id that owns the claim.
    #[arg(long)]
    pub session: String,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub file: PathBuf,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long)]
    pub idempotency_key: String,
    #[arg(long)]
    pub if_revision: Option<u64>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct WorkContextShowArgs {
    /// Managed session id whose authenticated context is shown.
    #[arg(long)]
    pub session: String,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct WorkContextCheckArgs {
    /// Existing managed session id selected for the advisory check.
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long = "self")]
    pub self_selector: bool,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub candidate: Option<PathBuf>,
    #[arg(long)]
    pub allow_incomplete: bool,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct WorkContextRenewArgs {
    /// Managed session id that owns the claim.
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub claim: String,
    #[arg(long)]
    pub if_revision: u64,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long)]
    pub idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct WorkContextReleaseArgs {
    /// Managed session id that owns the claim.
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub claim: String,
    #[arg(long)]
    pub if_revision: u64,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long)]
    pub idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct WorkContextAdmitArgs {
    /// Managed session id that owns the operation.
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub claim: String,
    #[arg(long)]
    pub if_revision: u64,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub targets_file: PathBuf,
    #[arg(long)]
    pub operation: String,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub execution_token_file: PathBuf,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long)]
    pub idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum OperationOutcome {
    Pass,
    Fail,
}

impl OperationOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Args)]
pub struct WorkContextCompleteArgs {
    /// Managed session id that owns the operation.
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub lease: String,
    #[arg(long)]
    pub if_revision: u64,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub execution_token_file: PathBuf,
    #[arg(long, value_enum)]
    pub outcome: OperationOutcome,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long)]
    pub idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct WorkContextReconcileArgs {
    /// Managed session id that owns the operation.
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub lease: String,
    #[arg(long)]
    pub if_revision: u64,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub proof_file: PathBuf,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long)]
    pub idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct BrokerArgs {
    #[command(subcommand)]
    pub command: BrokerCommand,
}

#[derive(Debug, Subcommand)]
pub enum BrokerCommand {
    /// Show privacy-safe broker readiness and claim/operation summaries.
    Status(BrokerStatusArgs),
    /// Adopt an unchanged live runtime after validating recovery proof.
    Adopt(BrokerRecoveryArgs),
    /// Reconcile broker and registry state from validated recovery proof.
    Reconcile(BrokerRecoveryArgs),
    #[command(hide = true)]
    Stop(BrokerStopArgs),
    #[command(hide = true)]
    Heartbeat(BrokerHeartbeatArgs),
}

#[derive(Debug, Args)]
pub struct BrokerStatusArgs {
    /// Managed session id whose broker status is shown.
    #[arg(long)]
    pub session: String,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    /// Require the capability to authenticate the exact broker incarnation.
    #[arg(long, hide = true)]
    pub authenticated: bool,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct BrokerRecoveryArgs {
    /// Managed session id whose broker is being recovered.
    #[arg(long)]
    pub session: String,
    /// Exact incarnation-bound controller capability. Defaults to
    /// AGENT_SESSION_CAPABILITY_FILE only inside the authenticated runtime.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub proof_file: PathBuf,
    #[arg(long)]
    pub idempotency_key: String,
    #[arg(long)]
    pub operation: Option<String>,
    #[arg(long)]
    pub if_revision: Option<u64>,
    #[arg(long)]
    pub attest_inactive: bool,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct BrokerStopArgs {
    /// Managed session id whose broker is being stopped.
    #[arg(long)]
    pub session: String,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct BrokerHeartbeatArgs {
    /// Managed session id owned by this heartbeat sidecar.
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub incarnation: String,
    #[arg(long)]
    pub generation: u64,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: PathBuf,
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct MessageArgs {
    #[command(subcommand)]
    pub command: MessageCommand,
}

#[derive(Debug, Subcommand)]
pub enum MessageCommand {
    /// Send one bounded private message and schedule an eventual fixed notification.
    Send(MessageSendArgs),
    /// List bounded private mailbox metadata for the authenticated recipient.
    Inbox(MessageInboxArgs),
    /// Read one private message body as its authenticated recipient.
    Show(MessageShowArgs),
    /// Acknowledge one private message using revision compare-and-swap.
    Ack(MessageAckArgs),
    /// Reply to one private message and schedule an eventual fixed notification.
    Reply(MessageReplyArgs),
    /// Wait a bounded duration for one message revision change.
    Wait(MessageWaitArgs),
}

#[derive(Debug, Args)]
pub struct MessageSendArgs {
    /// Authenticated managed session id sending the message.
    #[arg(long = "from")]
    pub from_session: String,
    #[arg(long = "to")]
    pub to_session: String,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub body_file: PathBuf,
    /// Private capability file; defaults to AGENT_SESSION_CAPABILITY_FILE.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long)]
    pub idempotency_key: String,
    #[arg(long)]
    pub reply_to: Option<String>,
    #[arg(long, value_name = "DURATION")]
    pub expires_in: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct MessageInboxArgs {
    /// Authenticated recipient session id.
    #[arg(long)]
    pub session: String,
    /// Private capability file; defaults to AGENT_SESSION_CAPABILITY_FILE.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long)]
    pub state: Option<String>,
    #[arg(long)]
    pub cursor: Option<String>,
    #[arg(long)]
    pub limit: Option<usize>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct MessageShowArgs {
    /// Authenticated recipient session id.
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub message: String,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct MessageAckArgs {
    /// Authenticated recipient session id.
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub message: String,
    #[arg(long)]
    pub if_revision: u64,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long)]
    pub idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct MessageReplyArgs {
    /// Authenticated recipient session id sending the reply.
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub message: String,
    #[arg(long)]
    pub if_revision: u64,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub body_file: PathBuf,
    /// Private capability file; defaults to AGENT_SESSION_CAPABILITY_FILE.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long)]
    pub idempotency_key: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct MessageWaitArgs {
    /// Authenticated recipient session id.
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub message: String,
    #[arg(long)]
    pub if_revision: u64,
    #[arg(long, value_name = "DURATION")]
    pub timeout: String,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub capability_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct SessionRefArgs {
    /// Session id.
    #[arg(value_name = "ID")]
    pub id: String,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct AttachArgs {
    /// Session id.
    #[arg(value_name = "ID")]
    pub id: String,

    /// tmux binary override.
    #[arg(long = "tmux-bin", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub tmux_bin: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct LogsArgs {
    /// Session id.
    #[arg(value_name = "ID")]
    pub id: String,

    /// Number of lines to capture from the tmux pane.
    #[arg(long, default_value_t = 120)]
    pub tail: usize,

    /// tmux binary override.
    #[arg(long = "tmux-bin", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub tmux_bin: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

/// Read-only control-plane diagnostics.
///
/// This reads only the local observation spool and coordination projection, so it
/// stays usable when `agent-session serve` is down — which is one of the states
/// it exists to diagnose.
#[derive(Debug, Args)]
pub struct DiagnoseArgs {
    /// Number of recent observation events to include (default 20, maximum 200).
    #[arg(long, value_name = "COUNT")]
    pub limit: Option<usize>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct SendArgs {
    /// Session id.
    #[arg(value_name = "ID")]
    pub id: String,

    /// Literal text to type into the session. Applied before any --key. Prefer
    /// --text-stdin for secrets (--text is visible in this process's arguments).
    #[arg(long, value_name = "TEXT")]
    pub text: Option<String>,

    /// Read the literal text to type from stdin (secret-safe; never echoed).
    #[arg(long = "text-stdin")]
    pub text_stdin: bool,

    /// Special key to press (repeatable), applied in order after any text.
    #[arg(long = "key", value_enum, value_name = "KEY")]
    pub keys: Vec<SpecialKey>,

    /// tmux binary override.
    #[arg(long = "tmux-bin", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub tmux_bin: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct GlanceArgs {
    /// Session id.
    #[arg(value_name = "ID")]
    pub id: String,

    /// Number of pane lines to capture for the glance tail.
    #[arg(long, default_value_t = DEFAULT_GLANCE_TAIL)]
    pub tail: usize,

    /// tmux binary override.
    #[arg(long = "tmux-bin", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub tmux_bin: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct ResumeArgs {
    /// Session id.
    #[arg(value_name = "ID")]
    pub id: String,

    /// tmux binary override.
    #[arg(long = "tmux-bin", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub tmux_bin: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct ActivityArgs {
    #[command(subcommand)]
    pub command: ActivityCommand,
}

#[derive(Debug, Subcommand)]
pub enum ActivityCommand {
    /// Ingest one normalized metadata-only lifecycle event from stdin.
    Event(ActivityEventArgs),
    /// Inspect the durable turn-state snapshot for one session.
    Status(ActivityStatusArgs),
    /// Translate one provider hook payload into a safe normalized event.
    #[command(hide = true)]
    Hook(ActivityHookArgs),
    /// Translate one provider notification payload into a safe normalized event.
    #[command(hide = true)]
    Notify(ActivityNotifyArgs),
    /// Report provider support, version, configuration, and repair guidance.
    Doctor(ActivityDoctorArgs),
    /// Preview or apply additive provider lifecycle configuration.
    Setup(ActivitySetupArgs),
}

#[derive(Debug, Args)]
pub struct ActivityEventArgs {
    /// Session id.
    #[arg(value_name = "ID")]
    pub id: String,

    /// Read the JSON event from stdin.
    #[arg(long, required = true)]
    pub stdin: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct ActivityStatusArgs {
    /// Session id.
    #[arg(value_name = "ID")]
    pub id: String,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct ActivityHookArgs {
    /// Provider whose hook payload is on stdin.
    #[arg(long, value_enum)]
    pub agent: AgentKind,

    /// Provider event name when the raw payload does not carry one.
    #[arg(long, hide = true)]
    pub event: Option<String>,
}

#[derive(Debug, Args)]
pub struct ActivityNotifyArgs {
    /// Provider whose notification payload is supplied as the final argument.
    #[arg(long, value_enum)]
    pub agent: AgentKind,

    /// JSON argv for an existing singular notifier composed by activity setup.
    #[arg(long, hide = true, value_name = "JSON")]
    pub forward_notify_argv_json: Option<String>,

    /// Provider-authored JSON passed by Codex in argv; content is discarded
    /// after parsing but is transiently visible to same-host process inspection.
    #[arg(value_name = "PAYLOAD")]
    pub payload: String,
}

#[derive(Debug, Args)]
pub struct ActivityDoctorArgs {
    /// Limit diagnostics to one provider.
    #[arg(long, value_enum)]
    pub agent: Option<AgentKind>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct ActivitySetupArgs {
    /// Provider to configure.
    #[arg(long, value_enum)]
    pub agent: AgentKind,

    /// Preview a Codex-only repair plan when combined with --repair; otherwise
    /// preview the exact additive change without writing it.
    #[arg(
        long,
        required_unless_present_any = ["apply", "remove", "repair"],
        conflicts_with_all = ["apply", "remove"]
    )]
    pub dry_run: bool,

    /// Apply the additive provider integration.
    #[arg(long, conflicts_with_all = ["dry_run", "remove", "repair"])]
    pub apply: bool,

    /// Remove only agent-session-owned provider lifecycle entries, including
    /// the exact Codex notify argv.
    #[arg(long, conflicts_with_all = ["dry_run", "apply", "repair"])]
    pub remove: bool,

    /// Restore missing agent-session-owned entries without replacing others.
    #[arg(long, conflicts_with_all = ["apply", "remove"])]
    pub repair: bool,

    /// Digest returned by the reviewed Codex repair preview. Required when
    /// applying Codex repair and rejected if either planned file changed.
    #[arg(
        long,
        value_name = "SHA256",
        requires = "repair",
        conflicts_with = "dry_run"
    )]
    pub expected_preview_digest: Option<String>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Versioned work-context (`work-context/v1`) and mailbox (`messages/v1`)
    /// routes share the same loopback control plane and library contracts.
    /// Address to bind. Defaults to loopback; a non-loopback address is refused
    /// unless --allow-non-loopback is passed (it exposes a remote shell).
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:8781")]
    pub bind: String,

    /// Bearer token required on activity streaming, write, and attach endpoints.
    /// Falls back to AGENT_SESSION_TOKEN. When unset, activity streaming,
    /// writes, and attach are disabled (session reads still work on loopback).
    #[arg(long, value_name = "TOKEN")]
    pub token: Option<String>,

    /// Read the bearer token once from stdin instead of process arguments.
    #[arg(long = "token-stdin", conflicts_with = "token")]
    pub token_stdin: bool,

    /// Machine identity reported in responses. Falls back to
    /// AGENT_SESSION_MACHINE, then --host, then the short hostname.
    #[arg(long, value_name = "NAME")]
    pub machine: Option<String>,

    /// Deliberately allow binding a non-loopback address. Without this, a
    /// non-loopback --bind is refused because it exposes a remote shell.
    #[arg(long = "allow-non-loopback")]
    pub allow_non_loopback: bool,

    /// tmux binary override.
    #[arg(long = "tmux-bin", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub tmux_bin: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    /// Session id.
    #[arg(value_name = "ID")]
    pub id: String,

    /// tmux binary override.
    #[arg(long = "tmux-bin", value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub tmux_bin: Option<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    /// Shell to generate completion script for.
    #[arg(value_enum)]
    pub shell: crate::completion::CompletionShell,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum AgentKind {
    Codex,
    Claude,
    Hermes,
}

impl AgentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Hermes => "hermes",
        }
    }

    /// Parse an agent name (as accepted by `--agent` / emitted by `as_str`).
    /// Used by the serve create endpoint to map a JSON `agent` field.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            "hermes" => Some(Self::Hermes),
            _ => None,
        }
    }
}

/// Named special keys accepted by `send`, mapped to tmux `send-keys` names.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum SpecialKey {
    Enter,
    Escape,
    Backspace,
    #[value(name = "c-c")]
    CtrlC,
    Up,
    Down,
    Left,
    Right,
    Tab,
}

impl SpecialKey {
    /// Canonical CLI name, used in the JSON contract (never echoes user input).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Escape => "escape",
            Self::Backspace => "backspace",
            Self::CtrlC => "c-c",
            Self::Up => "up",
            Self::Down => "down",
            Self::Left => "left",
            Self::Right => "right",
            Self::Tab => "tab",
        }
    }

    /// Parse a canonical key name (as accepted by `--key` / emitted by `as_str`).
    /// Used by the serve WebSocket protocol to map client key names to keys.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "enter" => Some(Self::Enter),
            "escape" => Some(Self::Escape),
            "backspace" => Some(Self::Backspace),
            "c-c" => Some(Self::CtrlC),
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "tab" => Some(Self::Tab),
            _ => None,
        }
    }

    /// tmux `send-keys` key name.
    pub fn tmux_key(self) -> &'static str {
        match self {
            Self::Enter => "Enter",
            Self::Escape => "Escape",
            Self::Backspace => "BSpace",
            Self::CtrlC => "C-c",
            Self::Up => "Up",
            Self::Down => "Down",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Tab => "Tab",
        }
    }
}

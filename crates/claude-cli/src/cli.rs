use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};

const ROOT_AFTER_HELP: &str = "\
EXAMPLES:
  claude-cli agent prompt 'Summarize this repository'
  claude-cli agent advice 'How should I split this change?'
  claude-cli agent commit 'Prefer the smallest accurate scope'
  claude-cli agent doctor --format json
  claude-cli auth status --format json
  claude-cli config show
  claude-cli prompt-segment
  claude-cli prompt-segment --refresh
  claude-cli prompt-segment status --format json
  claude-cli usage --format json --source auto
  claude-cli agent resume <session-id>
  claude-cli completion zsh

ENVIRONMENT:
  CLAUDE_CLI_BIN, CLAUDE_CLI_MODEL, CLAUDE_CLI_EFFORT
  CLAUDE_CLI_AGENT_RUNTIME, CLAUDE_CLI_NO_SESSION_PERSISTENCE
  CLAUDE_PROMPT_TTL, CLAUDE_PROMPT_STALE_SUFFIX
  CLAUDE_PROMPT_SEGMENT_TTL, CLAUDE_PROMPT_SEGMENT_STALE_SUFFIX
  CLAUDE_PROMPT_SEGMENT_CACHE_DIR, CLAUDE_PROMPT_SEGMENT_ENDPOINT
  CLAUDE_PROMPT_SEGMENT_REFRESH_MIN_SECONDS, CLAUDE_PROMPT_SEGMENT_LOCK_STALE_SECONDS
  CLAUDE_PROMPT_SEGMENT_EXE, CLAUDE_PROMPT_SEGMENT_ZSH_ESCAPE_ENABLED
  CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN, CLAUDE_PROMPT_SEGMENT_CREDENTIALS_JSON
  CLAUDE_PROMPT_SEGMENT_KEYCHAIN_DISABLED, CLAUDE_PROMPT_SEGMENT_KEYCHAIN_SERVICE
  NO_COLOR

EXIT CODES:
  0   success
  1   runtime false/failed state
  64  command-line usage error
  65  invalid input data or unresolved session id
  69  required Claude capability unavailable";

#[derive(Parser)]
#[command(
    name = "claude-cli",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Claude CLI for nils-cli workspace",
    long_about = "Run Claude-oriented helpers that should live in native CLI code instead of zsh glue.",
    after_help = ROOT_AFTER_HELP
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Agent command group
    Agent(AgentArgs),
    /// Authentication command group
    Auth(AuthArgs),
    /// Configuration command group
    Config(ConfigArgs),
    /// Prompt-segment command group
    PromptSegment(PromptSegmentArgs),
    /// Read Claude usage from OAuth, Claude CLI, or cache
    Usage(UsageArgs),
    /// Export shell completion script
    Completion(CompletionArgs),
}

#[derive(Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: Option<AgentCommand>,
}

#[derive(Subcommand)]
pub enum AgentCommand {
    /// Run a one-shot prompt through the safe Claude runtime
    Prompt(AgentOneShotArgs),
    /// Get actionable engineering advice
    Advice(AgentOneShotArgs),
    /// Get an explanation for a concept
    Knowledge(AgentOneShotArgs),
    /// Generate and create a semantic commit from staged changes
    Commit(AgentCommitArgs),
    /// Check whether the safe agent and commit runtime is ready
    Doctor {
        #[command(flatten)]
        output: OutputModeArgs,
    },
    /// Resume a Claude session in its recorded working directory
    Resume {
        /// Claude session id to resume
        #[arg(value_name = "session_id")]
        session_id: String,
        /// Override the recorded working directory (for a moved repository)
        #[arg(long = "cd", value_name = "dir", value_hint = ValueHint::DirPath)]
        cd: Option<PathBuf>,
    },
}

#[derive(Args)]
pub struct AgentCommitArgs {
    /// Push after the local commit succeeds
    #[arg(short = 'p', long = "push")]
    pub push: bool,
    /// Stage all tracked and untracked changes before generating the message
    #[arg(short = 'a', long = "auto-stage")]
    pub auto_stage: bool,
    /// Claude model override
    #[arg(long = "model", value_name = "model")]
    pub model: Option<String>,
    /// Claude effort override
    #[arg(long = "effort", value_enum, value_name = "level")]
    pub effort: Option<AgentEffort>,
    /// Additional commit-message guidance
    #[arg(value_name = "extra", num_args = 0.., allow_hyphen_values = true)]
    pub extra: Vec<String>,
}

#[derive(Args)]
pub struct AgentOneShotArgs {
    /// Runtime profile (default: safe)
    #[arg(long = "runtime", value_enum, value_name = "mode")]
    pub runtime: Option<AgentRuntimeMode>,
    /// Claude model override
    #[arg(long = "model", value_name = "model")]
    pub model: Option<String>,
    /// Claude effort override
    #[arg(long = "effort", value_enum, value_name = "level")]
    pub effort: Option<AgentEffort>,
    /// Disable session persistence (always enabled by the safe runtime)
    #[arg(long = "ephemeral")]
    pub ephemeral: bool,
    /// Prompt text; reads stdin when omitted
    #[arg(value_name = "input", num_args = 0.., allow_hyphen_values = true)]
    pub input: Vec<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum AgentRuntimeMode {
    Safe,
    Inherited,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum AgentEffort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl AgentEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[derive(Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: Option<AuthCommand>,
}

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Sign in through the upstream Claude Code authentication flow
    Login {
        /// Use Anthropic Console API billing
        #[arg(long, conflicts_with = "claudeai")]
        console: bool,
        /// Use a Claude subscription
        #[arg(long, conflicts_with = "console")]
        claudeai: bool,
        /// Pre-populate the login email address
        #[arg(long, value_name = "email")]
        email: Option<String>,
        /// Force the SSO login flow
        #[arg(long)]
        sso: bool,
    },
    /// Show redacted authentication status
    Status {
        #[command(flatten)]
        output: OutputModeArgs,
    },
    /// Sign out through the upstream Claude Code authentication flow
    Logout,
}

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: Option<ConfigCommand>,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Show effective wrapper configuration
    Show,
    /// Emit a validated export for the current shell
    Set { key: String, value: String },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    #[value(name = "text")]
    Text,
    #[value(name = "json")]
    Json,
}

#[derive(Args, Clone, Debug, Default)]
pub struct OutputModeArgs {
    /// Output format (`text` or `json`)
    #[arg(long = "format", value_enum, value_name = "format")]
    pub format: Option<OutputFormat>,
    /// Hidden alias for `--format json`.
    #[arg(long = "json", hide = true, conflicts_with = "format")]
    pub json: bool,
}

impl OutputModeArgs {
    pub fn is_json(&self) -> bool {
        self.json || matches!(self.format, Some(OutputFormat::Json))
    }
}

#[derive(Args)]
pub struct PromptSegmentArgs {
    #[command(subcommand)]
    pub command: Option<PromptSegmentCommand>,
    /// Cache TTL
    #[arg(long = "ttl")]
    pub ttl: Option<String>,
    /// Hide the 5h window output
    #[arg(long = "no-5h")]
    pub no_5h: bool,
    /// Reset time format (local time)
    #[arg(long = "time-format")]
    pub time_format: Option<String>,
    /// Show timezone offset in the default reset time display
    #[arg(long = "show-timezone")]
    pub show_timezone: bool,
    /// Force a fetch attempt regardless of TTL
    #[arg(long = "refresh")]
    pub refresh: bool,
    /// Exit 0 if Keychain or credential override has a Claude OAuth access token
    #[arg(long = "is-enabled", hide = true)]
    pub is_enabled: bool,
}

#[derive(Subcommand)]
pub enum PromptSegmentCommand {
    /// Exit 0 when Starship should run the prompt segment
    Check,
    /// Report prompt-segment readiness without exposing secrets
    Status {
        #[command(flatten)]
        output: OutputModeArgs,
    },
}

#[derive(Args)]
pub struct UsageArgs {
    #[command(flatten)]
    pub output: OutputModeArgs,
    /// Usage source to read
    #[arg(long = "source", value_enum, default_value_t = UsageSource::Auto)]
    pub source: UsageSource,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum UsageSource {
    Auto,
    Oauth,
    Cli,
    Cache,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
}

#[derive(Args)]
pub struct CompletionArgs {
    /// Shell to generate completion script for
    #[arg(value_enum, value_name = "shell")]
    pub shell: CompletionShell,
}

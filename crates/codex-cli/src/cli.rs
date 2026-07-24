use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};

const ROOT_AFTER_HELP: &str = "\
EXAMPLES:
  codex-cli agent prompt 'Summarize this diff'
  codex-cli auth status --format json
  codex-cli prompt-segment status
  codex-cli completion zsh

ENVIRONMENT:
  CODEX_CLI_MODEL, CODEX_CLI_REASONING, CODEX_CLI_AGENT_RUNTIME, CODEX_ALLOW_DANGEROUS_ENABLED
  CODEX_SECRET_DIR, CODEX_AUTH_FILE, CODEX_SECRET_CACHE_DIR
  CODEX_AUTO_REFRESH_ENABLED, CODEX_AUTO_REFRESH_MIN_DAYS
  CODEX_AUTH_REMOTE_SSH, CODEX_AUTH_REMOTE_NAME, CODEX_AUTH_REMOTE_REFRESH
  CODEX_CHATGPT_BASE_URL, CODEX_OAUTH_CLIENT_ID
  CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED, CODEX_RATE_LIMITS_WATCH_MAX_ROUNDS, CODEX_RATE_LIMITS_WATCH_INTERVAL_SECONDS
  CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS, CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS, CODEX_RATE_LIMITS_CACHE_TTL
  CODEX_PROMPT_SEGMENT_ENABLED, CODEX_PROMPT_SEGMENT_TTL, CODEX_PROMPT_SEGMENT_STALE_SUFFIX, CODEX_PROMPT_SEGMENT_NAME_SOURCE
  CODEX_PROMPT_SEGMENT_REFRESH_MIN_SECONDS, CODEX_PROMPT_SEGMENT_LOCK_STALE_SECONDS
  CODEX_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS, CODEX_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS, CODEX_PROMPT_SEGMENT_EXE
  CODEX_PROMPT_SEGMENT_SHOW_5H_ENABLED, CODEX_PROMPT_SEGMENT_SHOW_FALLBACK_NAME_ENABLED, CODEX_PROMPT_SEGMENT_SHOW_FULL_EMAIL_ENABLED
  CODEX_PROMPT_SEGMENT_COLOR_ENABLED, CODEX_PROMPT_SEGMENT_ZSH_ESCAPE_ENABLED
  ZSH_CACHE_DIR, ZSH_DEBUG, NO_COLOR, STARSHIP_SESSION_KEY, STARSHIP_SHELL

EXIT CODES:
  0   success
  1   runtime error
  64  command-line usage error
  65  invalid input data";

#[derive(Parser)]
#[command(
    name = "codex-cli",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Codex CLI for nils-cli workspace",
    long_about = "Run Codex-oriented agent helpers, authentication helpers, diagnostics, configuration, and prompt-segment utilities.",
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
    /// Diagnostics command group
    Diag(DiagArgs),
    /// Configuration command group
    Config(ConfigArgs),
    /// Prompt-segment command group
    PromptSegment(PromptSegmentArgs),
    /// Export shell completion script
    Completion(CompletionArgs),
}

#[derive(Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: Option<AgentCommand>,
}

/// `agent run` MCP policy selector for the Codex supervisor.
///
/// `inherited` is reachable only as an explicit flag on the current
/// invocation; there is deliberately no environment or configuration default.
pub type CapsuleMcpMode = codex_cli::agent::capsule::McpMode;

#[derive(Subcommand)]
pub enum AgentCommand {
    /// Run a raw prompt
    Prompt {
        /// Child runtime (default: isolated; inherited loads the full Codex home)
        #[arg(long = "runtime", value_enum, value_name = "mode")]
        runtime: Option<codex_cli::runtime::AgentRuntimeMode>,
        /// Run without persisting Codex session files to disk
        #[arg(long = "ephemeral")]
        ephemeral: bool,
        #[arg(value_name = "prompt", num_args = 0..)]
        prompt: Vec<String>,
    },
    /// Get actionable engineering advice
    Advice {
        /// Child runtime (default: isolated; inherited loads the full Codex home)
        #[arg(long = "runtime", value_enum, value_name = "mode")]
        runtime: Option<codex_cli::runtime::AgentRuntimeMode>,
        /// Run without persisting Codex session files to disk
        #[arg(long = "ephemeral")]
        ephemeral: bool,
        #[arg(value_name = "question", num_args = 0..)]
        question: Vec<String>,
    },
    /// Get an explanation for a concept
    Knowledge {
        /// Child runtime (default: isolated; inherited loads the full Codex home)
        #[arg(long = "runtime", value_enum, value_name = "mode")]
        runtime: Option<codex_cli::runtime::AgentRuntimeMode>,
        /// Run without persisting Codex session files to disk
        #[arg(long = "ephemeral")]
        ephemeral: bool,
        #[arg(value_name = "concept", num_args = 0..)]
        concept: Vec<String>,
    },
    /// Run the semantic-commit workflow
    Commit {
        /// Child runtime (default: isolated; inherited loads the full Codex home)
        #[arg(long = "runtime", value_enum, value_name = "mode")]
        runtime: Option<codex_cli::runtime::AgentRuntimeMode>,
        /// Push after committing
        #[arg(short = 'p', long = "push")]
        push: bool,
        /// Autostage changes before committing
        #[arg(short = 'a', long = "auto-stage")]
        auto_stage: bool,
        /// Run without persisting Codex session files to disk
        #[arg(long = "ephemeral")]
        ephemeral: bool,
        /// Extra prompt text
        #[arg(value_name = "extra", num_args = 0..)]
        extra: Vec<String>,
    },
    /// Resume a Codex session in its recorded working directory
    Resume {
        /// Codex session id to resume
        #[arg(value_name = "session_id")]
        session_id: String,
        /// Override the recorded working directory (for a moved repository)
        #[arg(long = "cd", value_name = "dir", value_hint = ValueHint::DirPath)]
        cd: Option<PathBuf>,
    },
    /// Run an operator-prepared execution capsule under Codex supervision
    Run {
        /// Private capsule directory containing manifest.json and run.sh
        #[arg(long = "capsule", value_name = "dir", value_hint = ValueHint::DirPath)]
        capsule: PathBuf,
        /// Acknowledge unsandboxed host access and supervisor-trusted evidence
        #[arg(long = "allow-host-access")]
        allow_host_access: bool,
        /// MCP policy for the Codex supervisor
        #[arg(
            long = "mcp-mode",
            value_enum,
            value_name = "mode",
            default_value = "disabled"
        )]
        mcp_mode: CapsuleMcpMode,
        #[command(flatten)]
        output: OutputModeArgs,
    },
    /// Internal sandbox-bound capsule command executor
    #[command(hide = true)]
    CapsuleExec {
        #[arg(long = "capsule", value_name = "dir", value_hint = ValueHint::DirPath)]
        capsule: PathBuf,
        #[arg(long = "nonce", hide = true)]
        nonce: String,
        #[arg(long = "validation-index", hide = true)]
        validation_index: Option<usize>,
    },
    /// Check whether the isolated one-shot runtime is ready
    Doctor {
        #[command(flatten)]
        output: OutputModeArgs,
    },
}

#[derive(Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: Option<AuthCommand>,
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
    /// Hidden alias for `--format json` (kept for backwards compatibility).
    #[arg(long = "json", hide = true, conflicts_with = "format")]
    pub json: bool,
}

impl OutputModeArgs {
    pub fn is_json(&self) -> bool {
        self.json || matches!(self.format, Some(OutputFormat::Json))
    }
}

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Login to Codex with ChatGPT (browser/device-code) or API key
    Login {
        #[command(flatten)]
        output: OutputModeArgs,
        /// Use API key login flow
        #[arg(long = "api-key")]
        api_key: bool,
        /// Use ChatGPT device-code login flow
        #[arg(long = "device-code")]
        device_code: bool,
    },
    /// Switch to a secret by name/name.json or email
    Use {
        #[command(flatten)]
        output: OutputModeArgs,
        #[arg(id = "target", value_name = "target", num_args = 0..)]
        args: Vec<String>,
    },
    /// Save active CODEX_AUTH_FILE into CODEX_SECRET_DIR as SECRET_JSON (auto-appends .json when missing)
    Save {
        #[command(flatten)]
        output: OutputModeArgs,
        /// Overwrite target file if it already exists (non-interactive)
        #[arg(short = 'y', long = "yes")]
        yes: bool,
        #[arg(id = "secret", value_name = "secret", num_args = 0..)]
        args: Vec<String>,
    },
    /// Remove SECRET_JSON from CODEX_SECRET_DIR (auto-appends .json when missing)
    Remove {
        #[command(flatten)]
        output: OutputModeArgs,
        /// Remove target file without prompt (non-interactive)
        #[arg(short = 'y', long = "yes")]
        yes: bool,
        #[arg(id = "secret", value_name = "secret", num_args = 0..)]
        args: Vec<String>,
    },
    /// Refresh OAuth tokens
    Refresh {
        #[command(flatten)]
        output: OutputModeArgs,
        #[arg(id = "secret", value_name = "secret", num_args = 0..)]
        args: Vec<String>,
    },
    /// Refresh stale tokens across auth + secrets
    AutoRefresh {
        #[command(flatten)]
        output: OutputModeArgs,
    },
    /// Report active Codex auth readiness without exposing secrets
    Status {
        #[command(flatten)]
        output: OutputModeArgs,
    },
    /// Show which secret matches CODEX_AUTH_FILE
    Current {
        #[command(flatten)]
        output: OutputModeArgs,
    },
    /// Sync CODEX_AUTH_FILE back into matching secrets
    Sync {
        #[command(flatten)]
        output: OutputModeArgs,
    },
    /// Pull access-only auth from a remote Codex token authority
    Remote(AuthRemoteArgs),
}

#[derive(Args)]
pub struct AuthRemoteArgs {
    #[command(subcommand)]
    pub command: Option<AuthRemoteCommand>,
}

#[derive(Subcommand)]
pub enum AuthRemoteCommand {
    /// Pull remote auth over SSH and write it to CODEX_AUTH_FILE
    Pull {
        #[command(flatten)]
        output: OutputModeArgs,
        /// SSH host alias for the token authority
        #[arg(long = "ssh", value_name = "host")]
        ssh: Option<String>,
        /// Remote stored secret name
        #[arg(long = "name", value_name = "name")]
        name: Option<String>,
        /// Import access/id/account fields only; write only a local refresh_token placeholder
        #[arg(long = "access-only")]
        access_only: bool,
        /// Write the pulled auth payload into CODEX_AUTH_FILE
        #[arg(long = "write-active")]
        write_active: bool,
        /// Ask the remote authority to refresh the named secret before export
        #[arg(long = "refresh")]
        refresh: bool,
    },
    /// Export remote auth payload for SSH transport
    #[command(hide = true)]
    Export {
        /// Remote stored secret name
        #[arg(long = "name", value_name = "name")]
        name: String,
        /// Export access/id/account fields only; never export refresh_token
        #[arg(long = "access-only")]
        access_only: bool,
        /// Refresh the named remote secret before exporting it
        #[arg(long = "refresh")]
        refresh: bool,
    },
}

#[derive(Args)]
pub struct DiagArgs {
    #[command(subcommand)]
    pub command: Option<DiagCommand>,
}

#[derive(Subcommand)]
pub enum DiagCommand {
    /// Rate-limits diagnostics
    RateLimits(RateLimitsArgs),
}

#[derive(Args)]
pub struct RateLimitsArgs {
    /// Clear prompt-segment cache before querying
    #[arg(short = 'c', long = "clear-cache")]
    pub clear_cache: bool,
    /// Debug output
    #[arg(short = 'd', long = "debug")]
    pub debug: bool,
    /// Cached mode (no network)
    #[arg(long = "cached")]
    pub cached: bool,
    /// Disable refresh-on-401 behavior even when CODEX_AUTO_REFRESH_ENABLED=true
    #[arg(long = "no-refresh-auth")]
    pub no_refresh_auth: bool,
    /// Output format (`text` or `json`)
    #[arg(long = "format", value_enum, value_name = "format")]
    pub format: Option<OutputFormat>,
    /// Hidden alias for `--format json` (kept for backwards compatibility).
    #[arg(long = "json", hide = true, conflicts_with = "format")]
    pub json: bool,
    /// Output a one-line summary
    #[arg(long = "one-line")]
    pub one_line: bool,
    /// Query all secrets under CODEX_SECRET_DIR
    #[arg(long = "all")]
    pub all: bool,
    /// Run concurrent async mode
    #[arg(long = "async")]
    pub async_mode: bool,
    /// Refresh output every 60 seconds until interrupted (requires --async)
    #[arg(long = "watch", requires = "async_mode")]
    pub watch: bool,
    /// Max concurrent jobs (async mode)
    #[arg(long = "jobs")]
    pub jobs: Option<String>,
    /// Optional secret.json
    pub secret: Option<String>,
}

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: Option<ConfigCommand>,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Show current configuration
    Show,
    /// Set configuration value (current shell only)
    Set { key: String, value: String },
}

#[derive(Args)]
pub struct PromptSegmentArgs {
    #[command(subcommand)]
    pub command: Option<PromptSegmentCommand>,
    /// Hide the 5h window output
    #[arg(long = "no-5h")]
    pub no_5h: bool,
    /// Cache TTL
    #[arg(long = "ttl")]
    pub ttl: Option<String>,
    /// Reset time format (local time)
    #[arg(long = "time-format")]
    pub time_format: Option<String>,
    /// Show timezone offset in the default reset time display
    #[arg(long = "show-timezone")]
    pub show_timezone: bool,
    /// Force a blocking refresh
    #[arg(long = "refresh")]
    pub refresh: bool,
    /// Exit 0 if prompt-segment output is enabled and active auth is usable
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

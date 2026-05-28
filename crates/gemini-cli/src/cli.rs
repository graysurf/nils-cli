use clap::{Args, Parser, Subcommand, ValueEnum};

const ROOT_AFTER_HELP: &str = "\
EXAMPLES:
  gemini-cli agent prompt 'Summarize this diff'
  gemini-cli auth status --format json
  gemini-cli prompt-segment status
  gemini-cli completion zsh

ENVIRONMENT:
  GEMINI_CLI_MODEL, GEMINI_CLI_REASONING, GEMINI_ALLOW_DANGEROUS_ENABLED
  GEMINI_SECRET_DIR, GEMINI_AUTH_FILE, GEMINI_SECRET_CACHE_DIR
  GEMINI_AUTO_REFRESH_ENABLED, GEMINI_AUTO_REFRESH_MIN_DAYS
  GEMINI_OAUTH_PROVIDER, GEMINI_OAUTH_CLIENT_ID, GEMINI_OAUTH_CLIENT_SECRET
  GEMINI_REFRESH_AUTH_CURL_CONNECT_TIMEOUT_SECONDS, GEMINI_REFRESH_AUTH_CURL_MAX_TIME_SECONDS
  GEMINI_RATE_LIMITS_DEFAULT_ALL_ENABLED, GEMINI_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS, GEMINI_RATE_LIMITS_CURL_MAX_TIME_SECONDS
  CODE_ASSIST_ENDPOINT, GEMINI_CODE_ASSIST_ENDPOINT, CODE_ASSIST_API_VERSION, GEMINI_CODE_ASSIST_API_VERSION
  GEMINI_CODE_ASSIST_PROJECT, GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_PROJECT_ID
  GEMINI_PROMPT_SEGMENT_ENABLED, GEMINI_PROMPT_SEGMENT_TTL, GEMINI_PROMPT_SEGMENT_STALE_SUFFIX, GEMINI_PROMPT_SEGMENT_NAME_SOURCE
  GEMINI_PROMPT_SEGMENT_REFRESH_MIN_SECONDS, GEMINI_PROMPT_SEGMENT_LOCK_STALE_SECONDS
  GEMINI_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS, GEMINI_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS, GEMINI_PROMPT_SEGMENT_EXE
  GEMINI_PROMPT_SEGMENT_SHOW_5H_ENABLED, GEMINI_PROMPT_SEGMENT_SHOW_FALLBACK_NAME_ENABLED, GEMINI_PROMPT_SEGMENT_SHOW_FULL_EMAIL_ENABLED, GEMINI_PROMPT_SEGMENT_COLOR_ENABLED
  ZSH_CACHE_DIR, NO_COLOR, STARSHIP_SESSION_KEY, STARSHIP_SHELL

EXIT CODES:
  0   success
  1   runtime error
  64  command-line usage error
  65  invalid input data";

#[derive(Parser)]
#[command(
    name = "gemini-cli",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Gemini CLI for nils-cli workspace",
    long_about = "Run Gemini-oriented agent helpers, authentication helpers, diagnostics, configuration, and prompt-segment utilities.",
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

#[derive(Subcommand)]
pub enum AgentCommand {
    /// Run a raw prompt
    Prompt {
        #[arg(value_name = "prompt", num_args = 0..)]
        prompt: Vec<String>,
    },
    /// Get actionable engineering advice
    Advice {
        #[arg(value_name = "question", num_args = 0..)]
        question: Vec<String>,
    },
    /// Get an explanation for a concept
    Knowledge {
        #[arg(value_name = "concept", num_args = 0..)]
        concept: Vec<String>,
    },
    /// Run the semantic-commit workflow
    Commit {
        /// Push after committing
        #[arg(short = 'p', long = "push")]
        push: bool,
        /// Autostage changes before committing
        #[arg(short = 'a', long = "auto-stage")]
        auto_stage: bool,
        /// Extra prompt text
        #[arg(value_name = "extra", num_args = 0..)]
        extra: Vec<String>,
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
    /// Login to Gemini with browser/device-code or API key
    Login {
        #[command(flatten)]
        output: OutputModeArgs,
        /// Use API key login flow
        #[arg(long = "api-key")]
        api_key: bool,
        /// Use device-code login flow
        #[arg(long = "device-code")]
        device_code: bool,
    },
    /// Switch to a secret by name or email
    Use {
        #[command(flatten)]
        output: OutputModeArgs,
        #[arg(id = "target", value_name = "target", num_args = 0..)]
        args: Vec<String>,
    },
    /// Save active auth file into secret directory as SECRET_JSON
    Save {
        #[command(flatten)]
        output: OutputModeArgs,
        /// Overwrite target file if it already exists (non-interactive)
        #[arg(short = 'y', long = "yes")]
        yes: bool,
        #[arg(id = "secret", value_name = "secret", num_args = 0..)]
        args: Vec<String>,
    },
    /// Remove SECRET_JSON from secret directory
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
    /// Show which secret matches active auth file
    Current {
        #[command(flatten)]
        output: OutputModeArgs,
    },
    /// Sync active auth file back into matching secrets
    Sync {
        #[command(flatten)]
        output: OutputModeArgs,
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
    #[arg(short = 'c')]
    pub clear_cache: bool,
    /// Debug output
    #[arg(short = 'd', long = "debug")]
    pub debug: bool,
    /// Cached mode (no network)
    #[arg(long = "cached")]
    pub cached: bool,
    /// Disable refresh-on-401 behavior
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
    /// Query all secrets under secret directory
    #[arg(long = "all")]
    pub all: bool,
    /// Run concurrent async mode
    #[arg(long = "async")]
    pub async_mode: bool,
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
    /// Exit 0 if prompt-segment output is enabled
    #[arg(long = "is-enabled")]
    pub is_enabled: bool,
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

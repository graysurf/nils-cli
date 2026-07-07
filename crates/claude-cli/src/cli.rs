use clap::{Args, Parser, Subcommand, ValueEnum};

const ROOT_AFTER_HELP: &str = "\
EXAMPLES:
  claude-cli prompt-segment
  claude-cli prompt-segment --refresh
  claude-cli prompt-segment status --format json
  claude-cli usage --format json --source auto
  claude-cli completion zsh

ENVIRONMENT:
  CLAUDE_PROMPT_TTL, CLAUDE_PROMPT_STALE_SUFFIX
  CLAUDE_PROMPT_SEGMENT_TTL, CLAUDE_PROMPT_SEGMENT_STALE_SUFFIX
  CLAUDE_PROMPT_SEGMENT_CACHE_DIR, CLAUDE_PROMPT_SEGMENT_ENDPOINT
  CLAUDE_PROMPT_SEGMENT_ACCESS_TOKEN, CLAUDE_PROMPT_SEGMENT_CREDENTIALS_JSON
  CLAUDE_PROMPT_SEGMENT_KEYCHAIN_DISABLED, CLAUDE_PROMPT_SEGMENT_KEYCHAIN_SERVICE
  NO_COLOR

EXIT CODES:
  0   success
  1   runtime false/failed state
  64  command-line usage error";

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
    /// Prompt-segment command group
    PromptSegment(PromptSegmentArgs),
    /// Read Claude usage from OAuth, Claude CLI, or cache
    Usage(UsageArgs),
    /// Export shell completion script
    Completion(CompletionArgs),
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
    /// Reset time format (local time)
    #[arg(long = "time-format")]
    pub time_format: Option<String>,
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

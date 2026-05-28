use clap::{Args, Parser, Subcommand, ValueEnum};

const ROOT_AFTER_HELP: &str = "\
EXAMPLES:
  api-websocket requests/health.ws.json
  api-websocket call --env staging requests/health.ws.json
  api-websocket report --case health --request requests/health.ws.json --run

ENVIRONMENT:
  WS_URL, WS_ENV_DEFAULT, WS_URL_*
  WS_TOKEN_NAME, WS_TOKEN_*, ACCESS_TOKEN, SERVICE_TOKEN
  WS_JWT_VALIDATE_ENABLED, WS_JWT_VALIDATE_STRICT, WS_JWT_VALIDATE_LEEWAY_SECONDS
  WS_HISTORY_ENABLED, WS_HISTORY_FILE, WS_HISTORY_MAX_MB, WS_HISTORY_ROTATE_COUNT
  WS_HISTORY_LOG_URL_ENABLED
  WS_REPORT_DIR, WS_REPORT_INCLUDE_COMMAND_ENABLED, WS_REPORT_COMMAND_LOG_URL_ENABLED

EXIT CODES:
  0   success
  1   runtime error
  64  command-line usage error
  65  invalid input data";

#[derive(Parser)]
#[command(
    name = "api-websocket",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "WebSocket API runner (call/history/report)",
    long_about = "Run WebSocket request files, inspect history, and generate Markdown reports. A bare request file is treated as the `call` subcommand.",
    after_help = ROOT_AFTER_HELP,
    disable_help_subcommand = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Execute a request file and print the last received message to stdout (default)
    Call(CallArgs),
    /// Print the last (or last N) history entries
    History(HistoryArgs),
    /// Generate a Markdown API test report
    Report(ReportArgs),
    /// Generate a Markdown API test report from a saved `call` command snippet
    ReportFromCmd(ReportFromCmdArgs),
    /// Print shell completion script
    Completion(CompletionArgs),
}

#[derive(Args)]
pub(crate) struct CompletionArgs {
    /// Shell to generate completion for
    #[arg(value_enum)]
    pub(crate) shell: crate::completion::CompletionShell,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum OutputFormat {
    Text,
    Json,
}

#[derive(Args)]
pub(crate) struct CallArgs {
    /// Endpoint preset name (or literal URL if it starts with ws(s)://)
    #[arg(short = 'e', long = "env")]
    pub(crate) env: Option<String>,

    /// Explicit WebSocket URL
    #[arg(short = 'u', long = "url")]
    pub(crate) url: Option<String>,

    /// Token profile name
    #[arg(long = "token")]
    pub(crate) token: Option<String>,

    /// WebSocket setup dir (discovery seed)
    #[arg(long = "config-dir")]
    pub(crate) config_dir: Option<String>,

    /// Disable writing to .ws_history for this run
    #[arg(long = "no-history")]
    pub(crate) no_history: bool,

    /// Response output format
    #[arg(long = "format", value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) format: OutputFormat,

    /// Request file path (*.ws.json or *.websocket.json)
    #[arg(value_name = "request.ws.json")]
    pub(crate) request: String,
}

#[derive(Args)]
pub(crate) struct HistoryArgs {
    /// WebSocket setup dir (discovery seed)
    #[arg(long = "config-dir")]
    pub(crate) config_dir: Option<String>,

    /// Explicit history file path (relative paths resolve under setup dir)
    #[arg(long = "file")]
    pub(crate) file: Option<String>,

    /// Print the last entry (default)
    #[arg(long = "last", conflicts_with = "tail")]
    pub(crate) last: bool,

    /// Print the last N entries (blank-line separated)
    #[arg(long = "tail")]
    pub(crate) tail: Option<u32>,

    /// Omit metadata lines (starting with "#") from each entry
    #[arg(long = "command-only")]
    pub(crate) command_only: bool,

    /// Output format
    #[arg(long = "format", value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) format: OutputFormat,
}

#[derive(Args)]
pub(crate) struct ReportArgs {
    /// Report case name
    #[arg(long = "case")]
    pub(crate) case: String,

    /// Request file path (*.ws.json or *.websocket.json)
    #[arg(long = "request")]
    pub(crate) request: String,

    /// Output report path (default: <project_root>/docs/<stamp>-<case>-api-test-report.md)
    #[arg(long = "out")]
    pub(crate) out: Option<String>,

    /// Endpoint preset name (passed through)
    #[arg(short = 'e', long = "env")]
    pub(crate) env: Option<String>,

    /// Explicit WebSocket URL (passed through)
    #[arg(short = 'u', long = "url")]
    pub(crate) url: Option<String>,

    /// Token profile name (passed through)
    #[arg(long = "token")]
    pub(crate) token: Option<String>,

    /// Execute the request and embed the transcript
    #[arg(
        long = "run",
        conflicts_with = "response",
        required_unless_present = "response"
    )]
    pub(crate) run: bool,

    /// Use an existing response/transcript file (or "-" for stdin)
    #[arg(
        long = "response",
        conflicts_with = "run",
        required_unless_present = "run"
    )]
    pub(crate) response: Option<String>,

    /// Do not redact secrets in request JSON blocks
    #[arg(long = "no-redact")]
    pub(crate) no_redact: bool,

    /// Omit the command snippet section
    #[arg(long = "no-command")]
    pub(crate) no_command: bool,

    /// When using --url, omit the URL value in the command snippet
    #[arg(long = "no-command-url")]
    pub(crate) no_command_url: bool,

    /// Override project root (default: git root or CWD)
    #[arg(long = "project-root")]
    pub(crate) project_root: Option<String>,

    /// WebSocket setup dir (passed through)
    #[arg(long = "config-dir")]
    pub(crate) config_dir: Option<String>,
}

#[derive(Args)]
pub(crate) struct ReportFromCmdArgs {
    /// Report case name (default: derived from the snippet)
    #[arg(long = "case")]
    pub(crate) case: Option<String>,

    /// Output report path (default: <project_root>/docs/<stamp>-<case>-api-test-report.md)
    #[arg(long = "out")]
    pub(crate) out: Option<String>,

    /// Use an existing response file (or "-" for stdin)
    ///
    /// Note: when using "--response -", stdin is reserved for the response body; provide the snippet as a positional argument.
    #[arg(long = "response")]
    pub(crate) response: Option<String>,

    /// Allow generating a report with an empty/no-data response (no-op for api-websocket; kept for parity)
    #[arg(long = "allow-empty", alias = "expect-empty")]
    pub(crate) allow_empty: bool,

    /// Print the equivalent `api-websocket report ...` command and exit 0
    #[arg(long = "dry-run")]
    pub(crate) dry_run: bool,

    /// Read the command snippet from stdin
    #[arg(long = "stdin", conflicts_with = "snippet")]
    pub(crate) stdin: bool,

    /// Command snippet (e.g. from `api-websocket history --command-only`)
    #[arg(value_name = "snippet", required_unless_present = "stdin")]
    pub(crate) snippet: Option<String>,
}

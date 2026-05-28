use clap::{Args, Parser, Subcommand};

const ROOT_AFTER_HELP: &str = "\
EXAMPLES:
  api-gql ops/health.graphql vars.json
  api-gql call --env staging ops/health.graphql
  api-gql report --case health --op ops/health.graphql --run
  api-gql schema --cat

ENVIRONMENT:
  GQL_URL, GQL_ENV_DEFAULT, GQL_URL_*
  GQL_JWT_NAME, GQL_JWT_*, ACCESS_TOKEN, SERVICE_TOKEN
  GQL_SCHEMA_FILE
  GQL_JWT_VALIDATE_ENABLED, GQL_JWT_VALIDATE_STRICT, GQL_JWT_VALIDATE_LEEWAY_SECONDS
  GQL_HISTORY_ENABLED, GQL_HISTORY_FILE, GQL_HISTORY_MAX_MB, GQL_HISTORY_ROTATE_COUNT
  GQL_HISTORY_LOG_URL_ENABLED, GQL_VARS_MIN_LIMIT
  GQL_REPORT_DIR, GQL_REPORT_INCLUDE_COMMAND_ENABLED, GQL_REPORT_COMMAND_LOG_URL_ENABLED
  GQL_ALLOW_EMPTY_ENABLED

EXIT CODES:
  0   success
  1   runtime error
  64  command-line usage error
  65  invalid input data";

#[derive(Parser)]
#[command(
    name = "api-gql",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "GraphQL runner (default subcommand: call)",
    long_about = "Run GraphQL operation files, inspect history, generate Markdown reports, and resolve schemas. A bare operation file is treated as the `call` subcommand.",
    after_help = ROOT_AFTER_HELP,
    disable_help_subcommand = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Execute an operation (and optional variables) and print the response body JSON to stdout (default)
    Call(CallArgs),
    /// Print the last (or last N) history entries
    History(HistoryArgs),
    /// Generate a Markdown API test report
    Report(ReportArgs),
    /// Generate a report from a command snippet (arg or stdin)
    ReportFromCmd(ReportFromCmdArgs),
    /// Print shell completion script
    Completion(CompletionArgs),
    /// Resolve a schema file path (or print schema contents)
    Schema(SchemaArgs),
}

#[derive(Args)]
pub(crate) struct CompletionArgs {
    /// Shell to generate completion for
    #[arg(value_enum)]
    pub(crate) shell: crate::completion::CompletionShell,
}

#[derive(Args, Clone)]
pub(crate) struct CallArgs {
    /// Endpoint preset name (or literal URL if it starts with http(s)://)
    #[arg(short = 'e', long = "env")]
    pub(crate) env: Option<String>,

    /// Explicit GraphQL endpoint URL
    #[arg(short = 'u', long = "url")]
    pub(crate) url: Option<String>,

    /// JWT profile name
    #[arg(long = "jwt")]
    pub(crate) jwt: Option<String>,

    /// GraphQL setup dir (discovery seed)
    #[arg(long = "config-dir")]
    pub(crate) config_dir: Option<String>,

    /// Print available env names from endpoints.env, then exit
    #[arg(long = "list-envs")]
    pub(crate) list_envs: bool,

    /// Print available JWT profile names from jwts(.local).env, then exit
    #[arg(long = "list-jwts")]
    pub(crate) list_jwts: bool,

    /// Disable writing to .gql_history for this run
    #[arg(long = "no-history")]
    pub(crate) no_history: bool,

    /// Operation file path (*.graphql)
    #[arg(value_name = "operation.graphql")]
    pub(crate) operation: Option<String>,

    /// Variables JSON file path
    #[arg(value_name = "variables.json")]
    pub(crate) variables: Option<String>,
}

#[derive(Args)]
pub(crate) struct HistoryArgs {
    /// GraphQL setup dir (discovery seed)
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
}

#[derive(Args, Clone)]
pub(crate) struct ReportArgs {
    /// Report case name
    #[arg(long = "case")]
    pub(crate) case: String,

    /// GraphQL operation file path (*.graphql)
    #[arg(long = "op", alias = "operation")]
    pub(crate) op: String,

    /// Variables JSON file path
    #[arg(long = "vars", alias = "variables")]
    pub(crate) vars: Option<String>,

    /// Output report path (default: <project_root>/docs/<stamp>-<case>-api-test-report.md)
    #[arg(long = "out")]
    pub(crate) out: Option<String>,

    /// Endpoint preset name (passed through)
    #[arg(short = 'e', long = "env")]
    pub(crate) env: Option<String>,

    /// Explicit GraphQL endpoint URL (passed through)
    #[arg(short = 'u', long = "url")]
    pub(crate) url: Option<String>,

    /// JWT profile name (passed through)
    #[arg(long = "jwt")]
    pub(crate) jwt: Option<String>,

    /// Execute the request and embed the response
    #[arg(
        long = "run",
        conflicts_with = "response",
        required_unless_present = "response"
    )]
    pub(crate) run: bool,

    /// Use an existing response file (or "-" for stdin)
    #[arg(
        long = "response",
        conflicts_with = "run",
        required_unless_present = "run"
    )]
    pub(crate) response: Option<String>,

    /// Allow generating a report with an empty/no-data response (or as a draft without --run/--response)
    #[arg(long = "allow-empty", alias = "expect-empty")]
    pub(crate) allow_empty: bool,

    /// Do not redact secrets in variables/response JSON blocks
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

    /// GraphQL setup dir (passed through)
    #[arg(long = "config-dir")]
    pub(crate) config_dir: Option<String>,
}

#[derive(Args)]
pub(crate) struct ReportFromCmdArgs {
    /// Override report case name (default: derived from snippet)
    #[arg(long = "case")]
    pub(crate) case: Option<String>,

    /// Output report path (default: <project_root>/docs/<stamp>-<case>-api-test-report.md)
    #[arg(long = "out")]
    pub(crate) out: Option<String>,

    /// Use an existing response file (or "-" for stdin)
    #[arg(long = "response")]
    pub(crate) response: Option<String>,

    /// Allow generating a report with an empty/no-data response
    #[arg(long = "allow-empty", alias = "expect-empty")]
    pub(crate) allow_empty: bool,

    /// Print equivalent `api-gql report ...` command and exit 0 (no network)
    #[arg(long = "dry-run")]
    pub(crate) dry_run: bool,

    /// Read the command snippet from stdin
    #[arg(long = "stdin", conflicts_with = "snippet")]
    pub(crate) stdin: bool,

    /// Command snippet (e.g. from `api-gql history --command-only`)
    #[arg(value_name = "snippet", required_unless_present = "stdin")]
    pub(crate) snippet: Option<String>,
}

#[derive(Args)]
pub(crate) struct SchemaArgs {
    /// GraphQL setup dir (same discovery semantics as call)
    #[arg(long = "config-dir")]
    pub(crate) config_dir: Option<String>,

    /// Explicit schema file path (overrides env + schema.env)
    #[arg(long = "file")]
    pub(crate) file: Option<String>,

    /// Print schema file contents (default: print resolved path)
    #[arg(long = "cat")]
    pub(crate) cat: bool,
}

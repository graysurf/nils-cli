use std::env;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

pub use nils_common::cli_contract::OutputFormat;

const MEMO_AFTER_HELP: &str = "\
EXAMPLES:
  memo-cli add 'Follow up with the release note'
  memo-cli list --state pending --limit 10
  memo-cli report week --tz Asia/Taipei

ENVIRONMENT:
  XDG_DATA_HOME  Base directory for the default SQLite database path.
  HOME           Fallback base directory for the default SQLite database path.

EXIT CODES:
  0   success
  1   runtime error
  64  command-line usage error
  65  invalid input data";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ItemState {
    All,
    Pending,
    Enriched,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SearchField {
    Raw,
    Derived,
    Tags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SearchMatch {
    Fts,
    Prefix,
    Contains,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReportPeriod {
    Week,
    Month,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FetchState {
    Pending,
}

#[derive(Debug, Parser)]
#[command(
    name = "memo-cli",
    version,
    about = "Capture-first memo CLI with agent enrichment",
    long_about = "Capture, search, report, fetch, and apply memo records through a local SQLite database with text or JSON output.",
    after_help = MEMO_AFTER_HELP
)]
pub struct Cli {
    /// SQLite file path
    #[arg(long, global = true, value_name = "path", default_value_os_t = default_db_path())]
    pub db: PathBuf,

    /// Hidden alias for `--format json` (kept for backwards compatibility).
    #[arg(long, global = true, hide = true, conflicts_with = "format")]
    pub json: bool,

    /// Output format
    #[arg(long, global = true, value_enum)]
    pub format: Option<OutputFormat>,

    #[command(subcommand)]
    pub command: MemoCommand,
}

#[derive(Debug, Subcommand)]
pub enum MemoCommand {
    /// Capture one raw memo entry
    #[command(long_about = "Capture one raw memo entry with source and timestamp metadata.")]
    Add(AddArgs),
    /// Update one memo entry and reset derived workflow state
    #[command(long_about = "Update one memo entry and reset derived enrichment workflow state.")]
    Update(UpdateArgs),
    /// Delete one memo entry and all dependent data
    #[command(
        long_about = "Delete one memo entry and all dependent derived data. Interactive terminals prompt by default; non-interactive deletion requires --yes."
    )]
    Delete(DeleteArgs),
    /// List memo entries in deterministic order
    #[command(
        long_about = "List memo entries in deterministic order with paging and state filters."
    )]
    List(ListArgs),
    /// Search memo entries with selectable match mode
    #[command(long_about = "Search memo entries across raw text, derived text, and tags.")]
    Search(SearchArgs),
    /// Show weekly or monthly summary report
    #[command(long_about = "Show weekly or monthly summary reports over captured memo entries.")]
    Report(ReportArgs),
    /// Fetch pending items for agent enrichment
    #[command(long_about = "Fetch pending memo entries for external agent enrichment.")]
    Fetch(FetchArgs),
    /// Apply enrichment payloads
    #[command(long_about = "Apply validated enrichment payloads to memo entries.")]
    Apply(ApplyArgs),
    /// Print shell completion script
    #[command(long_about = "Print a shell completion script for memo-cli.")]
    Completion(CompletionArgs),
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    #[arg(value_enum)]
    pub shell: crate::completion::CompletionShell,
}

#[derive(Debug, clap::Args)]
pub struct AddArgs {
    /// Memo text
    pub text: String,

    /// Capture source label
    #[arg(long, default_value = "cli")]
    pub source: String,

    /// Capture timestamp (RFC3339)
    #[arg(long)]
    pub at: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct UpdateArgs {
    /// Item identifier (itm_XXXXXXXX or integer id)
    pub item_id: String,

    /// Updated memo text
    pub text: String,
}

#[derive(Debug, clap::Args)]
pub struct DeleteArgs {
    /// Item identifier (itm_XXXXXXXX or integer id)
    pub item_id: String,

    /// Deprecated compatibility confirmation flag; use --yes to confirm deletion.
    #[arg(long)]
    pub hard: bool,

    /// Confirm deletion without prompting
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, clap::Args)]
pub struct ListArgs {
    /// Max rows to return
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Row offset for paging
    #[arg(long, default_value_t = 0)]
    pub offset: usize,

    /// Row selection mode
    #[arg(long, value_enum, default_value_t = ItemState::All)]
    pub state: ItemState,
}

#[derive(Debug, clap::Args)]
pub struct SearchArgs {
    /// Search query text
    pub query: String,

    /// Max rows to return
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Row selection mode
    #[arg(long, value_enum, default_value_t = ItemState::All)]
    pub state: ItemState,

    /// Search fields (comma-separated): raw, derived, tags
    #[arg(
        long = "field",
        value_enum,
        value_delimiter = ',',
        default_values_t = [SearchField::Raw, SearchField::Derived, SearchField::Tags]
    )]
    pub fields: Vec<SearchField>,

    /// Search match mode: fts, prefix, contains
    #[arg(long = "match", value_enum, default_value_t = SearchMatch::Fts)]
    pub match_mode: SearchMatch,
}

#[derive(Debug, clap::Args)]
pub struct ReportArgs {
    /// Report period: week or month
    pub period: ReportPeriod,

    /// IANA timezone for canonical period windows
    #[arg(long)]
    pub tz: Option<String>,

    /// Custom report start timestamp (RFC3339)
    #[arg(long)]
    pub from: Option<String>,

    /// Custom report end timestamp (RFC3339)
    #[arg(long)]
    pub to: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct FetchArgs {
    /// Max rows to return
    #[arg(long, default_value_t = 50)]
    pub limit: usize,

    /// Optional cursor (reserved for future pagination)
    #[arg(long)]
    pub cursor: Option<String>,

    /// Fetch selection mode
    #[arg(long, value_enum, default_value_t = FetchState::Pending)]
    pub state: FetchState,
}

#[derive(Debug, clap::Args)]
pub struct ApplyArgs {
    /// JSON file containing apply payload
    #[arg(long)]
    pub input: Option<PathBuf>,

    /// Read payload JSON from stdin
    #[arg(long)]
    pub stdin: bool,

    /// Validate payload without write-back
    #[arg(long)]
    pub dry_run: bool,
}

impl Cli {
    pub fn output_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            self.format.unwrap_or_default()
        }
    }

    pub fn command_id(&self) -> &'static str {
        match self.command {
            MemoCommand::Add(_) => "memo-cli add",
            MemoCommand::Update(_) => "memo-cli update",
            MemoCommand::Delete(_) => "memo-cli delete",
            MemoCommand::List(_) => "memo-cli list",
            MemoCommand::Search(_) => "memo-cli search",
            MemoCommand::Report(_) => "memo-cli report",
            MemoCommand::Fetch(_) => "memo-cli fetch",
            MemoCommand::Apply(_) => "memo-cli apply",
            MemoCommand::Completion(_) => "memo-cli completion",
        }
    }

    pub fn schema_version(&self) -> &'static str {
        match self.command {
            MemoCommand::Add(_) => "cli.memo-cli.add.v1",
            MemoCommand::Update(_) => "cli.memo-cli.update.v1",
            MemoCommand::Delete(_) => "cli.memo-cli.delete.v1",
            MemoCommand::List(_) => "cli.memo-cli.list.v1",
            MemoCommand::Search(_) => "cli.memo-cli.search.v1",
            MemoCommand::Report(_) => "cli.memo-cli.report.v1",
            MemoCommand::Fetch(_) => "cli.memo-cli.fetch.v1",
            MemoCommand::Apply(_) => "cli.memo-cli.apply.v1",
            MemoCommand::Completion(_) => "cli.memo-cli.completion.v1",
        }
    }
}

fn default_db_path() -> PathBuf {
    if let Some(data_home) = env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(data_home).join("nils-cli").join("memo.db");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("nils-cli")
            .join("memo.db");
    }

    PathBuf::from("memo.db")
}

#[cfg(test)]
pub(crate) mod tests {
    use clap::{CommandFactory, Parser};

    use super::{Cli, MemoCommand, OutputFormat, SearchField, SearchMatch};

    #[test]
    fn output_format_defaults_to_text() {
        let cli = Cli::parse_from(["memo-cli", "list"]);
        assert_eq!(cli.output_format(), OutputFormat::Text);
    }

    #[test]
    fn output_format_json_flag_wins() {
        let cli = Cli::parse_from(["memo-cli", "--json", "list"]);
        assert_eq!(cli.output_format(), OutputFormat::Json);
    }

    #[test]
    fn output_format_format_json_is_supported() {
        let cli = Cli::parse_from(["memo-cli", "--format", "json", "list"]);
        assert_eq!(cli.output_format(), OutputFormat::Json);
    }

    #[test]
    fn output_format_rejects_conflict_at_parse_time() {
        let err = Cli::try_parse_from(["memo-cli", "--json", "--format", "text", "list"])
            .expect_err("clap should reject --json + --format conflict");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn parser_exposes_expected_subcommands() {
        let mut cmd = Cli::command();
        let subcommands = cmd
            .get_subcommands_mut()
            .map(|sub| sub.get_name().to_string())
            .collect::<Vec<_>>();
        assert!(subcommands.contains(&"add".to_string()));
        assert!(subcommands.contains(&"update".to_string()));
        assert!(subcommands.contains(&"delete".to_string()));
        assert!(subcommands.contains(&"list".to_string()));
        assert!(subcommands.contains(&"search".to_string()));
        assert!(subcommands.contains(&"report".to_string()));
        assert!(subcommands.contains(&"fetch".to_string()));
        assert!(subcommands.contains(&"apply".to_string()));
        assert!(subcommands.contains(&"completion".to_string()));
    }

    #[test]
    fn search_fields_supports_comma_separated_values() {
        let cli = Cli::parse_from(["memo-cli", "search", "ssd", "--field", "raw,tags"]);
        let MemoCommand::Search(args) = cli.command else {
            panic!("expected search command");
        };

        assert_eq!(args.fields, vec![SearchField::Raw, SearchField::Tags]);
    }

    #[test]
    fn search_fields_default_to_all_fields() {
        let cli = Cli::parse_from(["memo-cli", "search", "ssd"]);
        let MemoCommand::Search(args) = cli.command else {
            panic!("expected search command");
        };

        assert_eq!(
            args.fields,
            vec![SearchField::Raw, SearchField::Derived, SearchField::Tags]
        );
    }

    #[test]
    fn search_match_mode_defaults_to_fts() {
        let cli = Cli::parse_from(["memo-cli", "search", "ssd"]);
        let MemoCommand::Search(args) = cli.command else {
            panic!("expected search command");
        };

        assert_eq!(args.match_mode, SearchMatch::Fts);
    }

    #[test]
    fn search_match_mode_accepts_explicit_value() {
        let cli = Cli::parse_from(["memo-cli", "search", "ssd", "--match", "contains"]);
        let MemoCommand::Search(args) = cli.command else {
            panic!("expected search command");
        };

        assert_eq!(args.match_mode, SearchMatch::Contains);
    }
}

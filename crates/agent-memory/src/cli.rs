use clap::{Args, Parser, Subcommand, ValueHint};
use clap_complete::engine::ArgValueCandidates;
use nils_common::cli_contract::OutputFormat;

use crate::completion::scope_candidates;

#[derive(Debug, Parser)]
#[command(
    name = "agent-memory",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Resolve and manage local agent memory directories.",
    long_about = "Resolve and manage a git-backed local agent memory store with curated notes, bounded recall profiles, producer candidates, explicit inactive history, personas, and per-agent scopes.",
    after_help = "SCOPE VALUES:\n  root             AGENT_MEMORY_HOME itself\n  global           curated shared memory store\n  <id>             shorthand for agents/<id>\n  agents/<id>      per-agent memory store\n  personas/<id>    persona launchpad directory\n  profiles/<id>    bounded recall profile\n  candidates/<id>  untrusted producer candidate store\n\nENVIRONMENT:\n  AGENT_MEMORY_HOME  Override memory-store root.\n  XDG_CONFIG_HOME    Parent for the default agent-memory root.\n  HOME               Fallback parent when XDG_CONFIG_HOME is unset.\n\nEXIT CODES:\n  0   success\n  1   runtime error or no recall match\n  64  command-line usage error",
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print directory for a scope.
    Path(ScopeArgs),
    /// List markdown files in a scope.
    #[command(alias = "ls")]
    List(ListArgs),
    /// Print MEMORY.md from a scope.
    #[command(alias = "idx")]
    Index(ScopeArgs),
    /// List registered per-agent scopes.
    Agents,
    /// List registered Claude Code personas.
    Personas,
    /// Create a per-agent memory scope.
    InitAgent(IdArgs),
    /// Scaffold a Claude Code persona launchpad.
    InitPersona(IdArgs),
    /// Print global and per-agent paths.
    Resolve(IdArgs),
    /// Emit shell exports for the resolved layout.
    Env,
    /// Verify memory-store layout.
    Doctor,
    /// Check structural integrity of a memory scope.
    Check(CheckArgs),
    /// Create a note and its index entry atomically.
    Add(AddArgs),
    /// Search note bodies and descriptions in a scope.
    Search(SearchArgs),
    /// Recall bounded startup, curated on-demand, or candidate memory.
    Recall(RecallArgs),
    /// Add, list, or promote untrusted memory candidates.
    Candidate(CandidateArgs),
    /// Inspect or retire superseded memory outside active recall.
    Archive(ArchiveArgs),
    /// Print shell completion script.
    Completion(CompletionArgs),
    /// Print help.
    Help,
}

#[derive(Debug, Args)]
pub struct ScopeArgs {
    /// Scope to resolve.
    #[arg(value_name = "SCOPE", value_hint = ValueHint::DirPath, add = ArgValueCandidates::new(scope_candidates))]
    pub scope: Option<String>,
}

#[derive(Debug, Args)]
pub struct IdArgs {
    /// Agent or persona ID.
    #[arg(value_name = "ID")]
    pub id: String,
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Scope to check (default: global).
    #[arg(value_name = "SCOPE", value_hint = ValueHint::DirPath, add = ArgValueCandidates::new(scope_candidates))]
    pub scope: Option<String>,
    /// Check every curated/profile scope (global, agents, personas, profiles).
    #[arg(long)]
    pub all: bool,
    /// Promote warn-level findings to failures.
    #[arg(long)]
    pub strict: bool,
    /// Fail when MEMORY.md exceeds this many bytes.
    #[arg(long, value_name = "BYTES")]
    pub max_index_bytes: Option<usize>,
    /// Fail on exact terms listed one per line in a regular, non-symlink file.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    pub forbid_terms_file: Option<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json` (kept for convenience).
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Scope to list (default: global).
    #[arg(value_name = "SCOPE", value_hint = ValueHint::DirPath, add = ArgValueCandidates::new(scope_candidates))]
    pub scope: Option<String>,
    /// Filter by frontmatter type (user|feedback|project|reference).
    #[arg(long, value_name = "TYPE")]
    pub r#type: Option<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json` (kept for convenience).
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Scope to write into (default: global).
    #[arg(value_name = "SCOPE", value_hint = ValueHint::DirPath, add = ArgValueCandidates::new(scope_candidates))]
    pub scope: Option<String>,
    /// Note slug (becomes `<slug>.md` and the frontmatter `name`).
    #[arg(long, value_name = "SLUG")]
    pub name: String,
    /// Note type (user|feedback|project|reference).
    #[arg(long, value_name = "TYPE")]
    pub r#type: String,
    /// One-line description for the frontmatter.
    #[arg(long, value_name = "TEXT")]
    pub description: String,
    /// Index title (defaults to the slug).
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,
    /// Index hook text (defaults to the description).
    #[arg(long, value_name = "TEXT")]
    pub hook: Option<String>,
    /// Read the note body from a file.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath, conflicts_with = "body")]
    pub body_file: Option<String>,
    /// Note body text, or `-` to read from stdin.
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,
    /// Stamp `metadata.originSessionId` with this value.
    #[arg(long, value_name = "UUID")]
    pub session_id: Option<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json` (kept for convenience).
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Term to find (matched in note bodies and descriptions).
    #[arg(value_name = "TERM")]
    pub term: String,
    /// Scope to search (default: global).
    #[arg(value_name = "SCOPE", value_hint = ValueHint::DirPath, add = ArgValueCandidates::new(scope_candidates))]
    pub scope: Option<String>,
    /// Search every curated/profile scope (global, agents, personas, profiles).
    #[arg(long)]
    pub all: bool,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json` (kept for convenience).
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RecallArgs {
    #[command(subcommand)]
    pub command: RecallCommand,
}

#[derive(Debug, Subcommand)]
pub enum RecallCommand {
    /// Print the bounded profiles/startup index.
    Startup(RecallStartupArgs),
    /// Search curated global notes, optionally including one agent scope.
    OnDemand(RecallOnDemandArgs),
    /// List untrusted candidate notes, optionally for one producer.
    Candidates(RecallCandidatesArgs),
}

#[derive(Debug, Args)]
pub struct RecallStartupArgs {
    /// Maximum allowed startup index size.
    #[arg(long, value_name = "BYTES", default_value_t = 768)]
    pub max_bytes: usize,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json`.
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RecallOnDemandArgs {
    /// Term to find in curated global note content.
    #[arg(value_name = "TERM")]
    pub term: String,
    /// Also search one exact registered non-Claude agent scope.
    #[arg(long, value_name = "ID")]
    pub agent: Option<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json`.
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RecallCandidatesArgs {
    /// Producer ID (for example claude, codex, or hermes).
    #[arg(value_name = "PRODUCER")]
    pub producer: Option<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json`.
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CandidateArgs {
    #[command(subcommand)]
    pub command: CandidateCommand,
}

#[derive(Debug, Subcommand)]
pub enum CandidateCommand {
    /// Add one untrusted proposal under a producer root.
    Add(CandidateAddArgs),
    /// List untrusted proposals, optionally for one producer.
    List(CandidateListArgs),
    /// Preview or apply promotion into curated global memory.
    Promote(CandidatePromoteArgs),
}

#[derive(Debug, Args)]
pub struct CandidateAddArgs {
    /// Producer ID (for example claude, codex, or hermes).
    #[arg(value_name = "PRODUCER")]
    pub producer: String,
    /// Candidate slug (becomes `<slug>.md`).
    #[arg(long, value_name = "SLUG")]
    pub name: String,
    /// Index title (defaults to the slug).
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,
    /// Index hook text (defaults to `untrusted candidate`).
    #[arg(long, value_name = "TEXT")]
    pub hook: Option<String>,
    /// Read candidate body from a file.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath, conflicts_with = "body")]
    pub body_file: Option<String>,
    /// Candidate body text, or `-` to read stdin.
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json`.
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CandidateListArgs {
    /// Producer ID (for example claude, codex, or hermes).
    #[arg(value_name = "PRODUCER")]
    pub producer: Option<String>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json`.
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CandidatePromoteArgs {
    /// Producer ID containing the candidate.
    #[arg(value_name = "PRODUCER")]
    pub producer: String,
    /// Candidate slug without `.md`.
    #[arg(value_name = "SLUG")]
    pub name: String,
    /// Canonical note type (user|feedback|project|reference).
    #[arg(long, value_name = "TYPE")]
    pub r#type: String,
    /// Canonical one-line description.
    #[arg(long, value_name = "TEXT")]
    pub description: String,
    /// Global index title (defaults to the slug).
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,
    /// Global index hook (defaults to the description).
    #[arg(long, value_name = "TEXT")]
    pub hook: Option<String>,
    /// Stamp required `metadata.originSessionId` promotion provenance.
    #[arg(long, value_name = "UUID")]
    pub session_id: String,
    /// Apply the promotion. Omit for a non-mutating preview.
    #[arg(long)]
    pub apply: bool,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json`.
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ArchiveArgs {
    #[command(subcommand)]
    pub command: ArchiveCommand,
}

#[derive(Debug, Subcommand)]
pub enum ArchiveCommand {
    /// List superseded historical notes.
    List(ArchiveListArgs),
    /// Search superseded historical notes explicitly.
    Search(ArchiveSearchArgs),
    /// Preview or apply retirement of one curated global note.
    Retire(ArchiveRetireArgs),
}

#[derive(Debug, Args)]
pub struct ArchiveListArgs {
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json`.
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ArchiveSearchArgs {
    /// Term to find in superseded historical notes.
    #[arg(value_name = "TERM")]
    pub term: String,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json`.
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ArchiveRetireArgs {
    /// Curated global note slug without `.md`.
    #[arg(value_name = "SLUG")]
    pub name: String,
    /// Stable reason the reminder no longer belongs in active recall.
    #[arg(long, value_name = "TEXT")]
    pub reason: String,
    /// Current policy, hook, CLI, config, test, or documentation owner.
    #[arg(long, value_name = "OWNER", required = true)]
    pub superseded_by: Vec<String>,
    /// Archive date in YYYY-MM-DD form.
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub archived_at: String,
    /// Apply the retirement. Omit for a non-mutating preview.
    #[arg(long)]
    pub apply: bool,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
    /// Hidden alias for `--format json`.
    #[arg(long, hide = true, conflicts_with = "format")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CompletionArgs {
    #[arg(value_enum)]
    pub shell: crate::completion::CompletionShell,
}

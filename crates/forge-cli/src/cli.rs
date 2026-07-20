//! Clap derive tree, global flag definitions, and the top-level dispatch
//! entry consumed by `crate::run`.
//!
//! Every subcommand listed in `crates/forge-cli/docs/specs/forge-cli-spec-v1.md` §"Command
//! tree" is declared here, even when the v1 handler is not yet implemented in
//! this sprint. Stubs return a structured `not_implemented` envelope under
//! `SOFTWARE 70` so callers see a stable failure shape rather than a panic.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use nils_common::cli_contract::{OutputFormat, emit_parse_error, exit};

use crate::error::ForgeError;
use crate::ops;
use crate::provider::ProviderHint;

/// Stable binary name used in `schema_version` literals (`cli.forge-cli.*`).
pub const BINARY: &str = "forge-cli";

/// Top-level CLI definition.
#[derive(Parser, Debug)]
#[command(
    name = "forge-cli",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Provider-neutral CLI for remote forge operations (gh / glab wrapper)."
)]
pub struct Cli {
    /// Output format (defaults to text).
    #[arg(long, global = true, value_enum)]
    pub format: Option<OutputFormat>,

    /// Git remote whose URL feeds provider detection (default: `origin`).
    #[arg(long, global = true, default_value = "origin")]
    pub remote: String,

    /// Override auto-detected provider.
    #[arg(long, global = true, value_enum)]
    pub provider: Option<ProviderFlag>,

    /// Override the forge authority (`hostname` or `hostname:port`). With
    /// `--provider`, a syntactically valid custom authority is accepted unless
    /// it is positively identified as the other provider. Without `--provider`,
    /// the authority must identify a supported provider.
    #[arg(long, global = true, value_name = "HOST")]
    pub host: Option<String>,

    /// Override the remote-derived repository path. Accepted shapes: GitHub
    /// `owner/name`; GitLab `group[/subgroup...]/project`; Local `local:<slug>` or
    /// `<slug>`.
    #[arg(long, global = true, value_name = "REPOSITORY")]
    pub repo: Option<String>,

    /// Root directory of the file-backed store used by `--provider local`.
    /// Overrides the `FORGE_CLI_LOCAL_STORE` env var.
    #[arg(long, global = true, value_name = "path")]
    pub store_root: Option<PathBuf>,

    /// Validate and describe the planned operation without applying its
    /// mutation. Read-only preflight calls may still run.
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub dry_run: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Snapshot of the global flags handed to ops without re-parsing.
#[derive(Debug, Clone)]
pub struct GlobalFlags {
    pub format: Option<OutputFormat>,
    pub remote: String,
    pub provider: Option<ProviderFlag>,
    pub host: Option<String>,
    pub repo: Option<String>,
    pub store_root: Option<PathBuf>,
    pub dry_run: bool,
}

impl From<&Cli> for GlobalFlags {
    fn from(cli: &Cli) -> Self {
        Self {
            format: cli.format,
            remote: cli.remote.clone(),
            provider: cli.provider,
            host: cli.host.clone(),
            repo: cli.repo.clone(),
            store_root: cli.store_root.clone(),
            dry_run: cli.dry_run,
        }
    }
}

impl GlobalFlags {
    /// Resolve the output format, defaulting to text.
    pub fn output_format(&self) -> OutputFormat {
        self.format.unwrap_or_default()
    }

    /// Convert the optional `--provider` flag into the typed provider hint.
    pub fn provider_hint(&self) -> ProviderHint {
        match (self.provider, self.host.as_deref()) {
            (Some(ProviderFlag::Github), Some(host)) => {
                ProviderHint::ForcedHost(crate::provider::Provider::GitHub, host.to_string())
            }
            (Some(ProviderFlag::Gitlab), Some(host)) => {
                ProviderHint::ForcedHost(crate::provider::Provider::GitLab, host.to_string())
            }
            (Some(ProviderFlag::Local), Some(host)) => {
                ProviderHint::ForcedHost(crate::provider::Provider::Local, host.to_string())
            }
            (Some(ProviderFlag::Github), None) => {
                ProviderHint::Forced(crate::provider::Provider::GitHub)
            }
            (Some(ProviderFlag::Gitlab), None) => {
                ProviderHint::Forced(crate::provider::Provider::GitLab)
            }
            (Some(ProviderFlag::Local), None) => {
                ProviderHint::Forced(crate::provider::Provider::Local)
            }
            (None, Some(host)) => ProviderHint::Host(host.to_string()),
            (None, None) => ProviderHint::Auto,
        }
    }

    /// True when `--provider local` was passed, i.e. ops should serve calls
    /// from the file-backed store via [`crate::local::LocalRunner`] instead of
    /// spawning a backend binary.
    pub fn is_local(&self) -> bool {
        matches!(self.provider, Some(ProviderFlag::Local))
    }
}

/// Provider override for `--provider`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum ProviderFlag {
    Github,
    Gitlab,
    Local,
}

/// Top-level subcommand tree.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Pull / merge request lifecycle.
    Pr(PrArgs),
    /// Issue lifecycle.
    Issue(IssueArgs),
    /// Personal activity across forge repositories.
    Activity(ActivityArgs),
    /// Repository label catalog audit and ensure operations.
    Label(LabelArgs),
    /// Personal cross-repo work inbox.
    Inbox(InboxArgs),
    /// Full-text / reverse-reference search over issues and PRs.
    ///
    /// `search` complements the other discovery surfaces, which serve distinct
    /// roles: `issue list` / `pr list` filter by structured fields (state,
    /// labels, author) within one repo; `inbox` is the personal cross-repo work
    /// queue; `search` runs free-text (`issues` / `prs`) and reverse-reference
    /// (`refs-to`) queries the structured lists cannot express. GitHub-only in
    /// v1; GitLab and Local return a structured `provider_unsupported` error.
    Search(SearchArgs),
    /// Repository helpers.
    Repo(RepoArgs),
    /// Backend authentication helpers.
    Auth(AuthArgs),
    /// Emit shell-completion scripts.
    Completion(CompletionArgs),
}

#[derive(Args, Debug)]
pub struct PrArgs {
    #[command(subcommand)]
    pub command: Option<PrCommand>,
}

#[derive(Args, Debug)]
pub struct IssueArgs {
    #[command(subcommand)]
    pub command: Option<IssueCommand>,
}

#[derive(Args, Debug)]
pub struct ActivityArgs {
    #[command(subcommand)]
    pub command: Option<ActivityCommand>,
}

#[derive(Args, Debug)]
pub struct LabelArgs {
    #[command(subcommand)]
    pub command: Option<LabelCommand>,
}

#[derive(Args, Debug)]
pub struct InboxArgs {
    #[command(subcommand)]
    pub command: Option<InboxCommand>,
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    #[command(subcommand)]
    pub command: Option<SearchCommand>,
}

#[derive(Args, Debug)]
pub struct RepoArgs {
    #[command(subcommand)]
    pub command: Option<RepoCommand>,
}

/// `repo push-default` arguments.
#[derive(Args, Debug, Clone)]
pub struct RepoPushDefaultArgs {
    /// Commit-ish to deliver. It must resolve to the currently checked-out HEAD.
    #[arg(long, default_value = "HEAD")]
    pub head: String,
    /// Exact remote default-branch SHA observed before authoring the commit.
    #[arg(long = "expected-base", value_name = "SHA")]
    pub expected_base: String,
    /// UTF-8 file describing the user's explicit direct-main authorization.
    #[arg(long = "reason-file", value_name = "PATH")]
    pub reason_file: PathBuf,
}

#[derive(Args, Debug)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: Option<AuthCommand>,
}

#[derive(Args, Debug)]
pub struct CompletionArgs {
    /// Target shell.
    #[arg(value_enum)]
    pub shell: Shell,
}

/// Clap-facing `--kind` enum. Maps 1:1 to [`crate::validations::PrKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum PrKindFlag {
    Feature,
    Bug,
    Chore,
    Docs,
    Ci,
    Refactor,
}

impl PrKindFlag {
    pub fn into_kind(self) -> crate::validations::PrKind {
        match self {
            PrKindFlag::Feature => crate::validations::PrKind::Feature,
            PrKindFlag::Bug => crate::validations::PrKind::Bug,
            PrKindFlag::Chore => crate::validations::PrKind::Chore,
            PrKindFlag::Docs => crate::validations::PrKind::Docs,
            PrKindFlag::Ci => crate::validations::PrKind::Ci,
            PrKindFlag::Refactor => crate::validations::PrKind::Refactor,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PrKindFlag::Feature => "feature",
            PrKindFlag::Bug => "bug",
            PrKindFlag::Chore => "chore",
            PrKindFlag::Docs => "docs",
            PrKindFlag::Ci => "ci",
            PrKindFlag::Refactor => "refactor",
        }
    }
}

/// `--state` filter shared by `pr list` and the close payload normaliser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum PrStateFilter {
    Open,
    Closed,
    Merged,
    All,
}

/// `--state` filter for `issue list`. Issues have no `merged` state, so
/// this enum is intentionally narrower than [`PrStateFilter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum IssueStateFilter {
    Open,
    Closed,
    All,
}

impl IssueStateFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            IssueStateFilter::Open => "open",
            IssueStateFilter::Closed => "closed",
            IssueStateFilter::All => "all",
        }
    }
}

/// Inbox item-kind / reason filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum InboxKindFlag {
    Review,
    Assigned,
    Todo,
    Authored,
    Involved,
}

impl InboxKindFlag {
    pub fn as_str(self) -> &'static str {
        match self {
            InboxKindFlag::Review => "review",
            InboxKindFlag::Assigned => "assigned",
            InboxKindFlag::Todo => "todo",
            InboxKindFlag::Authored => "authored",
            InboxKindFlag::Involved => "involved",
        }
    }
}

/// Inbox item-type filter. Distinct from `--kind` (reason): item-type selects
/// PR/MR-only or issue-only result classes so the CLI can skip irrelevant
/// provider query families before any subprocess runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum InboxItemTypeFlag {
    /// Include PRs/MRs, issues, and classifiable todos (default).
    #[default]
    All,
    /// Include only pull requests / merge requests (and todos that target them).
    Pr,
    /// Include only issues (and todos that target them).
    Issue,
}

impl InboxItemTypeFlag {
    pub fn as_str(self) -> &'static str {
        match self {
            InboxItemTypeFlag::All => "all",
            InboxItemTypeFlag::Pr => "pr",
            InboxItemTypeFlag::Issue => "issue",
        }
    }
}

/// GitLab VPN readiness mode for inbox calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum GitlabVpnModeFlag {
    /// Do not run a GitLab VPN readiness probe (default).
    #[default]
    Off,
    /// Run the probe when configured, but attempt GitLab even when it fails.
    Optional,
    /// Require readiness before any GitLab backend call.
    Required,
}

impl GitlabVpnModeFlag {
    pub fn as_str(self) -> &'static str {
        match self {
            GitlabVpnModeFlag::Off => "off",
            GitlabVpnModeFlag::Optional => "optional",
            GitlabVpnModeFlag::Required => "required",
        }
    }
}

impl PrStateFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            PrStateFilter::Open => "open",
            PrStateFilter::Closed => "closed",
            PrStateFilter::Merged => "merged",
            PrStateFilter::All => "all",
        }
    }
}

/// `pr edit` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.pr.edit` inputs.
#[derive(Args, Debug, Clone)]
pub struct PrEditArgs {
    /// Numeric PR / MR id.
    pub id: u64,
    /// Replace the title (revalidates `title_length`).
    #[arg(long)]
    pub title: Option<String>,
    /// Replace the body / description (revalidates body sections).
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read replacement body from a file. Use `-` for stdin.
    #[arg(long = "body-file", value_name = "PATH")]
    pub body_file: Option<String>,
    /// Re-target the PR / MR to this base branch.
    #[arg(long)]
    pub base: Option<String>,
    /// Add a label (repeatable).
    #[arg(long = "add-label", value_name = "LABEL")]
    pub add_labels: Vec<String>,
    /// Remove a label (repeatable).
    #[arg(long = "remove-label", value_name = "LABEL")]
    pub remove_labels: Vec<String>,
    /// Add a reviewer (repeatable).
    #[arg(long = "add-reviewer", value_name = "USER")]
    pub add_reviewers: Vec<String>,
}

/// `pr comment` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.pr.comment` inputs.
#[derive(Args, Debug, Clone)]
pub struct PrCommentArgs {
    /// Numeric PR / MR id.
    pub id: u64,
    /// Comment body. Mutually exclusive with `--body-file`.
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read comment body from a file. Use `-` for stdin.
    #[arg(long = "body-file", value_name = "PATH")]
    pub body_file: Option<String>,
}

/// Review outcome decision recorded by `pr review`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
#[clap(rename_all = "kebab-case")]
pub enum PrReviewDecision {
    /// Post an informational review outcome comment only.
    CommentsOnly,
    /// Record an approval outcome in the posted review comment.
    Approve,
    /// Record a request-changes outcome in the posted review comment.
    RequestChanges,
}

impl PrReviewDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CommentsOnly => "comments-only",
            Self::Approve => "approve",
            Self::RequestChanges => "request-changes",
        }
    }

    /// GitHub pull-request review `event` for this decision, used by
    /// `pr review --submit-review` to create a native review object.
    pub fn to_github_event(self) -> &'static str {
        match self {
            Self::CommentsOnly => "COMMENT",
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
        }
    }
}

/// `pr review` subcommands that do not post provider-visible review activity.
#[derive(Subcommand, Debug, Clone)]
pub enum PrReviewCommand {
    /// Validate review summary and thread specs before posting.
    Validate(PrReviewValidateArgs),
}

/// `pr review validate` arguments.
#[derive(Args, Debug, Clone)]
pub struct PrReviewValidateArgs {
    /// Numeric PR id. Required with `--check-diff`.
    pub id: Option<u64>,
    /// Review outcome comment body. Mutually exclusive with `--comment-file`.
    #[arg(long, conflicts_with = "comment_file")]
    pub comment: Option<String>,
    /// Read review outcome comment body from a file. Use `-` for stdin.
    #[arg(long = "comment-file", value_name = "PATH")]
    pub comment_file: Option<String>,
    /// Validate a JSON array of actionable review-thread specs.
    #[arg(long = "thread-file", value_name = "PATH")]
    pub thread_file: Option<String>,
    /// Check thread path / line coordinates against the live GitHub PR diff.
    #[arg(long = "check-diff", action = ArgAction::SetTrue)]
    pub check_diff: bool,
}

/// `pr review` arguments. This is a provider posting primitive: callers pass
/// an already-rendered review outcome comment; forge-cli posts it and can
/// mirror a compact activity note to an issue.
#[derive(Args, Debug, Clone)]
pub struct PrReviewArgs {
    #[command(subcommand)]
    pub command: Option<PrReviewCommand>,
    /// Numeric PR / MR id.
    pub id: Option<u64>,
    /// Outcome decision to record in the comment metadata.
    #[arg(long, value_enum, default_value_t = PrReviewDecision::CommentsOnly)]
    pub decision: PrReviewDecision,
    /// Review outcome comment body. Mutually exclusive with `--comment-file`.
    #[arg(long, conflicts_with = "comment_file")]
    pub comment: Option<String>,
    /// Read review outcome comment body from a file. Use `-` for stdin.
    #[arg(long = "comment-file", value_name = "PATH")]
    pub comment_file: Option<String>,
    /// Review lens name to include in output and issue mirror metadata.
    #[arg(long = "lens", value_name = "LENS")]
    pub lenses: Vec<String>,
    /// Issue number that should receive the optional activity mirror.
    #[arg(long, value_name = "ISSUE_NUMBER")]
    pub issue: Option<u64>,
    /// Mirror a compact activity note to `--issue` (required; omitting it
    /// returns the `issue_required` envelope at runtime).
    // Deliberately no clap `requires = "issue"`: the op returns the documented
    // `DATA 65` `issue_required` envelope at runtime so JSON consumers can
    // branch on the error kind instead of hitting a clap parse-time error.
    #[arg(long = "mirror-issue", action = ArgAction::SetTrue)]
    pub mirror_issue: bool,
    /// Submit a native provider review event instead of posting an outcome
    /// comment. On GitHub this POSTs `.../pulls/<id>/reviews`, creating the
    /// `#pullrequestreview-` object; `--decision` maps to the review event
    /// (comments-only→COMMENT, approve→APPROVE, request-changes→REQUEST_CHANGES).
    /// The review is authored by the invoking token's identity (e.g. a reviewer
    /// bot via FORGE_BOT_PROFILE). A body is required for COMMENT and
    /// REQUEST_CHANGES and optional for APPROVE. GitHub-only in v1.
    #[arg(long = "submit-review", action = ArgAction::SetTrue)]
    pub submit_review: bool,
    /// Full PR head SHA that was reviewed. Required with `--submit-review` and
    /// bound to both the pending-review preflight and provider mutation.
    #[arg(
        long = "expected-head",
        value_name = "SHA",
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    pub expected_head: Option<String>,
    /// Create or validate review threads from a JSON array of actionable
    /// findings (max 50 threads; 16 KiB body each). Requires --submit-review
    /// when posting; may be inherited by `validate` without posting. Omit this
    /// for summary-only reviews. Repair findings, then resolve via
    /// `pr review-threads resolve`.
    #[arg(long = "thread-file", value_name = "PATH")]
    pub thread_file: Option<String>,
}

/// `pr comments` arguments.
#[derive(Args, Debug, Clone)]
pub struct PrCommentsArgs {
    /// Numeric PR / MR id.
    pub id: u64,
}

/// `pr ready` arguments.
#[derive(Args, Debug, Clone)]
pub struct PrReadyArgs {
    /// Numeric PR / MR id.
    pub id: u64,
}

/// `pr review-threads` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.pr.review-threads` inputs.
///
/// `pr review-threads` is a clean subcommand group: `list <id>` reads, while
/// `resolve` / `reply` are the GitHub-first write surfaces. A subcommand is
/// required — there is no bare positional, so clap's zsh completion routes the
/// subcommand at `$line[1]` (the workspace completion-parity audit assumes a
/// clean group; a leading positional would shift it to `$line[2]`).
#[derive(Args, Debug, Clone)]
pub struct PrReviewThreadsArgs {
    #[command(subcommand)]
    pub command: ReviewThreadsCommand,
}

/// Resolved list arguments handed to the read op (`pr review-threads list`
/// read surface).
#[derive(Debug, Clone)]
pub struct PrReviewThreadsListArgs {
    /// Numeric PR / MR id.
    pub id: u64,
}

/// `pr review-threads` subtree: read (`list`) plus the GitHub-first write
/// surfaces (`resolve`, `reply`).
#[derive(Subcommand, Debug, Clone)]
pub enum ReviewThreadsCommand {
    /// List review threads attached to a PR / MR with their resolved state.
    List {
        /// Numeric PR / MR id.
        id: u64,
    },
    /// Resolve a review thread, optionally posting a reply first (GitHub).
    Resolve(PrReviewThreadResolveArgs),
    /// Reply to a review thread without resolving it (GitHub).
    Reply(PrReviewThreadReplyArgs),
}

/// `pr review-threads resolve` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.pr.review-threads.resolve` inputs.
#[derive(Args, Debug, Clone)]
pub struct PrReviewThreadResolveArgs {
    /// Numeric PR / MR id.
    pub id: u64,
    /// Thread node id to resolve (GitHub `PRRT_...`).
    #[arg(long, value_name = "THREAD_ID")]
    pub thread: String,
    /// Optional reply body posted before resolving. Mutually exclusive with
    /// `--note-file`.
    #[arg(long, conflicts_with = "note_file")]
    pub note: Option<String>,
    /// Read the optional reply body from a file. Use `-` for stdin.
    #[arg(long = "note-file", value_name = "PATH")]
    pub note_file: Option<String>,
}

/// `pr review-threads reply` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.pr.review-threads.reply` inputs.
#[derive(Args, Debug, Clone)]
pub struct PrReviewThreadReplyArgs {
    /// Numeric PR / MR id.
    pub id: u64,
    /// Thread node id to reply to (GitHub `PRRT_...`).
    #[arg(long, value_name = "THREAD_ID")]
    pub thread: String,
    /// Reply body. Mutually exclusive with `--body-file`.
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read the reply body from a file. Use `-` for stdin.
    #[arg(long = "body-file", value_name = "PATH")]
    pub body_file: Option<String>,
}

/// `pr tasks` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.pr.tasks` inputs.
#[derive(Args, Debug, Clone)]
pub struct PrTasksArgs {
    /// Numeric PR / MR id.
    pub id: u64,
}

/// `pr merge` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.pr.merge` inputs.
#[derive(Args, Debug, Clone)]
pub struct PrMergeArgs {
    /// Numeric PR / MR id.
    pub id: u64,
    /// Compare-and-swap head verified by the caller before merge.
    #[arg(
        long = "expected-head",
        value_name = "SHA",
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    pub expected_head_sha: Option<String>,
    /// Merge method override. When omitted, falls back to
    /// `.forge-cli.toml [merge].method` then the spec default `squash`.
    #[arg(long, value_enum)]
    pub method: Option<MergeMethodFlag>,
    /// Skip the post-merge branch deletion (`--delete-branch` on gh,
    /// `--remove-source-branch` on glab). Mutually exclusive with the
    /// implicit-true default; flagging both is `keep_branch_conflict`.
    #[arg(long = "keep-branch", action = ArgAction::SetTrue)]
    pub keep_branch: bool,
    /// Allow merges where the PR's base is not the repo's default branch.
    /// Without this flag, mismatched bases trigger `default_branch_protected`.
    #[arg(long = "allow-non-default-base", action = ArgAction::SetTrue)]
    pub allow_non_default_base: bool,
    /// Merge despite unresolved review threads. Without this flag, any
    /// non-outdated unresolved thread (bot or human) triggers
    /// `unresolved_review_threads`. Requires `--allow-unresolved-threads-reason`.
    #[arg(
        long = "allow-unresolved-threads",
        action = ArgAction::SetTrue,
        requires = "allow_unresolved_threads_reason"
    )]
    pub allow_unresolved_threads: bool,
    /// Required when `--allow-unresolved-threads` is set. Non-empty free-form
    /// text describing why the unresolved threads are safe to merge past; the
    /// reason is recorded in the merge envelope payload.
    #[arg(
        long = "allow-unresolved-threads-reason",
        value_name = "TEXT",
        requires = "allow_unresolved_threads",
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    pub allow_unresolved_threads_reason: Option<String>,
    /// Override `[review_convergence].require`. Passing the flag without a
    /// value enables it; use `--review-convergence=false` to disable a repo or
    /// user-global opt-in for this invocation.
    #[arg(
        long = "review-convergence",
        action = ArgAction::Set,
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    pub review_convergence: Option<bool>,
    /// Merge despite unchecked task-list items in the PR/MR description.
    /// Without this flag, any unchecked `- [ ]` item triggers
    /// `unchecked_task_items`. Requires `--allow-unchecked-tasks-reason`.
    #[arg(
        long = "allow-unchecked-tasks",
        action = ArgAction::SetTrue,
        requires = "allow_unchecked_tasks_reason"
    )]
    pub allow_unchecked_tasks: bool,
    /// Required when `--allow-unchecked-tasks` is set. Non-empty free-form
    /// text describing why the unchecked items are safe to merge past; the
    /// reason is recorded in the merge envelope payload.
    #[arg(
        long = "allow-unchecked-tasks-reason",
        value_name = "TEXT",
        requires = "allow_unchecked_tasks",
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    pub allow_unchecked_tasks_reason: Option<String>,
}

/// Native provider review summaries for one pull request.
#[derive(Args, Debug, Clone)]
pub struct PrReviewsArgs {
    /// Numeric pull request id.
    pub id: u64,
}

/// `pr pending-review` recovery group.
#[derive(Args, Debug, Clone)]
pub struct PrPendingReviewArgs {
    #[command(subcommand)]
    pub command: PrPendingReviewCommand,
}

/// Authenticated recovery actions for provider-valid pending reviews.
#[derive(Subcommand, Debug, Clone)]
pub enum PrPendingReviewCommand {
    /// Read one complete pending-review snapshot and immutable digest.
    Inspect(PrPendingReviewInspectArgs),
    /// Resume and submit an exact receipt-bound review transaction.
    ResumeSubmit(PrPendingReviewResumeSubmitArgs),
    /// Guardedly submit an unmarked pending review.
    Submit(PrPendingReviewSubmitArgs),
    /// Destructively discard one exact pending review.
    Discard(PrPendingReviewDiscardArgs),
    /// Verify and delete one exact pending review node.
    Delete(PrPendingReviewDeleteArgs),
}

#[derive(Args, Debug, Clone)]
pub struct PrPendingReviewInspectArgs {
    pub id: u64,
    #[arg(long, value_name = "REVIEW_ID", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub review: String,
}

#[derive(Args, Debug, Clone)]
pub struct PrPendingReviewResumeSubmitArgs {
    pub id: u64,
    #[arg(long, value_name = "REVIEW_ID", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub review: String,
    #[arg(long = "review-run-id", value_name = "DIGEST", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub review_run_id: String,
    #[arg(long = "expected-head", value_name = "SHA", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub expected_head: String,
    #[arg(long = "expected-commit", value_name = "SHA", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub expected_commit: String,
    #[arg(long = "expected-snapshot", value_name = "DIGEST", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub expected_snapshot: String,
    #[arg(long, value_enum)]
    pub decision: PrReviewDecision,
}

#[derive(Args, Debug, Clone)]
pub struct PrPendingReviewSubmitArgs {
    pub id: u64,
    #[arg(long, value_name = "REVIEW_ID", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub review: String,
    #[arg(long = "expected-head", value_name = "SHA", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub expected_head: String,
    #[arg(long = "expected-commit", value_name = "SHA", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub expected_commit: String,
    #[arg(long = "expected-snapshot", value_name = "DIGEST", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub expected_snapshot: String,
    #[arg(long, value_enum)]
    pub decision: PrReviewDecision,
    #[arg(long = "confirm-unmarked-submit", action = ArgAction::SetTrue, required = true)]
    pub confirm_unmarked_submit: bool,
}

#[derive(Args, Debug, Clone)]
pub struct PrPendingReviewDiscardArgs {
    pub id: u64,
    #[arg(long, value_name = "REVIEW_ID", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub review: String,
    #[arg(long = "expected-head", value_name = "SHA", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub expected_head: String,
    #[arg(long = "expected-commit", value_name = "SHA", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub expected_commit: String,
    #[arg(long = "expected-snapshot", value_name = "DIGEST", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub expected_snapshot: String,
    #[arg(long = "confirm-discard", action = ArgAction::SetTrue, required = true)]
    pub confirm_discard: bool,
    #[arg(long = "confirm-inline-content-loss", action = ArgAction::SetTrue)]
    pub confirm_inline_content_loss: bool,
}

/// `pr pending-review delete` arguments.
#[derive(Args, Debug, Clone)]
pub struct PrPendingReviewDeleteArgs {
    /// Numeric pull request id that must own the pending review.
    pub id: u64,
    /// Pending review node id (`PRR_...`) returned by `pr reviews`.
    #[arg(
        long,
        value_name = "REVIEW_ID",
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    pub review: String,
    /// Pull-request head that must still be current immediately before deletion.
    #[arg(
        long = "expected-head",
        value_name = "SHA",
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    pub expected_head: String,
    /// Commit to which the pending review must still be bound.
    #[arg(
        long = "expected-commit",
        value_name = "SHA",
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    pub expected_commit: String,
    /// Exact pending-review body expected before deletion.
    #[arg(
        long = "expected-body",
        conflicts_with = "expected_body_file",
        required_unless_present = "expected_body_file"
    )]
    pub expected_body: Option<String>,
    /// Read the exact expected pending-review body from a file. Use `-` for stdin.
    #[arg(
        long = "expected-body-file",
        value_name = "PATH",
        conflicts_with = "expected_body",
        required_unless_present = "expected_body"
    )]
    pub expected_body_file: Option<String>,
    /// Confirm that the exact guarded pending review is an abandoned draft.
    #[arg(long = "confirm-abandoned", action = ArgAction::SetTrue, required = true)]
    pub confirm_abandoned: bool,
}

/// CLI-facing merge method enum so clap can render `--method squash|merge|rebase`
/// without leaking the config crate's `MergeMethod` into the CLI layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum MergeMethodFlag {
    Squash,
    Merge,
    Rebase,
}

impl MergeMethodFlag {
    pub fn into_method(self) -> crate::config::MergeMethod {
        match self {
            Self::Squash => crate::config::MergeMethod::Squash,
            Self::Merge => crate::config::MergeMethod::Merge,
            Self::Rebase => crate::config::MergeMethod::Rebase,
        }
    }
}

/// `pr checks` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.pr.checks` inputs.
#[derive(Args, Debug, Clone)]
pub struct PrChecksArgs {
    /// Numeric id or branch name (GitHub accepts both natively; GitLab
    /// resolves numeric ids via `mr view` to fetch the source branch).
    pub id: String,
    /// Restrict the gating decision to required checks (default `true`).
    /// Non-required checks are always reported in `data.checks` regardless.
    #[arg(
        long = "required-only",
        action = ArgAction::Set,
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
    )]
    pub required_only: bool,
}

/// `pr wait-checks` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.pr.wait-checks` inputs.
#[derive(Args, Debug, Clone)]
pub struct PrWaitChecksArgs {
    /// Numeric id or branch name.
    pub id: String,
    /// Total budget before declaring `checks_timeout` (default `30m`).
    #[arg(long, value_parser = parse_duration, default_value = "30m")]
    pub timeout: Duration,
    /// Pause between polls (default `20s`).
    #[arg(long, value_parser = parse_duration, default_value = "20s")]
    pub interval: Duration,
    /// Restrict the gating decision to required checks (default `true`).
    #[arg(
        long = "required-only",
        action = ArgAction::Set,
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
    )]
    pub required_only: bool,
}

/// Parse a duration string like `30m`, `20s`, `5h`, `500ms`. Accepts bare
/// integers as seconds.
pub(crate) fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("duration cannot be empty".into());
    }
    // Split numeric prefix from unit suffix.
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let value: u64 = num
        .parse()
        .map_err(|_| format!("invalid number in {s:?}"))?;
    let dur = match unit {
        "" | "s" => Duration::from_secs(value),
        "ms" => Duration::from_millis(value),
        "m" => Duration::from_secs(
            value
                .checked_mul(60)
                .ok_or_else(|| format!("duration overflows in {s:?}"))?,
        ),
        "h" => Duration::from_secs(
            value
                .checked_mul(3600)
                .ok_or_else(|| format!("duration overflows in {s:?}"))?,
        ),
        other => return Err(format!("unknown duration unit {other:?} in {s:?}")),
    };
    Ok(dur)
}

/// `pr list` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.pr.list` inputs.
#[derive(Args, Debug, Clone)]
pub struct PrListArgs {
    /// Filter by state (default: open).
    #[arg(long, value_enum, default_value_t = PrStateFilter::Open)]
    pub state: PrStateFilter,
    /// Filter by author handle.
    #[arg(long)]
    pub author: Option<String>,
    /// Filter by head / source branch.
    #[arg(long)]
    pub head: Option<String>,
    /// Cap the number of returned PRs (default: 30).
    #[arg(long, default_value_t = 30)]
    pub limit: u32,
}

/// `pr create` arguments. Maps to the inputs declared in
/// `crates/forge-cli/docs/specs/forge-cli-ops-v1.yaml::operations.pr.create`.
#[derive(Args, Debug, Clone)]
pub struct PrCreateArgs {
    /// Source branch (defaults to the current branch).
    #[arg(long)]
    pub head: Option<String>,
    /// Target / base branch (defaults to the repo's default branch).
    #[arg(long)]
    pub base: Option<String>,
    /// PR / MR title (required).
    #[arg(long)]
    pub title: String,
    /// PR / MR body / description text. Mutually exclusive with `--body-file`.
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read PR / MR body from a file. Use `-` to read from stdin.
    #[arg(long = "body-file", value_name = "PATH")]
    pub body_file: Option<String>,
    /// PR / MR kind (selects branch-prefix rule). Required.
    #[arg(long, value_enum)]
    pub kind: PrKindFlag,
    /// Open as ready-for-review instead of draft (default: open as draft).
    #[arg(long = "no-draft", action = ArgAction::SetTrue)]
    pub no_draft: bool,
    /// Add a reviewer (repeatable).
    #[arg(long = "reviewer", value_name = "USER")]
    pub reviewers: Vec<String>,
    /// Add a label (repeatable).
    #[arg(long = "label", value_name = "LABEL")]
    pub labels: Vec<String>,
    /// Validate labels against this catalog when `--strict-labels` is set.
    #[arg(long = "label-catalog", value_name = "PATH")]
    pub label_catalog: Option<String>,
    /// Fail when selected labels are missing, not applicable, or conflict.
    #[arg(long = "strict-labels", action = ArgAction::SetTrue)]
    pub strict_labels: bool,
    /// Directory holding a `test-first-evidence` record. Verified by the
    /// test-first gate when `[test_first].require` resolves true for a
    /// feature/bug PR. Requires a complete v2 record with a testable
    /// classification, durable pre-fix evidence, scoped final validation, a
    /// residual-gap declaration, and a bound baseline/delivery subject
    /// matching the current checkout.
    #[arg(long = "test-first-evidence", value_name = "DIR")]
    pub test_first_evidence: Option<String>,
}

/// `pr` subtree.
#[derive(Subcommand, Debug)]
pub enum PrCommand {
    /// Open a draft pull / merge request from the current branch.
    Create(PrCreateArgs),
    /// Fetch a single PR / MR with normalised fields.
    View {
        /// Numeric id or branch name.
        id: String,
    },
    /// List PRs / MRs.
    List(PrListArgs),
    /// Mutate PR / MR title, body, base, labels, reviewers.
    Edit(PrEditArgs),
    /// Append a comment to a PR / MR.
    Comment(PrCommentArgs),
    /// Post a review outcome comment and optionally mirror it to an issue.
    Review(PrReviewArgs),
    /// List the issue-style comment stream attached to a PR / MR.
    Comments(PrCommentsArgs),
    /// Promote a draft PR / MR to ready-for-review.
    Ready(PrReadyArgs),
    /// List review threads attached to a PR / MR with their resolved state.
    ReviewThreads(PrReviewThreadsArgs),
    /// List native review summaries, classified against the current head.
    Reviews(PrReviewsArgs),
    /// Recover an authenticated actor's pending native review.
    PendingReview(PrPendingReviewArgs),
    /// List GFM task-list items in the PR / MR description with their state.
    Tasks(PrTasksArgs),
    /// Merge a ready PR / MR.
    Merge(PrMergeArgs),
    /// Close a PR / MR without merging.
    Close {
        /// Numeric id.
        id: u64,
    },
    /// One-shot snapshot of PR / MR check state.
    Checks(PrChecksArgs),
    /// Block until every required check reaches a terminal state.
    WaitChecks(PrWaitChecksArgs),
    /// End-to-end "open draft (or adopt the branch's open PR) → CI green →
    /// ready → merge" macro.
    Deliver(PrDeliverArgs),
}

/// `label` subtree.
#[derive(Subcommand, Debug)]
pub enum LabelCommand {
    /// List repository labels through the selected provider backend.
    List(LabelListArgs),
    /// Compare repository labels against a machine-readable catalog.
    Audit(LabelAuditArgs),
    /// Create missing labels and optionally update color / description drift.
    Ensure(LabelEnsureArgs),
}

#[derive(Args, Debug, Clone)]
pub struct LabelListArgs {
    /// Maximum labels to fetch from the provider.
    #[arg(long, default_value_t = 200)]
    pub limit: u32,
}

#[derive(Args, Debug, Clone)]
pub struct LabelAuditArgs {
    /// Path to the shared forge label catalog.
    #[arg(long, value_name = "PATH")]
    pub catalog: String,
    /// Maximum labels to fetch from the provider.
    #[arg(long, default_value_t = 200)]
    pub limit: u32,
}

#[derive(Args, Debug, Clone)]
pub struct LabelEnsureArgs {
    /// Path to the shared forge label catalog.
    #[arg(long, value_name = "PATH")]
    pub catalog: String,
    /// Update color / description drift on existing labels.
    #[arg(long = "update-existing", action = ArgAction::SetTrue)]
    pub update_existing: bool,
    /// Maximum labels to fetch from the provider.
    #[arg(long, default_value_t = 200)]
    pub limit: u32,
}

/// `pr deliver` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.pr.deliver` inputs.
#[derive(Args, Debug, Clone)]
#[command(
    after_help = "With --dry-run this runs a faithful local preflight: it evaluates the non-mutating lock-down rules (branch name, branch/kind match, title length, body Summary/Test plan sections, clean worktree, head pushed) and reports each verdict in data.local_preflight[] without invoking any provider backend, so one dry-run predicts whether the real run's local gates will pass."
)]
pub struct PrDeliverArgs {
    /// PR / MR kind (selects branch-prefix rule). Required.
    #[arg(long, value_enum)]
    pub kind: PrKindFlag,
    /// PR / MR title (required).
    #[arg(long)]
    pub title: String,
    /// PR / MR body / description text. Mutually exclusive with `--body-file`.
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read PR / MR body from a file. Use `-` to read from stdin.
    #[arg(long = "body-file", value_name = "PATH")]
    pub body_file: Option<String>,
    /// Source branch (defaults to the current branch).
    #[arg(long)]
    pub head: Option<String>,
    /// Target / base branch (defaults to the repo's default branch).
    #[arg(long)]
    pub base: Option<String>,
    /// Merge method (default: squash).
    #[arg(long, value_enum, default_value_t = MergeMethodFlag::Squash)]
    pub method: MergeMethodFlag,
    /// Add a reviewer (repeatable).
    #[arg(long = "reviewer", value_name = "USER")]
    pub reviewers: Vec<String>,
    /// Add a label to the created PR / MR (repeatable).
    #[arg(long = "label", value_name = "LABEL")]
    pub labels: Vec<String>,
    /// Validate labels against this catalog when `--strict-labels` is set.
    #[arg(long = "label-catalog", value_name = "PATH")]
    pub label_catalog: Option<String>,
    /// Fail when selected labels are missing, not applicable, or conflict.
    #[arg(long = "strict-labels", action = ArgAction::SetTrue)]
    pub strict_labels: bool,
    /// Directory holding a `test-first-evidence` record, forwarded to the
    /// create step and verified by the test-first gate when
    /// `[test_first].require` resolves true for a feature/bug PR. Requires a
    /// complete v2 record with a testable classification and a bound
    /// baseline/delivery subject matching the current checkout.
    #[arg(long = "test-first-evidence", value_name = "DIR")]
    pub test_first_evidence: Option<String>,
    /// Cumulative CI-wait budget before declaring `checks_timeout` (default `30m`).
    #[arg(long, value_parser = parse_duration, default_value = "30m")]
    pub timeout: Duration,
    /// Stop after `pr.wait-checks` — do not promote to ready or merge.
    #[arg(long = "no-merge", action = ArgAction::SetTrue)]
    pub no_merge: bool,
    /// Skip the post-merge linked-issue closeout. By default, after a
    /// successful merge the macro deterministically closes any still-open
    /// issue referenced by a `Closes/Fixes #N` closing keyword (GitHub
    /// surfaces these via `closingIssuesReferences`), so delivery does not
    /// depend on GitHub's asynchronous auto-close latency. Plan-tracking PRs
    /// use non-closing `Refs #N` (empty `closingIssuesReferences`) and are
    /// unaffected either way.
    #[arg(long = "no-issue-closeout", action = ArgAction::SetTrue)]
    pub no_issue_closeout: bool,
    /// Allow merges where the PR's base is not the repo's default branch.
    #[arg(long = "allow-non-default-base", action = ArgAction::SetTrue)]
    pub allow_non_default_base: bool,
    /// Merge despite unresolved review threads. Without this flag, any
    /// non-outdated unresolved thread (bot or human) triggers
    /// `unresolved_review_threads` at the merge step. Requires
    /// `--allow-unresolved-threads-reason`.
    #[arg(
        long = "allow-unresolved-threads",
        action = ArgAction::SetTrue,
        requires = "allow_unresolved_threads_reason"
    )]
    pub allow_unresolved_threads: bool,
    /// Required when `--allow-unresolved-threads` is set. Non-empty free-form
    /// text describing why the unresolved threads are safe to merge past; the
    /// reason is recorded in the merge-step payload.
    #[arg(
        long = "allow-unresolved-threads-reason",
        value_name = "TEXT",
        requires = "allow_unresolved_threads",
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    pub allow_unresolved_threads_reason: Option<String>,
    /// Override `[review_convergence].require` for the merge phase. Passing
    /// the flag without a value enables it; `--review-convergence=false`
    /// disables a configured opt-in for this invocation.
    #[arg(
        long = "review-convergence",
        action = ArgAction::Set,
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    pub review_convergence: Option<bool>,
    /// Merge despite unchecked task-list items in the PR/MR description.
    /// Without this flag, any unchecked `- [ ]` item triggers
    /// `unchecked_task_items` at the merge step. Requires
    /// `--allow-unchecked-tasks-reason`.
    #[arg(
        long = "allow-unchecked-tasks",
        action = ArgAction::SetTrue,
        requires = "allow_unchecked_tasks_reason"
    )]
    pub allow_unchecked_tasks: bool,
    /// Required when `--allow-unchecked-tasks` is set. Non-empty free-form
    /// text describing why the unchecked items are safe to merge past; the
    /// reason is recorded in the merge-step payload.
    #[arg(
        long = "allow-unchecked-tasks-reason",
        value_name = "TEXT",
        requires = "allow_unchecked_tasks",
        value_parser = clap::builder::NonEmptyStringValueParser::new()
    )]
    pub allow_unchecked_tasks_reason: Option<String>,
}

/// `issue` subtree.
#[derive(Subcommand, Debug)]
pub enum IssueCommand {
    /// Open a new issue.
    Create(IssueCreateArgs),
    /// Fetch a single issue.
    View(IssueViewArgs),
    /// List issues filtered by state / labels / author / assignee.
    List(IssueListArgs),
    /// Mutate an issue.
    Edit(IssueEditArgs),
    /// Append a comment to an issue.
    Comment(IssueCommentArgs),
    /// Close an issue.
    Close(IssueCloseArgs),
    /// Reopen a closed issue.
    Reopen {
        /// Numeric id.
        id: u64,
    },
}

/// `issue close` arguments.
#[derive(Args, Debug, Clone)]
pub struct IssueCloseArgs {
    /// Numeric issue id.
    pub id: u64,
    /// State reason recorded on close (GitHub only). `completed` marks the
    /// issue done; `not planned` marks it abandoned. GitLab / Local have no
    /// equivalent and silently ignore this flag.
    #[arg(long, value_enum)]
    pub reason: Option<CloseReasonFlag>,
}

/// `--reason` enum for `issue close`. GitHub-only; maps to
/// `gh issue close --reason completed|"not planned"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CloseReasonFlag {
    #[value(name = "completed")]
    Completed,
    #[value(name = "not planned")]
    NotPlanned,
}

impl CloseReasonFlag {
    /// Value forwarded to `gh issue close --reason <r>`.
    pub fn as_str(self) -> &'static str {
        match self {
            CloseReasonFlag::Completed => "completed",
            CloseReasonFlag::NotPlanned => "not planned",
        }
    }
}

/// `activity` subtree.
#[derive(Subcommand, Debug)]
pub enum ActivityCommand {
    /// Search recent GitHub commits authored by a user.
    Commits(ActivityCommitsArgs),
    /// List recent GitHub user activity events.
    Events(ActivityEventsArgs),
    /// List recent repository/project activity.
    Feed(ActivityFeedArgs),
    /// Summarize GitHub commit contributions by repository.
    Summary(ActivitySummaryArgs),
}

/// `activity commits` arguments.
#[derive(Args, Debug, Clone)]
pub struct ActivityCommitsArgs {
    /// GitHub login to inspect. Use @me for the authenticated account.
    #[arg(long, default_value = "@me", value_name = "LOGIN")]
    pub user: String,
    /// Only include commits authored at or after this date/datetime.
    #[arg(long, value_name = "DATE_OR_DATETIME")]
    pub since: Option<String>,
    /// Cap the number of returned commits (default: 30).
    #[arg(long, default_value_t = 30)]
    pub limit: u32,
}

/// `activity events` arguments.
#[derive(Args, Debug, Clone)]
pub struct ActivityEventsArgs {
    /// GitHub login to inspect. Use @me for the authenticated account.
    #[arg(long, default_value = "@me", value_name = "LOGIN")]
    pub user: String,
    /// Cap the number of returned events (default: 30).
    #[arg(long, default_value_t = 30)]
    pub limit: u32,
    /// Use the public-events endpoint even for @me.
    #[arg(long = "public-only", action = ArgAction::SetTrue)]
    pub public_only: bool,
}

/// `activity feed` arguments.
#[derive(Args, Debug, Clone)]
pub struct ActivityFeedArgs {
    /// Only include activity at or after this date/datetime.
    #[arg(long, value_name = "DATE_OR_DATETIME")]
    pub since: Option<String>,
    /// Cap the number of returned activity records (default: 30).
    #[arg(long, default_value_t = 30)]
    pub limit: u32,
}

/// `activity summary` arguments.
#[derive(Args, Debug, Clone)]
pub struct ActivitySummaryArgs {
    /// GitHub login to inspect. Use @me for the authenticated account.
    #[arg(long, default_value = "@me", value_name = "LOGIN")]
    pub user: String,
    /// Only count contributions at or after this date/datetime.
    #[arg(long, value_name = "DATE_OR_DATETIME")]
    pub since: Option<String>,
    /// Maximum repositories to include in the summary (default: 25).
    #[arg(long, default_value_t = 25)]
    pub limit: u32,
}

/// `search` subtree. Free-text / reverse-reference query over forge issues and
/// PRs, scoped to a single repository. GitHub-only in v1; GitLab / Local hit a
/// structured `provider_unsupported` seam.
#[derive(Subcommand, Debug)]
pub enum SearchCommand {
    /// Full-text search over issues (`gh search issues`).
    Issues(SearchQueryArgs),
    /// Full-text search over pull requests (`gh search prs`).
    Prs(SearchQueryArgs),
    /// List issues / PRs that reference a ref via cross-reference events.
    #[command(name = "refs-to")]
    RefsTo(SearchRefsToArgs),
}

/// Arguments for `search refs-to <ref>`.
#[derive(Args, Debug, Clone)]
pub struct SearchRefsToArgs {
    /// Target issue / PR to find references to. Accepts a GitHub URL,
    /// `owner/name#number`, or `#number` / `number` (repo from context).
    #[arg(value_name = "REF")]
    pub reference: String,
    /// Cap the number of cross-reference events scanned (default: 30).
    #[arg(long, default_value_t = 30)]
    pub limit: u32,
}

/// Shared arguments for `search issues` / `search prs`.
#[derive(Args, Debug, Clone)]
pub struct SearchQueryArgs {
    /// Free-text query passed to the provider's search primitive.
    #[arg(value_name = "QUERY")]
    pub query: String,
    /// Restrict matching to these fields (comma-separated or repeatable).
    /// Defaults to title, body, and comments.
    #[arg(
        long = "match",
        value_enum,
        value_delimiter = ',',
        default_values_t = [SearchMatchField::Title, SearchMatchField::Body, SearchMatchField::Comments],
        value_name = "FIELD"
    )]
    pub match_fields: Vec<SearchMatchField>,
    /// Cap the number of returned results (default: 30).
    #[arg(long, default_value_t = 30)]
    pub limit: u32,
}

/// Field a `search` query may match on. Mirrors `gh search --match`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum SearchMatchField {
    Title,
    Body,
    Comments,
}

impl SearchMatchField {
    /// Stable lower-case rendering used in the `gh search --match` value and
    /// in the envelope's `match_fields`.
    pub fn as_str(self) -> &'static str {
        match self {
            SearchMatchField::Title => "title",
            SearchMatchField::Body => "body",
            SearchMatchField::Comments => "comments",
        }
    }
}

/// `inbox` subtree.
#[derive(Subcommand, Debug)]
pub enum InboxCommand {
    /// Summarize bounded personal work counts.
    Status(InboxQueryArgs),
    /// List normalized inbox items.
    List(InboxQueryArgs),
    /// Return ranked next-action candidates.
    Next(InboxNextArgs),
}

/// Shared inbox query arguments for list/status.
#[derive(Args, Debug, Clone)]
pub struct InboxQueryArgs {
    /// GitLab host for inbox API calls. Defaults from FORGE_CLI_INBOX_GITLAB_HOST,
    /// GitLab remote inference, then gitlab.com.
    #[arg(long = "gitlab-host", value_name = "HOST")]
    pub gitlab_host: Option<String>,
    /// Restrict inbox reasons/kinds. Repeatable. Defaults to review,
    /// assigned, todo, and authored; `involved` is opt-in because it can be
    /// broad on GitHub.
    #[arg(long = "kind", value_enum)]
    pub kinds: Vec<InboxKindFlag>,
    /// Restrict inbox by item type (PR/MR vs issue). `--kind` filters reasons
    /// (review/assigned/todo/authored/involved); `--item-type` filters result
    /// classes so PR-only and issue-only modes skip irrelevant provider
    /// queries. Defaults to all.
    #[arg(long = "item-type", value_enum, default_value_t = InboxItemTypeFlag::All)]
    pub item_type: InboxItemTypeFlag,
    /// Per-provider, per-query-family bounded result limit (default: 30).
    #[arg(long, default_value_t = 30)]
    pub limit: u32,
    /// GitLab VPN readiness policy for inbox calls.
    #[arg(long = "gitlab-vpn", value_enum)]
    pub gitlab_vpn: Option<GitlabVpnModeFlag>,
    /// GitLab VPN readiness check: `tcp:<host>:<port>`, `cmd:<program>`, or `openvpn`.
    #[arg(long = "gitlab-vpn-check", value_name = "CHECK")]
    pub gitlab_vpn_check: Option<String>,
    /// GitLab VPN readiness check timeout (default: 2s).
    #[arg(long = "gitlab-vpn-check-timeout", value_parser = parse_duration)]
    pub gitlab_vpn_check_timeout: Option<Duration>,
    /// Local OpenVPN profile path for probe diagnostics. Redacted from output.
    #[arg(long = "gitlab-openvpn-profile", value_name = "PATH")]
    pub gitlab_openvpn_profile: Option<PathBuf>,
    /// Per-backend-call timeout for GitLab inbox calls (default: 20s; 0 disables).
    #[arg(long = "provider-timeout", value_parser = parse_duration)]
    pub provider_timeout: Option<Duration>,
    /// Fail when any selected inbox provider fails.
    #[arg(long = "strict-providers", action = ArgAction::SetTrue)]
    pub strict_providers: bool,
    /// Include recent cached items when a selected provider is unavailable or times out.
    #[arg(long = "cache-fallback", action = ArgAction::SetTrue)]
    pub cache_fallback: bool,
    /// Maximum age for opt-in cached fallback items (default: 30m).
    #[arg(long = "cache-max-age", value_parser = parse_duration)]
    pub cache_max_age: Option<Duration>,
    /// Disable inbox cache reads and writes.
    #[arg(long = "no-cache", action = ArgAction::SetTrue)]
    pub no_cache: bool,
}

/// `inbox next` arguments.
#[derive(Args, Debug, Clone)]
pub struct InboxNextArgs {
    /// GitLab host for inbox API calls. Defaults from FORGE_CLI_INBOX_GITLAB_HOST,
    /// GitLab remote inference, then gitlab.com.
    #[arg(long = "gitlab-host", value_name = "HOST")]
    pub gitlab_host: Option<String>,
    /// Restrict inbox reasons/kinds. Repeatable. Defaults to review,
    /// assigned, todo, and authored; `involved` is opt-in.
    #[arg(long = "kind", value_enum)]
    pub kinds: Vec<InboxKindFlag>,
    /// Restrict inbox by item type (PR/MR vs issue). See `inbox list --help`
    /// for the distinction from `--kind`. Defaults to all.
    #[arg(long = "item-type", value_enum, default_value_t = InboxItemTypeFlag::All)]
    pub item_type: InboxItemTypeFlag,
    /// Number of ranked items to return (default: 5). Provider queries remain
    /// bounded at at least 30 candidates so ranking has enough input.
    #[arg(long, default_value_t = 5)]
    pub limit: u32,
    /// GitLab VPN readiness policy for inbox calls.
    #[arg(long = "gitlab-vpn", value_enum)]
    pub gitlab_vpn: Option<GitlabVpnModeFlag>,
    /// GitLab VPN readiness check: `tcp:<host>:<port>`, `cmd:<program>`, or `openvpn`.
    #[arg(long = "gitlab-vpn-check", value_name = "CHECK")]
    pub gitlab_vpn_check: Option<String>,
    /// GitLab VPN readiness check timeout (default: 2s).
    #[arg(long = "gitlab-vpn-check-timeout", value_parser = parse_duration)]
    pub gitlab_vpn_check_timeout: Option<Duration>,
    /// Local OpenVPN profile path for probe diagnostics. Redacted from output.
    #[arg(long = "gitlab-openvpn-profile", value_name = "PATH")]
    pub gitlab_openvpn_profile: Option<PathBuf>,
    /// Per-backend-call timeout for GitLab inbox calls (default: 20s; 0 disables).
    #[arg(long = "provider-timeout", value_parser = parse_duration)]
    pub provider_timeout: Option<Duration>,
    /// Fail when any selected inbox provider fails.
    #[arg(long = "strict-providers", action = ArgAction::SetTrue)]
    pub strict_providers: bool,
    /// Include recent cached items when a selected provider is unavailable or times out.
    #[arg(long = "cache-fallback", action = ArgAction::SetTrue)]
    pub cache_fallback: bool,
    /// Maximum age for opt-in cached fallback items (default: 30m).
    #[arg(long = "cache-max-age", value_parser = parse_duration)]
    pub cache_max_age: Option<Duration>,
    /// Disable inbox cache reads and writes.
    #[arg(long = "no-cache", action = ArgAction::SetTrue)]
    pub no_cache: bool,
}

/// `issue list` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.issue.list` inputs.
#[derive(Args, Debug, Clone)]
pub struct IssueListArgs {
    /// Filter by state (default: open).
    #[arg(long, value_enum, default_value_t = IssueStateFilter::Open)]
    pub state: IssueStateFilter,
    /// Filter by label. Repeatable. GitHub joins labels into a comma list
    /// (issue must have all labels); GitLab passes a repeated --label.
    #[arg(long = "label", value_name = "NAME")]
    pub labels: Vec<String>,
    /// Filter by author handle.
    #[arg(long)]
    pub author: Option<String>,
    /// Filter by assignee handle.
    #[arg(long)]
    pub assignee: Option<String>,
    /// Cap the number of returned issues (default: 30).
    #[arg(long, default_value_t = 30)]
    pub limit: u32,
}

/// `issue create` arguments.
#[derive(Args, Debug, Clone)]
pub struct IssueCreateArgs {
    /// Issue title (≤70 chars per `title_length` rule).
    #[arg(long)]
    pub title: String,
    /// Issue body (inline). Mutually exclusive with `--body-file`.
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read body from a file. Use `-` for stdin.
    #[arg(long = "body-file")]
    pub body_file: Option<String>,
    /// Add a label. Repeat to apply multiple labels.
    #[arg(long = "label", value_name = "NAME")]
    pub labels: Vec<String>,
    /// Assign a user. Repeat to assign multiple.
    #[arg(long = "assignee", value_name = "LOGIN")]
    pub assignees: Vec<String>,
}

/// `issue view` arguments.
#[derive(Args, Debug, Clone)]
pub struct IssueViewArgs {
    /// Numeric issue id.
    pub id: u64,
    /// Also fetch the issue's comment stream and embed it under `comments`
    /// in the envelope payload. Adds one GitLab API call; for GitHub the
    /// comments are pulled in the same `gh issue view --json` invocation.
    #[arg(long = "with-comments", action = ArgAction::SetTrue)]
    pub with_comments: bool,
}

/// `issue edit` arguments.
#[derive(Args, Debug, Clone)]
pub struct IssueEditArgs {
    /// Numeric issue id.
    pub id: u64,
    /// New title (re-validated against `title_length`).
    #[arg(long)]
    pub title: Option<String>,
    /// New body. Mutually exclusive with `--body-file`.
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read body from a file. Use `-` for stdin.
    #[arg(long = "body-file")]
    pub body_file: Option<String>,
    /// Add a label. Repeat to add multiple. Accepts `--label` as a shorthand
    /// matching `issue create --label`.
    #[arg(long = "add-label", alias = "label", value_name = "NAME")]
    pub add_label: Vec<String>,
    /// Remove a label. Repeat to remove multiple.
    #[arg(long = "remove-label", value_name = "NAME")]
    pub remove_label: Vec<String>,
    /// Add an assignee. Repeat to assign multiple.
    #[arg(long = "add-assignee", value_name = "LOGIN")]
    pub add_assignee: Vec<String>,
}

/// `issue comment` arguments.
#[derive(Args, Debug, Clone)]
pub struct IssueCommentArgs {
    /// Numeric issue id.
    pub id: u64,
    /// Comment body. Mutually exclusive with `--body-file`.
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read body from a file. Use `-` for stdin.
    #[arg(long = "body-file")]
    pub body_file: Option<String>,
}

/// `repo` subtree.
#[derive(Subcommand, Debug)]
pub enum RepoCommand {
    /// Resolve the repo slug, default branch, and supported merge methods.
    View,
    /// Deliver one signed commit to the default branch with a normal fast-forward push.
    PushDefault(RepoPushDefaultArgs),
}

/// `auth` subtree.
#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// Verify backend auth (gh / glab).
    Status,
}

/// Public entry point: parses argv and dispatches to a handler.
pub fn dispatch(args: Vec<OsString>) -> i32 {
    let cli = match parse_or_exit(args) {
        Ok(cli) => cli,
        Err(code) => return code,
    };

    let global: GlobalFlags = (&cli).into();
    let format = global.output_format();

    // `--provider local` only models a subset of the command tree; reject the
    // rest up front so they never fall through to a real backend spawn.
    if global.is_local() && !crate::local::command_supported(&cli.command) {
        return crate::local::unsupported_command().emit(format);
    }

    let result = match cli.command {
        Some(Command::Auth(AuthArgs {
            command: Some(AuthCommand::Status),
        })) => ops::auth_status::run(&global, format),
        Some(Command::Repo(RepoArgs {
            command: Some(RepoCommand::View),
        })) => ops::repo_view::run(&global, format),
        Some(Command::Repo(RepoArgs {
            command: Some(RepoCommand::PushDefault(args)),
        })) => ops::repo_push_default::run(&global, args, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::Create(args)),
        })) => ops::pr_create::run(&global, args, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::View { id }),
        })) => ops::pr_view::run(&global, id, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::List(args)),
        })) => ops::pr_list::run(&global, args, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::Close { id }),
        })) => ops::pr_close::run(&global, id, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::Edit(args)),
        })) => ops::pr_edit::run(&global, args, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::Comment(args)),
        })) => ops::pr_comment::run(&global, args, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::Review(args)),
        })) => ops::pr_review::run(&global, args, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::Comments(args)),
        })) => ops::pr_comments::run(&global, args, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::Ready(args)),
        })) => ops::pr_ready::run(&global, args, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::ReviewThreads(args)),
        })) => match args.command {
            // `resolve` / `reply` route to the GitHub-first write ops.
            ReviewThreadsCommand::Resolve(resolve_args) => {
                ops::pr_review_thread_resolve::run(&global, resolve_args, format)
            }
            ReviewThreadsCommand::Reply(reply_args) => {
                ops::pr_review_thread_reply::run(&global, reply_args, format)
            }
            // `list <id>` reads.
            ReviewThreadsCommand::List { id } => {
                ops::pr_review_threads::run(&global, PrReviewThreadsListArgs { id }, format)
            }
        },
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::Reviews(args)),
        })) => ops::pr_reviews::run(&global, args, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::PendingReview(args)),
        })) => match args.command {
            PrPendingReviewCommand::Inspect(inspect_args) => {
                ops::pr_pending_review::run_inspect(&global, inspect_args, format)
            }
            PrPendingReviewCommand::ResumeSubmit(resume_args) => {
                ops::pr_pending_review::run_resume_submit(&global, resume_args, format)
            }
            PrPendingReviewCommand::Submit(submit_args) => {
                ops::pr_pending_review::run_submit(&global, submit_args, format)
            }
            PrPendingReviewCommand::Discard(discard_args) => {
                ops::pr_pending_review::run_discard(&global, discard_args, format)
            }
            PrPendingReviewCommand::Delete(delete_args) => {
                ops::pr_pending_review::run_delete(&global, delete_args, format)
            }
        },
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::Tasks(args)),
        })) => ops::pr_tasks::run(&global, args, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::Checks(args)),
        })) => {
            if global.dry_run {
                let ctx = match crate::provider::detect(
                    global.provider_hint(),
                    &global.remote,
                    global.repo.as_deref(),
                    crate::provider::git_remote_url,
                ) {
                    Ok(ctx) => ctx,
                    Err(err) => return err.emit(format),
                };
                let code = ops::pr_checks::emit_dry_run(&ctx, &args, format);
                return code;
            }
            ops::pr_checks::run(&global, args, format)
        }
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::WaitChecks(args)),
        })) => ops::pr_wait_checks::run(&global, args, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::Merge(args)),
        })) => ops::pr_merge::run(&global, args, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::Deliver(args)),
        })) => crate::macros::pr_deliver::run(&global, args, format),
        Some(Command::Issue(IssueArgs {
            command: Some(IssueCommand::Create(args)),
        })) => ops::issue_create::run(&global, args, format),
        Some(Command::Issue(IssueArgs {
            command: Some(IssueCommand::View(args)),
        })) => ops::issue_view::run(&global, args, format),
        Some(Command::Issue(IssueArgs {
            command: Some(IssueCommand::List(args)),
        })) => ops::issue_list::run(&global, args, format),
        Some(Command::Issue(IssueArgs {
            command: Some(IssueCommand::Edit(args)),
        })) => ops::issue_edit::run(&global, args, format),
        Some(Command::Issue(IssueArgs {
            command: Some(IssueCommand::Comment(args)),
        })) => ops::issue_comment::run(&global, args, format),
        Some(Command::Issue(IssueArgs {
            command: Some(IssueCommand::Close(args)),
        })) => ops::issue_close::run(&global, args.id, args.reason, format),
        Some(Command::Issue(IssueArgs {
            command: Some(IssueCommand::Reopen { id }),
        })) => ops::issue_reopen::run(&global, id, format),
        Some(Command::Activity(ActivityArgs {
            command: Some(command),
        })) => ops::activity::run(&global, command, format),
        Some(Command::Label(LabelArgs {
            command: Some(command),
        })) => ops::label::run(&global, command, format),
        Some(Command::Inbox(InboxArgs {
            command: Some(command),
        })) => ops::inbox::run(&global, command, format),
        Some(Command::Search(SearchArgs {
            command: Some(command),
        })) => ops::search::run(&global, command, format),
        Some(Command::Completion(CompletionArgs { shell })) => emit_completion(shell),
        None
        | Some(Command::Auth(AuthArgs { command: None }))
        | Some(Command::Repo(RepoArgs { command: None }))
        | Some(Command::Pr(PrArgs { command: None }))
        | Some(Command::Issue(IssueArgs { command: None }))
        | Some(Command::Activity(ActivityArgs { command: None }))
        | Some(Command::Label(LabelArgs { command: None }))
        | Some(Command::Inbox(InboxArgs { command: None }))
        | Some(Command::Search(SearchArgs { command: None })) => {
            if matches!(format, OutputFormat::Json) {
                return emit_parse_error(
                    BINARY,
                    format,
                    "parse-error",
                    "missing required subcommand",
                );
            }
            // No subcommand: print help and exit USAGE so callers don't
            // mistake the no-op for success.
            let _ = <Cli as clap::CommandFactory>::command().print_help();
            return exit::USAGE;
        }
    };

    match result {
        Ok(code) => code,
        Err(err) => err.emit(format),
    }
}

/// Parse argv, gracefully routing parse errors through the workspace
/// contract's `emit_parse_error` helper so `--format json` works at the parse
/// layer too.
fn parse_or_exit(args: Vec<OsString>) -> Result<Cli, i32> {
    let mut argv: Vec<OsString> = Vec::with_capacity(args.len() + 1);
    argv.push(OsString::from("forge-cli"));
    argv.extend(args);

    match Cli::try_parse_from(argv.iter()) {
        Ok(cli) => Ok(cli),
        Err(err) => {
            use clap::error::ErrorKind;
            let kind = err.kind();
            let format = detect_format_from_argv(&argv);
            if matches!(kind, ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                // Mirror clap's own help/version output to the user's terminal
                // and exit cleanly.
                let _ = err.print();
                return Err(exit::SUCCESS);
            }
            if matches!(kind, ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand) {
                if matches!(format, OutputFormat::Json) {
                    return Err(emit_parse_error(
                        BINARY,
                        format,
                        "parse-error",
                        "missing required subcommand",
                    ));
                }
                // Text mode keeps clap's help output for interactive use.
                let _ = err.print();
                return Err(exit::SUCCESS);
            }

            let code = match kind {
                ErrorKind::InvalidSubcommand
                | ErrorKind::UnknownArgument
                | ErrorKind::InvalidValue => "unknown-subcommand",
                _ => "parse-error",
            };
            let message = render_clap_message(&err);
            Err(emit_parse_error(BINARY, format, code, &message))
        }
    }
}

/// Scan raw argv for `--format json`, `--format=json`, or the (forbidden but
/// pre-parse-time) `--json` token so parse errors render as the JSON envelope
/// when the caller asked for JSON.
fn detect_format_from_argv(argv: &[OsString]) -> OutputFormat {
    let mut iter = argv.iter().skip(1);
    while let Some(arg) = iter.next() {
        let Some(s) = arg.to_str() else { continue };
        if s == "--format"
            && let Some(next) = iter.next()
            && let Some(value) = next.to_str()
            && value.eq_ignore_ascii_case("json")
        {
            return OutputFormat::Json;
        }
        if let Some(rest) = s.strip_prefix("--format=")
            && rest.eq_ignore_ascii_case("json")
        {
            return OutputFormat::Json;
        }
    }
    OutputFormat::Text
}

/// Reduce clap's multi-line error rendering to a single, terse message
/// suitable for the envelope's `error.message` field.
fn render_clap_message(err: &clap::Error) -> String {
    err.to_string()
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| {
            let line = line.trim();
            line.strip_prefix("error:")
                .map(str::trim)
                .unwrap_or(line)
                .to_string()
        })
        .unwrap_or_else(|| "command-line parse failed".to_string())
}

/// Emit a clap-generated shell-completion script to stdout. The bash output
/// goes through [`normalize_bash_completion`] so case labels stay flat
/// (`forge__cli__pr__create` instead of clap_complete's default
/// `forge__cli__subcmd__pr__subcmd__create`). The flat form is what the
/// workspace's `completion-flag-parity-audit.sh` audit expects.
fn emit_completion(shell: Shell) -> Result<i32, ForgeError> {
    use clap::CommandFactory;
    use std::io::Write as _;
    let mut cmd = Cli::command();
    let bin = cmd.get_name().to_string();
    if matches!(shell, Shell::Bash) {
        let mut out: Vec<u8> = Vec::new();
        clap_complete::generate(shell, &mut cmd, bin, &mut out);
        let normalized = normalize_bash_completion(String::from_utf8(out).map_err(|e| {
            ForgeError::software(
                nils_common::cli_contract::schema_version_for(BINARY, "error", 1),
                "bash completion output not UTF-8",
                Some(e.to_string()),
            )
        })?);
        let _ = std::io::stdout().write_all(normalized.as_bytes());
    } else {
        clap_complete::generate(shell, &mut cmd, bin, &mut std::io::stdout());
    }
    Ok(exit::SUCCESS)
}

fn normalize_bash_completion(script: String) -> String {
    script.replace("__subcmd__", "__")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let mut argv: Vec<OsString> = vec![OsString::from("forge-cli")];
        argv.extend(args.iter().map(OsString::from));
        Cli::try_parse_from(argv.iter())
    }

    #[test]
    fn parses_auth_status_with_defaults() {
        let cli = parse(&["auth", "status"]).expect("parse");
        match &cli.command {
            Some(Command::Auth(AuthArgs {
                command: Some(command),
            })) => {
                assert!(matches!(command, AuthCommand::Status));
            }
            other => panic!("expected auth, got {other:?}"),
        }
        let global: GlobalFlags = (&cli).into();
        assert_eq!(global.remote, "origin");
        assert!(!global.dry_run);
        assert_eq!(global.output_format(), OutputFormat::Text);
    }

    #[test]
    fn parses_global_format_json() {
        let cli = parse(&["--format", "json", "repo", "view"]).expect("parse");
        let global: GlobalFlags = (&cli).into();
        assert_eq!(global.output_format(), OutputFormat::Json);
    }

    #[test]
    fn rejects_unmarked_json_boolean_flag() {
        let result = parse(&["--json", "repo", "view"]);
        assert!(result.is_err(), "--json must not be accepted");
    }

    #[test]
    fn parses_provider_override() {
        let cli = parse(&["--provider", "github", "auth", "status"]).expect("parse");
        let global: GlobalFlags = (&cli).into();
        assert_eq!(global.provider, Some(ProviderFlag::Github));
    }

    #[test]
    fn parses_global_host_binding() {
        let cli = parse(&[
            "--provider",
            "gitlab",
            "--host",
            "gitlab.example.com",
            "--repo",
            "group/project",
            "repo",
            "view",
        ])
        .expect("parse");
        let global: GlobalFlags = (&cli).into();
        assert_eq!(global.host.as_deref(), Some("gitlab.example.com"));
        assert_eq!(
            global.provider_hint(),
            ProviderHint::ForcedHost(
                crate::provider::Provider::GitLab,
                "gitlab.example.com".into()
            )
        );
    }

    #[test]
    fn parses_host_only_binding_after_subcommand() {
        let cli = parse(&[
            "repo",
            "view",
            "--host",
            "internal.ghe.com",
            "--repo",
            "owner/repo",
        ])
        .expect("parse");
        let global: GlobalFlags = (&cli).into();
        assert_eq!(
            global.provider_hint(),
            ProviderHint::Host("internal.ghe.com".into())
        );
    }

    #[test]
    fn parses_dry_run_flag() {
        let cli = parse(&["--dry-run", "auth", "status"]).expect("parse");
        let global: GlobalFlags = (&cli).into();
        assert!(global.dry_run);
    }

    #[test]
    fn rejects_unknown_subcommand() {
        let result = parse(&["bogus"]);
        assert!(result.is_err());
    }

    #[test]
    fn lists_every_pr_v1_subcommand() {
        for sub in [
            "create",
            "view",
            "list",
            "edit",
            "comment",
            "comments",
            "ready",
            "review-threads",
            "pending-review",
            "tasks",
            "merge",
            "close",
            "checks",
            "wait-checks",
            "deliver",
        ] {
            let mut argv = vec!["pr", sub];
            match sub {
                "view" | "checks" | "wait-checks" => argv.push("1"),
                // `review-threads` is a clean subcommand group: list via `list <id>`.
                "review-threads" => argv.extend(["list", "1"]),
                "pending-review" => argv.extend([
                    "delete",
                    "1",
                    "--review",
                    "PRR_pending",
                    "--expected-head",
                    "head-reviewed",
                    "--expected-commit",
                    "head-reviewed",
                    "--expected-body",
                    "abandoned draft",
                    "--confirm-abandoned",
                ]),
                "edit" | "comment" | "comments" | "ready" | "tasks" | "merge" | "close" => {
                    argv.push("1")
                }
                "create" => {
                    argv.extend(["--title", "demo", "--kind", "feature", "--body", "x"]);
                }
                "deliver" => {
                    argv.extend(["--kind", "feature", "--title", "demo"]);
                }
                _ => {}
            }
            let result = parse(&argv);
            assert!(result.is_ok(), "pr {sub} should parse, got {result:?}");
        }
    }

    #[test]
    fn pr_review_threads_bare_id_is_rejected() {
        // `review-threads` is a clean subcommand group: a bare positional id is
        // no longer accepted (it would shift the zsh subcommand to $line[2] and
        // break the workspace completion-parity audit). Use `list <id>`.
        assert!(
            parse(&["pr", "review-threads", "7"]).is_err(),
            "bare `review-threads <id>` must be rejected; use `list <id>`",
        );
    }

    #[test]
    fn pr_review_thread_file_parses() {
        let cli = parse(&[
            "pr",
            "review",
            "7",
            "--submit-review",
            "--comment",
            "summary",
            "--thread-file",
            "review-threads.json",
        ])
        .expect("review thread-file parses");
        match cli.command {
            Some(Command::Pr(PrArgs {
                command: Some(PrCommand::Review(args)),
            })) => {
                assert_eq!(args.id, Some(7));
                assert!(args.submit_review);
                assert_eq!(args.thread_file.as_deref(), Some("review-threads.json"));
            }
            other => panic!("expected pr review, got {other:?}"),
        }
    }

    #[test]
    fn pr_review_validate_subcommand_parses() {
        let cli = parse(&[
            "pr",
            "review",
            "validate",
            "7",
            "--check-diff",
            "--comment",
            "summary",
            "--thread-file",
            "review-threads.json",
        ])
        .expect("review validate parses");
        match cli.command {
            Some(Command::Pr(PrArgs {
                command: Some(PrCommand::Review(args)),
            })) => match args.command {
                Some(PrReviewCommand::Validate(validate)) => {
                    assert_eq!(validate.id, Some(7));
                    assert!(validate.check_diff);
                    assert_eq!(validate.comment.as_deref(), Some("summary"));
                    assert_eq!(validate.thread_file.as_deref(), Some("review-threads.json"));
                }
                other => panic!("expected pr review validate, got {other:?}"),
            },
            other => panic!("expected pr review, got {other:?}"),
        }
    }

    #[test]
    fn pr_review_threads_list_subcommand_parses() {
        let cli = parse(&["pr", "review-threads", "list", "7"]).expect("list subcommand parses");
        match cli.command {
            Some(Command::Pr(PrArgs {
                command:
                    Some(PrCommand::ReviewThreads(PrReviewThreadsArgs {
                        command: ReviewThreadsCommand::List { id },
                    })),
            })) => assert_eq!(id, 7),
            other => panic!("expected review-threads list, got {other:?}"),
        }
    }

    #[test]
    fn pr_pending_review_delete_subcommand_parses() {
        let cli = parse(&[
            "pr",
            "pending-review",
            "delete",
            "7",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-reviewed",
            "--expected-commit",
            "head-reviewed",
            "--expected-body",
            "abandoned draft",
            "--confirm-abandoned",
        ])
        .expect("pending-review delete parses");
        match cli.command {
            Some(Command::Pr(PrArgs {
                command:
                    Some(PrCommand::PendingReview(PrPendingReviewArgs {
                        command: PrPendingReviewCommand::Delete(args),
                    })),
            })) => {
                assert_eq!(args.id, 7);
                assert_eq!(args.review, "PRR_pending");
                assert_eq!(args.expected_head, "head-reviewed");
                assert_eq!(args.expected_commit, "head-reviewed");
                assert_eq!(args.expected_body.as_deref(), Some("abandoned draft"));
                assert!(args.expected_body_file.is_none());
                assert!(args.confirm_abandoned);
            }
            other => panic!("expected pending-review delete, got {other:?}"),
        }
    }

    #[test]
    fn pr_pending_review_recovery_subcommands_parse() {
        let inspect = parse(&[
            "pr",
            "pending-review",
            "inspect",
            "7",
            "--review",
            "PRR_pending",
        ])
        .expect("pending-review inspect parses");
        assert!(matches!(
            inspect.command,
            Some(Command::Pr(PrArgs {
                command: Some(PrCommand::PendingReview(PrPendingReviewArgs {
                    command: PrPendingReviewCommand::Inspect(_),
                })),
            }))
        ));

        let resume = parse(&[
            "pr",
            "pending-review",
            "resume-submit",
            "7",
            "--review",
            "PRR_pending",
            "--review-run-id",
            "run-1",
            "--expected-head",
            "head-1",
            "--expected-commit",
            "head-1",
            "--expected-snapshot",
            "sha256:snapshot",
            "--decision",
            "comments-only",
        ])
        .expect("pending-review resume-submit parses");
        assert!(matches!(
            resume.command,
            Some(Command::Pr(PrArgs {
                command: Some(PrCommand::PendingReview(PrPendingReviewArgs {
                    command: PrPendingReviewCommand::ResumeSubmit(_),
                })),
            }))
        ));

        let submit = parse(&[
            "pr",
            "pending-review",
            "submit",
            "7",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-1",
            "--expected-commit",
            "head-1",
            "--expected-snapshot",
            "sha256:snapshot",
            "--decision",
            "comments-only",
            "--confirm-unmarked-submit",
        ])
        .expect("pending-review submit parses");
        assert!(matches!(
            submit.command,
            Some(Command::Pr(PrArgs {
                command: Some(PrCommand::PendingReview(PrPendingReviewArgs {
                    command: PrPendingReviewCommand::Submit(_),
                })),
            }))
        ));

        let discard = parse(&[
            "pr",
            "pending-review",
            "discard",
            "7",
            "--review",
            "PRR_pending",
            "--expected-head",
            "head-1",
            "--expected-commit",
            "head-1",
            "--expected-snapshot",
            "sha256:snapshot",
            "--confirm-discard",
            "--confirm-inline-content-loss",
        ])
        .expect("pending-review discard parses");
        assert!(matches!(
            discard.command,
            Some(Command::Pr(PrArgs {
                command: Some(PrCommand::PendingReview(PrPendingReviewArgs {
                    command: PrPendingReviewCommand::Discard(_),
                })),
            }))
        ));
    }

    #[test]
    fn pr_review_threads_resolve_subcommand_parses() {
        let cli = parse(&["pr", "review-threads", "resolve", "7", "--thread", "PRRT_x"])
            .expect("resolve subcommand parses");
        match cli.command {
            Some(Command::Pr(PrArgs {
                command:
                    Some(PrCommand::ReviewThreads(PrReviewThreadsArgs {
                        command: ReviewThreadsCommand::Resolve(args),
                    })),
            })) => {
                assert_eq!(args.id, 7);
                assert_eq!(args.thread, "PRRT_x");
                assert!(args.note.is_none());
                assert!(args.note_file.is_none());
            }
            other => panic!("expected review-threads resolve, got {other:?}"),
        }
    }

    #[test]
    fn pr_review_threads_reply_subcommand_parses() {
        let cli = parse(&[
            "pr",
            "review-threads",
            "reply",
            "7",
            "--thread",
            "PRRT_x",
            "--body",
            "ack",
        ])
        .expect("reply subcommand parses");
        match cli.command {
            Some(Command::Pr(PrArgs {
                command:
                    Some(PrCommand::ReviewThreads(PrReviewThreadsArgs {
                        command: ReviewThreadsCommand::Reply(args),
                    })),
            })) => {
                assert_eq!(args.id, 7);
                assert_eq!(args.thread, "PRRT_x");
                assert_eq!(args.body.as_deref(), Some("ack"));
            }
            other => panic!("expected review-threads reply, got {other:?}"),
        }
    }

    #[test]
    fn pr_review_threads_resolve_rejects_note_and_note_file_together() {
        let result = parse(&[
            "pr",
            "review-threads",
            "resolve",
            "7",
            "--thread",
            "PRRT_x",
            "--note",
            "inline",
            "--note-file",
            "-",
        ]);
        assert!(result.is_err(), "--note + --note-file must conflict");
    }

    #[test]
    fn pr_merge_allow_unchecked_tasks_requires_reason() {
        assert!(parse(&["pr", "merge", "1", "--allow-unchecked-tasks"]).is_err());
        assert!(
            parse(&[
                "pr",
                "merge",
                "1",
                "--allow-unchecked-tasks-reason",
                "tracked in #99"
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "pr",
                "merge",
                "1",
                "--allow-unchecked-tasks",
                "--allow-unchecked-tasks-reason",
                "tracked in #99"
            ])
            .is_ok()
        );
        assert!(
            parse(&[
                "pr",
                "merge",
                "1",
                "--allow-unchecked-tasks",
                "--allow-unchecked-tasks-reason",
                ""
            ])
            .is_err(),
            "empty bypass reason must be rejected"
        );
        assert!(
            parse(&[
                "pr",
                "deliver",
                "--kind",
                "feature",
                "--title",
                "demo",
                "--allow-unchecked-tasks"
            ])
            .is_err()
        );
    }

    #[test]
    fn pr_merge_allow_unresolved_threads_requires_reason() {
        // Bypassing the unresolved-threads gate must carry a recorded reason,
        // mirroring `--allow-unchecked-tasks` / `--allow-unchecked-tasks-reason`.
        assert!(
            parse(&["pr", "merge", "1", "--allow-unresolved-threads"]).is_err(),
            "--allow-unresolved-threads without a reason must be rejected"
        );
        assert!(
            parse(&[
                "pr",
                "merge",
                "1",
                "--allow-unresolved-threads-reason",
                "outdated bot threads"
            ])
            .is_err(),
            "the reason without the bypass flag must be rejected"
        );
        assert!(
            parse(&[
                "pr",
                "merge",
                "1",
                "--allow-unresolved-threads",
                "--allow-unresolved-threads-reason",
                "outdated bot threads"
            ])
            .is_ok()
        );
        assert!(
            parse(&[
                "pr",
                "merge",
                "1",
                "--allow-unresolved-threads",
                "--allow-unresolved-threads-reason",
                ""
            ])
            .is_err(),
            "empty bypass reason must be rejected"
        );
        assert!(
            parse(&[
                "pr",
                "deliver",
                "--kind",
                "feature",
                "--title",
                "demo",
                "--allow-unresolved-threads"
            ])
            .is_err()
        );
    }

    #[test]
    fn pr_create_rejects_both_body_and_body_file() {
        let result = parse(&[
            "pr",
            "create",
            "--title",
            "demo",
            "--kind",
            "feature",
            "--body",
            "inline",
            "--body-file",
            "-",
        ]);
        assert!(result.is_err(), "--body + --body-file must conflict");
    }

    #[test]
    fn pr_create_parses_reviewer_and_label_lists() {
        let cli = parse(&[
            "pr",
            "create",
            "--title",
            "demo",
            "--kind",
            "bug",
            "--body",
            "x",
            "--reviewer",
            "alice",
            "--reviewer",
            "bob",
            "--label",
            "p1",
            "--label",
            "needs-review",
        ])
        .expect("parse");
        match cli.command {
            Some(Command::Pr(PrArgs {
                command: Some(PrCommand::Create(args)),
            })) => {
                assert_eq!(args.kind, PrKindFlag::Bug);
                assert_eq!(args.reviewers, vec!["alice", "bob"]);
                assert_eq!(args.labels, vec!["p1", "needs-review"]);
                assert!(!args.no_draft);
            }
            other => panic!("expected pr create, got {other:?}"),
        }
    }

    #[test]
    fn lists_every_issue_v1_subcommand() {
        for sub in [
            "create", "view", "list", "edit", "comment", "close", "reopen",
        ] {
            let argv: Vec<&str> = match sub {
                "create" => vec!["issue", "create", "--title", "demo"],
                "list" => vec!["issue", "list"],
                "comment" | "edit" => vec!["issue", sub, "1"],
                _ => vec!["issue", sub, "1"],
            };
            let result = parse(&argv);
            assert!(result.is_ok(), "issue {sub} should parse, got {result:?}");
        }
    }

    #[test]
    fn lists_every_activity_v1_subcommand() {
        for sub in ["commits", "events", "feed", "summary"] {
            let result = parse(&["activity", sub]);
            assert!(
                result.is_ok(),
                "activity {sub} should parse, got {result:?}"
            );
        }
    }

    #[test]
    fn activity_commits_parses_user_since_and_limit() {
        let cli = parse(&[
            "activity",
            "commits",
            "--user",
            "alice",
            "--since",
            "2026-05-01",
            "--limit",
            "7",
        ])
        .expect("parse");
        match cli.command {
            Some(Command::Activity(ActivityArgs {
                command: Some(ActivityCommand::Commits(args)),
            })) => {
                assert_eq!(args.user, "alice");
                assert_eq!(args.since.as_deref(), Some("2026-05-01"));
                assert_eq!(args.limit, 7);
            }
            other => panic!("expected activity commits, got {other:?}"),
        }
    }

    #[test]
    fn activity_events_parses_public_only() {
        let cli = parse(&["activity", "events", "--public-only"]).expect("parse");
        match cli.command {
            Some(Command::Activity(ActivityArgs {
                command: Some(ActivityCommand::Events(args)),
            })) => {
                assert_eq!(args.user, "@me");
                assert_eq!(args.limit, 30);
                assert!(args.public_only);
            }
            other => panic!("expected activity events, got {other:?}"),
        }
    }

    #[test]
    fn activity_feed_parses_since_and_limit() {
        let cli =
            parse(&["activity", "feed", "--since", "2026-06-01", "--limit", "9"]).expect("parse");
        match cli.command {
            Some(Command::Activity(ActivityArgs {
                command: Some(ActivityCommand::Feed(args)),
            })) => {
                assert_eq!(args.since.as_deref(), Some("2026-06-01"));
                assert_eq!(args.limit, 9);
            }
            other => panic!("expected activity feed, got {other:?}"),
        }
    }

    #[test]
    fn activity_summary_parses_defaults_and_limit() {
        let cli = parse(&["activity", "summary", "--limit", "5"]).expect("parse");
        match cli.command {
            Some(Command::Activity(ActivityArgs {
                command: Some(ActivityCommand::Summary(args)),
            })) => {
                assert_eq!(args.user, "@me");
                assert_eq!(args.since, None);
                assert_eq!(args.limit, 5);
            }
            other => panic!("expected activity summary, got {other:?}"),
        }
    }

    #[test]
    fn lists_every_label_v1_subcommand() {
        for sub in ["list", "audit", "ensure"] {
            let argv: Vec<&str> = match sub {
                "list" => vec!["label", "list"],
                "audit" | "ensure" => vec!["label", sub, "--catalog", "labels.yaml"],
                _ => unreachable!(),
            };
            let result = parse(&argv);
            assert!(result.is_ok(), "label {sub} should parse, got {result:?}");
        }
    }

    #[test]
    fn lists_every_inbox_v1_subcommand() {
        for sub in ["status", "list", "next"] {
            let result = parse(&["inbox", sub]);
            assert!(result.is_ok(), "inbox {sub} should parse, got {result:?}");
        }
    }

    #[test]
    fn inbox_cli_parses_gitlab_host_kind_and_limit() {
        let cli = parse(&[
            "inbox",
            "list",
            "--gitlab-host",
            "gitlab.example.com",
            "--kind",
            "review",
            "--kind",
            "assigned",
            "--limit",
            "7",
        ])
        .expect("parse");
        match cli.command {
            Some(Command::Inbox(InboxArgs {
                command: Some(InboxCommand::List(args)),
            })) => {
                assert_eq!(args.gitlab_host.as_deref(), Some("gitlab.example.com"));
                assert_eq!(
                    args.kinds,
                    vec![InboxKindFlag::Review, InboxKindFlag::Assigned]
                );
                assert_eq!(args.limit, 7);
                assert_eq!(args.item_type, InboxItemTypeFlag::All);
            }
            other => panic!("expected inbox list, got {other:?}"),
        }
    }

    #[test]
    fn inbox_cli_parses_item_type_for_list_status_next() {
        for sub in ["list", "status", "next"] {
            let cli = parse(&["inbox", sub, "--item-type", "pr"]).expect("parse pr item-type");
            match cli.command {
                Some(Command::Inbox(InboxArgs {
                    command: Some(InboxCommand::List(args)),
                })) => assert_eq!(args.item_type, InboxItemTypeFlag::Pr),
                Some(Command::Inbox(InboxArgs {
                    command: Some(InboxCommand::Status(args)),
                })) => assert_eq!(args.item_type, InboxItemTypeFlag::Pr),
                Some(Command::Inbox(InboxArgs {
                    command: Some(InboxCommand::Next(args)),
                })) => assert_eq!(args.item_type, InboxItemTypeFlag::Pr),
                other => panic!("expected inbox {sub}, got {other:?}"),
            }
        }
    }

    #[test]
    fn inbox_cli_rejects_unknown_item_type() {
        let result = parse(&["inbox", "list", "--item-type", "bogus"]);
        assert!(result.is_err(), "--item-type bogus must fail clap parsing");
    }

    #[test]
    fn gitlab_host_is_not_global() {
        let result = parse(&["--gitlab-host", "gitlab.example.com", "repo", "view"]);
        assert!(result.is_err(), "--gitlab-host must stay inbox-local");
    }

    #[test]
    fn detect_format_from_argv_finds_json() {
        let argv = vec![
            OsString::from("forge-cli"),
            OsString::from("--format"),
            OsString::from("json"),
            OsString::from("auth"),
            OsString::from("status"),
        ];
        assert_eq!(detect_format_from_argv(&argv), OutputFormat::Json);
    }

    #[test]
    fn detect_format_from_argv_handles_equals_form() {
        let argv = vec![
            OsString::from("forge-cli"),
            OsString::from("--format=json"),
            OsString::from("auth"),
            OsString::from("status"),
        ];
        assert_eq!(detect_format_from_argv(&argv), OutputFormat::Json);
    }

    #[test]
    fn detect_format_from_argv_defaults_to_text() {
        let argv = vec![OsString::from("forge-cli"), OsString::from("auth")];
        assert_eq!(detect_format_from_argv(&argv), OutputFormat::Text);
    }
}

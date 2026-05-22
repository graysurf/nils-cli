//! Clap derive tree, global flag definitions, and the top-level dispatch
//! entry consumed by `crate::run`.
//!
//! Every subcommand listed in `crates/forge-cli/docs/specs/forge-cli-spec-v1.md` §"Command
//! tree" is declared here, even when the v1 handler is not yet implemented in
//! this sprint. Stubs return a structured `not_implemented` envelope under
//! `SOFTWARE 70` so callers see a stable failure shape rather than a panic.

use std::ffi::OsString;
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

    /// Override the repo slug (`owner/name`). When absent it is derived from
    /// the remote URL.
    #[arg(long, global = true, value_name = "owner/name")]
    pub repo: Option<String>,

    /// Render the backend command that would run, without invoking it. The
    /// envelope's `data.plan` carries the exact argv.
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
    pub repo: Option<String>,
    pub dry_run: bool,
}

impl From<&Cli> for GlobalFlags {
    fn from(cli: &Cli) -> Self {
        Self {
            format: cli.format,
            remote: cli.remote.clone(),
            provider: cli.provider,
            repo: cli.repo.clone(),
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
        match self.provider {
            Some(ProviderFlag::Github) => ProviderHint::Forced(crate::provider::Provider::GitHub),
            Some(ProviderFlag::Gitlab) => ProviderHint::Forced(crate::provider::Provider::GitLab),
            None => ProviderHint::Auto,
        }
    }
}

/// Provider override for `--provider`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum ProviderFlag {
    Github,
    Gitlab,
}

/// Top-level subcommand tree.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Pull / merge request lifecycle.
    Pr(PrArgs),
    /// Issue lifecycle.
    Issue(IssueArgs),
    /// Personal cross-repo work inbox.
    Inbox(InboxArgs),
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
pub struct InboxArgs {
    #[command(subcommand)]
    pub command: Option<InboxCommand>,
}

#[derive(Args, Debug)]
pub struct RepoArgs {
    #[command(subcommand)]
    pub command: Option<RepoCommand>,
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
}

impl PrKindFlag {
    pub fn into_kind(self) -> crate::validations::PrKind {
        match self {
            PrKindFlag::Feature => crate::validations::PrKind::Feature,
            PrKindFlag::Bug => crate::validations::PrKind::Bug,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PrKindFlag::Feature => "feature",
            PrKindFlag::Bug => "bug",
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

/// `pr ready` arguments.
#[derive(Args, Debug, Clone)]
pub struct PrReadyArgs {
    /// Numeric PR / MR id.
    pub id: u64,
}

/// `pr merge` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.pr.merge` inputs.
#[derive(Args, Debug, Clone)]
pub struct PrMergeArgs {
    /// Numeric PR / MR id.
    pub id: u64,
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
        "m" => Duration::from_secs(value * 60),
        "h" => Duration::from_secs(value * 3600),
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
    /// Promote a draft PR / MR to ready-for-review.
    Ready(PrReadyArgs),
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
    /// End-to-end "open draft → CI green → ready → merge" macro.
    Deliver(PrDeliverArgs),
}

/// `pr deliver` arguments. Maps to
/// `forge-cli-ops-v1.yaml::operations.pr.deliver` inputs.
#[derive(Args, Debug, Clone)]
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
    /// CI-wait budget before declaring `checks_timeout` (default `30m`).
    #[arg(long, value_parser = parse_duration, default_value = "30m")]
    pub timeout: Duration,
    /// Stop after `pr.wait-checks` — do not promote to ready or merge.
    #[arg(long = "no-merge", action = ArgAction::SetTrue)]
    pub no_merge: bool,
    /// Allow merges where the PR's base is not the repo's default branch.
    #[arg(long = "allow-non-default-base", action = ArgAction::SetTrue)]
    pub allow_non_default_base: bool,
}

/// `issue` subtree.
#[derive(Subcommand, Debug)]
pub enum IssueCommand {
    /// Open a new issue.
    Create(IssueCreateArgs),
    /// Fetch a single issue.
    View {
        /// Numeric id.
        id: u64,
    },
    /// Mutate an issue.
    Edit(IssueEditArgs),
    /// Append a comment to an issue.
    Comment(IssueCommentArgs),
    /// Close an issue.
    Close {
        /// Numeric id.
        id: u64,
    },
    /// Reopen a closed issue.
    Reopen {
        /// Numeric id.
        id: u64,
    },
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
    /// Per-provider, per-query-family bounded result limit (default: 30).
    #[arg(long, default_value_t = 30)]
    pub limit: u32,
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
    /// Number of ranked items to return (default: 5). Provider queries remain
    /// bounded at at least 30 candidates so ranking has enough input.
    #[arg(long, default_value_t = 5)]
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
    /// Add a label. Repeat to add multiple.
    #[arg(long = "add-label", value_name = "NAME")]
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

    let result = match cli.command {
        Some(Command::Auth(AuthArgs {
            command: Some(AuthCommand::Status),
        })) => ops::auth_status::run(&global, format),
        Some(Command::Repo(RepoArgs {
            command: Some(RepoCommand::View),
        })) => ops::repo_view::run(&global, format),
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
            command: Some(PrCommand::Ready(args)),
        })) => ops::pr_ready::run(&global, args, format),
        Some(Command::Pr(PrArgs {
            command: Some(PrCommand::Checks(args)),
        })) => {
            if global.dry_run {
                let ctx = match crate::provider::detect(
                    global.provider_hint(),
                    &global.remote,
                    crate::provider::git_remote_url,
                ) {
                    Ok(ctx) => ctx,
                    Err(err) => return err.emit(format),
                };
                let runner = crate::backend::ProcessRunner;
                let code = ops::pr_checks::emit_dry_run(&runner, &ctx, &args, format);
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
            command: Some(IssueCommand::View { id }),
        })) => ops::issue_view::run(&global, id, format),
        Some(Command::Issue(IssueArgs {
            command: Some(IssueCommand::Edit(args)),
        })) => ops::issue_edit::run(&global, args, format),
        Some(Command::Issue(IssueArgs {
            command: Some(IssueCommand::Comment(args)),
        })) => ops::issue_comment::run(&global, args, format),
        Some(Command::Issue(IssueArgs {
            command: Some(IssueCommand::Close { id }),
        })) => ops::issue_close::run(&global, id, format),
        Some(Command::Issue(IssueArgs {
            command: Some(IssueCommand::Reopen { id }),
        })) => ops::issue_reopen::run(&global, id, format),
        Some(Command::Inbox(InboxArgs {
            command: Some(command),
        })) => ops::inbox::run(&global, command, format),
        Some(Command::Completion(CompletionArgs { shell })) => emit_completion(shell),
        None
        | Some(Command::Auth(AuthArgs { command: None }))
        | Some(Command::Repo(RepoArgs { command: None }))
        | Some(Command::Pr(PrArgs { command: None }))
        | Some(Command::Issue(IssueArgs { command: None }))
        | Some(Command::Inbox(InboxArgs { command: None })) => {
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
            if matches!(
                kind,
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                // Mirror clap's own help/version output to the user's terminal
                // and exit cleanly.
                let _ = err.print();
                return Err(exit::SUCCESS);
            }

            let format = detect_format_from_argv(&argv);
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
    fn rejects_legacy_json_boolean_flag() {
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
            "ready",
            "merge",
            "close",
            "checks",
            "wait-checks",
            "deliver",
        ] {
            let mut argv = vec!["pr", sub];
            match sub {
                "view" | "checks" | "wait-checks" => argv.push("1"),
                "edit" | "comment" | "ready" | "merge" | "close" => argv.push("1"),
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
        for sub in ["create", "view", "edit", "comment", "close", "reopen"] {
            let argv: Vec<&str> = match sub {
                "create" => vec!["issue", "create", "--title", "demo"],
                "comment" | "edit" => vec!["issue", sub, "1"],
                _ => vec!["issue", sub, "1"],
            };
            let result = parse(&argv);
            assert!(result.is_ok(), "issue {sub} should parse, got {result:?}");
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
            }
            other => panic!("expected inbox list, got {other:?}"),
        }
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

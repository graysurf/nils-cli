use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

#[derive(Debug, Clone, Args, Serialize)]
pub struct RecordArgs {
    #[command(subcommand)]
    pub command: RecordCommand,
}

#[derive(Debug, Clone, Subcommand, Serialize)]
pub enum RecordCommand {
    /// Open a provider issue from a plan bundle and post initial lifecycle
    /// comments (v3 issue-backed plan record contract).
    Open(Box<RecordOpenArgs>),

    /// Attach source, plan, and initial state lifecycle comments to an
    /// existing provider issue.
    Attach(Box<RecordAttachArgs>),

    /// Append a canonical lifecycle comment (state, session, validation,
    /// review, or closeout) to an existing plan record issue.
    Post(Box<RecordPostArgs>),

    /// Recompute and edit the dashboard issue body from active payload
    /// evidence. Old state payload formats require one-off migration/repair.
    #[command(
        after_help = "State payload replacement policy: this command targets the active payload contract only. Old state payload formats require one-off migration/repair outside the main CLI; no long-term v2 reader or mixed old/new stream reconciliation is provided."
    )]
    RepairDashboard(Box<RecordRepairDashboardArgs>),

    /// Close a plan record issue after the strict lifecycle gate passes.
    Close(Box<RecordCloseArgs>),

    /// Audit issue body and comments for the active payload contract. Old
    /// state payload formats require one-off migration/repair.
    #[command(
        after_help = "State payload replacement policy: this command targets the active payload contract only. Old state payload formats require one-off migration/repair outside the main CLI; no long-term v2 reader or mixed old/new stream reconciliation is provided."
    )]
    Audit(Box<RecordAuditArgs>),

    /// Preview the visible Markdown or JSON payload skeleton for a lifecycle
    /// role. Non-mutating; backed by the vNext lifecycle role registry.
    Template(Box<RecordTemplateArgs>),

    /// Re-materialize a plan bundle's `source` and `plan` documents from a
    /// tracking issue's frozen snapshot comments, writing each file to its
    /// canonical path under `--out`. The inverse of `record open`'s
    /// snapshot rendering. The `state` role is a rendered lifecycle view
    /// (not a verbatim file snapshot) and is not restored.
    #[command(
        after_help = "Only the `source` and `plan` roles embed a verbatim file snapshot in a <details> block. The `state` role is rendered from structured payload data, so it is not a restorable file snapshot; its latest rendered form stays visible on the issue."
    )]
    Restore(Box<RecordRestoreArgs>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
pub enum RecordProfile {
    Tracking,
    Dispatch,
}

impl RecordProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tracking => "tracking",
            Self::Dispatch => "dispatch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
pub enum LifecycleCommentKind {
    #[value(name = "source", alias = "source-snapshot")]
    Source,
    #[value(name = "plan", alias = "plan-snapshot")]
    Plan,
    State,
    Session,
    Validation,
    Review,
    Closeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
pub enum TaskLedgerDisplay {
    /// Expand the rows when the state is terminal, collapse them otherwise.
    Auto,
    /// Wrap the rows in a closed `<details>` fold.
    Collapsed,
    /// Render the rows directly with no fold (required for the final
    /// pre-closeout state; visible-lint forbids a `<details>` wrapper there).
    Expanded,
    /// Wrap the rows in an open `<details open>` fold — the toggle stays, but
    /// the ledger is visible by default. Used for the first Execution State so
    /// the full plan is visible on load while remaining collapsible.
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
pub enum TemplateFormatArg {
    Markdown,
    Json,
}

impl TemplateFormatArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Json => "json",
        }
    }
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct RecordTemplateArgs {
    /// Lifecycle profile for the template preview.
    #[arg(long, value_enum, default_value_t = RecordProfile::Tracking)]
    pub profile: RecordProfile,

    /// Lifecycle role to preview.
    #[arg(long, value_enum)]
    pub kind: LifecycleCommentKind,

    /// Template output shape. `markdown` prints the visible body skeleton;
    /// `json` prints the payload data skeleton. (Named `--shape` rather than
    /// `--format` because the global `--format text|json` controls the
    /// command envelope and would shadow a subcommand `--format` flag.)
    #[arg(long, value_enum, default_value_t = TemplateFormatArg::Markdown)]
    pub shape: TemplateFormatArg,
}

impl LifecycleCommentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Plan => "plan",
            Self::State => "state",
            Self::Session => "session",
            Self::Validation => "validation",
            Self::Review => "review",
            Self::Closeout => "closeout",
        }
    }
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct RecordAuditArgs {
    /// Provider issue body Markdown.
    #[arg(long = "body-file", value_name = "path")]
    pub body_file: Option<PathBuf>,

    /// JSON containing either `comments` from `gh issue view --json comments`
    /// or a raw array of comment objects.
    #[arg(long = "comments-json", value_name = "path")]
    pub comments_json: PathBuf,

    /// Expected profile. When omitted, all recognized markers are reported.
    #[arg(long, value_enum)]
    pub profile: Option<RecordProfile>,

    /// Also run the visible-completeness lint against the latest comment body
    /// per role. Produces stable role-specific failure codes
    /// (`state-missing-task-ledger`, `validation-missing-overall`, …) in the
    /// `visible` block of the audit result.
    #[arg(long = "expect-visible", default_value_t = false)]
    pub expect_visible: bool,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct RecordRestoreArgs {
    /// Provider issue number or full URL to restore from (online mode).
    /// Requires the global `--repo owner/repo`. Omit when reading offline
    /// snapshots via `--comments-json`.
    #[arg(long, value_name = "issue")]
    pub issue: Option<String>,

    /// Output directory. Restored files are written at their canonical
    /// repo-relative paths under this directory.
    #[arg(long, value_name = "dir")]
    pub out: PathBuf,

    /// Offline issue body Markdown (optional; the dashboard body is not
    /// required for restore since snapshots live in comments).
    #[arg(long = "body-file", value_name = "path")]
    pub body_file: Option<PathBuf>,

    /// Offline comments JSON from `gh issue view --json comments` (or a raw
    /// array of comment objects). When provided, restore runs without any
    /// provider call.
    #[arg(long = "comments-json", value_name = "path")]
    pub comments_json: Option<PathBuf>,

    /// Restrict restoration to a lifecycle profile. When omitted, source and
    /// plan snapshots of any profile are accepted. Overwrite of existing
    /// files is governed by the global `--force` flag.
    #[arg(long, value_enum)]
    pub profile: Option<RecordProfile>,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct RecordOpenArgs {
    /// Lifecycle profile for the record.
    #[arg(long, value_enum, default_value_t = RecordProfile::Tracking)]
    pub profile: RecordProfile,

    /// Plan bundle directory. The bundle directory contains the source,
    /// plan, and execution-state Markdown files using the
    /// `<slug>-discussion-source.md` / `<slug>-review-source.md`,
    /// `<slug>-plan.md`, and `<slug>-execution-state.md` naming
    /// convention validated by `plan-tooling validate`.
    #[arg(long, value_name = "dir")]
    pub bundle: Option<PathBuf>,

    /// Explicit source document path. Overrides bundle derivation.
    #[arg(long = "source-file", value_name = "path")]
    pub source_file: Option<PathBuf>,

    /// Explicit plan document path. Overrides bundle derivation.
    #[arg(long = "plan-file", value_name = "path")]
    pub plan_file: Option<PathBuf>,

    /// Explicit execution-state document path. Overrides bundle derivation.
    #[arg(long = "execution-state-file", value_name = "path")]
    pub execution_state_file: Option<PathBuf>,

    /// Issue title. Defaults to the plan title.
    #[arg(long, value_name = "text")]
    pub title: Option<String>,

    /// Allow opening the record even when local plan files are dirty.
    #[arg(long = "allow-dirty")]
    pub allow_dirty: bool,

    /// Label to apply at issue creation. Repeatable. Empty values are
    /// dropped. Names are passed through to `gh issue create --label`.
    #[arg(long = "label", value_name = "NAME")]
    pub labels: Vec<String>,

    /// Deterministic fixture mode. The directory is consumed instead of
    /// live provider calls.
    #[arg(long, value_name = "dir")]
    pub fixture: Option<PathBuf>,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct RecordAttachArgs {
    /// Provider issue number or full URL.
    #[arg(long, value_name = "issue")]
    pub issue: String,

    /// Lifecycle profile for the record.
    #[arg(long, value_enum, default_value_t = RecordProfile::Tracking)]
    pub profile: RecordProfile,

    /// Plan bundle directory. The bundle directory contains the source,
    /// plan, and execution-state Markdown files using the same naming
    /// convention as `record open`.
    #[arg(long, value_name = "dir")]
    pub bundle: Option<PathBuf>,

    /// Explicit source document path. Overrides bundle derivation.
    #[arg(long = "source-file", value_name = "path")]
    pub source_file: Option<PathBuf>,

    /// Explicit plan document path. Overrides bundle derivation.
    #[arg(long = "plan-file", value_name = "path")]
    pub plan_file: Option<PathBuf>,

    /// Explicit execution-state document path. Overrides bundle derivation.
    #[arg(long = "execution-state-file", value_name = "path")]
    pub execution_state_file: Option<PathBuf>,

    /// Issue title for dashboard rendering. Defaults to the plan title.
    #[arg(long, value_name = "text")]
    pub title: Option<String>,

    /// Allow attaching the record even when local plan files are dirty.
    #[arg(long = "allow-dirty")]
    pub allow_dirty: bool,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct RecordPostArgs {
    /// Provider issue number or full URL.
    #[arg(long, value_name = "issue")]
    pub issue: String,

    /// Lifecycle profile for the marker and payload.
    #[arg(long, value_enum, default_value_t = RecordProfile::Tracking)]
    pub profile: RecordProfile,

    /// Lifecycle comment kind. `source` and `plan` kinds are owned by
    /// `record open` and rejected here.
    #[arg(long, value_enum)]
    pub kind: LifecycleCommentKind,

    /// JSON file containing the structured payload `data` field.
    #[arg(long = "payload-file", value_name = "path")]
    pub payload_file: Option<PathBuf>,

    /// Markdown execution-state document for state lifecycle comments.
    #[arg(
        long = "execution-state-file",
        value_name = "path",
        conflicts_with = "summary_file"
    )]
    pub execution_state_file: Option<PathBuf>,

    /// Visible Markdown commentary appended after the structured payload.
    #[arg(long = "summary-file", value_name = "path")]
    pub summary_file: Option<PathBuf>,

    /// Task Ledger display mode for state lifecycle comments
    /// (`auto`, `collapsed`, `expanded`, `open`).
    #[arg(
        long = "task-ledger-display",
        value_enum,
        default_value_t = TaskLedgerDisplay::Auto
    )]
    pub task_ledger_display: TaskLedgerDisplay,

    /// Add a label alongside the lifecycle comment in live mode. Repeatable.
    #[arg(long = "add-label", value_name = "NAME")]
    pub add_labels: Vec<String>,

    /// Remove a label alongside the lifecycle comment in live mode.
    /// Repeatable.
    #[arg(long = "remove-label", value_name = "NAME")]
    pub remove_labels: Vec<String>,

    /// Deterministic fixture mode.
    #[arg(long, value_name = "dir")]
    pub fixture: Option<PathBuf>,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct RecordRepairDashboardArgs {
    /// Provider issue number or full URL.
    #[arg(long, value_name = "issue")]
    pub issue: Option<String>,

    /// Provider issue body Markdown (deterministic mode).
    #[arg(long = "body-file", value_name = "path")]
    pub body_file: Option<PathBuf>,

    /// JSON containing either `comments` from `gh issue view --json
    /// comments` or a raw array of comment objects (deterministic mode).
    #[arg(long = "comments-json", value_name = "path")]
    pub comments_json: Option<PathBuf>,

    /// Deterministic fixture mode.
    #[arg(long, value_name = "dir")]
    pub fixture: Option<PathBuf>,

    /// Write rendered Markdown to this path instead of editing the issue.
    #[arg(long, value_name = "path")]
    pub out: Option<PathBuf>,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct RecordCloseArgs {
    /// Provider issue number or full URL.
    #[arg(long, value_name = "issue")]
    pub issue: String,

    /// Lifecycle profile of the record being closed.
    #[arg(long, value_enum, default_value_t = RecordProfile::Tracking)]
    pub profile: RecordProfile,

    /// Linked PR reference. Repeatable. Each ref is cross-checked against
    /// the latest state payload and verified through the provider for
    /// merge status.
    #[arg(long = "linked-pr", value_name = "ref")]
    pub linked_pr: Vec<String>,

    /// Approval evidence. May be a provider comment URL or non-empty
    /// approval text.
    #[arg(long = "approval", value_name = "text")]
    pub approval: Option<String>,

    /// Plan bundle directory. Used for local source/plan commit
    /// verification when provided.
    #[arg(long, value_name = "dir")]
    pub bundle: Option<PathBuf>,

    /// Deterministic test mode: issue body Markdown.
    #[arg(long = "body-file", value_name = "path")]
    pub body_file: Option<PathBuf>,

    /// Deterministic test mode: comments JSON.
    #[arg(long = "comments-json", value_name = "path")]
    pub comments_json: Option<PathBuf>,

    /// Add a label as part of the closeout transition in live mode (e.g.
    /// `state::closed`). Repeatable.
    #[arg(long = "add-label", value_name = "NAME")]
    pub add_labels: Vec<String>,

    /// Remove a label as part of the closeout transition in live mode
    /// (e.g. earlier `state::*` markers). Repeatable.
    #[arg(long = "remove-label", value_name = "NAME")]
    pub remove_labels: Vec<String>,

    /// Deterministic fixture mode. Contains issue body, comments JSON, and
    /// PR snapshots used in place of provider lookups.
    #[arg(long, value_name = "dir")]
    pub fixture: Option<PathBuf>,

    /// Allow the linked-PR branch of the strict closeout gate to pass
    /// even when the provider only reports a single aggregate check
    /// state (no required/non-required breakdown) and that aggregate
    /// state is `failure`. Use this when you have manually verified
    /// that the failing checks are non-required. Requires
    /// `--allow-non-required-check-failure-reason`. The override and
    /// the observed non-required failures are recorded in the
    /// closeout-comment evidence block.
    #[arg(long = "allow-non-required-check-failure", default_value_t = false)]
    pub allow_non_required_check_failure: bool,

    /// Required when `--allow-non-required-check-failure` is set.
    /// Non-empty free-form text describing why the operator verified
    /// the failing checks are safe to ignore. Stored verbatim in the
    /// closeout-comment evidence block.
    #[arg(long = "allow-non-required-check-failure-reason", value_name = "text")]
    pub allow_non_required_check_failure_reason: Option<String>,
}

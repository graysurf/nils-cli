use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use super::{GroupingArgs, PrefixArgs};

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

    /// Append a canonical lifecycle comment (state, session, validation,
    /// review, or closeout) to an existing plan record issue.
    Post(Box<RecordPostArgs>),

    /// Recompute and edit the dashboard issue body from audit evidence.
    RepairDashboard(Box<RecordRepairDashboardArgs>),

    /// Close a plan record issue after the strict lifecycle gate passes.
    Close(Box<RecordCloseArgs>),

    /// Render a mutable issue dashboard for an issue-backed plan record.
    RenderDashboard(Box<RenderDashboardArgs>),

    /// Render one append-only lifecycle comment.
    RenderComment(Box<RenderCommentArgs>),

    /// Audit issue body and comments for lifecycle markers.
    Audit(Box<RecordAuditArgs>),

    /// Evaluate closeout readiness from lifecycle audit evidence.
    CloseoutGate(Box<RecordCloseoutGateArgs>),

    /// Build a dispatch ledger from plan metadata and split grouping rules.
    BuildDispatchLedger(Box<BuildDispatchLedgerArgs>),
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
pub struct RenderDashboardArgs {
    /// Lifecycle profile for the issue record.
    #[arg(long, value_enum, default_value_t = RecordProfile::Tracking)]
    pub profile: RecordProfile,

    /// Current lifecycle status shown in the dashboard.
    #[arg(long, default_value = "in-progress", value_name = "text")]
    pub status: String,

    /// Target scope shown in the dashboard.
    #[arg(
        long = "target-scope",
        default_value = "issue-backed plan execution",
        value_name = "text"
    )]
    pub target_scope: String,

    /// Current task, lane, or closeout phase.
    #[arg(long = "current", default_value = "pending", value_name = "text")]
    pub current: String,

    /// Next action shown in the dashboard.
    #[arg(long = "next-action", default_value = "pending", value_name = "text")]
    pub next_action: String,

    /// Latest validation summary or URL.
    #[arg(long, default_value = "pending", value_name = "text")]
    pub validation: String,

    /// Linked PR reference. Repeatable.
    #[arg(long = "linked-pr", value_name = "ref")]
    pub linked_pr: Vec<String>,

    /// Current blocker. Repeatable.
    #[arg(long = "blocker", value_name = "text")]
    pub blocker: Vec<String>,

    /// Review or close approval state.
    #[arg(long, default_value = "pending", value_name = "text")]
    pub approval: String,

    /// Source snapshot URL.
    #[arg(long = "source-url", value_name = "url")]
    pub source_url: Option<String>,

    /// Plan snapshot URL.
    #[arg(long = "plan-url", value_name = "url")]
    pub plan_url: Option<String>,

    /// Latest execution state URL.
    #[arg(long = "state-url", value_name = "url")]
    pub state_url: Option<String>,

    /// Latest execution session URL.
    #[arg(long = "session-url", value_name = "url")]
    pub session_url: Option<String>,

    /// Latest validation evidence URL.
    #[arg(long = "validation-url", value_name = "url")]
    pub validation_url: Option<String>,

    /// Latest review evidence URL.
    #[arg(long = "review-url", value_name = "url")]
    pub review_url: Option<String>,

    /// Closeout comment URL.
    #[arg(long = "closeout-url", value_name = "url")]
    pub closeout_url: Option<String>,

    /// Original plan title.
    #[arg(long, value_name = "text")]
    pub title: Option<String>,

    /// Provider issue URL.
    #[arg(long = "issue-url", value_name = "url")]
    pub issue_url: Option<String>,

    /// Write rendered Markdown to this path.
    #[arg(long, value_name = "path")]
    pub out: Option<PathBuf>,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct RenderCommentArgs {
    /// Lifecycle profile for the comment marker and visible metadata.
    #[arg(long, value_enum, default_value_t = RecordProfile::Tracking)]
    pub profile: RecordProfile,

    /// Lifecycle comment kind.
    #[arg(long, value_enum)]
    pub kind: LifecycleCommentKind,

    /// Source file path represented by this comment.
    #[arg(long = "path", value_name = "path")]
    pub path: Option<PathBuf>,

    /// Commit hash for snapshot comments.
    #[arg(long, value_name = "sha")]
    pub commit: Option<String>,

    /// Markdown content to embed in the comment.
    #[arg(long = "content-file", value_name = "path")]
    pub content_file: Option<PathBuf>,

    /// Override the visible heading.
    #[arg(long, value_name = "text")]
    pub title: Option<String>,

    /// Override the collapsed details summary for source/plan snapshots.
    #[arg(long = "details-summary", value_name = "text")]
    pub details_summary: Option<String>,

    /// Write rendered Markdown to this path.
    #[arg(long, value_name = "path")]
    pub out: Option<PathBuf>,
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
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct RecordCloseoutGateArgs {
    /// Provider issue body Markdown.
    #[arg(long = "body-file", value_name = "path")]
    pub body_file: Option<PathBuf>,

    /// JSON containing either `comments` from `gh issue view --json comments`
    /// or a raw array of comment objects.
    #[arg(long = "comments-json", value_name = "path")]
    pub comments_json: PathBuf,

    /// Expected profile.
    #[arg(long, value_enum, default_value_t = RecordProfile::Tracking)]
    pub profile: RecordProfile,

    /// Require an execution state whose visible status is complete.
    #[arg(long = "require-complete")]
    pub require_complete: bool,

    /// Require a session comment.
    #[arg(long = "require-session")]
    pub require_session: bool,

    /// Require validation evidence.
    #[arg(long = "require-validation")]
    pub require_validation: bool,

    /// Require review evidence.
    #[arg(long = "require-review")]
    pub require_review: bool,

    /// Require a closeout comment.
    #[arg(long = "require-closeout")]
    pub require_closeout: bool,

    /// Explicit approval comment URL or approval evidence text.
    #[arg(long = "approval", value_name = "text")]
    pub approval: Option<String>,

    /// Linked PR reference. Repeatable; checked for presence in the body or
    /// comments, not for provider merge state.
    #[arg(long = "linked-pr", value_name = "ref")]
    pub linked_pr: Vec<String>,
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

    /// Deterministic fixture mode. The directory is consumed instead of
    /// live provider calls.
    #[arg(long, value_name = "dir")]
    pub fixture: Option<PathBuf>,
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

    /// Visible Markdown commentary appended after the structured payload.
    #[arg(long = "summary-file", value_name = "path")]
    pub summary_file: Option<PathBuf>,

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

    /// Deterministic fixture mode. Contains issue body, comments JSON, and
    /// PR snapshots used in place of provider lookups.
    #[arg(long, value_name = "dir")]
    pub fixture: Option<PathBuf>,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct BuildDispatchLedgerArgs {
    /// Plan markdown path.
    #[arg(long, value_name = "path")]
    pub plan: PathBuf,

    #[command(flatten)]
    pub prefixes: PrefixArgs,

    #[command(flatten)]
    pub grouping: GroupingArgs,

    /// Write rendered Markdown to this path.
    #[arg(long, value_name = "path")]
    pub out: Option<PathBuf>,
}

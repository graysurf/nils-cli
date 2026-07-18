//! `plan-issue tracking` subcommand surface.
//!
//! Owns the run-state controller commands (`status`, `run init`,
//! `run update`, `checkpoint`, `close-ready`). The handlers live in
//! [`crate::execute`] and the data shapes live in [`crate::tracking`].

use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::Serialize;

use crate::commands::record::RecordProfile;

#[derive(Debug, Clone, Args, Serialize)]
pub struct TrackingArgs {
    #[command(subcommand)]
    pub command: TrackingCommand,
}

#[derive(Debug, Clone, Subcommand, Serialize)]
pub enum TrackingCommand {
    /// Read active payload evidence + local run state and return the
    /// reconciled FSM state without provider mutation. Old state payload
    /// formats require one-off migration/repair.
    #[command(
        after_help = "State payload replacement policy: this command targets the active payload contract only. Old state payload formats require one-off migration/repair outside the main CLI; no long-term v2 reader or mixed old/new stream reconciliation is provided."
    )]
    Status(Box<TrackingStatusArgs>),

    /// Manage a typed local run state (`run init`, `run update`).
    Run(Box<TrackingRunArgs>),

    /// Render or post checkpoint lifecycle comments derived from run state.
    Checkpoint(Box<TrackingCheckpointArgs>),

    /// Non-mutating close-readiness probe over the active payload contract.
    /// Old state payload formats require one-off migration/repair.
    #[command(name = "close-ready")]
    #[command(
        after_help = "State payload replacement policy: this command targets the active payload contract only. Old state payload formats require one-off migration/repair outside the main CLI; no long-term v2 reader or mixed old/new stream reconciliation is provided."
    )]
    CloseReady(Box<TrackingCloseReadyArgs>),
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct TrackingRunArgs {
    #[command(subcommand)]
    pub command: TrackingRunCommand,
}

#[derive(Debug, Clone, Subcommand, Serialize)]
pub enum TrackingRunCommand {
    /// Create or refresh a run-state.json document under the issue runtime
    /// root.
    Init(Box<TrackingRunInitArgs>),

    /// Update a previously-initialized run-state.json without provider
    /// mutation.
    Update(Box<TrackingRunUpdateArgs>),
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct TrackingRunInitArgs {
    /// Repository identity as `owner/repo` or a qualified provider URL.
    #[arg(long = "provider-repo", value_name = "url|owner/repo")]
    pub provider_repo: String,

    /// Issue number.
    #[arg(long, value_name = "number")]
    pub issue: u64,

    /// Lifecycle profile for the new run.
    #[arg(long, value_enum, default_value_t = RecordProfile::Tracking)]
    pub profile: RecordProfile,

    /// Plan bundle directory.
    #[arg(long, value_name = "dir")]
    pub bundle: Option<PathBuf>,

    /// Canonical execution-state Markdown.
    #[arg(long = "execution-state-file", value_name = "path")]
    pub execution_state_file: Option<PathBuf>,

    /// Selected task id.
    #[arg(long, value_name = "id")]
    pub task: Option<String>,

    /// Selected sprint number.
    #[arg(long, value_name = "number")]
    pub sprint: Option<i32>,

    /// Branch backing the run.
    #[arg(long, value_name = "name")]
    pub branch: Option<String>,

    /// Worktree path.
    #[arg(long, value_name = "path")]
    pub worktree: Option<PathBuf>,

    /// Linked PR reference (`owner/repo#number`).
    #[arg(long = "linked-pr", value_name = "ref")]
    pub linked_pr: Option<String>,

    /// Override the generated `run_id`. Useful for deterministic tests.
    #[arg(long = "run-id", value_name = "id")]
    pub run_id: Option<String>,

    /// Override the recorded timestamp (`created_at` / `updated_at`).
    /// Defaults to the current UTC time; pass an explicit value for
    /// deterministic tests/fixtures.
    #[arg(long = "now", value_name = "rfc3339")]
    pub now: Option<String>,

    /// Write to this run-state path instead of the issue runtime root.
    #[arg(long = "out", value_name = "path")]
    pub out: Option<PathBuf>,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct TrackingRunUpdateArgs {
    /// Run-state path to mutate.
    #[arg(long = "run-state", value_name = "path")]
    pub run_state: PathBuf,

    /// New phase. Optional.
    #[arg(long, value_enum)]
    pub phase: Option<RunPhaseArg>,

    /// Update the selected task id.
    #[arg(long = "selected-task", value_name = "id")]
    pub selected_task: Option<String>,

    /// Update the branch name.
    #[arg(long, value_name = "name")]
    pub branch: Option<String>,

    /// Update the linked PR reference.
    #[arg(long = "linked-pr", value_name = "ref")]
    pub linked_pr: Option<String>,

    /// Validation overall status update (`pass|partial|fail`).
    #[arg(long = "validation-overall", value_name = "status")]
    pub validation_overall: Option<String>,

    /// Validation command row update.
    #[arg(long = "validation-command", value_name = "command")]
    pub validation_command: Option<String>,

    /// Validation command status update.
    #[arg(long = "validation-status", value_name = "status")]
    pub validation_status: Option<String>,

    /// Validation evidence path.
    #[arg(long = "validation-evidence", value_name = "path")]
    pub validation_evidence: Option<String>,

    /// Review decision (`approve|request-changes|comments-only`).
    #[arg(long = "review-decision", value_name = "decision")]
    pub review_decision: Option<String>,

    /// Review lens. Repeat to record multiple lenses.
    #[arg(long = "review-lens", value_name = "lens")]
    pub review_lens: Vec<String>,

    /// Review outcome comment URL or retained evidence path.
    #[arg(long = "review-outcome-comment", value_name = "url-or-path")]
    pub review_outcome_comment: Option<String>,

    /// JSON file containing review finding rows.
    #[arg(long = "review-findings-file", value_name = "path")]
    pub review_findings_file: Option<PathBuf>,

    /// Free-form note appended to `notes`.
    #[arg(long, value_name = "text")]
    pub note: Option<String>,

    /// Override the recorded `updated_at` timestamp.
    /// Defaults to the current UTC time; pass an explicit value for
    /// deterministic tests/fixtures.
    #[arg(long = "now", value_name = "rfc3339")]
    pub now: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
pub enum RunPhaseArg {
    Initial,
    Implementing,
    Validating,
    Reviewing,
    Blocked,
    ReadyForClose,
    Closed,
}

impl RunPhaseArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Implementing => "implementing",
            Self::Validating => "validating",
            Self::Reviewing => "reviewing",
            Self::Blocked => "blocked",
            Self::ReadyForClose => "ready_for_close",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct TrackingCheckpointArgs {
    /// Repository identity for live mode. It must match the persisted run state.
    /// Runs without provider/host metadata require a qualified URL. Persisted
    /// self-hosted identities require either a matching checkout or a qualified
    /// URL that confirms the same provider, host, and repository.
    #[arg(long = "provider-repo", value_name = "url|owner/repo")]
    pub provider_repo: Option<String>,

    /// Issue number for live mode. Must match a nonzero persisted issue.
    #[arg(long, value_name = "number")]
    pub issue: Option<u64>,

    /// Lifecycle profile.
    #[arg(long, value_enum, default_value_t = RecordProfile::Tracking)]
    pub profile: RecordProfile,

    /// Run-state path.
    #[arg(long = "run-state", value_name = "path")]
    pub run_state: PathBuf,

    /// Comma-separated lifecycle roles to render (`state,session,validation,review`).
    #[arg(long, value_name = "roles", default_value = "state")]
    pub post: String,

    /// Always repair the dashboard after checkpoint posting.
    #[arg(long = "repair-dashboard", default_value_t = false)]
    pub repair_dashboard: bool,

    /// Fixture directory for deterministic issue evidence.
    #[arg(long, value_name = "dir")]
    pub fixture: Option<PathBuf>,

    /// Body file (deterministic mode).
    #[arg(long = "body-file", value_name = "path")]
    pub body_file: Option<PathBuf>,

    /// Comments JSON file (deterministic mode).
    #[arg(long = "comments-json", value_name = "path")]
    pub comments_json: Option<PathBuf>,

    /// Opt into live mutation. Without this flag, `tracking checkpoint`
    /// renders the planned comments but never mutates the provider issue.
    /// With `--live`, the controller posts one lifecycle comment per role
    /// listed in `--post` (one comment per role, mirroring `record post`
    /// semantics), preserving declaration order. On the first per-role
    /// failure it stops and returns the already-posted URLs alongside a
    /// `tracking-checkpoint-live-post-failed` blocker so the caller can
    /// decide whether to retry. Combine with `--repair-dashboard` to
    /// refresh the issue body after all roles post successfully (skipped
    /// on partial failure). Combine with `--fixture <dir>` to exercise
    /// the post path deterministically without provider mutation.
    #[arg(long = "live", default_value_t = false)]
    pub live: bool,

    /// Run the visible-completeness lint against rendered bodies.
    #[arg(long = "expect-visible", default_value_t = true)]
    pub expect_visible: bool,

    /// Write rendered comment bodies under this directory instead of the
    /// run-state `rendered/` subtree.
    #[arg(long = "rendered-out", value_name = "dir")]
    pub rendered_out: Option<PathBuf>,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct TrackingCloseReadyArgs {
    /// Repository slug.
    #[arg(long = "provider-repo", value_name = "owner/repo")]
    pub provider_repo: Option<String>,

    /// Issue number.
    #[arg(long, value_name = "number")]
    pub issue: Option<u64>,

    /// Lifecycle profile.
    #[arg(long, value_enum, default_value_t = RecordProfile::Tracking)]
    pub profile: RecordProfile,

    /// Run-state path.
    #[arg(long = "run-state", value_name = "path")]
    pub run_state: Option<PathBuf>,

    /// Linked PR reference. Repeatable.
    #[arg(long = "linked-pr", value_name = "ref")]
    pub linked_pr: Vec<String>,

    /// Approval evidence (URL or text).
    #[arg(long, value_name = "text")]
    pub approval: Option<String>,

    /// Fixture directory.
    #[arg(long, value_name = "dir")]
    pub fixture: Option<PathBuf>,

    /// Body file.
    #[arg(long = "body-file", value_name = "path")]
    pub body_file: Option<PathBuf>,

    /// Comments JSON file.
    #[arg(long = "comments-json", value_name = "path")]
    pub comments_json: Option<PathBuf>,

    /// Run the visible-completeness lint before reporting ready.
    #[arg(long = "expect-visible", default_value_t = true)]
    pub expect_visible: bool,
}

#[derive(Debug, Clone, Args, Serialize)]
pub struct TrackingStatusArgs {
    /// Repository in `owner/repo` form. Required for live mode.
    #[arg(long, value_name = "owner/repo")]
    pub provider_repo: Option<String>,

    /// Issue number. Required when reading live provider evidence.
    #[arg(long, value_name = "number")]
    pub issue: Option<u64>,

    /// Lifecycle profile filter. Defaults to `tracking`.
    #[arg(long, value_enum, default_value_t = RecordProfile::Tracking)]
    pub profile: RecordProfile,

    /// Provider issue body Markdown for deterministic mode.
    #[arg(long = "body-file", value_name = "path")]
    pub body_file: Option<PathBuf>,

    /// JSON containing the issue comments (deterministic mode).
    #[arg(long = "comments-json", value_name = "path")]
    pub comments_json: Option<PathBuf>,

    /// Fixture directory containing `body.md` and `comments.json`.
    #[arg(long, value_name = "dir")]
    pub fixture: Option<PathBuf>,

    /// Local `run-state.json` path.
    #[arg(long = "run-state", value_name = "path")]
    pub run_state: Option<PathBuf>,

    /// Plan bundle directory used to validate execution-state metadata.
    #[arg(long, value_name = "dir")]
    pub bundle: Option<PathBuf>,

    /// Also run the visible-completeness lint against the latest comment
    /// body per role.
    #[arg(long = "expect-visible", default_value_t = false)]
    pub expect_visible: bool,
}

#[cfg(test)]
mod tests {
    use super::RunPhaseArg;
    use pretty_assertions::assert_eq;

    /// `RunPhaseArg::as_str` is the run-state JSON `phase` contract (see
    /// `execute.rs`). Pin every variant's snake_case wire value so a
    /// renamed or reordered arm cannot silently change emitted run state.
    #[test]
    fn run_phase_arg_as_str_matches_wire_contract() {
        assert_eq!(RunPhaseArg::Initial.as_str(), "initial");
        assert_eq!(RunPhaseArg::Implementing.as_str(), "implementing");
        assert_eq!(RunPhaseArg::Validating.as_str(), "validating");
        assert_eq!(RunPhaseArg::Reviewing.as_str(), "reviewing");
        assert_eq!(RunPhaseArg::Blocked.as_str(), "blocked");
        assert_eq!(RunPhaseArg::ReadyForClose.as_str(), "ready_for_close");
        assert_eq!(RunPhaseArg::Closed.as_str(), "closed");
    }
}

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
    /// Read issue lifecycle evidence + local run state and return the
    /// reconciled FSM state without provider mutation.
    Status(Box<TrackingStatusArgs>),
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

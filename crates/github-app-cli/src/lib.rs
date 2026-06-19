//! `github-app-cli` — mint GitHub App installation access tokens (and list
//! installations) so automation can act under a GitHub App bot identity.
//!
//! The binary implements the workspace output contract
//! (`docs/specs/cli-output-contract-v1.md`): `--format text|json`, a versioned
//! [`nils_common::cli_contract::Envelope`], and BSD sysexits exit codes. In text
//! mode the `token` command writes the raw token to stdout (for
//! `GH_TOKEN=$(github-app-cli token ...)`); JSON mode never emits the raw token.

pub mod cli;
pub mod commands;
pub mod completion;
pub mod error;
pub mod github;
pub mod jwt;

use std::time::{SystemTime, UNIX_EPOCH};

/// Binary name, used to build `schema_version` strings for JSON envelopes.
pub const BINARY: &str = "github-app-cli";

/// Current Unix time in seconds (saturating at `0` if the clock predates the
/// epoch). Isolated here so JWT minting stays testable with a fixed clock.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

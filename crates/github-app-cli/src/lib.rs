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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_name_backs_the_schema_version_prefix() {
        assert_eq!(BINARY, "github-app-cli");
    }

    #[test]
    fn now_unix_is_a_monotonic_wall_clock_second_count() {
        let first = now_unix();
        // Well past the 2020 epoch floor: a zero here would mean the clock
        // fallback fired and JWT `iat`/`exp` claims would be nonsense.
        assert!(first > 1_600_000_000, "unexpected clock value: {first}");
        assert!(now_unix() >= first);
    }
}

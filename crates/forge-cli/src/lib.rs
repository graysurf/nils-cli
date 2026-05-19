//! Provider-neutral forge CLI library entry point.
//!
//! The binary front-end at [`crate::run`] parses argv, dispatches to one of
//! the v1 atomic ops or the `pr deliver` macro, and exits with one of the six
//! BSD sysexits constants from `nils_common::cli_contract::exit`. The
//! authoritative spec lives at
//! `crates/forge-cli/docs/specs/forge-cli-spec-v1.md`; the op catalog lives
//! at `crates/forge-cli/docs/specs/forge-cli-ops-v1.yaml`.

pub mod backend;
pub mod cli;
pub mod config;
pub mod envelope;
pub mod error;
pub mod glab_version;
pub mod ops;
pub mod provider;
pub mod validations;

use std::ffi::OsString;

/// Public entry point used by `src/main.rs`. Accepts the argv tail (excluding
/// argv[0]) and returns the exit code to pass to `process::exit`.
pub fn run(args: Vec<OsString>) -> i32 {
    cli::dispatch(args)
}

//! `secrets` — pull / add a repo's `.env` from the central SOPS store.
//!
//! This crate ports the `graysurf/secrets` bash wrapper into the workspace. It
//! is a thin orchestrator over `sops` and `git`: it never parses or renders the
//! *contents* of an encrypted store entry. The decrypted plaintext is written
//! directly to disk (mode `600`) and is never routed back through stdout or the
//! JSON envelope. See `crates/secrets/docs/README.md` for the no-secret-leak
//! contract.

pub mod cli;
pub mod completion;

pub mod runtime;
pub mod store;

pub fn run() -> i32 {
    cli::run()
}

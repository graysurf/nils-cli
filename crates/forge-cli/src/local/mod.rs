//! `Provider::Local` — in-process, file-backed backend.
//!
//! Local rides the existing GitHub op paths: every op builds the same
//! gh-style [`crate::backend::BackendCall`] for `Provider::GitHub |
//! Provider::Local` and parses the same gh-shaped JSON, but in local mode the
//! op swaps [`crate::backend::ProcessRunner`] for [`LocalRunner`], which serves
//! the call from a [`store::Store`] instead of spawning `gh`. The on-disk
//! contract is frozen in
//! `crates/plan-issue/docs/specs/local-provider-contract-v1.md`.
//!
//! The store root comes from `--store-root <path>` (overriding the
//! `FORGE_CLI_LOCAL_STORE` env); the repo slug comes from `--repo` with a
//! leading `local:` stripped. Ops that the local backend does not model
//! (e.g. `pr merge`, `pr create`) reach [`LocalRunner`] with an unrecognized
//! call and receive a clean `software_error` rather than spawning a backend.

pub mod runner;
pub mod store;

use std::path::PathBuf;

use nils_common::cli_contract::schema_version_for;

use crate::cli::{BINARY, GlobalFlags};
use crate::error::ForgeError;

pub use runner::LocalRunner;

/// Env var consulted for the local store root when `--store-root` is absent.
pub const ENV_STORE_ROOT: &str = "FORGE_CLI_LOCAL_STORE";

/// Resolve the local store root: `--store-root` wins, then
/// `$FORGE_CLI_LOCAL_STORE`. Absence is an `UNAVAILABLE 69`
/// `local_store_unconfigured` error (the local backend cannot run without one).
pub fn resolve_store_root(global: &GlobalFlags) -> Result<PathBuf, ForgeError> {
    if let Some(path) = global.store_root.as_ref() {
        return Ok(path.clone());
    }
    if let Some(value) = std::env::var_os(ENV_STORE_ROOT).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    Err(ForgeError::unavailable(
        schema_version_for(BINARY, "error", 1),
        "local_store_unconfigured",
        "provider 'local' requires a store root: pass --store-root <path> or set FORGE_CLI_LOCAL_STORE",
        None,
    ))
}

/// Resolve the local repo slug from `--repo`, stripping a leading `local:`
/// scheme. Defaults to `local` when `--repo` is absent.
pub fn resolve_slug(repo: Option<&str>) -> String {
    match repo {
        Some(raw) => raw.strip_prefix("local:").unwrap_or(raw).to_string(),
        None => "local".to_string(),
    }
}

/// Whether `--provider local` models this command. Local is a file-backed
/// issue/plan store: it serves the issue lifecycle (REAL) and the PR read
/// surface (seeded), but not PR mutation, repo/auth/label/inbox, or the
/// `pr deliver` macro. The dispatcher rejects unsupported commands up front via
/// [`unsupported_command`] so they never spawn a backend binary.
pub fn command_supported(command: &Option<crate::cli::Command>) -> bool {
    use crate::cli::{Command, IssueCommand, PrCommand};
    match command {
        Some(Command::Issue(args)) => matches!(
            &args.command,
            Some(
                IssueCommand::Create(_)
                    | IssueCommand::View(_)
                    | IssueCommand::List(_)
                    | IssueCommand::Edit(_)
                    | IssueCommand::Comment(_)
                    | IssueCommand::Close(_)
            )
        ),
        Some(Command::Pr(args)) => matches!(
            &args.command,
            Some(PrCommand::View { .. } | PrCommand::Comments(_) | PrCommand::Checks(_))
        ),
        // Activity owns its own provider seam and returns an activity-specific
        // `provider_unsupported` for Local until a file-backed implementation
        // exists.
        Some(Command::Activity(_)) => true,
        // Search likewise owns its own seam: Local reaches the op and returns a
        // search-specific `provider_unsupported`, never a silent empty result.
        Some(Command::Search(_)) => true,
        _ => false,
    }
}

/// Error returned when an unsupported command is invoked under
/// `--provider local`.
pub fn unsupported_command() -> ForgeError {
    ForgeError::provider_unsupported(
        schema_version_for(BINARY, "error", 1),
        "provider 'local' supports only the issue lifecycle \
         (create/view/list/edit/comment/close) and pr read (view/comments/checks)",
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_common::cli_contract::OutputFormat;
    use pretty_assertions::assert_eq;

    fn global(store_root: Option<PathBuf>, repo: Option<&str>) -> GlobalFlags {
        GlobalFlags {
            format: Some(OutputFormat::Json),
            remote: "origin".into(),
            provider: Some(crate::cli::ProviderFlag::Local),
            repo: repo.map(str::to_string),
            store_root,
            dry_run: false,
        }
    }

    #[test]
    fn resolve_slug_strips_local_scheme_and_defaults() {
        assert_eq!(resolve_slug(Some("local:demo")), "demo");
        assert_eq!(resolve_slug(Some("owner/repo")), "owner/repo");
        assert_eq!(resolve_slug(None), "local");
    }

    #[test]
    fn resolve_store_root_prefers_flag() {
        let g = global(Some(PathBuf::from("/tmp/store")), Some("local:demo"));
        assert_eq!(resolve_store_root(&g).unwrap(), PathBuf::from("/tmp/store"));
    }

    #[test]
    fn resolve_store_root_missing_is_unavailable() {
        // Guard against a real env var leaking into the test environment.
        let saved = std::env::var_os(ENV_STORE_ROOT);
        unsafe {
            std::env::remove_var(ENV_STORE_ROOT);
        }
        let g = global(None, Some("local:demo"));
        let err = resolve_store_root(&g).expect_err("missing store root");
        if let Some(saved) = saved {
            unsafe {
                std::env::set_var(ENV_STORE_ROOT, saved);
            }
        }
        assert_eq!(err.kind(), "local_store_unconfigured");
    }
}

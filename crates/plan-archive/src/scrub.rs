//! Backwards-compatible shim for the pre-extraction `plan_archive::scrub`
//! module path.
//!
//! The scrub implementation moved to the shared [`nils_scrub`] crate in #846.
//! This module preserves the original `plan_archive::scrub::*` surface — the
//! re-exported types and functions, the `scrub::log` submodule, and the
//! label-free `format_log` / `write_log_if_any` signatures that hard-code the
//! historical `plan-archive` scrub-log header — so existing callers keep
//! compiling without adopting the new `label`-parameterized [`nils_scrub`]
//! API.

pub use nils_scrub::{Match, PATTERN_SET, REDACTION_TOKEN, ScrubResult, pattern_ids, scrub_text};

pub use log::{format_log, write_log_if_any};

/// Scrub-log helpers preserving the original label-free signatures.
///
/// These delegate to [`nils_scrub::log`] with the `plan-archive` header label
/// so callers of the pre-extraction `plan_archive::scrub::log` path keep the
/// same byte-identical output.
pub mod log {
    use std::path::Path;

    use nils_scrub::Match;

    /// Scrub-log header label used before the `label` parameter existed.
    const LABEL: &str = "plan-archive";

    /// Format the scrub log body with the historical `plan-archive` header.
    ///
    /// Thin shim over [`nils_scrub::format_log`] that hard-codes the
    /// `plan-archive` label so the original single-argument signature is
    /// preserved.
    pub fn format_log(matches: &[Match]) -> String {
        nils_scrub::format_log(LABEL, matches)
    }

    /// Write the scrub log to `path` when `matches` is non-empty, using the
    /// historical `plan-archive` header label.
    ///
    /// Thin shim over [`nils_scrub::write_log_if_any`] preserving the original
    /// `(path, matches)` signature.
    pub fn write_log_if_any(path: &Path, matches: &[Match]) -> std::io::Result<bool> {
        nils_scrub::write_log_if_any(LABEL, path, matches)
    }
}

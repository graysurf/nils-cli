//! Deterministic plan-archive CLI shipped as part of the `nils-cli` workspace.
//!
//! See the master design at
//! `agent-runtime-kit:docs/plans/2026-05-26-plan-archive-system/plan-archive-system-discussion-source.md`
//! for the surrounding contract. Sprint 1 of the Plan 1 plan
//! (`plan-archive-nils-cli`) lands the three schema validators that the
//! later `migrate`, `refresh`, and `query` subcommands build on.

pub mod catalog;
pub mod cli;
pub mod completion;
pub mod migrate;
pub mod query;
pub mod refresh;
pub mod scrub;
pub mod validate;

pub use scrub::{Match, PATTERN_SET, REDACTION_TOKEN, ScrubResult, pattern_ids, scrub_text};
pub use validate::{
    hosts::{HostsConfig, HostsValidation, HostsValidationData, validate_hosts_yaml},
    local::{
        LocalConfig, LocalValidation, LocalValidationData, validate_local_path, validate_local_yaml,
    },
    metadata::{
        MetadataConfig, MetadataValidation, MetadataValidationData, validate_metadata_yaml,
    },
};

/// Entrypoint used by `src/main.rs`. Returns the process exit code.
pub fn run() -> i32 {
    cli::run()
}

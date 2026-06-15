//! Deterministic `evidence` CLI shipped as part of the `nils-cli` workspace.
//!
//! The evidence subsystem archives skill-usage rollups out of the local
//! `agent-out` tree into a sibling `agent-evidence-archive` repository. It
//! mirrors the module layout of the `plan-archive` crate (CLI dispatcher +
//! `nils_common` Envelope, source/archive resolver, schema validators,
//! migrate pipeline, catalog, query, completion) but most of the migrate
//! behavior is net-new: it migrates a *batch* of skill-runs, scrubs every
//! payload inline before writing (borrowing the scrub primitives from the
//! shared `nils-scrub` crate, not from `plan-archive`), commits the whole
//! batch once, and never deletes the source records (idempotency is the
//! catalog `source_digest` dedup). There is no `_index/` provider-snapshot tree — skill-usage
//! rollups carry no provider refs to fetch — so `search` is a simple
//! catalog-row substring matcher rather than a clone of `plan-archive`'s
//! full-text-over-snapshots search.
//!
//! See the validated design at
//! `agent-runtime-kit:issue-352-delivery/evidence-subsystem-spec.json`.

pub mod catalog;
pub mod cli;
pub mod completion;
pub mod discover;
pub mod migrate;
pub mod purge;
pub mod query;
pub mod record;
pub mod search;
pub mod source;
pub mod validate;

pub use nils_scrub::{
    Match, PATTERN_SET, REDACTION_TOKEN, ScrubResult, format_log, pattern_ids, scrub_text,
    write_log_if_any,
};
pub use record::{Producer, SkillUsageRecord};
pub use validate::{
    hosts::{HostsConfig, HostsValidation, HostsValidationData, validate_hosts_yaml},
    local::{
        LocalConfig, LocalValidation, LocalValidationData, validate_local_path, validate_local_yaml,
    },
    record::{RecordValidation, validate_rollup_yaml},
};

/// Entrypoint used by `src/main.rs`. Returns the process exit code.
pub fn run() -> i32 {
    cli::run()
}

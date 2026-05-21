//! `agent-runtime install` body. Plan 04 Sprint 1 Task 1.2.
//!
//! Layout:
//!
//! - [`link_map`] — parser for `targets/<product>/link-map.yaml` with
//!   per-entry validation that mirrors the JSON Schema in
//!   `agent-runtime-kit/core/docs/schemas/link-map.schema.json`.
//! - [`plan`] — builder that turns a `LinkMap` into a flat ordered
//!   `InstallPlan`, expanding `recursive: true` directory entries into
//!   one action per file.
//! - [`executor`] — walks the plan and reconciles the runtime home to
//!   the plan. Re-running on a clean install is a byte-identical no-op.
//!
//! The CLI wrapper lives in `commands::install`; Task 1.3 layers
//! `--live-home` / `--tag` / overlay flags on top without rewriting any
//! of this contract.

pub mod executor;
pub mod link_map;
pub mod plan;

use std::path::Path;
use std::time::SystemTime;

pub use executor::{AppliedChange, ApplyError, Mode};
pub use link_map::{LinkMap, LinkMapError};
pub use plan::{InstallPlan, PlanError};

/// Top-level error returned by [`run`]. Each variant wraps the typed
/// error from the contributing module so callers can match on the root
/// cause if they need to.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("link-map: {0}")]
    LinkMap(#[from] LinkMapError),
    #[error("plan: {0}")]
    Plan(#[from] PlanError),
    #[error("apply: {0}")]
    Apply(#[from] ApplyError),
}

/// Execute one install cycle. Builds the plan from the link-map at
/// `<source_root>/targets/<product>/link-map.yaml`, then either prints
/// it ([`Mode::DryRun`]) or applies it ([`Mode::Apply`]). `home` and
/// `state_home` must be absolute. `now` is injected so the backup-dir
/// timestamp stays deterministic in tests.
pub fn run(
    product: &str,
    source_root: &Path,
    home: &Path,
    state_home: &Path,
    mode: Mode,
    now: SystemTime,
) -> Result<(InstallPlan, Vec<AppliedChange>), InstallError> {
    let link_map = LinkMap::load(source_root, product)?;
    let plan = InstallPlan::build(product, source_root, home, state_home, &link_map)?;
    let changes = executor::run(&plan, mode, now)?;
    Ok((plan, changes))
}

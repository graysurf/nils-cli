//! `agent-runtime install` body. Plan 04 Sprint 1 Tasks 1.2 / 1.3.
//!
//! Layout:
//!
//! - [`link_map`] — parser for `targets/<product>/link-map.yaml` with
//!   per-entry validation that mirrors the JSON Schema in
//!   `agent-runtime-kit/core/docs/schemas/link-map.schema.json`.
//! - [`overlay`] — optional `.private/link-map.overrides.yaml` parser plus
//!   the per-entry-replace merge function. Applied before plan generation.
//! - [`plan`] — builder that turns a (post-overlay-merge) `LinkMap` into a
//!   flat ordered `InstallPlan`, expanding `recursive: true` directory
//!   entries into one action per file.
//! - [`executor`] — walks the plan and reconciles the runtime home to
//!   the plan. Re-running on a clean install is a byte-identical no-op.
//!
//! The CLI wrapper lives in `commands::install`. Task 1.3 layers
//! `--live-home`, `--tag`, and `--no-overlay` on top of the Task 1.2
//! Rust API without rewriting the contract.

pub mod executor;
pub mod link_map;
pub mod overlay;
pub mod plan;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub use executor::{AppliedChange, ApplyError, Mode};
pub use link_map::{LinkMap, LinkMapError};
pub use overlay::{LinkMapOverlay, OVERLAY_REL_PATH};
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

/// Per-run knobs threaded through to the executor. Plan 04 Sprint 1
/// Task 1.3 introduces these; later sprints (gc-backups, doctor) keep
/// extending the struct rather than the positional `run` signature.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// Optional backup-directory tag. When set and at least one backup
    /// is created during apply, a `tag-<name>` marker file is written at
    /// the backup-run root so `gc-backups` (Task 2.4) can preserve the
    /// directory across retention sweeps.
    pub tag: Option<String>,
    /// When `false`, skip the `.private/link-map.overrides.yaml` read.
    /// Defaults to `true` — overlay merge is the production path; the
    /// `--no-overlay` flag wires this to `false` for tests and
    /// reproducible drift baselines.
    pub overlay_enabled: bool,
    /// Explicit overlay file location. When `None` and `overlay_enabled`
    /// is `true`, the conventional `<source_root>/.private/link-map.overrides.yaml`
    /// is used.
    pub overlay_path: Option<PathBuf>,
}

impl Default for InstallOptions {
    /// Default knobs: overlay merge on, no tag, conventional overlay path.
    /// Overlay-on is the production path — derive(Default) would land
    /// `overlay_enabled = false` and silently turn the production behaviour
    /// off for any caller using `InstallOptions::default()`.
    fn default() -> Self {
        Self {
            tag: None,
            overlay_enabled: true,
            overlay_path: None,
        }
    }
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
    options: &InstallOptions,
) -> Result<(InstallPlan, Vec<AppliedChange>), InstallError> {
    let mut link_map = LinkMap::load(source_root, product)?;
    if options.overlay_enabled {
        let overlay_opt = match options.overlay_path.as_deref() {
            Some(path) => LinkMapOverlay::load_from(path)?,
            None => LinkMapOverlay::load_optional(source_root)?,
        };
        if let Some(overlay) = overlay_opt {
            overlay::apply(&mut link_map, &overlay)?;
        }
    }
    let plan = InstallPlan::build(product, source_root, home, state_home, &link_map)?;
    let changes = executor::run(&plan, mode, now, options.tag.as_deref())?;
    Ok((plan, changes))
}

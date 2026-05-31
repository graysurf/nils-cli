//! `agent-runtime uninstall` body. Plan 04 Sprint 2 Task 2.1.
//!
//! Uninstall walks the same link-map + overlay pipeline as `install::run`
//! to discover exactly which symlinks and managed-block surfaces the
//! installer owns, then reverses them. It never touches anything outside
//! the link map: backups under `<state_home>/backups/` survive, and so do
//! product runtime homes' `auth*`, `history*`, `sessions*`, `cache*`, and
//! `projects*` trees (those are not referenced by the link map at all).
//!
//! Idempotence is enforced at the executor: a second uninstall on a home
//! whose install map has already been removed walks the plan, sees every
//! action's destination is already absent (or already free of the managed
//! block), emits `NoOp` for each, and exits successfully without mutating
//! the filesystem.
//!
//! Restore of previously-replaced files is **delegated to** `restore-backups`
//! (Sprint 2 Task 2.2). This module never reads `<state_home>/backups/`.

pub mod executor;
pub mod plan;

use std::path::{Path, PathBuf};

pub use executor::{ApplyError, Mode, UninstalledChange};
pub use plan::{UninstallAction, UninstallPlan};

use crate::install::link_map::{LinkMap, LinkMapError};
use crate::install::overlay::{self, LinkMapOverlay, OverlaySummary};
use crate::install::plan::{InstallPlan, PlanError};

/// Top-level error from [`run`]. Mirrors `install::InstallError` so the
/// CLI can map both surfaces through identical match arms.
#[derive(Debug, thiserror::Error)]
pub enum UninstallError {
    #[error("link-map: {0}")]
    LinkMap(#[from] LinkMapError),
    #[error("plan: {0}")]
    Plan(#[from] PlanError),
    #[error("apply: {0}")]
    Apply(#[from] ApplyError),
}

/// Per-run knobs. Mirrors `install::InstallOptions` so the overlay
/// resolution path is identical — uninstall must see the same effective
/// link map the installer wrote, otherwise overlay-added entries get
/// orphaned. The `tag` knob is deliberately omitted: uninstall never
/// writes a backup-run marker.
#[derive(Debug, Clone)]
pub struct UninstallOptions {
    pub overlay_enabled: bool,
    pub overlay_path: Option<PathBuf>,
}

impl Default for UninstallOptions {
    /// Defaults: overlay merge on. `derive(Default)` would silently land
    /// `overlay_enabled = false` and skip overlay-discovered entries.
    fn default() -> Self {
        Self {
            overlay_enabled: true,
            overlay_path: None,
        }
    }
}

/// Full outcome of one `uninstall::run` cycle. `overlay` is `Some` when
/// an overlay file was merged before the plan was built; the CLI prints
/// a one-line operator-visible notice so a silently-consumed overlay
/// never masks the resulting plan.
#[derive(Debug)]
pub struct UninstallOutcome {
    pub plan: UninstallPlan,
    pub changes: Vec<UninstalledChange>,
    pub overlay: Option<OverlaySummary>,
}

/// Execute one uninstall cycle. Builds the same plan structure the
/// installer used, then translates each `PlanAction` into a `Remove*`
/// step and walks the executor. `home` must be absolute (the CLI also
/// asserts this, but the library gate is defense in depth so direct
/// callers cannot point uninstall at a relative path).
pub fn run(
    product: &str,
    source_root: &Path,
    home: &Path,
    mode: Mode,
    options: &UninstallOptions,
) -> Result<UninstallOutcome, UninstallError> {
    let mut link_map = LinkMap::load(source_root, product)?;
    let overlay_summary = merge_overlay(&mut link_map, source_root, options)?;
    // `state_home` is unused by the uninstall executor (no backups are
    // read or written). Pass an empty path placeholder so we keep the
    // existing `InstallPlan::build` signature intact.
    let install_plan = InstallPlan::build(product, source_root, home, Path::new(""), &link_map)?;
    let plan = UninstallPlan::from_install(&install_plan);
    let changes = executor::run(&plan, mode)?;
    Ok(UninstallOutcome {
        plan,
        changes,
        overlay: overlay_summary,
    })
}

fn merge_overlay(
    link_map: &mut LinkMap,
    source_root: &Path,
    options: &UninstallOptions,
) -> Result<Option<OverlaySummary>, UninstallError> {
    if !options.overlay_enabled {
        return Ok(None);
    }
    let overlay_opt = match options.overlay_path.as_deref() {
        Some(path) => LinkMapOverlay::load_from(path)?,
        None => LinkMapOverlay::load_optional(source_root)?,
    };
    match overlay_opt {
        Some(overlay) => {
            let summary = overlay::apply(link_map, &overlay)?;
            Ok(Some(summary))
        }
        None => Ok(None),
    }
}

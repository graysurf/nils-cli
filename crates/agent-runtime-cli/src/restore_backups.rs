//! `agent-runtime restore-backups` body. Plan 04 Sprint 2 Task 2.2.
//!
//! Walks one backup-run root (`<state_home>/backups/<product>/<unix-seconds>/`)
//! and restores every backed-up regular file back to the install destination
//! the installer carried it from. The same link-map + overlay pipeline as
//! `install::run` / `uninstall::run` is used so the entry-id → destination
//! mapping seen at install time is regenerated deterministically — restore
//! only needs the link-map shape, not a separate manifest, because install
//! records the entry id in the backup directory layout itself.
//!
//! ## Scope
//!
//! - Only the `FileBackedUpThenSymlinked` arm of `install::executor` writes
//!   into the backup tree; managed-block surfaces are edited in place and
//!   have no per-run snapshot. Restore therefore only addresses the symlink
//!   arm; managed-block contents are restored by editing the surface back
//!   out via `uninstall --apply` (which strips the marker pair).
//! - `tag-*` markers at the run root (written by `install --tag` for
//!   retention pinning) are skipped — they are gc-hints, not files to
//!   restore.
//!
//! ## Non-coverage (advisory, deferred)
//!
//! - Recursive-tree link-map entries that backed up multiple files into the
//!   same `<entry_id>/<basename>` slot lose the relative subpath at install
//!   time (install's `move_to_backup` uses `dest.file_name()` only). Restore
//!   uses a regenerated `InstallPlan` to map each backup file back; when
//!   exactly one `PlanAction::Symlink` matches `(entry_id, dest.file_name())`
//!   it restores cleanly. Ambiguity (multiple matches) is recorded as
//!   `RestoreSkippedAmbiguous` — `gc-backups` ages these out the same as
//!   any other backup.

pub mod executor;
pub mod plan;

use std::path::{Path, PathBuf};

pub use executor::{ApplyError, Mode, RestoredChange};
pub use plan::{BackupRunSelector, RestoreAction, RestorePlan, RestorePlanError};

use crate::install::link_map::{LinkMap, LinkMapError};
use crate::install::overlay::{self, LinkMapOverlay, OverlaySummary};
use crate::install::plan::{InstallPlan, PlanError};

/// Top-level error from [`run`]. Mirrors `install::InstallError` and
/// `uninstall::UninstallError` so the CLI dispatches through identical
/// match arms.
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error("link-map: {0}")]
    LinkMap(#[from] LinkMapError),
    #[error("install-plan: {0}")]
    Plan(#[from] PlanError),
    #[error("restore-plan: {0}")]
    RestorePlan(#[from] RestorePlanError),
    #[error("apply: {0}")]
    Apply(#[from] ApplyError),
    #[error("no backup run found under {root}: selector={selector:?}; available={available:?}")]
    NoBackupRun {
        root: PathBuf,
        selector: BackupRunSelector,
        available: Vec<u64>,
    },
}

/// Per-run knobs. Defaults: overlay merge on, no surface filter, selector
/// resolves to the most recent backup run.
#[derive(Debug, Clone)]
pub struct RestoreOptions {
    pub selector: BackupRunSelector,
    /// Filter to a single link-map entry id. Default `None` = restore
    /// every backup file in the run.
    pub surface: Option<String>,
    pub overlay_enabled: bool,
    pub overlay_path: Option<PathBuf>,
}

impl Default for RestoreOptions {
    /// `derive(Default)` would silently land `overlay_enabled = false` and
    /// skip overlay-discovered entries from restore — same defensive
    /// pattern as `InstallOptions` / `UninstallOptions`.
    fn default() -> Self {
        Self {
            selector: BackupRunSelector::Latest,
            surface: None,
            overlay_enabled: true,
            overlay_path: None,
        }
    }
}

/// Full outcome of one `restore_backups::run` cycle. `backup_run` is the
/// resolved absolute path to the run that was inspected so the CLI can
/// echo it back to operators — useful when `--from latest` was specified.
#[derive(Debug)]
pub struct RestoreOutcome {
    pub plan: RestorePlan,
    pub changes: Vec<RestoredChange>,
    pub overlay: Option<OverlaySummary>,
    pub backup_run: PathBuf,
}

/// Execute one restore cycle for `product`. The backup-run path is
/// resolved from `<state_home>/backups/<product>/` according to
/// `options.selector`; the install plan is regenerated from the link map
/// so each backup file's `entry_id` can be mapped back to its original
/// install destination.
pub fn run(
    product: &str,
    source_root: &Path,
    home: &Path,
    state_home: &Path,
    mode: Mode,
    options: &RestoreOptions,
) -> Result<RestoreOutcome, RestoreError> {
    let mut link_map = LinkMap::load(source_root, product)?;
    let overlay_summary = merge_overlay(&mut link_map, source_root, options)?;
    let install_plan = InstallPlan::build(product, source_root, home, state_home, &link_map)?;

    let product_root = state_home.join("backups").join(product);
    let backup_run = resolve_backup_run(&product_root, &options.selector)?;

    let plan =
        RestorePlan::from_backup_run(&backup_run, &install_plan, options.surface.as_deref())?;
    let changes = executor::run(&plan, mode)?;
    Ok(RestoreOutcome {
        plan,
        changes,
        overlay: overlay_summary,
        backup_run,
    })
}

/// List the unix-seconds directories that look like backup runs under
/// `<state_home>/backups/<product>/`, sorted ascending. Used by the CLI
/// to print the available-timestamp list when `--from` is missing or
/// references a timestamp that does not exist.
pub fn list_available_timestamps(state_home: &Path, product: &str) -> Vec<u64> {
    let product_root = state_home.join("backups").join(product);
    enumerate_timestamps(&product_root)
}

fn merge_overlay(
    link_map: &mut LinkMap,
    source_root: &Path,
    options: &RestoreOptions,
) -> Result<Option<OverlaySummary>, RestoreError> {
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

fn resolve_backup_run(
    product_root: &Path,
    selector: &BackupRunSelector,
) -> Result<PathBuf, RestoreError> {
    let timestamps = enumerate_timestamps(product_root);
    match selector {
        BackupRunSelector::Latest => match timestamps.last() {
            Some(ts) => Ok(product_root.join(ts.to_string())),
            None => Err(RestoreError::NoBackupRun {
                root: product_root.to_path_buf(),
                selector: selector.clone(),
                available: timestamps,
            }),
        },
        BackupRunSelector::Exact(target) => {
            if timestamps.iter().any(|t| t == target) {
                Ok(product_root.join(target.to_string()))
            } else {
                Err(RestoreError::NoBackupRun {
                    root: product_root.to_path_buf(),
                    selector: selector.clone(),
                    available: timestamps,
                })
            }
        }
    }
}

fn enumerate_timestamps(product_root: &Path) -> Vec<u64> {
    let read_dir = match std::fs::read_dir(product_root) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut timestamps: Vec<u64> = read_dir
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| name.parse::<u64>().ok())
        .collect();
    timestamps.sort();
    timestamps
}

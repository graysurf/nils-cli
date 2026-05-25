//! `agent-runtime prune-stale` body.
//!
//! The command scans only install-map-owned runtime-home roots, then removes
//! stale symlinks whose targets are lexically under the selected source root.
//! Ambiguous user-owned files, non-empty directories, and foreign symlinks are
//! reported but never deleted.

use crate::install::link_map::{LinkMap, LinkMapError};
use crate::install::overlay::{self, LinkMapOverlay, OverlaySummary};
use crate::install::plan::{InstallPlan, PlanAction, PlanError};
use crate::live_surface;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    DryRun,
    Apply,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::DryRun => "dry-run",
            Mode::Apply => "apply",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PruneOptions {
    pub overlay_enabled: bool,
    pub overlay_path: Option<PathBuf>,
}

impl Default for PruneOptions {
    fn default() -> Self {
        Self {
            overlay_enabled: true,
            overlay_path: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum PruneError {
    #[error("link-map: {0}")]
    LinkMap(#[from] LinkMapError),
    #[error("plan: {0}")]
    Plan(#[from] PlanError),
    #[error("io error at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Serialize)]
pub struct PruneOutcome {
    pub product: String,
    pub source_root: PathBuf,
    pub live_home: PathBuf,
    pub mode: Mode,
    pub changes: Vec<PruneChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overlay: Option<OverlaySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PruneChange {
    WouldRemoveSymlink {
        rel_path: PathBuf,
        path: PathBuf,
        target: PathBuf,
    },
    RemovedSymlink {
        rel_path: PathBuf,
        path: PathBuf,
        target: PathBuf,
    },
    NoOpSymlink {
        rel_path: PathBuf,
        path: PathBuf,
        target: PathBuf,
    },
    WouldRemoveEmptyDirectory {
        rel_path: PathBuf,
        path: PathBuf,
    },
    RemovedEmptyDirectory {
        rel_path: PathBuf,
        path: PathBuf,
    },
    NoOpEmptyDirectory {
        rel_path: PathBuf,
        path: PathBuf,
    },
    SkippedForeignSymlink {
        rel_path: PathBuf,
        path: PathBuf,
        target: PathBuf,
    },
    SkippedRegularFile {
        rel_path: PathBuf,
        path: PathBuf,
    },
    SkippedNonEmptyDirectory {
        rel_path: PathBuf,
        path: PathBuf,
    },
}

impl PruneChange {
    pub fn is_change(&self) -> bool {
        matches!(
            self,
            PruneChange::WouldRemoveSymlink { .. }
                | PruneChange::RemovedSymlink { .. }
                | PruneChange::WouldRemoveEmptyDirectory { .. }
                | PruneChange::RemovedEmptyDirectory { .. }
        )
    }

    pub fn is_skip(&self) -> bool {
        matches!(
            self,
            PruneChange::SkippedForeignSymlink { .. }
                | PruneChange::SkippedRegularFile { .. }
                | PruneChange::SkippedNonEmptyDirectory { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Candidate {
    Symlink {
        rel_path: PathBuf,
        path: PathBuf,
        target: PathBuf,
    },
    EmptyDirectory {
        rel_path: PathBuf,
        path: PathBuf,
    },
    SkippedForeignSymlink {
        rel_path: PathBuf,
        path: PathBuf,
        target: PathBuf,
    },
    SkippedRegularFile {
        rel_path: PathBuf,
        path: PathBuf,
    },
    SkippedNonEmptyDirectory {
        rel_path: PathBuf,
        path: PathBuf,
    },
}

pub fn run(
    product: &str,
    source_root: &Path,
    live_home: &Path,
    mode: Mode,
    options: &PruneOptions,
) -> Result<PruneOutcome, PruneError> {
    let mut link_map = LinkMap::load(source_root, product)?;
    let overlay = merge_overlay(&mut link_map, source_root, options)?;
    let plan = InstallPlan::build(product, source_root, live_home, Path::new(""), &link_map)?;
    let expected = expected_paths_from_plan(live_home, &plan);
    let scan_roots = live_surface::scan_roots(&link_map);
    let candidates = collect_candidates(live_home, source_root, &expected, &scan_roots)?;
    let changes = match mode {
        Mode::DryRun => candidates.into_iter().map(dry_run_change).collect(),
        Mode::Apply => apply_candidates(candidates)?,
    };

    Ok(PruneOutcome {
        product: product.to_string(),
        source_root: source_root.to_path_buf(),
        live_home: live_home.to_path_buf(),
        mode,
        changes,
        overlay,
    })
}

fn merge_overlay(
    link_map: &mut LinkMap,
    source_root: &Path,
    options: &PruneOptions,
) -> Result<Option<OverlaySummary>, PruneError> {
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

fn expected_paths_from_plan(live_home: &Path, plan: &InstallPlan) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    for action in &plan.actions {
        let PlanAction::Symlink { dest, .. } = action else {
            continue;
        };
        if let Ok(rel) = dest.strip_prefix(live_home) {
            out.insert(rel.to_path_buf());
        }
    }
    out
}

fn collect_candidates(
    live_home: &Path,
    source_root: &Path,
    expected: &BTreeSet<PathBuf>,
    scan_roots: &BTreeSet<PathBuf>,
) -> Result<Vec<Candidate>, PruneError> {
    let mut candidates = Vec::new();
    for rel_root in scan_roots {
        collect_dir(
            live_home,
            source_root,
            expected,
            rel_root,
            rel_root,
            true,
            &mut candidates,
        )?;
    }
    candidates.sort_by(candidate_order);
    candidates.dedup();
    Ok(candidates)
}

fn collect_dir(
    live_home: &Path,
    source_root: &Path,
    expected: &BTreeSet<PathBuf>,
    scan_root: &Path,
    rel_dir: &Path,
    is_scan_root: bool,
    candidates: &mut Vec<Candidate>,
) -> Result<bool, PruneError> {
    let dir = live_home.join(rel_dir);
    let Ok(meta) = fs::symlink_metadata(&dir) else {
        return Ok(false);
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Ok(false);
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|source| PruneError::Io {
        path: dir.clone(),
        source,
    })? {
        let entry = entry.map_err(|source| PruneError::Io {
            path: dir.clone(),
            source,
        })?;
        entries.push(entry.file_name());
    }
    entries.sort();

    let mut all_children_removable = true;
    let mut saw_removable_child = false;
    for name in entries {
        let child_rel = rel_dir.join(&name);
        let child_abs = live_home.join(&child_rel);
        if expected.contains(&child_rel) || live_surface::ignored_live_file(&child_rel) {
            all_children_removable = false;
            continue;
        }
        let Ok(child_meta) = fs::symlink_metadata(&child_abs) else {
            continue;
        };
        if child_meta.file_type().is_symlink() {
            let target = fs::read_link(&child_abs).map_err(|source| PruneError::Io {
                path: child_abs.clone(),
                source,
            })?;
            if symlink_target_is_owned(source_root, &target) {
                candidates.push(Candidate::Symlink {
                    rel_path: child_rel,
                    path: child_abs,
                    target,
                });
                saw_removable_child = true;
            } else {
                candidates.push(Candidate::SkippedForeignSymlink {
                    rel_path: child_rel,
                    path: child_abs,
                    target,
                });
                all_children_removable = false;
            }
        } else if child_meta.is_file() {
            candidates.push(Candidate::SkippedRegularFile {
                rel_path: child_rel,
                path: child_abs,
            });
            all_children_removable = false;
        } else if child_meta.is_dir() {
            let child_removable = collect_dir(
                live_home,
                source_root,
                expected,
                scan_root,
                &child_rel,
                false,
                candidates,
            )?;
            if !child_removable {
                all_children_removable = false;
            } else {
                saw_removable_child = true;
            }
        } else {
            all_children_removable = false;
        }
    }

    if is_scan_root {
        return Ok(false);
    }

    if all_children_removable && saw_removable_child {
        candidates.push(Candidate::EmptyDirectory {
            rel_path: rel_dir.to_path_buf(),
            path: dir,
        });
        return Ok(true);
    }

    if !has_expected_descendant(expected, rel_dir) && rel_dir.starts_with(scan_root) {
        candidates.push(Candidate::SkippedNonEmptyDirectory {
            rel_path: rel_dir.to_path_buf(),
            path: dir,
        });
    }
    Ok(false)
}

fn has_expected_descendant(expected: &BTreeSet<PathBuf>, rel_dir: &Path) -> bool {
    expected.iter().any(|path| path.starts_with(rel_dir))
}

fn dry_run_change(candidate: Candidate) -> PruneChange {
    match candidate {
        Candidate::Symlink {
            rel_path,
            path,
            target,
        } => PruneChange::WouldRemoveSymlink {
            rel_path,
            path,
            target,
        },
        Candidate::EmptyDirectory { rel_path, path } => {
            PruneChange::WouldRemoveEmptyDirectory { rel_path, path }
        }
        Candidate::SkippedForeignSymlink {
            rel_path,
            path,
            target,
        } => PruneChange::SkippedForeignSymlink {
            rel_path,
            path,
            target,
        },
        Candidate::SkippedRegularFile { rel_path, path } => {
            PruneChange::SkippedRegularFile { rel_path, path }
        }
        Candidate::SkippedNonEmptyDirectory { rel_path, path } => {
            PruneChange::SkippedNonEmptyDirectory { rel_path, path }
        }
    }
}

fn apply_candidates(candidates: Vec<Candidate>) -> Result<Vec<PruneChange>, PruneError> {
    let mut symlinks = Vec::new();
    let mut dirs = Vec::new();
    let mut skips = Vec::new();
    for candidate in candidates {
        match candidate {
            Candidate::Symlink { .. } => symlinks.push(candidate),
            Candidate::EmptyDirectory { .. } => dirs.push(candidate),
            Candidate::SkippedForeignSymlink { .. }
            | Candidate::SkippedRegularFile { .. }
            | Candidate::SkippedNonEmptyDirectory { .. } => skips.push(dry_run_change(candidate)),
        }
    }
    dirs.sort_by(|a, b| {
        candidate_depth(b)
            .cmp(&candidate_depth(a))
            .then_with(|| candidate_order(a, b))
    });

    let mut changes = Vec::new();
    for candidate in symlinks {
        let Candidate::Symlink {
            rel_path,
            path,
            target,
        } = candidate
        else {
            unreachable!();
        };
        match fs::remove_file(&path) {
            Ok(()) => changes.push(PruneChange::RemovedSymlink {
                rel_path,
                path,
                target,
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                changes.push(PruneChange::NoOpSymlink {
                    rel_path,
                    path,
                    target,
                });
            }
            Err(source) => return Err(PruneError::Io { path, source }),
        }
    }
    for candidate in dirs {
        let Candidate::EmptyDirectory { rel_path, path } = candidate else {
            unreachable!();
        };
        match fs::remove_dir(&path) {
            Ok(()) => changes.push(PruneChange::RemovedEmptyDirectory { rel_path, path }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                changes.push(PruneChange::NoOpEmptyDirectory { rel_path, path });
            }
            Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                changes.push(PruneChange::SkippedNonEmptyDirectory { rel_path, path });
            }
            Err(source) => return Err(PruneError::Io { path, source }),
        }
    }
    changes.extend(skips);
    changes.sort_by(change_order);
    Ok(changes)
}

fn candidate_depth(candidate: &Candidate) -> usize {
    candidate_rel_path(candidate).components().count()
}

fn candidate_order(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    candidate_rel_path(a)
        .cmp(candidate_rel_path(b))
        .then_with(|| candidate_rank(a).cmp(&candidate_rank(b)))
}

fn candidate_rank(candidate: &Candidate) -> u8 {
    match candidate {
        Candidate::Symlink { .. } => 0,
        Candidate::EmptyDirectory { .. } => 1,
        Candidate::SkippedForeignSymlink { .. } => 2,
        Candidate::SkippedRegularFile { .. } => 3,
        Candidate::SkippedNonEmptyDirectory { .. } => 4,
    }
}

fn candidate_rel_path(candidate: &Candidate) -> &Path {
    match candidate {
        Candidate::Symlink { rel_path, .. }
        | Candidate::EmptyDirectory { rel_path, .. }
        | Candidate::SkippedForeignSymlink { rel_path, .. }
        | Candidate::SkippedRegularFile { rel_path, .. }
        | Candidate::SkippedNonEmptyDirectory { rel_path, .. } => rel_path,
    }
}

fn change_order(a: &PruneChange, b: &PruneChange) -> std::cmp::Ordering {
    change_rel_path(a)
        .cmp(change_rel_path(b))
        .then_with(|| change_rank(a).cmp(&change_rank(b)))
}

fn change_rank(change: &PruneChange) -> u8 {
    match change {
        PruneChange::WouldRemoveSymlink { .. } | PruneChange::RemovedSymlink { .. } => 0,
        PruneChange::NoOpSymlink { .. } => 1,
        PruneChange::WouldRemoveEmptyDirectory { .. }
        | PruneChange::RemovedEmptyDirectory { .. } => 2,
        PruneChange::NoOpEmptyDirectory { .. } => 3,
        PruneChange::SkippedForeignSymlink { .. } => 4,
        PruneChange::SkippedRegularFile { .. } => 5,
        PruneChange::SkippedNonEmptyDirectory { .. } => 6,
    }
}

fn change_rel_path(change: &PruneChange) -> &Path {
    match change {
        PruneChange::WouldRemoveSymlink { rel_path, .. }
        | PruneChange::RemovedSymlink { rel_path, .. }
        | PruneChange::NoOpSymlink { rel_path, .. }
        | PruneChange::WouldRemoveEmptyDirectory { rel_path, .. }
        | PruneChange::RemovedEmptyDirectory { rel_path, .. }
        | PruneChange::NoOpEmptyDirectory { rel_path, .. }
        | PruneChange::SkippedForeignSymlink { rel_path, .. }
        | PruneChange::SkippedRegularFile { rel_path, .. }
        | PruneChange::SkippedNonEmptyDirectory { rel_path, .. } => rel_path,
    }
}

fn symlink_target_is_owned(source_root: &Path, target: &Path) -> bool {
    if !target.is_absolute() {
        return false;
    }
    let source_root = normalize_absolute_path(source_root);
    let target = normalize_absolute_path(target);
    target.starts_with(source_root)
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

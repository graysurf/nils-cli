//! Install plan: the deterministic struct that both the dry-run printer
//! and the apply executor consume. Plan 04 Sprint 1 Task 1.2.
//!
//! The builder walks a [`LinkMap`] in declared order, expands
//! `recursive: true` directory entries into one [`PlanAction`] per file,
//! and produces a flat list. Order is preserved — the executor walks the
//! list top to bottom. Re-running against an unchanged tree produces an
//! empty change set (idempotence is enforced at apply time by checking
//! the current state of each destination before mutating).

use super::link_map::{CommentStyle, EntryKind, LinkEntry, LinkMap};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("source for entry `{id}` does not exist: {path}")]
    MissingSource { id: String, path: PathBuf },
    #[error("io error walking entry `{id}` source `{path}`: {source}")]
    Walk {
        id: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("destination for entry `{id}` is not relative: {path}")]
    NonRelativeDestination { id: String, path: PathBuf },
    #[error("source for entry `{id}` is not relative: {path}")]
    NonRelativeSource { id: String, path: PathBuf },
}

/// Single executable step the apply executor will run.
#[derive(Debug, Clone)]
pub enum PlanAction {
    /// Replace `dest` with a symlink pointing at `source`. Idempotent
    /// when `dest` already points at `source`. Pre-existing regular files
    /// at `dest` are backed up into `<state_home>/backups/...` first.
    Symlink {
        entry_id: String,
        source: PathBuf,
        dest: PathBuf,
        /// True when `dest` would replace a regular file (not a symlink).
        /// Drives backup behaviour at apply time.
        requires_backup: bool,
    },
    /// Write or replace a managed block in a config file.
    ManagedBlock {
        entry_id: String,
        config_file: PathBuf,
        surface: String,
        comment_style: CommentStyle,
        body: String,
    },
}

/// Resolved install plan. Each entry in `actions` is ready to run; the
/// dry-run printer formats them, the apply executor mutates the
/// filesystem. `home` and `source_root` are kept absolute so the plan
/// transcript is unambiguous in tests.
#[derive(Debug, Clone)]
pub struct InstallPlan {
    pub product: String,
    pub source_root: PathBuf,
    pub home: PathBuf,
    pub state_home: PathBuf,
    pub actions: Vec<PlanAction>,
}

impl InstallPlan {
    /// Build the plan from a parsed link-map. `source_root` must be the
    /// canonicalised agent-runtime-kit checkout root; `home` is the
    /// product runtime home (`~/.codex`, `~/.claude`, or a sandbox). Both
    /// must be absolute.
    pub fn build(
        product: &str,
        source_root: &Path,
        home: &Path,
        state_home: &Path,
        link_map: &LinkMap,
    ) -> Result<Self, PlanError> {
        let mut actions = Vec::new();
        for entry in &link_map.entries {
            let dest_rel = Path::new(&entry.destination);
            if dest_rel.is_absolute() {
                return Err(PlanError::NonRelativeDestination {
                    id: entry.id.clone(),
                    path: dest_rel.to_path_buf(),
                });
            }
            match entry.kind {
                EntryKind::SymlinkedFile => {
                    expand_symlinked_file(entry, source_root, home, &mut actions)?;
                }
                EntryKind::PluginManifestCopy | EntryKind::BackedUpOnReplace => {
                    expand_single_symlink(entry, source_root, home, &mut actions)?;
                }
                EntryKind::ManagedBlock => {
                    let dest_abs = home.join(dest_rel);
                    actions.push(PlanAction::ManagedBlock {
                        entry_id: entry.id.clone(),
                        config_file: dest_abs,
                        surface: entry
                            .surface
                            .clone()
                            .expect("validate() guarantees surface for managed-block"),
                        comment_style: entry
                            .comment_style
                            .expect("validate() guarantees comment_style for managed-block"),
                        body: entry
                            .body_template
                            .clone()
                            .expect("validate() guarantees body_template for managed-block"),
                    });
                }
            }
        }
        Ok(Self {
            product: product.to_string(),
            source_root: source_root.to_path_buf(),
            home: home.to_path_buf(),
            state_home: state_home.to_path_buf(),
            actions,
        })
    }
}

fn require_relative_source(entry: &LinkEntry) -> Result<&str, PlanError> {
    let s = entry
        .source
        .as_deref()
        .expect("validate() guarantees source for non-managed-block kinds");
    if Path::new(s).is_absolute() {
        return Err(PlanError::NonRelativeSource {
            id: entry.id.clone(),
            path: PathBuf::from(s),
        });
    }
    Ok(s)
}

fn expand_single_symlink(
    entry: &LinkEntry,
    source_root: &Path,
    home: &Path,
    out: &mut Vec<PlanAction>,
) -> Result<(), PlanError> {
    let source_rel = require_relative_source(entry)?;
    let source_abs = source_root.join(source_rel);
    if !source_abs.exists() {
        return Err(PlanError::MissingSource {
            id: entry.id.clone(),
            path: source_abs,
        });
    }
    let dest_abs = home.join(&entry.destination);
    let requires_backup = is_regular_non_symlink(&dest_abs);
    out.push(PlanAction::Symlink {
        entry_id: entry.id.clone(),
        source: source_abs,
        dest: dest_abs,
        requires_backup,
    });
    Ok(())
}

fn expand_symlinked_file(
    entry: &LinkEntry,
    source_root: &Path,
    home: &Path,
    out: &mut Vec<PlanAction>,
) -> Result<(), PlanError> {
    let source_rel = require_relative_source(entry)?;
    let source_abs = source_root.join(source_rel);
    if !source_abs.exists() {
        return Err(PlanError::MissingSource {
            id: entry.id.clone(),
            path: source_abs,
        });
    }
    if !entry.recursive {
        let dest_abs = home.join(&entry.destination);
        let requires_backup = is_regular_non_symlink(&dest_abs);
        out.push(PlanAction::Symlink {
            entry_id: entry.id.clone(),
            source: source_abs,
            dest: dest_abs,
            requires_backup,
        });
        return Ok(());
    }
    // Recursive: walk the source directory and emit one symlink per
    // file (sorted, so plan output stays deterministic across hosts).
    let mut files = Vec::new();
    collect_files(&source_abs, &source_abs, &mut files).map_err(|source| PlanError::Walk {
        id: entry.id.clone(),
        path: source_abs.clone(),
        source,
    })?;
    files.sort();
    for rel in files {
        let abs_source = source_abs.join(&rel);
        let abs_dest = home.join(&entry.destination).join(&rel);
        let requires_backup = is_regular_non_symlink(&abs_dest);
        out.push(PlanAction::Symlink {
            entry_id: entry.id.clone(),
            source: abs_source,
            dest: abs_dest,
            requires_backup,
        });
    }
    Ok(())
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    if dir.is_file() {
        // Caller passes a file path with `dir == file`: emit "".
        if let Ok(rel) = dir.strip_prefix(root) {
            out.push(rel.to_path_buf());
        }
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_path_buf())
                .unwrap_or(path.clone());
            out.push(rel);
        }
    }
    Ok(())
}

fn is_regular_non_symlink(p: &Path) -> bool {
    match std::fs::symlink_metadata(p) {
        Ok(meta) => meta.file_type().is_file() && !meta.file_type().is_symlink(),
        Err(_) => false,
    }
}

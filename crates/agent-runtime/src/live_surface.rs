//! Shared live runtime-home surface helpers.
//!
//! `audit-drift extra` and `prune-stale` must agree on which install-map
//! destinations are expected and which runtime-home roots are safe to scan.

use crate::install::link_map::{EntryKind, LinkMap};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub fn expected_live_paths(source_root: &Path, link_map: &LinkMap) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    for entry in &link_map.entries {
        let Some(dest) = clean_rel_path(&entry.destination) else {
            continue;
        };
        match entry.kind {
            EntryKind::SymlinkedFile if entry.recursive => {
                let Some(source) = entry.source.as_deref().and_then(clean_rel_path) else {
                    continue;
                };
                let source_abs = source_root.join(source);
                if source_abs.is_dir() {
                    for rel in source_files(&source_abs) {
                        out.insert(dest.join(rel));
                    }
                } else if source_abs.exists() {
                    out.insert(dest);
                }
            }
            EntryKind::SymlinkedFile
            | EntryKind::PluginManifestCopy
            | EntryKind::BackedUpOnReplace
            | EntryKind::ManagedBlock => {
                out.insert(dest);
            }
        }
    }
    out
}

pub fn scan_roots(link_map: &LinkMap) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    for entry in &link_map.entries {
        let Some(dest) = clean_rel_path(&entry.destination) else {
            continue;
        };
        match entry.kind {
            EntryKind::SymlinkedFile if entry.recursive => {
                out.insert(dest);
            }
            EntryKind::SymlinkedFile
            | EntryKind::PluginManifestCopy
            | EntryKind::BackedUpOnReplace => {
                if let Some(parent) = dest.parent()
                    && !parent.as_os_str().is_empty()
                {
                    out.insert(parent.to_path_buf());
                }
            }
            EntryKind::ManagedBlock => {}
        }
    }
    out
}

pub fn live_files_under_roots(
    live_home: &Path,
    roots: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, std::io::Error> {
    let mut out = BTreeSet::new();
    for rel_root in roots {
        collect_live_files(live_home, rel_root, &mut out)?;
    }
    Ok(out)
}

pub fn clean_rel_path(path: impl AsRef<str>) -> Option<PathBuf> {
    let path = Path::new(path.as_ref());
    if path.as_os_str().is_empty() || path.is_absolute() {
        return None;
    }
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    Some(path.to_path_buf())
}

pub fn ignored_live_file(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some(".DS_Store")
}

fn source_files(root: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    collect_source_files(root, root, &mut out);
    out
}

fn collect_source_files(root: &Path, dir: &Path, out: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_source_files(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.insert(rel.to_path_buf());
        }
    }
}

fn collect_live_files(
    live_home: &Path,
    rel: &Path,
    out: &mut BTreeSet<PathBuf>,
) -> Result<(), std::io::Error> {
    let path = live_home.join(rel);
    let Ok(meta) = fs::symlink_metadata(&path) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() || meta.is_file() {
        out.insert(rel.to_path_buf());
        return Ok(());
    }
    if !meta.is_dir() {
        return Ok(());
    }

    let entries = fs::read_dir(&path)?;
    for entry in entries {
        let entry = entry?;
        collect_live_files(live_home, &rel.join(entry.file_name()), out)?;
    }
    Ok(())
}

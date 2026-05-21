//! Extra live-surface drift class.
//!
//! The class compares the current product runtime home against the
//! install map. It only scans roots that the install map owns, so a
//! user's unrelated runtime state does not become audit input.

use crate::audit_drift::{DriftReport, Finding, Severity};
use crate::install::link_map::{EntryKind, LinkMap, LinkMapError};
use crate::render::manifest::{ManifestSet, ProductRoot, RuntimeRootsManifest, SourceRoot};
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const CLASS: &str = "extra";

pub fn check(
    root: &SourceRoot,
    manifests: Option<&ManifestSet>,
    product: &str,
    report: &mut DriftReport,
) -> Result<()> {
    let Some(manifests) = manifests else {
        return Ok(());
    };
    let Some(product_root) = product_root(&manifests.runtime_roots, product) else {
        return Ok(());
    };
    let live_home = resolve_live_home(product_root);
    if !live_home.is_absolute() || !live_home.exists() {
        return Ok(());
    }
    let link_map = match LinkMap::load(root.path(), product) {
        Ok(link_map) => link_map,
        Err(LinkMapError::Missing { .. }) => return Ok(()),
        Err(err) => return Err(err.into()),
    };

    let expected = expected_live_paths(root.path(), &link_map);
    let scan_roots = scan_roots(&link_map);
    if scan_roots.is_empty() {
        return Ok(());
    }

    let live_files = live_files_under_roots(&live_home, &scan_roots)?;
    for rel in live_files {
        if expected.contains(&rel) || ignored_live_file(&rel) {
            continue;
        }
        report.push(Finding {
            class: CLASS,
            severity: Severity::Warn,
            product: Some(product.to_string()),
            path: rel.clone(),
            message: format!(
                "live runtime surface exists under an install-map root but is not tracked by the install map: {}",
                rel.display()
            ),
        });
    }

    Ok(())
}

fn product_root<'a>(
    runtime_roots: &'a RuntimeRootsManifest,
    product: &str,
) -> Option<&'a ProductRoot> {
    match product {
        "codex" => Some(&runtime_roots.products.codex),
        "claude" => Some(&runtime_roots.products.claude),
        _ => None,
    }
}

fn resolve_live_home(root: &ProductRoot) -> PathBuf {
    let env: BTreeMap<String, String> = std::env::vars().collect();
    PathBuf::from(expand_env_vars(&root.live_home, &env))
}

fn expected_live_paths(source_root: &Path, link_map: &LinkMap) -> BTreeSet<PathBuf> {
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

fn scan_roots(link_map: &LinkMap) -> BTreeSet<PathBuf> {
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

fn live_files_under_roots(
    live_home: &Path,
    roots: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>> {
    let mut out = BTreeSet::new();
    for rel_root in roots {
        collect_live_files(live_home, rel_root, &mut out)?;
    }
    Ok(out)
}

fn collect_live_files(live_home: &Path, rel: &Path, out: &mut BTreeSet<PathBuf>) -> Result<()> {
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

    let entries = fs::read_dir(&path).with_context(|| format!("read {}", path.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("read {}", path.display()))?;
        collect_live_files(live_home, &rel.join(entry.file_name()), out)?;
    }
    Ok(())
}

fn clean_rel_path(path: impl AsRef<str>) -> Option<PathBuf> {
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

fn ignored_live_file(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some(".DS_Store")
}

fn expand_env_vars(raw: &str, env: &BTreeMap<String, String>) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        if chars.get(i + 1) == Some(&'{')
            && let Some(end) = find_matching_brace(&chars, i + 1)
        {
            let expr: String = chars[i + 2..end].iter().collect();
            out.push_str(&expand_braced_expr(&expr, env));
            i = end + 1;
            continue;
        }
        let mut end = i + 1;
        while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
            end += 1;
        }
        if end == i + 1 {
            out.push('$');
            i += 1;
            continue;
        }
        let name: String = chars[i + 1..end].iter().collect();
        out.push_str(env.get(&name).map(String::as_str).unwrap_or(""));
        i = end;
    }
    out
}

fn find_matching_brace(chars: &[char], open_brace: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open_brace + 1;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            depth += 1;
            i += 2;
            continue;
        }
        if chars[i] == '}' {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
        i += 1;
    }
    None
}

fn expand_braced_expr(expr: &str, env: &BTreeMap<String, String>) -> String {
    if let Some((name, fallback)) = expr.split_once(":-") {
        if let Some(value) = env.get(name)
            && !value.is_empty()
        {
            return value.clone();
        }
        expand_env_vars(fallback, env)
    } else {
        env.get(expr).cloned().unwrap_or_default()
    }
}

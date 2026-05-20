//! Rendered-target diff class (warn-tier, exit 1).
//!
//! Re-renders the active source root into a scratch tempdir and
//! compares the result against the live `<source-root>/build/<product>/`
//! tree byte-for-byte. Any difference — added, removed, or changed
//! file — is a `warn`-tier finding pointing at the offending path.
//!
//! The class is a no-op when the live build tree is empty (first
//! render hasn't happened yet); reporting POC consumers compute that
//! state from the per-product skipped/rendered counts separately.

use crate::audit_drift::{DriftReport, Finding, Severity};
use crate::render::cache::CACHE_FILE;
use crate::render::manifest::{self, ManifestSet, SourceRoot};
use crate::render::writer;
use anyhow::Result;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

pub const CLASS: &str = "rendered-target";

pub fn check(
    root: &SourceRoot,
    manifests: Option<&ManifestSet>,
    product: &str,
    report: &mut DriftReport,
) -> Result<()> {
    if manifests.is_none() {
        // Manifests didn't load — source_manifest::check already
        // recorded that. Re-rendering would just panic on missing
        // manifests, so we skip this product silently.
        return Ok(());
    }

    let live_dir = root.path().join("build").join(product);
    let live = snapshot_dir(&live_dir);
    if live.is_empty() {
        // No live render yet for this product. The reporting POC
        // covers the "no build yet" state via the render command's
        // exit code; audit-drift only flags drift between an existing
        // build and the current source.
        return Ok(());
    }

    let scratch = TempDir::new()?;
    let scratch_path = scratch.path().to_path_buf();
    // `ManifestSet` doesn't derive `Clone`, and the writer signature
    // wants an `Arc<ManifestSet>`. Re-loading from disk is the
    // cheapest side-effect-free path that keeps the writer API
    // stable — the source root hasn't changed between
    // source_manifest::check and this call.
    let reloaded = Arc::new(manifest::load_all(root)?);
    writer::write_product_to(root, reloaded, product, &scratch_path)?;
    let scratch_snapshot = snapshot_dir(&scratch_path);

    let diffs = diff_trees(&live, &scratch_snapshot);
    for path in diffs {
        report.push(Finding {
            class: CLASS,
            severity: Severity::Warn,
            product: Some(product.to_string()),
            path: PathBuf::from("build").join(product).join(&path),
            message: format!("rendered output drifted from source for {path}"),
        });
    }
    Ok(())
}

/// Walk a directory into a `BTreeMap<RelPath, Vec<u8>>`. Returns an
/// empty map when the directory does not exist (first-render case).
/// Skips `.render-cache.json` because the cache file is a scratchpad
/// — diffing the *rendered output* tree is the contract, not the
/// cache content.
pub(crate) fn snapshot_dir(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    if !root.exists() {
        return out;
    }
    walk(root, root, &mut out);
    out
}

fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            walk(base, &path, out);
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some(CACHE_FILE) {
            continue;
        }
        if let Ok(bytes) = fs::read(&path) {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            out.insert(rel, bytes);
        }
    }
}

fn diff_trees(
    live: &BTreeMap<String, Vec<u8>>,
    scratch: &BTreeMap<String, Vec<u8>>,
) -> Vec<String> {
    let mut paths: std::collections::BTreeSet<&String> = live.keys().collect();
    paths.extend(scratch.keys());
    paths
        .into_iter()
        .filter(|p| live.get(*p) != scratch.get(*p))
        .cloned()
        .collect()
}

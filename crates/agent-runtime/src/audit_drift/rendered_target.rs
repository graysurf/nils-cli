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

use crate::audit_drift::walk;
use crate::audit_drift::{DriftReport, Finding, Severity};
use crate::render::cache::{AGENTS_CACHE_FILE, CACHE_FILE};
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
    let live = snapshot_dir(&live_dir, root.path());
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
    let scratch_snapshot = snapshot_dir(&scratch_path, &scratch_path);

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

/// Walk `dir` into a `BTreeMap<RelPath, Vec<u8>>`. Returns an empty
/// map when the directory does not exist (first-render case). Skips
/// the render cache scratchpads (`.render-cache.json` and
/// `.render-cache-agents.json`) because cache equality is owned by the
/// render_determinism integration test, not by audit-drift's
/// source-vs-build diff.
///
/// The walk goes through the shared `audit_drift::walk` helper, so
/// symlinks that escape `containing_root` are silently dropped (no
/// host file slurp).
pub(crate) fn snapshot_dir(dir: &Path, containing_root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let canonical_root = match containing_root.canonicalize() {
        Ok(p) => p,
        Err(_) => return out,
    };
    // The walker returns canonical paths (via `canonicalize_under`).
    // We strip against the canonical form of `dir` so the relative
    // keys come out clean on macOS where `/var/` and `/private/var/`
    // resolve to each other.
    let Ok(canonical_dir) = dir.canonicalize() else {
        return out;
    };
    for path in walk::collect_files_under(&canonical_dir, &canonical_root) {
        if matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some(CACHE_FILE) | Some(AGENTS_CACHE_FILE)
        ) {
            continue;
        }
        if let Ok(bytes) = fs::read(&path) {
            let rel = path
                .strip_prefix(&canonical_dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            out.insert(rel, bytes);
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn snapshot_dir_returns_empty_for_missing_root() {
        let tmp = TempDir::new().unwrap();
        let canon = tmp.path().canonicalize().unwrap();
        let snap = snapshot_dir(&canon.join("nonexistent"), &canon);
        assert!(snap.is_empty());
    }

    #[test]
    fn snapshot_dir_skips_render_cache_files() {
        // Both cache files live at the root of build/<product>/; the
        // diff class deliberately ignores them because cache equality is
        // owned by the render_determinism test, not audit-drift.
        let tmp = TempDir::new().unwrap();
        let canon = tmp.path().canonicalize().unwrap();
        let build = canon.join("build/codex");
        fs::create_dir_all(build.join("skills")).unwrap();
        fs::write(build.join("skills/SKILL.md"), "hello").unwrap();
        fs::write(build.join(CACHE_FILE), "{}").unwrap();
        fs::write(build.join(AGENTS_CACHE_FILE), "{}").unwrap();
        let snap = snapshot_dir(&build, &canon);
        assert_eq!(snap.len(), 1, "{snap:?}");
        assert!(snap.contains_key("skills/SKILL.md"));
        assert!(!snap.contains_key(CACHE_FILE));
        assert!(!snap.contains_key(AGENTS_CACHE_FILE));
    }

    #[test]
    fn diff_trees_flags_added_removed_and_changed_paths() {
        let mut live = BTreeMap::new();
        live.insert("same.md".to_string(), b"x".to_vec());
        live.insert("changed.md".to_string(), b"old".to_vec());
        live.insert("removed.md".to_string(), b"gone".to_vec());

        let mut fresh = BTreeMap::new();
        fresh.insert("same.md".to_string(), b"x".to_vec());
        fresh.insert("changed.md".to_string(), b"new".to_vec());
        fresh.insert("added.md".to_string(), b"new".to_vec());

        let mut diffs = diff_trees(&live, &fresh);
        diffs.sort();
        assert_eq!(diffs, vec!["added.md", "changed.md", "removed.md"]);
    }
}

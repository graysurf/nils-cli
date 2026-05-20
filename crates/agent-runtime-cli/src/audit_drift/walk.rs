//! Shared directory walker for the audit-drift classes.
//!
//! Every walk under `<source-root>/` must defeat the same symlink-escape
//! that the render writer guards against — a hostile
//! `build/<product>/symlink -> /etc/passwd` (or a symlink under
//! `core/`/`manifests/`) would otherwise let audit-drift slurp host
//! files into the in-memory diff buffer, or panic the leak scan into
//! reading arbitrary OS state.
//!
//! `collect_files_under` walks `dir` (which must already sit under
//! `canonical_root`), sorts entries deterministically, and skips any
//! entry whose canonical path resolves outside `canonical_root`. The
//! per-file callers (`fs::read`, `fs::read_to_string`) receive the
//! already-resolved path so a subsequent symlink swap can't re-escape
//! between the check and the open.
//!
//! Sprint 1 added the same guard inline in `render::writer`; this
//! module is the audit-drift sibling so the leak/diff/docs-home
//! classes share one definition instead of three copies.

use crate::render::writer::canonicalize_under;
use std::fs;
use std::path::{Path, PathBuf};

/// Walk `dir` recursively, collecting every regular file's canonical
/// path. Returns an empty list when `dir` does not exist. Sorts the
/// output for byte-deterministic scan order.
///
/// Symlink behavior: each entry is resolved against `canonical_root`;
/// entries that escape the root are silently dropped. Audit-drift
/// surfaces "this file does not belong here" as a class finding, not
/// as a hard error during the walk itself.
pub fn collect_files_under(dir: &Path, canonical_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !dir.exists() {
        return out;
    }
    collect(dir, canonical_root, &mut out);
    out.sort();
    out
}

fn collect(dir: &Path, canonical_root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        // Canonicalize each entry so symlinks resolve before we either
        // recurse into a directory or open a file. A symlink pointing
        // outside the source root is dropped here — the leak/diff
        // classes never see it.
        let Ok(resolved) = canonicalize_under(canonical_root, &path) else {
            continue;
        };
        if resolved.is_dir() {
            collect(&resolved, canonical_root, out);
        } else {
            out.push(resolved);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn empty_dir_returns_empty_vec() {
        let tmp = TempDir::new().unwrap();
        let canon = tmp.path().canonicalize().unwrap();
        let out = collect_files_under(&canon, &canon);
        assert!(out.is_empty());
    }

    #[test]
    fn missing_dir_returns_empty_vec() {
        let tmp = TempDir::new().unwrap();
        let canon = tmp.path().canonicalize().unwrap();
        let out = collect_files_under(&canon.join("nonexistent"), &canon);
        assert!(out.is_empty());
    }

    #[test]
    fn collects_regular_files_in_sorted_order() {
        let tmp = TempDir::new().unwrap();
        let canon = tmp.path().canonicalize().unwrap();
        fs::write(canon.join("zeta.txt"), "z").unwrap();
        fs::write(canon.join("alpha.txt"), "a").unwrap();
        fs::create_dir(canon.join("sub")).unwrap();
        fs::write(canon.join("sub/beta.txt"), "b").unwrap();
        let out = collect_files_under(&canon, &canon);
        let names: Vec<_> = out
            .iter()
            .map(|p| {
                p.strip_prefix(&canon)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, vec!["alpha.txt", "sub/beta.txt", "zeta.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_root_is_silently_dropped() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new().unwrap();
        let outside_canon = outside.path().canonicalize().unwrap();
        fs::write(outside_canon.join("secret.txt"), "secret").unwrap();

        let inside = TempDir::new().unwrap();
        let inside_canon = inside.path().canonicalize().unwrap();
        fs::write(inside_canon.join("safe.txt"), "safe").unwrap();
        symlink(
            outside_canon.join("secret.txt"),
            inside_canon.join("escape"),
        )
        .unwrap();

        let out = collect_files_under(&inside_canon, &inside_canon);
        let names: Vec<_> = out
            .iter()
            .map(|p| {
                p.strip_prefix(&inside_canon)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            names,
            vec!["safe.txt"],
            "symlink to outside root must be skipped, not followed"
        );
    }
}

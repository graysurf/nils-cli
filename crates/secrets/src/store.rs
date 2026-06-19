//! Pure store-path resolution.
//!
//! This module owns the metadata-only logic ported from the bash script's
//! `slug()` and `store_file()` helpers: where the store lives, how a repo's
//! `origin` remote maps to an `owner/repo` slug, and how a `[name]` argument
//! resolves to a store entry. None of these functions touch secret *values* —
//! they deal only with paths and slugs, which keeps them trivially testable
//! without a real `sops` or `git`.

use std::path::{Path, PathBuf};

use nils_common::git::parse_git_remote_url;

/// Default store location when `SECRETS_REPO` is unset.
pub const DEFAULT_STORE_REL: &str = "Project/graysurf/secrets";

/// The `.enc.env` suffix every encrypted store entry carries.
pub const ENC_SUFFIX: &str = ".enc.env";

/// Resolve the store root, honoring `$SECRETS_REPO` then falling back to
/// `$HOME/Project/graysurf/secrets`. `home` is injected so tests stay
/// hermetic.
pub fn resolve_store_root(secrets_repo_env: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(value) = secrets_repo_env
        && !value.trim().is_empty()
    {
        return Some(PathBuf::from(value));
    }
    home.map(|h| h.join(DEFAULT_STORE_REL))
}

/// Derive the `owner/repo` (or nested `group/.../repo`) slug from a git remote
/// URL, mirroring the bash `slug()` sed pipeline but reusing the workspace's
/// canonical URL parser so SCP, ssh, http(s), ports, userinfo, and multi-segment
/// GitLab paths are all handled consistently. Returns `None` when the URL is not
/// a recognizable git remote.
pub fn slug_from_remote_url(remote: &str) -> Option<String> {
    parse_git_remote_url(remote).map(|parsed| parsed.path)
}

/// A resolved store entry: where it lives and how it is referenced relative to
/// the store root (the form shown to users).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreEntry {
    /// Absolute path to the `.enc.env` file in the store.
    pub path: PathBuf,
    /// Store-relative path (e.g. `repos/owner/repo.enc.env`).
    pub rel: String,
    /// Whether the file currently exists on disk.
    pub exists: bool,
}

/// Resolve the store entry for an explicit `[name]` argument, mirroring the bash
/// `store_file` lookup order: try `<name>`, then `repos/<name>`, then
/// `stacks/<name>`, each as both a bare path and with the `.enc.env` suffix
/// appended. When nothing matches on disk, fall back to `repos/<name>.enc.env`
/// (the default target `add` would create), with `exists = false`.
pub fn store_entry_for_name(store_root: &Path, name: &str) -> StoreEntry {
    for cand in [name, &format!("repos/{name}"), &format!("stacks/{name}")] {
        // Suffixed form: `<cand>.enc.env`.
        let with_suffix = format!("{cand}{ENC_SUFFIX}");
        let suffixed = store_root.join(&with_suffix);
        if suffixed.is_file() {
            return StoreEntry {
                path: suffixed,
                rel: with_suffix,
                exists: true,
            };
        }
        // Bare form: `<cand>` exactly as given (already carries an extension).
        let bare = store_root.join(cand);
        if bare.is_file() {
            return StoreEntry {
                path: bare,
                rel: cand.to_string(),
                exists: true,
            };
        }
    }

    let rel = format!("repos/{name}{ENC_SUFFIX}");
    StoreEntry {
        path: store_root.join(&rel),
        rel,
        exists: false,
    }
}

/// Resolve the store entry for the auto-detected repo slug
/// (`repos/<slug>.enc.env`).
pub fn store_entry_for_slug(store_root: &Path, slug: &str) -> StoreEntry {
    let rel = format!("repos/{slug}{ENC_SUFFIX}");
    let path = store_root.join(&rel);
    let exists = path.is_file();
    StoreEntry { path, rel, exists }
}

/// List every `*.enc.env` entry under `stacks/` and `repos/`, returned as
/// sorted store-relative paths with the `.enc.env` suffix stripped (matching the
/// bash `list` command). Only entry *names* are returned — never contents.
pub fn list_entries(store_root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for top in ["stacks", "repos"] {
        collect_enc_env(&store_root.join(top), top, &mut out);
    }
    out.sort();
    out
}

fn collect_enc_env(dir: &Path, rel_prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let child_rel = format!("{rel_prefix}/{name}");
        if file_type.is_dir() {
            collect_enc_env(&path, &child_rel, out);
        } else if let Some(stripped) = child_rel.strip_suffix(ENC_SUFFIX) {
            out.push(stripped.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn store_root_prefers_env_then_home() {
        let home = PathBuf::from("/home/someone");
        assert_eq!(
            resolve_store_root(Some("/custom/store"), Some(&home)),
            Some(PathBuf::from("/custom/store"))
        );
        assert_eq!(
            resolve_store_root(Some("   "), Some(&home)),
            Some(home.join(DEFAULT_STORE_REL))
        );
        assert_eq!(
            resolve_store_root(None, Some(&home)),
            Some(home.join("Project/graysurf/secrets"))
        );
        assert_eq!(resolve_store_root(None, None), None);
    }

    #[test]
    fn slug_handles_scp_https_and_nested_gitlab() {
        assert_eq!(
            slug_from_remote_url("git@github.com:graysurf/g14-infra.git").as_deref(),
            Some("graysurf/g14-infra")
        );
        assert_eq!(
            slug_from_remote_url("https://github.com/graysurf/g14-infra").as_deref(),
            Some("graysurf/g14-infra")
        );
        assert_eq!(
            slug_from_remote_url("https://gitlab.example.com/acme/platform/backend/svc.git")
                .as_deref(),
            Some("acme/platform/backend/svc")
        );
        assert_eq!(slug_from_remote_url("not a url"), None);
    }

    #[test]
    fn store_entry_for_slug_reports_existence() {
        let tmp = TempDir::new().expect("tempdir");
        let entry = store_entry_for_slug(tmp.path(), "owner/repo");
        assert_eq!(entry.rel, "repos/owner/repo.enc.env");
        assert!(!entry.exists);

        fs::create_dir_all(tmp.path().join("repos/owner")).expect("mkdir");
        fs::write(tmp.path().join("repos/owner/repo.enc.env"), "x").expect("write");
        let entry = store_entry_for_slug(tmp.path(), "owner/repo");
        assert!(entry.exists);
    }

    #[test]
    fn store_entry_for_name_lookup_order_and_fallback() {
        let tmp = TempDir::new().expect("tempdir");
        fs::create_dir_all(tmp.path().join("stacks")).expect("mkdir");
        fs::write(tmp.path().join("stacks/web.enc.env"), "x").expect("write");

        // Bare name resolves through `stacks/<name>.enc.env`.
        let entry = store_entry_for_name(tmp.path(), "web");
        assert_eq!(entry.rel, "stacks/web.enc.env");
        assert!(entry.exists);

        // Explicit `stacks/web` resolves the same file.
        let entry = store_entry_for_name(tmp.path(), "stacks/web");
        assert_eq!(entry.rel, "stacks/web.enc.env");
        assert!(entry.exists);

        // Unknown name falls back to the default add target, not existing.
        let entry = store_entry_for_name(tmp.path(), "ghost");
        assert_eq!(entry.rel, "repos/ghost.enc.env");
        assert!(!entry.exists);
    }

    #[test]
    fn list_entries_strips_suffix_and_sorts() {
        let tmp = TempDir::new().expect("tempdir");
        fs::create_dir_all(tmp.path().join("repos/owner")).expect("mkdir");
        fs::create_dir_all(tmp.path().join("stacks")).expect("mkdir");
        fs::write(tmp.path().join("repos/owner/repo.enc.env"), "x").expect("write");
        fs::write(tmp.path().join("stacks/web.enc.env"), "x").expect("write");
        // Non-matching files are ignored.
        fs::write(tmp.path().join("stacks/README.md"), "x").expect("write");

        let entries = list_entries(tmp.path());
        assert_eq!(
            entries,
            vec!["repos/owner/repo".to_string(), "stacks/web".to_string()]
        );
    }
}

use nils_common::git as common_git;
use std::path::{Path, PathBuf};

pub fn detect() -> PathBuf {
    common_git::repo_root_or_cwd()
}

/// Detect the repo root that contains `hint`. When `hint` is absolute,
/// walks up from its directory (or its parent, when `hint` is a file
/// path) to find the enclosing git toplevel. Falls back to
/// [`detect()`] when no enclosing repo is found or `hint` is relative.
///
/// Used by subcommands that accept a `--file <path>` argument so that
/// an absolute file path from a foreign cwd resolves repo-relative
/// lookups against the file's own repo, not the caller's cwd.
pub fn detect_from(hint: &Path) -> PathBuf {
    if !hint.is_absolute() {
        return detect();
    }
    let start = if hint.is_dir() {
        hint.to_path_buf()
    } else {
        hint.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    common_git::repo_root_in(&start)
        .ok()
        .flatten()
        .unwrap_or_else(detect)
}

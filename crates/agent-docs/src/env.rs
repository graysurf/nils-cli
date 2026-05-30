use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use nils_common::git as shared_git;
use serde::Serialize;

use crate::paths::normalize_root_path;

/// Install symlinks the harness creates; the docs-home is the directory that
/// holds the file each symlink resolves to (`AGENT_HOME.md`).
const INSTALL_SYMLINKS: [&str; 2] = [".claude/CLAUDE.md", ".codex/AGENTS.md"];

#[derive(Debug, Clone, Default)]
pub struct PathOverrides {
    pub docs_home: Option<PathBuf>,
    pub project_path: Option<PathBuf>,
}

/// How the docs-home was located.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocsHomeSource {
    /// Explicit `--docs-home` flag.
    Flag,
    /// Derived from the install symlink (`dirname(readlink ~/.claude/CLAUDE.md)`).
    Symlink,
    /// The `AGENT_DOCS_HOME` environment variable (lowest-precedence fallback).
    Env,
}

impl DocsHomeSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Symlink => "symlink",
            Self::Env => "env",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedRoots {
    pub docs_home: PathBuf,
    pub docs_home_source: DocsHomeSource,
    pub project_path: PathBuf,
    pub is_linked_worktree: bool,
    pub git_common_dir: Option<PathBuf>,
    pub primary_worktree_path: Option<PathBuf>,
}

impl ResolvedRoots {
    /// Construct roots for explicit paths (used by tests and internal callers).
    pub fn for_paths(docs_home: PathBuf, project_path: PathBuf) -> Self {
        Self {
            docs_home,
            docs_home_source: DocsHomeSource::Flag,
            project_path,
            is_linked_worktree: false,
            git_common_dir: None,
            primary_worktree_path: None,
        }
    }
}

pub fn resolve_roots(overrides: &PathOverrides) -> Result<ResolvedRoots> {
    let cwd = env::current_dir().context("failed to read current directory")?;
    let (docs_home, docs_home_source) = resolve_docs_home(overrides.docs_home.as_deref(), &cwd)?;
    let project_path = resolve_project_path(overrides.project_path.as_deref(), &cwd);
    let metadata = resolve_linked_worktree_metadata(&project_path);

    Ok(ResolvedRoots {
        docs_home,
        docs_home_source,
        project_path,
        is_linked_worktree: metadata.is_linked_worktree,
        git_common_dir: metadata.git_common_dir,
        primary_worktree_path: metadata.primary_worktree_path,
    })
}

fn resolve_docs_home(cli_value: Option<&Path>, cwd: &Path) -> Result<(PathBuf, DocsHomeSource)> {
    if let Some(path) = cli_value {
        return Ok((normalize_root_path(path, cwd), DocsHomeSource::Flag));
    }

    if let Some(path) = derive_docs_home_from_symlinks(home_dir().as_deref()) {
        return Ok((normalize_root_path(&path, cwd), DocsHomeSource::Symlink));
    }

    if let Some(path) = read_env_path("AGENT_DOCS_HOME") {
        return Ok((normalize_root_path(&path, cwd), DocsHomeSource::Env));
    }

    bail!(
        "cannot locate docs-home: no --docs-home, the install symlink \
         (~/.claude/CLAUDE.md or ~/.codex/AGENTS.md) does not resolve, and \
         AGENT_DOCS_HOME is unset; pass --docs-home <path>"
    )
}

/// The user's home directory, honoring `HOME` (so it can be overridden in
/// subprocess tests).
pub fn home_dir() -> Option<PathBuf> {
    read_env_path("HOME").or_else(|| read_env_path("USERPROFILE"))
}

/// Derive the docs-home from the harness install symlinks: the docs-home is the
/// directory that contains the file `~/.claude/CLAUDE.md` (or the Codex
/// equivalent) resolves to.
pub fn derive_docs_home_from_symlinks(home: Option<&Path>) -> Option<PathBuf> {
    let home = home?;
    for relative in INSTALL_SYMLINKS {
        let link = home.join(relative);
        if let Some(docs_home) = docs_home_from_symlink(&link) {
            return Some(docs_home);
        }
    }
    None
}

fn docs_home_from_symlink(link: &Path) -> Option<PathBuf> {
    // The link must itself be a symlink; a plain file is not the install wiring.
    let meta = std::fs::symlink_metadata(link).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    let resolved = std::fs::canonicalize(link).ok()?;
    resolved.parent().map(Path::to_path_buf)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymlinkWiring {
    /// A symlink is present and resolves into the docs-home.
    Intact,
    /// A symlink is present but resolves elsewhere.
    Mismatch,
    /// No install symlink is present.
    Missing,
}

/// Inspect the install symlink wiring relative to a resolved docs-home, for
/// `audit` reporting.
pub fn inspect_symlink_wiring(home: Option<&Path>, docs_home: &Path) -> (SymlinkWiring, String) {
    let Some(home) = home else {
        return (
            SymlinkWiring::Missing,
            "HOME is unset; cannot inspect install symlink".to_string(),
        );
    };
    let canonical_docs_home =
        std::fs::canonicalize(docs_home).unwrap_or_else(|_| docs_home.to_path_buf());
    for relative in INSTALL_SYMLINKS {
        let link = home.join(relative);
        let Ok(meta) = std::fs::symlink_metadata(&link) else {
            continue;
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        let Ok(resolved) = std::fs::canonicalize(&link) else {
            return (
                SymlinkWiring::Mismatch,
                format!("{} is a broken symlink", link.display()),
            );
        };
        let resolved_home = resolved.parent().map(Path::to_path_buf).unwrap_or(resolved);
        if resolved_home == canonical_docs_home {
            return (
                SymlinkWiring::Intact,
                format!("{} -> {}", link.display(), resolved_home.display()),
            );
        }
        return (
            SymlinkWiring::Mismatch,
            format!(
                "{} resolves to {} but docs-home is {}",
                link.display(),
                resolved_home.display(),
                canonical_docs_home.display()
            ),
        );
    }
    (
        SymlinkWiring::Missing,
        format!(
            "no install symlink found under {} ({})",
            home.display(),
            INSTALL_SYMLINKS.join(", ")
        ),
    )
}

fn resolve_project_path(cli_value: Option<&Path>, cwd: &Path) -> PathBuf {
    if let Some(path) = cli_value {
        return normalize_root_path(path, cwd);
    }

    if let Some(path) = read_env_path("PROJECT_PATH") {
        return normalize_root_path(&path, cwd);
    }

    if let Some(path) = git_top_level(cwd) {
        return normalize_root_path(&path, cwd);
    }

    normalize_root_path(cwd, cwd)
}

fn read_env_path(name: &str) -> Option<PathBuf> {
    let raw = env::var_os(name)?;
    if raw.is_empty() {
        None
    } else {
        Some(PathBuf::from(raw))
    }
}

fn git_top_level(cwd: &Path) -> Option<PathBuf> {
    git_rev_parse_path(cwd, "--show-toplevel")
}

#[derive(Debug, Default)]
struct LinkedWorktreeMetadata {
    is_linked_worktree: bool,
    git_common_dir: Option<PathBuf>,
    primary_worktree_path: Option<PathBuf>,
}

fn resolve_linked_worktree_metadata(cwd: &Path) -> LinkedWorktreeMetadata {
    let absolute_git_dir = git_rev_parse_path(cwd, "--absolute-git-dir");
    let git_common_dir = git_rev_parse_path(cwd, "--git-common-dir");

    let Some(git_common_dir) = git_common_dir else {
        return LinkedWorktreeMetadata::default();
    };

    let is_linked_worktree = absolute_git_dir
        .as_ref()
        .is_some_and(|git_dir| git_dir != &git_common_dir);
    let primary_worktree_path = if is_linked_worktree {
        git_common_dir.parent().map(Path::to_path_buf)
    } else {
        None
    };

    LinkedWorktreeMetadata {
        is_linked_worktree,
        git_common_dir: Some(git_common_dir),
        primary_worktree_path,
    }
}

fn git_rev_parse_path(cwd: &Path, arg: &str) -> Option<PathBuf> {
    let raw = shared_git::rev_parse_in(cwd, &[arg]).ok().flatten()?;
    Some(normalize_root_path(Path::new(&raw), cwd))
}

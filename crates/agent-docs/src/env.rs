use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use nils_common::git as shared_git;

use crate::paths::normalize_root_path;

#[derive(Debug, Clone, Default)]
pub struct PathOverrides {
    pub docs_home: Option<PathBuf>,
    pub project_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ResolvedRoots {
    pub docs_home: PathBuf,
    pub project_path: PathBuf,
    pub is_linked_worktree: bool,
    pub git_common_dir: Option<PathBuf>,
    pub primary_worktree_path: Option<PathBuf>,
}

pub fn resolve_roots(overrides: &PathOverrides) -> Result<ResolvedRoots> {
    let cwd = env::current_dir().context("failed to read current directory")?;
    let docs_home = resolve_docs_home(overrides.docs_home.as_deref(), &cwd)?;
    let project_path = resolve_project_path(overrides.project_path.as_deref(), &cwd);
    let metadata = resolve_linked_worktree_metadata(&project_path);

    Ok(ResolvedRoots {
        docs_home,
        project_path,
        is_linked_worktree: metadata.is_linked_worktree,
        git_common_dir: metadata.git_common_dir,
        primary_worktree_path: metadata.primary_worktree_path,
    })
}

fn resolve_docs_home(cli_value: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(path) = cli_value {
        return Ok(normalize_root_path(path, cwd));
    }

    if let Some(path) = read_env_path("AGENT_DOCS_HOME") {
        return Ok(normalize_root_path(&path, cwd));
    }

    bail!("AGENT_DOCS_HOME is required; set AGENT_DOCS_HOME or pass --docs-home")
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

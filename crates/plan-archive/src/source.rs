//! Shared source-repo / archive / hosts resolution.
//!
//! Both `migrate` and `discover` locate the working repo, the archive
//! clone, and the host-classification config through these helpers so
//! the two commands cannot drift on resolution semantics. Every helper
//! here is read-only; nothing copies, writes, or commits.

use std::fs;
use std::path::{Path, PathBuf};

use crate::validate;
use crate::validate::hosts::{HostsConfig, validate_hosts_yaml};

/// Resolution failures shared by `migrate` and `discover`. Callers map
/// these onto their own command-scoped error codes.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("source repo not found at `{0}`")]
    SourceRepoNotFound(PathBuf),
    #[error(
        "archive clone path not found at `{0}` (set `--archive` or seed `archive_clone_path` in the local config)"
    )]
    ArchiveCloneMissing(PathBuf),
    #[error("failed to load archive `config/hosts.yaml`: {0}")]
    HostsLoadFailed(String),
    #[error("failed to parse archive `config/hosts.yaml`: {0}")]
    HostsParseFailed(String),
    #[error("io error: {0}")]
    Io(String),
}

/// Resolve the source working repo: an explicit override or the git
/// repo root containing the current directory.
pub fn resolve_source_repo(arg: Option<&Path>) -> Result<PathBuf, SourceError> {
    let candidate = match arg {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().map_err(|e| SourceError::Io(e.to_string()))?,
    };
    let root = nils_common::git::repo_root_in(&candidate)
        .map_err(|e| SourceError::Io(e.to_string()))?
        .ok_or_else(|| SourceError::SourceRepoNotFound(candidate.clone()))?;
    if !root.is_dir() {
        return Err(SourceError::SourceRepoNotFound(root));
    }
    Ok(root)
}

/// Resolve the archive clone path: an explicit override or the
/// machine-local config's `archive_clone_path`.
pub fn resolve_archive(arg: Option<&Path>) -> Result<PathBuf, SourceError> {
    let candidate = match arg {
        Some(p) => p.to_path_buf(),
        None => default_archive_clone_path()?,
    };
    if !candidate.is_dir() {
        return Err(SourceError::ArchiveCloneMissing(candidate));
    }
    Ok(candidate)
}

/// Read `archive_clone_path` from the machine-local config (which
/// returns documented defaults when the file is absent).
pub fn default_archive_clone_path() -> Result<PathBuf, SourceError> {
    let local = validate::local::validate_local_path(&local_config_path())
        .map_err(|e| SourceError::Io(e.to_string()))?;
    Ok(local.data.config.archive_clone_path)
}

/// Machine-local config path, honouring `PLAN_ARCHIVE_LOCAL_CONFIG`,
/// then `XDG_CONFIG_HOME`, then `$HOME/.config`.
pub fn local_config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("PLAN_ARCHIVE_LOCAL_CONFIG") {
        return PathBuf::from(p);
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg)
            .join("agent-plan-archive")
            .join("config.yaml");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("agent-plan-archive")
            .join("config.yaml");
    }
    PathBuf::from("/nonexistent/agent-plan-archive/config.yaml")
}

/// `config/hosts.yaml` path for an archive clone, honouring an explicit
/// `--hosts` override.
pub fn hosts_path_for(archive: &Path, override_path: Option<&Path>) -> PathBuf {
    match override_path {
        Some(p) => p.to_path_buf(),
        None => archive.join("config").join("hosts.yaml"),
    }
}

/// Load and validate the archive `config/hosts.yaml`.
pub fn load_hosts(path: &Path) -> Result<HostsConfig, SourceError> {
    let raw = fs::read_to_string(path).map_err(|e| SourceError::HostsLoadFailed(e.to_string()))?;
    let v = validate_hosts_yaml(&raw).map_err(|e| SourceError::HostsParseFailed(e.to_string()))?;
    Ok(v.data.config)
}

/// Whether `git status --porcelain` reports any tracked-or-untracked
/// change under `rel` inside `repo`. Read-only; never mutates the repo.
pub fn has_dirty_path(repo: &Path, rel: &Path) -> Result<bool, SourceError> {
    let out = nils_common::git::run_output_in(
        repo,
        &["status", "--porcelain", "--", &rel.to_string_lossy()],
    )
    .map_err(|e| SourceError::Io(e.to_string()))?;
    if !out.status.success() {
        return Err(SourceError::Io(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(!out.stdout.is_empty())
}

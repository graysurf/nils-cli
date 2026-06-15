//! Archive / agent-out / hosts resolution.
//!
//! Mirrors `plan-archive/src/source.rs` with two deliberate divergences,
//! both documented inline so they read as intentional design rather than
//! copy-paste drift:
//!
//! 1. [`resolve_archive`] inserts an explicit `AGENT_EVIDENCE_ARCHIVE_HOME`
//!    env step (precedence 2) that plan-archive's resolver lacks. A
//!    set-but-missing path is an **error**, not a fall-through to the
//!    config/default.
//! 2. There is no source *repo* (skill-usage records live under the
//!    `agent-out` tree, not inside a git checkout), so
//!    [`resolve_source_repo`] is replaced by [`resolve_source_out`], which
//!    resolves the `${AGENT_HOME}/out/projects` root.
//!
//! Every helper here is read-only; nothing copies, writes, or commits.

use std::fs;
use std::path::{Path, PathBuf};

use crate::validate;
use crate::validate::hosts::{HostsConfig, validate_hosts_yaml};

/// Resolution failures shared by `migrate` and `discover`. Callers map these
/// onto their own command-scoped error codes.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("agent-out projects root not found at `{0}` (is `AGENT_HOME` set?)")]
    SourceOutMissing(PathBuf),
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

/// Resolve the agent-out `out/projects` root that holds skill-usage records.
///
/// Precedence: an explicit override, then `${AGENT_HOME}/out/projects`. This
/// is the evidence analogue of plan-archive's `resolve_source_repo`, but the
/// "source" is the agent-out tree, not a git checkout.
pub fn resolve_source_out(arg: Option<&Path>) -> Result<PathBuf, SourceError> {
    let candidate = match arg {
        Some(p) => p.to_path_buf(),
        None => {
            let home = std::env::var_os("AGENT_HOME")
                .ok_or_else(|| SourceError::SourceOutMissing(PathBuf::from("$AGENT_HOME")))?;
            PathBuf::from(home).join("out").join("projects")
        }
    };
    if !candidate.is_dir() {
        return Err(SourceError::SourceOutMissing(candidate));
    }
    Ok(candidate)
}

/// Resolve the archive clone path.
///
/// Precedence (first match wins):
/// 1. `--archive <path>` flag.
/// 2. `$AGENT_EVIDENCE_ARCHIVE_HOME` env. **DIVERGENCE** from plan-archive:
///    this env step does not exist there. A set-but-missing path is an
///    *error* (`ArchiveCloneMissing`), never a silent fall-through — folding
///    the env into the default would mask a misconfigured environment.
/// 3. machine-local config `archive_clone_path`.
/// 4. documented default
///    `${XDG_DATA_HOME:-$HOME/.local/share}/agent-evidence-archive`.
pub fn resolve_archive(arg: Option<&Path>) -> Result<PathBuf, SourceError> {
    if let Some(p) = arg {
        let candidate = p.to_path_buf();
        if !candidate.is_dir() {
            return Err(SourceError::ArchiveCloneMissing(candidate));
        }
        return Ok(candidate);
    }

    // DIVERGENCE (intentional, not drift): explicit env step before the
    // config/default chain. Set-but-missing is an error.
    if let Some(p) = std::env::var_os("AGENT_EVIDENCE_ARCHIVE_HOME") {
        let candidate = PathBuf::from(p);
        if !candidate.is_dir() {
            return Err(SourceError::ArchiveCloneMissing(candidate));
        }
        return Ok(candidate);
    }

    let candidate = default_archive_clone_path()?;
    if !candidate.is_dir() {
        return Err(SourceError::ArchiveCloneMissing(candidate));
    }
    Ok(candidate)
}

/// Read `archive_clone_path` from the machine-local config (which returns
/// documented defaults when the file is absent).
pub fn default_archive_clone_path() -> Result<PathBuf, SourceError> {
    let local = validate::local::validate_local_path(&local_config_path())
        .map_err(|e| SourceError::Io(e.to_string()))?;
    Ok(local.data.config.archive_clone_path)
}

/// Read `working_repo_roots` from the machine-local config (empty when the file
/// is absent or unreadable). Used as a last-resort host-resolution hint for a
/// record whose recorded `cwd` no longer exists (e.g. a removed agent worktree).
pub fn resolve_working_repo_roots() -> Vec<PathBuf> {
    validate::local::validate_local_path(&local_config_path())
        .map(|v| v.data.config.working_repo_roots)
        .unwrap_or_default()
}

/// Machine-local config path, honouring `AGENT_EVIDENCE_LOCAL_CONFIG`, then
/// `XDG_CONFIG_HOME`, then `$HOME/.config`, then a non-existent sentinel.
pub fn local_config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("AGENT_EVIDENCE_LOCAL_CONFIG") {
        return PathBuf::from(p);
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg)
            .join("agent-evidence-archive")
            .join("config.yaml");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("agent-evidence-archive")
            .join("config.yaml");
    }
    PathBuf::from("/nonexistent/agent-evidence-archive/config.yaml")
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

/// Whether `git status --porcelain` reports any tracked-or-untracked change
/// under `rel` inside `repo`. Read-only; never mutates the repo.
///
/// Evidence checks the **archive** clone (that is where it writes), unlike
/// plan-archive which checks the source plan path.
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

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock};

    #[test]
    fn archive_flag_wins_when_dir_exists() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_archive(Some(dir.path())).expect("flag resolves");
        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn archive_flag_missing_dir_errors() {
        let err = resolve_archive(Some(Path::new("/nonexistent/evidence-archive"))).unwrap_err();
        assert!(matches!(err, SourceError::ArchiveCloneMissing(_)));
    }

    #[test]
    fn env_archive_home_takes_precedence_over_config_and_default() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::set(
            &lock,
            "AGENT_EVIDENCE_ARCHIVE_HOME",
            &dir.path().to_string_lossy(),
        );
        let resolved = resolve_archive(None).expect("env resolves");
        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn env_archive_home_set_but_missing_is_error_not_fallthrough() {
        let lock = GlobalStateLock::new();
        let _env = EnvGuard::set(
            &lock,
            "AGENT_EVIDENCE_ARCHIVE_HOME",
            "/nonexistent/evidence-archive-xyz",
        );
        let err = resolve_archive(None).unwrap_err();
        assert!(
            matches!(err, SourceError::ArchiveCloneMissing(_)),
            "set-but-missing env must error, got {err:?}"
        );
    }

    #[test]
    fn local_config_path_honours_explicit_env() {
        let lock = GlobalStateLock::new();
        let _env = EnvGuard::set(&lock, "AGENT_EVIDENCE_LOCAL_CONFIG", "/tmp/my-config.yaml");
        assert_eq!(local_config_path(), PathBuf::from("/tmp/my-config.yaml"));
    }

    #[test]
    fn local_config_path_honours_xdg_config_home() {
        let lock = GlobalStateLock::new();
        let _no_explicit = EnvGuard::remove(&lock, "AGENT_EVIDENCE_LOCAL_CONFIG");
        let _xdg = EnvGuard::set(&lock, "XDG_CONFIG_HOME", "/xdg");
        assert_eq!(
            local_config_path(),
            PathBuf::from("/xdg/agent-evidence-archive/config.yaml")
        );
    }

    #[test]
    fn local_config_path_falls_back_to_home_dotconfig() {
        let lock = GlobalStateLock::new();
        let _no_explicit = EnvGuard::remove(&lock, "AGENT_EVIDENCE_LOCAL_CONFIG");
        let _no_xdg = EnvGuard::remove(&lock, "XDG_CONFIG_HOME");
        let _home = EnvGuard::set(&lock, "HOME", "/home/me");
        assert_eq!(
            local_config_path(),
            PathBuf::from("/home/me/.config/agent-evidence-archive/config.yaml")
        );
    }

    #[test]
    fn resolve_source_out_explicit_override() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_source_out(Some(dir.path())).expect("override resolves");
        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn resolve_source_out_missing_override_errors() {
        let err = resolve_source_out(Some(Path::new("/nonexistent/out-projects"))).unwrap_err();
        assert!(matches!(err, SourceError::SourceOutMissing(_)));
    }

    #[test]
    fn resolve_source_out_uses_agent_home() {
        let lock = GlobalStateLock::new();
        let home = tempfile::tempdir().unwrap();
        let projects = home.path().join("out").join("projects");
        fs::create_dir_all(&projects).unwrap();
        let _env = EnvGuard::set(&lock, "AGENT_HOME", &home.path().to_string_lossy());
        let resolved = resolve_source_out(None).expect("agent-home resolves");
        assert_eq!(resolved, projects);
    }

    #[test]
    fn hosts_path_for_default_and_override() {
        let archive = Path::new("/arch");
        assert_eq!(
            hosts_path_for(archive, None),
            PathBuf::from("/arch/config/hosts.yaml")
        );
        let override_p = Path::new("/custom/hosts.yaml");
        assert_eq!(hosts_path_for(archive, Some(override_p)), override_p);
    }

    #[test]
    fn load_hosts_missing_file_is_load_failed() {
        let err = load_hosts(Path::new("/nonexistent/config/hosts.yaml")).unwrap_err();
        assert!(matches!(err, SourceError::HostsLoadFailed(_)));
    }
}

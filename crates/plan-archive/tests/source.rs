//! Unit-level coverage for `plan_archive::source` resolution helpers.
//!
//! The `migrate`/`discover` integration suites only reach these helpers
//! on their happy paths (explicit `--source`/`--archive` overrides, a
//! seeded `hosts.yaml`). This file exercises the fallback and failure
//! branches those suites never hit:
//!
//! - the `current_dir()` fallback in [`resolve_source_repo`]
//! - the local-config fallback in [`resolve_archive`] /
//!   [`default_archive_clone_path`]
//! - the environment-precedence rules in [`local_config_path`]
//! - the `git status` failure branch in [`has_dirty_path`]
//!
//! All helpers are read-only; the env/cwd mutations below run under the
//! shared `GlobalStateLock` so they cannot race other global-state tests.

use std::fs;
use std::path::{Path, PathBuf};

use nils_test_support::{CwdGuard, EnvGuard, GlobalStateLock};
use plan_archive::source::{
    SourceError, default_archive_clone_path, has_dirty_path, local_config_path, resolve_archive,
    resolve_source_repo,
};
use pretty_assertions::assert_eq;

/// `local_config_path` honours the explicit `PLAN_ARCHIVE_LOCAL_CONFIG`
/// override before any XDG/HOME derivation.
#[test]
fn local_config_path_prefers_explicit_override_env() {
    let lock = GlobalStateLock::new();
    let _override = EnvGuard::set(
        &lock,
        "PLAN_ARCHIVE_LOCAL_CONFIG",
        "/custom/plan-archive.yaml",
    );

    assert_eq!(
        local_config_path(),
        PathBuf::from("/custom/plan-archive.yaml")
    );
}

/// With no explicit override, `local_config_path` derives the path from
/// `XDG_CONFIG_HOME`.
#[test]
fn local_config_path_falls_back_to_xdg_config_home() {
    let lock = GlobalStateLock::new();
    let _override = EnvGuard::remove(&lock, "PLAN_ARCHIVE_LOCAL_CONFIG");
    let _xdg = EnvGuard::set(&lock, "XDG_CONFIG_HOME", "/xdg-config");

    assert_eq!(
        local_config_path(),
        PathBuf::from("/xdg-config/agent-plan-archive/config.yaml")
    );
}

/// Without an override or `XDG_CONFIG_HOME`, `local_config_path` derives
/// the path from `$HOME/.config`.
#[test]
fn local_config_path_falls_back_to_home_dot_config() {
    let lock = GlobalStateLock::new();
    let _override = EnvGuard::remove(&lock, "PLAN_ARCHIVE_LOCAL_CONFIG");
    let _xdg = EnvGuard::remove(&lock, "XDG_CONFIG_HOME");
    let _home = EnvGuard::set(&lock, "HOME", "/home/tester");

    assert_eq!(
        local_config_path(),
        PathBuf::from("/home/tester/.config/agent-plan-archive/config.yaml")
    );
}

/// With none of the env vars set, `local_config_path` returns the
/// documented `/nonexistent` sentinel so callers fall back to defaults.
#[test]
fn local_config_path_uses_sentinel_when_no_env_present() {
    let lock = GlobalStateLock::new();
    let _override = EnvGuard::remove(&lock, "PLAN_ARCHIVE_LOCAL_CONFIG");
    let _xdg = EnvGuard::remove(&lock, "XDG_CONFIG_HOME");
    let _home = EnvGuard::remove(&lock, "HOME");

    assert_eq!(
        local_config_path(),
        PathBuf::from("/nonexistent/agent-plan-archive/config.yaml")
    );
}

/// `default_archive_clone_path` reads `archive_clone_path` from the
/// machine-local config pointed at by `PLAN_ARCHIVE_LOCAL_CONFIG`.
#[test]
fn default_archive_clone_path_reads_local_config_override() {
    let lock = GlobalStateLock::new();
    let tmp = tempfile::TempDir::new().unwrap();
    let archive = tmp.path().join("archive-clone");
    let cfg = tmp.path().join("config.yaml");
    fs::write(&cfg, format!("archive_clone_path: {}\n", archive.display())).unwrap();
    let cfg_str = cfg.to_string_lossy().into_owned();
    let _override = EnvGuard::set(&lock, "PLAN_ARCHIVE_LOCAL_CONFIG", &cfg_str);

    let resolved = default_archive_clone_path().expect("default archive clone path");
    assert_eq!(resolved, archive);
}

/// `resolve_archive(None)` falls through to the local-config default and
/// accepts it when the resolved directory exists.
#[test]
fn resolve_archive_none_uses_existing_local_config_default() {
    let lock = GlobalStateLock::new();
    let tmp = tempfile::TempDir::new().unwrap();
    let archive = tmp.path().join("archive-clone");
    fs::create_dir_all(&archive).unwrap();
    let cfg = tmp.path().join("config.yaml");
    fs::write(&cfg, format!("archive_clone_path: {}\n", archive.display())).unwrap();
    let cfg_str = cfg.to_string_lossy().into_owned();
    let _override = EnvGuard::set(&lock, "PLAN_ARCHIVE_LOCAL_CONFIG", &cfg_str);

    let resolved = resolve_archive(None).expect("resolve archive from local config");
    assert_eq!(resolved, archive);
}

/// `resolve_archive(None)` surfaces `ArchiveCloneMissing` when the
/// local-config default points at a non-existent directory.
#[test]
fn resolve_archive_none_reports_missing_when_default_absent() {
    let lock = GlobalStateLock::new();
    let tmp = tempfile::TempDir::new().unwrap();
    let archive = tmp.path().join("does-not-exist");
    let cfg = tmp.path().join("config.yaml");
    fs::write(&cfg, format!("archive_clone_path: {}\n", archive.display())).unwrap();
    let cfg_str = cfg.to_string_lossy().into_owned();
    let _override = EnvGuard::set(&lock, "PLAN_ARCHIVE_LOCAL_CONFIG", &cfg_str);

    let err = resolve_archive(None).expect_err("missing archive clone should fail");
    assert!(
        matches!(err, SourceError::ArchiveCloneMissing(ref p) if *p == archive),
        "expected ArchiveCloneMissing({archive:?}), got: {err:?}"
    );
}

/// `resolve_source_repo(None)` uses the current directory and reports
/// `SourceRepoNotFound` when it is not inside a git work tree.
#[test]
fn resolve_source_repo_none_outside_git_repo_reports_not_found() {
    let lock = GlobalStateLock::new();
    let tmp = tempfile::TempDir::new().unwrap();
    let _cwd = CwdGuard::set(&lock, tmp.path()).expect("set cwd to non-git temp dir");

    let err = resolve_source_repo(None).expect_err("non-git cwd should fail");
    assert!(
        matches!(err, SourceError::SourceRepoNotFound(_)),
        "expected SourceRepoNotFound, got: {err:?}"
    );
}

/// `has_dirty_path` maps a failed `git status` (here: a path that is not
/// a git work tree) onto `SourceError::Io` rather than reporting "clean".
#[test]
fn has_dirty_path_errors_when_not_a_git_repo() {
    let tmp = tempfile::TempDir::new().unwrap();

    let err = has_dirty_path(tmp.path(), Path::new(".")).expect_err("non-git repo should error");
    assert!(
        matches!(err, SourceError::Io(_)),
        "expected Io error from failed git status, got: {err:?}"
    );
}

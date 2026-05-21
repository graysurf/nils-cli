//! Uninstall executor. Walks each `UninstallAction` and reverses the
//! filesystem effect of `install::executor`. Idempotence is the
//! load-bearing invariant: a second uninstall on an already-clean home
//! emits only `NoOp` and exits successfully.

use super::plan::{UninstallAction, UninstallPlan};
use crate::install::link_map::CommentStyle;
use crate::managed_block::{CommentStyle as MbStyle, ManagedBlock, ManagedBlockError};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub enum Mode {
    DryRun,
    Apply,
}

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("managed-block helper rejected entry `{entry_id}` for {config_file}: {source}")]
    ManagedBlock {
        entry_id: String,
        config_file: PathBuf,
        #[source]
        source: ManagedBlockError,
    },
}

/// One change the executor emitted while walking the plan. `Skipped*`
/// variants record "we left this alone" — they are not errors, but the
/// dry-run printer surfaces them so operators can see uninstall declined
/// to delete content it did not own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UninstalledChange {
    /// The symlink at `dest` pointed at the expected install source and
    /// was removed.
    SymlinkRemoved { entry_id: String, dest: PathBuf },
    /// The managed block was located in `config_file` and removed; bytes
    /// outside the marker pair were preserved verbatim.
    ManagedBlockRemoved {
        entry_id: String,
        config_file: PathBuf,
    },
    /// `dest` is a symlink, but it points somewhere other than the
    /// install source we recorded. The executor refuses to remove it.
    SymlinkSkippedForeign {
        entry_id: String,
        dest: PathBuf,
        actual_target: PathBuf,
    },
    /// `dest` is a regular file (not a symlink). Uninstall does not own
    /// destruction of regular files — that responsibility lives in
    /// `restore-backups` (Sprint 2 Task 2.2).
    SymlinkSkippedRegularFile { entry_id: String, dest: PathBuf },
    /// Nothing at `dest` (already removed by a prior run, or never
    /// installed) — idempotent no-op.
    NoOp { entry_id: String, dest: PathBuf },
}

/// Walk `plan.actions`. In `Mode::DryRun` the filesystem is untouched;
/// every change is classified as if `Mode::Apply` had run, so dry-run
/// output mirrors apply output one-for-one.
pub fn run(plan: &UninstallPlan, mode: Mode) -> Result<Vec<UninstalledChange>, ApplyError> {
    let mut changes = Vec::with_capacity(plan.actions.len());
    for action in &plan.actions {
        let change = match action {
            UninstallAction::RemoveSymlink {
                entry_id,
                expected_source,
                dest,
            } => handle_remove_symlink(mode, entry_id, expected_source, dest)?,
            UninstallAction::RemoveManagedBlock {
                entry_id,
                config_file,
                surface,
                comment_style,
            } => handle_remove_managed_block(mode, entry_id, config_file, surface, *comment_style)?,
        };
        changes.push(change);
    }
    Ok(changes)
}

fn handle_remove_symlink(
    mode: Mode,
    entry_id: &str,
    expected_source: &Path,
    dest: &Path,
) -> Result<UninstalledChange, ApplyError> {
    let meta = match fs::symlink_metadata(dest) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(UninstalledChange::NoOp {
                entry_id: entry_id.to_string(),
                dest: dest.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(ApplyError::Io {
                path: dest.to_path_buf(),
                source,
            });
        }
    };
    if meta.file_type().is_symlink() {
        let actual = fs::read_link(dest).map_err(|source| ApplyError::Io {
            path: dest.to_path_buf(),
            source,
        })?;
        if actual != expected_source {
            return Ok(UninstalledChange::SymlinkSkippedForeign {
                entry_id: entry_id.to_string(),
                dest: dest.to_path_buf(),
                actual_target: actual,
            });
        }
        if matches!(mode, Mode::Apply) {
            fs::remove_file(dest).map_err(|source| ApplyError::Io {
                path: dest.to_path_buf(),
                source,
            })?;
        }
        return Ok(UninstalledChange::SymlinkRemoved {
            entry_id: entry_id.to_string(),
            dest: dest.to_path_buf(),
        });
    }
    if meta.file_type().is_file() {
        return Ok(UninstalledChange::SymlinkSkippedRegularFile {
            entry_id: entry_id.to_string(),
            dest: dest.to_path_buf(),
        });
    }
    // Directory or other type at `dest`. Treat as "foreign" with an
    // empty target so the caller surfaces a clear skipped notice.
    Ok(UninstalledChange::SymlinkSkippedForeign {
        entry_id: entry_id.to_string(),
        dest: dest.to_path_buf(),
        actual_target: PathBuf::new(),
    })
}

fn handle_remove_managed_block(
    mode: Mode,
    entry_id: &str,
    config_file: &Path,
    surface: &str,
    comment_style: CommentStyle,
) -> Result<UninstalledChange, ApplyError> {
    let existing = match fs::read_to_string(config_file) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(UninstalledChange::NoOp {
                entry_id: entry_id.to_string(),
                dest: config_file.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(ApplyError::Io {
                path: config_file.to_path_buf(),
                source,
            });
        }
    };
    let helper_style = match comment_style {
        CommentStyle::Hash => MbStyle::Hash,
        CommentStyle::DoubleSlash => MbStyle::DoubleSlash,
    };
    let block = ManagedBlock::new(surface.to_string(), helper_style);
    let new_content = block
        .remove(&existing)
        .map_err(|source| ApplyError::ManagedBlock {
            entry_id: entry_id.to_string(),
            config_file: config_file.to_path_buf(),
            source,
        })?;
    if new_content == existing {
        return Ok(UninstalledChange::NoOp {
            entry_id: entry_id.to_string(),
            dest: config_file.to_path_buf(),
        });
    }
    if matches!(mode, Mode::Apply) {
        fs::write(config_file, new_content).map_err(|source| ApplyError::Io {
            path: config_file.to_path_buf(),
            source,
        })?;
    }
    Ok(UninstalledChange::ManagedBlockRemoved {
        entry_id: entry_id.to_string(),
        config_file: config_file.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uninstall::plan::UninstallAction;
    use std::os::unix::fs as unix_fs;

    fn plan_for(action: UninstallAction) -> UninstallPlan {
        UninstallPlan {
            product: "claude".to_string(),
            home: PathBuf::from("/sandbox/home"),
            actions: vec![action],
        }
    }

    #[test]
    fn symlink_with_no_destination_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let plan = plan_for(UninstallAction::RemoveSymlink {
            entry_id: "x".to_string(),
            expected_source: tmp.path().join("src/file"),
            dest: tmp.path().join("home/missing"),
        });
        let changes = run(&plan, Mode::Apply).unwrap();
        assert!(matches!(
            changes[0],
            UninstalledChange::NoOp { ref entry_id, .. } if entry_id == "x"
        ));
    }

    #[test]
    fn symlink_pointing_at_expected_source_is_removed_on_apply() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("src/file");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "x").unwrap();
        let dest = tmp.path().join("home/link");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        unix_fs::symlink(&source, &dest).unwrap();

        let plan = plan_for(UninstallAction::RemoveSymlink {
            entry_id: "x".to_string(),
            expected_source: source.clone(),
            dest: dest.clone(),
        });
        let changes = run(&plan, Mode::Apply).unwrap();
        assert!(matches!(
            changes[0],
            UninstalledChange::SymlinkRemoved { .. }
        ));
        assert!(!dest.exists());
    }

    #[test]
    fn symlink_pointing_elsewhere_is_skipped_as_foreign() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real_source = tmp.path().join("src/file");
        let foreign_target = tmp.path().join("src/other");
        std::fs::create_dir_all(real_source.parent().unwrap()).unwrap();
        std::fs::write(&real_source, "x").unwrap();
        std::fs::write(&foreign_target, "y").unwrap();
        let dest = tmp.path().join("home/link");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        unix_fs::symlink(&foreign_target, &dest).unwrap();

        let plan = plan_for(UninstallAction::RemoveSymlink {
            entry_id: "x".to_string(),
            expected_source: real_source.clone(),
            dest: dest.clone(),
        });
        let changes = run(&plan, Mode::Apply).unwrap();
        match &changes[0] {
            UninstalledChange::SymlinkSkippedForeign { actual_target, .. } => {
                assert_eq!(actual_target, &foreign_target);
            }
            other => panic!("expected SymlinkSkippedForeign, got {other:?}"),
        }
        // Symlink survives a skip.
        assert!(dest.exists());
    }

    #[test]
    fn dry_run_does_not_mutate_filesystem() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("src/file");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "x").unwrap();
        let dest = tmp.path().join("home/link");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        unix_fs::symlink(&source, &dest).unwrap();

        let plan = plan_for(UninstallAction::RemoveSymlink {
            entry_id: "x".to_string(),
            expected_source: source.clone(),
            dest: dest.clone(),
        });
        let changes = run(&plan, Mode::DryRun).unwrap();
        assert!(matches!(
            changes[0],
            UninstalledChange::SymlinkRemoved { .. }
        ));
        // Dry-run reported the removal but did not perform it.
        assert!(dest.exists());
    }

    #[test]
    fn regular_file_at_dest_is_skipped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let source = tmp.path().join("src/file");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "x").unwrap();
        let dest = tmp.path().join("home/file");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"user-owned").unwrap();

        let plan = plan_for(UninstallAction::RemoveSymlink {
            entry_id: "x".to_string(),
            expected_source: source,
            dest: dest.clone(),
        });
        let changes = run(&plan, Mode::Apply).unwrap();
        assert!(matches!(
            changes[0],
            UninstalledChange::SymlinkSkippedRegularFile { .. }
        ));
        // User-owned file untouched.
        assert_eq!(std::fs::read(&dest).unwrap(), b"user-owned");
    }

    #[test]
    fn managed_block_without_config_file_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let plan = plan_for(UninstallAction::RemoveManagedBlock {
            entry_id: "x".to_string(),
            config_file: tmp.path().join("config.toml"),
            surface: "install".to_string(),
            comment_style: CommentStyle::Hash,
        });
        let changes = run(&plan, Mode::Apply).unwrap();
        assert!(matches!(changes[0], UninstalledChange::NoOp { .. }));
    }

    #[test]
    fn managed_block_present_is_removed_and_preserves_outside_bytes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = tmp.path().join("config.toml");
        std::fs::write(
            &config,
            "alpha\n# >>> agent-runtime-kit:install >>>\nfoo = 1\n# <<< agent-runtime-kit:install <<<\nbeta\n",
        )
        .unwrap();
        let plan = plan_for(UninstallAction::RemoveManagedBlock {
            entry_id: "x".to_string(),
            config_file: config.clone(),
            surface: "install".to_string(),
            comment_style: CommentStyle::Hash,
        });
        let changes = run(&plan, Mode::Apply).unwrap();
        assert!(matches!(
            changes[0],
            UninstalledChange::ManagedBlockRemoved { .. }
        ));
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "alpha\nbeta\n");
    }

    #[test]
    fn managed_block_already_absent_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = tmp.path().join("config.toml");
        std::fs::write(&config, "plain config\n").unwrap();
        let plan = plan_for(UninstallAction::RemoveManagedBlock {
            entry_id: "x".to_string(),
            config_file: config.clone(),
            surface: "install".to_string(),
            comment_style: CommentStyle::Hash,
        });
        let changes = run(&plan, Mode::Apply).unwrap();
        assert!(matches!(changes[0], UninstalledChange::NoOp { .. }));
        // File on disk untouched.
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "plain config\n");
    }
}

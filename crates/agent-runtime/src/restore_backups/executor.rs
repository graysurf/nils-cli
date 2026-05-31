//! Restore executor. Walks each `RestoreAction::RestoreFile` and moves
//! the backup file back to the install destination it came from.
//!
//! ## Filesystem-mode policy
//!
//! `install::executor::move_to_backup` used `fs::rename`, which is an
//! inode-level operation: it preserves mode, ownership, and timestamps
//! verbatim. Restore tries `fs::rename` first so those attributes ride
//! with the file. When `rename` fails with `EXDEV` (cross-device move
//! — backup dir and live home on different filesystems), we fall back
//! to `fs::copy` (which preserves the mode portion via the OS but does
//! NOT change ownership of the new file) followed by removal of the
//! backup. We never invoke `chown`: Plan 04 is not a root-scoped tool,
//! and silently failing chown calls would be worse than not running
//! them at all.

use super::plan::{RestoreAction, RestorePlan};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
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
}

/// One change the executor emitted while walking the plan. `Skipped*`
/// variants record refusal to mutate (operator already wrote a file at
/// the destination, directory in the way, etc.) — they are not errors,
/// but the printer surfaces them so the operator can see exactly what
/// was left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoredChange {
    /// The backup was successfully moved (or, in dry-run, would be
    /// moved) back to `dest`. `from_backup` is the path the file came
    /// from in `<state_home>/backups/`.
    FileRestored {
        entry_id: String,
        dest: PathBuf,
        from_backup: PathBuf,
    },
    /// `dest` is a regular file (not a symlink). Restore will not
    /// overwrite operator content — they may have already restored
    /// manually, or written a replacement.
    SkippedDestRegularFile {
        entry_id: String,
        dest: PathBuf,
        from_backup: PathBuf,
    },
    /// `dest` is a directory. Restore never destroys directories.
    SkippedDestDirectory {
        entry_id: String,
        dest: PathBuf,
        from_backup: PathBuf,
    },
    /// The restore plan flagged this backup as un-matchable in the
    /// regenerated install plan (link-map entry removed). Surface to the
    /// operator so they can decide whether to delete the orphan or
    /// reinstate the link map.
    SkippedNoMatch {
        entry_id: String,
        from_backup: PathBuf,
    },
    /// More than one install-plan action matched `(entry_id, basename)`.
    /// Recursive-tree expansion lost the subpath at install time.
    SkippedAmbiguous {
        entry_id: String,
        from_backup: PathBuf,
        candidates: Vec<PathBuf>,
    },
    /// `dest` is a symlink, but it points somewhere other than the
    /// install source the regenerated plan recorded. Restore refuses to
    /// destroy operator-retargeted symlinks — same contract `uninstall`
    /// enforces. The CLI prints both `actual_target` and
    /// `expected_install_source` so an operator who reshaped their kit
    /// checkout can decide whether to re-point `--source-root` or
    /// manually unwind the symlink.
    SkippedSymlinkForeign {
        entry_id: String,
        dest: PathBuf,
        actual_target: PathBuf,
        expected_install_source: PathBuf,
        from_backup: PathBuf,
    },
}

/// Walk `plan.actions`. In `Mode::DryRun` the filesystem is untouched;
/// every change is classified as if `Mode::Apply` had run, so dry-run
/// output mirrors apply output one-for-one (same convention as install
/// and uninstall executors).
pub fn run(plan: &RestorePlan, mode: Mode) -> Result<Vec<RestoredChange>, ApplyError> {
    let mut changes = Vec::with_capacity(plan.actions.len());
    for action in &plan.actions {
        let change = match action {
            RestoreAction::RestoreFile {
                entry_id,
                source_backup,
                dest,
                expected_install_source,
            } => handle_restore(mode, entry_id, source_backup, dest, expected_install_source)?,
            RestoreAction::SkippedNoMatch {
                entry_id,
                source_backup,
            } => RestoredChange::SkippedNoMatch {
                entry_id: entry_id.clone(),
                from_backup: source_backup.clone(),
            },
            RestoreAction::SkippedAmbiguous {
                entry_id,
                source_backup,
                candidates,
            } => RestoredChange::SkippedAmbiguous {
                entry_id: entry_id.clone(),
                from_backup: source_backup.clone(),
                candidates: candidates.clone(),
            },
        };
        changes.push(change);
    }
    Ok(changes)
}

fn handle_restore(
    mode: Mode,
    entry_id: &str,
    backup: &Path,
    dest: &Path,
    expected_install_source: &Path,
) -> Result<RestoredChange, ApplyError> {
    let meta = fs::symlink_metadata(dest);
    match meta {
        Ok(m) if m.file_type().is_symlink() => {
            let actual = fs::read_link(dest).map_err(|source| ApplyError::Io {
                path: dest.to_path_buf(),
                source,
            })?;
            if actual != expected_install_source {
                return Ok(RestoredChange::SkippedSymlinkForeign {
                    entry_id: entry_id.to_string(),
                    dest: dest.to_path_buf(),
                    actual_target: actual,
                    expected_install_source: expected_install_source.to_path_buf(),
                    from_backup: backup.to_path_buf(),
                });
            }
            match mode {
                Mode::DryRun => Ok(RestoredChange::FileRestored {
                    entry_id: entry_id.to_string(),
                    dest: dest.to_path_buf(),
                    from_backup: backup.to_path_buf(),
                }),
                Mode::Apply => {
                    fs::remove_file(dest).map_err(|source| ApplyError::Io {
                        path: dest.to_path_buf(),
                        source,
                    })?;
                    move_backup_to_dest(backup, dest)?;
                    Ok(RestoredChange::FileRestored {
                        entry_id: entry_id.to_string(),
                        dest: dest.to_path_buf(),
                        from_backup: backup.to_path_buf(),
                    })
                }
            }
        }
        Ok(m) if m.file_type().is_file() => Ok(RestoredChange::SkippedDestRegularFile {
            entry_id: entry_id.to_string(),
            dest: dest.to_path_buf(),
            from_backup: backup.to_path_buf(),
        }),
        Ok(_) => Ok(RestoredChange::SkippedDestDirectory {
            entry_id: entry_id.to_string(),
            dest: dest.to_path_buf(),
            from_backup: backup.to_path_buf(),
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => match mode {
            Mode::DryRun => Ok(RestoredChange::FileRestored {
                entry_id: entry_id.to_string(),
                dest: dest.to_path_buf(),
                from_backup: backup.to_path_buf(),
            }),
            Mode::Apply => {
                ensure_parent_dir(dest)?;
                move_backup_to_dest(backup, dest)?;
                Ok(RestoredChange::FileRestored {
                    entry_id: entry_id.to_string(),
                    dest: dest.to_path_buf(),
                    from_backup: backup.to_path_buf(),
                })
            }
        },
        Err(source) => Err(ApplyError::Io {
            path: dest.to_path_buf(),
            source,
        }),
    }
}

/// Move `backup` to `dest`. Prefer `fs::rename` (preserves all inode
/// attributes); fall back to copy + permission re-apply + delete on
/// `EXDEV`.
fn move_backup_to_dest(backup: &Path, dest: &Path) -> Result<(), ApplyError> {
    match fs::rename(backup, dest) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => copy_then_remove(backup, dest),
        Err(source) => Err(ApplyError::Io {
            path: backup.to_path_buf(),
            source,
        }),
    }
}

fn copy_then_remove(backup: &Path, dest: &Path) -> Result<(), ApplyError> {
    let meta = fs::metadata(backup).map_err(|source| ApplyError::Io {
        path: backup.to_path_buf(),
        source,
    })?;
    fs::copy(backup, dest).map_err(|source| ApplyError::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    let mode = meta.permissions().mode();
    fs::set_permissions(dest, fs::Permissions::from_mode(mode)).map_err(|source| {
        ApplyError::Io {
            path: dest.to_path_buf(),
            source,
        }
    })?;
    fs::remove_file(backup).map_err(|source| ApplyError::Io {
        path: backup.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn is_cross_device(e: &io::Error) -> bool {
    // EXDEV is libc::EXDEV (18 on Linux, 18 on macOS). Use raw_os_error
    // so we do not pull in libc just for this constant.
    e.raw_os_error() == Some(18)
}

fn ensure_parent_dir(dest: &Path) -> Result<(), ApplyError> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|source| ApplyError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_file(p: &Path, content: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    #[test]
    fn apply_restores_symlink_dest_back_to_original_content() {
        let tmp = TempDir::new().unwrap();
        let backup = tmp.path().join("backup/entry/settings.json");
        write_file(&backup, "original-content");
        let install_source = tmp.path().join("source/settings.json");
        write_file(&install_source, "rendered-content");
        let dest = tmp.path().join("home/settings.json");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&install_source, &dest).unwrap();

        let plan = RestorePlan {
            product: "claude".to_string(),
            home: tmp.path().join("home"),
            backup_run: tmp.path().join("backup"),
            actions: vec![RestoreAction::RestoreFile {
                entry_id: "claude.config".to_string(),
                source_backup: backup.clone(),
                dest: dest.clone(),
                expected_install_source: install_source.clone(),
            }],
        };
        let changes = run(&plan, Mode::Apply).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0], RestoredChange::FileRestored { .. }));
        assert!(!backup.exists(), "backup should be moved out");
        assert!(
            !fs::symlink_metadata(&dest)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&dest).unwrap(), "original-content");
    }

    #[test]
    fn dry_run_classifies_but_does_not_touch() {
        let tmp = TempDir::new().unwrap();
        let backup = tmp.path().join("backup/entry/settings.json");
        write_file(&backup, "original");
        let dest = tmp.path().join("home/settings.json");
        write_file(&dest, "post-install-content");

        let plan = RestorePlan {
            product: "claude".to_string(),
            home: tmp.path().join("home"),
            backup_run: tmp.path().join("backup"),
            actions: vec![RestoreAction::RestoreFile {
                entry_id: "claude.config".to_string(),
                source_backup: backup.clone(),
                dest: dest.clone(),
                expected_install_source: PathBuf::from("/unused-in-this-unit-test"),
            }],
        };
        let changes = run(&plan, Mode::DryRun).unwrap();
        // Regular file at dest -> SkippedDestRegularFile regardless of mode.
        assert!(matches!(
            changes[0],
            RestoredChange::SkippedDestRegularFile { .. }
        ));
        assert!(backup.exists(), "dry-run must not move backup");
        assert_eq!(fs::read_to_string(&dest).unwrap(), "post-install-content");
    }

    #[test]
    fn missing_dest_is_restored_in_place() {
        let tmp = TempDir::new().unwrap();
        let backup = tmp.path().join("backup/entry/settings.json");
        write_file(&backup, "original");
        let dest = tmp.path().join("home/sub/settings.json");
        // parent dir does not exist; executor should create it

        let plan = RestorePlan {
            product: "claude".to_string(),
            home: tmp.path().join("home"),
            backup_run: tmp.path().join("backup"),
            actions: vec![RestoreAction::RestoreFile {
                entry_id: "claude.config".to_string(),
                source_backup: backup.clone(),
                dest: dest.clone(),
                expected_install_source: PathBuf::from("/unused-in-this-unit-test"),
            }],
        };
        let changes = run(&plan, Mode::Apply).unwrap();
        assert!(matches!(changes[0], RestoredChange::FileRestored { .. }));
        assert_eq!(fs::read_to_string(&dest).unwrap(), "original");
    }

    #[test]
    fn directory_at_dest_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let backup = tmp.path().join("backup/entry/settings.json");
        write_file(&backup, "x");
        let dest = tmp.path().join("home/settings.json");
        fs::create_dir_all(&dest).unwrap();

        let plan = RestorePlan {
            product: "claude".to_string(),
            home: tmp.path().join("home"),
            backup_run: tmp.path().join("backup"),
            actions: vec![RestoreAction::RestoreFile {
                entry_id: "claude.config".to_string(),
                source_backup: backup.clone(),
                dest: dest.clone(),
                expected_install_source: PathBuf::from("/unused-in-this-unit-test"),
            }],
        };
        let changes = run(&plan, Mode::Apply).unwrap();
        assert!(matches!(
            changes[0],
            RestoredChange::SkippedDestDirectory { .. }
        ));
        assert!(backup.exists());
        assert!(dest.is_dir());
    }

    #[test]
    fn foreign_symlink_target_is_skipped_with_actual_and_expected_recorded() {
        let tmp = TempDir::new().unwrap();
        let backup = tmp.path().join("backup/entry/settings.json");
        write_file(&backup, "original-content");
        let expected_source = tmp.path().join("source/settings.json");
        write_file(&expected_source, "rendered-content");
        let operator_target = tmp.path().join("operator/custom.json");
        write_file(&operator_target, "operator-content");
        let dest = tmp.path().join("home/settings.json");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        // Symlink at dest points somewhere the operator manually retargeted to,
        // not the install source recorded in the plan.
        std::os::unix::fs::symlink(&operator_target, &dest).unwrap();

        let plan = RestorePlan {
            product: "claude".to_string(),
            home: tmp.path().join("home"),
            backup_run: tmp.path().join("backup"),
            actions: vec![RestoreAction::RestoreFile {
                entry_id: "claude.config".to_string(),
                source_backup: backup.clone(),
                dest: dest.clone(),
                expected_install_source: expected_source.clone(),
            }],
        };
        let changes = run(&plan, Mode::Apply).unwrap();
        assert_eq!(changes.len(), 1);
        match &changes[0] {
            RestoredChange::SkippedSymlinkForeign {
                actual_target,
                expected_install_source,
                ..
            } => {
                assert_eq!(actual_target, &operator_target);
                assert_eq!(expected_install_source, &expected_source);
            }
            other => panic!("expected SkippedSymlinkForeign, got {other:?}"),
        }
        // Operator content + backup file both survive.
        assert_eq!(
            fs::read_link(&dest).unwrap(),
            operator_target,
            "executor must not retarget operator symlink"
        );
        assert!(
            backup.exists(),
            "executor must not consume backup when skipping foreign"
        );
    }

    #[test]
    fn skipped_actions_propagate_through_executor() {
        let plan = RestorePlan {
            product: "claude".to_string(),
            home: PathBuf::from("/x"),
            backup_run: PathBuf::from("/y"),
            actions: vec![
                RestoreAction::SkippedNoMatch {
                    entry_id: "gone.entry".to_string(),
                    source_backup: PathBuf::from("/y/gone.entry/file"),
                },
                RestoreAction::SkippedAmbiguous {
                    entry_id: "tree.entry".to_string(),
                    source_backup: PathBuf::from("/y/tree.entry/file"),
                    candidates: vec![PathBuf::from("/a"), PathBuf::from("/b")],
                },
            ],
        };
        let changes = run(&plan, Mode::Apply).unwrap();
        assert_eq!(changes.len(), 2);
        assert!(matches!(changes[0], RestoredChange::SkippedNoMatch { .. }));
        assert!(matches!(
            changes[1],
            RestoredChange::SkippedAmbiguous { .. }
        ));
    }
}

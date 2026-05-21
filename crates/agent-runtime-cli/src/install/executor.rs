//! Apply executor for the install plan. Walks each [`PlanAction`] in
//! order, mutating the filesystem only when the current state diverges
//! from the desired state. Second-run-is-a-no-op idempotence is the
//! load-bearing invariant Plan 04 Sprint 1 Task 1.2 ships against — see
//! the integration test in `tests/integration/install_pipeline.rs`.

use super::plan::{InstallPlan, PlanAction};
use crate::managed_block::{CommentStyle as MbStyle, ManagedBlock};
use std::fs;
use std::io;
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not back up {dest} to {backup}: {source}")]
    Backup {
        dest: PathBuf,
        backup: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("managed-block helper rejected entry `{entry_id}` for {config_file}: {source}")]
    ManagedBlock {
        entry_id: String,
        config_file: PathBuf,
        #[source]
        source: crate::managed_block::ManagedBlockError,
    },
    #[error(
        "tag `{value}` is not a trusted tag name (allowed: ASCII alphanumeric / `-` / `_`, non-empty)"
    )]
    InvalidTag { value: String },
}

/// Tag-name trust contract: non-empty ASCII alphanumeric / `-` / `_`.
/// Mirrors `crate::managed_block::is_trusted_surface` because both produce
/// filesystem-visible names from user-controlled identifiers. Validated at
/// the executor entry as defense in depth — the CLI also rejects bad tags,
/// but library callers using `InstallOptions { tag, .. }` directly hit the
/// same gate.
pub fn is_trusted_tag(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Single applied change. The dry-run printer also emits a list of these
/// (without running them) so the user sees exactly what `--apply` would
/// do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppliedChange {
    SymlinkCreated {
        entry_id: String,
        dest: PathBuf,
        source: PathBuf,
    },
    SymlinkReplaced {
        entry_id: String,
        dest: PathBuf,
        source: PathBuf,
    },
    FileBackedUpThenSymlinked {
        entry_id: String,
        dest: PathBuf,
        source: PathBuf,
        backup: PathBuf,
    },
    ManagedBlockApplied {
        entry_id: String,
        config_file: PathBuf,
    },
    NoOp {
        entry_id: String,
        dest: PathBuf,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum Mode {
    DryRun,
    Apply,
}

/// Walk `plan.actions` and return the change set. In [`Mode::DryRun`]
/// the filesystem is untouched. In [`Mode::Apply`] every divergence is
/// reconciled. `now` is injected so the backup-directory timestamp is
/// deterministic in tests. When `tag` is set and at least one backup
/// directory was created during apply, a `tag-<name>` marker file is
/// written at the backup-run root so `gc-backups` (Task 2.4) can
/// preserve it across retention sweeps.
pub fn run(
    plan: &InstallPlan,
    mode: Mode,
    now: SystemTime,
    tag: Option<&str>,
) -> Result<Vec<AppliedChange>, ApplyError> {
    if let Some(name) = tag
        && !is_trusted_tag(name)
    {
        return Err(ApplyError::InvalidTag {
            value: name.to_string(),
        });
    }
    let backup_root = backup_root_for(plan, now);
    let mut changes = Vec::with_capacity(plan.actions.len());
    for action in &plan.actions {
        let change = match action {
            PlanAction::Symlink {
                entry_id,
                source,
                dest,
                requires_backup,
            } => handle_symlink(mode, entry_id, source, dest, *requires_backup, &backup_root)?,
            PlanAction::ManagedBlock {
                entry_id,
                config_file,
                surface,
                comment_style,
                body,
            } => handle_managed_block(mode, entry_id, config_file, surface, *comment_style, body)?,
        };
        changes.push(change);
    }

    // Write the tag marker only when we are in Apply mode AND at least one
    // backup directory was created during this run. Dry-run never touches
    // state_home; runs with zero backups produce no run root for the tag
    // to live in.
    if let (Mode::Apply, Some(name)) = (mode, tag) {
        let had_backup = changes
            .iter()
            .any(|c| matches!(c, AppliedChange::FileBackedUpThenSymlinked { .. }));
        if had_backup {
            let marker = backup_root.join(format!("tag-{name}"));
            fs::create_dir_all(&backup_root).map_err(|source| ApplyError::Io {
                path: backup_root.clone(),
                source,
            })?;
            fs::write(&marker, b"").map_err(|source| ApplyError::Io {
                path: marker,
                source,
            })?;
        }
    }
    Ok(changes)
}

fn backup_root_for(plan: &InstallPlan, now: SystemTime) -> PathBuf {
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    plan.state_home
        .join("backups")
        .join(&plan.product)
        .join(format!("{secs}"))
}

fn handle_symlink(
    mode: Mode,
    entry_id: &str,
    source: &Path,
    dest: &Path,
    requires_backup_flag: bool,
    backup_root: &Path,
) -> Result<AppliedChange, ApplyError> {
    let current = read_symlink_target(dest);
    if current.as_deref() == Some(source) {
        // Already a symlink pointing at our source. Idempotent no-op.
        return Ok(AppliedChange::NoOp {
            entry_id: entry_id.to_string(),
            dest: dest.to_path_buf(),
        });
    }

    if matches!(mode, Mode::DryRun) {
        return Ok(classify_dry_run(
            entry_id,
            source,
            dest,
            requires_backup_flag,
            backup_root,
        ));
    }

    ensure_parent_dir(dest)?;

    let meta = fs::symlink_metadata(dest);
    match meta {
        Ok(m) if m.file_type().is_symlink() => {
            // Existing symlink pointing somewhere else — replace.
            fs::remove_file(dest).map_err(|source| ApplyError::Io {
                path: dest.to_path_buf(),
                source,
            })?;
            unix_fs::symlink(source, dest).map_err(|source_err| ApplyError::Io {
                path: dest.to_path_buf(),
                source: source_err,
            })?;
            Ok(AppliedChange::SymlinkReplaced {
                entry_id: entry_id.to_string(),
                dest: dest.to_path_buf(),
                source: source.to_path_buf(),
            })
        }
        Ok(m) if m.file_type().is_file() => {
            // Existing regular file — back it up, then symlink.
            let backup = move_to_backup(dest, entry_id, backup_root)?;
            unix_fs::symlink(source, dest).map_err(|source_err| ApplyError::Io {
                path: dest.to_path_buf(),
                source: source_err,
            })?;
            Ok(AppliedChange::FileBackedUpThenSymlinked {
                entry_id: entry_id.to_string(),
                dest: dest.to_path_buf(),
                source: source.to_path_buf(),
                backup,
            })
        }
        Ok(m) => {
            // Directory or other file type at `dest`. Refuse — we don't
            // own destruction of directories.
            Err(ApplyError::Io {
                path: dest.to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "refusing to overwrite non-file destination (file_type={:?})",
                        m.file_type()
                    ),
                ),
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            unix_fs::symlink(source, dest).map_err(|source_err| ApplyError::Io {
                path: dest.to_path_buf(),
                source: source_err,
            })?;
            Ok(AppliedChange::SymlinkCreated {
                entry_id: entry_id.to_string(),
                dest: dest.to_path_buf(),
                source: source.to_path_buf(),
            })
        }
        Err(e) => Err(ApplyError::Io {
            path: dest.to_path_buf(),
            source: e,
        }),
    }
}

fn classify_dry_run(
    entry_id: &str,
    source: &Path,
    dest: &Path,
    requires_backup_flag: bool,
    backup_root: &Path,
) -> AppliedChange {
    let meta = fs::symlink_metadata(dest);
    match meta {
        Ok(m) if m.file_type().is_symlink() => AppliedChange::SymlinkReplaced {
            entry_id: entry_id.to_string(),
            dest: dest.to_path_buf(),
            source: source.to_path_buf(),
        },
        Ok(m) if m.file_type().is_file() => {
            let backup = backup_root
                .join(entry_id)
                .join(dest.file_name().map(Path::new).unwrap_or(Path::new("file")));
            AppliedChange::FileBackedUpThenSymlinked {
                entry_id: entry_id.to_string(),
                dest: dest.to_path_buf(),
                source: source.to_path_buf(),
                backup,
            }
        }
        Ok(_) => AppliedChange::SymlinkReplaced {
            entry_id: entry_id.to_string(),
            dest: dest.to_path_buf(),
            source: source.to_path_buf(),
        },
        Err(_) if requires_backup_flag => {
            // Plan said "requires backup" but the file vanished between
            // plan time and dry-run time — fall back to plain create.
            AppliedChange::SymlinkCreated {
                entry_id: entry_id.to_string(),
                dest: dest.to_path_buf(),
                source: source.to_path_buf(),
            }
        }
        Err(_) => AppliedChange::SymlinkCreated {
            entry_id: entry_id.to_string(),
            dest: dest.to_path_buf(),
            source: source.to_path_buf(),
        },
    }
}

fn move_to_backup(dest: &Path, entry_id: &str, backup_root: &Path) -> Result<PathBuf, ApplyError> {
    let file_name = dest.file_name().map(Path::new).unwrap_or(Path::new("file"));
    let backup_dir = backup_root.join(entry_id);
    fs::create_dir_all(&backup_dir).map_err(|source| ApplyError::Backup {
        dest: dest.to_path_buf(),
        backup: backup_dir.clone(),
        source,
    })?;
    let backup_path = backup_dir.join(file_name);
    fs::rename(dest, &backup_path).map_err(|source| ApplyError::Backup {
        dest: dest.to_path_buf(),
        backup: backup_path.clone(),
        source,
    })?;
    Ok(backup_path)
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

fn read_symlink_target(p: &Path) -> Option<PathBuf> {
    fs::read_link(p).ok()
}

fn handle_managed_block(
    mode: Mode,
    entry_id: &str,
    config_file: &Path,
    surface: &str,
    comment_style: super::link_map::CommentStyle,
    body: &str,
) -> Result<AppliedChange, ApplyError> {
    let helper_style = match comment_style {
        super::link_map::CommentStyle::Hash => MbStyle::Hash,
        super::link_map::CommentStyle::DoubleSlash => MbStyle::DoubleSlash,
    };
    let block = ManagedBlock::new(surface.to_string(), helper_style);

    let existing = match fs::read_to_string(config_file) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(ApplyError::Io {
                path: config_file.to_path_buf(),
                source: e,
            });
        }
    };

    // First install requires `force`; subsequent writes do not.
    let needs_force = block.read(&existing).map(|o| o.is_none()).unwrap_or(true);

    if matches!(mode, Mode::DryRun) {
        let projected = block
            .write(&existing, body, needs_force)
            .map_err(|source| ApplyError::ManagedBlock {
                entry_id: entry_id.to_string(),
                config_file: config_file.to_path_buf(),
                source,
            })?;
        return Ok(if projected == existing {
            AppliedChange::NoOp {
                entry_id: entry_id.to_string(),
                dest: config_file.to_path_buf(),
            }
        } else {
            AppliedChange::ManagedBlockApplied {
                entry_id: entry_id.to_string(),
                config_file: config_file.to_path_buf(),
            }
        });
    }

    let new_content = block
        .write(&existing, body, needs_force)
        .map_err(|source| ApplyError::ManagedBlock {
            entry_id: entry_id.to_string(),
            config_file: config_file.to_path_buf(),
            source,
        })?;
    if new_content == existing {
        return Ok(AppliedChange::NoOp {
            entry_id: entry_id.to_string(),
            dest: config_file.to_path_buf(),
        });
    }
    ensure_parent_dir(config_file)?;
    fs::write(config_file, new_content).map_err(|source| ApplyError::Io {
        path: config_file.to_path_buf(),
        source,
    })?;
    Ok(AppliedChange::ManagedBlockApplied {
        entry_id: entry_id.to_string(),
        config_file: config_file.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::plan::InstallPlan;
    use tempfile::TempDir;

    #[test]
    fn trusted_tag_accepts_alnum_dash_underscore() {
        assert!(is_trusted_tag("pre-bump"));
        assert!(is_trusted_tag("rc_1"));
        assert!(is_trusted_tag("0-2-0"));
        assert!(is_trusted_tag("A1B2C3"));
    }

    #[test]
    fn trusted_tag_rejects_empty_or_unsafe() {
        assert!(!is_trusted_tag(""));
        assert!(!is_trusted_tag("../escape"));
        assert!(!is_trusted_tag("with space"));
        assert!(!is_trusted_tag("dot.in.name"));
        assert!(!is_trusted_tag("slash/in/name"));
        assert!(!is_trusted_tag("null\0byte"));
    }

    #[test]
    fn run_rejects_untrusted_tag_before_walking_actions() {
        // Pins the defense-in-depth gate so a library caller that bypasses
        // the CLI cannot compose `InstallOptions { tag: "../escape", .. }`
        // that sneaks past the friendly anyhow message in commands::install.
        let tmp = TempDir::new().unwrap();
        let plan = InstallPlan {
            product: "claude".to_string(),
            source_root: tmp.path().to_path_buf(),
            home: tmp.path().join("home"),
            state_home: tmp.path().join("state"),
            actions: Vec::new(),
        };
        let err = run(
            &plan,
            Mode::Apply,
            SystemTime::UNIX_EPOCH,
            Some("../escape"),
        )
        .unwrap_err();
        match err {
            ApplyError::InvalidTag { value } => assert_eq!(value, "../escape"),
            other => panic!("expected InvalidTag, got {other:?}"),
        }
    }
}

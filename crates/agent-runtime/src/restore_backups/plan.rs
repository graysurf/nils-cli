//! Restore plan: walks one backup-run directory and matches every
//! backed-up file to a `PlanAction::Symlink` in a regenerated install
//! plan. The match is `(entry_id, dest.file_name())` — install's
//! `move_to_backup` records both into `<run>/<entry_id>/<basename>`, so
//! restoration is fully derivable from the link-map shape without a
//! per-run manifest.

use crate::install::plan::{InstallPlan, PlanAction};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Selector for `--from <timestamp>|latest`. Parsed by the CLI; the
/// resolver under `restore_backups::run` picks an actual directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupRunSelector {
    /// `--from latest` — pick the highest-numbered unix-seconds dir.
    Latest,
    /// `--from <unix-seconds>` — pick that exact dir, or error.
    Exact(u64),
}

impl std::str::FromStr for BackupRunSelector {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("latest") {
            return Ok(Self::Latest);
        }
        s.parse::<u64>().map(Self::Exact).map_err(|err| {
            format!("--from must be `latest` or a unix-seconds timestamp (got `{s}`): {err}")
        })
    }
}

/// One step the restore executor will run. Each action carries the
/// resolved source-of-truth backup path and the destination it should
/// land at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreAction {
    /// Move `source_backup` back to `dest`. Both paths are absolute.
    ///
    /// `expected_install_source` is the absolute path the post-install
    /// symlink at `dest` should be pointing at — recorded from the
    /// regenerated `InstallPlan` so the executor can refuse to clobber
    /// a symlink an operator has manually retargeted away from the
    /// install layout (the same protection `uninstall` enforces).
    RestoreFile {
        entry_id: String,
        source_backup: PathBuf,
        dest: PathBuf,
        expected_install_source: PathBuf,
    },
    /// The backup file matched no `PlanAction::Symlink` in the
    /// regenerated install plan — usually because the link-map entry was
    /// removed or its destination changed between install and restore.
    SkippedNoMatch {
        entry_id: String,
        source_backup: PathBuf,
    },
    /// More than one `PlanAction::Symlink` matched (`entry_id`,
    /// `file_name`). This is the recursive-tree collision case advisory
    /// in the module doc — install dropped the relative subpath, so
    /// restore cannot disambiguate without operator input.
    SkippedAmbiguous {
        entry_id: String,
        source_backup: PathBuf,
        candidates: Vec<PathBuf>,
    },
}

/// Resolved restore plan. `actions` preserves the order the run-directory
/// walk produced (sorted, so test goldens stay stable).
#[derive(Debug, Clone)]
pub struct RestorePlan {
    pub product: String,
    pub home: PathBuf,
    pub backup_run: PathBuf,
    pub actions: Vec<RestoreAction>,
}

/// Errors that can occur while walking the backup-run directory. None
/// fire when the run dir is empty — that case is an empty `actions`
/// vector, not an error.
#[derive(Debug, Error)]
pub enum RestorePlanError {
    #[error("io error reading backup run {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl RestorePlan {
    /// Build a restore plan by walking every regular file under
    /// `backup_run/<entry_id>/` (skipping top-level `tag-*` markers) and
    /// matching it against `install_plan.actions`.
    ///
    /// `surface_filter` (when `Some`) restricts the plan to backup files
    /// whose `entry_id` matches the filter. Unmatched entries are
    /// silently skipped at plan-build time so the executor does not
    /// generate noise for filtered-out backups.
    pub fn from_backup_run(
        backup_run: &Path,
        install_plan: &InstallPlan,
        surface_filter: Option<&str>,
    ) -> Result<Self, RestorePlanError> {
        let mut actions = Vec::new();
        let entries = match std::fs::read_dir(backup_run) {
            Ok(r) => r,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    product: install_plan.product.clone(),
                    home: install_plan.home.clone(),
                    backup_run: backup_run.to_path_buf(),
                    actions,
                });
            }
            Err(source) => {
                return Err(RestorePlanError::Io {
                    path: backup_run.to_path_buf(),
                    source,
                });
            }
        };

        let mut entry_dirs: Vec<(String, PathBuf)> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| RestorePlanError::Io {
                path: backup_run.to_path_buf(),
                source,
            })?;
            let file_type = entry.file_type().map_err(|source| RestorePlanError::Io {
                path: entry.path(),
                source,
            })?;
            if !file_type.is_dir() {
                // `tag-*` markers + any future top-level files: ignore.
                continue;
            }
            let name = match entry.file_name().into_string() {
                Ok(s) => s,
                Err(_) => continue,
            };
            entry_dirs.push((name, entry.path()));
        }
        entry_dirs.sort_by(|a, b| a.0.cmp(&b.0));

        for (entry_id, entry_dir) in entry_dirs {
            if let Some(filter) = surface_filter
                && entry_id != filter
            {
                continue;
            }
            walk_backup_dir(&entry_id, &entry_dir, install_plan, &mut actions)?;
        }

        Ok(Self {
            product: install_plan.product.clone(),
            home: install_plan.home.clone(),
            backup_run: backup_run.to_path_buf(),
            actions,
        })
    }
}

fn walk_backup_dir(
    entry_id: &str,
    dir: &Path,
    install_plan: &InstallPlan,
    out: &mut Vec<RestoreAction>,
) -> Result<(), RestorePlanError> {
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(dir, &mut files)?;
    files.sort();
    for backup_file in files {
        let file_name = match backup_file.file_name() {
            Some(n) => n.to_os_string(),
            None => continue,
        };
        let candidates: Vec<(PathBuf, PathBuf)> = install_plan
            .actions
            .iter()
            .filter_map(|a| match a {
                PlanAction::Symlink {
                    entry_id: id,
                    source,
                    dest,
                    ..
                } if id == entry_id && dest.file_name() == Some(file_name.as_ref()) => {
                    Some((dest.clone(), source.clone()))
                }
                _ => None,
            })
            .collect();
        match candidates.len() {
            1 => {
                let (dest, expected_install_source) =
                    candidates.into_iter().next().expect("len==1");
                out.push(RestoreAction::RestoreFile {
                    entry_id: entry_id.to_string(),
                    source_backup: backup_file,
                    dest,
                    expected_install_source,
                });
            }
            0 => out.push(RestoreAction::SkippedNoMatch {
                entry_id: entry_id.to_string(),
                source_backup: backup_file,
            }),
            _ => out.push(RestoreAction::SkippedAmbiguous {
                entry_id: entry_id.to_string(),
                source_backup: backup_file,
                candidates: candidates.into_iter().map(|(d, _)| d).collect(),
            }),
        }
    }
    Ok(())
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), RestorePlanError> {
    let read = std::fs::read_dir(dir).map_err(|source| RestorePlanError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in read {
        let entry = entry.map_err(|source| RestorePlanError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| RestorePlanError::Io {
            path: entry.path(),
            source,
        })?;
        if file_type.is_dir() {
            collect_files(&entry.path(), out)?;
        } else if file_type.is_file() {
            out.push(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_parses_latest_case_insensitively() {
        assert_eq!(
            "latest".parse::<BackupRunSelector>().unwrap(),
            BackupRunSelector::Latest
        );
        assert_eq!(
            "LATEST".parse::<BackupRunSelector>().unwrap(),
            BackupRunSelector::Latest
        );
    }

    #[test]
    fn selector_parses_unix_seconds() {
        assert_eq!(
            "1700000000".parse::<BackupRunSelector>().unwrap(),
            BackupRunSelector::Exact(1_700_000_000)
        );
    }

    #[test]
    fn selector_rejects_garbage() {
        assert!("yesterday".parse::<BackupRunSelector>().is_err());
        assert!("-5".parse::<BackupRunSelector>().is_err());
    }
}

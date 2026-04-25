use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationPolicy {
    pub max_mb: u64,
    pub keep: u32,
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self {
            max_mb: 10,
            keep: 5,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HistoryWriter {
    history_file: PathBuf,
    rotation: RotationPolicy,
}

impl HistoryWriter {
    pub fn new(history_file: PathBuf, rotation: RotationPolicy) -> Self {
        Self {
            history_file,
            rotation,
        }
    }

    pub fn history_file(&self) -> &Path {
        &self.history_file
    }

    pub fn append(&self, record: &str) -> Result<bool> {
        append_record(&self.history_file, record, self.rotation)
    }
}

fn lock_dir_for(history_file: &Path) -> PathBuf {
    let mut os: OsString = history_file.as_os_str().to_os_string();
    os.push(".lock");
    PathBuf::from(os)
}

fn rotated_path(history_file: &Path, i: u32) -> PathBuf {
    let mut os: OsString = history_file.as_os_str().to_os_string();
    os.push(format!(".{i}"));
    PathBuf::from(os)
}

fn rotate_file_keep_n(history_file: &Path, keep: u32) {
    if keep == 0 || !history_file.is_file() {
        return;
    }

    for i in (1..=keep).rev() {
        let dst = rotated_path(history_file, i);
        let src = if i == 1 {
            history_file.to_path_buf()
        } else {
            rotated_path(history_file, i - 1)
        };

        if !src.exists() {
            continue;
        }

        if let Err(err) = std::fs::remove_file(&dst)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "warning: history rotation failed to remove {}: {err}",
                dst.display()
            );
            return;
        }
        if let Err(err) = std::fs::rename(&src, &dst) {
            eprintln!(
                "warning: history rotation failed to rename {} -> {}: {err}",
                src.display(),
                dst.display()
            );
            return;
        }
    }
}

/// Resolve a history file path from `<setup_dir>` and an optional override.
///
/// Parity:
/// - if override is an absolute path, use it as-is
/// - if override is relative, resolve it under `<setup_dir>`
/// - otherwise use `<setup_dir>/<default_filename>`
pub fn resolve_history_file(
    setup_dir: &Path,
    override_path: Option<&Path>,
    default_filename: &str,
) -> PathBuf {
    match override_path {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => setup_dir.join(p),
        None => setup_dir.join(default_filename),
    }
}

/// Append a record to the history file using a lock directory (`<history_file>.lock`).
///
/// Returns:
/// - `Ok(true)` when a record was appended
/// - `Ok(false)` when the lock could not be acquired (skip silently)
pub fn append_record(history_file: &Path, record: &str, rotation: RotationPolicy) -> Result<bool> {
    let Some(parent) = history_file.parent() else {
        return Ok(false);
    };

    let _ = std::fs::create_dir_all(parent);

    let lock_dir = lock_dir_for(history_file);
    if std::fs::create_dir(&lock_dir).is_err() {
        return Ok(false);
    }
    let _lock_guard = LockGuard { lock_dir };

    if rotation.max_mb > 0 && history_file.is_file() {
        let bytes = std::fs::metadata(history_file)
            .map(|m| m.len())
            .unwrap_or(0);
        let max_bytes = rotation.max_mb * 1024 * 1024;
        if bytes >= max_bytes {
            rotate_file_keep_n(history_file, rotation.keep.max(1));
        }
    }

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(history_file)
        .with_context(|| format!("open history file for append: {}", history_file.display()))?;

    f.write_all(record.as_bytes())
        .context("write history record")?;

    Ok(true)
}

#[derive(Debug)]
struct LockGuard {
    lock_dir: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.lock_dir);
    }
}

/// Read blank-line-separated history records as raw strings.
pub fn read_records(history_file: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(history_file)
        .with_context(|| format!("read history file: {}", history_file.display()))?;
    let content = content.replace("\r\n", "\n");

    Ok(content
        .split("\n\n")
        .map(str::trim)
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!("{s}\n\n"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    use tempfile::TempDir;

    #[test]
    fn history_append_skips_when_lock_is_held() {
        let tmp = TempDir::new().expect("tmp");
        let setup_dir = tmp.path();
        let history_file = setup_dir.join(".rest_history");

        std::fs::create_dir_all(setup_dir).expect("mkdir");
        std::fs::create_dir(lock_dir_for(&history_file)).expect("lock");

        let appended =
            append_record(&history_file, "# entry\n\n", RotationPolicy::default()).unwrap();
        assert!(!appended);
        assert!(!history_file.exists());
    }

    #[test]
    fn history_rotation_happens_before_append() {
        let tmp = TempDir::new().expect("tmp");
        let setup_dir = tmp.path();
        let history_file = setup_dir.join(".rest_history");

        std::fs::create_dir_all(setup_dir).expect("mkdir");
        std::fs::write(&history_file, vec![b'a'; 1024 * 1024]).expect("write big file");

        let appended = append_record(
            &history_file,
            "# new\n\n",
            RotationPolicy { max_mb: 1, keep: 2 },
        )
        .unwrap();
        assert!(appended);

        assert!(history_file.is_file());
        assert_eq!(std::fs::read_to_string(&history_file).unwrap(), "# new\n\n");
        assert!(setup_dir.join(".rest_history.1").is_file());
    }

    #[test]
    fn history_read_records_splits_blank_lines_and_preserves_trailing_blank_line() {
        let tmp = TempDir::new().expect("tmp");
        let history_file = tmp.path().join(".rest_history");
        std::fs::write(&history_file, "# a\ncmd\n\n# b\ncmd2\n\n").expect("write");

        let records = read_records(&history_file).unwrap();
        assert_eq!(records.len(), 2);
        assert!(records[0].ends_with("\n\n"));
        assert!(records[1].ends_with("\n\n"));
    }
}

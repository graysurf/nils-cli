use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use nils_common::fs as shared_fs;

use crate::rate_limits;
use crate::rate_limits::client::{UsageRequest, fetch_usage};
use crate::rate_limits::render as rate_render;

use super::render as prompt_segment_render;

const REFRESH_MARKER_MODE: u32 = 0o644;

pub(crate) fn enqueue_background_refresh(target_file: &Path) {
    let cache_file = match rate_limits::cache_file_for_target(target_file) {
        Ok(value) => value,
        Err(_) => return,
    };
    let lock_dir = match lock_dir_for_cache_file(&cache_file) {
        Some(value) => value,
        None => return,
    };

    let refresh_min_seconds = super::env_u64("GEMINI_PROMPT_SEGMENT_REFRESH_MIN_SECONDS", 30);
    if refresh_min_seconds > 0 && is_within_min_interval(&cache_file, refresh_min_seconds) {
        return;
    }

    let lock_stale_seconds = super::env_u64("GEMINI_PROMPT_SEGMENT_LOCK_STALE_SECONDS", 90);
    if lock_dir.exists() && !is_stale(&lock_dir, lock_stale_seconds) {
        return;
    }

    write_last_attempt(&cache_file);

    let exe = match refresh_exe() {
        Some(value) => value,
        None => return,
    };

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("prompt-segment").arg("--refresh");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());

    let _ = cmd.spawn();
}

pub(crate) fn refresh_blocking(target_file: &Path) -> Option<prompt_segment_render::CacheEntry> {
    let cache_file = rate_limits::cache_file_for_target(target_file).ok()?;
    let lock_dir = lock_dir_for_cache_file(&cache_file)?;
    let lock_stale_seconds = super::env_u64("GEMINI_PROMPT_SEGMENT_LOCK_STALE_SECONDS", 90);

    let _lock = RefreshLock::acquire(&lock_dir, lock_stale_seconds)?;

    let entry = fetch_and_write_cache(target_file).ok()?;
    write_last_attempt(&cache_file);
    Some(entry)
}

fn fetch_and_write_cache(target_file: &Path) -> anyhow::Result<prompt_segment_render::CacheEntry> {
    let connect_timeout = super::env_u64("GEMINI_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS", 2);
    let max_time = super::env_u64("GEMINI_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS", 8);

    let usage_request = UsageRequest {
        target_file: target_file.to_path_buf(),
        refresh_on_401: false,
        endpoint: super::code_assist_endpoint(),
        api_version: super::code_assist_api_version(),
        project: super::code_assist_project(),
        connect_timeout_seconds: connect_timeout,
        max_time_seconds: max_time,
    };

    let usage = fetch_usage(&usage_request).map_err(anyhow::Error::msg)?;
    let usage_data = rate_render::parse_usage(&usage.body)
        .ok_or_else(|| anyhow::anyhow!("invalid usage payload"))?;
    let values = rate_render::render_values(&usage_data);
    let weekly = rate_render::weekly_values(&values);

    let fetched_at_epoch = super::now_epoch();
    if fetched_at_epoch > 0 {
        let _ = rate_limits::write_prompt_segment_cache(
            target_file,
            fetched_at_epoch,
            &weekly.non_weekly_label,
            weekly.non_weekly_remaining,
            weekly.weekly_remaining,
            weekly.weekly_reset_epoch,
            weekly.non_weekly_reset_epoch,
        );
    }

    Ok(prompt_segment_render::CacheEntry {
        fetched_at_epoch,
        non_weekly_label: weekly.non_weekly_label,
        non_weekly_remaining: weekly.non_weekly_remaining,
        non_weekly_reset_epoch: weekly.non_weekly_reset_epoch,
        weekly_remaining: weekly.weekly_remaining,
        weekly_reset_epoch: weekly.weekly_reset_epoch,
    })
}

fn refresh_exe() -> Option<PathBuf> {
    super::env_non_empty("GEMINI_PROMPT_SEGMENT_EXE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
}

struct RefreshLock {
    dir: PathBuf,
}

impl RefreshLock {
    fn acquire(dir: &Path, stale_seconds: u64) -> Option<Self> {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        match std::fs::create_dir(dir) {
            Ok(()) => {
                return Some(Self {
                    dir: dir.to_path_buf(),
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }

        if !is_stale(dir, stale_seconds) {
            return None;
        }

        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir(dir).ok()?;
        Some(Self {
            dir: dir.to_path_buf(),
        })
    }
}

impl Drop for RefreshLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn lock_dir_for_cache_file(cache_file: &Path) -> Option<PathBuf> {
    let stem = cache_file.file_stem()?.to_string_lossy();
    Some(cache_file.with_file_name(format!("{stem}.refresh.lock")))
}

fn last_attempt_path(cache_file: &Path) -> Option<PathBuf> {
    let stem = cache_file.file_stem()?.to_string_lossy();
    Some(cache_file.with_file_name(format!("{stem}.refresh.at")))
}

fn write_last_attempt(cache_file: &Path) {
    let path = match last_attempt_path(cache_file) {
        Some(value) => value,
        None => return,
    };
    let now_epoch = super::now_epoch();
    if now_epoch <= 0 {
        return;
    }

    // Use an atomic temp+rename so a concurrent `is_within_min_interval`
    // reader never observes an empty or partial marker (which would parse
    // as zero, disarm the gate, and fire a redundant background refresh).
    let _ = shared_fs::write_atomic(&path, now_epoch.to_string().as_bytes(), REFRESH_MARKER_MODE);
}

fn is_within_min_interval(cache_file: &Path, refresh_min_seconds: u64) -> bool {
    let path = match last_attempt_path(cache_file) {
        Some(value) => value,
        None => return false,
    };
    let content = match std::fs::read_to_string(path) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let last = match content.trim().parse::<i64>() {
        Ok(value) => value,
        Err(_) => return false,
    };
    if last <= 0 {
        return false;
    }
    let now = super::now_epoch();
    if now <= 0 {
        return false;
    }
    (now - last) >= 0 && (now - last) < refresh_min_seconds as i64
}

fn is_stale_modified(
    modified: std::io::Result<SystemTime>,
    stale_seconds: u64,
    now: SystemTime,
) -> bool {
    let modified = match modified {
        Ok(value) => value,
        Err(_) => return true,
    };
    let age = match now.duration_since(modified) {
        Ok(value) => value,
        Err(_) => Duration::from_secs(0),
    };
    age.as_secs() >= stale_seconds
}

fn is_stale(dir: &Path, stale_seconds: u64) -> bool {
    if stale_seconds == 0 {
        return true;
    }

    let meta = match std::fs::metadata(dir) {
        Ok(value) => value,
        Err(_) => return true,
    };
    is_stale_modified(meta.modified(), stale_seconds, SystemTime::now())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use std::io;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn refresh_lock_acquire_creates_and_cleans_up() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock_dir = tmp.path().join("nested").join("usage.refresh.lock");

        let lock = RefreshLock::acquire(&lock_dir, 60).expect("acquire");
        assert!(lock_dir.is_dir());

        drop(lock);
        assert!(!lock_dir.exists());
    }

    #[test]
    fn refresh_lock_acquire_returns_none_when_lock_exists_and_not_stale() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock_dir = tmp.path().join("usage.refresh.lock");
        std::fs::create_dir(&lock_dir).expect("create lock");

        assert!(RefreshLock::acquire(&lock_dir, 3600).is_none());
        assert!(lock_dir.is_dir());
    }

    #[test]
    fn refresh_lock_acquire_reclaims_lock_when_stale_seconds_zero() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock_dir = tmp.path().join("usage.refresh.lock");
        std::fs::create_dir(&lock_dir).expect("create lock");
        let marker = lock_dir.join("marker");
        std::fs::write(&marker, b"marker").expect("write marker");
        assert!(marker.is_file());

        let lock = RefreshLock::acquire(&lock_dir, 0).expect("acquire");
        assert!(lock_dir.is_dir());
        assert!(!marker.exists());

        drop(lock);
        assert!(!lock_dir.exists());
    }

    #[test]
    fn refresh_lock_acquire_returns_none_on_create_dir_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let parent_file = tmp.path().join("not_a_dir");
        std::fs::write(&parent_file, b"not a dir").expect("write parent file");

        let lock_dir = parent_file.join("usage.refresh.lock");
        assert!(RefreshLock::acquire(&lock_dir, 60).is_none());
    }

    #[test]
    fn lock_dir_for_cache_file_appends_refresh_lock_suffix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cache_file = tmp.path().join("usage.json");

        let lock_dir = lock_dir_for_cache_file(&cache_file).expect("lock dir");
        assert_eq!(lock_dir, tmp.path().join("usage.refresh.lock"));
    }

    #[test]
    fn last_attempt_path_appends_refresh_marker_suffix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cache_file = tmp.path().join("usage.kv");

        let marker = last_attempt_path(&cache_file).expect("marker path");
        assert_eq!(marker, tmp.path().join("usage.refresh.at"));
    }

    #[test]
    fn is_stale_returns_true_when_stale_seconds_zero() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock_dir = tmp.path().join("usage.refresh.lock");
        assert!(is_stale(&lock_dir, 0));
    }

    #[test]
    fn is_stale_returns_true_when_metadata_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("missing.refresh.lock");
        assert!(is_stale(&missing, 60));
    }

    #[test]
    fn is_stale_modified_returns_true_when_modified_fails() {
        let err = io::Error::other("boom");
        assert!(is_stale_modified(Err(err), 60, SystemTime::now()));
    }

    #[test]
    fn is_stale_modified_handles_future_modified_time() {
        let now = UNIX_EPOCH;
        let modified = UNIX_EPOCH + Duration::from_secs(10);
        assert!(!is_stale_modified(Ok(modified), 1, now));
    }

    #[test]
    fn is_stale_modified_returns_true_when_age_exceeds_threshold() {
        let now = UNIX_EPOCH + Duration::from_secs(100);
        let modified = UNIX_EPOCH + Duration::from_secs(90);
        assert!(is_stale_modified(Ok(modified), 5, now));
    }

    #[test]
    fn is_stale_modified_returns_false_when_age_below_threshold() {
        let now = UNIX_EPOCH + Duration::from_secs(100);
        let modified = UNIX_EPOCH + Duration::from_secs(99);
        assert!(!is_stale_modified(Ok(modified), 10, now));
    }
}

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use nils_common::fs as shared_fs;

use super::{auth, cache, client};

const DEFAULT_REFRESH_MIN_SECONDS: u64 = 60;

pub(crate) fn enqueue_background_refresh(cache_file: &Path) {
    let refresh_min_seconds = env_u64(
        "CLAUDE_PROMPT_SEGMENT_REFRESH_MIN_SECONDS",
        DEFAULT_REFRESH_MIN_SECONDS,
    );
    if refresh_min_seconds > 0 && is_within_min_interval(cache_file, refresh_min_seconds) {
        return;
    }
    let Some(spawn_lock_file) = sibling_path(cache_file, "refresh.spawn.lock") else {
        return;
    };
    let Some(_spawn_permit) = RefreshLock::acquire(&spawn_lock_file) else {
        return;
    };
    if refresh_min_seconds > 0 && is_within_min_interval(cache_file, refresh_min_seconds) {
        return;
    }
    let Some(refresh_lock_file) = sibling_path(cache_file, "refresh.lock") else {
        return;
    };
    let Some(refresh_probe) = RefreshLock::acquire(&refresh_lock_file) else {
        return;
    };
    drop(refresh_probe);

    let executable = std::env::var_os("CLAUDE_PROMPT_SEGMENT_EXE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok());
    let Some(executable) = executable else {
        return;
    };
    if Command::new(executable)
        .args(["prompt-segment", "--refresh"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
    {
        write_last_attempt(cache_file);
    }
}

pub(crate) fn refresh_blocking(cache_file: &Path) -> bool {
    let Some(lock_file) = sibling_path(cache_file, "refresh.lock") else {
        return false;
    };
    let Some(_lock) = RefreshLock::acquire(&lock_file) else {
        return false;
    };
    let Some(token) = auth::resolve_access_token() else {
        return false;
    };
    let Ok(body) = client::fetch_usage(&token.value) else {
        write_last_attempt(cache_file);
        return false;
    };
    let wrote = cache::write_cache_file(cache_file, &body).is_ok();
    write_last_attempt(cache_file);
    wrote
}

struct RefreshLock {
    file: File,
}

impl RefreshLock {
    fn acquire(path: &Path) -> Option<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        let acquired = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if !acquired {
            return None;
        }
        Some(Self { file })
    }
}

impl Drop for RefreshLock {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn sibling_path(path: &Path, suffix: &str) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy();
    Some(path.with_file_name(format!("{stem}.{suffix}")))
}

fn last_attempt_path(cache_file: &Path) -> Option<PathBuf> {
    sibling_path(cache_file, "refresh.at")
}

fn write_last_attempt(cache_file: &Path) {
    let Some(path) = last_attempt_path(cache_file) else {
        return;
    };
    let Some(now_epoch) = now_epoch() else {
        return;
    };
    let _ = shared_fs::write_atomic(
        &path,
        now_epoch.to_string().as_bytes(),
        shared_fs::SECRET_FILE_MODE,
    );
}

fn is_within_min_interval(cache_file: &Path, refresh_min_seconds: u64) -> bool {
    let Some(path) = last_attempt_path(cache_file) else {
        return false;
    };
    let Some(last) = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
    else {
        return false;
    };
    let Some(now) = now_epoch() else {
        return false;
    };
    last <= now.saturating_add(5) && now.saturating_sub(last) < refresh_min_seconds
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn now_epoch() -> Option<u64> {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn refresh_lock_is_exclusive_and_released_on_drop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock_file = tmp.path().join("usage.refresh.lock");
        let lock = RefreshLock::acquire(&lock_file).expect("lock");
        assert!(RefreshLock::acquire(&lock_file).is_none());
        drop(lock);
        assert!(lock_file.is_file());
        assert!(RefreshLock::acquire(&lock_file).is_some());
    }

    #[test]
    fn simultaneous_lock_contenders_have_exactly_one_winner() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock_file = Arc::new(tmp.path().join("usage.refresh.lock"));
        let barrier = Arc::new(Barrier::new(8));
        let attempts = Arc::new(AtomicUsize::new(0));
        let winners = Arc::new(AtomicUsize::new(0));
        let handles = (0..8)
            .map(|_| {
                let lock_file = Arc::clone(&lock_file);
                let barrier = Arc::clone(&barrier);
                let attempts = Arc::clone(&attempts);
                let winners = Arc::clone(&winners);
                thread::spawn(move || {
                    barrier.wait();
                    let lock = RefreshLock::acquire(&lock_file);
                    attempts.fetch_add(1, Ordering::SeqCst);
                    if lock.is_some() {
                        while attempts.load(Ordering::SeqCst) < 8 {
                            thread::yield_now();
                        }
                        winners.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("lock contender");
        }
        assert_eq!(winners.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn sibling_paths_are_deterministic() {
        let path = Path::new("/tmp/cache/usage.json");
        assert_eq!(
            sibling_path(path, "refresh.at"),
            Some(PathBuf::from("/tmp/cache/usage.refresh.at"))
        );
    }
}

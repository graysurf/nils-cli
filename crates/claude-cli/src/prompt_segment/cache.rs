use anyhow::{Context, Result};
use nils_common::env as shared_env;
use nils_common::fs as shared_fs;
use nils_common::usage_cache_policy;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const CACHE_FILE_NAME: &str = "usage.json";
const EXPIRED_REFRESH_COOLDOWN_SECONDS: u64 = 60;
const EXPIRED_REFRESH_LOCK_STALE_SECONDS: u64 = 60;

#[derive(Clone, Copy, Debug)]
pub struct CacheSnapshot {
    exists: bool,
    modified: Option<SystemTime>,
    observed_at: SystemTime,
}

impl CacheSnapshot {
    pub fn exists(self) -> bool {
        self.exists
    }

    pub fn stale(self, ttl_seconds: u64) -> bool {
        if ttl_seconds == 0 || !self.exists {
            return true;
        }
        let Some(modified) = self.modified else {
            return true;
        };
        self.observed_at
            .duration_since(modified)
            .unwrap_or(Duration::ZERO)
            .as_secs()
            >= ttl_seconds
    }

    pub fn display_expired(self) -> bool {
        let Some(modified) = self.modified.filter(|_| self.exists) else {
            return true;
        };
        !usage_cache_policy::classify_display_age_seconds(signed_age_seconds(
            self.observed_at,
            modified,
        ))
        .is_display_eligible()
    }
}

pub struct ExpiredRefreshGuard {
    lock_dir: PathBuf,
}

impl Drop for ExpiredRefreshGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.lock_dir);
    }
}

pub fn cache_file() -> Option<PathBuf> {
    let dir = cache_dir()?;
    Some(dir.join(CACHE_FILE_NAME))
}

pub fn read_cache_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

pub fn write_cache_file(path: &Path, body: &str) -> Result<()> {
    shared_fs::write_atomic(path, body.as_bytes(), shared_fs::SECRET_FILE_MODE)
        .with_context(|| format!("failed to write cache: {}", path.display()))
}

pub fn snapshot(path: &Path) -> CacheSnapshot {
    snapshot_at(path, SystemTime::now())
}

fn snapshot_at(path: &Path, observed_at: SystemTime) -> CacheSnapshot {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => CacheSnapshot {
            exists: true,
            modified: metadata.modified().ok(),
            observed_at,
        },
        _ => CacheSnapshot {
            exists: false,
            modified: None,
            observed_at,
        },
    }
}

pub fn cache_display_expired(path: &Path) -> bool {
    cache_display_expired_at(path, SystemTime::now())
}

fn cache_display_expired_at(path: &Path, now: SystemTime) -> bool {
    snapshot_at(path, now).display_expired()
}

pub fn begin_expired_refresh(path: &Path) -> Option<ExpiredRefreshGuard> {
    let lock_dir = sibling_path(path, "refresh.lock")?;
    let guard = acquire_refresh_lock(&lock_dir, EXPIRED_REFRESH_LOCK_STALE_SECONDS)?;
    let attempt_path = sibling_path(path, "refresh.at")?;
    if refresh_attempt_recent(&attempt_path, EXPIRED_REFRESH_COOLDOWN_SECONDS) {
        return None;
    }

    let now_epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    shared_fs::write_atomic(
        &attempt_path,
        now_epoch.to_string().as_bytes(),
        shared_fs::SECRET_FILE_MODE,
    )
    .ok()?;
    Some(guard)
}

fn sibling_path(path: &Path, suffix: &str) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy();
    Some(path.with_file_name(format!("{stem}.{suffix}")))
}

fn acquire_refresh_lock(path: &Path, stale_seconds: u64) -> Option<ExpiredRefreshGuard> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    match std::fs::create_dir(path) {
        Ok(()) => {
            return Some(ExpiredRefreshGuard {
                lock_dir: path.to_path_buf(),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return None,
    }

    if !path_is_stale(path, stale_seconds) {
        return None;
    }
    std::fs::remove_dir_all(path).ok()?;
    std::fs::create_dir(path).ok()?;
    Some(ExpiredRefreshGuard {
        lock_dir: path.to_path_buf(),
    })
}

fn path_is_stale(path: &Path, stale_seconds: u64) -> bool {
    if stale_seconds == 0 {
        return true;
    }
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age.as_secs() >= stale_seconds)
        .unwrap_or(true)
}

fn refresh_attempt_recent(path: &Path, cooldown_seconds: u64) -> bool {
    let Some(attempt_epoch) = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
    else {
        return false;
    };
    let Some(now_epoch) = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
    else {
        return false;
    };
    attempt_epoch <= now_epoch.saturating_add(5)
        && now_epoch.saturating_sub(attempt_epoch) < cooldown_seconds
}

fn signed_age_seconds(now: SystemTime, modified: SystemTime) -> Option<i64> {
    match now.duration_since(modified) {
        Ok(age) => i64::try_from(age.as_secs()).ok(),
        Err(_) => {
            let ahead = modified.duration_since(now).ok()?;
            let whole_seconds = i64::try_from(ahead.as_secs()).ok()?;
            let rounded_seconds = whole_seconds.checked_add(i64::from(ahead.subsec_nanos() > 0))?;
            rounded_seconds.checked_neg()
        }
    }
}

fn cache_dir() -> Option<PathBuf> {
    if let Some(value) = shared_env::env_non_empty("CLAUDE_PROMPT_SEGMENT_CACHE_DIR") {
        return Some(PathBuf::from(value));
    }

    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    if cfg!(target_os = "macos") {
        Some(
            home.join("Library")
                .join("Caches")
                .join("claude-prompt-segment"),
        )
    } else if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from) {
        Some(xdg.join("claude-prompt-segment"))
    } else {
        Some(home.join(".cache").join("claude-prompt-segment"))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        begin_expired_refresh, cache_display_expired_at, signed_age_seconds, snapshot, snapshot_at,
    };
    use pretty_assertions::assert_eq;
    use std::fs::File;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn cache_stale_treats_zero_ttl_as_stale() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("usage.json");
        std::fs::write(&path, "{}").expect("write");
        assert!(snapshot(&path).stale(0));
    }

    #[test]
    fn cache_stale_uses_file_mtime() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("usage.json");
        std::fs::write(&path, "{}").expect("write");
        let file = File::options().write(true).open(&path).expect("open");
        file.set_modified(SystemTime::now() - Duration::from_secs(120))
            .expect("set modified");

        assert!(snapshot(&path).stale(60));
        assert!(!snapshot(&path).stale(180));
    }

    #[test]
    fn cache_display_expiry_starts_at_600_seconds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("usage.json");
        std::fs::write(&path, "{}").expect("write");
        let modified = UNIX_EPOCH + Duration::from_secs(1_000);
        let file = File::options().write(true).open(&path).expect("open");
        file.set_modified(modified).expect("set modified");

        assert!(!cache_display_expired_at(
            &path,
            modified + Duration::from_secs(599)
        ));
        assert!(cache_display_expired_at(
            &path,
            modified + Duration::from_secs(600)
        ));
    }

    #[test]
    fn cache_snapshot_reuses_one_observation_for_ttl_and_display_policy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("usage.json");
        std::fs::write(&path, "{}").expect("write");
        let modified = UNIX_EPOCH + Duration::from_secs(1_000);
        let file = File::options().write(true).open(&path).expect("open");
        file.set_modified(modified).expect("set modified");

        let eligible = snapshot_at(&path, modified + Duration::from_secs(599));
        assert!(eligible.exists());
        assert!(eligible.stale(60));
        assert!(!eligible.display_expired());

        let expired = snapshot_at(&path, modified + Duration::from_secs(600));
        assert!(expired.stale(60));
        assert!(expired.display_expired());
    }

    #[test]
    fn expired_refresh_gate_coalesces_lock_and_cools_down_after_attempt() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("usage.json");
        std::fs::write(&path, "{}").expect("write");

        let guard = begin_expired_refresh(&path).expect("first refresh permit");
        assert!(begin_expired_refresh(&path).is_none());
        drop(guard);
        assert!(begin_expired_refresh(&path).is_none());
    }

    #[test]
    fn cache_display_future_clock_tolerance_ends_after_5_seconds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("usage.json");
        std::fs::write(&path, "{}").expect("write");
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let file = File::options().write(true).open(&path).expect("open");

        file.set_modified(now + Duration::from_secs(5))
            .expect("set modified within tolerance");
        assert!(!cache_display_expired_at(&path, now));

        file.set_modified(now + Duration::from_secs(6))
            .expect("set modified beyond tolerance");
        assert!(cache_display_expired_at(&path, now));
    }

    #[test]
    fn cache_display_future_age_conversion_rounds_away_from_zero() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);

        assert_eq!(
            signed_age_seconds(now, now + Duration::from_secs(5)),
            Some(-5)
        );
        assert_eq!(
            signed_age_seconds(now, now + Duration::from_secs(5) + Duration::from_nanos(1)),
            Some(-6)
        );
    }
}

use anyhow::{Context, Result};
use nils_common::env as shared_env;
use nils_common::fs as shared_fs;
use nils_common::usage_cache_policy;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const CACHE_FILE_NAME: &str = "usage.json";

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

pub fn cache_stale(path: &Path, ttl_seconds: u64) -> bool {
    if ttl_seconds == 0 {
        return true;
    }
    let modified = match std::fs::metadata(path).and_then(|meta| meta.modified()) {
        Ok(value) => value,
        Err(_) => return true,
    };
    let age = match SystemTime::now().duration_since(modified) {
        Ok(value) => value,
        Err(_) => Duration::from_secs(0),
    };
    age.as_secs() >= ttl_seconds
}

pub fn cache_display_expired(path: &Path) -> bool {
    cache_display_expired_at(path, SystemTime::now())
}

fn cache_display_expired_at(path: &Path, now: SystemTime) -> bool {
    let modified = match std::fs::metadata(path).and_then(|meta| meta.modified()) {
        Ok(value) => value,
        Err(_) => return true,
    };
    !usage_cache_policy::classify_display_age_seconds(signed_age_seconds(now, modified))
        .is_display_eligible()
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
    use super::{cache_display_expired_at, cache_stale, signed_age_seconds};
    use pretty_assertions::assert_eq;
    use std::fs::File;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn cache_stale_treats_zero_ttl_as_stale() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("usage.json");
        std::fs::write(&path, "{}").expect("write");
        assert!(cache_stale(&path, 0));
    }

    #[test]
    fn cache_stale_uses_file_mtime() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("usage.json");
        std::fs::write(&path, "{}").expect("write");
        let file = File::options().write(true).open(&path).expect("open");
        file.set_modified(SystemTime::now() - Duration::from_secs(120))
            .expect("set modified");

        assert!(cache_stale(&path, 60));
        assert!(!cache_stale(&path, 180));
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

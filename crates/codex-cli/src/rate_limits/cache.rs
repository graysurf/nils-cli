use anyhow::{Context, Result};
use nils_common::fs as shared_fs;
use nils_common::usage_cache_policy;
use std::fs;
use std::path::{Path, PathBuf};

use crate::auth;
use crate::paths;
use crate::rate_limits::render;
use nils_common::env as shared_env;

#[derive(Debug)]
pub struct CacheEntry {
    pub fetched_at_epoch: Option<i64>,
    pub non_weekly_label: Option<String>,
    pub non_weekly_remaining: Option<i64>,
    pub non_weekly_reset_epoch: Option<i64>,
    pub weekly_remaining: Option<i64>,
    pub weekly_reset_epoch: Option<i64>,
}

const DEFAULT_CACHE_TTL_SECONDS: u64 = 180;
const CACHE_MISS_HINT: &str =
    "rerun without --cached to refresh, or set CODEX_RATE_LIMITS_CACHE_ALLOW_STALE=true";

pub fn clear_prompt_segment_cache() -> Result<()> {
    let root = cache_root().context("cache root")?;
    if !root.is_absolute() {
        anyhow::bail!(
            "codex-rate-limits: refusing to clear cache with non-absolute cache root: {}",
            root.display()
        );
    }
    if root == Path::new("/") {
        anyhow::bail!(
            "codex-rate-limits: refusing to clear cache with invalid cache root: {}",
            root.display()
        );
    }

    let cache_dir = root.join("codex").join("prompt-segment-rate-limits");
    let cache_dir_str = cache_dir.to_string_lossy();
    if !cache_dir_str.ends_with("/codex/prompt-segment-rate-limits") {
        anyhow::bail!(
            "codex-rate-limits: refusing to clear unexpected cache dir: {}",
            cache_dir.display()
        );
    }

    if cache_dir.is_dir() {
        fs::remove_dir_all(&cache_dir).ok();
    }

    Ok(())
}

pub fn cache_file_for_target(target_file: &Path) -> Result<PathBuf> {
    let cache_dir = prompt_segment_cache_dir().context("cache dir")?;

    if let Some(secret_dir) = paths::resolve_secret_dir() {
        if target_file.starts_with(&secret_dir) {
            let display = secret_file_basename(target_file)?;
            let key = cache_key(&display)?;
            return Ok(cache_dir.join(format!("{key}.kv")));
        }

        if let Some(secret_name) = secret_name_for_auth(target_file, &secret_dir) {
            let key = cache_key(&secret_name)?;
            return Ok(cache_dir.join(format!("{key}.kv")));
        }
    }

    let hash = shared_fs::sha256_file(target_file)?;
    Ok(cache_dir.join(format!("auth_{}.kv", hash.to_lowercase())))
}

pub fn secret_name_for_target(target_file: &Path) -> Option<String> {
    let secret_dir = paths::resolve_secret_dir()?;
    if target_file.starts_with(&secret_dir) {
        return secret_file_basename(target_file).ok();
    }
    secret_name_for_auth(target_file, &secret_dir)
}

pub fn read_cache_entry(target_file: &Path) -> Result<CacheEntry> {
    let cache_file = cache_file_for_target(target_file)?;
    if !cache_file.is_file() {
        anyhow::bail!(
            "codex-rate-limits: cache not found (run codex-rate-limits without --cached, or codex-cli prompt-segment, to populate): {}",
            cache_file.display()
        );
    }

    let content = fs::read_to_string(&cache_file)
        .with_context(|| format!("failed to read cache: {}", cache_file.display()))?;
    let mut fetched_at_epoch: Option<i64> = None;
    let mut non_weekly_label: Option<String> = None;
    let mut non_weekly_remaining: Option<i64> = None;
    let mut non_weekly_reset_epoch: Option<i64> = None;
    let mut weekly_remaining: Option<i64> = None;
    let mut weekly_reset_epoch: Option<i64> = None;

    for line in content.lines() {
        if let Some(value) = line.strip_prefix("fetched_at=") {
            fetched_at_epoch = value.parse::<i64>().ok();
        } else if let Some(value) = line.strip_prefix("non_weekly_label=") {
            non_weekly_label = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("non_weekly_remaining=") {
            non_weekly_remaining = value.parse::<i64>().ok();
        } else if let Some(value) = line.strip_prefix("non_weekly_reset_epoch=") {
            non_weekly_reset_epoch = value.parse::<i64>().ok();
        } else if let Some(value) = line.strip_prefix("weekly_remaining=") {
            weekly_remaining = value.parse::<i64>().ok();
        } else if let Some(value) = line.strip_prefix("weekly_reset_epoch=") {
            weekly_reset_epoch = value.parse::<i64>().ok();
        }
    }

    if non_weekly_label.as_deref().is_some_and(str::is_empty)
        || non_weekly_label.is_some() != non_weekly_remaining.is_some()
        || weekly_remaining.is_some() != weekly_reset_epoch.is_some()
        || (non_weekly_remaining.is_none() && weekly_remaining.is_none())
    {
        anyhow::bail!(
            "codex-rate-limits: invalid cache (incomplete window data): {}",
            cache_file.display()
        );
    }

    Ok(CacheEntry {
        fetched_at_epoch,
        non_weekly_label,
        non_weekly_remaining,
        non_weekly_reset_epoch,
        weekly_remaining,
        weekly_reset_epoch,
    })
}

pub fn read_cache_entry_for_cached_mode(target_file: &Path) -> Result<CacheEntry> {
    let entry = read_cache_entry(target_file)?;
    ensure_cache_within_display_age(target_file, &entry)?;
    if cache_allow_stale() {
        return Ok(entry);
    }
    ensure_cache_fresh(target_file, &entry)?;
    Ok(entry)
}

/// A cached entry plus whether it is past the freshness TTL.
pub struct StaleCacheRead {
    pub entry: CacheEntry,
    pub stale: bool,
}

/// Reads a cached entry within the fixed display ceiling, reporting whether it
/// is past the freshness TTL.
///
/// Unlike [`read_cache_entry_for_cached_mode`] (which enforces the TTL for the
/// explicit `--cached` mode), this permits stale values only until the fixed
/// maximum display age so the diag command can degrade briefly without showing
/// indefinitely old quotas. Callers surface the `stale` flag to the user.
pub fn read_cache_entry_allow_stale(target_file: &Path) -> Result<StaleCacheRead> {
    let entry = read_cache_entry(target_file)?;
    ensure_cache_within_display_age(target_file, &entry)?;
    let stale = cache_entry_is_stale(&entry);
    Ok(StaleCacheRead { entry, stale })
}

pub fn cache_fetched_at_within_display_age(fetched_at_epoch: Option<i64>) -> bool {
    cache_fetched_at_within_display_age_at(fetched_at_epoch, chrono::Utc::now().timestamp())
}

fn cache_fetched_at_within_display_age_at(fetched_at_epoch: Option<i64>, now_epoch: i64) -> bool {
    let age_seconds = fetched_at_epoch
        .filter(|value| *value > 0 && now_epoch > 0)
        .map(|value| now_epoch.saturating_sub(value));
    usage_cache_policy::classify_display_age_seconds(age_seconds).is_display_eligible()
}

fn cache_entry_is_stale(entry: &CacheEntry) -> bool {
    let fetched_at = match entry.fetched_at_epoch {
        Some(value) if value > 0 => value,
        _ => return true,
    };
    let now_epoch = chrono::Utc::now().timestamp();
    if now_epoch <= 0 {
        return false;
    }
    let ttl_i64 = i64::try_from(cache_ttl_seconds()).unwrap_or(i64::MAX);
    now_epoch.saturating_sub(fetched_at) > ttl_i64
}

pub fn write_prompt_segment_cache(
    target_file: &Path,
    fetched_at_epoch: i64,
    values: &render::WeeklyValues,
) -> Result<()> {
    if values.weekly.is_none() && values.non_weekly.is_none() {
        anyhow::bail!("codex-rate-limits: refusing to write empty window cache");
    }
    let cache_file = cache_file_for_target(target_file)?;
    if let Some(parent) = cache_file.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut lines = Vec::new();
    lines.push(format!("fetched_at={fetched_at_epoch}"));
    if let Some(non_weekly) = &values.non_weekly {
        lines.push(format!("non_weekly_label={}", non_weekly.label));
        lines.push(format!("non_weekly_remaining={}", non_weekly.remaining));
        if non_weekly.reset_epoch > 0 {
            lines.push(format!("non_weekly_reset_epoch={}", non_weekly.reset_epoch));
        }
    }
    if let Some(weekly) = &values.weekly {
        lines.push(format!("weekly_remaining={}", weekly.remaining));
        lines.push(format!("weekly_reset_epoch={}", weekly.reset_epoch));
    }

    let data = lines.join("\n");
    shared_fs::write_atomic(&cache_file, data.as_bytes(), shared_fs::SECRET_FILE_MODE)?;
    Ok(())
}

fn prompt_segment_cache_dir() -> Result<PathBuf> {
    let root = cache_root().context("cache root")?;
    Ok(root.join("codex").join("prompt-segment-rate-limits"))
}

fn ensure_cache_fresh(target_file: &Path, entry: &CacheEntry) -> Result<()> {
    let ttl_seconds = cache_ttl_seconds();
    let ttl_i64 = i64::try_from(ttl_seconds).unwrap_or(i64::MAX);
    let cache_file = cache_file_for_target(target_file)?;

    let fetched_at_epoch = match entry.fetched_at_epoch {
        Some(value) if value > 0 => value,
        _ => {
            anyhow::bail!(
                "codex-rate-limits: cache expired (missing fetched_at): {} ({})",
                cache_file.display(),
                CACHE_MISS_HINT
            );
        }
    };

    let now_epoch = chrono::Utc::now().timestamp();
    if now_epoch <= 0 {
        return Ok(());
    }

    let age_seconds = if now_epoch >= fetched_at_epoch {
        now_epoch - fetched_at_epoch
    } else {
        0
    };
    if age_seconds > ttl_i64 {
        anyhow::bail!(
            "codex-rate-limits: cache expired (age={}s, ttl={}s): {} ({})",
            age_seconds,
            ttl_seconds,
            cache_file.display(),
            CACHE_MISS_HINT
        );
    }

    Ok(())
}

fn ensure_cache_within_display_age(target_file: &Path, entry: &CacheEntry) -> Result<()> {
    if cache_fetched_at_within_display_age(entry.fetched_at_epoch) {
        return Ok(());
    }
    let cache_file = cache_file_for_target(target_file)?;
    anyhow::bail!(
        "codex-rate-limits: cache exceeds maximum display age (max={}s): {}",
        usage_cache_policy::MAX_DISPLAY_AGE_SECONDS,
        cache_file.display()
    )
}

fn cache_ttl_seconds() -> u64 {
    if let Ok(raw) = std::env::var("CODEX_RATE_LIMITS_CACHE_TTL")
        && let Some(value) = shared_env::parse_duration_seconds(&raw)
    {
        return value;
    }
    DEFAULT_CACHE_TTL_SECONDS
}

fn cache_allow_stale() -> bool {
    shared_env::env_truthy_or("CODEX_RATE_LIMITS_CACHE_ALLOW_STALE", false)
}

fn cache_root() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ZSH_CACHE_DIR")
        && !path.is_empty()
    {
        return Some(PathBuf::from(path));
    }
    let zdotdir = paths::resolve_zdotdir()?;
    Some(zdotdir.join("cache"))
}

fn secret_name_for_auth(auth_file: &Path, secret_dir: &Path) -> Option<String> {
    let auth_key = auth::identity_key_from_auth_file(auth_file)
        .ok()
        .flatten()?;
    let entries = std::fs::read_dir(secret_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let candidate_key = match auth::identity_key_from_auth_file(&path).ok().flatten() {
            Some(value) => value,
            None => continue,
        };
        if candidate_key == auth_key {
            return secret_file_basename(&path).ok();
        }
    }
    None
}

fn secret_file_basename(path: &Path) -> Result<String> {
    let file = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let base = file.trim_end_matches(".json");
    Ok(base.to_string())
}

fn cache_key(name: &str) -> Result<String> {
    if name.is_empty() {
        anyhow::bail!("missing cache key name");
    }
    let mut key = String::new();
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            key.push(ch);
        } else {
            key.push('_');
        }
    }
    while key.starts_with('_') {
        key.remove(0);
    }
    while key.ends_with('_') {
        key.pop();
    }
    if key.is_empty() {
        anyhow::bail!("invalid cache key name");
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::{
        cache_fetched_at_within_display_age_at, cache_file_for_target, clear_prompt_segment_cache,
        read_cache_entry, read_cache_entry_allow_stale, read_cache_entry_for_cached_mode,
        secret_name_for_target, write_prompt_segment_cache,
    };
    use crate::rate_limits::render;
    use chrono::Utc;
    use nils_common::fs as shared_fs;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use std::fs;
    use std::path::Path;

    const HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
    const PAYLOAD_ALPHA: &str = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";

    fn token(payload: &str) -> String {
        format!("{HEADER}.{payload}.sig")
    }

    fn auth_json(
        payload: &str,
        account_id: &str,
        refresh_token: &str,
        last_refresh: &str,
    ) -> String {
        format!(
            r#"{{"tokens":{{"access_token":"{}","id_token":"{}","refresh_token":"{}","account_id":"{}"}},"last_refresh":"{}"}}"#,
            token(payload),
            token(payload),
            refresh_token,
            account_id,
            last_refresh
        )
    }

    fn set_cache_env(
        lock: &GlobalStateLock,
        secret_dir: &Path,
        cache_root: &Path,
    ) -> (EnvGuard, EnvGuard) {
        let secret = EnvGuard::set(
            lock,
            "CODEX_SECRET_DIR",
            secret_dir.to_str().expect("secret dir path"),
        );
        let cache = EnvGuard::set(
            lock,
            "ZSH_CACHE_DIR",
            cache_root.to_str().expect("cache root path"),
        );
        (secret, cache)
    }

    fn complete_weekly_values(
        non_weekly_label: &str,
        non_weekly_remaining: i64,
        weekly_remaining: i64,
        weekly_reset_epoch: i64,
        non_weekly_reset_epoch: Option<i64>,
    ) -> render::WeeklyValues {
        render::WeeklyValues {
            weekly: Some(render::WindowValues {
                label: "Weekly".to_string(),
                remaining: weekly_remaining,
                reset_epoch: weekly_reset_epoch,
            }),
            non_weekly: Some(render::WindowValues {
                label: non_weekly_label.to_string(),
                remaining: non_weekly_remaining,
                reset_epoch: non_weekly_reset_epoch.unwrap_or_default(),
            }),
        }
    }

    #[test]
    fn clear_prompt_segment_cache_rejects_relative_cache_root() {
        let lock = GlobalStateLock::new();
        let _cache = EnvGuard::set(&lock, "ZSH_CACHE_DIR", "relative/cache");

        let err = clear_prompt_segment_cache().expect_err("relative cache root should fail");
        assert!(err.to_string().contains("non-absolute cache root"));
    }

    #[test]
    fn clear_prompt_segment_cache_rejects_root_cache_path() {
        let lock = GlobalStateLock::new();
        let _cache = EnvGuard::set(&lock, "ZSH_CACHE_DIR", "/");

        let err = clear_prompt_segment_cache().expect_err("root cache path should fail");
        assert!(err.to_string().contains("invalid cache root"));
    }

    #[test]
    fn clear_prompt_segment_cache_removes_only_prompt_segment_cache_dir() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let cache_root = dir.path().join("cache-root");
        let remove_dir = cache_root.join("codex").join("prompt-segment-rate-limits");
        let keep_dir = cache_root.join("codex").join("secrets");
        fs::create_dir_all(&remove_dir).expect("remove dir");
        fs::create_dir_all(&keep_dir).expect("keep dir");
        fs::write(
            remove_dir.join("alpha.kv"),
            "weekly_remaining=1\nweekly_reset_epoch=2",
        )
        .expect("write cached file");
        fs::write(keep_dir.join("keep.txt"), "keep").expect("write keep file");
        let _cache = EnvGuard::set(
            &lock,
            "ZSH_CACHE_DIR",
            cache_root.to_str().expect("cache root path"),
        );

        clear_prompt_segment_cache().expect("clear cache");

        assert!(
            !remove_dir.exists(),
            "prompt-segment cache dir should be removed"
        );
        assert!(keep_dir.is_dir(), "non-target cache dir should remain");
    }

    #[test]
    fn cache_file_for_secret_target_uses_sanitized_secret_name() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _env = set_cache_env(&lock, &secret_dir, &cache_root);

        let target = secret_dir.join("My.Secret+Name.json");
        fs::write(&target, "{}").expect("write secret file");

        let cache_file = cache_file_for_target(&target).expect("cache file");
        assert_eq!(
            cache_file,
            cache_root
                .join("codex")
                .join("prompt-segment-rate-limits")
                .join("my_secret_name.kv")
        );
    }

    #[test]
    fn cache_file_for_non_secret_target_falls_back_to_hashed_key() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _env = set_cache_env(&lock, &secret_dir, &cache_root);

        let target = dir.path().join("auth.json");
        fs::write(&target, "{\"tokens\":{\"access_token\":\"tok\"}}").expect("write auth file");

        let hash = shared_fs::sha256_file(&target).expect("sha256");
        let cache_file = cache_file_for_target(&target).expect("cache file");
        assert_eq!(
            cache_file,
            cache_root
                .join("codex")
                .join("prompt-segment-rate-limits")
                .join(format!("auth_{hash}.kv"))
        );
    }

    #[test]
    fn cache_file_for_auth_target_reuses_matching_secret_identity() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _env = set_cache_env(&lock, &secret_dir, &cache_root);

        let target = dir.path().join("auth.json");
        let target_content = auth_json(
            PAYLOAD_ALPHA,
            "acct_001",
            "refresh_auth",
            "2025-01-20T12:34:56Z",
        );
        fs::write(&target, target_content).expect("write auth file");

        let secret_file = secret_dir.join("Alpha Team.json");
        let secret_content = auth_json(
            PAYLOAD_ALPHA,
            "acct_001",
            "refresh_secret",
            "2025-01-21T12:34:56Z",
        );
        fs::write(&secret_file, secret_content).expect("write matching secret file");

        let cache_file = cache_file_for_target(&target).expect("cache file");
        assert_eq!(
            cache_file.file_name().and_then(|name| name.to_str()),
            Some("alpha_team.kv")
        );
        assert_eq!(
            secret_name_for_target(&target),
            Some("Alpha Team".to_string())
        );
    }

    #[test]
    fn write_then_read_cache_entry_preserves_optional_non_weekly_reset_epoch() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _env = set_cache_env(&lock, &secret_dir, &cache_root);

        let target = secret_dir.join("alpha.json");
        fs::write(&target, "{}").expect("write target");

        let values = complete_weekly_values("5h", 91, 12, 1700600000, Some(1700003600));
        write_prompt_segment_cache(&target, 1700000000, &values).expect("write cache");

        let entry = read_cache_entry(&target).expect("read cache");
        assert_eq!(entry.non_weekly_label.as_deref(), Some("5h"));
        assert_eq!(entry.non_weekly_remaining, Some(91));
        assert_eq!(entry.non_weekly_reset_epoch, Some(1700003600));
        assert_eq!(entry.weekly_remaining, Some(12));
        assert_eq!(entry.weekly_reset_epoch, Some(1700600000));
    }

    #[test]
    fn write_cache_rejects_empty_windows_without_replacing_existing_data() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _env = set_cache_env(&lock, &secret_dir, &cache_root);

        let target = secret_dir.join("alpha.json");
        fs::write(&target, "{}").expect("write target");
        let values = complete_weekly_values("5h", 91, 12, 1_700_600_000, None);
        write_prompt_segment_cache(&target, 1, &values).expect("seed cache");
        let cache_file = cache_file_for_target(&target).expect("cache path");
        let before = fs::read_to_string(&cache_file).expect("read cache");

        let empty = render::WeeklyValues {
            weekly: None,
            non_weekly: None,
        };
        let err = write_prompt_segment_cache(&target, 2, &empty)
            .expect_err("empty windows must not replace cache");

        assert!(err.to_string().contains("refusing to write empty"));
        assert_eq!(fs::read_to_string(cache_file).expect("read cache"), before);
    }

    #[test]
    fn write_cache_omits_optional_non_weekly_reset_epoch_when_absent() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _env = set_cache_env(&lock, &secret_dir, &cache_root);

        let target = secret_dir.join("alpha.json");
        fs::write(&target, "{}").expect("write target");

        let values = complete_weekly_values("daily", 45, 9, 1700600000, None);
        write_prompt_segment_cache(&target, 1700000000, &values).expect("write cache");

        let cache_file = cache_file_for_target(&target).expect("cache path");
        let content = fs::read_to_string(&cache_file).expect("read cache file");
        assert!(!content.contains("non_weekly_reset_epoch="));

        let entry = read_cache_entry(&target).expect("read cache");
        assert_eq!(entry.non_weekly_label.as_deref(), Some("daily"));
        assert_eq!(entry.non_weekly_reset_epoch, None);
    }

    #[test]
    fn read_cache_entry_reports_incomplete_weekly_data() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _env = set_cache_env(&lock, &secret_dir, &cache_root);

        let target = secret_dir.join("alpha.json");
        fs::write(&target, "{}").expect("write target");
        let cache_file = cache_file_for_target(&target).expect("cache path");
        fs::create_dir_all(cache_file.parent().expect("cache parent")).expect("cache parent dir");
        fs::write(
            &cache_file,
            "fetched_at=1\nnon_weekly_label=5h\nnon_weekly_remaining=90\nweekly_remaining=1\n",
        )
        .expect("write invalid cache");

        let err = read_cache_entry(&target).expect_err("missing weekly reset should fail");
        assert!(err.to_string().contains("incomplete window data"));
    }

    #[test]
    fn read_cache_entry_accepts_missing_non_weekly_data() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _env = set_cache_env(&lock, &secret_dir, &cache_root);

        let target = secret_dir.join("alpha.json");
        fs::write(&target, "{}").expect("write target");
        let cache_file = cache_file_for_target(&target).expect("cache path");
        fs::create_dir_all(cache_file.parent().expect("cache parent")).expect("cache parent dir");
        fs::write(
            &cache_file,
            "fetched_at=1\nweekly_remaining=1\nweekly_reset_epoch=1700600000\n",
        )
        .expect("write invalid cache");

        let entry = read_cache_entry(&target).expect("weekly-only cache should load");
        assert_eq!(entry.non_weekly_label, None);
        assert_eq!(entry.weekly_remaining, Some(1));
    }

    #[test]
    fn read_cache_entry_for_cached_mode_rejects_expired_cache_by_default() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _env = set_cache_env(&lock, &secret_dir, &cache_root);

        let target = secret_dir.join("alpha.json");
        fs::write(&target, "{}").expect("write target");
        let values = complete_weekly_values("5h", 91, 12, 1_700_600_000, Some(1_700_003_600));
        let fetched_at = Utc::now().timestamp().saturating_sub(300);
        write_prompt_segment_cache(&target, fetched_at, &values).expect("write cache");

        let err = read_cache_entry_for_cached_mode(&target).expect_err("stale cache should fail");
        assert!(err.to_string().contains("cache expired"));
    }

    #[test]
    fn read_cache_entry_for_cached_mode_honors_ttl_env() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _env = set_cache_env(&lock, &secret_dir, &cache_root);
        let _ttl = EnvGuard::set(&lock, "CODEX_RATE_LIMITS_CACHE_TTL", "1h");

        let target = secret_dir.join("alpha.json");
        fs::write(&target, "{}").expect("write target");
        let now = Utc::now().timestamp();
        let fetched_at = now.saturating_sub(5 * 60);
        let values = complete_weekly_values("5h", 91, 12, 1_700_600_000, Some(1_700_003_600));
        write_prompt_segment_cache(&target, fetched_at, &values).expect("write cache");

        let entry = read_cache_entry_for_cached_mode(&target).expect("fresh cache");
        assert_eq!(entry.non_weekly_label.as_deref(), Some("5h"));
    }

    #[test]
    fn read_cache_entry_for_cached_mode_allows_stale_when_enabled() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _env = set_cache_env(&lock, &secret_dir, &cache_root);
        let _allow_stale = EnvGuard::set(&lock, "CODEX_RATE_LIMITS_CACHE_ALLOW_STALE", "true");

        let target = secret_dir.join("alpha.json");
        fs::write(&target, "{}").expect("write target");
        let values = complete_weekly_values("5h", 91, 12, 1_700_600_000, Some(1_700_003_600));
        let fetched_at = Utc::now().timestamp().saturating_sub(300);
        write_prompt_segment_cache(&target, fetched_at, &values).expect("write cache");

        let entry = read_cache_entry_for_cached_mode(&target).expect("allow stale");
        assert_eq!(entry.non_weekly_remaining, Some(91));
    }

    #[test]
    fn cache_display_age_boundary_is_600_seconds() {
        assert!(cache_fetched_at_within_display_age_at(Some(1_000), 1_599));
        assert!(!cache_fetched_at_within_display_age_at(Some(1_000), 1_600));
    }

    #[test]
    fn cache_display_future_clock_tolerance_ends_after_5_seconds() {
        assert!(cache_fetched_at_within_display_age_at(Some(1_005), 1_000));
        assert!(!cache_fetched_at_within_display_age_at(Some(1_006), 1_000));
    }

    #[test]
    fn cache_display_rejects_missing_or_invalid_timestamps() {
        assert!(!cache_fetched_at_within_display_age_at(None, 1_000));
        assert!(!cache_fetched_at_within_display_age_at(Some(0), 1_000));
        assert!(!cache_fetched_at_within_display_age_at(Some(1_000), 0));
    }

    #[test]
    fn cached_mode_allow_stale_does_not_bypass_max_display_age() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _env = set_cache_env(&lock, &secret_dir, &cache_root);
        let _allow_stale = EnvGuard::set(&lock, "CODEX_RATE_LIMITS_CACHE_ALLOW_STALE", "true");

        let target = secret_dir.join("alpha.json");
        fs::write(&target, "{}").expect("write target");
        let values = complete_weekly_values("5h", 91, 12, 1_700_600_000, Some(1_700_003_600));
        let fetched_at = Utc::now().timestamp().saturating_sub(600);
        write_prompt_segment_cache(&target, fetched_at, &values).expect("write cache");

        let error = read_cache_entry_for_cached_mode(&target)
            .expect_err("allow-stale must not bypass max display age");
        assert!(error.to_string().contains("maximum display age"));
    }

    #[test]
    fn stale_fallback_rejects_cache_older_than_max_display_age() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _env = set_cache_env(&lock, &secret_dir, &cache_root);

        let target = secret_dir.join("alpha.json");
        fs::write(&target, "{}").expect("write target");
        let values = complete_weekly_values("5h", 91, 12, 1_700_600_000, Some(1_700_003_600));
        let fetched_at = Utc::now().timestamp().saturating_sub(601);
        write_prompt_segment_cache(&target, fetched_at, &values).expect("write cache");

        let error = match read_cache_entry_allow_stale(&target) {
            Ok(_) => panic!("cache older than max display age must be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("maximum display age"));
        assert!(
            cache_file_for_target(&target)
                .expect("cache path")
                .is_file()
        );
    }
}

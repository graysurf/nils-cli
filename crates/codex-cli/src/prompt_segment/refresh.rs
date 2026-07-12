use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Utc;
use nils_common::env as shared_env;

use crate::provider_profile::CODEX_PROVIDER_PROFILE;
use crate::rate_limits::cache;
use crate::rate_limits::client::{UsageRequest, fetch_usage};
use crate::rate_limits::render;

use super::lock;
use super::render as prompt_segment_render;

pub fn enqueue_background_refresh(target_file: &Path) {
    let cache_file = match cache::cache_file_for_target(target_file) {
        Ok(value) => value,
        Err(_) => return,
    };
    let lock_dir = match lock::lock_dir_for_cache_file(&cache_file) {
        Some(value) => value,
        None => return,
    };

    let refresh_min_seconds = env_u64("CODEX_PROMPT_SEGMENT_REFRESH_MIN_SECONDS", 30);
    if refresh_min_seconds > 0 && is_within_min_interval(&cache_file, refresh_min_seconds) {
        return;
    }

    let lock_stale_seconds = env_u64("CODEX_PROMPT_SEGMENT_LOCK_STALE_SECONDS", 90);
    if lock_dir.exists() && !lock::is_stale(&lock_dir, lock_stale_seconds) {
        return;
    }

    write_last_attempt(&cache_file);

    let exe = match std::env::current_exe() {
        Ok(value) => value,
        Err(_) => return,
    };

    let mut cmd = Command::new(exe);
    cmd.arg("prompt-segment").arg("--refresh");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    detach_background_refresh(&mut cmd);

    let _ = cmd.spawn();
}

#[cfg(unix)]
fn detach_background_refresh(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;

    cmd.process_group(0);
}

#[cfg(not(unix))]
fn detach_background_refresh(_cmd: &mut Command) {}

pub fn refresh_blocking(target_file: &Path) -> Option<prompt_segment_render::CacheEntry> {
    let cache_file = cache::cache_file_for_target(target_file).ok()?;
    let lock_dir = lock::lock_dir_for_cache_file(&cache_file)?;
    let lock_stale_seconds = env_u64("CODEX_PROMPT_SEGMENT_LOCK_STALE_SECONDS", 90);

    cleanup_usage_files(&cache_file);

    let _lock = lock::RefreshLock::acquire(&lock_dir, lock_stale_seconds)?;

    let entry = fetch_and_write_cache(target_file).ok()?;
    write_last_attempt(&cache_file);
    Some(entry)
}

fn fetch_and_write_cache(target_file: &Path) -> anyhow::Result<prompt_segment_render::CacheEntry> {
    let base_url = std::env::var("CODEX_CHATGPT_BASE_URL")
        .unwrap_or_else(|_| "https://chatgpt.com/backend-api/".to_string());
    let connect_timeout = env_u64("CODEX_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS", 2);
    let max_time = env_u64("CODEX_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS", 8);

    let usage_request = UsageRequest {
        target_file: target_file.to_path_buf(),
        refresh_on_401: shared_env::env_truthy(CODEX_PROVIDER_PROFILE.env.auto_refresh_enabled),
        suppress_auth_refresh_output: true,
        base_url,
        connect_timeout_seconds: connect_timeout,
        max_time_seconds: max_time,
    };

    let usage = fetch_usage(&usage_request)?;
    let usage_data =
        render::parse_usage(&usage.json).ok_or_else(|| anyhow::anyhow!("invalid usage payload"))?;
    let values = render::render_values(&usage_data);
    let weekly = render::weekly_values(&values);
    if weekly.weekly.is_none() && weekly.non_weekly.is_none() {
        anyhow::bail!("no active rate-limit window");
    }

    let fetched_at_epoch = Utc::now().timestamp();
    if fetched_at_epoch > 0 {
        let _ = cache::write_prompt_segment_cache(target_file, fetched_at_epoch, &weekly);
    }

    Ok(prompt_segment_render::CacheEntry {
        fetched_at_epoch,
        non_weekly_label: weekly
            .non_weekly
            .as_ref()
            .map(|window| window.label.clone()),
        non_weekly_remaining: weekly.non_weekly.as_ref().map(|window| window.remaining),
        non_weekly_reset_epoch: weekly
            .non_weekly
            .as_ref()
            .map(|window| window.reset_epoch)
            .filter(|epoch| *epoch > 0),
        weekly_remaining: weekly.weekly.as_ref().map(|window| window.remaining),
        weekly_reset_epoch: weekly.weekly.as_ref().map(|window| window.reset_epoch),
    })
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(default)
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
    let now_epoch = now_epoch();
    if now_epoch <= 0 {
        return;
    }

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, now_epoch.to_string());
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
    let now = now_epoch();
    if now <= 0 {
        return false;
    }
    (now - last) >= 0 && (now - last) < refresh_min_seconds as i64
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

fn cleanup_usage_files(cache_file: &Path) {
    let cache_dir = match cache_file.parent() {
        Some(value) => value,
        None => return,
    };
    let entries = match std::fs::read_dir(cache_dir) {
        Ok(value) => value,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with("wham.usage.") {
            let _ = std::fs::remove_file(path);
        }
    }
}

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use crate::auth;
use crate::diag_output;
use crate::provider_profile::CODEX_PROVIDER_PROFILE;
use crate::rate_limits::client::{UsageRequest, fetch_usage};
use nils_common::env as shared_env;
use nils_common::fs;
use nils_common::provider_runtime::persistence::{
    SyncSecretsError, TimestampPolicy, sync_auth_to_matching_secrets,
};
use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};

pub use nils_common::rate_limits_ansi as ansi;
pub mod cache;
pub mod client;
pub mod render;
pub mod writeback;

#[derive(Clone, Debug)]
pub struct RateLimitsOptions {
    pub clear_cache: bool,
    pub debug: bool,
    pub cached: bool,
    pub no_refresh_auth: bool,
    pub json: bool,
    pub one_line: bool,
    pub all: bool,
    pub async_mode: bool,
    pub watch: bool,
    pub jobs: Option<String>,
    pub secret: Option<String>,
}

const DIAG_SCHEMA_VERSION: &str = "codex-cli.diag.rate-limits.v1";
const DIAG_COMMAND: &str = "diag rate-limits";
const WATCH_INTERVAL_SECONDS: u64 = 60;
const ANSI_CLEAR_SCREEN_AND_HOME: &str = "\x1b[2J\x1b[H";

fn refresh_on_401_enabled(no_refresh_auth: bool) -> bool {
    !no_refresh_auth && shared_env::env_truthy(CODEX_PROVIDER_PROFILE.env.auto_refresh_enabled)
}

#[derive(Debug, Clone, Serialize)]
struct RateLimitSummary {
    non_weekly_label: String,
    non_weekly_remaining: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    non_weekly_reset_epoch: Option<i64>,
    weekly_remaining: i64,
    weekly_reset_epoch: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    weekly_reset_local: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RateLimitWindow {
    label: String,
    used_percent: i64,
    remaining_percent: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reset_at_epoch: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
struct RateLimitJsonResult {
    provider: String,
    name: String,
    target_file: String,
    status: String,
    ok: bool,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<RateLimitSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    windows: Option<Vec<RateLimitWindow>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_usage: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<diag_output::ErrorEnvelope>,
}

#[derive(Debug, Clone, Serialize)]
struct RateLimitSingleEnvelope {
    schema_version: String,
    command: String,
    mode: String,
    ok: bool,
    result: RateLimitJsonResult,
}

#[derive(Debug, Clone, Serialize)]
struct RateLimitCollectionEnvelope {
    schema_version: String,
    command: String,
    mode: String,
    ok: bool,
    results: Vec<RateLimitJsonResult>,
}

pub fn run(args: &RateLimitsOptions) -> Result<i32> {
    let cached_mode = args.cached;
    let mut one_line = args.one_line;
    let mut all_mode = args.all;
    let output_json = args.json;

    let mut debug_mode = args.debug;
    if !debug_mode
        && let Ok(raw) = std::env::var("ZSH_DEBUG")
        && raw.parse::<i64>().unwrap_or(0) >= 2
    {
        debug_mode = true;
    }

    if args.async_mode {
        if !args.cached {
            maybe_sync_all_mode_auth_silent(debug_mode);
        }
        if args.json {
            return run_async_json_mode(args, debug_mode);
        }
        if args.watch {
            return run_async_watch_mode(args, debug_mode);
        }
        return run_async_mode(args, debug_mode);
    }

    if cached_mode {
        one_line = true;
        if output_json {
            diag_output::emit_error(
                DIAG_SCHEMA_VERSION,
                DIAG_COMMAND,
                "invalid-flag-combination",
                "codex-rate-limits: --json is not supported with --cached",
                Some(serde_json::json!({
                    "flags": ["--json", "--cached"],
                })),
            )?;
            return Ok(64);
        }
        if args.clear_cache {
            eprintln!("codex-rate-limits: -c is not compatible with --cached");
            return Ok(64);
        }
    }

    if output_json && one_line {
        diag_output::emit_error(
            DIAG_SCHEMA_VERSION,
            DIAG_COMMAND,
            "invalid-flag-combination",
            "codex-rate-limits: --one-line is not compatible with --json",
            Some(serde_json::json!({
                "flags": ["--one-line", "--json"],
            })),
        )?;
        return Ok(64);
    }

    if args.clear_cache
        && let Err(err) = cache::clear_prompt_segment_cache()
    {
        if output_json {
            diag_output::emit_error(
                DIAG_SCHEMA_VERSION,
                DIAG_COMMAND,
                "cache-clear-failed",
                err.to_string(),
                None,
            )?;
        } else {
            eprintln!("{err}");
        }
        return Ok(1);
    }

    if !all_mode
        && !output_json
        && !cached_mode
        && args.secret.is_none()
        && shared_env::env_truthy("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED")
    {
        all_mode = true;
    }

    if all_mode {
        if !cached_mode {
            maybe_sync_all_mode_auth_silent(debug_mode);
        }
        if args.secret.is_some() {
            eprintln!(
                "codex-rate-limits: usage: codex-rate-limits [-c] [-d] [--cached] [--no-refresh-auth] [--json] [--one-line] [--all] [secret.json]"
            );
            return Ok(64);
        }
        if output_json {
            return run_all_json_mode(args, cached_mode, debug_mode);
        }
        return run_all_mode(args, cached_mode, debug_mode);
    }

    run_single_mode(args, cached_mode, one_line, output_json)
}

fn run_async_json_mode(args: &RateLimitsOptions, _debug_mode: bool) -> Result<i32> {
    if args.one_line {
        let message = "codex-rate-limits: --async does not support --one-line";
        diag_output::emit_error(
            DIAG_SCHEMA_VERSION,
            DIAG_COMMAND,
            "invalid-flag-combination",
            message,
            Some(serde_json::json!({
                "flag": "--one-line",
                "mode": "async",
            })),
        )?;
        return Ok(64);
    }
    if let Some(secret) = args.secret.as_deref() {
        let message = format!(
            "codex-rate-limits: --async does not accept positional args: {}",
            secret
        );
        diag_output::emit_error(
            DIAG_SCHEMA_VERSION,
            DIAG_COMMAND,
            "invalid-positional-arg",
            message,
            Some(serde_json::json!({
                "secret": secret,
                "mode": "async",
            })),
        )?;
        return Ok(64);
    }
    if args.clear_cache && args.cached {
        let message = "codex-rate-limits: --async: -c is not compatible with --cached";
        diag_output::emit_error(
            DIAG_SCHEMA_VERSION,
            DIAG_COMMAND,
            "invalid-flag-combination",
            message,
            Some(serde_json::json!({
                "flags": ["--async", "--cached", "-c"],
            })),
        )?;
        return Ok(64);
    }
    if args.clear_cache
        && let Err(err) = cache::clear_prompt_segment_cache()
    {
        diag_output::emit_error(
            DIAG_SCHEMA_VERSION,
            DIAG_COMMAND,
            "cache-clear-failed",
            err.to_string(),
            None,
        )?;
        return Ok(1);
    }

    let secret_files = match collect_secret_files() {
        Ok(value) => value,
        Err((code, message, details)) => {
            diag_output::emit_error(
                DIAG_SCHEMA_VERSION,
                DIAG_COMMAND,
                "secret-discovery-failed",
                message,
                details,
            )?;
            return Ok(code);
        }
    };

    let jobs = resolve_async_jobs(args.jobs.as_deref());
    let cached_mode = args.cached;
    let no_refresh_auth = args.no_refresh_auth;
    let mut results_by_secret = collect_async_items(&secret_files, jobs, None, move |path, _| {
        collect_json_result_for_secret(&path, cached_mode, no_refresh_auth, true)
    });
    let mut results = Vec::new();
    let mut rc = 0;
    for secret_file in &secret_files {
        let secret_name = secret_file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();
        let result = results_by_secret.remove(&secret_name).unwrap_or_else(|| {
            json_result_error(
                secret_file,
                "network",
                "request-failed",
                format!(
                    "codex-rate-limits: async worker did not return a result for {}",
                    secret_file.display()
                ),
                None,
            )
        });
        if !args.cached && !result.ok {
            rc = 1;
        }
        results.push(result);
    }
    results.sort_by(|a, b| a.name.cmp(&b.name));
    emit_collection_envelope("async", rc == 0, results)?;
    Ok(rc)
}

fn run_all_json_mode(
    args: &RateLimitsOptions,
    cached_mode: bool,
    _debug_mode: bool,
) -> Result<i32> {
    let secret_files = match collect_secret_files() {
        Ok(value) => value,
        Err((code, message, details)) => {
            diag_output::emit_error(
                DIAG_SCHEMA_VERSION,
                DIAG_COMMAND,
                "secret-discovery-failed",
                message,
                details,
            )?;
            return Ok(code);
        }
    };

    let mut results = Vec::new();
    let mut rc = 0;
    for secret_file in &secret_files {
        let result =
            collect_json_result_for_secret(secret_file, cached_mode, args.no_refresh_auth, false);
        if !cached_mode && !result.ok {
            rc = 1;
        }
        results.push(result);
    }
    results.sort_by(|a, b| a.name.cmp(&b.name));
    emit_collection_envelope("all", rc == 0, results)?;
    Ok(rc)
}

fn emit_collection_envelope(mode: &str, ok: bool, results: Vec<RateLimitJsonResult>) -> Result<()> {
    diag_output::emit_json(&RateLimitCollectionEnvelope {
        schema_version: DIAG_SCHEMA_VERSION.to_string(),
        command: DIAG_COMMAND.to_string(),
        mode: mode.to_string(),
        ok,
        results,
    })
}

fn collect_secret_files() -> std::result::Result<Vec<PathBuf>, (i32, String, Option<Value>)> {
    if std::env::var_os(CODEX_PROVIDER_PROFILE.env.secret_dir).is_some() {
        let secret_dir = crate::paths::resolve_secret_dir().unwrap_or_default();
        return collect_json_secret_files_from_dir(&secret_dir, true);
    }

    let secret_dir = crate::paths::resolve_secret_dir().unwrap_or_default();
    if secret_dir.is_dir()
        && let Ok(secret_files) = collect_json_secret_files_from_dir(&secret_dir, false)
        && !secret_files.is_empty()
    {
        return Ok(secret_files);
    }

    if let Some(auth_file) = existing_active_auth_file() {
        return Ok(vec![auth_file]);
    }

    if let Some(auth_file) = official_codex_auth_file() {
        return Ok(vec![auth_file]);
    }

    no_secret_files_error(&secret_dir)
}

fn collect_json_secret_files_from_dir(
    secret_dir: &Path,
    strict: bool,
) -> std::result::Result<Vec<PathBuf>, (i32, String, Option<Value>)> {
    if !secret_dir.is_dir() {
        return Err((
            1,
            format!(
                "codex-rate-limits: CODEX_SECRET_DIR not found: {}",
                secret_dir.display()
            ),
            Some(serde_json::json!({
                "secret_dir": secret_dir.display().to_string(),
            })),
        ));
    }

    let mut secret_files: Vec<PathBuf> = std::fs::read_dir(secret_dir)
        .map_err(|err| {
            (
                1,
                format!("codex-rate-limits: failed to read CODEX_SECRET_DIR: {err}"),
                Some(serde_json::json!({
                    "secret_dir": secret_dir.display().to_string(),
                })),
            )
        })?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();

    if strict && secret_files.is_empty() {
        return no_secret_files_error(secret_dir);
    }

    secret_files.sort();
    Ok(secret_files)
}

fn no_secret_files_error(
    secret_dir: &Path,
) -> std::result::Result<Vec<PathBuf>, (i32, String, Option<Value>)> {
    Err((
        1,
        format!(
            "codex-rate-limits: no secrets found in {}",
            secret_dir.display()
        ),
        Some(serde_json::json!({
            "secret_dir": secret_dir.display().to_string(),
        })),
    ))
}

fn existing_active_auth_file() -> Option<PathBuf> {
    crate::paths::resolve_auth_file().filter(|path| path.is_file())
}

fn official_codex_auth_file() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .map(|home| home.join("auth.json"))
        .filter(|path| path.is_file())
    {
        return Some(path);
    }

    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".codex").join("auth.json"))
        .filter(|path| path.is_file())
}

fn is_official_codex_auth_file(target_file: &Path) -> bool {
    official_codex_auth_file().as_deref() == Some(target_file)
}

fn should_writeback_usage(target_file: &Path) -> bool {
    !is_official_codex_auth_file(target_file)
}

fn collect_json_result_for_secret(
    target_file: &Path,
    cached_mode: bool,
    no_refresh_auth: bool,
    allow_cache_fallback: bool,
) -> RateLimitJsonResult {
    if cached_mode {
        return collect_json_from_cache(target_file, "cache", true);
    }

    let base_url = std::env::var("CODEX_CHATGPT_BASE_URL")
        .unwrap_or_else(|_| "https://chatgpt.com/backend-api/".to_string());
    let connect_timeout = env_timeout("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", 2);
    let max_time = env_timeout("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", 8);
    let usage_request = UsageRequest {
        target_file: target_file.to_path_buf(),
        refresh_on_401: refresh_on_401_enabled(no_refresh_auth),
        suppress_auth_refresh_output: false,
        base_url,
        connect_timeout_seconds: connect_timeout,
        max_time_seconds: max_time,
    };

    match fetch_usage(&usage_request) {
        Ok(usage) => {
            if should_writeback_usage(target_file)
                && let Err(err) = writeback::write_weekly(target_file, &usage.json)
            {
                return json_result_error(
                    target_file,
                    "network",
                    "writeback-failed",
                    err.to_string(),
                    None,
                );
            }
            if is_auth_file(target_file)
                && let Ok(sync_rc) = auth::sync::run_with_json(false)
                && sync_rc != 0
            {
                return json_result_error(
                    target_file,
                    "network",
                    "sync-failed",
                    "codex-rate-limits: failed to sync auth after usage fetch".to_string(),
                    None,
                );
            }
            match summary_and_windows_from_usage(&usage.json) {
                Some((summary, windows)) => {
                    let fetched_at_epoch = Utc::now().timestamp();
                    if fetched_at_epoch > 0 {
                        let _ = cache::write_prompt_segment_cache(
                            target_file,
                            fetched_at_epoch,
                            &summary.non_weekly_label,
                            summary.non_weekly_remaining,
                            summary.weekly_remaining,
                            summary.weekly_reset_epoch,
                            summary.non_weekly_reset_epoch,
                        );
                    }
                    RateLimitJsonResult {
                        provider: "codex".to_string(),
                        name: secret_display_name(target_file),
                        target_file: target_file_name(target_file),
                        status: "ok".to_string(),
                        ok: true,
                        source: "network".to_string(),
                        summary: Some(summary),
                        windows: Some(windows),
                        raw_usage: Some(redact_sensitive_json(&usage.json)),
                        error: None,
                    }
                }
                None if render::rate_limit_is_explicit_null(&usage.json) => {
                    // Benign: the backend reports no active window. Prefer the
                    // last-known cached values (stale allowed), else report the
                    // empty window as a success rather than an error.
                    if allow_cache_fallback {
                        let fallback =
                            collect_json_from_cache(target_file, "cache-fallback", false);
                        if fallback.ok {
                            return fallback;
                        }
                    }
                    json_result_no_window(target_file)
                }
                None => json_result_error(
                    target_file,
                    "network",
                    "invalid-usage-payload",
                    "codex-rate-limits: invalid usage payload".to_string(),
                    Some(serde_json::json!({
                        "raw_usage": redact_sensitive_json(&usage.json),
                    })),
                ),
            }
        }
        Err(err) => {
            if allow_cache_fallback {
                let fallback = collect_json_from_cache(target_file, "cache-fallback", false);
                if fallback.ok {
                    return fallback;
                }
            }
            let msg = err.to_string();
            let code = if msg.contains("missing access_token") {
                "missing-access-token"
            } else {
                "request-failed"
            };
            json_result_error(target_file, "network", code, msg, None)
        }
    }
}

fn collect_json_from_cache(
    target_file: &Path,
    source: &str,
    enforce_ttl: bool,
) -> RateLimitJsonResult {
    let cache_entry = if enforce_ttl {
        cache::read_cache_entry_for_cached_mode(target_file)
    } else {
        cache::read_cache_entry(target_file)
    };

    match cache_entry {
        Ok(entry) => RateLimitJsonResult {
            provider: "codex".to_string(),
            name: secret_display_name(target_file),
            target_file: target_file_name(target_file),
            status: "ok".to_string(),
            ok: true,
            source: source.to_string(),
            summary: Some(summary_from_cache(&entry)),
            windows: Some(windows_from_cache(&entry)),
            raw_usage: None,
            error: None,
        },
        Err(err) => json_result_error(
            target_file,
            source,
            "cache-read-failed",
            err.to_string(),
            None,
        ),
    }
}

fn json_result_error(
    target_file: &Path,
    source: &str,
    code: &str,
    message: String,
    details: Option<Value>,
) -> RateLimitJsonResult {
    RateLimitJsonResult {
        provider: "codex".to_string(),
        name: secret_display_name(target_file),
        target_file: target_file_name(target_file),
        status: "error".to_string(),
        ok: false,
        source: source.to_string(),
        summary: None,
        windows: None,
        raw_usage: None,
        error: Some(diag_output::ErrorEnvelope {
            code: code.to_string(),
            message,
            details,
        }),
    }
}

/// Benign "no active rate-limit window" result for a `rate_limit: null` payload
/// with no cache to fall back to. Reported as a success, not an error.
fn json_result_no_window(target_file: &Path) -> RateLimitJsonResult {
    RateLimitJsonResult {
        provider: "codex".to_string(),
        name: secret_display_name(target_file),
        target_file: target_file_name(target_file),
        status: "no-rate-limit-window".to_string(),
        ok: true,
        source: "network".to_string(),
        summary: None,
        windows: Some(Vec::new()),
        raw_usage: None,
        error: None,
    }
}

fn secret_display_name(target_file: &Path) -> String {
    cache::secret_name_for_target(target_file).unwrap_or_else(|| {
        target_file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .trim_end_matches(".json")
            .to_string()
    })
}

fn target_file_name(target_file: &Path) -> String {
    target_file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string()
}

fn summary_and_windows_from_usage(
    usage_json: &Value,
) -> Option<(RateLimitSummary, Vec<RateLimitWindow>)> {
    let usage_data = render::parse_usage(usage_json)?;
    let values = render::render_values(&usage_data);
    let weekly = render::weekly_values(&values);
    let summary = RateLimitSummary {
        non_weekly_label: weekly.non_weekly_label,
        non_weekly_remaining: weekly.non_weekly_remaining,
        non_weekly_reset_epoch: weekly.non_weekly_reset_epoch,
        weekly_remaining: weekly.weekly_remaining,
        weekly_reset_epoch: weekly.weekly_reset_epoch,
        weekly_reset_local: render::format_epoch_local_datetime_with_offset(
            weekly.weekly_reset_epoch,
        ),
    };
    let windows = windows_from_usage_values(&usage_data, &values);
    Some((summary, windows))
}

fn windows_from_usage_values(
    usage_data: &render::UsageData,
    values: &render::RenderValues,
) -> Vec<RateLimitWindow> {
    vec![
        RateLimitWindow {
            label: values.primary_label.clone(),
            used_percent: percent_i64(usage_data.primary.used_percent),
            remaining_percent: values.primary_remaining,
            reset_at_epoch: Some(values.primary_reset_epoch).filter(|epoch| *epoch > 0),
        },
        RateLimitWindow {
            label: values.secondary_label.clone(),
            used_percent: percent_i64(usage_data.secondary.used_percent),
            remaining_percent: values.secondary_remaining,
            reset_at_epoch: Some(values.secondary_reset_epoch).filter(|epoch| *epoch > 0),
        },
    ]
}

fn summary_from_cache(entry: &cache::CacheEntry) -> RateLimitSummary {
    RateLimitSummary {
        non_weekly_label: entry.non_weekly_label.clone(),
        non_weekly_remaining: entry.non_weekly_remaining,
        non_weekly_reset_epoch: entry.non_weekly_reset_epoch,
        weekly_remaining: entry.weekly_remaining,
        weekly_reset_epoch: entry.weekly_reset_epoch,
        weekly_reset_local: render::format_epoch_local_datetime_with_offset(
            entry.weekly_reset_epoch,
        ),
    }
}

fn windows_from_cache(entry: &cache::CacheEntry) -> Vec<RateLimitWindow> {
    vec![
        RateLimitWindow {
            label: entry.non_weekly_label.clone(),
            used_percent: remaining_to_used_percent(entry.non_weekly_remaining),
            remaining_percent: entry.non_weekly_remaining,
            reset_at_epoch: entry.non_weekly_reset_epoch,
        },
        RateLimitWindow {
            label: "Weekly".to_string(),
            used_percent: remaining_to_used_percent(entry.weekly_remaining),
            remaining_percent: entry.weekly_remaining,
            reset_at_epoch: Some(entry.weekly_reset_epoch).filter(|epoch| *epoch > 0),
        },
    ]
}

fn percent_i64(percent: f64) -> i64 {
    if percent.is_finite() {
        (percent.round() as i64).clamp(0, 100)
    } else {
        0
    }
}

fn remaining_to_used_percent(remaining: i64) -> i64 {
    (100 - remaining).clamp(0, 100)
}

fn redact_sensitive_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut next = serde_json::Map::new();
            for (key, val) in map {
                if is_sensitive_key(key) {
                    continue;
                }
                next.insert(key.clone(), redact_sensitive_json(val));
            }
            Value::Object(next)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_sensitive_json).collect()),
        _ => value.clone(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    matches!(
        key,
        "access_token" | "refresh_token" | "id_token" | "authorization" | "Authorization"
    )
}

#[derive(Default)]
struct AsyncFetchResult {
    line: Option<String>,
    rc: i32,
    err: String,
    /// The line was served from cache past its freshness TTL.
    stale: bool,
    /// The live fetch succeeded but the backend reported no active rate-limit
    /// window (`rate_limit: null`) and no cache was available. Benign.
    no_window: bool,
}

/// Distinct rc for "live fetch ok, but backend reports no active window". Kept
/// separate from the network-failure codes (1/2/3) so it is neither retried
/// nor counted as an error.
const RC_NO_RATE_LIMIT_WINDOW: i32 = 6;

struct AsyncCollectedItem<T> {
    secret_name: String,
    value: T,
}

fn run_async_mode(args: &RateLimitsOptions, debug_mode: bool) -> Result<i32> {
    run_async_mode_impl(args, debug_mode, false)
}

fn run_async_watch_mode(args: &RateLimitsOptions, debug_mode: bool) -> Result<i32> {
    run_async_mode_impl(args, debug_mode, true)
}

fn run_async_mode_impl(
    args: &RateLimitsOptions,
    debug_mode: bool,
    watch_mode: bool,
) -> Result<i32> {
    if args.json {
        eprintln!("codex-rate-limits: --async does not support --json");
        return Ok(64);
    }
    if args.one_line {
        eprintln!("codex-rate-limits: --async does not support --one-line");
        return Ok(64);
    }
    if let Some(secret) = args.secret.as_deref() {
        let _ = secret;
        eprintln!("codex-rate-limits: --async does not accept positional args");
        eprintln!(
            "codex-rate-limits: hint: async always queries all secrets under CODEX_SECRET_DIR"
        );
        return Ok(64);
    }
    if args.clear_cache && args.cached {
        eprintln!("codex-rate-limits: --async: -c is not compatible with --cached");
        return Ok(64);
    }

    let jobs = resolve_async_jobs(args.jobs.as_deref());

    if args.clear_cache
        && let Err(err) = cache::clear_prompt_segment_cache()
    {
        eprintln!("{err}");
        return Ok(1);
    }

    let secret_files = match collect_secret_files_for_async_text() {
        Ok(value) => value,
        Err(err) => {
            eprintln!("{err}");
            return Ok(1);
        }
    };

    if !watch_mode {
        if secret_files.is_empty() {
            let secret_dir = crate::paths::resolve_secret_dir().unwrap_or_default();
            eprintln!(
                "codex-rate-limits-async: no secrets found in {}",
                secret_dir.display()
            );
            return Ok(1);
        }

        let current_name = current_secret_basename(&secret_files);
        let round = collect_async_round(&secret_files, args.cached, args.no_refresh_auth, jobs);
        render_all_accounts_table(
            round.rows,
            &round.window_labels,
            current_name.as_deref(),
            None,
        );
        emit_async_debug(debug_mode, &secret_files, &round.stderr_map);
        return Ok(round.rc);
    }

    let mut overall_rc = 0;
    let mut rendered_rounds = 0u64;
    let max_rounds = watch_max_rounds_for_test();
    let watch_interval_seconds = watch_interval_seconds();
    let is_terminal_stdout = std::io::stdout().is_terminal();

    loop {
        let secret_files = match collect_secret_files_for_async_text() {
            Ok(value) => value,
            Err(err) => {
                overall_rc = 1;
                if is_terminal_stdout {
                    print!("{ANSI_CLEAR_SCREEN_AND_HOME}");
                }
                eprintln!("{err}");
                let _ = std::io::stdout().flush();

                rendered_rounds += 1;
                if let Some(limit) = max_rounds
                    && rendered_rounds >= limit
                {
                    break;
                }

                thread::sleep(Duration::from_secs(watch_interval_seconds));
                continue;
            }
        };
        let current_name = current_secret_basename(&secret_files);
        let round = collect_async_round(&secret_files, args.cached, args.no_refresh_auth, jobs);
        if round.rc != 0 {
            overall_rc = 1;
        }

        if is_terminal_stdout {
            print!("{ANSI_CLEAR_SCREEN_AND_HOME}");
        }

        let now_epoch = Utc::now().timestamp();
        let update_time = format_watch_update_time(now_epoch);
        render_all_accounts_table(
            round.rows,
            &round.window_labels,
            current_name.as_deref(),
            Some(update_time.as_str()),
        );
        emit_async_debug(debug_mode, &secret_files, &round.stderr_map);
        let _ = std::io::stdout().flush();

        rendered_rounds += 1;
        if let Some(limit) = max_rounds
            && rendered_rounds >= limit
        {
            break;
        }

        thread::sleep(Duration::from_secs(watch_interval_seconds));
    }

    Ok(overall_rc)
}

fn collect_secret_files_for_async_text() -> std::result::Result<Vec<PathBuf>, String> {
    let secret_dir = crate::paths::resolve_secret_dir().unwrap_or_default();
    if !secret_dir.is_dir() {
        return Err(format!(
            "codex-rate-limits-async: CODEX_SECRET_DIR not found: {}",
            secret_dir.display()
        ));
    }

    let mut secret_files: Vec<PathBuf> = std::fs::read_dir(&secret_dir)
        .map_err(|err| format!("codex-rate-limits-async: failed to read CODEX_SECRET_DIR: {err}"))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();

    secret_files.sort();
    Ok(secret_files)
}

struct AsyncRound {
    rc: i32,
    rows: Vec<Row>,
    window_labels: std::collections::HashSet<String>,
    stderr_map: std::collections::HashMap<String, String>,
}

fn resolve_async_jobs(jobs: Option<&str>) -> usize {
    jobs.and_then(|raw| raw.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value as usize)
        .unwrap_or(5)
}

fn collect_async_items<T, F>(
    secret_files: &[PathBuf],
    jobs: usize,
    progress_prefix: Option<&str>,
    worker: F,
) -> std::collections::HashMap<String, T>
where
    T: Send + 'static,
    F: Fn(PathBuf, String) -> T + Send + Sync + 'static,
{
    let total = secret_files.len();
    if total == 0 {
        return std::collections::HashMap::new();
    }

    let progress = if total > 1 {
        progress_prefix.map(|prefix| {
            Progress::new(
                total as u64,
                ProgressOptions::default()
                    .with_prefix(prefix)
                    .with_finish(ProgressFinish::Clear),
            )
        })
    } else {
        None
    };

    let worker_count = jobs.clamp(1, total);
    let worker = Arc::new(worker);
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::new();
    let mut index = 0usize;

    while index < total && handles.len() < worker_count {
        let path = secret_files[index].clone();
        index += 1;
        let tx = tx.clone();
        let worker = Arc::clone(&worker);
        handles.push(thread::spawn(move || {
            let secret_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_string();
            let value = worker(path, secret_name.clone());
            let _ = tx.send(AsyncCollectedItem { secret_name, value });
        }));
    }

    let mut items = std::collections::HashMap::new();
    while items.len() < total {
        let item = match rx.recv() {
            Ok(item) => item,
            Err(_) => break,
        };
        if let Some(progress) = &progress {
            progress.set_message(item.secret_name.clone());
            progress.inc(1);
        }
        items.insert(item.secret_name.clone(), item.value);

        if index < total {
            let path = secret_files[index].clone();
            index += 1;
            let tx = tx.clone();
            let worker = Arc::clone(&worker);
            handles.push(thread::spawn(move || {
                let secret_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("")
                    .to_string();
                let value = worker(path, secret_name.clone());
                let _ = tx.send(AsyncCollectedItem { secret_name, value });
            }));
        }
    }

    if let Some(progress) = progress {
        progress.finish_and_clear();
    }

    drop(tx);
    for handle in handles {
        let _ = handle.join();
    }

    items
}

fn collect_async_round(
    secret_files: &[PathBuf],
    cached_mode: bool,
    no_refresh_auth: bool,
    jobs: usize,
) -> AsyncRound {
    let mut events = collect_async_items(
        secret_files,
        jobs,
        Some("codex-rate-limits "),
        move |path, secret_name| {
            async_fetch_one_line(&path, cached_mode, no_refresh_auth, &secret_name)
        },
    );

    let mut rc = 0;
    let mut rows: Vec<Row> = Vec::new();
    let mut window_labels = std::collections::HashSet::new();
    let mut stderr_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for secret_file in secret_files {
        let secret_name = secret_file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();

        let mut row = Row::empty(secret_name.trim_end_matches(".json").to_string());
        let mut benign_no_window = false;
        let event = events.remove(&secret_name);
        if let Some(event) = event {
            if !event.err.is_empty() {
                stderr_map.insert(secret_name.clone(), event.err.clone());
            }
            // A null window (no_window) is benign and must not fail the round;
            // a stale cache fallback (rc == 0) likewise succeeded.
            if !cached_mode && event.rc != 0 && !event.no_window {
                rc = 1;
            }
            benign_no_window = event.no_window;

            if let Some(line) = &event.line
                && let Some(parsed) = parse_one_line_output(line)
            {
                row.window_label = parsed.window_label.clone();
                row.non_weekly_remaining = parsed.non_weekly_remaining;
                row.weekly_remaining = parsed.weekly_remaining;
                row.weekly_reset_iso = parsed.weekly_reset_iso.clone();

                if cached_mode {
                    if let Ok(cache_entry) = cache::read_cache_entry_for_cached_mode(secret_file) {
                        row.non_weekly_reset_epoch = cache_entry.non_weekly_reset_epoch;
                        row.weekly_reset_epoch = Some(cache_entry.weekly_reset_epoch);
                    }
                } else {
                    let values = crate::json::read_json(secret_file).ok();
                    if let Some(values) = values {
                        row.non_weekly_reset_epoch = crate::json::i64_at(
                            &values,
                            &["codex_rate_limits", "non_weekly_reset_at_epoch"],
                        );
                        row.weekly_reset_epoch = crate::json::i64_at(
                            &values,
                            &["codex_rate_limits", "weekly_reset_at_epoch"],
                        );
                    }
                    if (row.non_weekly_reset_epoch.is_none() || row.weekly_reset_epoch.is_none())
                        && let Ok(cache_entry) = cache::read_cache_entry(secret_file)
                    {
                        if row.non_weekly_reset_epoch.is_none() {
                            row.non_weekly_reset_epoch = cache_entry.non_weekly_reset_epoch;
                        }
                        if row.weekly_reset_epoch.is_none() {
                            row.weekly_reset_epoch = Some(cache_entry.weekly_reset_epoch);
                        }
                    }
                }

                row.state = if event.stale {
                    RowState::Stale
                } else {
                    RowState::Filled
                };
                window_labels.insert(row.window_label.clone());
                rows.push(row);
                continue;
            }
        }

        if benign_no_window {
            row.state = RowState::NoWindow;
        } else if !cached_mode {
            rc = 1;
        }
        rows.push(row);
    }

    AsyncRound {
        rc,
        rows,
        window_labels,
        stderr_map,
    }
}

fn render_all_accounts_table(
    mut rows: Vec<Row>,
    window_labels: &std::collections::HashSet<String>,
    current_name: Option<&str>,
    update_time: Option<&str>,
) {
    println!("\n🚦 Codex rate limits for all accounts\n");

    let mut non_weekly_header = "Non-weekly".to_string();
    let multiple_labels = window_labels.len() != 1;
    if !multiple_labels && let Some(label) = window_labels.iter().next() {
        non_weekly_header = label.clone();
    }

    let now_epoch = Utc::now().timestamp();

    println!(
        "{:<15}  {:>8}  {:>7}  {:>8}  {:>7}  {:<18}",
        "Name", non_weekly_header, "Left", "Weekly", "Left", "Reset"
    );
    println!("----------------------------------------------------------------------------");

    rows.sort_by_key(|row| row.sort_key());

    for row in rows {
        // A null window reports "n/a"; a stale fallback keeps its values but is
        // marked so the reader knows they are not live.
        let no_window = row.state == RowState::NoWindow;

        let display_non_weekly = if no_window {
            "n/a".to_string()
        } else if multiple_labels && !row.window_label.is_empty() {
            if row.non_weekly_remaining >= 0 {
                format!("{}:{}%", row.window_label, row.non_weekly_remaining)
            } else {
                "-".to_string()
            }
        } else if row.non_weekly_remaining >= 0 {
            format!("{}%", row.non_weekly_remaining)
        } else {
            "-".to_string()
        };

        let non_weekly_left = row
            .non_weekly_reset_epoch
            .and_then(|epoch| render::format_until_epoch_compact(epoch, now_epoch))
            .unwrap_or_else(|| "-".to_string());
        let weekly_left = row
            .weekly_reset_epoch
            .and_then(|epoch| render::format_until_epoch_compact(epoch, now_epoch))
            .unwrap_or_else(|| "-".to_string());
        let mut reset_display = if no_window {
            "n/a".to_string()
        } else {
            row.weekly_reset_epoch
                .and_then(render::format_epoch_local_datetime_with_offset)
                .unwrap_or_else(|| "-".to_string())
        };
        if row.state == RowState::Stale {
            reset_display.push_str(" (stale)");
        }

        let non_weekly_display = ansi::format_percent_cell(&display_non_weekly, 8, None);
        let weekly_display = if no_window {
            ansi::format_percent_cell("n/a", 8, None)
        } else if row.weekly_remaining >= 0 {
            ansi::format_percent_cell(&format!("{}%", row.weekly_remaining), 8, None)
        } else {
            ansi::format_percent_cell("-", 8, None)
        };

        let is_current = current_name == Some(row.name.as_str());
        let name_display = ansi::format_name_cell(&row.name, 15, is_current, None);

        println!(
            "{}  {}  {:>7}  {}  {:>7}  {:<18}",
            name_display,
            non_weekly_display,
            non_weekly_left,
            weekly_display,
            weekly_left,
            reset_display
        );
    }

    if let Some(update_time) = update_time {
        println!();
        println!("Last update: {update_time}");
    }
}

fn emit_async_debug(
    debug_mode: bool,
    secret_files: &[PathBuf],
    stderr_map: &std::collections::HashMap<String, String>,
) {
    if !debug_mode {
        return;
    }

    let mut printed = false;
    for secret_file in secret_files {
        let secret_name = secret_file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();
        if let Some(err) = stderr_map.get(&secret_name) {
            if err.is_empty() {
                continue;
            }
            if !printed {
                printed = true;
                eprintln!();
                eprintln!("codex-rate-limits-async: per-account stderr (captured):");
            }
            let _ = secret_name;
            eprintln!("---- account stderr ----");
            eprintln!("{err}");
        }
    }
}

fn watch_max_rounds_for_test() -> Option<u64> {
    std::env::var("CODEX_RATE_LIMITS_WATCH_MAX_ROUNDS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|value| *value > 0)
}

fn watch_interval_seconds() -> u64 {
    std::env::var("CODEX_RATE_LIMITS_WATCH_INTERVAL_SECONDS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(WATCH_INTERVAL_SECONDS)
}

fn format_watch_update_time(now_epoch: i64) -> String {
    render::format_epoch_local(now_epoch, "%Y-%m-%d %H:%M:%S %:z")
        .unwrap_or_else(|| now_epoch.to_string())
}

fn async_fetch_one_line(
    target_file: &Path,
    cached_mode: bool,
    no_refresh_auth: bool,
    secret_name: &str,
) -> AsyncFetchResult {
    if cached_mode {
        return fetch_one_line_cached(target_file);
    }

    let mut attempt = 1;
    let max_attempts = 2;
    let mut network_err: Option<String> = None;

    let mut result = fetch_one_line_network(target_file, no_refresh_auth);
    if !result.err.is_empty() {
        network_err = Some(result.err.clone());
    }

    while attempt < max_attempts && result.rc == 3 {
        thread::sleep(Duration::from_millis(250));
        let next = fetch_one_line_network(target_file, no_refresh_auth);
        if !next.err.is_empty() {
            network_err = Some(next.err.clone());
        }
        result = next;
        attempt += 1;
        if result.rc != 3 {
            break;
        }
    }

    let mut errors: Vec<String> = Vec::new();
    if let Some(err) = network_err {
        errors.push(err);
    }

    // A null window is benign — the backend simply has no usage recorded in the
    // current window. Degrade to the last-known cached values (marked stale)
    // rather than surfacing it as a failure.
    let no_window_live = result.rc == RC_NO_RATE_LIMIT_WINDOW;

    let missing_line = result
        .line
        .as_ref()
        .map(|line| line.trim().is_empty())
        .unwrap_or(true);

    if result.rc != 0 || missing_line {
        let cached = fetch_one_line_cached_allow_stale(target_file);
        if !cached.err.is_empty() {
            errors.push(cached.err.clone());
        }
        if cached.rc == 0
            && cached
                .line
                .as_ref()
                .map(|line| !line.trim().is_empty())
                .unwrap_or(false)
        {
            // Only annotate the fallback for genuine failures; a null window is
            // an expected, quiet condition.
            if result.rc != 0 && !no_window_live {
                let _ = secret_name;
                errors.push(format!(
                    "codex-rate-limits-async: falling back to cache (rc={})",
                    result.rc
                ));
            }
            result = AsyncFetchResult {
                line: cached.line,
                stale: cached.stale,
                ..Default::default()
            };
        } else if no_window_live {
            // No cache to borrow from, but a null window is still benign.
            result = AsyncFetchResult {
                rc: RC_NO_RATE_LIMIT_WINDOW,
                no_window: true,
                ..Default::default()
            };
        }
    }

    let line = result.line.map(normalize_one_line);
    let err = errors.join("\n");
    AsyncFetchResult {
        line,
        rc: result.rc,
        err,
        stale: result.stale,
        no_window: result.no_window,
    }
}

fn fetch_one_line_network(target_file: &Path, no_refresh_auth: bool) -> AsyncFetchResult {
    if !target_file.is_file() {
        return AsyncFetchResult {
            line: None,
            rc: 1,
            err: format!("codex-rate-limits: {} not found", target_file.display()),
            ..Default::default()
        };
    }

    let base_url = std::env::var("CODEX_CHATGPT_BASE_URL")
        .unwrap_or_else(|_| "https://chatgpt.com/backend-api/".to_string());
    let connect_timeout = env_timeout("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", 2);
    let max_time = env_timeout("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", 8);

    let usage_request = UsageRequest {
        target_file: target_file.to_path_buf(),
        refresh_on_401: refresh_on_401_enabled(no_refresh_auth),
        suppress_auth_refresh_output: false,
        base_url,
        connect_timeout_seconds: connect_timeout,
        max_time_seconds: max_time,
    };

    let usage = match fetch_usage(&usage_request) {
        Ok(value) => value,
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("missing access_token") {
                return AsyncFetchResult {
                    line: None,
                    rc: 2,
                    err: format!(
                        "codex-rate-limits: missing access_token in {}",
                        target_file.display()
                    ),
                    ..Default::default()
                };
            }
            return AsyncFetchResult {
                line: None,
                rc: 3,
                err: msg,
                ..Default::default()
            };
        }
    };

    if should_writeback_usage(target_file)
        && let Err(err) = writeback::write_weekly(target_file, &usage.json)
    {
        return AsyncFetchResult {
            line: None,
            rc: 4,
            err: err.to_string(),
            ..Default::default()
        };
    }

    if is_auth_file(target_file) {
        match sync_auth_silent() {
            Ok((sync_rc, sync_err)) => {
                if sync_rc != 0 {
                    return AsyncFetchResult {
                        line: None,
                        rc: 5,
                        err: sync_err.unwrap_or_default(),
                        ..Default::default()
                    };
                }
            }
            Err(_) => {
                return AsyncFetchResult {
                    line: None,
                    rc: 1,
                    ..Default::default()
                };
            }
        }
    }

    let usage_data = match render::parse_usage(&usage.json) {
        Some(value) => value,
        None => {
            if render::rate_limit_is_explicit_null(&usage.json) {
                return AsyncFetchResult {
                    rc: RC_NO_RATE_LIMIT_WINDOW,
                    ..Default::default()
                };
            }
            return AsyncFetchResult {
                line: None,
                rc: 3,
                err: "codex-rate-limits: invalid usage payload".to_string(),
                ..Default::default()
            };
        }
    };

    let values = render::render_values(&usage_data);
    let weekly = render::weekly_values(&values);

    let fetched_at_epoch = Utc::now().timestamp();
    if fetched_at_epoch > 0 {
        let _ = cache::write_prompt_segment_cache(
            target_file,
            fetched_at_epoch,
            &weekly.non_weekly_label,
            weekly.non_weekly_remaining,
            weekly.weekly_remaining,
            weekly.weekly_reset_epoch,
            weekly.non_weekly_reset_epoch,
        );
    }

    AsyncFetchResult {
        line: Some(format_one_line_output(
            target_file,
            &weekly.non_weekly_label,
            weekly.non_weekly_remaining,
            weekly.weekly_remaining,
            weekly.weekly_reset_epoch,
        )),
        rc: 0,
        ..Default::default()
    }
}

fn fetch_one_line_cached(target_file: &Path) -> AsyncFetchResult {
    match cache::read_cache_entry_for_cached_mode(target_file) {
        Ok(entry) => AsyncFetchResult {
            line: Some(format_one_line_output(
                target_file,
                &entry.non_weekly_label,
                entry.non_weekly_remaining,
                entry.weekly_remaining,
                entry.weekly_reset_epoch,
            )),
            rc: 0,
            ..Default::default()
        },
        Err(err) => AsyncFetchResult {
            line: None,
            rc: 1,
            err: err.to_string(),
            ..Default::default()
        },
    }
}

/// Reads the cached one-line value without enforcing the freshness TTL, marking
/// the result `stale` when it is past the TTL. Used as the diag-path fallback
/// so a transient null/failed live fetch degrades to the last-known values.
fn fetch_one_line_cached_allow_stale(target_file: &Path) -> AsyncFetchResult {
    match cache::read_cache_entry_allow_stale(target_file) {
        Ok(read) => AsyncFetchResult {
            line: Some(format_one_line_output(
                target_file,
                &read.entry.non_weekly_label,
                read.entry.non_weekly_remaining,
                read.entry.weekly_remaining,
                read.entry.weekly_reset_epoch,
            )),
            rc: 0,
            stale: read.stale,
            ..Default::default()
        },
        Err(err) => AsyncFetchResult {
            line: None,
            rc: 1,
            err: err.to_string(),
            ..Default::default()
        },
    }
}

fn format_one_line_output(
    target_file: &Path,
    non_weekly_label: &str,
    non_weekly_remaining: i64,
    weekly_remaining: i64,
    weekly_reset_epoch: i64,
) -> String {
    let _ = target_file;
    let weekly_reset_iso =
        render::format_epoch_local_datetime(weekly_reset_epoch).unwrap_or_else(|| "?".to_string());

    format!(
        "{}:{}% W:{}% {}",
        non_weekly_label, non_weekly_remaining, weekly_remaining, weekly_reset_iso
    )
}

fn normalize_one_line(line: String) -> String {
    line.replace(['\n', '\r', '\t'], " ")
}

fn sync_auth_silent() -> Result<(i32, Option<String>)> {
    let auth_file = match crate::paths::resolve_auth_file() {
        Some(path) => path,
        None => return Ok((0, None)),
    };

    let sync_result = match sync_auth_to_matching_secrets(
        &CODEX_PROVIDER_PROFILE,
        &auth_file,
        fs::SECRET_FILE_MODE,
        TimestampPolicy::Strict,
    ) {
        Ok(result) => result,
        Err(SyncSecretsError::HashAuthFile { path, .. })
        | Err(SyncSecretsError::HashSecretFile { path, .. }) => {
            return Ok((1, Some(format!("codex: failed to hash {}", path.display()))));
        }
        Err(err) => return Err(err.into()),
    };
    if !sync_result.auth_file_present || !sync_result.auth_identity_present {
        return Ok((0, None));
    }

    Ok((0, None))
}

fn maybe_sync_all_mode_auth_silent(debug_mode: bool) {
    match sync_auth_silent() {
        Ok((0, _)) => {}
        Ok((_, sync_err)) => {
            if debug_mode
                && let Some(message) = sync_err
                && !message.trim().is_empty()
            {
                eprintln!("{message}");
            }
        }
        Err(err) => {
            if debug_mode {
                eprintln!("codex-rate-limits: failed to sync auth and secrets: {err}");
            }
        }
    }
}

fn run_all_mode(args: &RateLimitsOptions, cached_mode: bool, debug_mode: bool) -> Result<i32> {
    let secret_dir = crate::paths::resolve_secret_dir().unwrap_or_default();
    if !secret_dir.is_dir() {
        eprintln!(
            "codex-rate-limits: CODEX_SECRET_DIR not found: {}",
            secret_dir.display()
        );
        return Ok(1);
    }

    let mut secret_files: Vec<PathBuf> = std::fs::read_dir(&secret_dir)?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();

    if secret_files.is_empty() {
        eprintln!(
            "codex-rate-limits: no secrets found in {}",
            secret_dir.display()
        );
        return Ok(1);
    }

    secret_files.sort();

    let current_name = current_secret_basename(&secret_files);

    let total = secret_files.len();
    let progress = if total > 1 {
        Some(Progress::new(
            total as u64,
            ProgressOptions::default()
                .with_prefix("codex-rate-limits ")
                .with_finish(ProgressFinish::Clear),
        ))
    } else {
        None
    };

    let mut rc = 0;
    let mut rows: Vec<Row> = Vec::new();
    let mut window_labels = std::collections::HashSet::new();

    for secret_file in secret_files {
        let secret_name = secret_file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();
        if let Some(progress) = &progress {
            progress.set_message(secret_name.clone());
        }

        let mut row = Row::empty(secret_name.trim_end_matches(".json").to_string());
        let output =
            match single_one_line(&secret_file, cached_mode, args.no_refresh_auth, debug_mode) {
                Ok(Some(line)) => line,
                Ok(None) => String::new(),
                Err(_) => String::new(),
            };

        if output.is_empty() {
            if !cached_mode {
                rc = 1;
            }
            rows.push(row);
            continue;
        }

        if let Some(parsed) = parse_one_line_output(&output) {
            row.window_label = parsed.window_label.clone();
            row.non_weekly_remaining = parsed.non_weekly_remaining;
            row.weekly_remaining = parsed.weekly_remaining;
            row.weekly_reset_iso = parsed.weekly_reset_iso.clone();

            if cached_mode {
                if let Ok(cache_entry) = cache::read_cache_entry_for_cached_mode(&secret_file) {
                    row.non_weekly_reset_epoch = cache_entry.non_weekly_reset_epoch;
                    row.weekly_reset_epoch = Some(cache_entry.weekly_reset_epoch);
                }
            } else {
                let values = crate::json::read_json(&secret_file).ok();
                if let Some(values) = values {
                    row.non_weekly_reset_epoch = crate::json::i64_at(
                        &values,
                        &["codex_rate_limits", "non_weekly_reset_at_epoch"],
                    );
                    row.weekly_reset_epoch = crate::json::i64_at(
                        &values,
                        &["codex_rate_limits", "weekly_reset_at_epoch"],
                    );
                }
            }

            window_labels.insert(row.window_label.clone());
            rows.push(row);
        } else {
            if !cached_mode {
                rc = 1;
            }
            rows.push(row);
        }

        if let Some(progress) = &progress {
            progress.inc(1);
        }
    }

    if let Some(progress) = progress {
        progress.finish_and_clear();
    }

    println!("\n🚦 Codex rate limits for all accounts\n");

    let mut non_weekly_header = "Non-weekly".to_string();
    let multiple_labels = window_labels.len() != 1;
    if !multiple_labels && let Some(label) = window_labels.iter().next() {
        non_weekly_header = label.clone();
    }

    let now_epoch = Utc::now().timestamp();

    println!(
        "{:<15}  {:>8}  {:>7}  {:>8}  {:>7}  {:<18}",
        "Name", non_weekly_header, "Left", "Weekly", "Left", "Reset"
    );
    println!("----------------------------------------------------------------------------");

    rows.sort_by_key(|row| row.sort_key());

    for row in rows {
        let display_non_weekly = if multiple_labels && !row.window_label.is_empty() {
            if row.non_weekly_remaining >= 0 {
                format!("{}:{}%", row.window_label, row.non_weekly_remaining)
            } else {
                "-".to_string()
            }
        } else if row.non_weekly_remaining >= 0 {
            format!("{}%", row.non_weekly_remaining)
        } else {
            "-".to_string()
        };

        let non_weekly_left = row
            .non_weekly_reset_epoch
            .and_then(|epoch| render::format_until_epoch_compact(epoch, now_epoch))
            .unwrap_or_else(|| "-".to_string());
        let weekly_left = row
            .weekly_reset_epoch
            .and_then(|epoch| render::format_until_epoch_compact(epoch, now_epoch))
            .unwrap_or_else(|| "-".to_string());
        let reset_display = row
            .weekly_reset_epoch
            .and_then(render::format_epoch_local_datetime_with_offset)
            .unwrap_or_else(|| "-".to_string());

        let non_weekly_display = ansi::format_percent_cell(&display_non_weekly, 8, None);
        let weekly_display = if row.weekly_remaining >= 0 {
            ansi::format_percent_cell(&format!("{}%", row.weekly_remaining), 8, None)
        } else {
            ansi::format_percent_cell("-", 8, None)
        };

        let is_current = current_name.as_deref() == Some(row.name.as_str());
        let name_display = ansi::format_name_cell(&row.name, 15, is_current, None);

        println!(
            "{}  {}  {:>7}  {}  {:>7}  {:<18}",
            name_display,
            non_weekly_display,
            non_weekly_left,
            weekly_display,
            weekly_left,
            reset_display
        );
    }

    Ok(rc)
}

fn current_secret_basename(secret_files: &[PathBuf]) -> Option<String> {
    let auth_file = crate::paths::resolve_auth_file()?;
    if !auth_file.is_file() {
        return None;
    }

    let auth_key = auth::identity_key_from_auth_file(&auth_file).ok().flatten();
    let auth_hash = fs::sha256_file(&auth_file).ok();

    if let Some(auth_hash) = auth_hash.as_deref() {
        for secret_file in secret_files {
            if let Ok(secret_hash) = fs::sha256_file(secret_file)
                && secret_hash == auth_hash
                && let Some(name) = secret_file.file_name().and_then(|name| name.to_str())
            {
                return Some(name.trim_end_matches(".json").to_string());
            }
        }
    }

    if let Some(auth_key) = auth_key.as_deref() {
        for secret_file in secret_files {
            if let Ok(Some(candidate_key)) = auth::identity_key_from_auth_file(secret_file)
                && candidate_key == auth_key
                && let Some(name) = secret_file.file_name().and_then(|name| name.to_str())
            {
                return Some(name.trim_end_matches(".json").to_string());
            }
        }
    }

    None
}

fn run_single_mode(
    args: &RateLimitsOptions,
    cached_mode: bool,
    one_line: bool,
    output_json: bool,
) -> Result<i32> {
    let target_file = match resolve_target(args.secret.as_deref()) {
        Ok(path) => path,
        Err(code) => return Ok(code),
    };

    if !target_file.is_file() {
        if output_json {
            diag_output::emit_error(
                DIAG_SCHEMA_VERSION,
                DIAG_COMMAND,
                "target-not-found",
                format!("codex-rate-limits: {} not found", target_file.display()),
                Some(serde_json::json!({
                    "target_file": target_file.display().to_string(),
                })),
            )?;
        } else {
            eprintln!("codex-rate-limits: {} not found", target_file.display());
        }
        return Ok(1);
    }

    if cached_mode {
        match cache::read_cache_entry_for_cached_mode(&target_file) {
            Ok(entry) => {
                let weekly_reset_iso =
                    render::format_epoch_local_datetime(entry.weekly_reset_epoch)
                        .unwrap_or_else(|| "?".to_string());
                println!(
                    "{}:{}% W:{}% {}",
                    entry.non_weekly_label,
                    entry.non_weekly_remaining,
                    entry.weekly_remaining,
                    weekly_reset_iso
                );
                return Ok(0);
            }
            Err(err) => {
                eprintln!("{err}");
                return Ok(1);
            }
        }
    }

    let base_url = std::env::var("CODEX_CHATGPT_BASE_URL")
        .unwrap_or_else(|_| "https://chatgpt.com/backend-api/".to_string());
    let connect_timeout = env_timeout("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", 2);
    let max_time = env_timeout("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", 8);

    let usage_request = UsageRequest {
        target_file: target_file.clone(),
        refresh_on_401: refresh_on_401_enabled(args.no_refresh_auth),
        suppress_auth_refresh_output: false,
        base_url,
        connect_timeout_seconds: connect_timeout,
        max_time_seconds: max_time,
    };

    let usage = match fetch_usage(&usage_request) {
        Ok(value) => value,
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("missing access_token") {
                if output_json {
                    diag_output::emit_error(
                        DIAG_SCHEMA_VERSION,
                        DIAG_COMMAND,
                        "missing-access-token",
                        format!(
                            "codex-rate-limits: missing access_token in {}",
                            target_file.display()
                        ),
                        Some(serde_json::json!({
                            "target_file": target_file.display().to_string(),
                        })),
                    )?;
                } else {
                    eprintln!(
                        "codex-rate-limits: missing access_token in {}",
                        target_file.display()
                    );
                }
                return Ok(2);
            }
            if output_json {
                diag_output::emit_error(
                    DIAG_SCHEMA_VERSION,
                    DIAG_COMMAND,
                    "request-failed",
                    msg,
                    Some(serde_json::json!({
                        "target_file": target_file.display().to_string(),
                    })),
                )?;
            } else {
                eprintln!("{msg}");
            }
            return Ok(3);
        }
    };

    if should_writeback_usage(&target_file)
        && let Err(err) = writeback::write_weekly(&target_file, &usage.json)
    {
        if output_json {
            diag_output::emit_error(
                DIAG_SCHEMA_VERSION,
                DIAG_COMMAND,
                "writeback-failed",
                err.to_string(),
                Some(serde_json::json!({
                    "target_file": target_file.display().to_string(),
                })),
            )?;
        } else {
            eprintln!("{err}");
        }
        return Ok(4);
    }

    if is_auth_file(&target_file) {
        let sync_rc = auth::sync::run_with_json(false)?;
        if sync_rc != 0 {
            if output_json {
                diag_output::emit_error(
                    DIAG_SCHEMA_VERSION,
                    DIAG_COMMAND,
                    "sync-failed",
                    "codex-rate-limits: failed to sync auth file",
                    Some(serde_json::json!({
                        "target_file": target_file.display().to_string(),
                    })),
                )?;
            }
            return Ok(5);
        }
    }

    let usage_data = match render::parse_usage(&usage.json) {
        Some(value) => value,
        None => {
            if render::rate_limit_is_explicit_null(&usage.json) {
                return emit_single_no_window(&target_file, output_json, one_line);
            }
            if output_json {
                diag_output::emit_error(
                    DIAG_SCHEMA_VERSION,
                    DIAG_COMMAND,
                    "invalid-usage-payload",
                    "codex-rate-limits: invalid usage payload",
                    Some(serde_json::json!({
                        "target_file": target_file.display().to_string(),
                        "raw_usage": redact_sensitive_json(&usage.json),
                    })),
                )?;
            } else {
                eprintln!("codex-rate-limits: invalid usage payload");
            }
            return Ok(3);
        }
    };

    let values = render::render_values(&usage_data);
    let weekly = render::weekly_values(&values);

    let fetched_at_epoch = Utc::now().timestamp();
    if fetched_at_epoch > 0 {
        let _ = cache::write_prompt_segment_cache(
            &target_file,
            fetched_at_epoch,
            &weekly.non_weekly_label,
            weekly.non_weekly_remaining,
            weekly.weekly_remaining,
            weekly.weekly_reset_epoch,
            weekly.non_weekly_reset_epoch,
        );
    }

    if output_json {
        let windows = windows_from_usage_values(&usage_data, &values);
        let result = RateLimitJsonResult {
            provider: "codex".to_string(),
            name: secret_display_name(&target_file),
            target_file: target_file_name(&target_file),
            status: "ok".to_string(),
            ok: true,
            source: "network".to_string(),
            summary: Some(RateLimitSummary {
                non_weekly_label: weekly.non_weekly_label,
                non_weekly_remaining: weekly.non_weekly_remaining,
                non_weekly_reset_epoch: weekly.non_weekly_reset_epoch,
                weekly_remaining: weekly.weekly_remaining,
                weekly_reset_epoch: weekly.weekly_reset_epoch,
                weekly_reset_local: render::format_epoch_local_datetime_with_offset(
                    weekly.weekly_reset_epoch,
                ),
            }),
            windows: Some(windows),
            raw_usage: Some(redact_sensitive_json(&usage.json)),
            error: None,
        };
        diag_output::emit_json(&RateLimitSingleEnvelope {
            schema_version: DIAG_SCHEMA_VERSION.to_string(),
            command: DIAG_COMMAND.to_string(),
            mode: "single".to_string(),
            ok: true,
            result,
        })?;
        return Ok(0);
    }

    if one_line {
        let weekly_reset_iso = render::format_epoch_local_datetime(weekly.weekly_reset_epoch)
            .unwrap_or_else(|| "?".to_string());

        println!(
            "{}:{}% W:{}% {}",
            weekly.non_weekly_label,
            weekly.non_weekly_remaining,
            weekly.weekly_remaining,
            weekly_reset_iso
        );
        return Ok(0);
    }

    println!("Rate limits remaining");
    let primary_reset = render::format_epoch_local_datetime(values.primary_reset_epoch)
        .unwrap_or_else(|| "?".to_string());
    let secondary_reset = render::format_epoch_local_datetime(values.secondary_reset_epoch)
        .unwrap_or_else(|| "?".to_string());

    println!(
        "{} {}% • {}",
        values.primary_label, values.primary_remaining, primary_reset
    );
    println!(
        "{} {}% • {}",
        values.secondary_label, values.secondary_remaining, secondary_reset
    );

    Ok(0)
}

/// Renders a benign `rate_limit: null` response in single mode: serve the
/// last-known cached values (marked stale) when available, otherwise report
/// "no active rate-limit window" as a success rather than a malformed payload.
fn emit_single_no_window(target_file: &Path, output_json: bool, one_line: bool) -> Result<i32> {
    if let Ok(read) = cache::read_cache_entry_allow_stale(target_file) {
        if output_json {
            let result = collect_json_from_cache(target_file, "cache-fallback", false);
            let ok = result.ok;
            diag_output::emit_json(&RateLimitSingleEnvelope {
                schema_version: DIAG_SCHEMA_VERSION.to_string(),
                command: DIAG_COMMAND.to_string(),
                mode: "single".to_string(),
                ok,
                result,
            })?;
            return Ok(0);
        }

        let entry = read.entry;
        let stale_suffix = if read.stale { " (stale)" } else { "" };
        if one_line {
            let weekly_reset_iso = render::format_epoch_local_datetime(entry.weekly_reset_epoch)
                .unwrap_or_else(|| "?".to_string());
            println!(
                "{}:{}% W:{}% {}{}",
                entry.non_weekly_label,
                entry.non_weekly_remaining,
                entry.weekly_remaining,
                weekly_reset_iso,
                stale_suffix
            );
            return Ok(0);
        }

        let weekly_reset = render::format_epoch_local_datetime(entry.weekly_reset_epoch)
            .unwrap_or_else(|| "?".to_string());
        let non_weekly_reset = entry
            .non_weekly_reset_epoch
            .and_then(render::format_epoch_local_datetime)
            .unwrap_or_else(|| "?".to_string());
        println!("Rate limits remaining{stale_suffix}");
        println!(
            "{} {}% • {}",
            entry.non_weekly_label, entry.non_weekly_remaining, non_weekly_reset
        );
        println!("Weekly {}% • {}", entry.weekly_remaining, weekly_reset);
        return Ok(0);
    }

    if output_json {
        let result = json_result_no_window(target_file);
        diag_output::emit_json(&RateLimitSingleEnvelope {
            schema_version: DIAG_SCHEMA_VERSION.to_string(),
            command: DIAG_COMMAND.to_string(),
            mode: "single".to_string(),
            ok: true,
            result,
        })?;
        return Ok(0);
    }

    println!("No active rate-limit window");
    Ok(0)
}

fn single_one_line(
    target_file: &Path,
    cached_mode: bool,
    no_refresh_auth: bool,
    debug_mode: bool,
) -> Result<Option<String>> {
    if !target_file.is_file() {
        if debug_mode {
            eprintln!("codex-rate-limits: target file not found");
        }
        return Ok(None);
    }

    if cached_mode {
        return match cache::read_cache_entry_for_cached_mode(target_file) {
            Ok(entry) => {
                let weekly_reset_iso =
                    render::format_epoch_local_datetime(entry.weekly_reset_epoch)
                        .unwrap_or_else(|| "?".to_string());
                Ok(Some(format!(
                    "{}:{}% W:{}% {}",
                    entry.non_weekly_label,
                    entry.non_weekly_remaining,
                    entry.weekly_remaining,
                    weekly_reset_iso
                )))
            }
            Err(err) => {
                if debug_mode {
                    eprintln!("{err}");
                }
                Ok(None)
            }
        };
    }

    let base_url = std::env::var("CODEX_CHATGPT_BASE_URL")
        .unwrap_or_else(|_| "https://chatgpt.com/backend-api/".to_string());
    let connect_timeout = env_timeout("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", 2);
    let max_time = env_timeout("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", 8);

    let usage_request = UsageRequest {
        target_file: target_file.to_path_buf(),
        refresh_on_401: refresh_on_401_enabled(no_refresh_auth),
        suppress_auth_refresh_output: false,
        base_url,
        connect_timeout_seconds: connect_timeout,
        max_time_seconds: max_time,
    };

    let usage = match fetch_usage(&usage_request) {
        Ok(value) => value,
        Err(err) => {
            if debug_mode {
                eprintln!("{err}");
            }
            return Ok(None);
        }
    };

    if should_writeback_usage(target_file) {
        let _ = writeback::write_weekly(target_file, &usage.json);
    }
    if is_auth_file(target_file) {
        let _ = auth::sync::run();
    }

    let usage_data = match render::parse_usage(&usage.json) {
        Some(value) => value,
        None => return Ok(None),
    };
    let values = render::render_values(&usage_data);
    let weekly = render::weekly_values(&values);
    let fetched_at_epoch = Utc::now().timestamp();
    if fetched_at_epoch > 0 {
        let _ = cache::write_prompt_segment_cache(
            target_file,
            fetched_at_epoch,
            &weekly.non_weekly_label,
            weekly.non_weekly_remaining,
            weekly.weekly_remaining,
            weekly.weekly_reset_epoch,
            weekly.non_weekly_reset_epoch,
        );
    }
    let weekly_reset_iso = render::format_epoch_local_datetime(weekly.weekly_reset_epoch)
        .unwrap_or_else(|| "?".to_string());

    Ok(Some(format!(
        "{}:{}% W:{}% {}",
        weekly.non_weekly_label,
        weekly.non_weekly_remaining,
        weekly.weekly_remaining,
        weekly_reset_iso
    )))
}

fn resolve_target(secret: Option<&str>) -> std::result::Result<PathBuf, i32> {
    if let Some(secret_name) = secret {
        if secret_name.is_empty() || secret_name.contains('/') || secret_name.contains("..") {
            eprintln!("codex-rate-limits: invalid secret file name");
            return Err(64);
        }
        let secret_dir = crate::paths::resolve_secret_dir().unwrap_or_default();
        return Ok(secret_dir.join(secret_name));
    }

    if let Some(auth_file) = existing_active_auth_file() {
        return Ok(auth_file);
    }

    if let Some(auth_file) = official_codex_auth_file() {
        return Ok(auth_file);
    }

    if let Some(auth_file) = crate::paths::resolve_auth_file() {
        return Ok(auth_file);
    }

    Err(1)
}

fn is_auth_file(target_file: &Path) -> bool {
    if let Some(auth_file) = crate::paths::resolve_auth_file() {
        return auth_file == target_file;
    }
    false
}

fn env_timeout(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(default)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowState {
    /// No value available (genuine fetch failure with no cache to fall back to).
    Missing,
    /// Fresh live (or within-TTL cache) values.
    Filled,
    /// Last-known values served from cache past the freshness TTL.
    Stale,
    /// Backend reported no active rate-limit window and no cache was available.
    NoWindow,
}

struct Row {
    name: String,
    window_label: String,
    non_weekly_remaining: i64,
    non_weekly_reset_epoch: Option<i64>,
    weekly_remaining: i64,
    weekly_reset_epoch: Option<i64>,
    weekly_reset_iso: String,
    state: RowState,
}

impl Row {
    fn empty(name: String) -> Self {
        Self {
            name,
            window_label: String::new(),
            non_weekly_remaining: -1,
            non_weekly_reset_epoch: None,
            weekly_remaining: -1,
            weekly_reset_epoch: None,
            weekly_reset_iso: String::new(),
            state: RowState::Missing,
        }
    }

    fn sort_key(&self) -> (i32, i64, String) {
        if let Some(epoch) = self.weekly_reset_epoch {
            (0, epoch, self.name.clone())
        } else {
            (1, i64::MAX, self.name.clone())
        }
    }
}

struct ParsedOneLine {
    window_label: String,
    non_weekly_remaining: i64,
    weekly_remaining: i64,
    weekly_reset_iso: String,
}

fn parse_one_line_output(line: &str) -> Option<ParsedOneLine> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }

    fn parse_fields(
        window_field: &str,
        weekly_field: &str,
        reset_iso: String,
    ) -> Option<ParsedOneLine> {
        let window_label = window_field
            .split(':')
            .next()?
            .trim_matches('"')
            .to_string();
        let non_weekly_remaining = window_field.split(':').nth(1)?;
        let non_weekly_remaining = non_weekly_remaining
            .trim_end_matches('%')
            .parse::<i64>()
            .ok()?;

        let weekly_remaining = weekly_field.trim_start_matches("W:").trim_end_matches('%');
        let weekly_remaining = weekly_remaining.parse::<i64>().ok()?;

        Some(ParsedOneLine {
            window_label,
            non_weekly_remaining,
            weekly_remaining,
            weekly_reset_iso: reset_iso,
        })
    }

    let len = parts.len();
    let window_field = parts[len - 3];
    let weekly_field = parts[len - 2];
    let reset_iso = parts[len - 1].to_string();

    if let Some(parsed) = parse_fields(window_field, weekly_field, reset_iso) {
        return Some(parsed);
    }

    if len < 4 {
        return None;
    }

    parse_fields(
        parts[len - 4],
        parts[len - 3],
        format!("{} {}", parts[len - 2], parts[len - 1]),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        async_fetch_one_line, cache, collect_json_from_cache, collect_secret_files,
        collect_secret_files_for_async_text, current_secret_basename, env_timeout,
        fetch_one_line_cached, is_auth_file, normalize_one_line, parse_one_line_output,
        redact_sensitive_json, resolve_target, secret_display_name, single_one_line,
        sync_auth_silent, target_file_name,
    };
    use chrono::Utc;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use serde_json::json;
    use std::fs;
    use std::path::Path;

    const HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
    const PAYLOAD_ALPHA: &str = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";
    const PAYLOAD_BETA: &str = "eyJzdWIiOiJ1c2VyXzQ1NiIsImVtYWlsIjoiYmV0YUBleGFtcGxlLmNvbSIsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X3VzZXJfaWQiOiJ1c2VyXzQ1NiIsImVtYWlsIjoiYmV0YUBleGFtcGxlLmNvbSJ9fQ";

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

    fn fresh_fetched_at() -> i64 {
        Utc::now().timestamp()
    }

    #[test]
    fn redact_sensitive_json_removes_tokens_recursively() {
        let input = json!({
            "tokens": {
                "access_token": "a",
                "refresh_token": "b",
                "nested": {
                    "id_token": "c",
                    "Authorization": "Bearer x",
                    "ok": 1
                }
            },
            "items": [
                {"authorization": "Bearer y", "value": 2}
            ],
            "safe": true
        });

        let redacted = redact_sensitive_json(&input);
        assert_eq!(redacted["tokens"]["nested"]["ok"], 1);
        assert_eq!(redacted["safe"], true);
        assert!(
            redacted["tokens"].get("access_token").is_none(),
            "access_token should be removed"
        );
        assert!(
            redacted["tokens"]["nested"].get("id_token").is_none(),
            "id_token should be removed"
        );
        assert!(
            redacted["tokens"]["nested"].get("Authorization").is_none(),
            "Authorization should be removed"
        );
        assert!(
            redacted["items"][0].get("authorization").is_none(),
            "authorization should be removed"
        );
    }

    #[test]
    fn collect_secret_files_reports_missing_secret_dir() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let missing = dir.path().join("missing");
        let _secret = EnvGuard::set(
            &lock,
            "CODEX_SECRET_DIR",
            missing.to_str().expect("missing path"),
        );

        let err = collect_secret_files().expect_err("expected missing dir error");
        assert_eq!(err.0, 1);
        assert!(err.1.contains("CODEX_SECRET_DIR not found"));
    }

    #[test]
    fn collect_secret_files_returns_sorted_json_files_only() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secrets = dir.path().join("secrets");
        fs::create_dir_all(&secrets).expect("secrets dir");
        fs::write(secrets.join("beta.json"), "{}").expect("write beta");
        fs::write(secrets.join("alpha.json"), "{}").expect("write alpha");
        fs::write(secrets.join("note.txt"), "ignore").expect("write note");
        let _secret = EnvGuard::set(
            &lock,
            "CODEX_SECRET_DIR",
            secrets.to_str().expect("secrets path"),
        );

        let files = collect_secret_files().expect("secret files");
        assert_eq!(files.len(), 2);
        assert_eq!(
            files[0].file_name().and_then(|name| name.to_str()),
            Some("alpha.json")
        );
        assert_eq!(
            files[1].file_name().and_then(|name| name.to_str()),
            Some("beta.json")
        );
    }

    #[test]
    fn collect_secret_files_for_async_text_allows_empty_secret_dir() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        let _secret = EnvGuard::set(
            &lock,
            "CODEX_SECRET_DIR",
            secret_dir.to_str().expect("secret"),
        );

        let files = collect_secret_files_for_async_text().expect("async text secret files");
        assert!(files.is_empty());
    }

    #[test]
    fn rate_limits_helper_env_timeout_supports_default_and_parse() {
        let lock = GlobalStateLock::new();
        let key = "CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS";

        let _removed = EnvGuard::remove(&lock, key);
        assert_eq!(env_timeout(key, 7), 7);

        let _set = EnvGuard::set(&lock, key, "11");
        assert_eq!(env_timeout(key, 7), 11);

        let _invalid = EnvGuard::set(&lock, key, "oops");
        assert_eq!(env_timeout(key, 7), 7);
    }

    #[test]
    fn rate_limits_helper_resolve_target_and_is_auth_file() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        fs::create_dir_all(&secret_dir).expect("secret dir");
        let auth_file = dir.path().join("auth.json");
        fs::write(&auth_file, "{}").expect("auth");

        let _secret = EnvGuard::set(
            &lock,
            "CODEX_SECRET_DIR",
            secret_dir.to_str().expect("secret"),
        );
        let _auth = EnvGuard::set(&lock, "CODEX_AUTH_FILE", auth_file.to_str().expect("auth"));

        assert_eq!(
            resolve_target(Some("alpha.json")).expect("target"),
            secret_dir.join("alpha.json")
        );
        assert_eq!(resolve_target(Some("../bad")).expect_err("usage"), 64);
        assert_eq!(resolve_target(None).expect("auth default"), auth_file);
        assert!(is_auth_file(&auth_file));
        assert!(!is_auth_file(&secret_dir.join("alpha.json")));
    }

    #[test]
    fn rate_limits_helper_resolve_target_without_auth_returns_default_active_path() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir.path().join("home");
        let codex_home = dir.path().join("codex-home");
        let _auth = EnvGuard::remove(&lock, "CODEX_AUTH_FILE");
        let _codex_home = EnvGuard::set(&lock, "CODEX_HOME", codex_home.to_str().expect("home"));
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().expect("home"));

        assert_eq!(
            resolve_target(None).expect("default active auth"),
            home.join(".agents").join("auth.json")
        );
    }

    #[test]
    fn rate_limits_helper_collect_json_from_cache_covers_hit_and_miss() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache-root");
        fs::create_dir_all(&secret_dir).expect("secrets");
        fs::create_dir_all(&cache_root).expect("cache");

        let alpha = secret_dir.join("alpha.json");
        fs::write(&alpha, "{}").expect("alpha");

        let _secret = EnvGuard::set(
            &lock,
            "CODEX_SECRET_DIR",
            secret_dir.to_str().expect("secret"),
        );
        let _cache = EnvGuard::set(&lock, "ZSH_CACHE_DIR", cache_root.to_str().expect("cache"));
        cache::write_prompt_segment_cache(
            &alpha,
            fresh_fetched_at(),
            "3h",
            92,
            88,
            1_700_003_600,
            Some(1_700_001_200),
        )
        .expect("write cache");

        let hit = collect_json_from_cache(&alpha, "cache", true);
        assert!(hit.ok);
        assert_eq!(hit.status, "ok");
        let summary = hit.summary.expect("summary");
        assert_eq!(summary.non_weekly_label, "3h");
        assert_eq!(summary.non_weekly_remaining, 92);
        assert_eq!(summary.weekly_remaining, 88);

        let missing_target = secret_dir.join("missing.json");
        let miss = collect_json_from_cache(&missing_target, "cache", true);
        assert!(!miss.ok);
        let error = miss.error.expect("error");
        assert_eq!(error.code, "cache-read-failed");
        assert!(error.message.contains("cache not found"));
    }

    #[test]
    fn rate_limits_helper_fetch_one_line_cached_covers_success_and_error() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache-root");
        fs::create_dir_all(&secret_dir).expect("secrets");
        fs::create_dir_all(&cache_root).expect("cache");

        let alpha = secret_dir.join("alpha.json");
        fs::write(&alpha, "{}").expect("alpha");

        let _secret = EnvGuard::set(
            &lock,
            "CODEX_SECRET_DIR",
            secret_dir.to_str().expect("secret"),
        );
        let _cache = EnvGuard::set(&lock, "ZSH_CACHE_DIR", cache_root.to_str().expect("cache"));
        cache::write_prompt_segment_cache(
            &alpha,
            fresh_fetched_at(),
            "3h",
            70,
            55,
            1_700_003_600,
            Some(1_700_001_200),
        )
        .expect("write cache");

        let cached = fetch_one_line_cached(&alpha);
        assert_eq!(cached.rc, 0);
        assert!(cached.err.is_empty());
        assert!(cached.line.expect("line").contains("3h:70%"));

        let miss = fetch_one_line_cached(&secret_dir.join("beta.json"));
        assert_eq!(miss.rc, 1);
        assert!(miss.line.is_none());
        assert!(miss.err.contains("cache not found"));
    }

    #[test]
    fn rate_limits_helper_async_fetch_one_line_uses_cache_fallback() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache-root");
        fs::create_dir_all(&secret_dir).expect("secrets");
        fs::create_dir_all(&cache_root).expect("cache");

        let missing = secret_dir.join("ghost.json");
        let _secret = EnvGuard::set(
            &lock,
            "CODEX_SECRET_DIR",
            secret_dir.to_str().expect("secret"),
        );
        let _cache = EnvGuard::set(&lock, "ZSH_CACHE_DIR", cache_root.to_str().expect("cache"));
        cache::write_prompt_segment_cache(
            &missing,
            fresh_fetched_at(),
            "3h",
            68,
            42,
            1_700_003_600,
            Some(1_700_001_200),
        )
        .expect("write cache");

        let result = async_fetch_one_line(&missing, false, true, "ghost");
        assert_eq!(result.rc, 0);
        let line = result.line.expect("line");
        assert!(line.contains("3h:68%"));
        assert!(result.err.contains("falling back to cache"));
    }

    #[test]
    fn rate_limits_helper_single_one_line_cached_mode_handles_hit_and_miss() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_root = dir.path().join("cache-root");
        fs::create_dir_all(&secret_dir).expect("secrets");
        fs::create_dir_all(&cache_root).expect("cache");

        let alpha = secret_dir.join("alpha.json");
        let beta = secret_dir.join("beta.json");
        fs::write(&alpha, "{}").expect("alpha");
        fs::write(&beta, "{}").expect("beta");

        let _secret = EnvGuard::set(
            &lock,
            "CODEX_SECRET_DIR",
            secret_dir.to_str().expect("secret"),
        );
        let _cache = EnvGuard::set(&lock, "ZSH_CACHE_DIR", cache_root.to_str().expect("cache"));
        cache::write_prompt_segment_cache(
            &alpha,
            fresh_fetched_at(),
            "3h",
            61,
            39,
            1_700_003_600,
            Some(1_700_001_200),
        )
        .expect("write cache");

        let hit = single_one_line(&alpha, true, true, false).expect("single");
        assert!(hit.expect("line").contains("3h:61%"));

        let miss = single_one_line(&beta, true, true, true).expect("single");
        assert!(miss.is_none());

        let missing =
            single_one_line(&secret_dir.join("missing.json"), true, true, true).expect("single");
        assert!(missing.is_none());
    }

    #[test]
    fn rate_limits_helper_sync_auth_silent_updates_matching_secret_and_timestamps() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&secret_dir).expect("secrets");
        fs::create_dir_all(&cache_dir).expect("cache");

        let auth_file = dir.path().join("auth.json");
        let alpha = secret_dir.join("alpha.json");
        let beta = secret_dir.join("beta.json");
        fs::write(
            &auth_file,
            auth_json(
                PAYLOAD_ALPHA,
                "acct_001",
                "refresh_new",
                "2025-01-20T12:34:56Z",
            ),
        )
        .expect("auth");
        fs::write(
            &alpha,
            auth_json(
                PAYLOAD_ALPHA,
                "acct_001",
                "refresh_old",
                "2025-01-19T12:34:56Z",
            ),
        )
        .expect("alpha");
        fs::write(
            &beta,
            auth_json(
                PAYLOAD_BETA,
                "acct_002",
                "refresh_beta",
                "2025-01-18T12:34:56Z",
            ),
        )
        .expect("beta");
        fs::write(secret_dir.join("invalid.json"), "{invalid").expect("invalid");
        fs::write(secret_dir.join("note.txt"), "ignore").expect("note");

        let _auth = EnvGuard::set(&lock, "CODEX_AUTH_FILE", auth_file.to_str().expect("auth"));
        let _secret = EnvGuard::set(
            &lock,
            "CODEX_SECRET_DIR",
            secret_dir.to_str().expect("secret"),
        );
        let _cache = EnvGuard::set(
            &lock,
            "CODEX_SECRET_CACHE_DIR",
            cache_dir.to_str().expect("cache"),
        );

        let (rc, err) = sync_auth_silent().expect("sync");
        assert_eq!(rc, 0);
        assert!(err.is_none());
        assert_eq!(
            fs::read(&alpha).expect("alpha"),
            fs::read(&auth_file).expect("auth")
        );
        assert_ne!(
            fs::read(&beta).expect("beta"),
            fs::read(&auth_file).expect("auth")
        );
        assert!(cache_dir.join("alpha.json.timestamp").is_file());
        assert!(cache_dir.join("auth.json.timestamp").is_file());
    }

    #[test]
    fn rate_limits_helper_parsers_and_name_helpers_cover_fallbacks() {
        let parsed =
            parse_one_line_output("alpha 3h:90% W:80% 2025-01-20 12:00:00+00:00").expect("parsed");
        assert_eq!(parsed.window_label, "3h");
        assert_eq!(parsed.non_weekly_remaining, 90);
        assert_eq!(parsed.weekly_remaining, 80);
        assert_eq!(parsed.weekly_reset_iso, "2025-01-20 12:00:00+00:00");
        assert!(parse_one_line_output("bad").is_none());

        assert_eq!(normalize_one_line("a\tb\nc\r".to_string()), "a b c ");
        assert_eq!(target_file_name(Path::new("alpha.json")), "alpha.json");
        assert_eq!(target_file_name(Path::new("")), "");
        assert_eq!(secret_display_name(Path::new("alpha.json")), "alpha");
    }

    #[test]
    fn rate_limits_helper_current_secret_basename_tracks_auth_switch() {
        let lock = GlobalStateLock::new();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let secret_dir = dir.path().join("secrets");
        fs::create_dir_all(&secret_dir).expect("secrets");

        let auth_file = dir.path().join("auth.json");
        let alpha = secret_dir.join("alpha.json");
        let beta = secret_dir.join("beta.json");

        let alpha_json = auth_json(
            PAYLOAD_ALPHA,
            "acct_001",
            "refresh_alpha",
            "2025-01-20T12:34:56Z",
        );
        let beta_json = auth_json(
            PAYLOAD_BETA,
            "acct_002",
            "refresh_beta",
            "2025-01-21T12:34:56Z",
        );
        fs::write(&alpha, &alpha_json).expect("alpha");
        fs::write(&beta, &beta_json).expect("beta");
        fs::write(&auth_file, &alpha_json).expect("auth alpha");

        let _auth = EnvGuard::set(&lock, "CODEX_AUTH_FILE", auth_file.to_str().expect("auth"));

        let secret_files = vec![alpha.clone(), beta.clone()];
        assert_eq!(
            current_secret_basename(&secret_files).as_deref(),
            Some("alpha")
        );

        fs::write(&auth_file, &beta_json).expect("auth beta");
        assert_eq!(
            current_secret_basename(&secret_files).as_deref(),
            Some("beta")
        );
    }
}

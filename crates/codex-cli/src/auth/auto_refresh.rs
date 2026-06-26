use anyhow::Result;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

use crate::auth;
use crate::auth::output::{self, AuthAutoRefreshResult, AuthAutoRefreshTargetResult};
use crate::auth::remote;
use crate::paths;
use crate::provider_profile::CODEX_PROVIDER_PROFILE;
use nils_common::env as shared_env;
use nils_common::fs;

pub fn run() -> Result<i32> {
    run_with_json(false)
}

pub fn run_with_json(output_json: bool) -> Result<i32> {
    if !is_enabled() {
        if output_json {
            output::emit_result("auth auto-refresh", zero_result(false, 0))?;
        } else {
            println!(
                "codex-auto-refresh: disabled (set {}=true to enable)",
                CODEX_PROVIDER_PROFILE.env.auto_refresh_enabled
            );
        }
        return Ok(0);
    }

    let remote_config = match remote::configured_pull_from_env() {
        Ok(config) => config,
        Err(err) => {
            if output_json {
                output::emit_error(
                    "auth auto-refresh",
                    err.code,
                    err.message,
                    Some(err.details),
                )?;
            } else {
                eprintln!("{}", err.message);
            }
            return Ok(64);
        }
    };
    let remote_configured = remote_config.is_some();

    if !is_configured(remote_configured) {
        if output_json {
            output::emit_result("auth auto-refresh", zero_result(true, 0))?;
        }
        return Ok(0);
    }

    let min_days_raw =
        std::env::var("CODEX_AUTO_REFRESH_MIN_DAYS").unwrap_or_else(|_| "5".to_string());
    let min_days = match min_days_raw.parse::<i64>() {
        Ok(value) => value,
        Err(_) => {
            if output_json {
                output::emit_error(
                    "auth auto-refresh",
                    "invalid-min-days",
                    format!(
                        "codex-auto-refresh: invalid CODEX_AUTO_REFRESH_MIN_DAYS: {}",
                        min_days_raw
                    ),
                    Some(serde_json::json!({
                        "value": min_days_raw,
                    })),
                )?;
            } else {
                eprintln!(
                    "codex-auto-refresh: invalid CODEX_AUTO_REFRESH_MIN_DAYS: {}",
                    min_days_raw
                );
            }
            return Ok(64);
        }
    };

    let min_seconds = min_days.saturating_mul(86_400);
    let now_epoch = Utc::now().timestamp();

    let auth_file = paths::resolve_auth_file();
    if auth_file.is_some() {
        let sync_rc = auth::sync::run_with_json(false)?;
        if sync_rc != 0 {
            if output_json {
                output::emit_error(
                    "auth auto-refresh",
                    "sync-failed",
                    "codex-auto-refresh: failed to sync auth and secrets before refresh",
                    None,
                )?;
            }
            return Ok(1);
        }
    }

    let mut targets = Vec::new();
    if let Some(auth_file) = auth_file.as_ref() {
        targets.push(auth_file.clone());
    }
    if !remote_configured
        && let Some(secret_dir) = paths::resolve_secret_dir()
        && let Ok(entries) = std::fs::read_dir(&secret_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                targets.push(path);
            }
        }
    }

    let mut refreshed: i64 = 0;
    let mut skipped: i64 = 0;
    let mut failed: i64 = 0;
    let mut target_results: Vec<AuthAutoRefreshTargetResult> = Vec::new();

    for target in targets {
        let target_is_auth = auth_file.as_ref().map(|p| p == &target).unwrap_or(false);
        let missing_remote_auth = target_is_auth && remote_configured && !target.is_file();
        if !target.is_file() && !missing_remote_auth {
            if target_is_auth {
                skipped += 1;
                target_results.push(AuthAutoRefreshTargetResult {
                    target_file: target.display().to_string(),
                    status: "skipped".to_string(),
                    reason: Some("auth-file-missing".to_string()),
                });
                continue;
            }
            if !output_json {
                eprintln!("codex-auto-refresh: missing file: {}", target.display());
            }
            failed += 1;
            target_results.push(AuthAutoRefreshTargetResult {
                target_file: target.display().to_string(),
                status: "failed".to_string(),
                reason: Some("missing-file".to_string()),
            });
            continue;
        }

        let timestamp_path = timestamp_path(&target)?;
        let decision = if missing_remote_auth {
            RefreshDecision::Refresh
        } else {
            should_refresh(&target, &timestamp_path, now_epoch, min_seconds)
        };
        match decision {
            RefreshDecision::Refresh => {
                let rc = if target_is_auth {
                    if output_json {
                        auth::refresh::run_silent(&[])?
                    } else {
                        auth::refresh::run(&[])?
                    }
                } else {
                    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if output_json {
                        auth::refresh::run_silent(&[name.to_string()])?
                    } else {
                        auth::refresh::run(&[name.to_string()])?
                    }
                };
                if rc == 0 {
                    refreshed += 1;
                    target_results.push(AuthAutoRefreshTargetResult {
                        target_file: target.display().to_string(),
                        status: "refreshed".to_string(),
                        reason: None,
                    });
                } else {
                    failed += 1;
                    target_results.push(AuthAutoRefreshTargetResult {
                        target_file: target.display().to_string(),
                        status: "failed".to_string(),
                        reason: Some(format!("refresh-exit-{rc}")),
                    });
                }
            }
            RefreshDecision::Skip => {
                skipped += 1;
                target_results.push(AuthAutoRefreshTargetResult {
                    target_file: target.display().to_string(),
                    status: "skipped".to_string(),
                    reason: Some("not-due".to_string()),
                });
            }
            RefreshDecision::WarnFuture => {
                if !output_json {
                    eprintln!(
                        "codex-auto-refresh: warning: future timestamp for {}",
                        target.display()
                    );
                }
                skipped += 1;
                target_results.push(AuthAutoRefreshTargetResult {
                    target_file: target.display().to_string(),
                    status: "skipped".to_string(),
                    reason: Some("future-timestamp".to_string()),
                });
            }
        }
    }

    if output_json {
        output::emit_result(
            "auth auto-refresh",
            AuthAutoRefreshResult {
                enabled: true,
                refreshed,
                skipped,
                failed,
                min_age_days: min_days,
                targets: target_results,
            },
        )?;
    } else {
        println!(
            "codex-auto-refresh: refreshed={} skipped={} failed={} (min_age_days={})",
            refreshed, skipped, failed, min_days
        );
    }

    if failed > 0 {
        return Ok(1);
    }

    Ok(0)
}

fn is_enabled() -> bool {
    shared_env::env_truthy(CODEX_PROVIDER_PROFILE.env.auto_refresh_enabled)
}

fn zero_result(enabled: bool, min_age_days: i64) -> AuthAutoRefreshResult {
    AuthAutoRefreshResult {
        enabled,
        refreshed: 0,
        skipped: 0,
        failed: 0,
        min_age_days,
        targets: Vec::new(),
    }
}

fn is_configured(remote_configured: bool) -> bool {
    if remote_configured && paths::resolve_auth_file().is_some() {
        return true;
    }

    let mut candidates = Vec::new();
    if let Some(auth_file) = paths::resolve_auth_file() {
        candidates.push(auth_file);
    }
    if let Some(secret_dir) = paths::resolve_secret_dir()
        && let Ok(entries) = std::fs::read_dir(&secret_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                candidates.push(path);
            }
        }
    }

    candidates.iter().any(|path| path.is_file())
}

enum RefreshDecision {
    Refresh,
    Skip,
    WarnFuture,
}

fn should_refresh(
    target: &Path,
    timestamp_path: &Path,
    now_epoch: i64,
    min_seconds: i64,
) -> RefreshDecision {
    if let Some(last_epoch) = last_refresh_epoch(target, timestamp_path) {
        let age = now_epoch - last_epoch;
        if age < 0 {
            return RefreshDecision::WarnFuture;
        }
        if age >= min_seconds {
            RefreshDecision::Refresh
        } else {
            RefreshDecision::Skip
        }
    } else {
        RefreshDecision::Refresh
    }
}

fn last_refresh_epoch(target: &Path, timestamp_path: &Path) -> Option<i64> {
    if let Ok(content) = std::fs::read_to_string(timestamp_path) {
        let iso = normalize_iso(&content);
        if let Some(epoch) = iso_to_epoch(&iso) {
            return Some(epoch);
        }
    }

    let iso = auth::last_refresh_from_auth_file(target).ok().flatten()?;
    let iso = normalize_iso(&iso);
    let epoch = iso_to_epoch(&iso)?;
    let _ = fs::write_timestamp(timestamp_path, Some(&iso));
    Some(epoch)
}

fn normalize_iso(raw: &str) -> String {
    let mut trimmed = raw
        .split(&['\n', '\r'][..])
        .next()
        .unwrap_or("")
        .to_string();
    if let Some(dot) = trimmed.find('.')
        && trimmed.ends_with('Z')
    {
        trimmed.truncate(dot);
        trimmed.push('Z');
    }
    trimmed
}

fn iso_to_epoch(iso: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|dt| dt.timestamp())
}

fn timestamp_path(target: &Path) -> Result<PathBuf> {
    let cache_dir = paths::resolve_secret_cache_dir()
        .ok_or_else(|| anyhow::anyhow!("CODEX_SECRET_CACHE_DIR not resolved"))?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("auth.json");
    Ok(cache_dir.join(format!("{name}.timestamp")))
}

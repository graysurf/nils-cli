use std::path::{Path, PathBuf};

use crate::auth;
use crate::auth::output;
use crate::provider_profile::GEMINI_PROVIDER_PROFILE;
use nils_common::env as shared_env;

pub fn run() -> i32 {
    run_with_json(false)
}

pub fn run_with_json(output_json: bool) -> i32 {
    if !is_enabled() {
        if output_json {
            let _ = output::emit_result("auth auto-refresh", zero_result(false, 0));
        } else {
            println!(
                "gemini-auto-refresh: disabled (set {}=true to enable)",
                GEMINI_PROVIDER_PROFILE.env.auto_refresh_enabled
            );
        }
        return 0;
    }

    if !is_configured() {
        if output_json {
            let _ = output::emit_result("auth auto-refresh", zero_result(true, 0));
        }
        return 0;
    }

    let min_days_raw =
        std::env::var("GEMINI_AUTO_REFRESH_MIN_DAYS").unwrap_or_else(|_| "5".to_string());
    let min_days = match min_days_raw.parse::<i64>() {
        Ok(value) => value,
        Err(_) => {
            if output_json {
                let _ = output::emit_error(
                    "auth auto-refresh",
                    "invalid-min-days",
                    format!(
                        "gemini-auto-refresh: invalid GEMINI_AUTO_REFRESH_MIN_DAYS: {}",
                        min_days_raw
                    ),
                    Some(output::obj(vec![("value", output::s(min_days_raw))])),
                );
            } else {
                eprintln!(
                    "gemini-auto-refresh: invalid GEMINI_AUTO_REFRESH_MIN_DAYS: {}",
                    min_days_raw
                );
            }
            return 64;
        }
    };

    let min_seconds = min_days.saturating_mul(86_400);
    let now_epoch = auth::now_epoch_seconds();

    let auth_file = crate::paths::resolve_auth_file();
    if auth_file.is_some() {
        let sync_rc = auth::sync::run_with_json(false);
        if sync_rc != 0 {
            if output_json {
                let _ = output::emit_error(
                    "auth auto-refresh",
                    "sync-failed",
                    "gemini-auto-refresh: failed to sync auth and secrets before refresh",
                    None,
                );
            }
            return 1;
        }
    }

    let mut targets = Vec::new();
    if let Some(auth_file) = auth_file.as_ref() {
        targets.push(auth_file.clone());
    }
    if let Some(secret_dir) = crate::paths::resolve_secret_dir()
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
    let mut target_results: Vec<output::JsonValue> = Vec::new();

    for target in targets {
        if !target.is_file() {
            if auth_file.as_ref().map(|p| p == &target).unwrap_or(false) {
                skipped += 1;
                target_results.push(target_result(&target, "skipped", Some("auth-file-missing")));
                continue;
            }
            if !output_json {
                eprintln!("gemini-auto-refresh: missing file: {}", target.display());
            }
            failed += 1;
            target_results.push(target_result(&target, "failed", Some("missing-file")));
            continue;
        }

        let timestamp_path = timestamp_path(&target);
        match should_refresh(&target, timestamp_path.as_deref(), now_epoch, min_seconds) {
            RefreshDecision::Refresh => {
                let rc = if auth_file.as_ref().map(|p| p == &target).unwrap_or(false) {
                    if output_json {
                        auth::refresh::run_silent(&[])
                    } else {
                        auth::refresh::run(&[])
                    }
                } else {
                    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if output_json {
                        auth::refresh::run_silent(&[name.to_string()])
                    } else {
                        auth::refresh::run(&[name.to_string()])
                    }
                };

                if rc == 0 {
                    refreshed += 1;
                    target_results.push(target_result(&target, "refreshed", None));
                } else {
                    failed += 1;
                    target_results.push(target_result(
                        &target,
                        "failed",
                        Some(&format!("refresh-exit-{rc}")),
                    ));
                }
            }
            RefreshDecision::Skip => {
                skipped += 1;
                target_results.push(target_result(&target, "skipped", Some("not-due")));
            }
            RefreshDecision::WarnFuture => {
                if !output_json {
                    eprintln!(
                        "gemini-auto-refresh: warning: future timestamp for {}",
                        target.display()
                    );
                }
                skipped += 1;
                target_results.push(target_result(&target, "skipped", Some("future-timestamp")));
            }
        }
    }

    if output_json {
        let _ = output::emit_result(
            "auth auto-refresh",
            auto_refresh_result(true, refreshed, skipped, failed, min_days, target_results),
        );
    } else {
        println!(
            "gemini-auto-refresh: refreshed={} skipped={} failed={} (min_age_days={})",
            refreshed, skipped, failed, min_days
        );
    }

    if failed > 0 {
        return 1;
    }

    0
}

fn is_enabled() -> bool {
    shared_env::env_truthy(GEMINI_PROVIDER_PROFILE.env.auto_refresh_enabled)
}

fn zero_result(enabled: bool, min_age_days: i64) -> output::JsonValue {
    auto_refresh_result(enabled, 0, 0, 0, min_age_days, Vec::new())
}

fn auto_refresh_result(
    enabled: bool,
    refreshed: i64,
    skipped: i64,
    failed: i64,
    min_age_days: i64,
    targets: Vec<output::JsonValue>,
) -> output::JsonValue {
    output::obj(vec![
        ("enabled", output::b(enabled)),
        ("refreshed", output::n(refreshed)),
        ("skipped", output::n(skipped)),
        ("failed", output::n(failed)),
        ("min_age_days", output::n(min_age_days)),
        ("targets", output::arr(targets)),
    ])
}

fn target_result(target: &Path, status: &str, reason: Option<&str>) -> output::JsonValue {
    let mut fields = vec![
        (
            "target_file".to_string(),
            output::s(target.display().to_string()),
        ),
        ("status".to_string(), output::s(status)),
    ];
    if let Some(reason) = reason {
        fields.push(("reason".to_string(), output::s(reason)));
    }
    output::obj_dynamic(fields)
}

fn is_configured() -> bool {
    let mut candidates = Vec::new();
    if let Some(auth_file) = crate::paths::resolve_auth_file() {
        candidates.push(auth_file);
    }
    if let Some(secret_dir) = crate::paths::resolve_secret_dir()
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
    timestamp_path: Option<&Path>,
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

fn last_refresh_epoch(target: &Path, timestamp_path: Option<&Path>) -> Option<i64> {
    if let Some(path) = timestamp_path
        && let Ok(content) = std::fs::read_to_string(path)
    {
        let iso = auth::normalize_iso(&content);
        if let Some(epoch) = auth::parse_rfc3339_epoch(&iso) {
            return Some(epoch);
        }
    }

    let iso = auth::last_refresh_from_auth_file(target).ok().flatten()?;
    let iso = auth::normalize_iso(&iso);
    let epoch = auth::parse_rfc3339_epoch(&iso)?;
    if let Some(path) = timestamp_path {
        let _ = auth::write_timestamp(path, Some(&iso));
    }
    Some(epoch)
}

fn timestamp_path(target: &Path) -> Option<PathBuf> {
    let cache_dir = crate::paths::resolve_secret_cache_dir()?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("auth.json");
    Some(cache_dir.join(format!("{name}.timestamp")))
}

#[cfg(test)]
mod tests {
    use super::{
        RefreshDecision, is_configured, last_refresh_epoch, run_with_json, should_refresh,
        timestamp_path,
    };
    use crate::auth;
    use nils_test_support::fs as test_fs;
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn set_env(lock: &GlobalStateLock, key: &str, value: impl AsRef<OsStr>) -> EnvGuard {
        let value = value.as_ref().to_string_lossy().into_owned();
        EnvGuard::set(lock, key, &value)
    }

    fn write_auth(target: &Path, last_refresh: &str) {
        test_fs::write_text(target, &format!("{{\"last_refresh\":\"{last_refresh}\"}}"));
    }

    #[test]
    fn run_with_json_returns_zero_when_not_configured() {
        let lock = GlobalStateLock::new();
        let dir = TempDir::new().expect("tempdir");
        let _auth = set_env(
            &lock,
            "GEMINI_AUTH_FILE",
            dir.path().join("missing-auth.json"),
        );
        let _secret = set_env(
            &lock,
            "GEMINI_SECRET_DIR",
            dir.path().join("missing-secrets"),
        );
        let _enabled = set_env(&lock, "GEMINI_AUTO_REFRESH_ENABLED", "true");
        assert_eq!(run_with_json(true), 0);
        assert_eq!(run_with_json(false), 0);
    }

    #[test]
    fn run_with_json_invalid_min_days_returns_64() {
        let lock = GlobalStateLock::new();
        let dir = TempDir::new().expect("tempdir");
        let secrets = dir.path().join("secrets");
        fs::create_dir_all(&secrets).expect("secrets");
        write_auth(&secrets.join("alpha.json"), "2026-01-01T00:00:00Z");

        let _auth = set_env(
            &lock,
            "GEMINI_AUTH_FILE",
            dir.path().join("missing-auth.json"),
        );
        let _secret = set_env(&lock, "GEMINI_SECRET_DIR", &secrets);
        let _enabled = set_env(&lock, "GEMINI_AUTO_REFRESH_ENABLED", "true");
        let _min_days = set_env(&lock, "GEMINI_AUTO_REFRESH_MIN_DAYS", "bogus");

        assert_eq!(run_with_json(true), 64);
        assert_eq!(run_with_json(false), 64);
    }

    #[test]
    fn should_refresh_covers_refresh_skip_and_future() {
        let dir = TempDir::new().expect("tempdir");
        let auth_file = dir.path().join("auth.json");
        write_auth(&auth_file, "2026-01-01T00:00:00Z");
        let last_epoch = auth::parse_rfc3339_epoch("2026-01-01T00:00:00Z").expect("epoch");

        assert!(matches!(
            should_refresh(&auth_file, None, last_epoch + 86_400, 86_400),
            RefreshDecision::Refresh
        ));
        assert!(matches!(
            should_refresh(&auth_file, None, last_epoch + 100, 86_400),
            RefreshDecision::Skip
        ));
        assert!(matches!(
            should_refresh(&auth_file, None, last_epoch - 1, 86_400),
            RefreshDecision::WarnFuture
        ));
    }

    #[test]
    fn last_refresh_epoch_prefers_timestamp_and_backfills_when_needed() {
        let dir = TempDir::new().expect("tempdir");
        let auth_file = dir.path().join("auth.json");
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&cache_dir).expect("cache dir");
        let ts_file = cache_dir.join("auth.json.timestamp");
        write_auth(&auth_file, "2026-01-01T00:00:00Z");

        test_fs::write_text(&ts_file, "2026-01-02T00:00:00Z");
        let from_timestamp =
            last_refresh_epoch(&auth_file, Some(&ts_file)).expect("epoch from timestamp");
        let expected_from_ts = auth::parse_rfc3339_epoch("2026-01-02T00:00:00Z").expect("epoch");
        assert_eq!(from_timestamp, expected_from_ts);

        test_fs::write_text(&ts_file, "not-an-iso");
        let from_auth = last_refresh_epoch(&auth_file, Some(&ts_file)).expect("epoch from auth");
        let expected_from_auth = auth::parse_rfc3339_epoch("2026-01-01T00:00:00Z").expect("epoch");
        assert_eq!(from_auth, expected_from_auth);
        assert!(
            fs::read_to_string(&ts_file)
                .expect("read backfilled")
                .contains("2026-01-01")
        );
    }

    #[test]
    fn is_configured_detects_auth_or_secret_files() {
        let lock = GlobalStateLock::new();
        let dir = TempDir::new().expect("tempdir");
        let auth_file = dir.path().join("auth.json");
        let secrets = dir.path().join("secrets");
        fs::create_dir_all(&secrets).expect("secrets");

        let missing_auth = dir.path().join("missing-auth.json");
        let _auth = set_env(&lock, "GEMINI_AUTH_FILE", &missing_auth);
        let _secret = set_env(&lock, "GEMINI_SECRET_DIR", &secrets);
        assert!(!is_configured());

        write_auth(&auth_file, "2026-01-01T00:00:00Z");
        let _auth = set_env(&lock, "GEMINI_AUTH_FILE", &auth_file);
        assert!(is_configured());

        let _auth = set_env(&lock, "GEMINI_AUTH_FILE", &missing_auth);
        write_auth(&secrets.join("alpha.json"), "2026-01-01T00:00:00Z");
        assert!(is_configured());
    }

    #[test]
    fn timestamp_path_uses_secret_cache_dir() {
        let lock = GlobalStateLock::new();
        let dir = TempDir::new().expect("tempdir");
        let cache_root = dir.path().join("cache");
        fs::create_dir_all(&cache_root).expect("cache root");
        let _cache = set_env(&lock, "GEMINI_SECRET_CACHE_DIR", &cache_root);
        let path = timestamp_path(Path::new("/tmp/alpha.json")).expect("timestamp path");
        assert_eq!(path, cache_root.join("alpha.json.timestamp"));
    }

    #[test]
    fn run_with_json_reports_failed_for_missing_file_like_target() {
        let lock = GlobalStateLock::new();
        let dir = TempDir::new().expect("tempdir");
        let secrets = dir.path().join("secrets");
        fs::create_dir_all(&secrets).expect("secrets");

        write_auth(&secrets.join("good.json"), "2100-01-01T00:00:00Z");
        fs::create_dir_all(secrets.join("broken.json")).expect("broken json dir");

        let _auth = set_env(
            &lock,
            "GEMINI_AUTH_FILE",
            dir.path().join("missing-auth.json"),
        );
        let _secret = set_env(&lock, "GEMINI_SECRET_DIR", &secrets);
        let _enabled = set_env(&lock, "GEMINI_AUTO_REFRESH_ENABLED", "true");
        let _min_days = set_env(&lock, "GEMINI_AUTO_REFRESH_MIN_DAYS", "5");
        assert_eq!(run_with_json(false), 1);
    }

    #[test]
    fn run_with_json_emits_summary_when_targets_are_skipped() {
        let lock = GlobalStateLock::new();
        let dir = TempDir::new().expect("tempdir");
        let secrets = dir.path().join("secrets");
        fs::create_dir_all(&secrets).expect("secrets");
        write_auth(&secrets.join("alpha.json"), "2026-01-01T00:00:00Z");

        let _auth = set_env(
            &lock,
            "GEMINI_AUTH_FILE",
            dir.path().join("missing-auth.json"),
        );
        let _secret = set_env(&lock, "GEMINI_SECRET_DIR", &secrets);
        let _enabled = set_env(&lock, "GEMINI_AUTO_REFRESH_ENABLED", "true");
        let _min_days = set_env(&lock, "GEMINI_AUTO_REFRESH_MIN_DAYS", "99999");
        assert_eq!(run_with_json(true), 0);
    }
}

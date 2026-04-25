use gemini_cli::rate_limits;
use nils_test_support::{EnvGuard, GlobalStateLock};

use std::fs as stdfs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "nils-gemini-cli-{label}-{}-{nanos}",
            std::process::id()
        ));
        let _ = stdfs::remove_dir_all(&path);
        stdfs::create_dir_all(&path).expect("temp dir");
        Self { path }
    }

    fn join(&self, child: &str) -> PathBuf {
        self.path.join(child)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = stdfs::remove_dir_all(&self.path);
    }
}

fn write_secret(path: PathBuf, with_access_token: bool) {
    let payload = if with_access_token {
        r#"{"tokens":{"access_token":"tok","account_id":"acct_001"}}"#
    } else {
        r#"{"tokens":{"account_id":"acct_001"}}"#
    };
    stdfs::write(path, payload).expect("write secret");
}

fn write_cache_for_target(target: &std::path::Path, remaining: i64) {
    let cache = rate_limits::cache_file_for_target(target).expect("cache file");
    if let Some(parent) = cache.parent() {
        stdfs::create_dir_all(parent).expect("cache parent");
    }
    stdfs::write(
        cache,
        format!(
            "fetched_at=1700000000\nnon_weekly_label=5h\nnon_weekly_remaining={remaining}\nweekly_remaining=70\nweekly_reset_epoch=1700600000\n"
        ),
    )
    .expect("write cache");
}

#[test]
fn rate_limits_async_one_line_conflict_returns_64() {
    let _lock = GlobalStateLock::new();
    let options = rate_limits::RateLimitsOptions {
        async_mode: true,
        one_line: true,
        ..Default::default()
    };
    assert_eq!(rate_limits::run(&options), 64);
}

#[test]
fn rate_limits_async_positional_secret_conflict_returns_64() {
    let _lock = GlobalStateLock::new();
    let options = rate_limits::RateLimitsOptions {
        async_mode: true,
        secret: Some("alpha.json".to_string()),
        ..Default::default()
    };
    assert_eq!(rate_limits::run(&options), 64);
}

#[test]
fn rate_limits_async_cached_clear_conflict_returns_64() {
    let _lock = GlobalStateLock::new();
    let options = rate_limits::RateLimitsOptions {
        async_mode: true,
        cached: true,
        clear_cache: true,
        ..Default::default()
    };
    assert_eq!(rate_limits::run(&options), 64);
}

#[test]
fn rate_limits_async_clear_cache_non_absolute_root_returns_1() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("rate-limits-async-clear-cache-invalid-root");
    let secrets = dir.join("secrets");
    stdfs::create_dir_all(&secrets).expect("secrets");
    let secrets = stdfs::canonicalize(&secrets).expect("canonical secrets");

    let secrets_env = secrets.display().to_string();
    let _secret_dir = EnvGuard::set(&lock, "GEMINI_SECRET_DIR", &secrets_env);
    let _cache_root = EnvGuard::set(&lock, "ZSH_CACHE_DIR", "relative-cache");

    let options = rate_limits::RateLimitsOptions {
        async_mode: true,
        clear_cache: true,
        ..Default::default()
    };
    assert_eq!(rate_limits::run(&options), 1);
}

#[test]
fn rate_limits_async_missing_secret_dir_returns_1() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("rate-limits-async-missing-secret-dir");
    let missing = dir.join("missing");

    let missing_env = missing.display().to_string();
    let _secret_dir = EnvGuard::set(&lock, "GEMINI_SECRET_DIR", &missing_env);
    let options = rate_limits::RateLimitsOptions {
        async_mode: true,
        ..Default::default()
    };
    assert_eq!(rate_limits::run(&options), 1);
}

#[test]
fn rate_limits_async_invalid_jobs_value_defaults_without_usage_error() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("rate-limits-async-invalid-jobs");
    let missing = dir.join("missing");

    let missing_env = missing.display().to_string();
    let _secret_dir = EnvGuard::set(&lock, "GEMINI_SECRET_DIR", &missing_env);
    let options = rate_limits::RateLimitsOptions {
        async_mode: true,
        jobs: Some("bogus".to_string()),
        ..Default::default()
    };
    assert_eq!(rate_limits::run(&options), 1);
}

#[test]
fn rate_limits_async_cached_success_returns_0() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("rate-limits-async-cached-success");

    let secrets = dir.join("secrets");
    stdfs::create_dir_all(&secrets).expect("secrets");
    let alpha = secrets.join("alpha.json");
    let beta = secrets.join("beta.json");
    write_secret(alpha.clone(), true);
    write_secret(beta.clone(), true);
    let secrets = stdfs::canonicalize(&secrets).expect("canonical secrets");
    let alpha = secrets.join("alpha.json");
    let beta = secrets.join("beta.json");

    let cache_root = dir.join("cache-root");
    stdfs::create_dir_all(&cache_root).expect("cache root");

    let secrets_env = secrets.display().to_string();
    let cache_root_env = cache_root.display().to_string();
    let _secret_dir = EnvGuard::set(&lock, "GEMINI_SECRET_DIR", &secrets_env);
    let _cache_root = EnvGuard::set(&lock, "ZSH_CACHE_DIR", &cache_root_env);

    write_cache_for_target(&alpha, 90);
    write_cache_for_target(&beta, 91);

    let options = rate_limits::RateLimitsOptions {
        async_mode: true,
        cached: true,
        ..Default::default()
    };
    assert_eq!(rate_limits::run(&options), 0);
}

#[test]
fn rate_limits_async_json_missing_access_token_uses_cache_fallback() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("rate-limits-async-json-cache-fallback");

    let secrets = dir.join("secrets");
    stdfs::create_dir_all(&secrets).expect("secrets");
    let alpha = secrets.join("alpha.json");
    write_secret(alpha.clone(), false);
    let secrets = stdfs::canonicalize(&secrets).expect("canonical secrets");
    let alpha = secrets.join("alpha.json");

    let cache_root = dir.join("cache-root");
    stdfs::create_dir_all(&cache_root).expect("cache root");

    let secrets_env = secrets.display().to_string();
    let cache_root_env = cache_root.display().to_string();
    let _secret_dir = EnvGuard::set(&lock, "GEMINI_SECRET_DIR", &secrets_env);
    let _cache_root = EnvGuard::set(&lock, "ZSH_CACHE_DIR", &cache_root_env);

    write_cache_for_target(&alpha, 77);

    let options = rate_limits::RateLimitsOptions {
        async_mode: true,
        json: true,
        ..Default::default()
    };
    assert_eq!(rate_limits::run(&options), 0);
}

#[test]
fn rate_limits_async_json_missing_access_token_without_cache_returns_1() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("rate-limits-async-json-no-cache-fallback");

    let secrets = dir.join("secrets");
    stdfs::create_dir_all(&secrets).expect("secrets");
    write_secret(secrets.join("alpha.json"), false);
    let secrets = stdfs::canonicalize(&secrets).expect("canonical secrets");
    let cache_root = dir.join("cache-root");
    stdfs::create_dir_all(&cache_root).expect("cache root");

    let secrets_env = secrets.display().to_string();
    let cache_root_env = cache_root.display().to_string();
    let _secret_dir = EnvGuard::set(&lock, "GEMINI_SECRET_DIR", &secrets_env);
    let _cache_root = EnvGuard::set(&lock, "ZSH_CACHE_DIR", &cache_root_env);

    let options = rate_limits::RateLimitsOptions {
        async_mode: true,
        json: true,
        ..Default::default()
    };
    assert_eq!(rate_limits::run(&options), 1);
}

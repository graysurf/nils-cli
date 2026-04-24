use gemini_cli::{prompt_segment, rate_limits};
use nils_test_support::{EnvGuard, GlobalStateLock};

use std::fs as stdfs;
use std::path::{Path, PathBuf};
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

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

fn write_auth_secret(dir: &TestDir) -> (PathBuf, PathBuf, PathBuf) {
    let secrets = dir.join("secrets");
    stdfs::create_dir_all(&secrets).expect("secrets");
    let auth_file = secrets.join("alpha.json");
    stdfs::write(
        &auth_file,
        r#"{"tokens":{"access_token":"tok","account_id":"acct_001"}}"#,
    )
    .expect("write auth");

    let cache_root = dir.join("cache-root");
    stdfs::create_dir_all(&cache_root).expect("cache root");
    (auth_file, secrets, cache_root)
}

fn write_auth_with_id_token(path: &Path, id_token: &str) {
    stdfs::write(
        path,
        format!(
            "{{\"tokens\":{{\"id_token\":\"{id_token}\",\"access_token\":\"tok\",\"account_id\":\"acct_001\"}}}}"
        ),
    )
    .expect("write auth with id token");
}

fn set_env(lock: &GlobalStateLock, key: &str, value: impl AsRef<std::ffi::OsStr>) -> EnvGuard {
    let value = value.as_ref().to_string_lossy().into_owned();
    EnvGuard::set(lock, key, &value)
}

fn set_fast_fail_refresh_env(lock: &GlobalStateLock) -> (EnvGuard, EnvGuard, EnvGuard, EnvGuard) {
    (
        set_env(lock, "CODE_ASSIST_ENDPOINT", "http://127.0.0.1:9/"),
        set_env(
            lock,
            "GEMINI_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS",
            "1",
        ),
        set_env(lock, "GEMINI_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS", "1"),
        set_env(lock, "GEMINI_PROMPT_SEGMENT_EXE", "/usr/bin/false"),
    )
}

fn lock_dir_for_auth_file(auth_file: &Path) -> PathBuf {
    let cache_file = rate_limits::cache_file_for_target(auth_file).expect("cache file");
    let stem = cache_file
        .file_stem()
        .expect("cache stem")
        .to_string_lossy();
    cache_file.with_file_name(format!("{stem}.refresh.lock"))
}

#[test]
fn prompt_segment_disabled_returns_0() {
    let lock = GlobalStateLock::new();
    let options = prompt_segment::PromptSegmentOptions::default();
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "false");
    assert_eq!(prompt_segment::run(&options), 0);
}

#[test]
fn prompt_segment_is_enabled_flag_reflects_env() {
    let lock = GlobalStateLock::new();
    let options = prompt_segment::PromptSegmentOptions {
        is_enabled: true,
        ..Default::default()
    };

    let _enabled_false = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "false");
    assert_eq!(prompt_segment::run(&options), 1);
    drop(_enabled_false);

    let _enabled_true = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    assert_eq!(prompt_segment::run(&options), 0);
}

#[test]
fn prompt_segment_invalid_ttl_returns_2() {
    let lock = GlobalStateLock::new();
    let options = prompt_segment::PromptSegmentOptions {
        ttl: Some("bogus".to_string()),
        ..Default::default()
    };
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    assert_eq!(prompt_segment::run(&options), 2);
}

#[test]
fn prompt_segment_non_stale_cache_skips_failed_refresh() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("prompt-segment-non-stale-cache");
    let (auth_file, secrets, cache_root) = write_auth_secret(&dir);

    let _auth = set_env(&lock, "GEMINI_AUTH_FILE", &auth_file);
    let _secret_dir = set_env(&lock, "GEMINI_SECRET_DIR", &secrets);
    let _cache_root = set_env(&lock, "ZSH_CACHE_DIR", &cache_root);
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    let _base = set_env(&lock, "CODE_ASSIST_ENDPOINT", "http://127.0.0.1:9/");
    let _connect = set_env(
        &lock,
        "GEMINI_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS",
        "1",
    );
    let _max_time = set_env(&lock, "GEMINI_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS", "1");
    let _exe = set_env(&lock, "GEMINI_PROMPT_SEGMENT_EXE", "/usr/bin/false");

    rate_limits::write_prompt_segment_cache(
        &auth_file,
        now_epoch().max(1),
        "5h",
        94,
        88,
        1700600000,
        Some(1700003600),
    )
    .expect("write cache");
    let cache_file = rate_limits::cache_file_for_target(&auth_file).expect("cache file");
    let before = stdfs::read_to_string(&cache_file).expect("cache before");

    let options = prompt_segment::PromptSegmentOptions {
        time_format: Some("%Y-%m-%dT%H:%MZ".to_string()),
        ..Default::default()
    };
    assert_eq!(prompt_segment::run(&options), 0);

    let after = stdfs::read_to_string(&cache_file).expect("cache after");
    assert_eq!(before, after);
}

#[test]
fn prompt_segment_stale_cache_with_live_refresh_lock_returns_0() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("prompt-segment-stale-cache-failed-refresh");
    let (auth_file, secrets, cache_root) = write_auth_secret(&dir);

    let _auth = set_env(&lock, "GEMINI_AUTH_FILE", &auth_file);
    let _secret_dir = set_env(&lock, "GEMINI_SECRET_DIR", &secrets);
    let _cache_root = set_env(&lock, "ZSH_CACHE_DIR", &cache_root);
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    let _base = set_env(&lock, "CODE_ASSIST_ENDPOINT", "http://127.0.0.1:9/");
    let _connect = set_env(
        &lock,
        "GEMINI_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS",
        "1",
    );
    let _max_time = set_env(&lock, "GEMINI_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS", "1");
    let _exe = set_env(&lock, "GEMINI_PROMPT_SEGMENT_EXE", "/usr/bin/false");

    rate_limits::write_prompt_segment_cache(
        &auth_file,
        now_epoch().saturating_sub(10).max(1),
        "5h",
        1,
        2,
        1700600000,
        Some(1700003600),
    )
    .expect("write cache");

    let lock_dir = lock_dir_for_auth_file(&auth_file);
    stdfs::create_dir_all(&lock_dir).expect("create live lock");

    let options = prompt_segment::PromptSegmentOptions {
        ttl: Some("1s".to_string()),
        time_format: Some("%Y-%m-%dT%H:%MZ".to_string()),
        ..Default::default()
    };
    assert_eq!(prompt_segment::run(&options), 0);
}

#[test]
fn prompt_segment_valid_ttl_units_parse_when_disabled() {
    let lock = GlobalStateLock::new();
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "false");

    for ttl in ["1s", "2m", "3h", "4d", "1w", "5"] {
        let options = prompt_segment::PromptSegmentOptions {
            ttl: Some(ttl.to_string()),
            ..Default::default()
        };
        assert_eq!(prompt_segment::run(&options), 0, "ttl={ttl}");
    }
}

#[test]
fn prompt_segment_zero_ttl_returns_2() {
    let lock = GlobalStateLock::new();
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "false");
    let options = prompt_segment::PromptSegmentOptions {
        ttl: Some("0".to_string()),
        ..Default::default()
    };
    assert_eq!(prompt_segment::run(&options), 2);
}

#[test]
fn prompt_segment_env_ttl_parse_when_cli_absent() {
    let lock = GlobalStateLock::new();
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "false");
    let _ttl = set_env(&lock, "GEMINI_PROMPT_SEGMENT_TTL", "2m");
    let options = prompt_segment::PromptSegmentOptions::default();
    assert_eq!(prompt_segment::run(&options), 0);
}

#[test]
fn prompt_segment_missing_auth_file_returns_0() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("prompt-segment-missing-auth-file");
    let missing = dir.join("missing.json");

    let _auth = set_env(&lock, "GEMINI_AUTH_FILE", &missing);
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    assert_eq!(
        prompt_segment::run(&prompt_segment::PromptSegmentOptions::default()),
        0
    );
}

#[test]
fn prompt_segment_unresolvable_auth_path_returns_0() {
    let lock = GlobalStateLock::new();
    let _auth = set_env(&lock, "GEMINI_AUTH_FILE", "");
    let _home = set_env(&lock, "HOME", "");
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    assert_eq!(
        prompt_segment::run(&prompt_segment::PromptSegmentOptions::default()),
        0
    );
}

#[test]
fn prompt_segment_missing_cache_root_is_treated_as_no_cache() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("prompt-segment-missing-cache-root");
    let (auth_file, secrets, _cache_root) = write_auth_secret(&dir);

    let _auth = set_env(&lock, "GEMINI_AUTH_FILE", &auth_file);
    let _secret_dir = set_env(&lock, "GEMINI_SECRET_DIR", &secrets);
    let _home = set_env(&lock, "HOME", "");
    let _zdot = set_env(&lock, "ZDOTDIR", "");
    let _cache_root = set_env(&lock, "ZSH_CACHE_DIR", "");
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    let (_base, _connect, _max_time, _exe) = set_fast_fail_refresh_env(&lock);

    assert_eq!(
        prompt_segment::run(&prompt_segment::PromptSegmentOptions::default()),
        0
    );
}

#[test]
fn prompt_segment_invalid_cache_entry_is_ignored() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("prompt-segment-invalid-cache-entry");
    let (auth_file, secrets, cache_root) = write_auth_secret(&dir);

    let _auth = set_env(&lock, "GEMINI_AUTH_FILE", &auth_file);
    let _secret_dir = set_env(&lock, "GEMINI_SECRET_DIR", &secrets);
    let _cache_root = set_env(&lock, "ZSH_CACHE_DIR", &cache_root);
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    let (_base, _connect, _max_time, _exe) = set_fast_fail_refresh_env(&lock);

    let cache_file = rate_limits::cache_file_for_target(&auth_file).expect("cache file");
    if let Some(parent) = cache_file.parent() {
        stdfs::create_dir_all(parent).expect("cache parent");
    }
    stdfs::write(&cache_file, "not-a-valid-cache").expect("write invalid cache");

    assert_eq!(
        prompt_segment::run(&prompt_segment::PromptSegmentOptions::default()),
        0
    );
}

#[test]
fn prompt_segment_non_positive_fetched_at_is_treated_as_stale() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("prompt-segment-non-positive-fetched-at");
    let (auth_file, secrets, cache_root) = write_auth_secret(&dir);

    let _auth = set_env(&lock, "GEMINI_AUTH_FILE", &auth_file);
    let _secret_dir = set_env(&lock, "GEMINI_SECRET_DIR", &secrets);
    let _cache_root = set_env(&lock, "ZSH_CACHE_DIR", &cache_root);
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    let (_base, _connect, _max_time, _exe) = set_fast_fail_refresh_env(&lock);

    rate_limits::write_prompt_segment_cache(
        &auth_file,
        0,
        "5h",
        94,
        88,
        1700600000,
        Some(1700003600),
    )
    .expect("write cache");

    assert_eq!(
        prompt_segment::run(&prompt_segment::PromptSegmentOptions::default()),
        0
    );
}

#[test]
fn prompt_segment_default_time_format_paths_execute() {
    let lock = GlobalStateLock::new();
    let dir = TestDir::new("prompt-segment-default-time-format");
    let (auth_file, secrets, cache_root) = write_auth_secret(&dir);

    let _auth = set_env(&lock, "GEMINI_AUTH_FILE", &auth_file);
    let _secret_dir = set_env(&lock, "GEMINI_SECRET_DIR", &secrets);
    let _cache_root = set_env(&lock, "ZSH_CACHE_DIR", &cache_root);
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    let (_base, _connect, _max_time, _exe) = set_fast_fail_refresh_env(&lock);

    rate_limits::write_prompt_segment_cache(
        &auth_file,
        now_epoch().max(1),
        "5h",
        94,
        88,
        1700600000,
        Some(1700003600),
    )
    .expect("write cache");

    let with_timezone = prompt_segment::PromptSegmentOptions {
        show_timezone: true,
        ..Default::default()
    };
    assert_eq!(prompt_segment::run(&with_timezone), 0);

    assert_eq!(
        prompt_segment::run(&prompt_segment::PromptSegmentOptions::default()),
        0
    );
}

#[test]
fn prompt_segment_email_name_source_paths_execute() {
    const TOKEN_WITH_EMAIL: &str =
        "x.eyJlbWFpbCI6ImFsaWNlQGV4YW1wbGUuY29tIiwic3ViIjoiYWxpY2UtaWQifQ.y";
    const TOKEN_WITH_SUB_ONLY: &str = "x.eyJzdWIiOiJhbGljZS1pZCJ9.y";

    let lock = GlobalStateLock::new();
    let dir = TestDir::new("prompt-segment-email-name-source");
    let (auth_file, secrets, cache_root) = write_auth_secret(&dir);

    let _auth = set_env(&lock, "GEMINI_AUTH_FILE", &auth_file);
    let _secret_dir = set_env(&lock, "GEMINI_SECRET_DIR", &secrets);
    let _cache_root = set_env(&lock, "ZSH_CACHE_DIR", &cache_root);
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    let _source = set_env(&lock, "GEMINI_PROMPT_SEGMENT_NAME_SOURCE", "email");
    let (_base, _connect, _max_time, _exe) = set_fast_fail_refresh_env(&lock);

    write_auth_with_id_token(&auth_file, TOKEN_WITH_EMAIL);
    assert_eq!(
        prompt_segment::run(&prompt_segment::PromptSegmentOptions::default()),
        0
    );

    write_auth_with_id_token(&auth_file, TOKEN_WITH_SUB_ONLY);
    let _fallback = set_env(
        &lock,
        "GEMINI_PROMPT_SEGMENT_SHOW_FALLBACK_NAME_ENABLED",
        "true",
    );
    let _show_full = set_env(
        &lock,
        "GEMINI_PROMPT_SEGMENT_SHOW_FULL_EMAIL_ENABLED",
        "true",
    );
    assert_eq!(
        prompt_segment::run(&prompt_segment::PromptSegmentOptions::default()),
        0
    );
    drop(_show_full);
    drop(_fallback);

    let _fallback_off = set_env(
        &lock,
        "GEMINI_PROMPT_SEGMENT_SHOW_FALLBACK_NAME_ENABLED",
        "false",
    );
    assert_eq!(
        prompt_segment::run(&prompt_segment::PromptSegmentOptions::default()),
        0
    );
}

#[test]
fn prompt_segment_secret_name_fallback_paths_execute() {
    const TOKEN_WITH_SUB_ONLY: &str = "x.eyJzdWIiOiJhbGljZS1pZCJ9.y";

    let lock = GlobalStateLock::new();
    let dir = TestDir::new("prompt-segment-secret-name-fallback");
    let auth_file = dir.join("auth.json");
    write_auth_with_id_token(&auth_file, TOKEN_WITH_SUB_ONLY);

    let secrets = dir.join("secrets");
    stdfs::create_dir_all(&secrets).expect("secrets");
    let cache_root = dir.join("cache-root");
    stdfs::create_dir_all(&cache_root).expect("cache root");

    let _auth = set_env(&lock, "GEMINI_AUTH_FILE", &auth_file);
    let _secret_dir = set_env(&lock, "GEMINI_SECRET_DIR", &secrets);
    let _cache_root = set_env(&lock, "ZSH_CACHE_DIR", &cache_root);
    let _enabled = set_env(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    let (_base, _connect, _max_time, _exe) = set_fast_fail_refresh_env(&lock);

    let _fallback_on = set_env(
        &lock,
        "GEMINI_PROMPT_SEGMENT_SHOW_FALLBACK_NAME_ENABLED",
        "true",
    );
    assert_eq!(
        prompt_segment::run(&prompt_segment::PromptSegmentOptions::default()),
        0
    );
    drop(_fallback_on);

    let _fallback_off = set_env(
        &lock,
        "GEMINI_PROMPT_SEGMENT_SHOW_FALLBACK_NAME_ENABLED",
        "false",
    );
    assert_eq!(
        prompt_segment::run(&prompt_segment::PromptSegmentOptions::default()),
        0
    );
}

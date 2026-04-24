use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::http::{HttpResponse, LoopbackServer};
use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn run(args: &[&str], envs: &[(&str, &Path)], vars: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default()
        // Stabilize output for tests regardless of user shell prompt environment.
        .with_env("NO_COLOR", "1")
        .with_env("TZ", "UTC")
        .with_env_remove("STARSHIP_SESSION_KEY")
        .with_env_remove("STARSHIP_SHELL");
    for (key, path) in envs {
        let value = path.to_string_lossy();
        options = options.with_env(key, value.as_ref());
    }
    for (key, value) in vars {
        options = options.with_env(key, value);
    }
    let bin = codex_cli_bin();
    cmd::run_with(&bin, args, &options)
}

fn stdout(output: &CmdOutput) -> String {
    output.stdout_text()
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(output.code, code);
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

fn wait_for_file_contains(path: &Path, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(content) = fs::read_to_string(path)
            && content.contains(needle)
        {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    false
}

fn write_auth_and_secret(dir: &tempfile::TempDir) -> (PathBuf, PathBuf, PathBuf) {
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let payload_alpha = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";
    let hdr = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
    let token = format!("{hdr}.{payload_alpha}.sig");

    let secret_alpha = secrets.join("alpha.json");
    fs::write(
        &secret_alpha,
        format!(
            r#"{{
  "tokens": {{
    "access_token": "tok",
    "refresh_token": "refresh_token_value",
    "id_token": "{token}",
    "account_id": "acct_001"
  }},
  "last_refresh": "2025-01-20T12:34:56Z"
}}"#
        ),
    )
    .expect("write alpha secret");

    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, fs::read(&secret_alpha).expect("read alpha")).expect("write auth");

    (auth_file, secrets, cache_root)
}

fn cache_file(cache_root: &Path, key: &str) -> PathBuf {
    cache_root
        .join("codex")
        .join("prompt-segment-rate-limits")
        .join(format!("{key}.kv"))
}

fn write_prompt_segment_cache_kv(cache_root: &Path, key: &str, kv: &str) -> PathBuf {
    let path = cache_file(cache_root, key);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("cache dir");
    }
    fs::write(&path, kv).expect("write kv");
    path
}

fn wham_usage_ok_body() -> String {
    r#"{
  "rate_limit": {
    "primary_window": { "limit_window_seconds": 18000, "used_percent": 6, "reset_at": 1700003600 },
    "secondary_window": { "limit_window_seconds": 604800, "used_percent": 12, "reset_at": 1700600000 }
  }
}"#
    .to_string()
}

#[test]
fn prompt_segment_refresh_updates_cache_and_prints() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &[
            "prompt-segment",
            "--refresh",
            "--time-format",
            "%Y-%m-%dT%H:%MZ",
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_PROMPT_SEGMENT_ENABLED", "true"),
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS", "3"),
        ],
    );
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "alpha 5h:94% W:88% 2023-11-21T20:53Z\n");

    let kv_path = cache_file(&cache_root, "alpha");
    let kv = fs::read_to_string(&kv_path).expect("read cache kv");
    assert!(kv.contains("weekly_remaining=88"));
    assert!(kv.contains("non_weekly_remaining=94"));
}

#[test]
fn prompt_segment_stale_cache_triggers_background_refresh() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let fetched_at = now_epoch().saturating_sub(10).max(1);
    write_prompt_segment_cache_kv(
        &cache_root,
        "alpha",
        &format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=1\nweekly_remaining=2\nweekly_reset_epoch=1700600000\n"
        ),
    );

    let output = run(
        &[
            "prompt-segment",
            "--ttl",
            "1s",
            "--time-format",
            "%Y-%m-%dT%H:%MZ",
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_PROMPT_SEGMENT_ENABLED", "true"),
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_PROMPT_SEGMENT_STALE_SUFFIX", " (STALE)"),
            ("CODEX_PROMPT_SEGMENT_REFRESH_MIN_SECONDS", "0"),
            ("CODEX_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS", "3"),
        ],
    );
    assert_exit(&output, 0);
    assert_eq!(
        stdout(&output),
        "alpha 5h:1% W:2% 2023-11-21T20:53Z (STALE)\n"
    );

    let kv_path = cache_file(&cache_root, "alpha");
    assert!(
        wait_for_file_contains(&kv_path, "weekly_remaining=88", Duration::from_secs(3)),
        "expected background refresh to update cache kv"
    );
}

#[test]
fn prompt_segment_refresh_recovers_from_stale_lock_dir() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let lock_dir = cache_root
        .join("codex")
        .join("prompt-segment-rate-limits")
        .join("alpha.refresh.lock");
    fs::create_dir_all(&lock_dir).expect("create lock dir");

    let output = run(
        &[
            "prompt-segment",
            "--refresh",
            "--time-format",
            "%Y-%m-%dT%H:%MZ",
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_PROMPT_SEGMENT_ENABLED", "true"),
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_PROMPT_SEGMENT_LOCK_STALE_SECONDS", "0"),
            ("CODEX_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS", "3"),
        ],
    );
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "alpha 5h:94% W:88% 2023-11-21T20:53Z\n");
}

#[test]
fn prompt_segment_refresh_respects_min_interval() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_ok_body()),
    );
    let base_url = server.url();

    let fetched_at = now_epoch().saturating_sub(10).max(1);
    write_prompt_segment_cache_kv(
        &cache_root,
        "alpha",
        &format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=1\nweekly_remaining=2\nweekly_reset_epoch=1700600000\n"
        ),
    );

    let vars = [
        ("CODEX_PROMPT_SEGMENT_ENABLED", "true"),
        ("CODEX_CHATGPT_BASE_URL", base_url.as_str()),
        ("CODEX_PROMPT_SEGMENT_REFRESH_MIN_SECONDS", "9999"),
        ("CODEX_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
        ("CODEX_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS", "3"),
    ];
    let envs = [
        ("CODEX_AUTH_FILE", auth_file.as_path()),
        ("CODEX_SECRET_DIR", secrets.as_path()),
        ("ZSH_CACHE_DIR", cache_root.as_path()),
    ];

    let output = run(&["prompt-segment", "--ttl", "1s"], &envs, &vars);
    assert_exit(&output, 0);
    let output = run(&["prompt-segment", "--ttl", "1s"], &envs, &vars);
    assert_exit(&output, 0);

    thread::sleep(Duration::from_secs(1));
    let requests = server.take_requests();
    assert_eq!(
        requests.iter().filter(|r| r.path == "/wham/usage").count(),
        1
    );
}

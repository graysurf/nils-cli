use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::http::{HttpResponse, LoopbackServer};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn gemini_cli_bin() -> PathBuf {
    bin::resolve("gemini-cli")
}

fn run(args: &[&str], envs: &[(&str, &Path)], vars: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default();
    for (key, path) in envs {
        let value = path.to_string_lossy();
        options = options.with_env(key, value.as_ref());
    }
    for (key, value) in vars {
        options = options.with_env(key, value);
    }
    let bin = gemini_cli_bin();
    cmd::run_with(&bin, args, &options)
}

fn stdout(output: &CmdOutput) -> String {
    output.stdout_text()
}

fn stderr(output: &CmdOutput) -> String {
    output.stderr_text()
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(
        output.code,
        code,
        "unexpected exit code.\nstdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

const JWT_HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
const JWT_PAYLOAD_ALPHA: &str = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20ifQ";

fn token(payload: &str) -> String {
    format!("{JWT_HEADER}.{payload}.sig")
}

fn write_secret(dir: &Path, name: &str, access_token: Option<&str>) -> PathBuf {
    let path = dir.join(name);
    let json = match access_token {
        Some(token) => format!(
            r#"{{
  "tokens": {{
    "access_token": "{token}",
    "account_id": "acct_001"
  }}
}}"#
        ),
        None => r#"{"tokens":{"account_id":"acct_001"}}"#.to_string(),
    };
    fs::write(&path, json).expect("write secret");
    path
}

fn write_secret_with_identity(dir: &Path, name: &str, access_token: Option<&str>) -> PathBuf {
    let path = dir.join(name);
    let id_token = token(JWT_PAYLOAD_ALPHA);
    let json = match access_token {
        Some(token_value) => format!(
            r#"{{
  "tokens": {{
    "id_token": "{id_token}",
    "access_token": "{token_value}",
    "account_id": "acct_001"
  }}
}}"#
        ),
        None => format!(
            r#"{{
  "tokens": {{
    "id_token": "{id_token}",
    "account_id": "acct_001"
  }}
}}"#
        ),
    };
    fs::write(&path, json).expect("write secret with identity");
    path
}

fn write_auth_with_identity(path: &Path, access_token: &str) {
    let id_token = token(JWT_PAYLOAD_ALPHA);
    let json = format!(
        r#"{{
  "tokens": {{
    "id_token": "{id_token}",
    "access_token": "{access_token}",
    "account_id": "acct_001"
  }}
}}"#
    );
    fs::write(path, json).expect("write auth");
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

fn cache_kv_path(cache_root: &Path, key: &str) -> PathBuf {
    cache_root
        .join("gemini")
        .join("prompt-segment-rate-limits")
        .join(format!("{key}.kv"))
}

fn handle_barrier_connection(
    stream: &mut TcpStream,
    response_body: &str,
    state: &Arc<(Mutex<usize>, Condvar)>,
    expected_requests: usize,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0u8; 4096];
    let _ = stream.read(&mut buf);

    let (lock, cv) = &**state;
    let seen = lock.lock().expect("seen lock");
    let mut seen = seen;
    *seen += 1;
    cv.notify_all();
    let ready = cv
        .wait_timeout_while(seen, Duration::from_secs(2), |count| {
            *count < expected_requests
        })
        .expect("barrier wait");
    let concurrent = *ready.0 >= expected_requests;

    let (status, reason, body) = if concurrent {
        (200, "OK", response_body.to_string())
    } else {
        (
            504,
            "Gateway Timeout",
            r#"{"error":"concurrency barrier not satisfied"}"#.to_string(),
        )
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn spawn_concurrency_barrier_server(
    expected_requests: usize,
) -> (String, thread::JoinHandle<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener
        .set_nonblocking(true)
        .expect("set listener nonblocking");
    let addr = listener.local_addr().expect("local addr");
    let body = wham_usage_ok_body();
    let state = Arc::new((Mutex::new(0usize), Condvar::new()));

    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut accepted = 0usize;
        let mut handlers = Vec::new();
        while Instant::now() < deadline && accepted < expected_requests {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    accepted += 1;
                    let state = Arc::clone(&state);
                    let body = body.clone();
                    handlers.push(thread::spawn(move || {
                        handle_barrier_connection(&mut stream, &body, &state, expected_requests);
                    }));
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }

        for handler in handlers {
            let _ = handler.join();
        }
        accepted
    });

    (format!("http://{addr}"), handle)
}

#[test]
fn rate_limits_single_default_output_from_network() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "POST",
        "/v1internal:retrieveUserQuota",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "alpha.json"],
        &[
            ("GEMINI_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODE_ASSIST_ENDPOINT", &server.url()),
            ("GEMINI_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("GEMINI_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("GEMINI_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);
    assert_eq!(
        stdout(&output),
        "Rate limits remaining\n5h 94% • 11-14 23:13\nWeekly 88% • 11-21 20:53\n"
    );
}

#[test]
fn rate_limits_single_one_line_writes_cache() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "POST",
        "/v1internal:retrieveUserQuota",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--one-line", "alpha.json"],
        &[
            ("GEMINI_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODE_ASSIST_ENDPOINT", &server.url()),
            ("GEMINI_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("GEMINI_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("GEMINI_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "5h:94% W:88% 11-21 20:53\n");

    let kv_path = cache_kv_path(&cache_root, "alpha");
    let kv = fs::read_to_string(&kv_path).expect("read kv");
    assert!(kv.contains("weekly_remaining=88"));
    assert!(kv.contains("non_weekly_remaining=94"));
    assert!(kv.contains("weekly_reset_epoch=1700600000"));
}

#[test]
fn rate_limits_single_json_outputs_body() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "POST",
        "/v1internal:retrieveUserQuota",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--json", "alpha.json"],
        &[
            ("GEMINI_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODE_ASSIST_ENDPOINT", &server.url()),
            ("GEMINI_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("GEMINI_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("GEMINI_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
        ],
    );
    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "gemini-cli.diag.rate-limits.v1");
    assert_eq!(payload["command"], "diag rate-limits");
    assert_eq!(payload["mode"], "single");
    assert_eq!(payload["ok"], true);
    assert!(payload["result"]["raw_usage"]["rate_limit"].is_object());
    assert!(payload["result"]["summary"]["non_weekly_label"].is_string());
}

#[test]
fn rate_limits_all_mode_renders_table() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok_a"));
    write_secret(&secrets, "beta.json", Some("tok_b"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "POST",
        "/v1internal:retrieveUserQuota",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--all"],
        &[
            ("GEMINI_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODE_ASSIST_ENDPOINT", &server.url()),
            ("GEMINI_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("GEMINI_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("GEMINI_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(out.contains("🚦 Gemini rate limits for all accounts"));
    assert!(out.contains("Name"));
    assert!(out.contains("alpha"));
    assert!(out.contains("beta"));
    assert!(out.contains("+00:00"));
}

#[test]
fn rate_limits_all_mode_syncs_matching_secret_before_fetch() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret_with_identity(&secrets, "alpha.json", None);

    let auth_file = dir.path().join("auth.json");
    write_auth_with_identity(&auth_file, "tok_fresh");

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "POST",
        "/v1internal:retrieveUserQuota",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--all"],
        &[
            ("GEMINI_SECRET_DIR", &secrets),
            ("GEMINI_AUTH_FILE", &auth_file),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODE_ASSIST_ENDPOINT", &server.url()),
            ("GEMINI_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("GEMINI_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("GEMINI_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);

    let synced: Value =
        serde_json::from_str(&fs::read_to_string(secrets.join("alpha.json")).expect("read synced"))
            .expect("synced json");
    assert_eq!(synced["tokens"]["access_token"], "tok_fresh");
    assert!(!stderr(&output).contains("missing access_token"));
}

#[test]
fn rate_limits_default_all_env_enables_all_mode_without_flag() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok_alpha"));
    write_secret(&secrets, "beta.json", Some("tok_beta"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "POST",
        "/v1internal:retrieveUserQuota",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits"],
        &[
            ("GEMINI_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODE_ASSIST_ENDPOINT", &server.url()),
            ("GEMINI_RATE_LIMITS_DEFAULT_ALL_ENABLED", "true"),
            ("GEMINI_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("GEMINI_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(out.contains("🚦 Gemini rate limits for all accounts"));
    assert!(out.contains("alpha"));
    assert!(out.contains("beta"));
    assert!(!out.contains("Rate limits remaining"));
}

#[test]
fn rate_limits_async_json_falls_back_to_cache_for_missing_token() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok_a"));
    write_secret(&secrets, "beta.json", None);

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let fetched_at = now_epoch().saturating_sub(10).max(1);
    let kv_path = cache_kv_path(&cache_root, "beta");
    if let Some(parent) = kv_path.parent() {
        fs::create_dir_all(parent).expect("cache dir");
    }
    fs::write(
        &kv_path,
        format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=1\nweekly_remaining=2\nweekly_reset_epoch=1700600000\n"
        ),
    )
    .expect("write cache kv");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "POST",
        "/v1internal:retrieveUserQuota",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--async", "--json"],
        &[
            ("GEMINI_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODE_ASSIST_ENDPOINT", &server.url()),
            ("GEMINI_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("GEMINI_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("GEMINI_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["mode"], "async");

    let results = payload["results"].as_array().expect("results array");
    let beta = results
        .iter()
        .find(|item| item["name"] == "beta")
        .expect("beta result");
    assert_eq!(beta["source"], "cache-fallback");
    assert_eq!(beta["ok"], true);
}

#[test]
fn rate_limits_async_jobs_zero_defaults_to_concurrent_workers() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok_a"));
    write_secret(&secrets, "beta.json", Some("tok_b"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let (base_url, server) = spawn_concurrency_barrier_server(2);

    let output = run(
        &["diag", "rate-limits", "--async", "--jobs", "0"],
        &[
            ("GEMINI_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODE_ASSIST_ENDPOINT", &base_url),
            ("GEMINI_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("GEMINI_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("GEMINI_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);

    let out = stdout(&output);
    assert!(out.contains("🚦 Gemini rate limits for all accounts"));
    assert!(out.contains("alpha"));
    assert!(out.contains("beta"));
    assert!(
        out.find("alpha").expect("alpha in output") < out.find("beta").expect("beta in output")
    );
    assert_eq!(server.join().expect("server join"), 2);
}

#[test]
fn rate_limits_async_json_partial_failure_keeps_sorted_results() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "beta.json", None);
    write_secret(&secrets, "alpha.json", Some("tok_a"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "POST",
        "/v1internal:retrieveUserQuota",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--async", "--json"],
        &[
            ("GEMINI_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODE_ASSIST_ENDPOINT", &server.url()),
            ("GEMINI_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("GEMINI_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("GEMINI_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 1);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["mode"], "async");

    let results = payload["results"].as_array().expect("results array");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["name"], "alpha");
    assert_eq!(results[1]["name"], "beta");
    assert_eq!(results[0]["ok"], true);
    assert_eq!(results[1]["ok"], false);
    assert_eq!(results[1]["source"], "network");
    assert_eq!(results[1]["error"]["code"], "missing-access-token");
}

#[test]
fn rate_limits_clear_cache_removes_old_prompt_segment_cache_dir() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok"));

    let cache_root = dir.path().join("cache_root");
    let old_dir = cache_root.join("gemini").join("prompt-segment-rate-limits");
    fs::create_dir_all(&old_dir).expect("cache dir");
    let junk = old_dir.join("junk.txt");
    fs::write(&junk, "junk").expect("write junk");
    assert!(junk.is_file());

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "POST",
        "/v1internal:retrieveUserQuota",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "-c", "--one-line", "alpha.json"],
        &[
            ("GEMINI_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODE_ASSIST_ENDPOINT", &server.url()),
            ("GEMINI_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("GEMINI_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("GEMINI_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);

    assert!(!junk.exists());
    assert!(cache_kv_path(&cache_root, "alpha").is_file());
}

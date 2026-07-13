use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::http::{HttpResponse, LoopbackServer};
use nils_test_support::write_exe;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn build_cmd_options(
    base: CmdOptions,
    envs: &[(&str, &Path)],
    vars: &[(&str, &str)],
) -> CmdOptions {
    let mut options = base.with_env_remove_many(&[
        "CODEX_AUTO_REFRESH_ENABLED",
        "CODEX_AUTH_REMOTE_SSH",
        "CODEX_AUTH_REMOTE_NAME",
        "CODEX_AUTH_REMOTE_REFRESH",
    ]);
    for (key, path) in envs {
        let value = path.to_string_lossy();
        options = options.with_env(key, value.as_ref());
    }
    for (key, value) in vars {
        options = options.with_env(key, value);
    }
    options
}

fn run(args: &[&str], envs: &[(&str, &Path)], vars: &[(&str, &str)]) -> CmdOutput {
    let options = build_cmd_options(CmdOptions::default(), envs, vars);
    let bin = codex_cli_bin();
    cmd::run_with(&bin, args, &options)
}

fn run_with_path_prepend(
    args: &[&str],
    envs: &[(&str, &Path)],
    vars: &[(&str, &str)],
    path_prepend: &Path,
) -> CmdOutput {
    let options = build_cmd_options(
        CmdOptions::default().with_path_prepend(path_prepend),
        envs,
        vars,
    );
    let bin = codex_cli_bin();
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

fn wham_usage_weekly_only_body() -> String {
    r#"{
  "user_id": "user-private",
  "account_id": "account-private",
  "email": "private@example.com",
  "EMAIL": "private-alias@example.com",
  "api_key": "private-api-key",
  "nested": { "organization_id": "private-org" },
  "plan_type": "pro",
  "rate_limit": {
    "allowed": true,
    "limit_reached": false,
    "primary_window": { "limit_window_seconds": 604800, "used_percent": 21, "reset_at": 1700600000 },
    "secondary_window": null
  }
}"#
    .to_string()
}

fn wham_usage_non_weekly_only_body() -> String {
    r#"{
  "plan_type": "pro",
  "rate_limit": {
    "allowed": true,
    "limit_reached": false,
    "primary_window": null,
    "secondary_window": { "limit_window_seconds": 18000, "used_percent": 9, "reset_at": 1700003600 }
  }
}"#
    .to_string()
}

fn wham_usage_empty_windows_body() -> String {
    r#"{
  "plan_type": "pro",
  "rate_limit": {
    "primary_window": null,
    "secondary_window": null
  }
}"#
    .to_string()
}

fn wham_usage_null_rate_limit_body() -> String {
    r#"{"plan_type":"pro","rate_limit":null}"#.to_string()
}

fn json_contains_key(value: &Value, needle: &str) -> bool {
    match value {
        Value::Object(map) => {
            map.contains_key(needle) || map.values().any(|value| json_contains_key(value, needle))
        }
        Value::Array(items) => items.iter().any(|value| json_contains_key(value, needle)),
        _ => false,
    }
}

fn cache_kv_path(cache_root: &Path, key: &str) -> PathBuf {
    cache_root
        .join("codex")
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
    let mut seen = lock.lock().expect("seen lock");
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
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
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
fn rate_limits_single_one_line_writes_cache_and_metadata() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    let secret_path = write_secret(&secrets, "alpha.json", Some("tok"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--one-line", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "5h:94% W:88% 11-21 20:53\n");

    let secret_json: Value =
        serde_json::from_str(&fs::read_to_string(&secret_path).expect("read secret"))
            .expect("json");
    assert_eq!(
        secret_json["codex_rate_limits"]["weekly_reset_at_epoch"].as_i64(),
        Some(1700600000)
    );
    assert_eq!(
        secret_json["codex_rate_limits"]["non_weekly_reset_at_epoch"].as_i64(),
        Some(1700003600)
    );

    let kv_path = cache_kv_path(&cache_root, "alpha");
    let kv = fs::read_to_string(&kv_path).expect("read kv");
    assert!(kv.contains("weekly_remaining=88"));
    assert!(kv.contains("non_weekly_remaining=94"));
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
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--json", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
        ],
    );
    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.diag.rate-limits.v1");
    assert_eq!(payload["command"], "diag rate-limits");
    assert_eq!(payload["mode"], "single");
    assert_eq!(payload["ok"], true);
    assert!(payload["result"]["raw_usage"]["rate_limit"].is_object());
    assert!(payload["result"]["summary"]["non_weekly_label"].is_string());
}

#[test]
fn rate_limits_single_json_accepts_weekly_only_and_redacts_identity() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_weekly_only_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--json", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
        ],
    );

    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    let result = &payload["result"];
    let windows = result["windows"].as_array().expect("windows");
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0]["label"], "Weekly");
    assert_eq!(windows[0]["used_percent"], 21);
    assert_eq!(windows[0]["remaining_percent"], 79);
    assert_eq!(result["summary"]["weekly_remaining"], 79);
    assert!(result["summary"]["non_weekly_remaining"].is_null());
    let output_json = stdout(&output);
    for private in ["user-private", "account-private", "private@example.com"] {
        assert!(
            !output_json.contains(private),
            "diagnostic output leaked private identity: {private}"
        );
    }
    for sensitive_key in [
        "user_id",
        "account_id",
        "email",
        "EMAIL",
        "api_key",
        "organization_id",
    ] {
        assert!(
            !json_contains_key(&result["raw_usage"], sensitive_key),
            "diagnostic output retained sensitive key: {sensitive_key}"
        );
    }
    assert_eq!(result["raw_usage"]["plan_type"], "pro");
}

#[test]
fn rate_limits_single_json_accepts_non_weekly_only_and_clears_weekly_cache() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok"));

    let cache_root = dir.path().join("cache_root");
    let kv_path = cache_kv_path(&cache_root, "alpha");
    fs::create_dir_all(kv_path.parent().expect("cache parent")).expect("cache dir");
    fs::write(
        &kv_path,
        "fetched_at=1\nnon_weekly_label=5h\nnon_weekly_remaining=1\nweekly_remaining=2\nweekly_reset_epoch=1700600000\n",
    )
    .expect("stale cache");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_non_weekly_only_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--json", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
        ],
    );

    assert_exit(&output, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    let result = &payload["result"];
    assert_eq!(result["windows"].as_array().expect("windows").len(), 1);
    assert_eq!(result["windows"][0]["label"], "5h");
    assert_eq!(result["windows"][0]["remaining_percent"], 91);
    assert_eq!(result["summary"]["non_weekly_remaining"], 91);
    assert!(result["summary"]["weekly_remaining"].is_null());

    let cache = fs::read_to_string(&kv_path).expect("cache");
    assert!(cache.contains("non_weekly_remaining=91"));
    assert!(
        !cache
            .lines()
            .any(|line| line.starts_with("weekly_remaining="))
    );
    assert!(
        !cache
            .lines()
            .any(|line| line.starts_with("weekly_reset_epoch="))
    );
}

#[test]
fn rate_limits_single_one_line_weekly_only_replaces_non_weekly_cache() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok"));

    let cache_root = dir.path().join("cache_root");
    let kv_path = cache_kv_path(&cache_root, "alpha");
    fs::create_dir_all(kv_path.parent().expect("cache parent")).expect("cache dir");
    fs::write(
        &kv_path,
        "fetched_at=1\nnon_weekly_label=5h\nnon_weekly_remaining=1\nnon_weekly_reset_epoch=1700003600\nweekly_remaining=2\nweekly_reset_epoch=1700600000\n",
    )
    .expect("stale cache");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_weekly_only_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--one-line", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );

    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "W:79% 11-21 20:53\n");
    let cache = fs::read_to_string(&kv_path).expect("refreshed cache");
    assert!(cache.contains("weekly_remaining=79"));
    assert!(!cache.contains("non_weekly_"));
}

#[test]
fn rate_limits_single_401_does_not_refresh_auth_by_default() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/wham/usage", HttpResponse::new(401, ""));

    let output = run(
        &["diag", "rate-limits", "--json", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
        ],
    );
    assert_exit(&output, 3);
    assert!(!stderr(&output).contains("codex-refresh"));

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["error"]["code"], "request-failed");
    assert_eq!(payload["error"]["details"]["reason_code"], "auth_expired");
    assert!(
        !payload["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("HTTP 401")
    );

    let requests = server.take_requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "GET" && request.path == "/wham/usage")
            .count(),
        1
    );
}

#[test]
fn rate_limits_single_401_refreshes_auth_when_enabled() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/wham/usage", HttpResponse::new(401, ""));

    let output = run(
        &["diag", "rate-limits", "--json", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_AUTO_REFRESH_ENABLED", "true"),
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
        ],
    );
    assert_exit(&output, 3);
    assert!(stderr(&output).contains("codex-refresh"));
    assert!(stderr(&output).contains("failed to read refresh token"));

    let requests = server.take_requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "GET" && request.path == "/wham/usage")
            .count(),
        2
    );
}

#[test]
fn rate_limits_single_401_uses_remote_authority_for_secret_target() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");

    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("stale-token"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let args_file = dir.path().join("ssh-args.txt");
    write_exe(
        &stubs,
        "ssh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" > "$SSH_ARGS_FILE"
printf '%s\n' "$REMOTE_AUTH_PAYLOAD"
"#,
    );

    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/wham/usage", HttpResponse::new(401, ""));

    let remote_payload = r#"{"tokens":{"access_token":"remote-access-token","id_token":"remote-id-token","account_id":"acct_001"},"last_refresh":"2025-01-20T12:34:56Z"}"#;
    let output = run_with_path_prepend(
        &["diag", "rate-limits", "--json", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_AUTO_REFRESH_ENABLED", "true"),
            ("CODEX_AUTH_REMOTE_SSH", "auth-host"),
            ("CODEX_AUTH_REMOTE_NAME", "gamania"),
            ("CODEX_AUTH_REMOTE_REFRESH", "true"),
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("REMOTE_AUTH_PAYLOAD", remote_payload),
            ("SSH_ARGS_FILE", args_file.to_str().expect("args path")),
        ],
        &stubs,
    );

    assert_exit(&output, 3);
    assert!(!stderr(&output).contains("failed to read refresh token"));

    let captured_args = fs::read_to_string(&args_file).expect("read ssh args");
    assert!(captured_args.contains("--name alpha"));
    assert!(!captured_args.contains("--name gamania"));
    assert!(captured_args.contains("--refresh"));

    let requests = server.take_requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "GET" && request.path == "/wham/usage")
            .count(),
        2
    );
    assert_eq!(
        requests[0].header_value("authorization").as_deref(),
        Some("Bearer stale-token")
    );
    assert_eq!(
        requests[1].header_value("authorization").as_deref(),
        Some("Bearer remote-access-token")
    );
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
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--all"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(out.contains("🚦 Codex rate limits for all accounts"));
    assert!(out.contains("Name"));
    assert!(out.contains("alpha"));
    assert!(out.contains("beta"));
    assert!(out.contains("+00:00"));
}

#[test]
fn rate_limits_all_mode_renders_weekly_only_without_stale_non_weekly_data() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok_a"));
    write_secret(&secrets, "beta.json", Some("tok_b"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_weekly_only_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--all"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );

    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(out.contains("alpha"));
    assert!(out.contains("beta"));
    assert!(out.contains("Weekly"));
    assert!(out.contains("79%"), "expected weekly usage, got:\n{out}");
    assert!(!out.contains("5h"), "unexpected non-weekly usage:\n{out}");
    assert!(!out.contains("stale"), "unexpected stale marker:\n{out}");
}

#[test]
fn rate_limits_all_mode_empty_windows_serves_preserved_cache() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    let secret_file = write_secret(&secrets, "alpha.json", Some("tok_a"));
    let before_secret = fs::read_to_string(&secret_file).expect("secret");

    let cache_root = dir.path().join("cache_root");
    let kv_path = cache_kv_path(&cache_root, "alpha");
    fs::create_dir_all(kv_path.parent().expect("cache parent")).expect("cache dir");
    let fetched_at = now_epoch().saturating_sub(300);
    let cache = format!(
        "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=91\nnon_weekly_reset_epoch=1700003600\nweekly_remaining=70\nweekly_reset_epoch=1700600000\n"
    );
    fs::write(&kv_path, &cache).expect("cache");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_empty_windows_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--all"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );

    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(
        out.contains("91%"),
        "expected cached non-weekly usage:\n{out}"
    );
    assert!(out.contains("70%"), "expected cached weekly usage:\n{out}");
    assert_eq!(
        fs::read_to_string(&secret_file).expect("secret"),
        before_secret
    );
    assert_eq!(fs::read_to_string(&kv_path).expect("cache"), cache);
}

#[test]
fn rate_limits_all_mode_null_rate_limit_serves_preserved_cache() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    let secret_file = write_secret(&secrets, "alpha.json", Some("tok_a"));
    let before_secret = fs::read_to_string(&secret_file).expect("secret");

    let cache_root = dir.path().join("cache_root");
    let kv_path = cache_kv_path(&cache_root, "alpha");
    fs::create_dir_all(kv_path.parent().expect("cache parent")).expect("cache dir");
    let fetched_at = now_epoch().saturating_sub(300);
    let cache = format!(
        "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=91\nnon_weekly_reset_epoch=1700003600\nweekly_remaining=70\nweekly_reset_epoch=1700600000\n"
    );
    fs::write(&kv_path, &cache).expect("cache");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_null_rate_limit_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--all"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );

    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(
        out.contains("91%"),
        "expected cached non-weekly usage:\n{out}"
    );
    assert!(out.contains("70%"), "expected cached weekly usage:\n{out}");
    assert_eq!(
        fs::read_to_string(&secret_file).expect("secret"),
        before_secret
    );
    assert_eq!(fs::read_to_string(&kv_path).expect("cache"), cache);
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
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--all"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("CODEX_AUTH_FILE", &auth_file),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
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
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "true"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(out.contains("🚦 Codex rate limits for all accounts"));
    assert!(out.contains("alpha"));
    assert!(out.contains("beta"));
    assert!(!out.contains("Rate limits remaining"));
}

#[test]
fn rate_limits_async_falls_back_to_cache_in_debug_mode() {
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
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "--async", "--debug"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);

    assert!(stdout(&output).contains("🚦 Codex rate limits for all accounts"));
    assert!(stdout(&output).contains("+00:00"));
    assert!(stderr(&output).contains("falling back to cache"));
    assert!(stderr(&output).contains("auth_required"));
}

#[test]
fn rate_limits_async_json_jobs_zero_defaults_to_concurrent_workers() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "beta.json", Some("tok_b"));
    write_secret(&secrets, "alpha.json", Some("tok_a"));

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let (base_url, server) = spawn_concurrency_barrier_server(2);

    let output = run(
        &["diag", "rate-limits", "--async", "--json", "--jobs", "0"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &base_url),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    let results = payload["results"].as_array().expect("results array");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["name"], "alpha");
    assert_eq!(results[1]["name"], "beta");
    assert_eq!(server.join().expect("server join"), 2);
}

#[test]
fn rate_limits_clear_cache_removes_old_prompt_segment_cache_dir() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    write_secret(&secrets, "alpha.json", Some("tok"));

    let cache_root = dir.path().join("cache_root");
    let old_dir = cache_root.join("codex").join("prompt-segment-rate-limits");
    fs::create_dir_all(&old_dir).expect("cache dir");
    let junk = old_dir.join("junk.txt");
    fs::write(&junk, "junk").expect("write junk");
    assert!(junk.is_file());

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let output = run(
        &["diag", "rate-limits", "-c", "--one-line", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);

    assert!(!junk.exists());
    assert!(cache_kv_path(&cache_root, "alpha").is_file());
}

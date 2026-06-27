use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::http::{HttpResponse, LoopbackServer, RecordedRequest, TestServer};
use nils_test_support::write_exe;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

fn run_with_path_prepend(
    args: &[&str],
    envs: &[(&str, &Path)],
    vars: &[(&str, &str)],
    path_prepend: &Path,
) -> CmdOutput {
    let mut options = CmdOptions::default()
        // Stabilize output for tests regardless of user shell prompt environment.
        .with_path_prepend(path_prepend)
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

fn alpha_id_token() -> String {
    let payload_alpha = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";
    let hdr = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
    format!("{hdr}.{payload_alpha}.sig")
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

fn collect_requests_for(server: &TestServer, timeout: Duration) -> Vec<RecordedRequest> {
    let deadline = Instant::now() + timeout;
    let mut requests = Vec::new();
    while Instant::now() < deadline {
        requests.extend(server.take_requests());
        thread::sleep(Duration::from_millis(25));
    }
    requests.extend(server.take_requests());
    requests
}

fn write_auth_and_secret(dir: &tempfile::TempDir) -> (PathBuf, PathBuf, PathBuf) {
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let token = alpha_id_token();

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
    const STALE_CACHE_AGE_SECONDS: u64 = 10;

    let dir = tempfile::TempDir::new().expect("tempdir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, wham_usage_ok_body()),
    );

    let fetched_at = now_epoch().saturating_sub(STALE_CACHE_AGE_SECONDS).max(1);
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

#[cfg(unix)]
#[test]
fn prompt_segment_background_refresh_survives_prompt_shell_hup() {
    use std::os::unix::process::CommandExt;

    let dir = tempfile::TempDir::new().expect("tempdir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

    let server = TestServer::new(|request| {
        if request.method == "GET" && request.path == "/wham/usage" {
            thread::sleep(Duration::from_millis(300));
            return HttpResponse::new(200, wham_usage_ok_body());
        }
        HttpResponse::new(404, "")
    })
    .expect("server");

    let fetched_at = now_epoch().saturating_sub(10).max(1);
    write_prompt_segment_cache_kv(
        &cache_root,
        "alpha",
        &format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=1\nweekly_remaining=2\nweekly_reset_epoch=1700600000\n"
        ),
    );

    let mut command = Command::new("/bin/bash");
    command
        .arg("-c")
        .arg(
            r#"
set -uo pipefail
output="$("$CODEX_CLI_BIN" prompt-segment --ttl 1s --time-format '%Y-%m-%dT%H:%MZ')"
printf '%s\n' "$output"
kill -HUP 0
"#,
        )
        .env("CODEX_CLI_BIN", codex_cli_bin())
        .env("CODEX_AUTH_FILE", auth_file.as_path())
        .env("CODEX_SECRET_DIR", secrets.as_path())
        .env("ZSH_CACHE_DIR", cache_root.as_path())
        .env("CODEX_PROMPT_SEGMENT_ENABLED", "true")
        .env("CODEX_CHATGPT_BASE_URL", server.url())
        .env("CODEX_PROMPT_SEGMENT_STALE_SUFFIX", " (STALE)")
        .env("CODEX_PROMPT_SEGMENT_REFRESH_MIN_SECONDS", "0")
        .env("CODEX_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS", "1")
        .env("CODEX_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS", "3")
        .env("NO_COLOR", "1")
        .env("TZ", "UTC")
        .env_remove("STARSHIP_SESSION_KEY")
        .env_remove("STARSHIP_SHELL")
        .stdin(Stdio::null())
        .process_group(0);

    let output = command.output().expect("run prompt shell");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "alpha 5h:1% W:2% 2023-11-21T20:53Z (STALE)\n");

    let kv_path = cache_file(&cache_root, "alpha");
    assert!(
        wait_for_file_contains(&kv_path, "weekly_remaining=88", Duration::from_secs(3)),
        "expected detached background refresh to survive prompt shell HUP and update cache kv"
    );
}

#[test]
fn prompt_segment_stale_cache_401_refreshes_auth_in_background() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

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

    let server = TestServer::new(|request| {
        if request.header_value("authorization").as_deref() == Some("Bearer remote-access-token") {
            HttpResponse::new(200, wham_usage_ok_body())
        } else {
            HttpResponse::new(401, "")
        }
    })
    .expect("server");

    let fetched_at = now_epoch().saturating_sub(10).max(1);
    write_prompt_segment_cache_kv(
        &cache_root,
        "alpha",
        &format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=1\nweekly_remaining=2\nweekly_reset_epoch=1700600000\n"
        ),
    );

    let remote_id_token = alpha_id_token();
    let remote_payload = format!(
        r#"{{"tokens":{{"access_token":"remote-access-token","id_token":"{remote_id_token}","account_id":"acct_001"}},"last_refresh":"2025-01-20T12:34:56Z"}}"#
    );
    let output = run_with_path_prepend(
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
            ("CODEX_AUTO_REFRESH_ENABLED", "true"),
            ("CODEX_AUTH_REMOTE_SSH", "auth-host"),
            ("CODEX_AUTH_REMOTE_NAME", "gamania"),
            ("CODEX_AUTH_REMOTE_REFRESH", "true"),
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_PROMPT_SEGMENT_STALE_SUFFIX", " (STALE)"),
            ("CODEX_PROMPT_SEGMENT_REFRESH_MIN_SECONDS", "0"),
            ("CODEX_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS", "3"),
            ("REMOTE_AUTH_PAYLOAD", remote_payload.as_str()),
            ("SSH_ARGS_FILE", args_file.to_str().expect("args path")),
        ],
        &stubs,
    );
    assert_exit(&output, 0);
    assert_eq!(
        stdout(&output),
        "alpha 5h:1% W:2% 2023-11-21T20:53Z (STALE)\n"
    );

    let kv_path = cache_file(&cache_root, "alpha");
    let refreshed = wait_for_file_contains(&kv_path, "weekly_remaining=88", Duration::from_secs(3));
    let requests = server.take_requests();
    let captured_args =
        fs::read_to_string(&args_file).unwrap_or_else(|err| format!("<missing: {err}>"));
    let auth_content =
        fs::read_to_string(&auth_file).unwrap_or_else(|err| format!("<missing: {err}>"));
    let cache_content =
        fs::read_to_string(&kv_path).unwrap_or_else(|err| format!("<missing: {err}>"));
    assert!(
        refreshed,
        "expected background refresh to refresh auth after 401 and update cache kv\nrequests={requests:#?}\nssh_args={captured_args}\nauth={auth_content}\ncache={cache_content}"
    );

    assert!(captured_args.contains("--name alpha"));
    assert!(!captured_args.contains("--name gamania"));
    assert!(captured_args.contains("--refresh"));

    let usage_requests = requests
        .iter()
        .filter(|request| request.method == "GET" && request.path == "/wham/usage")
        .collect::<Vec<_>>();
    assert_eq!(usage_requests.len(), 2);
    assert_eq!(
        usage_requests[0].header_value("authorization").as_deref(),
        Some("Bearer tok")
    );
    assert_eq!(
        usage_requests[1].header_value("authorization").as_deref(),
        Some("Bearer remote-access-token")
    );
}

#[test]
fn prompt_segment_refresh_401_suppresses_auth_refresh_output() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

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

    let server = TestServer::new(|request| {
        if request.header_value("authorization").as_deref() == Some("Bearer remote-access-token") {
            HttpResponse::new(200, wham_usage_ok_body())
        } else {
            HttpResponse::new(401, "")
        }
    })
    .expect("server");

    let remote_id_token = alpha_id_token();
    let remote_payload = format!(
        r#"{{"tokens":{{"access_token":"remote-access-token","id_token":"{remote_id_token}","account_id":"acct_001"}},"last_refresh":"2025-01-20T12:34:56Z"}}"#
    );
    let output = run_with_path_prepend(
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
            ("CODEX_AUTO_REFRESH_ENABLED", "true"),
            ("CODEX_AUTH_REMOTE_SSH", "auth-host"),
            ("CODEX_AUTH_REMOTE_NAME", "gamania"),
            ("CODEX_AUTH_REMOTE_REFRESH", "true"),
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS", "3"),
            ("REMOTE_AUTH_PAYLOAD", remote_payload.as_str()),
            ("SSH_ARGS_FILE", args_file.to_str().expect("args path")),
        ],
        &stubs,
    );
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "alpha 5h:94% W:88% 2023-11-21T20:53Z\n");

    let captured_args = fs::read_to_string(&args_file).expect("read ssh args");
    assert!(captured_args.contains("--name alpha"));
    assert!(!captured_args.contains("--name gamania"));
    assert!(captured_args.contains("--refresh"));
}

#[test]
fn prompt_segment_stale_cache_401_respects_disabled_auto_refresh() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");
    let (auth_file, secrets, cache_root) = write_auth_and_secret(&dir);

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

    let server = TestServer::new(|request| {
        if request.header_value("authorization").as_deref() == Some("Bearer remote-access-token") {
            HttpResponse::new(200, wham_usage_ok_body())
        } else {
            HttpResponse::new(401, "")
        }
    })
    .expect("server");

    let fetched_at = now_epoch().saturating_sub(10).max(1);
    write_prompt_segment_cache_kv(
        &cache_root,
        "alpha",
        &format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=1\nweekly_remaining=2\nweekly_reset_epoch=1700600000\n"
        ),
    );

    let remote_id_token = alpha_id_token();
    let remote_payload = format!(
        r#"{{"tokens":{{"access_token":"remote-access-token","id_token":"{remote_id_token}","account_id":"acct_001"}},"last_refresh":"2025-01-20T12:34:56Z"}}"#
    );
    let output = run_with_path_prepend(
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
            ("CODEX_AUTO_REFRESH_ENABLED", "false"),
            ("CODEX_AUTH_REMOTE_SSH", "auth-host"),
            ("CODEX_AUTH_REMOTE_NAME", "gamania"),
            ("CODEX_AUTH_REMOTE_REFRESH", "true"),
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_PROMPT_SEGMENT_STALE_SUFFIX", " (STALE)"),
            ("CODEX_PROMPT_SEGMENT_REFRESH_MIN_SECONDS", "0"),
            ("CODEX_PROMPT_SEGMENT_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_PROMPT_SEGMENT_CURL_MAX_TIME_SECONDS", "3"),
            ("REMOTE_AUTH_PAYLOAD", remote_payload.as_str()),
            ("SSH_ARGS_FILE", args_file.to_str().expect("args path")),
        ],
        &stubs,
    );
    assert_exit(&output, 0);
    assert_eq!(
        stdout(&output),
        "alpha 5h:1% W:2% 2023-11-21T20:53Z (STALE)\n"
    );

    let requests = collect_requests_for(&server, Duration::from_secs(1));
    let usage_requests = requests
        .iter()
        .filter(|request| request.method == "GET" && request.path == "/wham/usage")
        .collect::<Vec<_>>();
    assert_eq!(usage_requests.len(), 1);
    assert_eq!(
        usage_requests[0].header_value("authorization").as_deref(),
        Some("Bearer tok")
    );
    assert!(
        !args_file.exists(),
        "disabled auto-refresh must not invoke remote auth"
    );

    let kv_path = cache_file(&cache_root, "alpha");
    let cache_content = fs::read_to_string(&kv_path).expect("read cache");
    assert!(cache_content.contains("weekly_remaining=2"));
    assert!(!cache_content.contains("weekly_remaining=88"));
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

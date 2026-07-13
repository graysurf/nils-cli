use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::http::{HttpResponse, LoopbackServer, TestServer};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
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
    let bin = codex_cli_bin();
    cmd::run_with(&bin, args, &options)
}

fn run_in_dir(
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &Path)],
    vars: &[(&str, &str)],
) -> CmdOutput {
    let mut options = CmdOptions::default();
    for (key, path) in envs {
        let value = path.to_string_lossy();
        options = options.with_env(key, value.as_ref());
    }
    for (key, value) in vars {
        options = options.with_env(key, value);
    }
    let bin = codex_cli_bin();
    cmd::run_in_dir_with(dir, &bin, args, &options)
}

fn stderr(output: &CmdOutput) -> String {
    output.stderr_text()
}

fn stdout(output: &CmdOutput) -> String {
    output.stdout_text()
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(output.code, code);
}

fn cache_kv_path(cache_root: &Path, key: &str) -> PathBuf {
    cache_root
        .join("codex")
        .join("prompt-segment-rate-limits")
        .join(format!("{key}.kv"))
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

#[cfg(unix)]
fn assert_partial_live_ignores_invalid_reset_cache(fetched_at: i64) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    fs::write(
        secret_dir.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok-alpha","account_id":"acct_001"}}"#,
    )
    .expect("write alpha");

    let cache_root = dir.path().join("cache_root");
    let kv_path = cache_kv_path(&cache_root, "alpha");
    let cache_dir = kv_path.parent().expect("cache parent");
    fs::create_dir_all(cache_dir).expect("cache dir");
    let reset_epoch = now_epoch().saturating_add(900_000);
    fs::write(
        &kv_path,
        format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=91\nnon_weekly_reset_epoch={reset_epoch}\n"
        ),
    )
    .expect("write cache");

    // Keep the invalid entry readable while preventing the successful live
    // request from replacing it before collect_async_round performs reset
    // metadata backfill.
    let mut read_only = fs::metadata(cache_dir)
        .expect("cache metadata")
        .permissions();
    read_only.set_mode(0o555);
    fs::set_permissions(cache_dir, read_only).expect("make cache read-only");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(
            200,
            r#"{
  "rate_limit": {
    "primary_window": { "limit_window_seconds": 18000, "used_percent": 6, "reset_at": 0 },
    "secondary_window": null
  }
}"#,
        ),
    );

    let output = run(
        &["diag", "rate-limits", "--async"],
        &[
            ("CODEX_SECRET_DIR", &secret_dir),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
        ],
    );

    let mut writable = fs::metadata(cache_dir)
        .expect("cache metadata")
        .permissions();
    writable.set_mode(0o700);
    fs::set_permissions(cache_dir, writable).expect("restore cache permissions");

    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(out.contains("94%"), "expected live 5h value, got:\n{out}");
    assert!(
        !out.contains("10d"),
        "invalid cached reset metadata leaked into live row:\n{out}"
    );
}

#[cfg(unix)]
#[test]
fn rate_limits_async_partial_live_does_not_backfill_expired_cache_reset() {
    assert_partial_live_ignores_invalid_reset_cache(now_epoch().saturating_sub(600));
}

#[cfg(unix)]
#[test]
fn rate_limits_async_partial_live_does_not_backfill_future_invalid_cache_reset() {
    assert_partial_live_ignores_invalid_reset_cache(now_epoch().saturating_add(30));
}

#[test]
fn rate_limits_async_json_one_line_conflict_is_structured() {
    let output = run(
        &["diag", "rate-limits", "--async", "--json", "--one-line"],
        &[],
        &[],
    );
    assert_exit(&output, 64);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.diag.rate-limits.v1");
    assert_eq!(payload["command"], "diag rate-limits");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "invalid-flag-combination");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--async does not support --one-line")
    );
}

#[test]
fn rate_limits_async_json_positional_arg_is_structured() {
    let output = run(
        &["diag", "rate-limits", "--async", "--json", "alpha.json"],
        &[],
        &[],
    );
    assert_exit(&output, 64);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.diag.rate-limits.v1");
    assert_eq!(payload["command"], "diag rate-limits");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "invalid-positional-arg");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--async does not accept positional args")
    );
}

#[test]
fn rate_limits_async_json_cached_clear_cache_conflict_is_structured() {
    let output = run(
        &["diag", "rate-limits", "--async", "--json", "--cached", "-c"],
        &[],
        &[],
    );
    assert_exit(&output, 64);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.diag.rate-limits.v1");
    assert_eq!(payload["command"], "diag rate-limits");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "invalid-flag-combination");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("-c is not compatible with --cached")
    );
}

#[test]
fn rate_limits_async_json_missing_secret_dir_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let missing = dir.path().join("missing");

    let output = run(
        &["diag", "rate-limits", "--async", "--format", "json"],
        &[("CODEX_SECRET_DIR", &missing)],
        &[("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false")],
    );
    assert_exit(&output, 1);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.diag.rate-limits.v1");
    assert_eq!(payload["command"], "diag rate-limits");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "secret-discovery-failed");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("CODEX_SECRET_DIR not found")
    );
}

#[test]
fn rate_limits_async_json_outputs_results() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    fs::write(
        secret_dir.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok-alpha","account_id":"acct_001"}}"#,
    )
    .expect("write alpha");
    fs::write(
        secret_dir.join("beta.json"),
        r#"{"tokens":{"access_token":"tok-beta","account_id":"acct_002"}}"#,
    )
    .expect("write beta");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(
            200,
            r#"{
  "rate_limit": {
    "primary_window": { "limit_window_seconds": 18000, "used_percent": 6, "reset_at": 1700003600 },
    "secondary_window": { "limit_window_seconds": 604800, "used_percent": 12, "reset_at": 1700600000 }
  }
}"#,
        ),
    );

    let output = run(
        &["diag", "rate-limits", "--async", "--json"],
        &[("CODEX_SECRET_DIR", &secret_dir)],
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
    assert_eq!(payload["mode"], "async");
    assert_eq!(payload["ok"], true);
    let results = payload["results"].as_array().expect("results");
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|entry| entry["ok"] == true));
}

#[test]
fn rate_limits_async_one_line_conflict() {
    let output = run(&["diag", "rate-limits", "--async", "--one-line"], &[], &[]);
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("--async does not support --one-line"));
}

#[test]
fn rate_limits_watch_requires_async() {
    let output = run(&["diag", "rate-limits", "--watch"], &[], &[]);
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("--async"));
}

#[test]
fn rate_limits_async_watch_renders_last_update_timestamp() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    fs::write(
        secret_dir.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok-alpha","account_id":"acct_001"}}"#,
    )
    .expect("write alpha");

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(
            200,
            r#"{
  "rate_limit": {
    "primary_window": { "limit_window_seconds": 18000, "used_percent": 6, "reset_at": 1700003600 },
    "secondary_window": { "limit_window_seconds": 604800, "used_percent": 12, "reset_at": 1700600000 }
  }
}"#,
        ),
    );

    let output = run(
        &["diag", "rate-limits", "--async", "--watch"],
        &[
            ("CODEX_SECRET_DIR", &secret_dir),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("CODEX_RATE_LIMITS_WATCH_MAX_ROUNDS", "1"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);

    let out = stdout(&output);
    assert!(out.contains("🚦 Codex rate limits for all accounts"));
    assert!(out.contains("alpha"));
    assert!(out.contains("Last update: "));
}

#[test]
fn rate_limits_async_watch_rescans_secrets_and_updates_last_rendered_rows() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secret dir");

    let alpha_json = r#"{"tokens":{"access_token":"tok-alpha","account_id":"acct_001"}}"#;
    let beta_json = r#"{"tokens":{"access_token":"tok-beta","account_id":"acct_002"}}"#;
    fs::write(secret_dir.join("alpha.json"), alpha_json).expect("write alpha");

    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, alpha_json).expect("write auth");

    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&cache_root).expect("cache root");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(
            200,
            r#"{
  "rate_limit": {
    "primary_window": { "limit_window_seconds": 18000, "used_percent": 6, "reset_at": 1700003600 },
    "secondary_window": { "limit_window_seconds": 604800, "used_percent": 12, "reset_at": 1700600000 }
  }
}"#,
        ),
    );

    let secret_dir_for_update = secret_dir.clone();
    let auth_file_for_update = auth_file.clone();
    let updater = thread::spawn(move || {
        thread::sleep(Duration::from_millis(500));
        fs::remove_file(secret_dir_for_update.join("alpha.json")).expect("remove alpha");
        fs::write(secret_dir_for_update.join("beta.json"), beta_json).expect("write beta");
        fs::write(auth_file_for_update, beta_json).expect("switch auth");
    });

    let output = run(
        &["diag", "rate-limits", "--async", "--watch"],
        &[
            ("CODEX_SECRET_DIR", &secret_dir),
            ("CODEX_AUTH_FILE", &auth_file),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("CODEX_RATE_LIMITS_WATCH_MAX_ROUNDS", "2"),
            ("CODEX_RATE_LIMITS_WATCH_INTERVAL_SECONDS", "2"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );

    updater.join().expect("updater join");
    assert_exit(&output, 0);

    let out = stdout(&output);
    let last_render_start = out
        .rfind("🚦 Codex rate limits for all accounts")
        .expect("last render start");
    let last_render = &out[last_render_start..];
    assert!(last_render.contains("beta"));
    assert!(!last_render.contains("alpha"));
    assert!(cache_kv_path(&cache_root, "beta").is_file());
}

#[test]
fn rate_limits_async_jobs_zero_defaults() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secret dir");

    let output = run(
        &["diag", "rate-limits", "--async", "--jobs", "0"],
        &[("CODEX_SECRET_DIR", &secret_dir)],
        &[],
    );
    assert_exit(&output, 1);
    let err = stderr(&output);
    assert!(err.contains("no secrets found"));
    assert!(!err.contains("invalid --jobs value"));
}

#[test]
fn rate_limits_async_missing_secret_dir() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let missing = dir.path().join("missing");

    let output = run(
        &["diag", "rate-limits", "--async"],
        &[("CODEX_SECRET_DIR", &missing)],
        &[],
    );
    assert_exit(&output, 1);
    assert!(stderr(&output).contains("CODEX_SECRET_DIR not found"));
}

#[test]
fn rate_limits_async_rejects_positional_secret_arg() {
    let output = run(&["diag", "rate-limits", "--async", "alpha.json"], &[], &[]);
    assert_exit(&output, 64);
    let err = stderr(&output);
    assert!(err.contains("--async does not accept positional args"));
    assert!(err.contains("hint: async always queries all secrets under CODEX_SECRET_DIR"));
}

#[test]
fn rate_limits_async_rejects_cached_clear_cache_combo() {
    let output = run(
        &["diag", "rate-limits", "--async", "--cached", "-c"],
        &[],
        &[],
    );
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("--async: -c is not compatible with --cached"));
}

#[test]
fn rate_limits_async_clear_cache_failure_reports_error() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    let working_dir = tempfile::TempDir::new().expect("working dir");

    let output = run_in_dir(
        working_dir.path(),
        &["diag", "rate-limits", "--async", "-c"],
        &[("CODEX_SECRET_DIR", &secret_dir)],
        &[("ZSH_CACHE_DIR", "relative-cache")],
    );
    assert_exit(&output, 1);
    assert!(stderr(&output).contains("refusing to clear cache"));
}

#[test]
fn rate_limits_async_json_clear_cache_failure_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    let working_dir = tempfile::TempDir::new().expect("working dir");

    let output = run_in_dir(
        working_dir.path(),
        &["diag", "rate-limits", "--async", "--json", "-c"],
        &[("CODEX_SECRET_DIR", &secret_dir)],
        &[
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("ZSH_CACHE_DIR", "relative-cache"),
        ],
    );
    assert_exit(&output, 1);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "cache-clear-failed");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("refusing to clear cache")
    );
}

#[test]
fn rate_limits_async_json_falls_back_to_cache_for_missing_access_token() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    fs::write(
        secret_dir.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok-alpha","account_id":"acct_001"}}"#,
    )
    .expect("write alpha");
    fs::write(
        secret_dir.join("beta.json"),
        r#"{"tokens":{"account_id":"acct_002"}}"#,
    )
    .expect("write beta");

    let cache_root = dir.path().join("cache_root");
    let kv_path = cache_kv_path(&cache_root, "beta");
    fs::create_dir_all(kv_path.parent().expect("cache parent")).expect("cache dir");
    let fetched_at = now_epoch().saturating_sub(300);
    fs::write(
        &kv_path,
        format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=91\nweekly_remaining=70\nweekly_reset_epoch=1700600000\n"
        ),
    )
    .expect("write beta cache");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(
            200,
            r#"{
  "rate_limit": {
    "primary_window": { "limit_window_seconds": 18000, "used_percent": 6, "reset_at": 1700003600 },
    "secondary_window": { "limit_window_seconds": 604800, "used_percent": 12, "reset_at": 1700600000 }
  }
}"#,
        ),
    );

    let output = run(
        &["diag", "rate-limits", "--async", "--json"],
        &[
            ("CODEX_SECRET_DIR", &secret_dir),
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
    assert_eq!(payload["mode"], "async");
    assert_eq!(payload["ok"], true);

    let results = payload["results"].as_array().expect("results");
    assert_eq!(results.len(), 2);

    let alpha = results
        .iter()
        .find(|entry| entry["target_file"] == "alpha.json")
        .expect("alpha result");
    assert_eq!(alpha["ok"], true);
    assert_eq!(alpha["source"], "network");
    assert!(alpha["raw_usage"]["rate_limit"].is_object());

    let beta = results
        .iter()
        .find(|entry| entry["target_file"] == "beta.json")
        .expect("beta result");
    assert_eq!(beta["ok"], true);
    assert_eq!(beta["source"], "cache-fallback");
    assert_eq!(beta["summary"]["non_weekly_label"], "5h");
    assert_eq!(beta["summary"]["non_weekly_remaining"], 91);
    assert!(beta["raw_usage"].is_null());
}

#[test]
fn rate_limits_async_json_partial_failure_keeps_results_array() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    fs::create_dir_all(&cache_root).expect("cache root");
    fs::write(
        secret_dir.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok-alpha","account_id":"acct_001"}}"#,
    )
    .expect("write alpha");
    fs::write(
        secret_dir.join("beta.json"),
        r#"{"tokens":{"access_token":"tok-beta","account_id":"acct_002"}}"#,
    )
    .expect("write beta");

    let server = TestServer::new(|request| {
        if request
            .header_value("authorization")
            .as_deref()
            .is_some_and(|value| value.contains("tok-beta"))
        {
            return HttpResponse::new(500, r#"{"error":"simulated failure"}"#);
        }
        HttpResponse::new(
            200,
            r#"{
  "rate_limit": {
    "primary_window": { "limit_window_seconds": 18000, "used_percent": 6, "reset_at": 1700003600 },
    "secondary_window": { "limit_window_seconds": 604800, "used_percent": 12, "reset_at": 1700600000 }
  }
}"#,
        )
    })
    .expect("server");

    let output = run(
        &["diag", "rate-limits", "--async", "--json"],
        &[
            ("CODEX_SECRET_DIR", &secret_dir),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
        ],
    );
    assert_exit(&output, 1);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["mode"], "async");
    assert_eq!(payload["ok"], false);
    let results = payload["results"].as_array().expect("results");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["name"], "alpha");
    assert_eq!(results[1]["name"], "beta");

    let alpha = results
        .iter()
        .find(|entry| entry["target_file"] == "alpha.json")
        .expect("alpha result");
    assert_eq!(alpha["ok"], true);
    assert_eq!(alpha["source"], "network");

    let beta = results
        .iter()
        .find(|entry| entry["target_file"] == "beta.json")
        .expect("beta result");
    assert_eq!(beta["ok"], false);
    assert_eq!(beta["source"], "network");
    assert_eq!(beta["error"]["code"], "request-failed");
    assert!(beta["error"]["message"].is_string());
}

// A valid 200 usage payload whose `rate_limit` is explicitly null. The ChatGPT
// backend returns this when there is no usage recorded in the current window;
// it is a benign "no active window" state, not a malformed payload.
const NULL_RATE_LIMIT_BODY: &str = r#"{
  "plan_type": "pro",
  "rate_limit": null,
  "code_review_rate_limit": null,
  "additional_rate_limits": null
}"#;

const EMPTY_RATE_LIMIT_OBJECT_BODY: &str = r#"{
  "plan_type": "pro",
  "rate_limit": {
    "primary_window": null,
    "secondary_window": null
  }
}"#;

#[test]
fn rate_limits_async_empty_window_object_preserves_fallback_cache_and_metadata() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    let secret_file = secret_dir.join("alpha.json");
    let secret_json = r#"{
  "tokens": {"access_token":"tok-alpha","account_id":"acct_001"},
  "codex_rate_limits": {
    "weekly_reset_at_epoch": 1700600000,
    "weekly_reset_at": "2023-11-21T20:53:20Z",
    "weekly_fetched_at": "2023-11-14T22:13:20Z",
    "non_weekly_reset_at_epoch": 1700003600,
    "non_weekly_reset_at": "2023-11-14T23:13:20Z"
  }
}"#;
    fs::write(&secret_file, secret_json).expect("write alpha");

    let cache_root = dir.path().join("cache_root");
    let kv_path = cache_kv_path(&cache_root, "alpha");
    fs::create_dir_all(kv_path.parent().expect("cache parent")).expect("cache dir");
    let fetched_at = now_epoch().saturating_sub(300);
    let cache = format!(
        "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=91\nnon_weekly_reset_epoch=1700003600\nweekly_remaining=70\nweekly_reset_epoch=1700600000\n"
    );
    fs::write(&kv_path, &cache).expect("write alpha cache");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, EMPTY_RATE_LIMIT_OBJECT_BODY),
    );

    let output = run(
        &["diag", "rate-limits", "--async"],
        &[
            ("CODEX_SECRET_DIR", &secret_dir),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
        ],
    );

    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(out.contains("91%"), "expected cached 5h value, got:\n{out}");
    assert!(
        out.contains("70%"),
        "expected cached weekly value, got:\n{out}"
    );
    assert!(
        out.contains("(stale)"),
        "expected stale marker, got:\n{out}"
    );
    assert_eq!(
        fs::read_to_string(&secret_file).expect("read alpha"),
        secret_json
    );
    assert_eq!(fs::read_to_string(&kv_path).expect("read cache"), cache);
}

#[test]
fn rate_limits_async_text_null_payload_serves_stale_cache() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    fs::write(
        secret_dir.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok-alpha","account_id":"acct_001"}}"#,
    )
    .expect("write alpha");

    // Last successful fetch is well past the TTL, i.e. genuinely stale.
    let cache_root = dir.path().join("cache_root");
    let kv_path = cache_kv_path(&cache_root, "alpha");
    fs::create_dir_all(kv_path.parent().expect("cache parent")).expect("cache dir");
    let fetched_at = now_epoch().saturating_sub(300);
    fs::write(
        &kv_path,
        format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=91\nnon_weekly_reset_epoch=1700003600\nweekly_remaining=70\nweekly_reset_epoch=1700600000\n"
        ),
    )
    .expect("write alpha cache");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, NULL_RATE_LIMIT_BODY),
    );

    let output = run(
        &["diag", "rate-limits", "--async"],
        &[
            ("CODEX_SECRET_DIR", &secret_dir),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
        ],
    );

    // A null window is benign: degrade to the last-known cached values instead
    // of failing the whole command.
    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(out.contains("91%"), "expected cached 5h value, got:\n{out}");
    assert!(
        out.contains("70%"),
        "expected cached weekly value, got:\n{out}"
    );
    assert!(
        out.contains("(stale)"),
        "expected stale marker, got:\n{out}"
    );
    assert!(
        !out.contains("invalid usage payload"),
        "null window must not be reported as a malformed payload, got:\n{out}"
    );
}

#[test]
fn rate_limits_async_text_null_payload_without_cache_shows_na() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    fs::create_dir_all(&cache_root).expect("cache root");
    fs::write(
        secret_dir.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok-alpha","account_id":"acct_001"}}"#,
    )
    .expect("write alpha");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, NULL_RATE_LIMIT_BODY),
    );

    let output = run(
        &["diag", "rate-limits", "--async"],
        &[
            ("CODEX_SECRET_DIR", &secret_dir),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
        ],
    );

    // No cache to fall back to, but a null window is still benign: report n/a
    // rather than a hard failure.
    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(out.contains("n/a"), "expected n/a marker, got:\n{out}");
}

#[test]
fn rate_limits_async_json_null_payload_is_benign() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    let cache_root = dir.path().join("cache_root");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    fs::create_dir_all(&cache_root).expect("cache root");
    fs::write(
        secret_dir.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok-alpha","account_id":"acct_001"}}"#,
    )
    .expect("write alpha");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, NULL_RATE_LIMIT_BODY),
    );

    let output = run(
        &["diag", "rate-limits", "--async", "--json"],
        &[
            ("CODEX_SECRET_DIR", &secret_dir),
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
    assert_eq!(payload["ok"], true);
    let results = payload["results"].as_array().expect("results");
    assert_eq!(results.len(), 1);
    let alpha = &results[0];
    assert_eq!(alpha["ok"], true);
    assert_eq!(alpha["status"], "ok");
    assert!(alpha["error"].is_null());
    assert!(alpha["summary"].is_null());
}

#[test]
fn rate_limits_async_json_null_payload_serves_stale_cache() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secret dir");
    fs::write(
        secret_dir.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok-alpha","account_id":"acct_001"}}"#,
    )
    .expect("write alpha");

    let cache_root = dir.path().join("cache_root");
    let kv_path = cache_kv_path(&cache_root, "alpha");
    fs::create_dir_all(kv_path.parent().expect("cache parent")).expect("cache dir");
    let fetched_at = now_epoch().saturating_sub(300);
    fs::write(
        &kv_path,
        format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=91\nweekly_remaining=70\nweekly_reset_epoch=1700600000\n"
        ),
    )
    .expect("write alpha cache");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(200, NULL_RATE_LIMIT_BODY),
    );

    let output = run(
        &["diag", "rate-limits", "--async", "--json"],
        &[
            ("CODEX_SECRET_DIR", &secret_dir),
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
    assert_eq!(payload["ok"], true);
    let results = payload["results"].as_array().expect("results");
    let alpha = &results[0];
    assert_eq!(alpha["ok"], true);
    assert_eq!(alpha["source"], "cache-fallback");
    assert_eq!(alpha["summary"]["non_weekly_remaining"], 91);
}

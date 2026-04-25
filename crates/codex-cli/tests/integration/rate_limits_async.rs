use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::http::{HttpResponse, LoopbackServer, TestServer};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

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
    assert!(stderr(&output).contains("--watch requires --async"));
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
    fs::write(
        &kv_path,
        "fetched_at=1700000000\nnon_weekly_label=5h\nnon_weekly_remaining=91\nweekly_remaining=70\nweekly_reset_epoch=1700600000\n",
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

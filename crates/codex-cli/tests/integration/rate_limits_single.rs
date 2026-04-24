use chrono::Utc;
use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::http::{HttpResponse, LoopbackServer};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

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
fn rate_limits_single_json_one_line_conflict() {
    let output = run(
        &["diag", "rate-limits", "--json", "--one-line"],
        &[],
        &[("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false")],
    );
    assert_exit(&output, 64);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "invalid-flag-combination");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--one-line is not compatible with --json")
    );
}

#[test]
fn rate_limits_single_cached_missing_cache() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"tok"}}"#).expect("write auth");

    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).expect("cache dir");

    let output = run(
        &["diag", "rate-limits", "--cached"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_CACHE_DIR", &cache_dir),
        ],
        &[("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false")],
    );
    assert_exit(&output, 1);
    assert!(stderr(&output).contains("cache not found"));
}

#[test]
fn rate_limits_single_cached_json_conflict() {
    let output = run(
        &["diag", "rate-limits", "--cached", "--json"],
        &[],
        &[("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false")],
    );
    assert_exit(&output, 64);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "invalid-flag-combination");
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--json is not supported with --cached")
    );
}

#[test]
fn rate_limits_single_cached_clear_cache_conflict() {
    let output = run(
        &["diag", "rate-limits", "--cached", "-c"],
        &[],
        &[("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false")],
    );
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("-c is not compatible with --cached"));
}

#[test]
fn rate_limits_single_json_target_not_found_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");

    let output = run(
        &["diag", "rate-limits", "--json", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
        &[("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false")],
    );
    assert_exit(&output, 1);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "target-not-found");
}

#[test]
fn rate_limits_single_cached_success_reads_cache() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(
        secrets.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok","account_id":"acct_001"}}"#,
    )
    .expect("alpha");

    let cache_root = dir.path().join("cache_root");
    let kv_path = cache_kv_path(&cache_root, "alpha");
    fs::create_dir_all(kv_path.parent().expect("cache parent")).expect("cache dir");
    let fetched_at = Utc::now().timestamp();
    fs::write(
        &kv_path,
        format!(
            "fetched_at={fetched_at}\nnon_weekly_label=5h\nnon_weekly_remaining=94\nweekly_remaining=88\nweekly_reset_epoch=1700600000\n"
        ),
    )
    .expect("kv");

    let output = run(
        &["diag", "rate-limits", "--cached", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("TZ", "UTC"),
            ("NO_COLOR", "1"),
        ],
    );
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "5h:94% W:88% 11-21 20:53\n");
}

#[test]
fn rate_limits_single_cached_stale_cache_is_rejected() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(
        secrets.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok","account_id":"acct_001"}}"#,
    )
    .expect("alpha");

    let cache_root = dir.path().join("cache_root");
    let kv_path = cache_kv_path(&cache_root, "alpha");
    fs::create_dir_all(kv_path.parent().expect("cache parent")).expect("cache dir");
    fs::write(
        &kv_path,
        "fetched_at=1\nnon_weekly_label=5h\nnon_weekly_remaining=94\nweekly_remaining=88\nweekly_reset_epoch=1700600000\n",
    )
    .expect("kv");

    let output = run(
        &["diag", "rate-limits", "--cached", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("ZSH_CACHE_DIR", &cache_root),
        ],
        &[("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false")],
    );
    assert_exit(&output, 1);
    assert!(stderr(&output).contains("cache expired"));
}

#[test]
fn rate_limits_single_json_missing_access_token_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(
        secrets.join("alpha.json"),
        r#"{"tokens":{"account_id":"acct_001"}}"#,
    )
    .expect("alpha");

    let output = run(
        &["diag", "rate-limits", "--json", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
        &[("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false")],
    );
    assert_exit(&output, 2);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "missing-access-token");
}

#[test]
fn rate_limits_single_text_missing_access_token_reports_stderr() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(
        secrets.join("alpha.json"),
        r#"{"tokens":{"account_id":"acct_001"}}"#,
    )
    .expect("alpha");

    let output = run(
        &["diag", "rate-limits", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
        &[("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false")],
    );
    assert_exit(&output, 2);
    assert!(stderr(&output).contains("missing access_token"));
}

#[test]
fn rate_limits_single_json_request_failed_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(
        secrets.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok","account_id":"acct_001"}}"#,
    )
    .expect("alpha");

    let output = run(
        &["diag", "rate-limits", "--json", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
        &[
            ("CODEX_CHATGPT_BASE_URL", "http://127.0.0.1:9/"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
        ],
    );
    assert_exit(&output, 3);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "request-failed");
}

#[test]
fn rate_limits_single_json_invalid_usage_payload_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(
        secrets.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok","account_id":"acct_001"}}"#,
    )
    .expect("alpha");

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(
            200,
            r#"{"rate_limit":{"primary_window":{"limit_window_seconds":18000}}}"#,
        ),
    );

    let output = run(
        &["diag", "rate-limits", "--json", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
        ],
    );
    assert_exit(&output, 3);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "invalid-usage-payload");
    assert!(payload["error"]["details"]["raw_usage"].is_object());
}

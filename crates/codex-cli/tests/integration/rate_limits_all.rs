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

fn run_with_options(args: &[&str], options: &CmdOptions) -> CmdOutput {
    let bin = codex_cli_bin();
    cmd::run_with(&bin, args, options)
}

fn stdout(output: &CmdOutput) -> String {
    output.stdout_text()
}

fn stderr(output: &CmdOutput) -> String {
    output.stderr_text()
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(output.code, code);
}

#[test]
fn rate_limits_all_missing_secret_dir() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let missing = dir.path().join("missing");

    let output = run(
        &["diag", "rate-limits", "--all"],
        &[("CODEX_SECRET_DIR", &missing)],
        &[],
    );
    assert_exit(&output, 1);
    assert!(stderr(&output).contains("CODEX_SECRET_DIR not found"));
}

#[test]
fn rate_limits_all_json_missing_secret_dir_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let missing = dir.path().join("missing");

    let output = run(
        &["diag", "rate-limits", "--all", "--format", "json"],
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
fn rate_limits_all_json_empty_secret_dir_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");

    let output = run(
        &["diag", "rate-limits", "--all", "--json"],
        &[("CODEX_SECRET_DIR", &secrets)],
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
            .contains("no secrets found")
    );
}

#[test]
fn rate_limits_all_json_outputs_results() {
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
        &["diag", "rate-limits", "--all", "--json"],
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
    assert_eq!(payload["mode"], "all");
    assert_eq!(payload["ok"], true);
    let results = payload["results"].as_array().expect("results");
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|entry| entry["ok"] == true));
    assert!(
        results
            .iter()
            .all(|entry| entry["raw_usage"]["rate_limit"].is_object())
    );
}

#[test]
fn rate_limits_all_json_falls_back_to_official_codex_auth_file() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let home = dir.path().join("home");
    let codex_home = home.join(".codex");
    fs::create_dir_all(&codex_home).expect("codex home");
    fs::write(
        codex_home.join("auth.json"),
        r#"{"access_token":"tok-official","account_id":"acct_official"}"#,
    )
    .expect("write official auth");

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

    let options = CmdOptions::new()
        .with_env("HOME", home.to_str().expect("home"))
        .with_env("CODEX_CHATGPT_BASE_URL", &server.url())
        .with_env("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false")
        .with_env("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1")
        .with_env("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3")
        .with_env_remove("CODEX_HOME")
        .with_env_remove("CODEX_SECRET_DIR")
        .with_env_remove("CODEX_AUTH_FILE");

    let output = run_with_options(
        &[
            "diag",
            "rate-limits",
            "--all",
            "--format",
            "json",
            "--no-refresh-auth",
        ],
        &options,
    );
    assert_exit(&output, 0);
    assert!(!stdout(&output).contains("tok-official"));
    assert!(!stdout(&output).contains(codex_home.to_str().expect("codex home")));

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.diag.rate-limits.v1");
    assert_eq!(payload["ok"], true);
    let results = payload["results"].as_array().expect("results");
    assert_eq!(results.len(), 1);
    let result = &results[0];
    assert_eq!(result["provider"], "codex");
    assert_eq!(result["target_file"], "auth.json");
    assert_eq!(result["ok"], true);
    assert_eq!(result["summary"]["weekly_remaining"], 88);
    assert_eq!(result["windows"][0]["label"], "5h");
    assert_eq!(result["windows"][0]["used_percent"], 6);
    assert_eq!(result["windows"][0]["remaining_percent"], 94);
    assert_eq!(result["windows"][1]["label"], "Weekly");
    assert_eq!(result["windows"][1]["used_percent"], 12);
    assert_eq!(result["windows"][1]["remaining_percent"], 88);
    assert!(
        !fs::read_to_string(codex_home.join("auth.json"))
            .expect("auth")
            .contains("codex_rate_limits")
    );

    let requests = server.take_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header_value("authorization"),
        Some("Bearer tok-official".to_string())
    );
    assert_eq!(
        requests[0].header_value("chatgpt-account-id"),
        Some("acct_official".to_string())
    );
}

#[test]
fn rate_limits_all_rejects_positional_secret_arg() {
    let output = run(&["diag", "rate-limits", "--all", "alpha.json"], &[], &[]);
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("usage: codex-rate-limits"));
}

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
fn rate_limits_all_rejects_positional_secret_arg() {
    let output = run(&["diag", "rate-limits", "--all", "alpha.json"], &[], &[]);
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("usage: codex-rate-limits"));
}

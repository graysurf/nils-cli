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

#[test]
fn diag_json_contract_single_failure_envelope_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(
        secrets.join("alpha.json"),
        r#"{"tokens":{"account_id":"acct_001"}}"#,
    )
    .expect("write secret");

    let output = run(
        &["diag", "rate-limits", "--json", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
        &[("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false")],
    );
    assert_eq!(output.code, 2);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.diag.rate-limits.v1");
    assert_eq!(payload["command"], "diag rate-limits");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "missing-access-token");
    assert!(payload["error"]["message"].is_string());
}

#[test]
fn diag_json_contract_all_partial_failure_keeps_results_array() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(
        secrets.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok-alpha","account_id":"acct_001"}}"#,
    )
    .expect("write alpha");
    fs::write(
        secrets.join("beta.json"),
        r#"{"tokens":{"account_id":"acct_002"}}"#,
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
        &[("CODEX_SECRET_DIR", &secrets)],
        &[
            ("CODEX_CHATGPT_BASE_URL", &server.url()),
            ("CODEX_RATE_LIMITS_DEFAULT_ALL_ENABLED", "false"),
            ("CODEX_RATE_LIMITS_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_RATE_LIMITS_CURL_MAX_TIME_SECONDS", "3"),
        ],
    );
    assert_eq!(output.code, 1);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.diag.rate-limits.v1");
    assert_eq!(payload["command"], "diag rate-limits");
    assert_eq!(payload["mode"], "all");
    assert_eq!(payload["ok"], false);
    let results = payload["results"].as_array().expect("results");
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|entry| entry["ok"] == true));
    assert!(results.iter().any(|entry| entry["ok"] == false));
}

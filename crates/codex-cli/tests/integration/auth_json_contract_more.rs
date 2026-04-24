use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

const HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
const PAYLOAD_ALPHA: &str = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";

fn token(payload: &str) -> String {
    format!("{HEADER}.{payload}.sig")
}

fn auth_json(payload: &str, account_id: &str, refresh_token: &str, last_refresh: &str) -> String {
    format!(
        r#"{{"tokens":{{"access_token":"{}","id_token":"{}","refresh_token":"{}","account_id":"{}"}},"last_refresh":"{}"}}"#,
        token(payload),
        token(payload),
        refresh_token,
        account_id,
        last_refresh
    )
}

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn run(args: &[&str], envs: &[(&str, &Path)]) -> CmdOutput {
    run_with(args, envs, &[])
}

fn run_with(args: &[&str], envs: &[(&str, &Path)], vars: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default();
    for (key, path) in envs {
        options = options.with_env(key, path.to_string_lossy().as_ref());
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

fn json(output: &CmdOutput) -> Value {
    serde_json::from_str(&stdout(output)).expect("json output")
}

#[test]
fn auth_json_contract_save_invalid_name_is_structured() {
    let invalid_name = run(&["auth", "save", "--json", "../bad.json"], &[]);
    assert_eq!(invalid_name.code, 64);
    let invalid_payload = json(&invalid_name);
    assert_eq!(invalid_payload["command"], "auth save");
    assert_eq!(invalid_payload["ok"], false);
    assert_eq!(invalid_payload["error"]["code"], "invalid-secret-file-name");
}

#[test]
fn auth_json_contract_save_missing_paths_are_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"tok"}}"#).expect("write auth");

    let missing_secret_dir = dir.path().join("missing-secrets");
    let missing_secret_output = run(
        &["auth", "save", "--json", "alpha.json"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &missing_secret_dir),
        ],
    );
    assert_eq!(missing_secret_output.code, 1);
    let missing_secret_payload = json(&missing_secret_output);
    assert_eq!(
        missing_secret_payload["error"]["code"],
        "secret-dir-not-found"
    );

    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("create secrets dir");

    let no_auth_file_config = run_with(
        &["auth", "save", "--json", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
        &[("CODEX_AUTH_FILE", ""), ("HOME", "")],
    );
    assert_eq!(no_auth_file_config.code, 1);
    let no_auth_file_payload = json(&no_auth_file_config);
    assert_eq!(
        no_auth_file_payload["error"]["code"],
        "auth-file-not-configured"
    );

    let missing_auth_path = dir.path().join("missing-auth.json");
    let missing_auth_output = run(
        &["auth", "save", "--json", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("CODEX_AUTH_FILE", &missing_auth_path),
        ],
    );
    assert_eq!(missing_auth_output.code, 1);
    let missing_auth_payload = json(&missing_auth_output);
    assert_eq!(missing_auth_payload["error"]["code"], "auth-file-not-found");
}

#[test]
fn auth_json_contract_remove_invalid_name_and_missing_paths_are_structured() {
    let invalid_name = run(&["auth", "remove", "--json", "../bad.json"], &[]);
    assert_eq!(invalid_name.code, 64);
    let invalid_payload = json(&invalid_name);
    assert_eq!(invalid_payload["command"], "auth remove");
    assert_eq!(invalid_payload["error"]["code"], "invalid-secret-file-name");

    let dir = tempfile::TempDir::new().expect("tempdir");
    let missing_secret_dir = dir.path().join("missing-secrets");
    let missing_secret = run(
        &["auth", "remove", "--json", "alpha.json"],
        &[("CODEX_SECRET_DIR", &missing_secret_dir)],
    );
    assert_eq!(missing_secret.code, 1);
    let missing_secret_payload = json(&missing_secret);
    assert_eq!(
        missing_secret_payload["error"]["code"],
        "secret-dir-not-found"
    );

    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("create secrets dir");
    let target_missing = run(
        &["auth", "remove", "--json", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
    );
    assert_eq!(target_missing.code, 1);
    let target_missing_payload = json(&target_missing);
    assert_eq!(target_missing_payload["error"]["code"], "target-not-found");
}

#[test]
fn auth_json_contract_current_not_configured_and_secret_dir_read_failed_are_structured() {
    let unconfigured = run_with(
        &["auth", "current", "--json"],
        &[],
        &[("CODEX_AUTH_FILE", ""), ("HOME", "")],
    );
    assert_eq!(unconfigured.code, 1);
    let unconfigured_payload = json(&unconfigured);
    assert_eq!(unconfigured_payload["command"], "auth current");
    assert_eq!(
        unconfigured_payload["error"]["code"],
        "auth-file-not-configured"
    );

    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    fs::write(
        &auth_file,
        auth_json(
            PAYLOAD_ALPHA,
            "acct_001",
            "refresh_a",
            "2025-01-20T12:34:56Z",
        ),
    )
    .expect("write auth");
    let secret_dir_file = dir.path().join("secret-dir-as-file");
    fs::write(&secret_dir_file, "not-a-directory").expect("write secret dir file");

    let read_failed = run(
        &["auth", "current", "--json"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secret_dir_file),
        ],
    );
    assert_eq!(read_failed.code, 1);
    let read_failed_payload = json(&read_failed);
    assert_eq!(read_failed_payload["command"], "auth current");
    assert_eq!(
        read_failed_payload["error"]["code"],
        "secret-dir-read-failed"
    );
}

#[test]
fn auth_json_contract_sync_handles_unconfigured_and_missing_identity() {
    let unconfigured = run_with(
        &["auth", "sync", "--json"],
        &[],
        &[("CODEX_AUTH_FILE", ""), ("HOME", "")],
    );
    assert_eq!(unconfigured.code, 0);
    let unconfigured_payload = json(&unconfigured);
    assert_eq!(unconfigured_payload["command"], "auth sync");
    assert_eq!(unconfigured_payload["ok"], true);
    assert_eq!(unconfigured_payload["result"]["auth_file"], "");
    assert_eq!(unconfigured_payload["result"]["synced"], 0);
    assert_eq!(unconfigured_payload["result"]["skipped"], 0);
    assert_eq!(unconfigured_payload["result"]["failed"], 0);

    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, r#"{"tokens":{"refresh_token":"refresh-only"}}"#).expect("write auth");
    let no_identity = run(
        &["auth", "sync", "--json"],
        &[("CODEX_AUTH_FILE", &auth_file)],
    );
    assert_eq!(no_identity.code, 0);
    let no_identity_payload = json(&no_identity);
    assert_eq!(no_identity_payload["command"], "auth sync");
    assert_eq!(no_identity_payload["ok"], true);
    assert_eq!(no_identity_payload["result"]["synced"], 0);
    assert_eq!(no_identity_payload["result"]["skipped"], 1);
}

#[test]
fn auth_json_contract_refresh_request_failure_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    fs::write(
        &auth_file,
        auth_json(
            PAYLOAD_ALPHA,
            "acct_001",
            "refresh_a",
            "2025-01-20T12:34:56Z",
        ),
    )
    .expect("write auth");

    let output = run_with(
        &["auth", "refresh", "--json"],
        &[("CODEX_AUTH_FILE", &auth_file)],
        &[
            ("HTTPS_PROXY", "http://127.0.0.1:9"),
            ("https_proxy", "http://127.0.0.1:9"),
            ("CODEX_REFRESH_AUTH_CURL_CONNECT_TIMEOUT_SECONDS", "1"),
            ("CODEX_REFRESH_AUTH_CURL_MAX_TIME_SECONDS", "1"),
        ],
    );
    assert_eq!(output.code, 3);
    let payload = json(&output);
    assert_eq!(payload["command"], "auth refresh");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "token-endpoint-request-failed");
}

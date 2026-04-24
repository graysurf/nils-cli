use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::write_exe;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

const HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
const PAYLOAD_ALPHA: &str = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";
const PAYLOAD_BETA: &str = "eyJzdWIiOiJ1c2VyXzQ1NiIsImVtYWlsIjoiYmV0YUBleGFtcGxlLmNvbSIsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X3VzZXJfaWQiOiJ1c2VyXzQ1NiIsImVtYWlsIjoiYmV0YUBleGFtcGxlLmNvbSJ9fQ";

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
    let mut options = CmdOptions::default().with_path_prepend(path_prepend);
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

fn codex_stub_script() -> &'static str {
    r#"#!/bin/bash
set -euo pipefail
exit "${CODEX_STUB_EXIT_CODE:-0}"
"#
}

#[test]
fn auth_json_contract_current_success_includes_stable_fields() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");

    let auth_file = dir.path().join("auth.json");
    let content = auth_json(
        PAYLOAD_ALPHA,
        "acct_001",
        "refresh_a",
        "2025-01-20T12:34:56Z",
    );
    fs::write(&auth_file, &content).expect("write auth");
    fs::write(secrets.join("alpha.json"), &content).expect("write secret");

    let output = run(
        &["auth", "current", "--json"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
        ],
    );
    assert_eq!(output.code, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth current");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["matched"], true);
    assert_eq!(payload["result"]["matched_secret"], "alpha.json");
}

#[test]
fn auth_json_contract_use_ambiguous_returns_structured_error() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, "{}").expect("write auth");

    let content = auth_json(
        PAYLOAD_ALPHA,
        "acct_001",
        "refresh_a",
        "2025-01-20T12:34:56Z",
    );
    fs::write(secrets.join("alpha.json"), &content).expect("write secret");
    fs::write(secrets.join("alpha-dup.json"), &content).expect("write secret");

    let output = run(
        &["auth", "use", "--json", "alpha@example.com"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
        ],
    );
    assert_eq!(output.code, 2);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth use");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "ambiguous-secret");
    assert!(payload["error"]["details"]["candidates"].is_array());
}

#[test]
fn auth_json_contract_auto_refresh_unconfigured_returns_zeroed_result() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("missing_auth.json");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secret dir");

    let output = run(
        &["auth", "auto-refresh", "--json"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secret_dir),
        ],
    );
    assert_eq!(output.code, 0);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth auto-refresh");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["refreshed"], 0);
    assert_eq!(payload["result"]["failed"], 0);
}

#[test]
fn auth_json_contract_refresh_invalid_name_is_structured() {
    let output = run(&["auth", "refresh", "--json", "../bad.json"], &[]);
    assert_eq!(output.code, 64);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth refresh");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "invalid-secret-file-name");
    assert_eq!(
        payload["error"]["message"],
        "codex-refresh: invalid secret file name"
    );
    assert!(payload["error"]["details"]["secret"].is_null());
}

#[test]
fn auth_json_contract_current_missing_auth_file_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("missing.json");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secrets dir");

    let output = run(
        &["auth", "current", "--format", "json"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secret_dir),
        ],
    );
    assert_eq!(output.code, 1);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth current");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "auth-file-not-found");
}

#[test]
fn auth_json_contract_current_defaults_to_home_secret_dir_when_env_unset() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let home = dir.path().join("home");
    let auth_dir = home.join(".agents");
    let secret_dir = home.join(".config").join("codex_secrets");
    fs::create_dir_all(&auth_dir).expect("auth dir");
    fs::create_dir_all(&secret_dir).expect("secret dir");

    let auth_file = auth_dir.join("auth.json");
    let content = auth_json(
        PAYLOAD_ALPHA,
        "acct_001",
        "refresh_a",
        "2025-01-20T12:34:56Z",
    );
    fs::write(&auth_file, &content).expect("write auth");
    fs::write(secret_dir.join("alpha.json"), &content).expect("write secret");

    let home_str = home.to_string_lossy().to_string();
    let output = run_with(
        &["auth", "current", "--json"],
        &[],
        &[
            ("HOME", home_str.as_str()),
            ("CODEX_AUTH_FILE", ""),
            ("CODEX_SECRET_DIR", ""),
        ],
    );
    assert_eq!(output.code, 0);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth current");
    assert_eq!(payload["ok"], true);
    assert_eq!(
        payload["result"]["auth_file"],
        auth_file.display().to_string()
    );
    assert_eq!(payload["result"]["matched"], true);
    assert_eq!(payload["result"]["matched_secret"], "alpha.json");
}

#[test]
fn auth_json_contract_current_missing_default_secret_dir_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let home = dir.path().join("home");
    let auth_dir = home.join(".agents");
    fs::create_dir_all(&auth_dir).expect("auth dir");

    let auth_file = auth_dir.join("auth.json");
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

    let home_str = home.to_string_lossy().to_string();
    let output = run_with(
        &["auth", "current", "--json"],
        &[],
        &[
            ("HOME", home_str.as_str()),
            ("CODEX_AUTH_FILE", ""),
            ("CODEX_SECRET_DIR", ""),
        ],
    );
    assert_eq!(output.code, 1);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth current");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "secret-dir-not-found");
    assert_eq!(
        payload["error"]["details"]["secret_dir"],
        home.join(".config")
            .join("codex_secrets")
            .display()
            .to_string()
    );
}

#[test]
fn auth_json_contract_current_no_match_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");

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
    fs::write(
        secrets.join("beta.json"),
        auth_json(
            PAYLOAD_BETA,
            "acct_002",
            "refresh_b",
            "2025-01-21T12:34:56Z",
        ),
    )
    .expect("write beta");

    let output = run(
        &["auth", "current", "--json"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
        ],
    );
    assert_eq!(output.code, 2);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth current");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "secret-not-matched");
}

#[test]
fn auth_json_contract_sync_missing_auth_reports_skipped() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("missing.json");

    let output = run(
        &["auth", "sync", "--json"],
        &[("CODEX_AUTH_FILE", &auth_file)],
    );
    assert_eq!(output.code, 0);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth sync");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["synced"], 0);
    assert_eq!(payload["result"]["skipped"], 1);
    assert_eq!(
        payload["result"]["auth_file"],
        auth_file.display().to_string()
    );
}

#[test]
fn auth_json_contract_sync_success_reports_updated_files() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    let cache = dir.path().join("cache");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::create_dir_all(&cache).expect("cache dir");

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
    let alpha = secrets.join("alpha.json");
    fs::write(
        &alpha,
        auth_json(
            PAYLOAD_ALPHA,
            "acct_001",
            "refresh_b",
            "2025-01-21T12:34:56Z",
        ),
    )
    .expect("write alpha");
    fs::write(
        secrets.join("beta.json"),
        auth_json(
            PAYLOAD_BETA,
            "acct_002",
            "refresh_c",
            "2025-01-22T12:34:56Z",
        ),
    )
    .expect("write beta");

    let output = run(
        &["auth", "sync", "--json"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
            ("CODEX_SECRET_CACHE_DIR", &cache),
        ],
    );
    assert_eq!(output.code, 0);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth sync");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["synced"], 1);
    assert_eq!(payload["result"]["failed"], 0);
    let updated = payload["result"]["updated_files"]
        .as_array()
        .expect("updated_files");
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0], alpha.display().to_string());
}

#[test]
fn auth_json_contract_use_not_found_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, "{}").expect("write auth");

    let output = run(
        &["auth", "use", "--json", "missing@example.com"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
        ],
    );
    assert_eq!(output.code, 1);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth use");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "secret-not-found");
}

#[test]
fn auth_json_contract_use_invalid_name_is_structured() {
    let output = run(&["auth", "use", "--format", "json", "../bad"], &[]);
    assert_eq!(output.code, 64);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth use");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "invalid-secret-name");
}

#[test]
fn auth_json_contract_login_success_includes_stable_fields() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs");
    write_exe(&stubs, "codex", codex_stub_script());

    let output = run_with_path_prepend(&["auth", "login", "--json", "--api-key"], &[], &[], &stubs);
    assert_eq!(output.code, 0);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth login");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["method"], "api-key");
    assert_eq!(payload["result"]["provider"], "openai-api");
}

#[test]
fn auth_json_contract_login_exec_failure_is_structured() {
    let output = run_with(&["auth", "login", "--json"], &[], &[("PATH", "")]);
    assert_eq!(output.code, 1);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth login");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "login-exec-failed");
}

#[test]
fn auth_json_contract_save_success_includes_stable_fields() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"tok"}}"#).expect("write auth");

    let output = run(
        &["auth", "save", "--json", "alpha.json"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
        ],
    );
    assert_eq!(output.code, 0);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth save");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["saved"], true);
    assert_eq!(payload["result"]["overwritten"], false);
}

#[test]
fn auth_json_contract_save_overwrite_requires_confirmation() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"tok-new"}}"#).expect("write auth");
    fs::write(
        secrets.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok-old"}}"#,
    )
    .expect("write target");

    let output = run(
        &["auth", "save", "--json", "alpha.json"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
        ],
    );
    assert_eq!(output.code, 1);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth save");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "overwrite-confirmation-required");
}

#[test]
fn auth_json_contract_remove_success_includes_stable_fields() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets");
    fs::write(
        secrets.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok-old"}}"#,
    )
    .expect("write target");

    let output = run(
        &["auth", "remove", "--json", "--yes", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
    );
    assert_eq!(output.code, 0);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth remove");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["removed"], true);
    assert_eq!(
        payload["result"]["target_file"],
        secrets.join("alpha.json").display().to_string()
    );
}

#[test]
fn auth_json_contract_remove_requires_confirmation() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets");
    fs::write(
        secrets.join("alpha.json"),
        r#"{"tokens":{"access_token":"tok-old"}}"#,
    )
    .expect("write target");

    let output = run(
        &["auth", "remove", "--json", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
    );
    assert_eq!(output.code, 1);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth remove");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "remove-confirmation-required");
}

#[test]
fn auth_json_contract_auto_refresh_invalid_min_days_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    let secret_dir = dir.path().join("secrets");
    fs::create_dir_all(&secret_dir).expect("secrets dir");
    fs::write(&auth_file, r#"{"last_refresh":"2025-01-20T12:34:56Z"}"#).expect("write auth");

    let output = run_with(
        &["auth", "auto-refresh", "--json"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secret_dir),
        ],
        &[("CODEX_AUTO_REFRESH_MIN_DAYS", "oops")],
    );
    assert_eq!(output.code, 64);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth auto-refresh");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "invalid-min-days");
}

#[test]
fn auth_json_contract_refresh_missing_token_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"tok-only"}}"#).expect("write auth");

    let output = run(
        &["auth", "refresh", "--json"],
        &[("CODEX_AUTH_FILE", &auth_file)],
    );
    assert_eq!(output.code, 2);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth refresh");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "refresh-token-missing");
}

#[test]
fn auth_json_contract_refresh_bad_json_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(secrets.join("alpha.json"), "{not-json").expect("write invalid");

    let output = run(
        &["auth", "refresh", "--json", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
    );
    assert_eq!(output.code, 2);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth refresh");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "refresh-token-read-failed");
}

#[test]
fn auth_json_contract_refresh_missing_target_is_structured() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");

    let output = run(
        &["auth", "refresh", "--format", "json", "missing.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
    );
    assert_eq!(output.code, 1);

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth refresh");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "target-not-found");
}

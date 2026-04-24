use nils_test_support::StubBinDir;
use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn gemini_cli_bin() -> PathBuf {
    bin::resolve("gemini-cli")
}

fn run(
    args: &[&str],
    envs: &[(&str, &Path)],
    vars: &[(&str, &str)],
    path_prepend: Option<&Path>,
) -> CmdOutput {
    let mut options = CmdOptions::default();
    if let Some(path) = path_prepend {
        options = options.with_path_prepend(path);
    }
    for (key, path) in envs {
        let value = path.to_string_lossy();
        options = options.with_env(key, value.as_ref());
    }
    for (key, value) in vars {
        options = options.with_env(key, value);
    }
    let bin = gemini_cli_bin();
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

fn write_curl_stub(stubs: &StubBinDir, script_body: &str) {
    stubs.write_exe("curl", script_body);
}

#[test]
fn auth_refresh_missing_token() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"tok"}}"#).expect("write auth");

    let output = run(
        &["auth", "refresh"],
        &[("GEMINI_AUTH_FILE", &auth_file)],
        &[],
        None,
    );
    assert_exit(&output, 2);
    assert!(stderr(&output).contains("failed to read refresh token"));
}

#[test]
fn auth_refresh_invalid_name() {
    let output = run(&["auth", "refresh", "../bad.json"], &[], &[], None);
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("invalid secret file name"));
}

#[test]
fn auth_refresh_missing_secret_file() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");

    let output = run(
        &["auth", "refresh", "missing.json"],
        &[("GEMINI_SECRET_DIR", &secrets)],
        &[],
        None,
    );
    assert_exit(&output, 1);
    assert!(stderr(&output).contains("not found"));
}

#[test]
fn auth_refresh_json_missing_token() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"tok"}}"#).expect("write auth");

    let output = run(
        &["auth", "refresh", "--json"],
        &[("GEMINI_AUTH_FILE", &auth_file)],
        &[],
        None,
    );
    assert_exit(&output, 2);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["command"], "auth refresh");
    assert_eq!(payload["error"]["code"], "refresh-token-missing");
}

#[test]
fn auth_refresh_json_invalid_name() {
    let output = run(
        &["auth", "refresh", "--json", "../bad.json"],
        &[],
        &[],
        None,
    );
    assert_exit(&output, 64);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "invalid-secret-file-name");
}

#[test]
fn auth_refresh_json_missing_secret_file() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");

    let output = run(
        &["auth", "refresh", "--json", "missing.json"],
        &[("GEMINI_SECRET_DIR", &secrets)],
        &[],
        None,
    );
    assert_exit(&output, 1);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "target-not-found");
}

#[test]
fn auth_refresh_success_updates_tokens_and_timestamp() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets");
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).expect("cache");

    write_curl_stub(
        &stubs,
        "#!/bin/sh\ncat <<'EOF'\n{\"access_token\":\"new_access\",\"refresh_token\":\"new_refresh\",\"id_token\":\"new_id\"}\n__HTTP_STATUS__:200\nEOF\n",
    );

    let target = secrets.join("alpha.json");
    fs::write(
        &target,
        r#"{"refresh_token":"old_refresh","account_id":"acct_001"}"#,
    )
    .expect("write target");

    let output = run(
        &["auth", "refresh", "alpha.json"],
        &[
            ("GEMINI_SECRET_DIR", &secrets),
            ("GEMINI_SECRET_CACHE_DIR", &cache_dir),
        ],
        &[],
        Some(stubs.path()),
    );
    assert_exit(&output, 0);
    assert!(stdout(&output).contains("gemini: refreshed"));

    let refreshed: Value =
        serde_json::from_str(&fs::read_to_string(&target).expect("read target")).expect("json");
    assert_eq!(refreshed["tokens"]["access_token"], "new_access");
    assert_eq!(refreshed["tokens"]["refresh_token"], "new_refresh");
    assert!(refreshed["last_refresh"].is_string());

    let timestamp = cache_dir.join("alpha.json.timestamp");
    assert!(timestamp.is_file());
}

#[test]
fn auth_refresh_json_http_error_contains_summary() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();

    write_curl_stub(
        &stubs,
        "#!/bin/sh\ncat <<'EOF'\n{\"error\":{\"code\":\"invalid_grant\",\"message\":\"expired\"},\"error_description\":\"reauth\"}\n__HTTP_STATUS__:401\nEOF\n",
    );

    let auth_file = dir.path().join("auth.json");
    fs::write(
        &auth_file,
        r#"{"tokens":{"refresh_token":"old_refresh","access_token":"old_access","account_id":"acct_001"}}"#,
    )
    .expect("write auth");

    let output = run(
        &["auth", "refresh", "--json"],
        &[("GEMINI_AUTH_FILE", &auth_file)],
        &[],
        Some(stubs.path()),
    );
    assert_exit(&output, 3);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["error"]["code"], "token-endpoint-failed");
    assert_eq!(payload["error"]["details"]["http_status"], 401);
    let summary = payload["error"]["details"]["summary"]
        .as_str()
        .expect("summary");
    assert!(summary.contains("invalid_grant"));
    assert!(summary.contains("expired"));
    assert!(summary.contains("reauth"));
}

#[test]
fn auth_refresh_invalid_json_payload_returns_4() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();

    write_curl_stub(
        &stubs,
        "#!/bin/sh\ncat <<'EOF'\nnot-json\n__HTTP_STATUS__:200\nEOF\n",
    );

    let auth_file = dir.path().join("auth.json");
    fs::write(
        &auth_file,
        r#"{"tokens":{"refresh_token":"old_refresh","access_token":"old_access","account_id":"acct_001"}}"#,
    )
    .expect("write auth");

    let output = run(
        &["auth", "refresh"],
        &[("GEMINI_AUTH_FILE", &auth_file)],
        &[],
        Some(stubs.path()),
    );
    assert_exit(&output, 4);
    assert!(stderr(&output).contains("invalid JSON"));
}

#[test]
fn auth_refresh_merge_failed_when_endpoint_payload_not_object() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();

    write_curl_stub(
        &stubs,
        "#!/bin/sh\ncat <<'EOF'\n123\n__HTTP_STATUS__:200\nEOF\n",
    );

    let auth_file = dir.path().join("auth.json");
    fs::write(
        &auth_file,
        r#"{"tokens":{"refresh_token":"old_refresh","access_token":"old_access","account_id":"acct_001"}}"#,
    )
    .expect("write auth");

    let output = run(
        &["auth", "refresh", "--json"],
        &[("GEMINI_AUTH_FILE", &auth_file)],
        &[],
        Some(stubs.path()),
    );
    assert_exit(&output, 5);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["error"]["code"], "merge-failed");
}

#[test]
fn auth_refresh_missing_curl_binary_returns_3() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    fs::write(
        &auth_file,
        r#"{"tokens":{"refresh_token":"old_refresh","access_token":"old_access","account_id":"acct_001"}}"#,
    )
    .expect("write auth");

    let output = run(
        &["auth", "refresh"],
        &[("GEMINI_AUTH_FILE", &auth_file)],
        &[("PATH", "")],
        None,
    );
    assert_exit(&output, 3);
    assert!(stderr(&output).contains("token endpoint request failed"));
}

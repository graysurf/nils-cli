use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::write_exe;
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

fn auth_json_with_extra_secrets(
    payload: &str,
    account_id: &str,
    refresh_token: &str,
    last_refresh: &str,
) -> String {
    let mut value: Value =
        serde_json::from_str(&auth_json(payload, account_id, refresh_token, last_refresh))
            .expect("auth json");
    value["OPENAI_API_KEY"] = Value::String("sk-remote-secret".to_string());
    value["other"] = serde_json::json!({ "refresh_token": "extra-refresh-secret" });
    value["tokens"]["api_key"] = Value::String("sk-token-secret".to_string());
    serde_json::to_string(&value).expect("extra secret auth json")
}

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

fn stderr(output: &CmdOutput) -> String {
    output.stderr_text()
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(output.code, code, "stderr: {}", stderr(output));
}

fn assert_auth_remote_error(output: &CmdOutput, code: &str) -> Value {
    let payload: Value = serde_json::from_str(&stdout(output)).expect("json error envelope");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth remote pull");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], code);
    payload
}

#[test]
fn auth_remote_export_access_only_strips_refresh_token() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");

    let content = auth_json_with_extra_secrets(
        PAYLOAD_ALPHA,
        "acct_001",
        "refresh_secret_value",
        "2025-01-20T12:34:56Z",
    );
    fs::write(secrets.join("team.json"), &content).expect("write secret");

    let output = run(
        &[
            "auth",
            "remote",
            "export",
            "--name",
            "team",
            "--access-only",
        ],
        &[("CODEX_SECRET_DIR", &secrets)],
        &[],
    );

    assert_exit(&output, 0);
    let raw = stdout(&output);
    assert!(!raw.contains("refresh_secret_value"));
    assert!(!raw.contains("sk-remote-secret"));
    assert!(!raw.contains("sk-token-secret"));

    let exported: Value = serde_json::from_str(&raw).expect("raw exported auth json");
    assert!(exported["tokens"]["access_token"].is_string());
    assert!(exported["tokens"]["id_token"].is_string());
    assert_eq!(exported["tokens"]["account_id"], "acct_001");
    assert!(exported["tokens"].get("refresh_token").is_none());
    assert!(exported["tokens"].get("api_key").is_none());
    assert!(exported.get("refresh_token").is_none());
    assert!(exported.get("OPENAI_API_KEY").is_none());
    assert!(exported.get("other").is_none());
}

#[test]
fn auth_remote_pull_access_only_writes_active_without_refresh_token() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");

    let auth_file = dir.path().join("auth.json");
    let cache = dir.path().join("cache");
    fs::create_dir_all(&cache).expect("cache dir");
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

    let remote_payload = auth_json_with_extra_secrets(
        PAYLOAD_ALPHA,
        "acct_001",
        "remote_refresh_secret",
        "2025-01-20T12:34:56Z",
    );
    let output = run_with_path_prepend(
        &[
            "auth",
            "remote",
            "pull",
            "--ssh",
            "auth-host",
            "--name",
            "team",
            "--access-only",
            "--write-active",
            "--json",
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_CACHE_DIR", &cache),
        ],
        &[
            ("REMOTE_AUTH_PAYLOAD", &remote_payload),
            ("SSH_ARGS_FILE", args_file.to_str().expect("args path")),
        ],
        &stubs,
    );

    assert_exit(&output, 0);
    let raw = stdout(&output);
    assert!(!raw.contains("remote_refresh_secret"));
    assert!(!raw.contains("sk-remote-secret"));
    assert!(!raw.contains("sk-token-secret"));
    assert!(!raw.contains(&token(PAYLOAD_ALPHA)));

    let payload: Value = serde_json::from_str(&raw).expect("json envelope");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth remote pull");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["result"]["ssh"], "auth-host");
    assert_eq!(payload["result"]["name"], "team");
    assert_eq!(payload["result"]["access_only"], true);
    assert_eq!(payload["result"]["write_active"], true);
    assert_eq!(
        payload["result"]["auth_file"],
        auth_file.to_string_lossy().as_ref()
    );
    assert_eq!(payload["result"]["has_oauth_access_token"], true);
    assert_eq!(payload["result"]["has_oauth_refresh_token"], false);

    let applied: Value =
        serde_json::from_str(&fs::read_to_string(&auth_file).expect("read auth file"))
            .expect("applied auth json");
    assert!(applied["tokens"]["access_token"].is_string());
    assert!(applied["tokens"]["id_token"].is_string());
    assert_eq!(applied["tokens"]["account_id"], "acct_001");
    assert!(applied["tokens"].get("refresh_token").is_none());
    assert!(applied["tokens"].get("api_key").is_none());
    assert!(applied.get("refresh_token").is_none());
    assert!(applied.get("OPENAI_API_KEY").is_none());
    assert!(applied.get("other").is_none());

    let captured_args = fs::read_to_string(&args_file).expect("read ssh args");
    assert!(captured_args.contains("auth-host"));
    assert!(captured_args.contains("codex-cli auth remote export"));
    assert!(captured_args.contains("--name team"));
    assert!(captured_args.contains("--access-only"));
    assert!(!captured_args.contains("--refresh"));

    let timestamp = cache.join("auth.json.timestamp");
    assert_eq!(
        fs::read_to_string(&timestamp).expect("read timestamp"),
        "2025-01-20T12:34:56Z"
    );
}

#[test]
fn auth_remote_pull_persists_fallback_last_refresh_when_remote_omits_it() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");

    let auth_file = dir.path().join("auth.json");
    let cache = dir.path().join("cache");
    fs::create_dir_all(&cache).expect("cache dir");

    write_exe(
        &stubs,
        "ssh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$REMOTE_AUTH_PAYLOAD"
"#,
    );

    let remote_payload = format!(
        r#"{{"tokens":{{"access_token":"{}","id_token":"{}","account_id":"acct_001"}}}}"#,
        token(PAYLOAD_ALPHA),
        token(PAYLOAD_ALPHA)
    );
    let output = run_with_path_prepend(
        &[
            "auth",
            "remote",
            "pull",
            "--ssh",
            "auth-host",
            "--name",
            "team",
            "--access-only",
            "--write-active",
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_CACHE_DIR", &cache),
        ],
        &[("REMOTE_AUTH_PAYLOAD", &remote_payload)],
        &stubs,
    );

    assert_exit(&output, 0);

    let applied: Value =
        serde_json::from_str(&fs::read_to_string(&auth_file).expect("read auth file"))
            .expect("applied auth json");
    let last_refresh = applied["last_refresh"]
        .as_str()
        .expect("fallback last_refresh");
    assert!(last_refresh.ends_with('Z'));
    assert_eq!(
        fs::read_to_string(cache.join("auth.json.timestamp")).expect("read timestamp"),
        last_refresh
    );
}

#[test]
fn auth_remote_pull_rejects_payload_without_oauth_access_token() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");

    let auth_file = dir.path().join("auth.json");

    write_exe(
        &stubs,
        "ssh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$REMOTE_AUTH_PAYLOAD"
"#,
    );

    let remote_payload = r#"{"tokens":{"id_token":"remote-id-token","account_id":"acct_001"},"last_refresh":"2025-01-20T12:34:56Z"}"#;
    let output = run_with_path_prepend(
        &[
            "auth",
            "remote",
            "pull",
            "--ssh",
            "auth-host",
            "--name",
            "team",
            "--access-only",
            "--write-active",
            "--json",
        ],
        &[("CODEX_AUTH_FILE", &auth_file)],
        &[("REMOTE_AUTH_PAYLOAD", remote_payload)],
        &stubs,
    );

    assert_exit(&output, 1);
    assert!(
        !auth_file.exists(),
        "invalid remote payload should not write active auth"
    );

    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json envelope");
    assert_eq!(payload["schema_version"], "codex-cli.auth.v1");
    assert_eq!(payload["command"], "auth remote pull");
    assert_eq!(payload["ok"], false);
    assert_eq!(
        payload["error"]["code"],
        "remote-export-missing-access-token"
    );
}

#[test]
fn auth_remote_pull_refresh_flag_requests_remote_refresh_before_export() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");

    let auth_file = dir.path().join("auth.json");
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

    let remote_payload = auth_json(
        PAYLOAD_ALPHA,
        "acct_001",
        "remote_refresh_secret",
        "2025-01-20T12:34:56Z",
    );
    let output = run_with_path_prepend(
        &[
            "auth",
            "remote",
            "pull",
            "--ssh",
            "auth-host",
            "--name",
            "team",
            "--access-only",
            "--write-active",
            "--refresh",
        ],
        &[("CODEX_AUTH_FILE", &auth_file)],
        &[
            ("REMOTE_AUTH_PAYLOAD", &remote_payload),
            ("SSH_ARGS_FILE", args_file.to_str().expect("args path")),
        ],
        &stubs,
    );

    assert_exit(&output, 0);
    let captured_args = fs::read_to_string(&args_file).expect("read ssh args");
    assert!(captured_args.contains("--refresh"));
}

#[test]
fn auth_remote_pull_rejects_ssh_option_injection() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");

    let output = run(
        &[
            "auth",
            "remote",
            "pull",
            "--ssh=-oProxyCommand=bad",
            "--name",
            "team",
            "--access-only",
            "--write-active",
        ],
        &[("CODEX_AUTH_FILE", &auth_file)],
        &[],
    );

    assert_exit(&output, 64);
    assert!(stderr(&output).contains("invalid ssh host"));
}

#[test]
fn auth_remote_pull_rejects_secret_name_shell_metachar() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");

    let output = run(
        &[
            "auth",
            "remote",
            "pull",
            "--ssh",
            "auth-host",
            "--name",
            "team;bad",
            "--access-only",
            "--write-active",
        ],
        &[("CODEX_AUTH_FILE", &auth_file)],
        &[],
    );

    assert_exit(&output, 64);
    assert!(stderr(&output).contains("invalid secret name"));
}

#[test]
fn auth_remote_pull_json_reports_missing_ssh_flag() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");

    let output = run(
        &[
            "auth",
            "remote",
            "pull",
            "--format",
            "json",
            "--name",
            "team",
            "--access-only",
            "--write-active",
        ],
        &[("CODEX_AUTH_FILE", &auth_file)],
        &[],
    );

    assert_exit(&output, 64);
    assert_auth_remote_error(&output, "missing-ssh");
    assert!(stderr(&output).is_empty());
}

#[test]
fn auth_remote_pull_json_reports_missing_name_flag() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");

    let output = run(
        &[
            "auth",
            "remote",
            "pull",
            "--format",
            "json",
            "--ssh",
            "auth-host",
            "--access-only",
            "--write-active",
        ],
        &[("CODEX_AUTH_FILE", &auth_file)],
        &[],
    );

    assert_exit(&output, 64);
    assert_auth_remote_error(&output, "missing-name");
    assert!(stderr(&output).is_empty());
}

#[test]
fn auth_remote_pull_json_reports_active_auth_write_failure() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");

    let auth_parent = dir.path().join("not-a-directory");
    fs::write(&auth_parent, "occupied").expect("write auth parent file");
    let auth_file = auth_parent.join("auth.json");

    write_exe(
        &stubs,
        "ssh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$REMOTE_AUTH_PAYLOAD"
"#,
    );

    let remote_payload = auth_json(
        PAYLOAD_ALPHA,
        "acct_001",
        "remote_refresh_secret",
        "2025-01-20T12:34:56Z",
    );
    let output = run_with_path_prepend(
        &[
            "auth",
            "remote",
            "pull",
            "--ssh",
            "auth-host",
            "--name",
            "team",
            "--access-only",
            "--write-active",
            "--format",
            "json",
        ],
        &[("CODEX_AUTH_FILE", &auth_file)],
        &[("REMOTE_AUTH_PAYLOAD", &remote_payload)],
        &stubs,
    );

    assert_exit(&output, 1);
    let payload = assert_auth_remote_error(&output, "active-auth-write-failed");
    assert_eq!(
        payload["error"]["details"]["auth_file"],
        auth_file.to_string_lossy().as_ref()
    );
    assert_eq!(payload["error"]["details"]["phase"], "auth-file");
    assert_eq!(payload["error"]["details"]["auth_written"], false);
    assert!(!stdout(&output).contains("remote_refresh_secret"));
    assert!(stderr(&output).is_empty());
}

#[test]
fn auth_remote_pull_json_reports_timestamp_write_failure_after_active_auth_write() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");

    let auth_file = dir.path().join("auth.json");
    let cache_file = dir.path().join("cache-file");
    fs::write(&cache_file, "occupied").expect("write cache file");

    write_exe(
        &stubs,
        "ssh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$REMOTE_AUTH_PAYLOAD"
"#,
    );

    let remote_payload = auth_json(
        PAYLOAD_ALPHA,
        "acct_001",
        "remote_refresh_secret",
        "2025-01-20T12:34:56Z",
    );
    let output = run_with_path_prepend(
        &[
            "auth",
            "remote",
            "pull",
            "--ssh",
            "auth-host",
            "--name",
            "team",
            "--access-only",
            "--write-active",
            "--format",
            "json",
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_CACHE_DIR", &cache_file),
        ],
        &[("REMOTE_AUTH_PAYLOAD", &remote_payload)],
        &stubs,
    );

    assert_exit(&output, 1);
    let payload = assert_auth_remote_error(&output, "active-auth-timestamp-write-failed");
    assert_eq!(
        payload["error"]["details"]["auth_file"],
        auth_file.to_string_lossy().as_ref()
    );
    assert_eq!(payload["error"]["details"]["phase"], "timestamp");
    assert_eq!(payload["error"]["details"]["auth_written"], true);
    assert!(!stdout(&output).contains("remote_refresh_secret"));
    assert!(stderr(&output).is_empty());

    let applied: Value =
        serde_json::from_str(&fs::read_to_string(&auth_file).expect("read auth file"))
            .expect("applied auth json");
    assert!(applied["tokens"].get("refresh_token").is_none());
}

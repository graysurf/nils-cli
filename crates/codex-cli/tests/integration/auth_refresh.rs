use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::write_exe;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

const ACCESS_TOKEN: &str = "remote-access-token";
const ID_TOKEN: &str = "remote-id-token";
const HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
const PAYLOAD_ALPHA: &str = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";
const ACCESS_ONLY_REFRESH_TOKEN_PLACEHOLDER: &str = "codex-remote-access-only-placeholder";

fn token(payload: &str) -> String {
    format!("{HEADER}.{payload}.sig")
}

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn run(args: &[&str], envs: &[(&str, &Path)]) -> CmdOutput {
    let mut options = CmdOptions::default();
    for (key, path) in envs {
        let value = path.to_string_lossy();
        options = options.with_env(key, value.as_ref());
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

fn stderr(output: &CmdOutput) -> String {
    output.stderr_text()
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(output.code, code);
}

#[test]
fn auth_refresh_missing_token() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"tok"}}"#).expect("write auth");

    let output = run(&["auth", "refresh"], &[("CODEX_AUTH_FILE", &auth_file)]);
    assert_exit(&output, 2);
    assert!(stderr(&output).contains("failed to read refresh token"));
}

#[test]
fn auth_refresh_invalid_name() {
    let output = run(&["auth", "refresh", "../bad.json"], &[]);
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
        &[("CODEX_SECRET_DIR", &secrets)],
    );
    assert_exit(&output, 1);
    assert!(stderr(&output).contains("not found"));
}

#[test]
fn auth_refresh_delegates_to_configured_remote_without_local_refresh_token() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");

    let auth_file = dir.path().join("auth.json");
    let cache = dir.path().join("cache");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"stale-local"}}"#).expect("write auth");

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

    let remote_payload = format!(
        r#"{{"tokens":{{"access_token":"{}","id_token":"{}","account_id":"acct_001"}},"last_refresh":"2025-01-20T12:34:56Z"}}"#,
        ACCESS_TOKEN, ID_TOKEN
    );
    let output = run_with_path_prepend(
        &["auth", "refresh"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_CACHE_DIR", &cache),
        ],
        &[
            ("CODEX_AUTH_REMOTE_SSH", "auth-host"),
            ("CODEX_AUTH_REMOTE_NAME", "team"),
            ("REMOTE_AUTH_PAYLOAD", &remote_payload),
            ("SSH_ARGS_FILE", args_file.to_str().expect("args path")),
        ],
        &stubs,
    );

    assert_exit(&output, 0);

    let applied: Value =
        serde_json::from_str(&fs::read_to_string(&auth_file).expect("read auth file"))
            .expect("applied auth json");
    assert_eq!(applied["tokens"]["access_token"], ACCESS_TOKEN);
    assert_eq!(applied["tokens"]["id_token"], ID_TOKEN);
    assert_eq!(applied["tokens"]["account_id"], "acct_001");
    assert_eq!(
        applied["tokens"]["refresh_token"],
        ACCESS_ONLY_REFRESH_TOKEN_PLACEHOLDER
    );

    let captured_args = fs::read_to_string(&args_file).expect("read ssh args");
    assert!(captured_args.contains("auth-host"));
    assert!(captured_args.contains("codex-cli auth remote export"));
    assert!(captured_args.contains("--name team"));
    assert!(captured_args.contains("--access-only"));
    assert!(!captured_args.contains("--refresh"));
}

#[test]
fn auth_refresh_remote_does_not_overwrite_matching_full_secret() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");

    let auth_file = dir.path().join("auth.json");
    let cache = dir.path().join("cache");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"stale-local"}}"#).expect("write auth");

    let full_secret = secrets.join("team.json");
    let full_secret_before = format!(
        r#"{{"tokens":{{"access_token":"{}","id_token":"{}","refresh_token":"keep-refresh","account_id":"acct_001"}},"last_refresh":"2025-01-19T12:34:56Z"}}"#,
        token(PAYLOAD_ALPHA),
        token(PAYLOAD_ALPHA)
    );
    fs::write(&full_secret, &full_secret_before).expect("write full secret");

    write_exe(
        &stubs,
        "ssh",
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$REMOTE_AUTH_PAYLOAD"
"#,
    );

    let remote_payload = format!(
        r#"{{"tokens":{{"access_token":"{}","id_token":"{}","account_id":"acct_001"}},"last_refresh":"2025-01-20T12:34:56Z"}}"#,
        token(PAYLOAD_ALPHA),
        token(PAYLOAD_ALPHA)
    );
    let output = run_with_path_prepend(
        &["auth", "refresh"],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_CACHE_DIR", &cache),
            ("CODEX_SECRET_DIR", &secrets),
        ],
        &[
            ("CODEX_AUTH_REMOTE_SSH", "auth-host"),
            ("CODEX_AUTH_REMOTE_NAME", "team"),
            ("REMOTE_AUTH_PAYLOAD", &remote_payload),
        ],
        &stubs,
    );

    assert_exit(&output, 0);

    let full_secret_after = fs::read_to_string(&full_secret).expect("read full secret");
    assert_eq!(full_secret_after, full_secret_before);
    let parsed: Value = serde_json::from_str(&full_secret_after).expect("full secret json");
    assert_eq!(parsed["tokens"]["refresh_token"], "keep-refresh");
}

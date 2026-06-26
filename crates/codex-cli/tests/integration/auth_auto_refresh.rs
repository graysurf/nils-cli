use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::write_exe;
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

const ACCESS_ONLY_REFRESH_TOKEN_PLACEHOLDER: &str = "codex-remote-access-only-placeholder";

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn isolated_options() -> CmdOptions {
    CmdOptions::default().with_env_remove_many(&[
        "CODEX_AUTO_REFRESH_ENABLED",
        "CODEX_AUTH_REMOTE_SSH",
        "CODEX_AUTH_REMOTE_NAME",
        "CODEX_AUTH_REMOTE_REFRESH",
    ])
}

fn run(args: &[&str], envs: &[(&str, &str)], path_envs: &[(&str, &Path)]) -> CmdOutput {
    let mut options = isolated_options();
    for (key, value) in envs {
        options = options.with_env(key, value);
    }
    for (key, path) in path_envs {
        let value = path.to_string_lossy();
        options = options.with_env(key, value.as_ref());
    }
    let bin = codex_cli_bin();
    cmd::run_with(&bin, args, &options)
}

fn run_with_path_prepend(
    args: &[&str],
    envs: &[(&str, &str)],
    path_envs: &[(&str, &Path)],
    path_prepend: &Path,
) -> CmdOutput {
    let mut options = isolated_options().with_path_prepend(path_prepend);
    for (key, value) in envs {
        options = options.with_env(key, value);
    }
    for (key, path) in path_envs {
        let value = path.to_string_lossy();
        options = options.with_env(key, value.as_ref());
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
fn auth_auto_refresh_disabled_by_default_does_not_refresh_due_targets() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    let cache = dir.path().join("cache");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(
        &auth_file,
        r#"{"tokens":{"access_token":"tok"},"last_refresh":"2025-01-20T12:34:56Z"}"#,
    )
    .expect("write auth");

    let output = run(
        &["auth", "auto-refresh"],
        &[("CODEX_AUTO_REFRESH_MIN_DAYS", "0")],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_CACHE_DIR", &cache),
            ("CODEX_SECRET_DIR", &secrets),
        ],
    );

    assert_exit(&output, 0);
    assert!(stdout(&output).contains("disabled"));
    assert!(!stderr(&output).contains("failed to read refresh token"));
    assert!(!cache.join("auth.json.timestamp").exists());
}

#[test]
fn auth_auto_refresh_invalid_min_days() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, r#"{"last_refresh":"2025-01-20T12:34:56Z"}"#).expect("write auth");

    let output = run(
        &["auth", "auto-refresh"],
        &[
            ("CODEX_AUTO_REFRESH_ENABLED", "true"),
            ("CODEX_AUTO_REFRESH_MIN_DAYS", "oops"),
        ],
        &[("CODEX_AUTH_FILE", &auth_file)],
    );

    assert_exit(&output, 64);
    assert!(stderr(&output).contains("invalid CODEX_AUTO_REFRESH_MIN_DAYS"));
}

#[test]
fn auth_auto_refresh_backfills_timestamp() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    let cache = dir.path().join("cache");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::create_dir_all(&secrets).expect("secrets dir");
    let last_refresh = "2025-01-20T12:34:56Z";
    fs::write(
        &auth_file,
        format!(r#"{{"last_refresh":"{}"}}"#, last_refresh),
    )
    .expect("write auth");

    let output = run(
        &["auth", "auto-refresh"],
        &[
            ("CODEX_AUTO_REFRESH_ENABLED", "true"),
            ("CODEX_AUTO_REFRESH_MIN_DAYS", "9999"),
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_CACHE_DIR", &cache),
            ("CODEX_SECRET_DIR", &secrets),
        ],
    );

    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(out.contains("refreshed=0 skipped=1 failed=0 (min_age_days=9999)"));

    let timestamp = cache.join("auth.json.timestamp");
    assert_eq!(fs::read_to_string(&timestamp).unwrap(), last_refresh);
}

#[test]
fn auth_auto_refresh_unconfigured_exits_zero() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("missing_auth.json");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets dir");

    let output = run(
        &["auth", "auto-refresh"],
        &[("CODEX_AUTO_REFRESH_ENABLED", "true")],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_DIR", &secrets),
        ],
    );

    assert_exit(&output, 0);
    assert!(stdout(&output).trim().is_empty());
}

#[test]
fn auth_auto_refresh_warns_on_future_timestamp_and_skips() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    let cache = dir.path().join("cache");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(&auth_file, r#"{"last_refresh":"2025-01-20T12:34:56Z"}"#).expect("write auth");

    let timestamp = cache.join("auth.json.timestamp");
    fs::write(&timestamp, "2999-01-01T00:00:00Z").expect("write timestamp");

    let output = run(
        &["auth", "auto-refresh"],
        &[
            ("CODEX_AUTO_REFRESH_ENABLED", "true"),
            ("CODEX_AUTO_REFRESH_MIN_DAYS", "1"),
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_CACHE_DIR", &cache),
            ("CODEX_SECRET_DIR", &secrets),
        ],
    );

    assert_exit(&output, 0);
    assert!(stderr(&output).contains("warning: future timestamp"));
    assert!(stdout(&output).contains("skipped=1 failed=0"));
}

#[test]
fn auth_auto_refresh_counts_non_file_secret_entry_as_failed() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    let cache = dir.path().join("cache");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(&auth_file, r#"{"last_refresh":"2025-01-20T12:34:56Z"}"#).expect("write auth");

    fs::create_dir_all(secrets.join("not_a_file.json")).expect("create not_a_file.json dir");

    let output = run(
        &["auth", "auto-refresh"],
        &[
            ("CODEX_AUTO_REFRESH_ENABLED", "true"),
            ("CODEX_AUTO_REFRESH_MIN_DAYS", "9999"),
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_CACHE_DIR", &cache),
            ("CODEX_SECRET_DIR", &secrets),
        ],
    );

    assert_exit(&output, 1);
    assert!(stderr(&output).contains("missing file"));
    assert!(stdout(&output).contains("failed=1"));
}

#[test]
fn auth_auto_refresh_normalizes_fractional_last_refresh() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    let cache = dir.path().join("cache");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::create_dir_all(&secrets).expect("secrets dir");

    fs::write(&auth_file, r#"{"last_refresh":"2025-01-20T12:34:56.789Z"}"#).expect("write auth");

    let output = run(
        &["auth", "auto-refresh"],
        &[
            ("CODEX_AUTO_REFRESH_ENABLED", "true"),
            ("CODEX_AUTO_REFRESH_MIN_DAYS", "9999"),
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_CACHE_DIR", &cache),
            ("CODEX_SECRET_DIR", &secrets),
        ],
    );

    assert_exit(&output, 0);
    let timestamp = cache.join("auth.json.timestamp");
    assert_eq!(
        fs::read_to_string(&timestamp).expect("read timestamp"),
        "2025-01-20T12:34:56Z"
    );
}

#[test]
fn auth_auto_refresh_remote_mode_refreshes_active_auth_only() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");

    let auth_file = dir.path().join("auth.json");
    let cache = dir.path().join("cache");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(
        &auth_file,
        r#"{"tokens":{"access_token":"stale-local"},"last_refresh":"2025-01-20T12:34:56Z"}"#,
    )
    .expect("write auth");
    fs::write(
        secrets.join("access-only.json"),
        r#"{"tokens":{"access_token":"stale-secret"},"last_refresh":"2025-01-20T12:34:56Z"}"#,
    )
    .expect("write access-only secret");

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

    let remote_payload = r#"{"tokens":{"access_token":"remote-access-token","id_token":"remote-id-token","account_id":"acct_001"},"last_refresh":"2025-01-20T12:34:56Z"}"#;
    let output = run_with_path_prepend(
        &["auth", "auto-refresh"],
        &[
            ("CODEX_AUTO_REFRESH_ENABLED", "true"),
            ("CODEX_AUTO_REFRESH_MIN_DAYS", "0"),
            ("CODEX_AUTH_REMOTE_SSH", "auth-host"),
            ("CODEX_AUTH_REMOTE_NAME", "team"),
            ("REMOTE_AUTH_PAYLOAD", remote_payload),
            ("SSH_ARGS_FILE", args_file.to_str().expect("args path")),
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_CACHE_DIR", &cache),
            ("CODEX_SECRET_DIR", &secrets),
        ],
        &stubs,
    );

    assert_exit(&output, 0);
    assert!(stdout(&output).contains("refreshed=1 skipped=0 failed=0"));
    assert!(!stderr(&output).contains("failed to read refresh token"));

    let applied: Value =
        serde_json::from_str(&fs::read_to_string(&auth_file).expect("read auth file"))
            .expect("applied auth json");
    assert_eq!(applied["tokens"]["access_token"], "remote-access-token");
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
fn auth_auto_refresh_remote_mode_recreates_missing_active_auth_despite_recent_timestamp() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = dir.path().join("stubs");
    fs::create_dir_all(&stubs).expect("stubs dir");

    let auth_file = dir.path().join("auth.json");
    let cache = dir.path().join("cache");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&cache).expect("cache dir");
    fs::create_dir_all(&secrets).expect("secrets dir");
    fs::write(cache.join("auth.json.timestamp"), "2999-01-01T00:00:00Z").expect("write timestamp");

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

    let remote_payload = r#"{"tokens":{"access_token":"remote-access-token","id_token":"remote-id-token","account_id":"acct_001"},"last_refresh":"2025-01-20T12:34:56Z"}"#;
    let output = run_with_path_prepend(
        &["auth", "auto-refresh"],
        &[
            ("CODEX_AUTO_REFRESH_ENABLED", "true"),
            ("CODEX_AUTO_REFRESH_MIN_DAYS", "9999"),
            ("CODEX_AUTH_REMOTE_SSH", "auth-host"),
            ("CODEX_AUTH_REMOTE_NAME", "team"),
            ("REMOTE_AUTH_PAYLOAD", remote_payload),
            ("SSH_ARGS_FILE", args_file.to_str().expect("args path")),
        ],
        &[
            ("CODEX_AUTH_FILE", &auth_file),
            ("CODEX_SECRET_CACHE_DIR", &cache),
            ("CODEX_SECRET_DIR", &secrets),
        ],
        &stubs,
    );

    assert_exit(&output, 0);
    assert!(stdout(&output).contains("refreshed=1 skipped=0 failed=0"));
    assert!(
        auth_file.is_file(),
        "remote refresh should recreate active auth"
    );

    let applied: Value =
        serde_json::from_str(&fs::read_to_string(&auth_file).expect("read auth file"))
            .expect("applied auth json");
    assert_eq!(applied["tokens"]["access_token"], "remote-access-token");

    let captured_args = fs::read_to_string(&args_file).expect("read ssh args");
    assert!(captured_args.contains("codex-cli auth remote export"));
    assert!(captured_args.contains("--access-only"));
}

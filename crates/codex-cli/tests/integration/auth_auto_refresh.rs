use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn run(args: &[&str], envs: &[(&str, &str)], path_envs: &[(&str, &Path)]) -> CmdOutput {
    let mut options = CmdOptions::default();
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
fn auth_auto_refresh_invalid_min_days() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, r#"{"last_refresh":"2025-01-20T12:34:56Z"}"#).expect("write auth");

    let output = run(
        &["auth", "auto-refresh"],
        &[("CODEX_AUTO_REFRESH_MIN_DAYS", "oops")],
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
        &[("CODEX_AUTO_REFRESH_MIN_DAYS", "9999")],
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
        &[],
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
        &[("CODEX_AUTO_REFRESH_MIN_DAYS", "1")],
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
        &[("CODEX_AUTO_REFRESH_MIN_DAYS", "9999")],
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
        &[("CODEX_AUTO_REFRESH_MIN_DAYS", "9999")],
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

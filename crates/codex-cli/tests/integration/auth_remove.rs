use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
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

fn stderr(output: &CmdOutput) -> String {
    output.stderr_text()
}

#[test]
fn auth_remove_requires_file_name() {
    let output = run_with(&["auth", "remove"], &[], &[]);
    assert_eq!(output.code, 64);
    assert!(stderr(&output).contains("usage"));
}

#[test]
fn auth_remove_rejects_path_traversal() {
    let output = run_with(&["auth", "remove", "../bad.json"], &[], &[]);
    assert_eq!(output.code, 64);
    assert!(stderr(&output).contains("invalid secret file name"));
}

#[test]
fn auth_remove_errors_when_secret_dir_missing() {
    let output = run_with(
        &["auth", "remove", "alpha.json"],
        &[],
        &[("CODEX_SECRET_DIR", "")],
    );
    assert_eq!(output.code, 1);
    assert!(stderr(&output).contains("CODEX_SECRET_DIR is not configured"));
}

#[test]
fn auth_remove_keeps_env_only_contract_even_if_home_secret_dir_exists() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let home = dir.path().join("home");
    let fallback_secret_dir = home.join(".config").join("codex_secrets");
    let target = fallback_secret_dir.join("alpha.json");
    fs::create_dir_all(&fallback_secret_dir).expect("fallback secret dir");
    fs::write(&target, r#"{"tokens":{"access_token":"tok"}}"#).expect("target");

    let output = run_with(
        &["auth", "remove", "--yes", "alpha.json"],
        &[("HOME", &home)],
        &[("CODEX_SECRET_DIR", "")],
    );

    assert_eq!(output.code, 1);
    assert!(stderr(&output).contains("CODEX_SECRET_DIR is not configured"));
    assert!(
        target.exists(),
        "remove must not use HOME fallback when CODEX_SECRET_DIR is empty"
    );
}

#[test]
fn auth_remove_errors_when_target_missing() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets");

    let output = run_with(
        &["auth", "remove", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
        &[],
    );
    assert_eq!(output.code, 1);
    assert!(stderr(&output).contains("secret file not found"));
}

#[test]
fn auth_remove_requires_yes_in_non_tty_mode() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    let target = secrets.join("alpha.json");
    fs::create_dir_all(&secrets).expect("secrets");
    fs::write(&target, r#"{"tokens":{"access_token":"tok"}}"#).expect("target");

    let output = run_with(
        &["auth", "remove", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
        &[],
    );
    assert_eq!(output.code, 1);
    assert!(stderr(&output).contains("rerun with --yes"));
    assert!(target.exists(), "target should still exist");
}

#[test]
fn auth_remove_yes_deletes_file_and_timestamp() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    let cache = dir.path().join("cache");
    let target = secrets.join("alpha.json");
    let timestamp = cache.join("alpha.json.timestamp");
    fs::create_dir_all(&secrets).expect("secrets");
    fs::create_dir_all(&cache).expect("cache");
    fs::write(&target, r#"{"tokens":{"access_token":"tok"}}"#).expect("target");
    fs::write(&timestamp, "2025-01-20T00:00:00Z").expect("timestamp");

    let output = run_with(
        &["auth", "remove", "--yes", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("CODEX_SECRET_CACHE_DIR", &cache),
        ],
        &[],
    );
    assert_eq!(output.code, 0);
    assert!(stdout(&output).contains("codex: removed"));
    assert!(!target.exists(), "target should be removed");
    assert!(!timestamp.exists(), "timestamp should be removed");
}

#[test]
fn auth_remove_appends_json_suffix_when_missing() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets");
    let target = secrets.join("alpha.json");
    fs::write(&target, r#"{"tokens":{"access_token":"tok"}}"#).expect("target");

    let output = run_with(
        &["auth", "remove", "--yes", "alpha"],
        &[("CODEX_SECRET_DIR", &secrets)],
        &[],
    );
    assert_eq!(output.code, 0);
    assert!(!target.exists(), "alpha.json should be removed");
}

#[test]
fn auth_remove_yes_does_not_create_missing_cache_dir() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    let cache = dir.path().join("cache");
    let target = secrets.join("alpha.json");
    fs::create_dir_all(&secrets).expect("secrets");
    fs::write(&target, r#"{"tokens":{"access_token":"tok"}}"#).expect("target");
    assert!(!cache.exists(), "cache should start missing");

    let output = run_with(
        &["auth", "remove", "--yes", "alpha.json"],
        &[
            ("CODEX_SECRET_DIR", &secrets),
            ("CODEX_SECRET_CACHE_DIR", &cache),
        ],
        &[],
    );
    assert_eq!(output.code, 0);
    assert!(!target.exists(), "target should be removed");
    assert!(
        !cache.exists(),
        "cache directory should stay absent when timestamp file is missing"
    );
}

#[test]
fn auth_remove_json_requires_confirmation() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    let target = secrets.join("alpha.json");
    fs::create_dir_all(&secrets).expect("secrets");
    fs::write(&target, r#"{"tokens":{"access_token":"tok"}}"#).expect("target");

    let output = run_with(
        &["auth", "remove", "--json", "alpha.json"],
        &[("CODEX_SECRET_DIR", &secrets)],
        &[],
    );
    assert_eq!(output.code, 1);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "remove-confirmation-required");
}

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use pretty_assertions::assert_eq;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn gemini_cli_bin() -> PathBuf {
    bin::resolve("gemini-cli")
}

fn run_with(args: &[&str], envs: &[(&str, &Path)], vars: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default();
    for (key, path) in envs {
        options = options.with_env(key, path.to_string_lossy().as_ref());
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

#[test]
fn auth_save_requires_file_name() {
    let output = run_with(&["auth", "save"], &[], &[]);
    assert_eq!(output.code, 64);
    assert!(stderr(&output).contains("usage"));
}

#[test]
fn auth_save_rejects_path_traversal() {
    let output = run_with(&["auth", "save", "../bad.json"], &[], &[]);
    assert_eq!(output.code, 64);
    assert!(stderr(&output).contains("invalid secret file name"));
}

#[test]
fn auth_save_errors_when_secret_dir_missing() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"tok"}}"#).expect("write auth");

    let output = run_with(
        &["auth", "save", "alpha.json"],
        &[("GEMINI_AUTH_FILE", &auth_file)],
        &[
            ("GEMINI_SECRET_DIR", ""),
            ("HOME", ""),
            ("ZDOTDIR", ""),
            ("ZSH_SCRIPT_DIR", ""),
            ("_ZSH_BOOTSTRAP_PRELOAD_PATH", ""),
        ],
    );
    assert_eq!(output.code, 1);
    assert!(stderr(&output).contains("secret directory is not configured"));
}

#[test]
fn auth_save_errors_when_auth_file_missing() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets");
    let missing_auth = dir.path().join("missing-auth.json");

    let output = run_with(
        &["auth", "save", "alpha.json"],
        &[
            ("GEMINI_AUTH_FILE", &missing_auth),
            ("GEMINI_SECRET_DIR", &secrets),
        ],
        &[],
    );
    assert_eq!(output.code, 1);
    assert!(stderr(&output).contains("auth file not found"));
}

#[test]
fn auth_save_writes_target_file() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets");
    let auth_file = dir.path().join("auth.json");
    fs::write(
        &auth_file,
        r#"{"tokens":{"access_token":"tok","refresh_token":"refresh"},"last_refresh":"2025-01-20T00:00:00Z"}"#,
    )
    .expect("write auth");

    let output = run_with(
        &["auth", "save", "alpha.json"],
        &[
            ("GEMINI_AUTH_FILE", &auth_file),
            ("GEMINI_SECRET_DIR", &secrets),
        ],
        &[],
    );
    assert_eq!(output.code, 0);
    assert!(stdout(&output).contains("gemini: saved"));
    assert_eq!(
        fs::read_to_string(secrets.join("alpha.json")).expect("read saved"),
        fs::read_to_string(&auth_file).expect("read auth")
    );
}

#[test]
fn auth_save_overwrite_prompt_default_no_in_non_tty_mode() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets");
    let auth_file = dir.path().join("auth.json");
    let target = secrets.join("alpha.json");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"new"}}"#).expect("write auth");
    fs::write(&target, r#"{"tokens":{"access_token":"old"}}"#).expect("write target");

    let output = run_with(
        &["auth", "save", "alpha.json"],
        &[
            ("GEMINI_AUTH_FILE", &auth_file),
            ("GEMINI_SECRET_DIR", &secrets),
        ],
        &[],
    );
    assert_eq!(output.code, 1);
    assert!(stderr(&output).contains("rerun with --yes"));
    assert_eq!(
        fs::read_to_string(&target).expect("read target"),
        r#"{"tokens":{"access_token":"old"}}"#
    );
}

#[test]
fn auth_save_overwrite_yes_bypasses_prompt() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets");
    let auth_file = dir.path().join("auth.json");
    let target = secrets.join("alpha.json");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"new"}}"#).expect("write auth");
    fs::write(&target, r#"{"tokens":{"access_token":"old"}}"#).expect("write target");

    let output = run_with(
        &["auth", "save", "--yes", "alpha.json"],
        &[
            ("GEMINI_AUTH_FILE", &auth_file),
            ("GEMINI_SECRET_DIR", &secrets),
        ],
        &[],
    );
    assert_eq!(output.code, 0);
    assert!(stdout(&output).contains("(overwritten)"));
    assert_eq!(
        fs::read_to_string(&target).expect("read target"),
        r#"{"tokens":{"access_token":"new"}}"#
    );
}

#[test]
fn auth_save_json_overwrite_requires_confirmation() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let secrets = dir.path().join("secrets");
    fs::create_dir_all(&secrets).expect("secrets");
    let auth_file = dir.path().join("auth.json");
    let target = secrets.join("alpha.json");
    fs::write(&auth_file, r#"{"tokens":{"access_token":"new"}}"#).expect("write auth");
    fs::write(&target, r#"{"tokens":{"access_token":"old"}}"#).expect("write target");

    let output = run_with(
        &["auth", "save", "--json", "alpha.json"],
        &[
            ("GEMINI_AUTH_FILE", &auth_file),
            ("GEMINI_SECRET_DIR", &secrets),
        ],
        &[],
    );
    assert_eq!(output.code, 1);
    let payload: Value = serde_json::from_str(&stdout(&output)).expect("json");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "overwrite-confirmation-required");
}

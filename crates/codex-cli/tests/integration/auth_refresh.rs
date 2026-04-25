use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};

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

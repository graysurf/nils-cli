//! CLI contract tests for `github-app-cli` (offline paths only).
//!
//! Network-backed `token` / `installations` success paths require live GitHub
//! credentials and are exercised manually; here we cover parsing, exit codes,
//! the JSON error envelope, and completion export.

use std::process::Command;

fn bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_github-app-cli"));
    // Keep tests hermetic regardless of the developer's shell environment.
    cmd.env_remove("GITHUB_APP_ID")
        .env_remove("GITHUB_APP_INSTALLATION_ID")
        .env_remove("GITHUB_APP_PRIVATE_KEY")
        .env_remove("GITHUB_APP_PRIVATE_KEY_PATH")
        .env_remove("GITHUB_API_URL");
    cmd
}

#[test]
fn version_flag_succeeds() {
    let out = bin().arg("--version").output().expect("run --version");
    assert!(out.status.success());
}

#[test]
fn unknown_subcommand_exits_usage() {
    let out = bin().arg("definitely-not-a-command").output().expect("run");
    assert_eq!(out.status.code(), Some(64));
}

#[test]
fn unknown_subcommand_json_emits_error_envelope() {
    let out = bin()
        .args(["--format", "json", "definitely-not-a-command"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(64));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"schema_version\":\"cli.github-app-cli.error.v1\""),
        "expected error envelope, got: {stdout}"
    );
    assert!(stdout.contains("\"ok\":false"), "got: {stdout}");
}

#[test]
fn token_without_key_is_usage_error() {
    // app-id + installation-id supplied as flags; no key anywhere -> exit 64.
    let out = bin()
        .args(["token", "--app-id", "1", "--installation-id", "2"])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(64));
}

#[test]
fn token_without_key_json_error_has_stable_code() {
    let out = bin()
        .args([
            "--format",
            "json",
            "token",
            "--app-id",
            "1",
            "--installation-id",
            "2",
        ])
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(64));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"code\":\"missing-key\""), "got: {stdout}");
    assert!(stdout.contains("\"ok\":false"), "got: {stdout}");
}

#[test]
fn completion_zsh_exports_compdef() {
    let out = bin().args(["completion", "zsh"]).output().expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("#compdef github-app-cli"),
        "expected zsh compdef header"
    );
}

#[test]
fn completion_bash_exports_function() {
    let out = bin().args(["completion", "bash"]).output().expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("github-app-cli"),
        "expected bash completion"
    );
}

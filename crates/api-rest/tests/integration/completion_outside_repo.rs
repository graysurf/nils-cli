use std::path::PathBuf;
use std::process::{Command, Stdio};

use nils_test_support::bin::resolve;

fn api_rest_bin() -> PathBuf {
    resolve("api-rest")
}

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = Command::new(api_rest_bin())
        .args(["completion", "zsh"])
        .current_dir(temp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run api-rest completion zsh");

    assert!(
        output.status.success(),
        "expected exit code 0, got: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("#compdef api-rest"),
        "missing zsh completion header: {stdout}"
    );
}

#[test]
fn completion_rejects_unknown_shell_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let output = Command::new(api_rest_bin())
        .args(["completion", "fish"])
        .current_dir(temp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run api-rest completion fish");

    assert!(
        !output.status.success(),
        "expected non-zero exit code for unknown shell, got: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid value") && stderr.contains("fish"),
        "missing invalid shell error: {stderr}"
    );
}

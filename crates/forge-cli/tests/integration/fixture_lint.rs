//! Sprint 7 Task 7.3 — synthetic regression for the fixture redaction lint.
//!
//! Spec: `forge-cli-spec-v1` §"Security and redaction expectations". The
//! lint script lives at `scripts/ci/forge-cli-fixture-lint.sh`; this test
//! plants a fake `ghp_aaaaaaaaaaaaaaaaaaaa` string into a tempdir and runs
//! the script against that root, asserting the script fails (non-zero exit)
//! and reports the file path + line in stderr.

use std::fs;
use std::process::Command;

use tempfile::TempDir;

#[test]
fn fixture_lint_catches_planted_ghp_token() {
    let tempdir = TempDir::new().expect("tempdir");
    let bad_file = tempdir.path().join("planted.json");
    fs::write(&bad_file, r#"{ "token": "ghp_aaaaaaaaaaaaaaaaaaaa" }"#)
        .expect("write planted fixture");

    let script = std::env::current_dir()
        .expect("cwd")
        .ancestors()
        .find(|p| p.join("scripts/ci/forge-cli-fixture-lint.sh").is_file())
        .expect("locate lint script root (cargo test from any depth)")
        .join("scripts/ci/forge-cli-fixture-lint.sh");

    let output = Command::new("bash")
        .arg(&script)
        .arg(tempdir.path())
        .output()
        .expect("spawn fixture lint");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "expected non-zero exit on planted token; stderr={stderr}"
    );
    assert!(
        stderr.contains("planted.json"),
        "expected the planted filename in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("ghp_aaaaaaaaaaaaaaaaaaaa"),
        "expected the offending match echoed in stderr, got: {stderr}"
    );
}

#[test]
fn fixture_lint_passes_on_clean_tempdir() {
    let tempdir = TempDir::new().expect("tempdir");
    fs::write(
        tempdir.path().join("clean.json"),
        r#"{ "token": "<redacted-token>" }"#,
    )
    .expect("write clean fixture");
    fs::write(
        tempdir.path().join("clean.txt"),
        "Bearer short\n", // under the 16-char threshold, must not match
    )
    .expect("write short bearer fixture");

    let script = std::env::current_dir()
        .expect("cwd")
        .ancestors()
        .find(|p| p.join("scripts/ci/forge-cli-fixture-lint.sh").is_file())
        .expect("locate lint script root")
        .join("scripts/ci/forge-cli-fixture-lint.sh");

    let output = Command::new("bash")
        .arg(&script)
        .arg(tempdir.path())
        .output()
        .expect("spawn fixture lint");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "expected zero exit on clean tempdir; stdout={stdout}, stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.starts_with("PASS:"), "stdout={stdout}");
}

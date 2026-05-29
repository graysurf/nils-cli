//! Integration coverage for `agent-runtime doctor --class version-alignment`.

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn agent_runtime_bin() -> PathBuf {
    bin::resolve("agent-runtime")
}

fn run(args: &[&str]) -> CmdOutput {
    cmd::run(&agent_runtime_bin(), args, &[], None)
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

/// First `major.minor.patch` token reported by `agent-runtime --version`.
/// The host check compares against the running binary's own version, so the
/// test self-calibrates instead of hardcoding a release tag.
fn host_mmp() -> String {
    let text = run(&["--version"]).stdout_text();
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|token| {
            let parts: Vec<&str> = token.split('.').collect();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(str::to_string)
        .expect("agent-runtime --version reports a semver")
}

fn pin_manifest(pinned_tag: &str, required: &[(&str, &str)]) -> String {
    let mut body = format!("schema_version: 1\nnils_cli:\n  pinned_tag: \"{pinned_tag}\"\n");
    if !required.is_empty() {
        body.push_str("required_clis:\n");
        for (bin, min) in required {
            body.push_str(&format!("  - bin: {bin}\n    min: \"{min}\"\n"));
        }
    }
    body
}

fn write_pin(tmp: &TempDir, body: &str) -> String {
    let path = tmp.path().join("pin.yaml");
    write(&path, body);
    path.to_string_lossy().into_owned()
}

#[test]
fn version_alignment_aligned_host_exits_zero() {
    let mmp = host_mmp();
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(&tmp, &pin_manifest(&format!("v{mmp}"), &[]));

    let output = run(&[
        "doctor",
        "--class",
        "version-alignment",
        "--pin",
        &pin,
        "--format",
        "json",
    ]);

    assert_eq!(
        output.code,
        0,
        "aligned host should exit 0; stderr={}",
        output.stderr_text()
    );
    let json: Value = serde_json::from_str(&output.stdout_text()).unwrap();
    assert_eq!(json["schema_version"], "agent-runtime-cli.doctor.v1");
    assert_eq!(json["exit_code"], 0);
    assert_eq!(json["version_alignment"]["pinned_tag"], format!("v{mmp}"));
    assert_eq!(
        json["version_alignment"]["items"][0]["check"],
        "version-alignment.host"
    );
    assert_eq!(json["version_alignment"]["items"][0]["severity"], "ok");
}

#[test]
fn version_alignment_drifted_host_blocks() {
    let tmp = TempDir::new().unwrap();
    // v999.0.0 cannot match any real host build.
    let pin = write_pin(&tmp, &pin_manifest("v999.0.0", &[]));

    let output = run(&[
        "doctor",
        "--class",
        "version-alignment",
        "--pin",
        &pin,
        "--format",
        "json",
    ]);

    assert_eq!(
        output.code,
        2,
        "drift should block (exit 2); stderr={}",
        output.stderr_text()
    );
    let json: Value = serde_json::from_str(&output.stdout_text()).unwrap();
    assert_eq!(json["block"], 1);
    assert_eq!(json["exit_code"], 2);
    assert_eq!(json["version_alignment"]["items"][0]["severity"], "block");
    assert_eq!(json["findings"][0]["check"], "version-alignment.host");
}

#[test]
fn version_alignment_missing_required_cli_blocks() {
    let mmp = host_mmp();
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(
        &tmp,
        &pin_manifest(
            &format!("v{mmp}"),
            &[("nils-cli-no-such-binary-xyz", "0.1.0")],
        ),
    );

    let output = run(&[
        "doctor",
        "--class",
        "version-alignment",
        "--pin",
        &pin,
        "--format",
        "json",
    ]);

    assert_eq!(
        output.code,
        2,
        "missing required cli should block; stderr={}",
        output.stderr_text()
    );
    let json: Value = serde_json::from_str(&output.stdout_text()).unwrap();
    let findings = json["findings"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|f| f["check"] == "version-alignment.required-cli"),
        "expected a required-cli finding: {findings:?}"
    );
}

#[test]
fn version_alignment_schema_mismatch_errors() {
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(
        &tmp,
        "schema_version: 99\nnils_cli:\n  pinned_tag: \"v0.17.7\"\n",
    );

    let output = run(&["doctor", "--class", "version-alignment", "--pin", &pin]);

    assert_ne!(output.code, 0, "schema mismatch should error");
    assert!(
        output.stderr_text().contains("schema_version"),
        "stderr should name schema_version: {}",
        output.stderr_text()
    );
}

#[test]
fn version_alignment_requires_pin_flag() {
    let output = run(&["doctor", "--class", "version-alignment", "--format", "json"]);

    assert_ne!(output.code, 0, "missing --pin should error");
    assert!(
        output.stderr_text().contains("--pin"),
        "stderr should name --pin: {}",
        output.stderr_text()
    );
}

#[test]
fn version_alignment_text_output_ends_with_acceptance_boundary() {
    let mmp = host_mmp();
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(&tmp, &pin_manifest(&format!("v{mmp}"), &[]));

    let output = run(&["doctor", "--class", "version-alignment", "--pin", &pin]);

    assert_eq!(output.code, 0);
    let stderr = output.stderr_text();
    assert!(
        stderr.trim_end().ends_with(
            "agent-runtime doctor: acceptance-boundary: version-number gate only; does not diff surfaces between tags or query a registry for newer releases"
        ),
        "stderr should end with acceptance boundary: {stderr}"
    );
}

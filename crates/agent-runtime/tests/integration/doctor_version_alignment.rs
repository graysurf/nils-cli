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

fn version_policy_manifest(
    minimum_supported_tag: &str,
    validated_tag: &str,
    required: &[(&str, &str)],
) -> String {
    let mut body = format!(
        "schema_version: 2\nnils_cli:\n  minimum_supported_tag: \"{minimum_supported_tag}\"\n  validated_tag: \"{validated_tag}\"\n  release_sha256:\n    linux_amd64: \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n    linux_arm64: \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"\n"
    );
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
    assert!(json["version_alignment"]["minimum_supported_tag"].is_null());
    assert!(json["version_alignment"]["validated_tag"].is_null());
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
fn version_alignment_schema_v2_exact_validated_host_exits_zero() {
    let mmp = host_mmp();
    let tag = format!("v{mmp}");
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(&tmp, &version_policy_manifest(&tag, &tag, &[]));

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
        "exact validated host should pass; stderr={}",
        output.stderr_text()
    );
    let json: Value = serde_json::from_str(&output.stdout_text()).unwrap();
    assert_eq!(
        json["version_alignment"]["minimum_supported_tag"],
        tag.as_str()
    );
    assert_eq!(json["version_alignment"]["validated_tag"], tag.as_str());
    assert_eq!(json["warn"], 0);
    assert_eq!(json["block"], 0);
    let checks: Vec<&str> = json["version_alignment"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["check"].as_str())
        .collect();
    assert!(checks.contains(&"version-alignment.minimum"));
    assert!(checks.contains(&"version-alignment.validated"));
}

#[test]
fn version_alignment_schema_v2_ahead_warns_without_blocking() {
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(&tmp, &version_policy_manifest("v0.1.0", "v0.1.0", &[]));

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
        "ahead host should warn without blocking; stderr={}",
        output.stderr_text()
    );
    let json: Value = serde_json::from_str(&output.stdout_text()).unwrap();
    assert_eq!(json["warn"], 1);
    assert_eq!(json["block"], 0);
    assert_eq!(json["exit_code"], 0);
    assert!(json["findings"].as_array().unwrap().iter().any(|finding| {
        finding["check"] == "version-alignment.validated"
            && finding["severity"] == "warn"
            && finding["message"]
                .as_str()
                .is_some_and(|message| message.contains("not formally validated"))
    }));
}

#[test]
fn version_alignment_schema_v2_below_minimum_blocks() {
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(&tmp, &version_policy_manifest("v999.0.0", "v999.0.0", &[]));

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
        "below-minimum host should block; stderr={}",
        output.stderr_text()
    );
    let json: Value = serde_json::from_str(&output.stdout_text()).unwrap();
    assert_eq!(json["block"], 1);
    assert!(json["findings"].as_array().unwrap().iter().any(|finding| {
        finding["check"] == "version-alignment.minimum" && finding["severity"] == "block"
    }));
}

#[test]
fn version_alignment_schema_v2_rejects_inverted_range() {
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(&tmp, &version_policy_manifest("v999.0.0", "v0.1.0", &[]));

    let output = run(&["doctor", "--class", "version-alignment", "--pin", &pin]);

    assert_ne!(output.code, 0, "inverted range should fail closed");
    let stderr = output.stderr_text();
    assert!(
        stderr.contains("minimum_supported_tag") && stderr.contains("validated_tag"),
        "stderr should name the invalid relationship: {stderr}"
    );
}

#[test]
fn version_alignment_schema_v2_rejects_non_stable_tags() {
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(
        &tmp,
        &version_policy_manifest("v1.2.3-beta.1", "v1.2.3", &[]),
    );

    let output = run(&["doctor", "--class", "version-alignment", "--pin", &pin]);

    assert_ne!(output.code, 0, "prerelease policy tag should fail closed");
    assert!(
        output.stderr_text().contains("stable tag"),
        "stderr should explain the stable-tag contract: {}",
        output.stderr_text()
    );
}

#[test]
fn version_alignment_schema_v2_rejects_build_metadata_tags() {
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(
        &tmp,
        &version_policy_manifest("v1.2.3+canary.1", "v1.2.3", &[]),
    );

    let output = run(&["doctor", "--class", "version-alignment", "--pin", &pin]);

    assert_ne!(
        output.code, 0,
        "build-metadata policy tag should fail closed"
    );
    assert!(
        output.stderr_text().contains("stable tag"),
        "stderr should explain the stable-tag contract: {}",
        output.stderr_text()
    );
}

#[test]
fn version_alignment_schema_v2_requires_validated_tag() {
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(
        &tmp,
        "schema_version: 2\nnils_cli:\n  minimum_supported_tag: \"v1.2.3\"\n  release_sha256:\n    linux_amd64: \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n    linux_arm64: \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"\n",
    );

    let output = run(&["doctor", "--class", "version-alignment", "--pin", &pin]);

    assert_ne!(output.code, 0, "missing validated_tag should fail closed");
    assert!(
        output.stderr_text().contains("validated_tag"),
        "stderr should name the missing validated role: {}",
        output.stderr_text()
    );
}

#[test]
fn version_alignment_schema_v2_requires_validated_release_digests() {
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(
        &tmp,
        "schema_version: 2\nnils_cli:\n  minimum_supported_tag: \"v1.2.3\"\n  validated_tag: \"v1.2.3\"\n",
    );

    let output = run(&["doctor", "--class", "version-alignment", "--pin", &pin]);

    assert_ne!(output.code, 0, "missing release digests should fail closed");
    assert!(
        output.stderr_text().contains("release_sha256"),
        "stderr should name validated-release digest ownership: {}",
        output.stderr_text()
    );
}

#[test]
fn version_alignment_schema_v2_rejects_invalid_required_cli_floor() {
    let mmp = host_mmp();
    let tag = format!("v{mmp}");
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(
        &tmp,
        &version_policy_manifest(&tag, &tag, &[("plan-issue", "v1.0.0-beta.1")]),
    );

    let output = run(&["doctor", "--class", "version-alignment", "--pin", &pin]);

    assert_ne!(
        output.code, 0,
        "invalid required CLI floor should fail closed"
    );
    assert!(
        output
            .stderr_text()
            .contains("required_clis[plan-issue].min"),
        "stderr should identify the invalid floor: {}",
        output.stderr_text()
    );
}

#[test]
fn version_alignment_schema_v2_rejects_duplicate_required_clis() {
    let mmp = host_mmp();
    let tag = format!("v{mmp}");
    let tmp = TempDir::new().unwrap();
    let pin = write_pin(
        &tmp,
        &version_policy_manifest(
            &tag,
            &tag,
            &[("plan-issue", "1.0.0"), ("plan-issue", "1.1.0")],
        ),
    );

    let output = run(&["doctor", "--class", "version-alignment", "--pin", &pin]);

    assert_ne!(output.code, 0, "duplicate CLI policy should fail closed");
    assert!(
        output.stderr_text().contains("duplicate required_clis"),
        "stderr should identify the duplicate: {}",
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

use tempfile::TempDir;

use crate::common;
#[test]
fn profile_validate_accepts_default_fixture() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("real_e2e_profile_default_1440p.json");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "profile",
            "validate",
            "--file",
            fixture.to_str().unwrap(),
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("profile validate json");
    assert_eq!(payload["command"], serde_json::json!("profile.validate"));
    assert_eq!(payload["result"]["valid"], serde_json::json!(true));
}

#[test]
fn profile_init_writes_scaffold_to_requested_path() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let output = cwd.path().join("generated-profile.json");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "profile",
            "init",
            "--name",
            "ci-1440p",
            "--path",
            output.to_str().unwrap(),
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert!(output.is_file());

    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("profile init json");
    assert_eq!(payload["command"], serde_json::json!("profile.init"));
    assert_eq!(
        payload["result"]["profile_name"],
        serde_json::json!("ci-1440p")
    );
}

#[test]
fn profile_validate_reports_actionable_error_for_missing_keys() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let bad = cwd.path().join("bad-profile.json");
    std::fs::write(&bad, "{\"profile_name\":\"\",\"arc\":{}}").expect("write bad profile");

    let out = harness.run(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "profile",
            "validate",
            "--file",
            bad.to_str().unwrap(),
        ],
    );

    assert_eq!(out.code, 2);
    let payload: serde_json::Value =
        serde_json::from_str(&out.stderr_text()).expect("profile validate error json");
    assert_eq!(
        payload["error"]["operation"],
        serde_json::json!("profile.validate")
    );
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("profile_name")
    );
}

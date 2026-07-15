use std::path::PathBuf;

use tempfile::TempDir;

use crate::common;

#[test]
fn root_help_exposes_only_the_peekaboo_adapter_surface() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(cwd.path(), &["--help"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let help = format!("{}{}", out.stdout_text(), out.stderr_text());
    for command in [
        "backend",
        "doctor",
        "capabilities",
        "exec",
        "scenario",
        "mcp",
        "journal",
    ] {
        assert!(
            help.contains(command),
            "missing new adapter command: {command}"
        );
    }
    for retired in ["preflight", "input-source", "ax", "observe", "profile"] {
        assert!(
            !help.contains(&format!("\n  {retired}")),
            "retired engine command still exposed: {retired}"
        );
    }
}

#[test]
fn readme_maps_every_retired_public_surface_to_the_adapter_v2_boundary() {
    let readme =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md"))
            .expect("README");
    for required in [
        "## Migrating from the native engine",
        "`preflight` → `doctor --strict`",
        "`windows`, `apps`, `window`, `input`, `input-source`, and `ax`",
        "`observe`, `debug`, `wait`, and `profile`",
        "`scenario`",
        "`macos-agent.adapter.v2`",
        "exit codes",
        "nils-cli v1.22.6",
    ] {
        assert!(
            readme.contains(required),
            "missing migration contract: {required}"
        );
    }
}

#[test]
fn repository_contains_a_complete_immutable_peekaboo_lock() {
    let lock_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("peekaboo-lock.json");
    let raw = std::fs::read_to_string(&lock_path)
        .unwrap_or_else(|err| panic!("required Peekaboo lock is missing: {err}"));
    let lock: serde_json::Value = serde_json::from_str(&raw).expect("lock must be valid JSON");

    assert_eq!(lock["schema_version"], 2);
    assert_eq!(lock["repository"], "https://github.com/openclaw/Peekaboo");
    assert_eq!(lock["tag"], "v3.9.3");
    assert_eq!(lock["commit"], "3cfd612adbcb1b43e8431a7a1f3b02ec45d01269");
    assert_eq!(lock["minimum_macos"], "15.0");
    assert_eq!(lock["assets"].as_array().map(Vec::len), Some(2));
    assert_eq!(lock["assets"][0]["notarization"]["policy"], "waived");
    assert_eq!(
        lock["assets"][0]["notarization"]["waiver"]["approval"],
        "https://github.com/graysurf/agent-runtime-kit/issues/610#issuecomment-4984437753"
    );
    assert_eq!(lock["assets"][1]["notarization"]["policy"], "required");
    assert!(
        lock["required_capability_probes"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
}

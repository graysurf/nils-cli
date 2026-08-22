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
        "mcp",
        "journal",
    ] {
        assert!(
            help.contains(command),
            "missing new adapter command: {command}"
        );
    }
    for retired in [
        "preflight",
        "input-source",
        "ax",
        "observe",
        "profile",
        "scenario",
    ] {
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
        "Peekaboo v4 removed the `.peekaboo.json` runner",
        "`macos-agent.adapter.v3`",
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
    assert_eq!(lock["tag"], "v4.2.2");
    assert_eq!(lock["commit"], "05675b0b5e2c382146963e19493787d9dac0d45b");
    assert_eq!(lock["minimum_macos"], "15.0");
    assert_eq!(lock["assets"].as_array().map(Vec::len), Some(2));
    assert_eq!(lock["assets"][0]["notarization"]["policy"], "required");
    assert_eq!(lock["assets"][1]["notarization"]["policy"], "required");
    assert_eq!(lock["rollback_releases"].as_array().map(Vec::len), Some(0));
    assert_eq!(
        lock["upgrade_from_releases"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(lock["upgrade_from_releases"][0]["tag"], "v3.9.3");
    let probe_ids = lock["required_capability_probes"]
        .as_array()
        .expect("probe array")
        .iter()
        .map(|probe| probe["id"].as_str().expect("probe id"))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        probe_ids,
        std::collections::BTreeSet::from([
            "action",
            "bridge",
            "click",
            "mcp_stdio",
            "observation",
            "permissions",
            "press",
            "tools",
            "verification",
            "version",
        ])
    );
}

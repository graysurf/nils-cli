//! Integration coverage for Plan 04 Sprint 3 Task 3.1 doctor probes.

use agent_runtime::doctor::{self, DoctorOptions, DoctorSeverity};
use agent_runtime::install::{self, InstallOptions, Mode};
use agent_runtime::managed_block::{CommentStyle, ManagedBlock};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

fn fixed_time() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

fn build_source_root(tmp: &Path, product: &str, home: &Path, state_home: &Path) -> PathBuf {
    let root = tmp.join("src");
    fs::create_dir_all(root.join("manifests")).unwrap();
    let bin = root.join("bin").join(format!("{product}-version"));
    fs::create_dir_all(bin.parent().unwrap()).unwrap();
    fs::write(
        &bin,
        format!("#!/usr/bin/env sh\necho \"{product} 0.0.0\"\n"),
    )
    .unwrap();
    let mut perms = fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&bin, perms).unwrap();

    let skill = root
        .join("build")
        .join(product)
        .join("plugins")
        .join("reporting")
        .join("skills")
        .join("daily-brief")
        .join("SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::write(&skill, "# daily-brief\n").unwrap();

    let target_dir = root.join("targets").join(product);
    fs::create_dir_all(&target_dir).unwrap();
    fs::write(
        target_dir.join("link-map.yaml"),
        format!(
            "\
schema_version: 1
entries:
  - id: reporting.daily-brief
    kind: symlinked-file
    source: build/{product}/plugins/reporting/skills/daily-brief/SKILL.md
    destination: plugins/reporting/skills/daily-brief/SKILL.md
  - id: {product}.hooks
    kind: managed-block
    destination: settings.json
    surface: hooks
    comment_style: double-slash
    body_template: '\"hooks\": []'
",
        ),
    )
    .unwrap();

    let runtime_roots = format!(
        "\
schema_version: 1
products:
  codex:
    live_home: \"{home}\"
    docs_home: \"{home}\"
    state_home: \"{state_home}\"
    plugin_root: \"{home}/plugins\"
    hook_config_strategy: managed-block
    min_version: \"0.0.0\"
    recommended_version: \"0.0.0\"
    min_version_effective_from: \"2099-01-01\"
    version_probe: \"{version_probe}\"
  claude:
    live_home: \"{home}\"
    docs_home: \"{home}\"
    state_home: \"{state_home}\"
    hook_config_strategy: settings-json
    min_version: \"0.0.0\"
    recommended_version: \"0.0.0\"
    min_version_effective_from: \"2099-01-01\"
    version_probe: \"{version_probe}\"
  hermes:
    live_home: \"{home}\"
    docs_home: \"{home}\"
    state_home: \"{state_home}\"
    min_version: \"0.0.0\"
    recommended_version: \"0.0.0\"
    min_version_effective_from: \"2099-01-01\"
    version_probe: \"{version_probe}\"
",
        home = home.display(),
        state_home = state_home.display(),
        version_probe = bin.display(),
    );
    fs::write(
        root.join("manifests").join("runtime-roots.yaml"),
        runtime_roots,
    )
    .unwrap();
    fs::write(
        root.join("manifests").join("skills.yaml"),
        "schema_version: 1\nskills: []\n",
    )
    .unwrap();
    fs::write(
        root.join("manifests").join("cli-tools.yaml"),
        "schema_version: 1\nprofiles:\n  core: []\n  recommended: []\n  full: []\nformulas: {}\n",
    )
    .unwrap();

    fs::canonicalize(&root).unwrap()
}

fn install_clean(product: &str, source_root: &Path, home: &Path, state_home: &Path) {
    install::run(
        product,
        source_root,
        home,
        state_home,
        Mode::Apply,
        fixed_time(),
        &InstallOptions::default(),
    )
    .unwrap();
}

fn commit_source(source_root: &Path) {
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["config", "user.name", "Fixture"],
        vec!["add", "."],
        vec!["commit", "-qm", "fixture"],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(source_root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
}

#[test]
fn installed_runtime_class_requires_and_verifies_portable_receipt() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&home).unwrap();
    let source_root = build_source_root(tmp.path(), "claude", &home, &state_home);
    commit_source(&source_root);
    let options = DoctorOptions {
        class_filter: Some(doctor::DoctorClass::InstalledRuntime),
        ..DoctorOptions::default()
    };

    let missing = doctor::run(
        "claude",
        &source_root,
        Some(&home),
        Some(&state_home),
        &options,
    )
    .unwrap();
    assert_eq!(missing.exit_code(), 2);
    assert!(!missing.installed_runtime.unwrap().receipt_present);

    install_clean("claude", &source_root, &home, &state_home);
    let verified = doctor::run(
        "claude",
        &source_root,
        Some(&home),
        Some(&state_home),
        &options,
    )
    .unwrap();
    assert_eq!(verified.exit_code(), 0, "findings: {:?}", verified.findings);
    let report = verified.installed_runtime.unwrap();
    assert!(report.verified);
    assert!(report.source_clean);
    assert!(report.plan_match);

    let receipt_path = state_home.join("receipts/claude.json");
    let mut receipt: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&receipt_path).unwrap()).unwrap();
    let keys: std::collections::BTreeSet<_> =
        receipt.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        [
            "install_plan_digest",
            "managed_entries",
            "producer_version",
            "product",
            "recorded_at_unix_seconds",
            "schema",
            "source_dirty",
            "source_revision",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    );
    for entry in receipt["managed_entries"].as_array().unwrap() {
        let entry_keys: std::collections::BTreeSet<_> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(entry_keys, ["digest", "id"].into_iter().collect());
    }
    receipt["private_state_home"] =
        serde_json::Value::String("/PRIVATE/HOST/ACCOUNT/STATE_HOME_SENTINEL".to_string());
    fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    let tampered = doctor::run(
        "claude",
        &source_root,
        Some(&home),
        Some(&state_home),
        &options,
    )
    .unwrap();
    assert_eq!(tampered.exit_code(), 2);
    let rendered = format!("{:?}", tampered.findings);
    assert!(!rendered.contains("STATE_HOME_SENTINEL"));

    install_clean("claude", &source_root, &home, &state_home);

    let live_target = home.join("plugins/reporting/skills/daily-brief/SKILL.md");
    fs::remove_file(&live_target).unwrap();
    fs::write(&live_target, "# drifted\n").unwrap();
    let drifted = doctor::run(
        "claude",
        &source_root,
        Some(&home),
        Some(&state_home),
        &options,
    )
    .unwrap();
    assert_eq!(drifted.exit_code(), 2);
    assert!(
        !drifted.installed_runtime.unwrap().verified,
        "nested receipt verdict must include live managed-target acceptance"
    );
}

fn run_doctor(
    product: &str,
    source_root: &Path,
    home: &Path,
    state_home: &Path,
) -> doctor::DoctorOutcome {
    doctor::run(
        product,
        source_root,
        Some(home),
        Some(state_home),
        &DoctorOptions::default(),
    )
    .unwrap()
}

#[test]
fn clean_install_produces_zero_findings_and_exit_zero() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&state_home).unwrap();
    let source_root = build_source_root(tmp.path(), "claude", &home, &state_home);

    install_clean("claude", &source_root, &home, &state_home);

    let outcome = run_doctor("claude", &source_root, &home, &state_home);
    assert_eq!(outcome.exit_code(), 0);
    assert_eq!(outcome.findings, Vec::new());
    assert!(outcome.ok > 0, "doctor should count successful probes");
}

#[test]
fn broken_tracked_symlink_blocks_and_exits_two() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&state_home).unwrap();
    let source_root = build_source_root(tmp.path(), "claude", &home, &state_home);

    install_clean("claude", &source_root, &home, &state_home);
    fs::remove_file(
        home.join("plugins")
            .join("reporting")
            .join("skills")
            .join("daily-brief")
            .join("SKILL.md"),
    )
    .unwrap();

    let outcome = run_doctor("claude", &source_root, &home, &state_home);
    assert_eq!(outcome.exit_code(), 2);
    assert!(
        outcome.findings.iter().any(|finding| {
            finding.severity == DoctorSeverity::Block
                && finding.check == "link-map.symlink"
                && finding.entry_id.as_deref() == Some("reporting.daily-brief")
        }),
        "expected broken symlink block finding: {:#?}",
        outcome.findings
    );
}

#[test]
fn unbalanced_managed_block_marker_blocks_and_exits_two() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&state_home).unwrap();
    let source_root = build_source_root(tmp.path(), "claude", &home, &state_home);

    install_clean("claude", &source_root, &home, &state_home);
    let config = home.join("settings.json");
    let block = ManagedBlock::new("hooks", CommentStyle::DoubleSlash);
    let edited = fs::read_to_string(&config)
        .unwrap()
        .replace(&(block.close_marker() + "\n"), "");
    fs::write(&config, edited).unwrap();

    let outcome = run_doctor("claude", &source_root, &home, &state_home);
    assert_eq!(outcome.exit_code(), 2);
    assert!(
        outcome.findings.iter().any(|finding| {
            finding.severity == DoctorSeverity::Block
                && finding.check == "managed-block"
                && finding.message.contains("unbalanced")
        }),
        "expected unbalanced managed-block finding: {:#?}",
        outcome.findings
    );
}

#[test]
fn install_receipt_materializes_state_home_for_clean_doctor() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&home).unwrap();
    let source_root = build_source_root(tmp.path(), "claude", &home, &state_home);

    install_clean("claude", &source_root, &home, &state_home);

    let outcome = run_doctor("claude", &source_root, &home, &state_home);
    assert_eq!(outcome.exit_code(), 0);
    assert!(
        outcome
            .installed_runtime
            .as_ref()
            .is_some_and(|report| report.receipt_present),
        "expected installed-runtime receipt: {:#?}",
        outcome.findings
    );
}

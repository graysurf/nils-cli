//! `agent-runtime install` flag tests. Plan 04 Sprint 1 Task 1.3.
//!
//! Covers:
//!
//! - `--live-home`: rejects relative paths with a usage error naming the
//!   flag (Open Q1 resolved-default behaviour); accepts absolute paths.
//! - `--tag`: writes a `tag-<name>` marker file at the backup-run root
//!   when at least one backup is created; rejects unsafe characters.
//! - `--no-overlay` / `--overlay-path`: the `.private/link-map.overrides.yaml`
//!   merge runs before plan generation and can be redirected or skipped.

use agent_runtime_cli::install::{self, InstallOptions, Mode};
use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

fn agent_runtime_bin() -> PathBuf {
    bin::resolve("agent-runtime")
}

fn run_cli(args: &[&str]) -> CmdOutput {
    let bin = agent_runtime_bin();
    cmd::run(&bin, args, &[], None)
}

fn fixed_time() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// Build a source root matching the install_pipeline test fixture so the
/// in-process install API has something realistic to run against.
fn build_source_root(tmp: &Path, product: &str) -> PathBuf {
    let root = tmp.join("src");
    fs::create_dir_all(&root).unwrap();
    let plugin_dir = root
        .join("targets")
        .join(product)
        .join("plugins")
        .join("reporting")
        .join(format!(".{product}-plugin"));
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join("plugin.json"),
        r#"{"name":"reporting","version":"0.1.0"}"#,
    )
    .unwrap();
    let build_dir = root
        .join("build")
        .join(product)
        .join("plugins")
        .join("reporting")
        .join("skills");
    fs::create_dir_all(build_dir.join("daily-brief")).unwrap();
    fs::write(
        build_dir.join("daily-brief").join("SKILL.md"),
        "# daily-brief\n",
    )
    .unwrap();
    let link_map = format!(
        "schema_version: 1\nentries:\n  - id: reporting.plugin-manifest\n    kind: plugin-manifest-copy\n    source: targets/{product}/plugins/reporting/.{product}-plugin/plugin.json\n    destination: plugins/reporting/.{product}-plugin/plugin.json\n  - id: reporting.skills-tree\n    kind: symlinked-file\n    source: build/{product}/plugins/reporting/skills\n    destination: plugins/reporting/skills\n    recursive: true\n",
    );
    fs::write(
        root.join("targets").join(product).join("link-map.yaml"),
        link_map,
    )
    .unwrap();
    fs::canonicalize(&root).unwrap()
}

// ---------------------------------------------------------------------------
// --live-home
// ---------------------------------------------------------------------------

#[test]
fn live_home_rejects_relative_path_with_usage_error() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&state_home).unwrap();

    let source_arg = source_root.to_string_lossy().into_owned();
    let state_arg = state_home.to_string_lossy().into_owned();
    let output = run_cli(&[
        "install",
        "--source-root",
        &source_arg,
        "--product",
        "claude",
        "--live-home",
        "./relative-sandbox",
        "--state-home",
        &state_arg,
        "--dry-run",
    ]);
    assert_ne!(
        output.code, 0,
        "expected non-zero exit for relative --live-home"
    );
    let stderr = output.stderr_text();
    assert!(
        stderr.contains("--live-home"),
        "stderr should name the offending flag: {stderr}"
    );
    assert!(
        stderr.contains("absolute"),
        "stderr should mention the absolute-path requirement: {stderr}"
    );
}

#[test]
fn live_home_accepts_absolute_path_dry_run() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let live_home = tmp.path().join("sandbox");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&live_home).unwrap();

    let source_arg = source_root.to_string_lossy().into_owned();
    let live_arg = live_home.to_string_lossy().into_owned();
    let state_arg = state_home.to_string_lossy().into_owned();
    let output = run_cli(&[
        "install",
        "--source-root",
        &source_arg,
        "--product",
        "claude",
        "--live-home",
        &live_arg,
        "--state-home",
        &state_arg,
        "--dry-run",
    ]);
    assert_eq!(
        output.code,
        0,
        "expected zero exit for absolute --live-home; stderr={}",
        output.stderr_text()
    );
    // Dry-run must not have written into the sandbox.
    let entries: Vec<_> = fs::read_dir(&live_home).unwrap().collect();
    assert!(
        entries.is_empty(),
        "dry-run wrote into sandbox: {entries:?}"
    );
}

// ---------------------------------------------------------------------------
// --tag
// ---------------------------------------------------------------------------

#[test]
fn tag_writes_marker_into_backup_run_root_when_backup_happens() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    // Seed a regular file at one of the install destinations so the apply
    // executor produces a FileBackedUpThenSymlinked change.
    let pre_existing = home.join("plugins/reporting/skills/daily-brief/SKILL.md");
    fs::create_dir_all(pre_existing.parent().unwrap()).unwrap();
    fs::write(&pre_existing, b"user-edited content\n").unwrap();

    let options = InstallOptions {
        tag: Some("pre-bump".to_string()),
        ..InstallOptions::default()
    };
    let __outcome = install::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        Mode::Apply,
        fixed_time(),
        &options,
    )
    .unwrap();

    let backup_run_root = state_home
        .join("backups")
        .join("claude")
        .join(format!("{}", 1_700_000_000_u64));
    let marker = backup_run_root.join("tag-pre-bump");
    assert!(
        marker.exists(),
        "expected tag marker at {} after a backup-triggering apply",
        marker.display()
    );
}

#[test]
fn tag_writes_no_marker_when_no_backup_happens() {
    // Fresh home → no pre-existing files → no backups → no marker.
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    let options = InstallOptions {
        tag: Some("pre-bump".to_string()),
        ..InstallOptions::default()
    };
    install::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        Mode::Apply,
        fixed_time(),
        &options,
    )
    .unwrap();

    let backup_run_root = state_home
        .join("backups")
        .join("claude")
        .join(format!("{}", 1_700_000_000_u64));
    assert!(
        !backup_run_root.exists(),
        "backup run root must not be created on a backup-free apply"
    );
}

#[test]
fn tag_rejects_unsafe_characters() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let live_home = tmp.path().join("sandbox");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&live_home).unwrap();

    let source_arg = source_root.to_string_lossy().into_owned();
    let live_arg = live_home.to_string_lossy().into_owned();
    let state_arg = state_home.to_string_lossy().into_owned();
    let output = run_cli(&[
        "install",
        "--source-root",
        &source_arg,
        "--product",
        "claude",
        "--live-home",
        &live_arg,
        "--state-home",
        &state_arg,
        "--tag",
        "../escape",
        "--dry-run",
    ]);
    assert_ne!(output.code, 0, "expected non-zero exit for unsafe --tag");
    let stderr = output.stderr_text();
    assert!(
        stderr.contains("--tag"),
        "stderr should name the offending flag: {stderr}"
    );
    assert!(
        stderr.contains("trusted") || stderr.contains("allowed"),
        "stderr should explain the trust contract: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Overlay merge
// ---------------------------------------------------------------------------

fn write_overlay(source_root: &Path, body: &str) {
    let dir = source_root.join(".private");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("link-map.overrides.yaml"), body).unwrap();
}

#[test]
fn overlay_drops_disabled_entry_before_plan_generation() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    write_overlay(
        &source_root,
        "\
schema_version: 1
entries:
  - id: reporting.skills-tree
    enabled: false
",
    );
    let overlay_file = source_root.join(".private/link-map.overrides.yaml");
    assert!(
        overlay_file.exists(),
        "overlay file should be readable from source_root: {}",
        overlay_file.display()
    );
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    let __outcome = install::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        Mode::DryRun,
        fixed_time(),
        &InstallOptions::default(),
    )
    .unwrap();
    let plan = __outcome.plan;

    // Only the plugin-manifest entry survives — skills-tree was dropped.
    assert_eq!(
        plan.actions.len(),
        1,
        "expected only the plugin-manifest action: {:?}",
        plan.actions
    );
}

#[test]
fn overlay_replaces_existing_entry_destination() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    write_overlay(
        &source_root,
        "\
schema_version: 1
entries:
  - id: reporting.plugin-manifest
    enabled: true
    kind: plugin-manifest-copy
    source: targets/claude/plugins/reporting/.claude-plugin/plugin.json
    destination: plugins/overridden/.claude-plugin/plugin.json
",
    );
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    let __outcome = install::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        Mode::DryRun,
        fixed_time(),
        &InstallOptions::default(),
    )
    .unwrap();
    let plan = __outcome.plan;

    let dests: Vec<String> = plan
        .actions
        .iter()
        .map(|a| match a {
            agent_runtime_cli::install::plan::PlanAction::Symlink { dest, .. } => {
                dest.display().to_string()
            }
            agent_runtime_cli::install::plan::PlanAction::ManagedBlock { config_file, .. } => {
                config_file.display().to_string()
            }
        })
        .collect();
    assert!(
        dests.iter().any(|d| d.contains("plugins/overridden/")),
        "overlay destination should appear in plan: {dests:?}"
    );
}

#[test]
fn no_overlay_skips_private_file_even_when_present() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    write_overlay(
        &source_root,
        "\
schema_version: 1
entries:
  - id: reporting.skills-tree
    enabled: false
",
    );
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    let options = InstallOptions {
        overlay_enabled: false,
        ..InstallOptions::default()
    };
    let __outcome = install::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        Mode::DryRun,
        fixed_time(),
        &options,
    )
    .unwrap();
    let plan = __outcome.plan;

    // skills-tree drop is ignored → both initial entries flow through, so
    // the plan has >1 actions (manifest + every file under skills-tree).
    assert!(
        plan.actions.len() > 1,
        "expected overlay-disabled run to include the skills-tree entries: {:?}",
        plan.actions
    );
}

#[test]
fn overlay_path_redirects_to_explicit_file() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");

    // Put the overlay somewhere *other* than the conventional .private/
    // location so we can prove --overlay-path was honoured.
    let custom = tmp.path().join("alt-overlay.yaml");
    fs::write(
        &custom,
        "\
schema_version: 1
entries:
  - id: reporting.skills-tree
    enabled: false
",
    )
    .unwrap();
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    let options = InstallOptions {
        overlay_path: Some(custom.clone()),
        ..InstallOptions::default()
    };
    let __outcome = install::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        Mode::DryRun,
        fixed_time(),
        &options,
    )
    .unwrap();
    let plan = __outcome.plan;
    assert_eq!(
        plan.actions.len(),
        1,
        "custom overlay path should have dropped skills-tree: {:?}",
        plan.actions
    );
}

#[test]
fn overlay_consumption_is_announced_on_stderr() {
    // Architecture-doc requirement: `agent-runtime install --dry-run` must
    // expose the post-overlay-merge effective config to reviewers. Pin the
    // one-line summary naming dropped/replaced/added counts whenever an
    // overlay is consumed.
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    write_overlay(
        &source_root,
        "\
schema_version: 1
entries:
  - id: reporting.skills-tree
    enabled: false
",
    );
    let live_home = tmp.path().join("sandbox");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&live_home).unwrap();

    let source_arg = source_root.to_string_lossy().into_owned();
    let live_arg = live_home.to_string_lossy().into_owned();
    let state_arg = state_home.to_string_lossy().into_owned();
    let output = run_cli(&[
        "install",
        "--source-root",
        &source_arg,
        "--product",
        "claude",
        "--live-home",
        &live_arg,
        "--state-home",
        &state_arg,
        "--dry-run",
    ]);
    assert_eq!(output.code, 0, "install dry-run should exit 0");
    let stderr = output.stderr_text();
    assert!(
        stderr.contains("overlay merged"),
        "stderr should announce overlay consumption: {stderr}"
    );
    assert!(
        stderr.contains("dropped=1"),
        "stderr should name the drop count: {stderr}"
    );
}

#[test]
fn overlay_stays_silent_when_no_overlay_file_present() {
    // Mirror of the above — when no overlay is loaded, the CLI must NOT
    // print the overlay-merged line; otherwise the operator cannot tell
    // at a glance whether an overlay was in play.
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let live_home = tmp.path().join("sandbox");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&live_home).unwrap();

    let source_arg = source_root.to_string_lossy().into_owned();
    let live_arg = live_home.to_string_lossy().into_owned();
    let state_arg = state_home.to_string_lossy().into_owned();
    let output = run_cli(&[
        "install",
        "--source-root",
        &source_arg,
        "--product",
        "claude",
        "--live-home",
        &live_arg,
        "--state-home",
        &state_arg,
        "--dry-run",
    ]);
    assert_eq!(output.code, 0);
    let stderr = output.stderr_text();
    assert!(
        !stderr.contains("overlay merged"),
        "stderr must NOT announce overlay merge when no overlay file is present: {stderr}"
    );
}

#[test]
fn overlay_schema_mismatch_aborts_install() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    write_overlay(
        &source_root,
        "\
schema_version: 99
entries: []
",
    );
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    let err = install::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        Mode::DryRun,
        fixed_time(),
        &InstallOptions::default(),
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("schema_version"),
        "expected schema_version error, got: {msg}"
    );
}

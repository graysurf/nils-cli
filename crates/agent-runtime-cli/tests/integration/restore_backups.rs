//! Restore-backups pipeline integration tests. Plan 04 Sprint 2 Task 2.2.
//!
//! The load-bearing assertions are:
//!
//! - Write → install → restore round-trip puts the operator's original
//!   pre-install file content back at the install destination.
//! - `--from latest` resolves to the highest-numbered unix-seconds run.
//! - `--from <unknown-ts>` returns `NoBackupRun` with the available list.
//! - `tag-*` marker files at the run root are not treated as restore
//!   sources.
//! - A regular file at the destination is not overwritten (operator
//!   already restored or wrote a replacement).
//! - `--surface <entry_id>` restricts restore to that entry id only.
//! - Recursive link-map entries that collide on `(entry_id, basename)`
//!   produce `SkippedAmbiguous`.

use agent_runtime_cli::install::{self, InstallOptions, Mode as InstallMode};
use agent_runtime_cli::restore_backups::{
    self, BackupRunSelector, Mode as RestoreMode, RestoreError, RestoreOptions, RestoredChange,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

fn ts(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

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
        r#"{"name":"reporting","version":"0.1.0-from-source"}"#,
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
    fs::create_dir_all(build_dir.join("topic-radar")).unwrap();
    fs::write(
        build_dir.join("topic-radar").join("SKILL.md"),
        "# topic-radar\n",
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

fn seed_pre_install_manifest(home: &Path, content: &str) -> PathBuf {
    let dest = home
        .join("plugins")
        .join("reporting")
        .join(".claude-plugin")
        .join("plugin.json");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, content).unwrap();
    dest
}

fn seed_pre_install_skills(home: &Path) {
    let skills_root = home.join("plugins").join("reporting").join("skills");
    for (sub, content) in [
        ("daily-brief/SKILL.md", "ORIG-DAILY"),
        ("topic-radar/SKILL.md", "ORIG-RADAR"),
    ] {
        let dest = skills_root.join(sub);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, content).unwrap();
    }
}

fn run_install(source_root: &Path, home: &Path, state_home: &Path, at: SystemTime) {
    let outcome = install::run(
        "claude",
        source_root,
        home,
        state_home,
        InstallMode::Apply,
        at,
        &InstallOptions::default(),
    )
    .unwrap();
    assert!(!outcome.changes.is_empty(), "install produced no changes");
}

fn run_install_tagged(
    source_root: &Path,
    home: &Path,
    state_home: &Path,
    at: SystemTime,
    tag: &str,
) {
    let options = InstallOptions {
        tag: Some(tag.to_string()),
        ..InstallOptions::default()
    };
    install::run(
        "claude",
        source_root,
        home,
        state_home,
        InstallMode::Apply,
        at,
        &options,
    )
    .unwrap();
}

#[test]
fn roundtrip_install_then_restore_returns_original_content() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    let manifest_dest = seed_pre_install_manifest(&home, "ORIG-MANIFEST");
    run_install(&source_root, &home, &state_home, ts(1_700_000_000));

    // After install, dest is a symlink pointing into the source root.
    let meta = fs::symlink_metadata(&manifest_dest).unwrap();
    assert!(meta.file_type().is_symlink());
    assert_ne!(fs::read_to_string(&manifest_dest).unwrap(), "ORIG-MANIFEST");

    // The backup file landed under <state>/backups/claude/<ts>/reporting.plugin-manifest/plugin.json.
    let backup_file =
        state_home.join("backups/claude/1700000000/reporting.plugin-manifest/plugin.json");
    assert!(backup_file.exists(), "install did not produce backup file");
    assert_eq!(fs::read_to_string(&backup_file).unwrap(), "ORIG-MANIFEST");

    let outcome = restore_backups::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        RestoreMode::Apply,
        &RestoreOptions::default(),
    )
    .unwrap();
    assert!(
        outcome
            .changes
            .iter()
            .any(|c| matches!(c, RestoredChange::FileRestored { .. })),
        "no FileRestored in {:#?}",
        outcome.changes
    );

    // Dest is now a regular file with the original content.
    let meta_after = fs::symlink_metadata(&manifest_dest).unwrap();
    assert!(
        meta_after.file_type().is_file() && !meta_after.file_type().is_symlink(),
        "restore did not replace symlink with regular file"
    );
    assert_eq!(fs::read_to_string(&manifest_dest).unwrap(), "ORIG-MANIFEST");
    // The backup file has been moved out of the backup tree.
    assert!(
        !backup_file.exists(),
        "restore did not consume the backup file"
    );
}

#[test]
fn restore_dry_run_does_not_mutate_home_or_backup_tree() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    let manifest_dest = seed_pre_install_manifest(&home, "ORIG-MANIFEST");
    run_install(&source_root, &home, &state_home, ts(1_700_000_000));

    let manifest_target_before = fs::read_link(&manifest_dest).unwrap();
    let backup_file =
        state_home.join("backups/claude/1700000000/reporting.plugin-manifest/plugin.json");
    assert!(backup_file.exists());

    let outcome = restore_backups::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        RestoreMode::DryRun,
        &RestoreOptions::default(),
    )
    .unwrap();
    // Dry-run still classifies one FileRestored — same convention as install/uninstall.
    assert!(
        outcome
            .changes
            .iter()
            .any(|c| matches!(c, RestoredChange::FileRestored { .. })),
        "dry-run failed to classify restore: {:#?}",
        outcome.changes
    );

    // Home and backup tree untouched.
    let meta = fs::symlink_metadata(&manifest_dest).unwrap();
    assert!(meta.file_type().is_symlink());
    assert_eq!(
        fs::read_link(&manifest_dest).unwrap(),
        manifest_target_before
    );
    assert!(backup_file.exists());
    assert_eq!(fs::read_to_string(&backup_file).unwrap(), "ORIG-MANIFEST");
}

#[test]
fn restore_with_no_backups_returns_no_backup_run_with_empty_list() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&state_home).unwrap();

    let err = restore_backups::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        RestoreMode::Apply,
        &RestoreOptions::default(),
    )
    .unwrap_err();

    match err {
        RestoreError::NoBackupRun {
            available,
            selector,
            ..
        } => {
            assert!(
                available.is_empty(),
                "expected empty available list, got {available:?}"
            );
            assert_eq!(selector, BackupRunSelector::Latest);
        }
        other => panic!("expected NoBackupRun, got {other:?}"),
    }
}

#[test]
fn restore_from_exact_unknown_timestamp_lists_available_runs() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    seed_pre_install_manifest(&home, "ORIG-MANIFEST");
    run_install(&source_root, &home, &state_home, ts(1_700_000_000));

    let options = RestoreOptions {
        selector: BackupRunSelector::Exact(9_999_999_999),
        ..RestoreOptions::default()
    };
    let err = restore_backups::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        RestoreMode::Apply,
        &options,
    )
    .unwrap_err();

    match err {
        RestoreError::NoBackupRun { available, .. } => {
            assert_eq!(available, vec![1_700_000_000_u64]);
        }
        other => panic!("expected NoBackupRun, got {other:?}"),
    }
}

#[test]
fn restore_from_latest_picks_newest_run_across_multiple_installs() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    // First install at ts=1700000000 with original content A.
    seed_pre_install_manifest(&home, "ORIG-A");
    run_install(&source_root, &home, &state_home, ts(1_700_000_000));

    // Operator manually replaces the symlink with a regular file
    // containing content B, then re-installs at a later ts. The second
    // install backs up content B into the newer run dir.
    let manifest_dest = home.join("plugins/reporting/.claude-plugin/plugin.json");
    fs::remove_file(&manifest_dest).unwrap();
    fs::write(&manifest_dest, "ORIG-B").unwrap();
    run_install(&source_root, &home, &state_home, ts(1_700_500_000));

    let timestamps = restore_backups::list_available_timestamps(&state_home, "claude");
    assert_eq!(timestamps, vec![1_700_000_000, 1_700_500_000]);

    // Latest picks the second run, restoring content B.
    let outcome = restore_backups::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        RestoreMode::Apply,
        &RestoreOptions::default(),
    )
    .unwrap();
    assert!(
        outcome
            .changes
            .iter()
            .any(|c| matches!(c, RestoredChange::FileRestored { .. })),
        "no FileRestored: {:#?}",
        outcome.changes
    );
    assert_eq!(fs::read_to_string(&manifest_dest).unwrap(), "ORIG-B");
}

#[test]
fn restore_skips_tag_marker_files_at_run_root() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    seed_pre_install_manifest(&home, "ORIG-MANIFEST");
    run_install_tagged(
        &source_root,
        &home,
        &state_home,
        ts(1_700_000_000),
        "pre-bump",
    );

    // Sanity: tag marker landed.
    let tag_marker = state_home.join("backups/claude/1700000000/tag-pre-bump");
    assert!(tag_marker.exists(), "tag marker missing");

    let outcome = restore_backups::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        RestoreMode::Apply,
        &RestoreOptions::default(),
    )
    .unwrap();

    // Exactly one FileRestored (the manifest); no action references the tag marker.
    let restored: Vec<_> = outcome
        .changes
        .iter()
        .filter(|c| matches!(c, RestoredChange::FileRestored { .. }))
        .collect();
    assert_eq!(restored.len(), 1, "expected 1 restored, got {restored:?}");
    for c in &outcome.changes {
        if let RestoredChange::FileRestored { from_backup, .. } = c {
            assert!(
                !from_backup
                    .file_name()
                    .map(|n| n.to_string_lossy().starts_with("tag-"))
                    .unwrap_or(false),
                "restore tried to restore a tag-* marker: {from_backup:?}",
            );
        }
    }
    // Tag marker still present after restore (gc-backups, not restore, owns its lifetime).
    assert!(tag_marker.exists(), "restore consumed the tag marker");
}

#[test]
fn restore_skips_regular_file_at_dest_preserving_operator_content() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    let manifest_dest = seed_pre_install_manifest(&home, "ORIG-MANIFEST");
    run_install(&source_root, &home, &state_home, ts(1_700_000_000));

    // Operator manually overwrote the symlink with a custom file before running restore.
    fs::remove_file(&manifest_dest).unwrap();
    fs::write(&manifest_dest, "OPERATOR-CUSTOM").unwrap();

    let outcome = restore_backups::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        RestoreMode::Apply,
        &RestoreOptions::default(),
    )
    .unwrap();

    // Skipped because of regular file at dest.
    assert!(
        outcome
            .changes
            .iter()
            .any(|c| matches!(c, RestoredChange::SkippedDestRegularFile { .. })),
        "expected SkippedDestRegularFile in {:#?}",
        outcome.changes,
    );
    // No FileRestored emitted for the manifest entry.
    assert!(
        !outcome.changes.iter().any(|c| matches!(
            c,
            RestoredChange::FileRestored { dest, .. } if dest == &manifest_dest
        )),
        "restore overwrote operator content: {:#?}",
        outcome.changes,
    );
    assert_eq!(
        fs::read_to_string(&manifest_dest).unwrap(),
        "OPERATOR-CUSTOM"
    );
    // Backup file still present so the operator can recover manually later.
    let backup_file =
        state_home.join("backups/claude/1700000000/reporting.plugin-manifest/plugin.json");
    assert!(backup_file.exists());
}

#[test]
fn restore_surface_filter_restricts_to_named_entry_id() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    seed_pre_install_manifest(&home, "ORIG-MANIFEST");
    seed_pre_install_skills(&home);
    run_install(&source_root, &home, &state_home, ts(1_700_000_000));

    let options = RestoreOptions {
        surface: Some("reporting.plugin-manifest".to_string()),
        ..RestoreOptions::default()
    };
    let outcome = restore_backups::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        RestoreMode::Apply,
        &options,
    )
    .unwrap();

    // Only one action ran (the manifest); skills tree entries were filtered out.
    assert_eq!(
        outcome.changes.len(),
        1,
        "expected exactly one restore action under --surface filter, got {:#?}",
        outcome.changes,
    );
    assert!(matches!(
        outcome.changes[0],
        RestoredChange::FileRestored { .. }
    ));

    // Manifest restored.
    let manifest_dest = home.join("plugins/reporting/.claude-plugin/plugin.json");
    let meta = fs::symlink_metadata(&manifest_dest).unwrap();
    assert!(meta.file_type().is_file() && !meta.file_type().is_symlink());

    // Skills tree symlinks still in place (filter excluded them).
    let skill_dest = home.join("plugins/reporting/skills/daily-brief/SKILL.md");
    let skill_meta = fs::symlink_metadata(&skill_dest).unwrap();
    assert!(
        skill_meta.file_type().is_symlink(),
        "surface filter did not protect skills tree from restore",
    );
}

#[test]
fn restore_emits_skipped_ambiguous_for_recursive_basename_collision() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    seed_pre_install_manifest(&home, "ORIG-MANIFEST");
    // Both daily-brief/SKILL.md and topic-radar/SKILL.md will land in
    // <state>/backups/claude/<ts>/reporting.skills-tree/SKILL.md — the
    // second clobbers the first because install's move_to_backup uses
    // dest.file_name() only. Restore sees one backup file with two
    // candidate install destinations and emits SkippedAmbiguous.
    seed_pre_install_skills(&home);
    run_install(&source_root, &home, &state_home, ts(1_700_000_000));

    let outcome = restore_backups::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        RestoreMode::Apply,
        &RestoreOptions::default(),
    )
    .unwrap();

    let ambiguous: Vec<_> = outcome
        .changes
        .iter()
        .filter(|c| matches!(c, RestoredChange::SkippedAmbiguous { .. }))
        .collect();
    assert_eq!(
        ambiguous.len(),
        1,
        "expected one ambiguous skip from skills tree collision: {:#?}",
        outcome.changes,
    );
    if let RestoredChange::SkippedAmbiguous {
        entry_id,
        candidates,
        ..
    } = &ambiguous[0]
    {
        assert_eq!(entry_id, "reporting.skills-tree");
        assert_eq!(candidates.len(), 2, "expected 2 candidates: {candidates:?}");
    }
}

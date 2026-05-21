//! Uninstall pipeline integration tests. Plan 04 Sprint 2 Task 2.1.
//!
//! Each test composes the full pipeline: install the link map against a
//! sandbox home, then drive `uninstall::run` end-to-end. The load-bearing
//! assertions are:
//!
//! - After install + uninstall, every symlink owned by the link map is
//!   gone and every managed-block surface no longer carries markers.
//! - Backup directories under `<state_home>/backups/` are not touched
//!   by uninstall.
//! - Auth / history / sessions / cache / projects trees under the
//!   sandbox home survive uninstall byte-identically.
//! - A second uninstall on an already-clean home is an exit-0 no-op.

use agent_runtime_cli::install::{self, InstallOptions, Mode as InstallMode};
use agent_runtime_cli::uninstall::{
    self, Mode as UninstallMode, UninstallOptions, UninstalledChange,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

fn fixed_time() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// Build a link-map that exercises every branch the uninstall executor
/// has to reverse: a non-recursive symlinked-file (plugin manifest), a
/// recursive directory expansion (skills tree), and a managed block on a
/// JSON config surface.
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
    fs::create_dir_all(build_dir.join("topic-radar").join("scripts")).unwrap();
    fs::write(
        build_dir.join("topic-radar").join("SKILL.md"),
        "# topic-radar\n",
    )
    .unwrap();
    fs::write(
        build_dir
            .join("topic-radar")
            .join("scripts")
            .join("topic-radar.sh"),
        "#!/bin/sh\necho radar\n",
    )
    .unwrap();

    let link_map = format!(
        "schema_version: 1\nentries:\n  - id: reporting.plugin-manifest\n    kind: plugin-manifest-copy\n    source: targets/{product}/plugins/reporting/.{product}-plugin/plugin.json\n    destination: plugins/reporting/.{product}-plugin/plugin.json\n  - id: reporting.skills-tree\n    kind: symlinked-file\n    source: build/{product}/plugins/reporting/skills\n    destination: plugins/reporting/skills\n    recursive: true\n  - id: {product}.config\n    kind: managed-block\n    destination: settings.json\n    surface: install\n    comment_style: double-slash\n    body_template: |-\n      \"agent-runtime\": true\n",
    );
    fs::write(
        root.join("targets").join(product).join("link-map.yaml"),
        link_map,
    )
    .unwrap();

    fs::canonicalize(&root).unwrap()
}

/// Seed the runtime home with sibling state that uninstall must not
/// touch: auth / history / sessions / cache / projects. Each tree is
/// populated with one file so the assertion has bytes to compare.
fn seed_user_state(home: &Path) {
    for top in &["auth", "history", "sessions", "cache", "projects"] {
        let dir = home.join(top);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("user.bin"), format!("user-owned {top}").as_bytes()).unwrap();
    }
}

/// Compare every regular file under `dir` byte-for-byte against the
/// snapshot returned by an earlier call. Order-independent.
fn snapshot_files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(read) = fs::read_dir(dir) else { return };
        for entry in read.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_dir() {
                walk(root, &path, out);
            } else if meta.file_type().is_file() {
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                out.push((rel, fs::read(&path).unwrap()));
            }
        }
    }
    walk(dir, dir, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn run_install(source_root: &Path, home: &Path, state_home: &Path) {
    let outcome = install::run(
        "claude",
        source_root,
        home,
        state_home,
        InstallMode::Apply,
        fixed_time(),
        &InstallOptions::default(),
    )
    .unwrap();
    assert!(!outcome.changes.is_empty(), "install produced no changes");
}

#[test]
fn install_then_uninstall_apply_removes_every_link_map_artifact() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    run_install(&source_root, &home, &state_home);

    // Sanity-check: install actually placed link-map artifacts.
    let manifest_dest = home.join("plugins/reporting/.claude-plugin/plugin.json");
    assert!(fs::symlink_metadata(&manifest_dest).is_ok());
    let skill_dest = home.join("plugins/reporting/skills/daily-brief/SKILL.md");
    assert!(fs::symlink_metadata(&skill_dest).is_ok());
    let settings_after_install = fs::read_to_string(home.join("settings.json")).unwrap();
    assert!(settings_after_install.contains("// >>> agent-runtime-kit:install >>>"));

    // Drive uninstall apply.
    let outcome = uninstall::run(
        "claude",
        &source_root,
        &home,
        UninstallMode::Apply,
        &UninstallOptions::default(),
    )
    .unwrap();
    assert!(!outcome.changes.is_empty(), "uninstall reported no changes");
    let removed = outcome
        .changes
        .iter()
        .filter(|c| {
            matches!(
                c,
                UninstalledChange::SymlinkRemoved { .. }
                    | UninstalledChange::ManagedBlockRemoved { .. }
            )
        })
        .count();
    assert!(
        removed > 0,
        "uninstall must report at least one removal: {:#?}",
        outcome.changes
    );

    // Every link-map symlink is gone.
    assert!(
        fs::symlink_metadata(&manifest_dest).is_err(),
        "plugin manifest symlink survived uninstall: {}",
        manifest_dest.display()
    );
    assert!(
        fs::symlink_metadata(&skill_dest).is_err(),
        "skill symlink survived uninstall: {}",
        skill_dest.display()
    );
    // Managed block has been stripped from settings.json — bytes outside
    // the markers are preserved, so the file may still exist but the
    // marker pair must be gone.
    let settings_after_uninstall =
        fs::read_to_string(home.join("settings.json")).unwrap_or_default();
    assert!(
        !settings_after_uninstall.contains("agent-runtime-kit:install"),
        "managed block markers survived uninstall: {settings_after_uninstall}"
    );
}

#[test]
fn second_uninstall_on_clean_home_is_idempotent_noop() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    run_install(&source_root, &home, &state_home);
    let _first = uninstall::run(
        "claude",
        &source_root,
        &home,
        UninstallMode::Apply,
        &UninstallOptions::default(),
    )
    .unwrap();

    let snapshot_before_second = snapshot_files(&home);

    let outcome = uninstall::run(
        "claude",
        &source_root,
        &home,
        UninstallMode::Apply,
        &UninstallOptions::default(),
    )
    .unwrap();

    // Every change reported by the second run is a NoOp.
    for c in &outcome.changes {
        assert!(
            matches!(c, UninstalledChange::NoOp { .. }),
            "second uninstall produced non-NoOp change: {c:?}"
        );
    }
    // Filesystem is byte-identical across the second uninstall.
    assert_eq!(snapshot_files(&home), snapshot_before_second);
}

#[test]
fn uninstall_against_a_home_with_no_install_is_clean_noop() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let before = snapshot_files(&home);

    let outcome = uninstall::run(
        "claude",
        &source_root,
        &home,
        UninstallMode::Apply,
        &UninstallOptions::default(),
    )
    .unwrap();

    for c in &outcome.changes {
        assert!(
            matches!(c, UninstalledChange::NoOp { .. }),
            "uninstall on a never-installed home produced non-NoOp: {c:?}"
        );
    }
    assert_eq!(snapshot_files(&home), before);
}

#[test]
fn dry_run_does_not_mutate_home() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    run_install(&source_root, &home, &state_home);
    let before = snapshot_files(&home);

    let outcome = uninstall::run(
        "claude",
        &source_root,
        &home,
        UninstallMode::DryRun,
        &UninstallOptions::default(),
    )
    .unwrap();

    // Dry-run still classifies changes (so the printer has something to
    // emit) but does not touch any file.
    assert!(!outcome.changes.is_empty());
    assert_eq!(
        snapshot_files(&home),
        before,
        "home was mutated by uninstall dry-run"
    );
}

#[test]
fn uninstall_does_not_touch_auth_history_sessions_cache_or_projects() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    run_install(&source_root, &home, &state_home);
    // Seed the sibling state AFTER install so we are asserting on the
    // post-install layout, not a fresh empty home.
    seed_user_state(&home);

    let mut expected: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for top in &["auth", "history", "sessions", "cache", "projects"] {
        let top_dir = home.join(top);
        for (rel, bytes) in snapshot_files(&top_dir) {
            expected.push((PathBuf::from(top).join(rel), bytes));
        }
    }

    let _outcome = uninstall::run(
        "claude",
        &source_root,
        &home,
        UninstallMode::Apply,
        &UninstallOptions::default(),
    )
    .unwrap();

    let mut after: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for top in &["auth", "history", "sessions", "cache", "projects"] {
        let top_dir = home.join(top);
        for (rel, bytes) in snapshot_files(&top_dir) {
            after.push((PathBuf::from(top).join(rel), bytes));
        }
    }
    assert_eq!(
        expected, after,
        "uninstall mutated reserved user-owned trees (auth/history/sessions/cache/projects)"
    );
}

#[test]
fn backups_directory_survives_uninstall() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    // Seed a regular file at one of the install destinations so install
    // produces a backup under <state_home>/backups/claude/...
    let pre_existing = home.join("plugins/reporting/skills/topic-radar/SKILL.md");
    fs::create_dir_all(pre_existing.parent().unwrap()).unwrap();
    fs::write(&pre_existing, b"user-edited content\n").unwrap();

    run_install(&source_root, &home, &state_home);

    let backups_root = state_home.join("backups").join("claude");
    assert!(backups_root.exists(), "install did not create backups dir");
    let backups_snapshot = snapshot_files(&backups_root);
    assert!(
        !backups_snapshot.is_empty(),
        "install backed up no files: {}",
        backups_root.display()
    );

    let _outcome = uninstall::run(
        "claude",
        &source_root,
        &home,
        UninstallMode::Apply,
        &UninstallOptions::default(),
    )
    .unwrap();

    // Backups directory still exists with identical bytes.
    assert!(backups_root.exists(), "uninstall removed the backups dir");
    assert_eq!(
        snapshot_files(&backups_root),
        backups_snapshot,
        "uninstall mutated bytes under {}",
        backups_root.display()
    );
}

#[test]
fn foreign_symlink_at_install_dest_is_skipped() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    run_install(&source_root, &home, &state_home);

    // Operator manually repoints the plugin manifest at a foreign file
    // between install and uninstall.
    let foreign = tmp.path().join("operator-managed-file.json");
    fs::write(&foreign, b"{}").unwrap();
    let manifest_dest = home.join("plugins/reporting/.claude-plugin/plugin.json");
    fs::remove_file(&manifest_dest).unwrap();
    std::os::unix::fs::symlink(&foreign, &manifest_dest).unwrap();

    let outcome = uninstall::run(
        "claude",
        &source_root,
        &home,
        UninstallMode::Apply,
        &UninstallOptions::default(),
    )
    .unwrap();

    // The foreign symlink should be reported as skipped, not removed.
    let foreign_skips = outcome
        .changes
        .iter()
        .filter(|c| matches!(c, UninstalledChange::SymlinkSkippedForeign { .. }))
        .count();
    assert!(
        foreign_skips >= 1,
        "expected at least one SymlinkSkippedForeign, got: {:#?}",
        outcome.changes
    );
    // And the foreign symlink survives.
    assert!(
        fs::symlink_metadata(&manifest_dest)
            .unwrap()
            .file_type()
            .is_symlink(),
        "operator's foreign symlink was removed by uninstall"
    );
}

#[test]
fn missing_link_map_returns_typed_error() {
    let tmp = TempDir::new().unwrap();
    let source_root = tmp.path().join("src");
    fs::create_dir_all(&source_root).unwrap();
    let home = tmp.path().join("home");
    let err = uninstall::run(
        "claude",
        &source_root,
        &home,
        UninstallMode::Apply,
        &UninstallOptions::default(),
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("link-map"), "expected link-map error: {msg}");
}

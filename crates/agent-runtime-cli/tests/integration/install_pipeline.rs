//! Install pipeline integration tests. Plan 04 Sprint 1 Task 1.2.
//!
//! These tests build a deterministic tmp source root + tmp home and
//! drive `agent_runtime_cli::install::run` end-to-end so the dry-run
//! printer and apply executor both exercise the byte-identical
//! idempotence guarantee the Plan 04 acceptance criteria require.

use agent_runtime_cli::install::{self, AppliedChange, Mode};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

/// Build a minimal source root with one product's link-map pointing at
/// a plugin manifest (`targets/`) and a recursive skills tree (`build/`).
fn build_source_root(tmp: &Path, product: &str) -> PathBuf {
    let root = tmp.join("src");
    fs::create_dir_all(&root).unwrap();

    // Plugin manifest under targets/.
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

    // Rendered skills tree under build/.
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

    // link-map.yaml that mirrors the production initial shape.
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

fn fixed_time() -> SystemTime {
    // Tests use a fixed timestamp so backup-dir paths are stable.
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// Build a sorted snapshot of `dir`: every relative path under the
/// directory plus, for each path, either the symlink target or the
/// file contents. The snapshot is the canonical input to byte-identical
/// idempotence assertions.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Snapshot)> {
    let mut out = Vec::new();
    collect(dir, dir, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[derive(Debug, PartialEq, Eq)]
enum Snapshot {
    Symlink(PathBuf),
    File(Vec<u8>),
    Dir,
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, Snapshot)>) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).unwrap();
        let rel = path.strip_prefix(root).unwrap().to_path_buf();
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&path).unwrap();
            out.push((rel, Snapshot::Symlink(target)));
        } else if meta.file_type().is_dir() {
            out.push((rel, Snapshot::Dir));
            collect(root, &path, out);
        } else if meta.file_type().is_file() {
            let bytes = fs::read(&path).unwrap();
            out.push((rel, Snapshot::File(bytes)));
        }
    }
}

#[test]
fn dry_run_does_not_mutate_home() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");
    fs::create_dir_all(&home).unwrap();

    let before = snapshot(&home);
    let (plan, changes) = install::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        Mode::DryRun,
        fixed_time(),
    )
    .unwrap();

    // Dry-run must not touch the filesystem.
    assert_eq!(snapshot(&home), before, "home was mutated by dry-run");
    assert!(!state_home.exists(), "state_home was created by dry-run");

    // Every action is reported.
    assert_eq!(plan.actions.len(), changes.len());
    // No NoOps on first dry-run (fresh home).
    for c in &changes {
        assert!(
            !matches!(c, AppliedChange::NoOp { .. }),
            "unexpected NoOp on first dry-run: {c:?}"
        );
    }
}

#[test]
fn apply_writes_symlinks_and_is_byte_identical_on_second_run() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    let (_, changes_first) = install::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        Mode::Apply,
        fixed_time(),
    )
    .unwrap();
    // First apply: every action is a SymlinkCreated (fresh home).
    for c in &changes_first {
        assert!(
            matches!(c, AppliedChange::SymlinkCreated { .. }),
            "first apply expected only SymlinkCreated, got {c:?}"
        );
    }

    let snapshot_after_first = snapshot(&home);

    // Sanity-check that the destinations exist as symlinks and resolve.
    let manifest_dest = home.join("plugins/reporting/.claude-plugin/plugin.json");
    assert!(
        fs::symlink_metadata(&manifest_dest)
            .unwrap()
            .file_type()
            .is_symlink(),
        "manifest dest is not a symlink"
    );
    let skill_dest = home.join("plugins/reporting/skills/daily-brief/SKILL.md");
    assert_eq!(fs::read_to_string(&skill_dest).unwrap(), "# daily-brief\n",);

    // Second apply: byte-identical no-op.
    let (_, changes_second) = install::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        Mode::Apply,
        fixed_time(),
    )
    .unwrap();
    for c in &changes_second {
        assert!(
            matches!(c, AppliedChange::NoOp { .. }),
            "second apply expected only NoOp, got {c:?}"
        );
    }
    assert_eq!(
        snapshot(&home),
        snapshot_after_first,
        "second apply mutated the home tree",
    );
}

#[test]
fn pre_existing_regular_file_is_backed_up_then_symlinked() {
    let tmp = TempDir::new().unwrap();
    let source_root = build_source_root(tmp.path(), "claude");
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    // Seed a regular file at one of the install destinations.
    let pre_existing = home.join("plugins/reporting/skills/topic-radar/SKILL.md");
    fs::create_dir_all(pre_existing.parent().unwrap()).unwrap();
    fs::write(&pre_existing, b"user-edited content\n").unwrap();

    let (_, changes) = install::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        Mode::Apply,
        fixed_time(),
    )
    .unwrap();

    let backed_up = changes
        .iter()
        .filter(|c| matches!(c, AppliedChange::FileBackedUpThenSymlinked { .. }))
        .count();
    assert_eq!(
        backed_up, 1,
        "exactly one entry should have triggered a backup: {changes:#?}",
    );
    // The original bytes survive under <state_home>/backups/...
    assert!(state_home.exists(), "state_home was not created");
    let backup_root = state_home.join("backups").join("claude");
    let mut found_backup = false;
    for entry in walkdir_files(&backup_root) {
        if fs::read(&entry).ok() == Some(b"user-edited content\n".to_vec()) {
            found_backup = true;
            break;
        }
    }
    assert!(
        found_backup,
        "backed-up bytes not found under {}",
        backup_root.display()
    );
}

fn walkdir_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(read) = fs::read_dir(dir) else { return };
        for entry in read.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_dir() {
                walk(&path, out);
            } else if meta.file_type().is_file() {
                out.push(path);
            }
        }
    }
    walk(dir, &mut out);
    out
}

#[test]
fn managed_block_entry_writes_block_and_is_idempotent_on_second_apply() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("src");
    fs::create_dir_all(root.join("targets").join("codex")).unwrap();
    let link_map = "\
schema_version: 1
entries:
  - id: codex.config
    kind: managed-block
    destination: config.toml
    surface: install
    comment_style: hash
    body_template: |-
      tag = \"agent-runtime\"
      live_home = \"/tmp/sandbox\"
";
    fs::write(
        root.join("targets").join("codex").join("link-map.yaml"),
        link_map,
    )
    .unwrap();
    let source_root = fs::canonicalize(&root).unwrap();

    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");

    let (_, changes_first) = install::run(
        "codex",
        &source_root,
        &home,
        &state_home,
        Mode::Apply,
        fixed_time(),
    )
    .unwrap();
    assert_eq!(changes_first.len(), 1);
    assert!(matches!(
        changes_first[0],
        AppliedChange::ManagedBlockApplied { .. }
    ));

    let config_after_first = fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(config_after_first.contains("# >>> agent-runtime-kit:install >>>"));
    assert!(config_after_first.contains("tag = \"agent-runtime\""));
    assert!(config_after_first.contains("# <<< agent-runtime-kit:install <<<"));

    let (_, changes_second) = install::run(
        "codex",
        &source_root,
        &home,
        &state_home,
        Mode::Apply,
        fixed_time(),
    )
    .unwrap();
    assert!(matches!(changes_second[0], AppliedChange::NoOp { .. }));
    let config_after_second = fs::read_to_string(home.join("config.toml")).unwrap();
    assert_eq!(config_after_first, config_after_second);
}

#[test]
fn missing_link_map_returns_typed_error() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let state_home = tmp.path().join("state");
    let source_root = tmp.path().join("src");
    fs::create_dir_all(&source_root).unwrap();
    let err = install::run(
        "claude",
        &source_root,
        &home,
        &state_home,
        Mode::DryRun,
        fixed_time(),
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("link-map"),
        "expected link-map error, got: {msg}"
    );
}

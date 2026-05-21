//! `agent-runtime gc-backups` integration tests. Plan 04 Sprint 2 Task 2.4.
//!
//! Drive the real binary against a seeded backup tree so the CLI flag
//! shape, retention selection, `--tag`-marker preservation, and per-mode
//! mutation contract stay pinned end-to-end.

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn agent_runtime_bin() -> PathBuf {
    bin::resolve("agent-runtime")
}

fn run_gc(args: &[&str]) -> CmdOutput {
    cmd::run(&agent_runtime_bin(), args, &[], None)
}

fn seed_run(state: &Path, product: &str, ts: u64, entry_id: &str) -> PathBuf {
    let entry = state
        .join("backups")
        .join(product)
        .join(ts.to_string())
        .join(entry_id);
    fs::create_dir_all(&entry).unwrap();
    fs::write(entry.join("plugin.json"), format!("BACKUP-{product}-{ts}")).unwrap();
    state.join("backups").join(product).join(ts.to_string())
}

fn seed_tag(state: &Path, product: &str, ts: u64, name: &str) {
    let path = state
        .join("backups")
        .join(product)
        .join(ts.to_string())
        .join(format!("tag-{name}"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"").unwrap();
}

#[test]
fn seven_runs_with_default_retention_five_keeps_five_in_apply() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path();
    for ts in [100u64, 200, 300, 400, 500, 600, 700] {
        seed_run(state_path, "claude", ts, "reporting");
    }
    let state = state_path.to_string_lossy().to_string();

    let out = run_gc(&[
        "gc-backups",
        "--state-home",
        &state,
        "--product",
        "claude",
        "--apply",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    // Newest 5 stay on disk; oldest 2 deleted.
    for keep in [300u64, 400, 500, 600, 700] {
        assert!(
            state_path
                .join("backups/claude")
                .join(keep.to_string())
                .is_dir(),
            "retained ts={keep}"
        );
    }
    for gone in [100u64, 200] {
        assert!(
            !state_path
                .join("backups/claude")
                .join(gone.to_string())
                .exists(),
            "deleted ts={gone}"
        );
    }

    let stderr = out.stderr_text();
    assert!(
        stderr.contains("retained=5") && stderr.contains("deleted=2"),
        "summary line missing counts: {stderr}"
    );
}

#[test]
fn install_tag_marker_directory_is_preserved_outside_retention_window() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path();
    for ts in [100u64, 200, 300, 400, 500, 600, 700] {
        seed_run(state_path, "claude", ts, "reporting");
    }
    // Tag the OLDEST run — without preservation it would be the first
    // deleted by default retention=5.
    seed_tag(state_path, "claude", 100, "pre-bump");
    let state = state_path.to_string_lossy().to_string();

    let out = run_gc(&[
        "gc-backups",
        "--state-home",
        &state,
        "--product",
        "claude",
        "--apply",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    // Tagged dir survives.
    assert!(
        state_path.join("backups/claude/100").is_dir(),
        "tagged run must survive"
    );
    // With 6 untagged + retention=5, exactly the next-oldest (200) goes.
    assert!(!state_path.join("backups/claude/200").exists());
    for keep in [300u64, 400, 500, 600, 700] {
        assert!(
            state_path
                .join("backups/claude")
                .join(keep.to_string())
                .is_dir()
        );
    }
}

#[test]
fn retention_three_apply_retains_exactly_three() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path();
    for ts in [100u64, 200, 300, 400, 500] {
        seed_run(state_path, "claude", ts, "reporting");
    }
    let state = state_path.to_string_lossy().to_string();

    let out = run_gc(&[
        "gc-backups",
        "--state-home",
        &state,
        "--product",
        "claude",
        "--retention",
        "3",
        "--apply",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let surviving: Vec<u64> = fs::read_dir(state_path.join("backups/claude"))
        .unwrap()
        .filter_map(|e| {
            e.ok()
                .and_then(|d| d.file_name().to_string_lossy().parse::<u64>().ok())
        })
        .collect();
    let mut sorted = surviving.clone();
    sorted.sort();
    assert_eq!(sorted, vec![300, 400, 500], "kept newest 3 only");
}

#[test]
fn dry_run_produces_zero_file_mutations() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path();
    for ts in [100u64, 200, 300, 400, 500, 600, 700] {
        seed_run(state_path, "claude", ts, "reporting");
    }
    let before: Vec<u64> = fs::read_dir(state_path.join("backups/claude"))
        .unwrap()
        .filter_map(|e| {
            e.ok()
                .and_then(|d| d.file_name().to_string_lossy().parse::<u64>().ok())
        })
        .collect();
    let state = state_path.to_string_lossy().to_string();

    let out = run_gc(&[
        "gc-backups",
        "--state-home",
        &state,
        "--product",
        "claude",
        "--dry-run",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let after: Vec<u64> = fs::read_dir(state_path.join("backups/claude"))
        .unwrap()
        .filter_map(|e| {
            e.ok()
                .and_then(|d| d.file_name().to_string_lossy().parse::<u64>().ok())
        })
        .collect();
    let mut before_sorted = before;
    let mut after_sorted = after;
    before_sorted.sort();
    after_sorted.sort();
    assert_eq!(
        before_sorted, after_sorted,
        "dry-run must not mutate the backup tree"
    );

    let stderr = out.stderr_text();
    assert!(
        stderr.contains("would-delete=2"),
        "dry-run summary should report would-delete count: {stderr}"
    );
    assert!(
        !stderr.contains("deleted="),
        "dry-run must not emit a `deleted=` count: {stderr}"
    );
}

#[test]
fn surface_filter_only_sweeps_runs_with_that_entry_id() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path();
    seed_run(state_path, "claude", 100, "reporting");
    seed_run(state_path, "claude", 200, "skills");
    seed_run(state_path, "claude", 300, "reporting");
    let state = state_path.to_string_lossy().to_string();

    let out = run_gc(&[
        "gc-backups",
        "--state-home",
        &state,
        "--product",
        "claude",
        "--surface",
        "reporting",
        "--retention",
        "1",
        "--apply",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    // ts=200 has no `reporting/` subdir — filter excludes it; survives.
    assert!(
        state_path.join("backups/claude/200").is_dir(),
        "skills run unaffected by --surface reporting"
    );
    // Among reporting runs (100, 300), newest (300) kept, oldest (100) deleted.
    assert!(state_path.join("backups/claude/300").is_dir());
    assert!(!state_path.join("backups/claude/100").exists());
}

#[test]
fn product_default_walks_both_claude_and_codex_subtrees() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path();
    for ts in [100u64, 200, 300, 400, 500, 600] {
        seed_run(state_path, "claude", ts, "reporting");
    }
    for ts in [10u64, 20, 30] {
        seed_run(state_path, "codex", ts, "profile");
    }
    let state = state_path.to_string_lossy().to_string();

    let out = run_gc(&["gc-backups", "--state-home", &state, "--apply"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    // claude: 6 runs → keep 5, delete 1 (ts=100).
    assert!(!state_path.join("backups/claude/100").exists());
    for keep in [200u64, 300, 400, 500, 600] {
        assert!(
            state_path
                .join("backups/claude")
                .join(keep.to_string())
                .is_dir()
        );
    }
    // codex: 3 runs (<= retention 5) → all kept.
    for keep in [10u64, 20, 30] {
        assert!(
            state_path
                .join("backups/codex")
                .join(keep.to_string())
                .is_dir()
        );
    }
}

#[test]
fn missing_mode_flag_exits_nonzero_with_named_flags() {
    let tmp = TempDir::new().unwrap();
    let state = tmp.path().to_string_lossy().to_string();
    let out = run_gc(&["gc-backups", "--state-home", &state, "--product", "claude"]);
    assert_ne!(out.code, 0);
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("--dry-run") && stderr.contains("--apply"),
        "stderr must name both modes: {stderr}"
    );
}

#[test]
fn relative_state_home_is_rejected() {
    let out = run_gc(&[
        "gc-backups",
        "--state-home",
        "./relative",
        "--product",
        "claude",
        "--dry-run",
    ]);
    assert_ne!(out.code, 0);
    assert!(out.stderr_text().contains("absolute"));
}

#[test]
fn invalid_product_value_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let state = tmp.path().to_string_lossy().to_string();
    let out = run_gc(&[
        "gc-backups",
        "--state-home",
        &state,
        "--product",
        "anthropic",
        "--apply",
    ]);
    assert_ne!(out.code, 0);
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("claude") && stderr.contains("codex"),
        "stderr must enumerate valid products: {stderr}"
    );
}

#[test]
fn invalid_surface_with_path_separator_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let state = tmp.path().to_string_lossy().to_string();
    let out = run_gc(&[
        "gc-backups",
        "--state-home",
        &state,
        "--product",
        "claude",
        "--surface",
        "../escape",
        "--apply",
    ]);
    assert_ne!(out.code, 0);
    assert!(out.stderr_text().contains("--surface"));
}

#[test]
fn empty_backup_root_is_clean_noop() {
    let tmp = TempDir::new().unwrap();
    let state = tmp.path().to_string_lossy().to_string();
    let out = run_gc(&["gc-backups", "--state-home", &state, "--apply"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("retained=0") && stderr.contains("deleted=0"),
        "noop summary missing zero counts: {stderr}"
    );
}

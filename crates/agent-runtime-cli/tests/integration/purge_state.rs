//! `agent-runtime purge-state` integration tests. Plan 04 Sprint 2 Task 2.3.
//!
//! Drive the real binary so the CLI argument shape, the `--yes` audit
//! line, the confirmation prompt, and the `--scope` selection all stay
//! pinned end-to-end.

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn agent_runtime_bin() -> std::path::PathBuf {
    bin::resolve("agent-runtime")
}

fn run_purge(args: &[&str], stdin: Option<&[u8]>) -> CmdOutput {
    cmd::run(&agent_runtime_bin(), args, &[], stdin)
}

fn seed_state_tree(state: &Path) {
    fs::create_dir_all(state.join("out/sub")).unwrap();
    fs::write(state.join("out/render.log"), b"RENDER").unwrap();
    fs::write(state.join("out/sub/artifact.txt"), b"ARTIFACT").unwrap();
    fs::create_dir_all(state.join("backups/claude/1700000000/entry")).unwrap();
    fs::write(
        state.join("backups/claude/1700000000/entry/plugin.json"),
        b"BACKUP",
    )
    .unwrap();
}

#[test]
fn missing_scope_exits_non_zero_and_names_three_valid_values() {
    let tmp = TempDir::new().unwrap();
    let state = tmp.path().to_string_lossy().to_string();
    // No `--scope` — must error.
    let out = run_purge(&["purge-state", "--state-home", &state, "--yes"], None);
    assert_ne!(out.code, 0, "missing --scope must exit non-zero: {out:?}");
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("out") && stderr.contains("backups") && stderr.contains("all"),
        "stderr must name the three valid scope values: {stderr}"
    );
}

#[test]
fn invalid_scope_exits_non_zero() {
    let tmp = TempDir::new().unwrap();
    let state = tmp.path().to_string_lossy().to_string();
    let out = run_purge(
        &[
            "purge-state",
            "--state-home",
            &state,
            "--scope",
            "everything",
            "--yes",
        ],
        None,
    );
    assert_ne!(out.code, 0);
    let stderr = out.stderr_text();
    assert!(
        stderr.contains("out") && stderr.contains("backups") && stderr.contains("all"),
        "stderr must enumerate valid scopes on bad input: {stderr}"
    );
}

#[test]
fn scope_out_yes_removes_only_out_subtree_and_emits_audit_line() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path();
    seed_state_tree(state_path);
    let state = state_path.to_string_lossy().to_string();

    let out = run_purge(
        &[
            "purge-state",
            "--state-home",
            &state,
            "--scope",
            "out",
            "--yes",
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let stderr = out.stderr_text();
    // Audit line carries scope.
    assert!(
        stderr.contains("--yes") && stderr.contains("scope=out"),
        "audit line missing or malformed: {stderr}"
    );

    // out/ exists and is empty.
    assert!(state_path.join("out").is_dir());
    assert!(state_path.join("out").read_dir().unwrap().next().is_none());
    // backups/ survived byte-for-byte.
    assert_eq!(
        fs::read_to_string(state_path.join("backups/claude/1700000000/entry/plugin.json")).unwrap(),
        "BACKUP"
    );
}

#[test]
fn scope_backups_yes_removes_only_backups_subtree() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path();
    seed_state_tree(state_path);
    let state = state_path.to_string_lossy().to_string();

    let out = run_purge(
        &[
            "purge-state",
            "--state-home",
            &state,
            "--scope",
            "backups",
            "--yes",
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stderr_text().contains("scope=backups"));

    assert_eq!(
        fs::read_to_string(state_path.join("out/render.log")).unwrap(),
        "RENDER"
    );
    assert!(state_path.join("backups").is_dir());
    assert!(
        state_path
            .join("backups")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn scope_all_yes_removes_both_subtrees() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path();
    seed_state_tree(state_path);
    let state = state_path.to_string_lossy().to_string();

    let out = run_purge(
        &[
            "purge-state",
            "--state-home",
            &state,
            "--scope",
            "all",
            "--yes",
        ],
        None,
    );
    assert_eq!(out.code, 0);
    assert!(state_path.join("out").read_dir().unwrap().next().is_none());
    assert!(
        state_path
            .join("backups")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn confirmation_prompt_with_y_proceeds_and_with_n_cancels() {
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path();
    seed_state_tree(state_path);
    let state = state_path.to_string_lossy().to_string();

    // Answer "y\n" → purge proceeds without the --yes flag.
    let out = run_purge(
        &["purge-state", "--state-home", &state, "--scope", "out"],
        Some(b"y\n"),
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    // No `--yes` audit line in the prompt path.
    assert!(
        !out.stderr_text().contains("--yes"),
        "prompt path must not emit --yes audit: {}",
        out.stderr_text()
    );
    assert!(state_path.join("out").read_dir().unwrap().next().is_none());

    // Re-seed; answer "n\n" → cancelled, content preserved.
    seed_state_tree(state_path);
    let out = run_purge(
        &["purge-state", "--state-home", &state, "--scope", "out"],
        Some(b"n\n"),
    );
    assert_ne!(out.code, 0, "refusal must exit non-zero");
    assert!(out.stderr_text().contains("cancelled"));
    assert_eq!(
        fs::read_to_string(state_path.join("out/render.log")).unwrap(),
        "RENDER"
    );
}

#[test]
fn does_not_touch_runtime_homes_or_auth_history_sessions_cache_projects() {
    // purge-state has no --live-home flag, so by construction it
    // cannot reach a runtime home. This test pins the contract by
    // placing a fake "runtime-home" alongside state_home and
    // asserting purge --scope all leaves every file outside
    // <state>/out and <state>/backups byte-identical.
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state");
    let runtime_home = tmp.path().join("home");
    seed_state_tree(&state_path);
    for top in &["auth", "history", "sessions", "cache", "projects"] {
        let dir = runtime_home.join(top);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("user.bin"), format!("user-{top}")).unwrap();
    }
    fs::write(state_path.join("sibling.txt"), b"SIBLING").unwrap();
    let state = state_path.to_string_lossy().to_string();

    let out = run_purge(
        &[
            "purge-state",
            "--state-home",
            &state,
            "--scope",
            "all",
            "--yes",
        ],
        None,
    );
    assert_eq!(out.code, 0);

    for top in &["auth", "history", "sessions", "cache", "projects"] {
        let path = runtime_home.join(top).join("user.bin");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("user-{top}"),
            "purge must not touch runtime home {path:?}",
        );
    }
    // Sibling files inside state_home but outside out/ and backups/ also survive.
    assert_eq!(
        fs::read_to_string(state_path.join("sibling.txt")).unwrap(),
        "SIBLING"
    );
}

#[test]
fn relative_state_home_is_rejected() {
    let out = run_purge(
        &[
            "purge-state",
            "--state-home",
            "./relative-state",
            "--scope",
            "out",
            "--yes",
        ],
        None,
    );
    assert_ne!(out.code, 0);
    assert!(
        out.stderr_text().contains("absolute"),
        "stderr must explain the rejection: {}",
        out.stderr_text()
    );
}

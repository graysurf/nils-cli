//! Linked-worktree fallback: a project-scope required doc missing in a linked
//! worktree but present in the primary worktree is satisfied under `auto` and
//! missing under `local-only`.

use std::fs;
use std::path::{Path, PathBuf};

use nils_test_support::{cmd, git as test_git};
use tempfile::TempDir;

use super::common::run_cli;

/// Keeps every temp guard (kit, workspace, and the primary repo) alive for the
/// lifetime of a test. Dropping the primary repo would delete its `.git`
/// worktree metadata and break linked-worktree detection.
struct WorktreeFixture {
    _kit: TempDir,
    _workspace: TempDir,
    _repo: TempDir,
    kit_path: PathBuf,
    linked: PathBuf,
}

fn setup() -> WorktreeFixture {
    // kit (docs-home) with a catalog requiring DEVELOPMENT.md for project-dev.
    let kit = TempDir::new().unwrap();
    fs::write(
        kit.path().join("AGENT_DOCS.toml"),
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n",
    )
    .unwrap();

    let workspace = TempDir::new().unwrap();
    let repo = test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    let linked = workspace.path().join("linked");
    test_git::worktree_add_branch(repo.path(), &linked, "linked-worktree");

    // DEVELOPMENT.md lives only in the primary worktree.
    fs::write(repo.path().join("DEVELOPMENT.md"), "# Dev\n").unwrap();
    let local = linked.join("DEVELOPMENT.md");
    if local.exists() {
        fs::remove_file(&local).unwrap();
    }

    let kit_path = kit.path().to_path_buf();
    WorktreeFixture {
        _kit: kit,
        _workspace: workspace,
        _repo: repo,
        kit_path,
        linked,
    }
}

/// Pass `--docs-home` explicitly so the real install symlink never leaks in,
/// and let the project path be detected from the (linked) worktree cwd.
fn options(cwd: &Path) -> cmd::CmdOptions {
    cmd::CmdOptions::default()
        .with_cwd(cwd)
        .with_env_remove("AGENT_DOCS_HOME")
        .with_env_remove("PROJECT_PATH")
}

#[test]
fn auto_uses_primary_worktree_fallback() {
    let fx = setup();
    let out = run_cli(
        &[
            "--docs-home",
            fx.kit_path.to_str().unwrap(),
            "preflight",
            "--intent",
            "project-dev",
            "--strict",
            "--format",
            "json",
        ],
        &options(&fx.linked),
    );
    assert_eq!(
        out.code, 0,
        "auto fallback should satisfy the doc from the primary worktree:\nstdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    assert_eq!(out.json()["documents"][0]["status"], "present");
}

#[test]
fn local_only_keeps_local_strict_behavior() {
    let fx = setup();
    let out = run_cli(
        &[
            "--docs-home",
            fx.kit_path.to_str().unwrap(),
            "--worktree-fallback",
            "local-only",
            "preflight",
            "--intent",
            "project-dev",
            "--strict",
        ],
        &options(&fx.linked),
    );
    assert_eq!(
        out.code, 1,
        "local-only should keep the missing doc unsatisfied:\nstdout={}\nstderr={}",
        out.stdout, out.stderr
    );
}

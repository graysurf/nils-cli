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
    home: PathBuf,
    xdg_config_home: PathBuf,
    linked: PathBuf,
}

fn setup() -> WorktreeFixture {
    let kit = TempDir::new().unwrap();

    let workspace = TempDir::new().unwrap();
    let repo = test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    fs::write(
        repo.path().join("AGENT_DOCS.toml"),
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n",
    )
    .unwrap();
    let linked = workspace.path().join("linked");
    test_git::worktree_add_branch(repo.path(), &linked, "linked-worktree");

    // DEVELOPMENT.md lives only in the primary worktree.
    fs::write(repo.path().join("DEVELOPMENT.md"), "# Dev\n").unwrap();
    let local = linked.join("DEVELOPMENT.md");
    if local.exists() {
        fs::remove_file(&local).unwrap();
    }

    let kit_path = repo.path().to_path_buf();
    let home = kit.path().join("isolated-home");
    let xdg_config_home = kit.path().join("isolated-xdg");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&xdg_config_home).unwrap();
    WorktreeFixture {
        _kit: kit,
        _workspace: workspace,
        _repo: repo,
        kit_path,
        home,
        xdg_config_home,
        linked,
    }
}

/// Pass `--docs-home` explicitly so the real install symlink never leaks in,
/// and let the project path be detected from the (linked) worktree cwd.
fn options(fx: &WorktreeFixture) -> cmd::CmdOptions {
    cmd::CmdOptions::default()
        .with_cwd(&fx.linked)
        .with_env("HOME", fx.home.to_str().unwrap())
        .with_env("XDG_CONFIG_HOME", fx.xdg_config_home.to_str().unwrap())
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
        &options(&fx),
    );
    assert_eq!(
        out.code, 0,
        "auto fallback should satisfy the doc from the primary worktree:\nstdout={}\nstderr={}",
        out.stdout, out.stderr
    );
    assert_eq!(out.json()["documents"][0]["status"], "present");
}

#[test]
fn linked_worktree_subdirectory_fallback_preserves_the_relative_path() {
    const PRIMARY_NESTED: &str = "PRIMARY_NESTED_POLICY_2a";
    const PRIMARY_ROOT: &str = "PRIMARY_ROOT_DECOY_2a";
    const LINKED_ROOT: &str = "LINKED_ROOT_DECOY_2a";

    let fx = setup();
    let nested = fx.linked.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(fx.kit_path.join("nested")).unwrap();
    fs::write(
        fx.kit_path.join("AGENT_DOCS.toml"),
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"POLICY.md\"\nrequired = true\n",
    )
    .unwrap();
    fs::write(fx.kit_path.join("POLICY.md"), PRIMARY_ROOT).unwrap();
    fs::write(fx.kit_path.join("nested/POLICY.md"), PRIMARY_NESTED).unwrap();
    fs::write(fx.linked.join("POLICY.md"), LINKED_ROOT).unwrap();

    let out = run_cli(
        &[
            "--docs-home",
            fx.kit_path.to_str().unwrap(),
            "--project-path",
            nested.to_str().unwrap(),
            "preflight",
            "--intent",
            "project-dev",
            "--strict",
            "--format",
            "json",
        ],
        &isolated_options(&nested, &fx.home, &fx.xdg_config_home),
    );

    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    let document = &out.json()["documents"][0];
    assert_eq!(
        document["path"],
        fx.kit_path.join("nested/POLICY.md").to_str().unwrap()
    );
    assert_eq!(document["content"], PRIMARY_NESTED);
    assert!(!out.stdout.contains(PRIMARY_ROOT));
    assert!(!out.stdout.contains(LINKED_ROOT));
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
        &options(&fx),
    );
    assert_eq!(
        out.code, 1,
        "local-only should keep the missing doc unsatisfied:\nstdout={}\nstderr={}",
        out.stdout, out.stderr
    );
}

#[test]
fn prunable_sibling_does_not_break_current_worktree_preflight() {
    let fx = setup();
    let stale = fx._workspace.path().join("stale-linked");
    test_git::worktree_add_branch(fx._repo.path(), &stale, "stale-linked-worktree");
    fs::remove_dir_all(&stale).unwrap();

    let porcelain = test_git::git(fx._repo.path(), &["worktree", "list", "--porcelain"]);
    assert!(porcelain.contains("prunable"), "{porcelain}");

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
        &options(&fx),
    );
    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(out.json()["documents"][0]["status"], "present");
}

fn isolated_options(cwd: &Path, home: &Path, xdg: &Path) -> cmd::CmdOptions {
    cmd::CmdOptions::default()
        .with_cwd(cwd)
        .with_env("HOME", home.to_str().unwrap())
        .with_env("XDG_CONFIG_HOME", xdg.to_str().unwrap())
        .with_env_remove("AGENT_DOCS_HOME")
        .with_env_remove("PROJECT_PATH")
}

#[test]
fn separate_git_dir_never_uses_common_dir_parent_as_fallback() {
    const OUTSIDE: &str = "OUTSIDE_SEPARATE_GIT_DIR_MARKER_4c8f";

    let temp = TempDir::new().unwrap();
    let worktree = temp.path().join("actual-worktree");
    let docs_home = worktree.clone();
    let git_dir = temp.path().join("separate-metadata.git");
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    for path in [&home, &xdg] {
        fs::create_dir_all(path).unwrap();
    }
    test_git::git(
        temp.path(),
        &[
            "init",
            "-q",
            "--separate-git-dir",
            git_dir.to_str().unwrap(),
            worktree.to_str().unwrap(),
        ],
    );
    fs::write(
        docs_home.join("AGENT_DOCS.toml"),
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"FALLBACK_ONLY.md\"\nrequired = true\n",
    )
    .unwrap();
    fs::write(temp.path().join("FALLBACK_ONLY.md"), OUTSIDE).unwrap();

    let out = run_cli(
        &[
            "--docs-home",
            docs_home.to_str().unwrap(),
            "--project-path",
            worktree.to_str().unwrap(),
            "preflight",
            "--intent",
            "project-dev",
            "--strict",
            "--format",
            "json",
        ],
        &isolated_options(&worktree, &home, &xdg),
    );
    assert_eq!(out.code, 4, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(out.json()["error"]["code"], "root-resolution-failed");
    assert!(!out.stdout.contains(OUTSIDE));
    assert!(!out.stderr.contains(OUTSIDE));
}

#[test]
fn separate_git_dir_linked_worktree_does_not_infer_a_primary_fallback() {
    const PRIMARY: &str = "ACTUAL_PRIMARY_WORKTREE_MARKER_34df";
    const OUTSIDE: &str = "METADATA_PARENT_MARKER_5e2a";

    let temp = TempDir::new().unwrap();
    let primary = temp.path().join("primary");
    let docs_home = primary.clone();
    let git_dir = temp.path().join("separate-metadata.git");
    let linked = temp.path().join("linked");
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    for path in [&home, &xdg] {
        fs::create_dir_all(path).unwrap();
    }
    test_git::git(
        temp.path(),
        &[
            "init",
            "-q",
            "--separate-git-dir",
            git_dir.to_str().unwrap(),
            primary.to_str().unwrap(),
        ],
    );
    test_git::git(&primary, &["checkout", "-q", "-B", "main"]);
    test_git::git(&primary, &["config", "user.email", "test@example.com"]);
    test_git::git(&primary, &["config", "user.name", "Test User"]);
    fs::write(primary.join("seed.md"), "seed\n").unwrap();
    test_git::git(&primary, &["add", "seed.md"]);
    test_git::git(&primary, &["commit", "-qm", "seed"]);
    test_git::worktree_add_branch(&primary, &linked, "linked");

    fs::write(primary.join("FALLBACK_ONLY.md"), PRIMARY).unwrap();
    fs::write(temp.path().join("FALLBACK_ONLY.md"), OUTSIDE).unwrap();
    fs::write(
        docs_home.join("AGENT_DOCS.toml"),
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"FALLBACK_ONLY.md\"\nrequired = true\n",
    )
    .unwrap();

    let out = run_cli(
        &[
            "--docs-home",
            docs_home.to_str().unwrap(),
            "--project-path",
            linked.to_str().unwrap(),
            "preflight",
            "--intent",
            "project-dev",
            "--strict",
            "--format",
            "json",
        ],
        &isolated_options(&linked, &home, &xdg),
    );
    assert_eq!(out.code, 1, "stdout={} stderr={}", out.stdout, out.stderr);
    let json = out.json();
    assert_eq!(json["schema_version"], "agent-docs.preflight.v2");
    assert_eq!(json["is_linked_worktree"], true);
    assert_eq!(json["documents"][0]["status"], "missing");
    assert!(!out.stdout.contains(PRIMARY));
    assert!(!out.stderr.contains(PRIMARY));
    assert!(!out.stdout.contains(OUTSIDE));
    assert!(!out.stderr.contains(OUTSIDE));
}

#[test]
fn bare_backed_linked_worktree_never_uses_bare_parent_content() {
    const OUTSIDE: &str = "BARE_PARENT_MARKER_973b";

    let temp = TempDir::new().unwrap();
    let bare = temp.path().join("bare.git");
    let seed = temp.path().join("seed");
    let linked = temp.path().join("linked");
    let docs_home = linked.clone();
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    for path in [&home, &xdg] {
        fs::create_dir_all(path).unwrap();
    }
    test_git::git(
        temp.path(),
        &["init", "-q", "--bare", bare.to_str().unwrap()],
    );
    test_git::git(
        temp.path(),
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            seed.to_str().unwrap(),
        ],
    );
    test_git::git(&seed, &["config", "user.email", "test@example.com"]);
    test_git::git(&seed, &["config", "user.name", "Test User"]);
    fs::write(seed.join("seed.md"), "seed\n").unwrap();
    test_git::git(&seed, &["add", "seed.md"]);
    test_git::git(&seed, &["commit", "-qm", "seed"]);
    test_git::git(&seed, &["push", "-q", "origin", "HEAD:main"]);
    test_git::git(
        temp.path(),
        &[
            "--git-dir",
            bare.to_str().unwrap(),
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
    );
    test_git::worktree_add_branch(&bare, &linked, "linked");

    fs::write(temp.path().join("FALLBACK_ONLY.md"), OUTSIDE).unwrap();
    fs::write(
        docs_home.join("AGENT_DOCS.toml"),
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"FALLBACK_ONLY.md\"\nrequired = true\n",
    )
    .unwrap();

    let out = run_cli(
        &[
            "--docs-home",
            docs_home.to_str().unwrap(),
            "--project-path",
            linked.to_str().unwrap(),
            "preflight",
            "--intent",
            "project-dev",
            "--strict",
            "--format",
            "json",
        ],
        &isolated_options(&linked, &home, &xdg),
    );
    assert_eq!(out.code, 1, "stdout={} stderr={}", out.stdout, out.stderr);
    let json = out.json();
    assert_eq!(json["schema_version"], "agent-docs.preflight.v2");
    assert_eq!(json["is_linked_worktree"], true);
    assert_eq!(json["documents"][0]["status"], "missing");
    assert!(!out.stdout.contains(OUTSIDE));
    assert!(!out.stderr.contains(OUTSIDE));
}

#[test]
fn bare_repository_never_emits_common_dir_parent_content() {
    const OUTSIDE: &str = "OUTSIDE_BARE_REPOSITORY_MARKER_81bd";

    let temp = TempDir::new().unwrap();
    let bare = temp.path().join("bare.git");
    let docs_home = bare.clone();
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    for path in [&home, &xdg] {
        fs::create_dir_all(path).unwrap();
    }
    test_git::git(
        temp.path(),
        &["init", "-q", "--bare", bare.to_str().unwrap()],
    );
    fs::write(
        docs_home.join("AGENT_DOCS.toml"),
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"FALLBACK_ONLY.md\"\nrequired = true\n",
    )
    .unwrap();
    fs::write(temp.path().join("FALLBACK_ONLY.md"), OUTSIDE).unwrap();

    let out = run_cli(
        &[
            "--docs-home",
            docs_home.to_str().unwrap(),
            "--project-path",
            bare.to_str().unwrap(),
            "preflight",
            "--intent",
            "project-dev",
            "--strict",
            "--format",
            "json",
        ],
        &isolated_options(&bare, &home, &xdg),
    );
    assert_eq!(out.code, 1, "stdout={} stderr={}", out.stdout, out.stderr);
    let json = out.json();
    assert_eq!(json["schema_version"], "agent-docs.preflight.v2");
    assert_eq!(json["documents"][0]["status"], "missing");
    assert!(!out.stdout.contains(OUTSIDE));
    assert!(!out.stderr.contains(OUTSIDE));
}

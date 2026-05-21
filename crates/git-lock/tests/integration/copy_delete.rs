use crate::common;
use common::{init_repo, repo_id, run_git_lock, run_git_lock_output};
use std::path::PathBuf;
use tempfile::TempDir;

fn cache_dir() -> TempDir {
    tempfile::TempDir::new().expect("cache dir")
}

fn latest_file(cache: &TempDir, repo: &str) -> PathBuf {
    cache
        .path()
        .join("git-locks")
        .join(format!("{repo}-latest"))
}

#[test]
fn copy_overwrite_prompt() {
    let repo = init_repo();
    let cache = cache_dir();
    let repo_name = repo_id(repo.path());
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    run_git_lock(repo.path(), &["lock", "a"], &env, None);
    run_git_lock(repo.path(), &["lock", "b"], &env, None);

    let output = run_git_lock_output(repo.path(), &["copy", "a", "b"], &env, Some("n\n"));
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("Overwrite? [y/N]"));
    assert!(stdout.contains("🚫 Aborted"));
    assert!(!output.status.success());
    assert!(latest_file(&cache, &repo_name).exists());
}

#[test]
fn copy_missing_source_label() {
    let repo = init_repo();
    let cache = cache_dir();
    let repo_name = repo_id(repo.path());
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    run_git_lock(repo.path(), &["lock", "a"], &env, None);

    let output = run_git_lock_output(repo.path(), &["copy", "missing", "b"], &env, None);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains(&format!("Source git-lock [{repo_name}:missing] not found")));
    assert!(!output.status.success());
}

#[test]
fn delete_latest() {
    let repo = init_repo();
    let cache = cache_dir();
    let repo_name = repo_id(repo.path());
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    run_git_lock(repo.path(), &["lock", "wip"], &env, None);

    let output = run_git_lock_output(repo.path(), &["delete", "--force", "wip"], &env, None);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains(&format!("🗑️  Deleted git-lock [{repo_name}:wip]")));
    assert!(stdout.contains("Removed latest marker"));
    assert!(!latest_file(&cache, &repo_name).exists());
}

#[test]
fn delete_non_interactive_without_force_exits_usage() {
    let repo = init_repo();
    let cache = cache_dir();
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    run_git_lock(repo.path(), &["lock", "wip"], &env, None);

    let output = run_git_lock_output(repo.path(), &["delete", "wip"], &env, None);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(64));
    assert!(stderr.contains("requires --force when stdin is not a TTY"));
}

#[test]
fn delete_force_skips_prompt() {
    let repo = init_repo();
    let cache = cache_dir();
    let repo_name = repo_id(repo.path());
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    run_git_lock(repo.path(), &["lock", "wip"], &env, None);

    let output = run_git_lock_output(repo.path(), &["delete", "--force", "wip"], &env, None);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(!stdout.contains("[y/N]"));
    assert!(stdout.contains(&format!("🗑️  Deleted git-lock [{repo_name}:wip]")));
}

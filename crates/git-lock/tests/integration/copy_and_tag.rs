use crate::common;
use common::{commit_file, init_repo, repo_id, run_git_lock, run_git_lock_output};
use nils_test_support::{EnvGuard, GlobalStateLock};
use std::fs;
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

fn isolate_git_env() -> (GlobalStateLock, TempDir, EnvGuard, EnvGuard, EnvGuard) {
    let lock = GlobalStateLock::new();
    let home = tempfile::TempDir::new().expect("home dir");
    let home_guard = EnvGuard::set(&lock, "HOME", home.path().to_str().unwrap());
    let global_guard = EnvGuard::set(&lock, "GIT_CONFIG_GLOBAL", "/dev/null");
    let system_guard = EnvGuard::set(&lock, "GIT_CONFIG_SYSTEM", "/dev/null");
    (lock, home, home_guard, global_guard, system_guard)
}

#[test]
fn copy_overwrite_prompt_accepts() {
    let (_lock, _home, _home_guard, _global_guard, _system_guard) = isolate_git_env();
    let repo = init_repo();
    let cache = cache_dir();
    let repo_name = repo_id(repo.path());
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    run_git_lock(repo.path(), &["lock", "a"], &env, None);
    run_git_lock(repo.path(), &["lock", "b"], &env, None);

    let output = run_git_lock_output(repo.path(), &["copy", "a", "b"], &env, Some("y\n"));
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("Copied git-lock"));
    let latest = fs::read_to_string(latest_file(&cache, &repo_name)).expect("read latest");
    assert_eq!(latest.trim(), "b");
}

#[test]
fn tag_overwrite_prompt_aborts_then_accepts() {
    let (_lock, _home, _home_guard, _global_guard, _system_guard) = isolate_git_env();
    let repo = init_repo();
    let cache = cache_dir();
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    let first_hash = common::git(repo.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    run_git_lock(repo.path(), &["lock", "first"], &env, None);

    let output = run_git_lock_output(repo.path(), &["tag", "first", "release"], &env, None);
    assert!(output.status.success());
    let tag_hash = common::git(repo.path(), &["rev-parse", "release^{}"])
        .trim()
        .to_string();
    assert_eq!(tag_hash, first_hash);

    let second_hash = commit_file(repo.path(), "new.txt", "v2", "second");
    run_git_lock(repo.path(), &["lock", "second"], &env, None);

    let output = run_git_lock_output(
        repo.path(),
        &["tag", "second", "release"],
        &env,
        Some("n\n"),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success());
    assert!(stdout.contains("Overwrite it? [y/N]"));
    assert!(stdout.contains("🚫 Aborted"));
    let tag_hash_after = common::git(repo.path(), &["rev-parse", "release^{}"])
        .trim()
        .to_string();
    assert_eq!(tag_hash_after, tag_hash);

    let output = run_git_lock_output(
        repo.path(),
        &["tag", "second", "release"],
        &env,
        Some("y\n"),
    );
    assert!(output.status.success());
    let tag_hash_after = common::git(repo.path(), &["rev-parse", "release^{}"])
        .trim()
        .to_string();
    assert_eq!(tag_hash_after, second_hash);
}

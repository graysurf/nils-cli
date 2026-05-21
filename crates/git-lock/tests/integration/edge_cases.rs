use crate::common;
use common::{init_repo, repo_id, run_git_lock, run_git_lock_output};
use std::fs;

#[test]
fn not_a_repo() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let output = run_git_lock_output(
        dir.path(),
        &["list"],
        &[("ZSH_CACHE_DIR", dir.path().to_str().unwrap())],
        None,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Not a Git repository"));
    assert!(!output.status.success());
}

#[test]
fn invalid_commit() {
    let repo = init_repo();
    let cache = tempfile::TempDir::new().expect("cache");
    let output = run_git_lock_output(
        repo.path(),
        &["lock", "bad", "note", "BADCOMMIT"],
        &[("ZSH_CACHE_DIR", cache.path().to_str().unwrap())],
        None,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("❌ Invalid commit: BADCOMMIT"));
    assert!(!output.status.success());
}

#[test]
fn missing_latest_unlock() {
    let repo = init_repo();
    let cache = tempfile::TempDir::new().expect("cache");
    let output = run_git_lock_output(
        repo.path(),
        &["unlock"],
        &[("ZSH_CACHE_DIR", cache.path().to_str().unwrap())],
        Some("n\n"),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No recent git-lock found"));
    assert!(!output.status.success());
}

#[test]
fn diff_too_many_labels() {
    let repo = init_repo();
    let cache = tempfile::TempDir::new().expect("cache");
    let output = run_git_lock_output(
        repo.path(),
        &["diff", "a", "b", "c"],
        &[("ZSH_CACHE_DIR", cache.path().to_str().unwrap())],
        None,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Too many labels provided"));
    assert!(stdout.contains("Usage: git-lock diff"));
    assert!(!output.status.success());
}

#[test]
fn delete_without_latest_label() {
    let repo = init_repo();
    let cache = tempfile::TempDir::new().expect("cache");
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    let lock_dir = cache.path().join("git-locks");
    fs::create_dir_all(&lock_dir).expect("create lock dir");

    let output = run_git_lock_output(repo.path(), &["delete"], &env, None);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No label provided and no latest git-lock exists"));
    assert!(!output.status.success());
}

#[test]
fn unlock_missing_label() {
    let repo = init_repo();
    let cache = tempfile::TempDir::new().expect("cache");
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    run_git_lock(repo.path(), &["lock", "wip"], &env, None);

    let output = run_git_lock_output(repo.path(), &["unlock", "missing"], &env, None);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let repo_name = repo_id(repo.path());
    assert!(stdout.contains(&format!(
        "No git-lock named 'missing' found for {repo_name}"
    )));
    assert!(stdout.contains("run `git-lock list`"));
    assert!(!output.status.success());
}

#[test]
fn diff_missing_second_label() {
    let repo = init_repo();
    let cache = tempfile::TempDir::new().expect("cache");
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];
    let repo_name = repo_id(repo.path());

    run_git_lock(repo.path(), &["lock", "a"], &env, None);

    let latest = cache
        .path()
        .join("git-locks")
        .join(format!("{repo_name}-latest"));
    fs::remove_file(&latest).expect("remove latest");

    let output = run_git_lock_output(repo.path(), &["diff", "a"], &env, None);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Second label not provided or found"));
    assert!(!output.status.success());
}

#[test]
fn unknown_command_prints_message() {
    let repo = init_repo();
    let cache = tempfile::TempDir::new().expect("cache");
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    let output = run_git_lock_output(repo.path(), &["nope"], &env, None);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Unknown command: 'nope'"));
    assert!(stdout.contains("Run 'git-lock help' for usage."));
    assert!(!output.status.success());
}

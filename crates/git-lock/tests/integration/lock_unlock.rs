use crate::common;
use common::{commit_file, init_repo, repo_id, run_git_lock, run_git_lock_output};
use std::path::PathBuf;
use tempfile::TempDir;

fn cache_dir() -> TempDir {
    tempfile::TempDir::new().expect("cache dir")
}

fn lock_path(cache: &TempDir, repo: &str, label: &str) -> PathBuf {
    cache
        .path()
        .join("git-locks")
        .join(format!("{repo}-{label}.lock"))
}

#[test]
fn lock_default() {
    let repo = init_repo();
    let cache = cache_dir();
    let repo_name = repo_id(repo.path());

    let output = run_git_lock(
        repo.path(),
        &["lock"],
        &[("ZSH_CACHE_DIR", cache.path().to_str().unwrap())],
        None,
    );

    assert!(output.contains(&format!("🔐 [{repo_name}:default] Locked:")));
    assert!(output.contains("    at "));

    let lock_file = lock_path(&cache, &repo_name, "default");
    assert!(lock_file.exists());
}

#[test]
fn unlock_cancel() {
    let repo = init_repo();
    let cache = cache_dir();
    let repo_name = repo_id(repo.path());

    run_git_lock(
        repo.path(),
        &["lock", "wip"],
        &[("ZSH_CACHE_DIR", cache.path().to_str().unwrap())],
        None,
    );

    let output = run_git_lock_output(
        repo.path(),
        &["unlock", "wip"],
        &[("ZSH_CACHE_DIR", cache.path().to_str().unwrap())],
        Some("n\n"),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&format!("🔐 Found [{repo_name}:wip]")));
    assert!(stdout.contains("Hard reset to [wip]"));
    assert!(stdout.contains("🚫 Aborted"));
    assert!(!output.status.success());
}

#[test]
fn unlock_dry_run_prints_summary_and_keeps_head() {
    let repo = init_repo();
    let cache = cache_dir();
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    let base_hash = common::git(repo.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    commit_file(repo.path(), "change.txt", "second", "second");
    let second_hash = common::git(repo.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_string();

    run_git_lock(
        repo.path(),
        &["lock", "base", "note", &base_hash],
        &env,
        None,
    );

    let output = run_git_lock_output(repo.path(), &["unlock", "--dry-run", "base"], &env, None);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Dry run"));
    assert!(stdout.contains("Changed files: 1"));
    assert!(stdout.contains("change.txt"));

    let head_after = common::git(repo.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_eq!(head_after, second_hash);
}

#[test]
fn unlock_verbose_prints_full_diff_before_prompt() {
    let repo = init_repo();
    let cache = cache_dir();
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    let base_hash = common::git(repo.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    commit_file(repo.path(), "change.txt", "second", "second");

    run_git_lock(
        repo.path(),
        &["lock", "base", "note", &base_hash],
        &env,
        None,
    );

    let output = run_git_lock_output(
        repo.path(),
        &["unlock", "--verbose", "base"],
        &env,
        Some("n\n"),
    );
    assert!(!output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Full diff"));
    assert!(stdout.contains("diff --git"));
    assert!(stdout.contains("Hard reset to [base]"));
}

#[test]
fn unlock_confirms_and_resets() {
    let repo = init_repo();
    let cache = cache_dir();
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    let base_hash = common::git(repo.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    commit_file(repo.path(), "change.txt", "second", "second");

    run_git_lock(
        repo.path(),
        &["lock", "base", "note", &base_hash],
        &env,
        None,
    );

    let output = run_git_lock_output(repo.path(), &["unlock", "base"], &env, Some("y\n"));
    assert!(output.status.success());

    let head_after = common::git(repo.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_eq!(head_after, base_hash);
}

use crate::common;
use common::{git, init_repo, repo_id, run_git_lock};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn cache_dir() -> TempDir {
    tempfile::TempDir::new().expect("cache dir")
}

fn lock_file(cache: &TempDir, repo: &str, label: &str) -> PathBuf {
    cache
        .path()
        .join("git-locks")
        .join(format!("{repo}-{label}.lock"))
}

fn rewrite_timestamp(path: &PathBuf, timestamp: &str) {
    let content = fs::read_to_string(path).expect("read lock file");
    let mut lines: Vec<String> = content.lines().map(|line| line.to_string()).collect();
    if lines.len() >= 2 {
        lines[1] = format!("timestamp={timestamp}");
    } else {
        lines.push(format!("timestamp={timestamp}"));
    }
    fs::write(path, lines.join("\n") + "\n").expect("write lock file");
}

#[test]
fn list_latest_sorted() {
    let repo = init_repo();
    let cache = cache_dir();
    let repo_name = repo_id(repo.path());
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    run_git_lock(repo.path(), &["lock", "first"], &env, None);
    run_git_lock(repo.path(), &["lock", "second"], &env, None);

    let first_path = lock_file(&cache, &repo_name, "first");
    let second_path = lock_file(&cache, &repo_name, "second");
    rewrite_timestamp(&first_path, "2000-01-01 00:00:00");
    rewrite_timestamp(&second_path, "2001-01-01 00:00:00");

    let output = run_git_lock(repo.path(), &["list"], &env, None);
    let idx_second = output.find("tag:     second").expect("second label");
    let idx_first = output.find("tag:     first").expect("first label");
    assert!(idx_second < idx_first);
    assert!(output.contains("second  ⭐ (latest)"));
}

#[test]
fn list_no_lock_dir_prints_empty_message() {
    let repo = init_repo();
    let cache = cache_dir();
    let repo_name = repo_id(repo.path());
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    let output = run_git_lock(repo.path(), &["list"], &env, None);
    assert!(output.contains(&format!("📬 No git-locks found for [{repo_name}]")));
}

#[test]
fn list_empty_lock_dir_prints_empty_message() {
    let repo = init_repo();
    let cache = cache_dir();
    let repo_name = repo_id(repo.path());
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    let lock_dir = cache.path().join("git-locks");
    fs::create_dir_all(&lock_dir).expect("create lock dir");

    let output = run_git_lock(repo.path(), &["list"], &env, None);
    assert!(output.contains(&format!("📬 No git-locks found for [{repo_name}]")));
}

#[test]
fn list_handles_corrupted_lock_files() {
    let repo = init_repo();
    let cache = cache_dir();
    let repo_name = repo_id(repo.path());
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];

    let lock_dir = cache.path().join("git-locks");
    fs::create_dir_all(&lock_dir).expect("create lock dir");

    let hash = git(repo.path(), &["rev-parse", "HEAD"]).trim().to_string();

    let broken_path = lock_dir.join(format!("{repo_name}-broken.lock"));
    let broken_content = format!("  {hash}   #   spaced note   \ntimestamp=bad timestamp\n");
    fs::write(&broken_path, broken_content).expect("write broken lock");

    let empty_path = lock_dir.join(format!("{repo_name}-empty.lock"));
    fs::write(&empty_path, " \n").expect("write empty lock");

    let latest_path = lock_dir.join(format!("{repo_name}-latest"));
    fs::write(&latest_path, "broken\n").expect("write latest");

    let output = run_git_lock(repo.path(), &["list"], &env, None);
    assert!(output.contains("tag:     broken"));
    assert!(output.contains("tag:     empty"));
    assert!(output.contains("📝 note:    spaced note"));
    assert!(output.contains("📅 time:    bad timestamp"));
}

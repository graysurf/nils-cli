use crate::common;

#[test]
fn unknown_command_exits_usage() {
    let repo = common::init_repo();
    let cache = tempfile::TempDir::new().expect("cache");
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];
    let output = common::run_git_lock_output(repo.path(), &["nope"], &env, None);
    assert_eq!(output.status.code(), Some(64));
}

#[test]
fn outside_repo_exits_runtime() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let cache = tempfile::TempDir::new().expect("cache");
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];
    let output = common::run_git_lock_output(dir.path(), &["list"], &env, None);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn help_flag_exits_success() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let cache = tempfile::TempDir::new().expect("cache");
    let env = [("ZSH_CACHE_DIR", cache.path().to_str().unwrap())];
    let output = common::run_git_lock_output(dir.path(), &["--help"], &env, None);
    assert_eq!(output.status.code(), Some(0));
}

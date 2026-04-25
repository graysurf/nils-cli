use std::path::Path;
use std::process::Output;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::run_resolved_in_dir_with_stdin_str;
use nils_test_support::git::init_repo_main_with_initial_commit;
#[allow(unused_imports)]
pub use nils_test_support::git::{commit_file, git, repo_id};

#[allow(dead_code)]
pub fn init_repo() -> tempfile::TempDir {
    init_repo_main_with_initial_commit()
}

#[allow(dead_code)]
pub fn git_lock_bin() -> std::path::PathBuf {
    resolve("git-lock")
}

#[allow(dead_code)]
pub fn run_git_lock_output(
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    input: Option<&str>,
) -> Output {
    let output = run_resolved_in_dir_with_stdin_str("git-lock", dir, args, envs, input);
    output.into_output()
}

#[allow(dead_code)]
pub fn run_git_lock(
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    input: Option<&str>,
) -> String {
    let output = run_git_lock_output(dir, args, envs, input);
    if !output.status.success() {
        panic!(
            "git-lock {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    }
    String::from_utf8_lossy(&output.stdout).to_string()
}

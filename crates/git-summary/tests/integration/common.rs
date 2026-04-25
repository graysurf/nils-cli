#![allow(dead_code)]

use std::path::Path;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{options_in_dir_with_envs, run_resolved};
use nils_test_support::git::{InitRepoOptions, init_repo_with};
#[allow(unused_imports)]
pub use nils_test_support::git::{git, git_with_env};

pub fn init_repo() -> tempfile::TempDir {
    init_repo_with(InitRepoOptions::new().without_branch())
}

pub fn git_summary_bin() -> std::path::PathBuf {
    resolve("git-summary")
}

pub fn run_git_summary(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
    let options = options_in_dir_with_envs(dir, envs);
    let output = run_resolved("git-summary", args, &options);
    if !output.success() {
        panic!(
            "git-summary {:?} failed: {}{}",
            args,
            output.stderr_text(),
            output.stdout_text()
        );
    }
    output.stdout_text()
}

#[allow(dead_code)]
pub fn run_git_summary_allow_fail(
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> (i32, String) {
    let options = options_in_dir_with_envs(dir, envs);
    let output = run_resolved("git-summary", args, &options);
    (output.code, output.stdout_text())
}

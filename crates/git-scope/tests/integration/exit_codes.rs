use crate::common;
use nils_test_support::cmd::{options_in_dir_with_envs, run_resolved};

#[test]
fn commit_missing_required_arg_exits_usage() {
    let repo = common::init_repo();
    let options = options_in_dir_with_envs(repo.path(), &[]);
    let output = run_resolved("git-scope", &["commit"], &options);
    assert_eq!(output.code, 64);
}

#[test]
fn unknown_subcommand_returns_clap_usage_exit_code() {
    let repo = common::init_repo();
    let options = options_in_dir_with_envs(repo.path(), &[]);
    let output = run_resolved("git-scope", &["bogus"], &options);
    assert_eq!(output.code, 2);
}

#[test]
fn outside_git_repo_returns_runtime_exit_code() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let options = options_in_dir_with_envs(dir.path(), &[]);
    let output = run_resolved("git-scope", &["staged"], &options);
    assert_eq!(output.code, 1);
}

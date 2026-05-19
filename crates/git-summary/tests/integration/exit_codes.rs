use crate::common;

#[test]
fn missing_required_args_exits_usage() {
    let repo = common::init_repo();
    let (code, _) = common::run_git_summary_allow_fail(repo.path(), &["2024-01-01"], &[]);
    assert_eq!(code, 64);
}

#[test]
fn outside_git_repo_exits_runtime() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let (code, _) = common::run_git_summary_allow_fail(dir.path(), &["all"], &[]);
    assert_eq!(code, 1);
}

#[test]
fn help_exits_success() {
    let repo = common::init_repo();
    let (code, _) = common::run_git_summary_allow_fail(repo.path(), &["--help"], &[]);
    assert_eq!(code, 0);
}

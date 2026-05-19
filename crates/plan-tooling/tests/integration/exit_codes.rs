use nils_test_support::cmd::run_resolved_in_dir;

#[test]
fn unknown_subcommand_exits_usage() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run_resolved_in_dir("plan-tooling", tmp.path(), &["bogus"], &[], None);
    assert_eq!(output.code, 64);
}

#[test]
fn help_flag_exits_success() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run_resolved_in_dir("plan-tooling", tmp.path(), &["--help"], &[], None);
    assert_eq!(output.code, 0);
}

#[test]
fn no_args_exits_success_with_help() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run_resolved_in_dir("plan-tooling", tmp.path(), &[], &[], None);
    assert_eq!(output.code, 0);
}

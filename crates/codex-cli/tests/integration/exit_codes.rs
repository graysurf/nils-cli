use nils_test_support::cmd::run_resolved_in_dir;

#[test]
fn unknown_subcommand_exits_usage() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run_resolved_in_dir("codex-cli", tmp.path(), &["bogus"], &[], None);
    assert_eq!(output.code, 64);
}

#[test]
fn help_flag_exits_success() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run_resolved_in_dir("codex-cli", tmp.path(), &["--help"], &[], None);
    assert_eq!(output.code, 0);
}

#[test]
fn invalid_flag_exits_usage() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run_resolved_in_dir("codex-cli", tmp.path(), &["--no-such-flag"], &[], None);
    assert_eq!(output.code, 64);
}

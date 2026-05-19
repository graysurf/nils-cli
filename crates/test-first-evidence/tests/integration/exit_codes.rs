use nils_test_support::cmd::run_resolved_in_dir;

#[test]
fn unknown_subcommand_exits_usage() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run_resolved_in_dir("test-first-evidence", tmp.path(), &["bogus"], &[], None);
    assert_eq!(output.code, 64);
}

#[test]
fn missing_subcommand_exits_clap_help() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run_resolved_in_dir("test-first-evidence", tmp.path(), &[], &[], None);
    // No subcommand → clap renders help (exit 2) or 64.
    assert!(
        matches!(output.code, 0 | 2 | 64),
        "unexpected exit code {}",
        output.code
    );
}

#[test]
fn help_flag_exits_success() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run_resolved_in_dir("test-first-evidence", tmp.path(), &["--help"], &[], None);
    assert_eq!(output.code, 0);
}

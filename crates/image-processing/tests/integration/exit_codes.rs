use crate::common;

#[test]
fn unknown_subcommand_exits_usage() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let out = common::run_image_processing(dir.path(), &["bogus"], &[]);
    assert_eq!(out.code, 64);
}

#[test]
fn missing_required_arg_exits_usage() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let out = common::run_image_processing(dir.path(), &["convert"], &[]);
    assert_eq!(out.code, 64);
}

#[test]
fn help_flag_exits_success() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let out = common::run_image_processing(dir.path(), &["--help"], &[]);
    assert_eq!(out.code, 0);
}

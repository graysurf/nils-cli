use std::path::Path;

use nils_test_support::cmd::{CmdOptions, run_resolved};

fn run_exit_code(dir: &Path, args: &[&str]) -> i32 {
    run_resolved("agent-out", args, &CmdOptions::new().with_cwd(dir)).code
}

#[test]
fn unknown_subcommand_exits_usage() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    assert_eq!(run_exit_code(dir.path(), &["bogus-command"]), 64);
}

#[test]
fn missing_subcommand_exits_usage() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    assert_eq!(run_exit_code(dir.path(), &[]), 64);
}

#[test]
fn help_flag_exits_success() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    assert_eq!(run_exit_code(dir.path(), &["--help"]), 0);
}

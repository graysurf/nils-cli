use tempfile::TempDir;

use crate::common;
#[test]
fn completion_export_succeeds_outside_git_repo() {
    let dir = TempDir::new().expect("tempdir");
    let out = common::run_plan_tooling(dir.path(), &["completion", "zsh"]);

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert!(
        out.stdout.contains("#compdef plan-tooling"),
        "{}",
        out.stdout
    );
}

#[test]
fn completion_rejects_unknown_shell_outside_git_repo() {
    let dir = TempDir::new().expect("tempdir");
    let out = common::run_plan_tooling(dir.path(), &["completion", "fish"]);

    assert_ne!(out.code, 0);
    assert!(out.stderr.contains("unsupported completion shell"));
    assert!(out.stderr.contains("fish"));
}

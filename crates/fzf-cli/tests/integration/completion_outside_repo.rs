use crate::common;
#[test]
fn completion_export_succeeds_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let out = common::run_fzf_cli(temp.path(), &["completion", "zsh"], &[], None);

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert!(out.stdout.contains("#compdef fzf-cli"), "{}", out.stdout);
}

#[test]
fn completion_rejects_unknown_shell_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let out = common::run_fzf_cli(temp.path(), &["completion", "fish"], &[], None);

    assert_ne!(out.code, 0);
    assert!(out.stderr.contains("unsupported completion shell"));
    assert!(out.stderr.contains("fish"));
}

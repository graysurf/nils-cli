use nils_test_support::cmd;

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let options = cmd::CmdOptions::default().with_cwd(temp.path());
    let output = cmd::run_resolved("memo", &["completion", "zsh"], &options);

    assert_eq!(output.code, 0, "expected exit code 0, got: {output:?}");
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("#compdef memo"),
        "missing zsh completion header: {stdout}"
    );
}

#[test]
fn completion_rejects_unknown_shell_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let options = cmd::CmdOptions::default().with_cwd(temp.path());
    let output = cmd::run_resolved("memo", &["completion", "fish"], &options);

    assert!(
        output.code != 0,
        "expected non-zero exit code for unknown shell, got: {output:?}"
    );
    let stderr = output.stderr_text();
    assert!(
        stderr.contains("invalid value") && stderr.contains("fish"),
        "missing invalid shell error: {stderr}"
    );
}

#[test]
fn completion_bash_export_is_normalized() {
    let temp = tempfile::TempDir::new().unwrap();
    let options = cmd::CmdOptions::default().with_cwd(temp.path());
    let output = cmd::run_resolved("memo", &["completion", "bash"], &options);

    assert_eq!(output.code, 0, "expected exit code 0, got: {output:?}");
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("_memo()"),
        "missing bash completion entry point: {stdout}"
    );
    // clap_complete emits `__subcmd__` separators in its generated command
    // ids; the shipped `completions/bash/memo` asset must not carry them, so
    // the normalizer rewrites them before the script reaches stdout.
    assert!(
        !stdout.contains("__subcmd__"),
        "bash completion leaked an un-normalized subcommand separator"
    );
}

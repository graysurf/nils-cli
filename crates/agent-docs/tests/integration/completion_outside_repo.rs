use nils_test_support::cmd;

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let options = cmd::CmdOptions::default().with_cwd(temp.path());
    let output = cmd::run_resolved("agent-docs", &["completion", "zsh"], &options);

    assert_eq!(output.code, 0, "expected exit code 0, got: {output:?}");
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("#compdef agent-docs"),
        "missing zsh completion header: {stdout}"
    );
}

#[test]
fn completion_rejects_unknown_shell_outside_git_repo() {
    let temp = tempfile::TempDir::new().unwrap();
    let options = cmd::CmdOptions::default().with_cwd(temp.path());
    let output = cmd::run_resolved("agent-docs", &["completion", "fish"], &options);

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

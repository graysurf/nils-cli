use crate::common;
use common::GitCliHarness;

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["completion", "zsh"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    let stdout = output.stdout_text();
    assert!(stdout.contains("#compdef git-cli"));
    assert!(!stdout.contains("Not a git repository"));
}

#[test]
fn completion_zsh_emits_dynamic_registration() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["completion", "zsh"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    let stdout = output.stdout_text();
    // git-cli is a `completion_engine=dynamic` CLI: the exported zsh script is a
    // clap_complete `CompleteEnv` registration stub, not a static `generate()`
    // script. The dynamic completer calls back into the binary at TAB time.
    assert!(
        stdout.contains("#compdef git-cli"),
        "dynamic zsh registration keeps the #compdef header"
    );
    assert!(
        stdout.contains("_clap_dynamic_completer_git_cli"),
        "dynamic zsh registration defines the CompleteEnv completer function"
    );
    assert!(
        stdout.contains("compdef _clap_dynamic_completer_git_cli git-cli"),
        "dynamic zsh registration binds the completer to git-cli"
    );
    // The static `generate()` surface (subcommand descriptions / `_arguments`)
    // must be gone: candidates are computed at runtime, not baked into the stub.
    assert!(
        !stdout.contains("worktree:Worktree helpers"),
        "dynamic stub must not embed static subcommand descriptions"
    );
    assert!(
        !stdout.contains("_arguments"),
        "dynamic stub must not embed the static `_arguments` surface"
    );
}

#[test]
fn completion_bash_emits_dynamic_registration() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["completion", "bash"]);

    assert_eq!(output.code, 0);
    assert_eq!(output.stderr_text(), "");
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("_clap_complete_git_cli"),
        "dynamic bash registration defines the CompleteEnv completer function"
    );
    assert!(
        stdout.contains("-F _clap_complete_git_cli git-cli"),
        "dynamic bash registration binds the completer to git-cli via complete -F"
    );
}

#[test]
fn completion_rejects_unknown_shell_outside_git_repo() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let output = harness.run(dir.path(), &["completion", "fish"]);

    assert_eq!(output.code, 1);
    assert!(
        output
            .stderr_text()
            .contains("unsupported completion shell")
    );
    assert!(!output.stderr_text().contains("Not a git repository"));
}

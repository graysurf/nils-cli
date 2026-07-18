use crate::common;
use common::GitCliHarness;
use nils_test_support::cmd::run_with;

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
fn dynamic_completion_exposes_dirty_checkout_argument_contracts() {
    let harness = GitCliHarness::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("reason.txt"), "reason\n").expect("write reason fixture");

    let complete = |words: &[&str], index: &str| {
        let options = harness
            .cmd_options(dir.path())
            .with_env("COMPLETE", "zsh")
            .with_env("_CLAP_COMPLETE_INDEX", index)
            .with_env("_CLAP_IFS", "\n");
        let mut args = vec!["--", "git-cli"];
        args.extend_from_slice(words);
        let output = run_with(&harness.git_cli_bin(), &args, &options);
        assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
        output
            .stdout_text()
            .lines()
            .map(|line| {
                line.split(':')
                    .next()
                    .expect("completion candidate")
                    .to_string()
            })
            .collect::<Vec<_>>()
    };
    let has = |candidates: &[String], expected: &str| {
        candidates.iter().any(|candidate| candidate == expected)
    };

    let snapshot = complete(&["worktree", "dirty-snapshot", ""], "3");
    assert!(has(&snapshot, "--format"));
    assert!(!has(&snapshot, "--challenge"));

    let adoption = complete(&["worktree", "adopt-dirty", ""], "3");
    for expected in ["--challenge", "--reason-file", "--format"] {
        assert!(has(&adoption, expected), "missing {expected}: {adoption:?}");
    }
    assert!(!has(&adoption, "--receipt"));

    let revocation = complete(&["worktree", "revoke-dirty", ""], "3");
    for expected in ["--receipt", "--format"] {
        assert!(
            has(&revocation, expected),
            "missing {expected}: {revocation:?}"
        );
    }
    assert!(!has(&revocation, "--reason-file"));

    assert_eq!(
        complete(&["worktree", "adopt-dirty", "--format", ""], "4"),
        ["text", "json"]
    );
    let reason_files = complete(&["worktree", "adopt-dirty", "--reason-file", "r"], "4");
    assert!(has(&reason_files, "reason.txt"));
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

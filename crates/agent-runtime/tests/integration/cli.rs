use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
use pretty_assertions::{assert_eq, assert_ne};
use std::path::PathBuf;

fn agent_runtime_bin() -> PathBuf {
    bin::resolve("agent-runtime")
}

fn run(args: &[&str]) -> CmdOutput {
    let bin = agent_runtime_bin();
    cmd::run(&bin, args, &[], None)
}

const ALL_SUBCOMMANDS: &[&str] = &[
    "render",
    "install",
    "uninstall",
    "doctor",
    "bootstrap-host",
    "audit-drift",
    "gc-backups",
    "list-skills",
    "pr-body",
    "prune-stale",
    "restore-backups",
    "purge-state",
];

/// Subcommands whose body still prints `not implemented` and exits 1.
/// All root `agent-runtime` subcommands have an owned body as of Plan 04
/// Sprint 3 Task 3.1.
const STUB_SUBCOMMANDS: &[&str] = &[];

#[test]
fn version_prints_workspace_version() {
    let output = run(&["--version"]);
    assert_eq!(output.code, 0);
    let stdout = output.stdout_text();
    let expected = env!("CARGO_PKG_VERSION");
    assert!(
        stdout.contains(expected),
        "version output should include {expected}: {stdout}"
    );
}

#[test]
fn short_version_prints_clean_workspace_version() {
    let output = run(&["-V"]);
    assert_eq!(output.code, 0);

    let expected = format!("agent-runtime {}\n", env!("CARGO_PKG_VERSION"));
    assert_eq!(output.stdout_text(), expected);
}

#[test]
fn long_version_prints_build_metadata() {
    let output = run(&["--version"]);
    assert_eq!(output.code, 0);

    let stdout = output.stdout_text();
    assert!(
        stdout.starts_with(&format!("agent-runtime {} (", env!("CARGO_PKG_VERSION"))),
        "long version should start with semver and metadata paren: {stdout}"
    );
    assert!(
        stdout.contains(nils_build_info::GIT_DESCRIBE),
        "long version should include git describe token {}: {stdout}",
        nils_build_info::GIT_DESCRIBE
    );
    assert!(
        stdout.contains("rustc "),
        "long version should include rustc version: {stdout}"
    );
}

#[test]
fn help_lists_every_subcommand() {
    let output = run(&["--help"]);
    assert_eq!(output.code, 0);
    let stdout = output.stdout_text();
    for sub in ALL_SUBCOMMANDS {
        assert!(
            stdout.contains(sub),
            "help should list `{sub}` subcommand: {stdout}"
        );
    }
}

#[test]
fn bootstrap_host_help_lists_setup_flags() {
    let output = run(&["bootstrap-host", "--help"]);
    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    let stdout = output.stdout_text();
    for flag in [
        "--dry-run",
        "--apply",
        "--profile",
        "--source-root",
        "--backup-root",
        "--skip-homebrew-install",
        "--skip-cli-tools",
        "--product",
        "--format",
    ] {
        assert!(stdout.contains(flag), "help should list `{flag}`: {stdout}");
    }
}

#[test]
fn every_stub_subcommand_exits_one_with_not_implemented_stderr() {
    for sub in STUB_SUBCOMMANDS {
        let output = run(&[sub]);
        assert_eq!(output.code, 1, "subcommand `{sub}` should exit 1");
        let stderr = output.stderr_text();
        let expected = format!("agent-runtime {sub}: not implemented");
        assert!(
            stderr.contains(&expected),
            "subcommand `{sub}` stderr should contain `{expected}`: {stderr}"
        );
    }
}

#[test]
fn unknown_subcommand_exits_nonzero() {
    let output = run(&["does-not-exist"]);
    assert_ne!(output.code, 0);
}

use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOutput};
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
    "audit-drift",
    "gc-backups",
    "restore-backups",
    "purge-state",
];

/// Subcommands whose body still prints `not implemented` and exits 1.
/// `render` and `install` are no longer stubs once Plan 04 Sprint 1
/// Tasks 1.1 / 1.2 land; later sprints peel further entries off this
/// list.
const STUB_SUBCOMMANDS: &[&str] = &[
    "uninstall",
    "doctor",
    "gc-backups",
    "restore-backups",
    "purge-state",
];

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

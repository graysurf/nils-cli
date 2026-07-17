//! Task 3.1 — the collapsed command surface. `--help` shows only the new
//! commands; the retired commands are gone.

use super::common::{TestEnv, run_cli};
use nils_test_support::cmd;

fn root_help() -> String {
    let temp = tempfile::TempDir::new().unwrap();
    let options = cmd::CmdOptions::default().with_cwd(temp.path());
    let out = run_cli(&["--help"], &options);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    out.stdout
}

#[test]
fn help_lists_only_the_new_surface() {
    let help = root_help();
    for command in [
        "audit",
        "preflight",
        "init",
        "explain",
        "list",
        "remove",
        "config",
        "integration",
        "session",
        "completion",
    ] {
        assert!(help.contains(command), "help missing `{command}`:\n{help}");
    }
}

#[test]
fn retired_commands_are_gone() {
    let help = root_help();
    // These commands must no longer be advertised.
    for retired in [
        "resolve",
        "baseline",
        "scaffold-agents",
        "scaffold-baseline",
        "contexts",
    ] {
        // Use a word-boundary-ish check: the command name as a standalone token
        // in the Commands section. A simple contains is too loose for `resolve`
        // (substring of nothing here) so check the leading two-space indent.
        let token = format!("  {retired}");
        assert!(
            !help.contains(&token),
            "retired command `{retired}` still advertised:\n{help}"
        );
    }
}

#[test]
fn retired_subcommands_exit_usage() {
    let temp = tempfile::TempDir::new().unwrap();
    let options = cmd::CmdOptions::default().with_cwd(temp.path());
    for retired in [
        ["resolve", "--context", "startup"].as_slice(),
        ["baseline", "--check"].as_slice(),
        ["scaffold-baseline"].as_slice(),
        ["add"].as_slice(),
    ] {
        let out = run_cli(retired, &options);
        assert_eq!(
            out.code, 64,
            "retired `{}` should be a usage error: code={} stderr={}",
            retired[0], out.code, out.stderr
        );
    }
}

#[test]
fn private_config_surface_is_structured() {
    let temp = tempfile::TempDir::new().unwrap();
    let options = cmd::CmdOptions::default().with_cwd(temp.path());
    for (args, expected) in [
        (
            ["config", "--help"].as_slice(),
            ["enroll", "exclude", "show", "list", "remove"].as_slice(),
        ),
        (
            ["config", "enroll", "--help"].as_slice(),
            [
                "--catalog",
                "--all-worktrees",
                "--reason",
                "--apply",
                "--format",
            ]
            .as_slice(),
        ),
        (["integration", "--help"].as_slice(), ["resolve"].as_slice()),
        (
            ["integration", "resolve", "--help"].as_slice(),
            ["--product", "codex", "claude", "hermes", "--format"].as_slice(),
        ),
    ] {
        let out = run_cli(args, &options);
        assert_eq!(out.code, 0, "stderr: {}", out.stderr);
        for token in expected {
            assert!(
                out.stdout.contains(token),
                "help for {args:?} missing `{token}`:\n{}",
                out.stdout
            );
        }
    }

    let remove = run_cli(&["config", "remove", "--help"], &options);
    assert_eq!(remove.code, 0, "stderr: {}", remove.stderr);
    assert!(!remove.stdout.contains("--reason"), "{}", remove.stdout);

    let root = root_help();
    for option in ["--user-config", "--integration-fingerprint"] {
        assert!(
            root.contains(option),
            "root help missing `{option}`:\n{root}"
        );
    }
}

#[test]
fn list_dedupes_a_doubly_declared_doc() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n",
    );
    env.write_project_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n",
    );
    env.write_project_doc("DEVELOPMENT.md", "# Dev\n");
    let out = env.run(&["list", "--format", "json"]);
    let json = out.json();
    assert_eq!(
        json["documents"].as_array().unwrap().len(),
        1,
        "doc listed twice:\n{}",
        out.stdout
    );
}

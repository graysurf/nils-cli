//! Task 3.1 — the collapsed command surface. `--help` shows only the new
//! commands; the retired commands are gone.

use std::fs;

use super::common::run_cli;
use nils_test_support::{cmd, git as test_git};

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
            ["--product", "codex", "claude", "hermes", "dsh", "--format"].as_slice(),
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
fn list_dedupes_linked_worktree_documents_with_project_precedence() {
    let temp = tempfile::TempDir::new().unwrap();
    let docs_home =
        test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    let project = temp.path().join("linked");
    test_git::worktree_add_branch(docs_home.path(), &project, "linked");
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&xdg).unwrap();
    fs::write(
        docs_home.path().join("AGENT_DOCS.toml"),
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\nnotes = \"home\"\n",
    )
    .unwrap();
    fs::write(
        project.join("AGENT_DOCS.toml"),
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\nnotes = \"project\"\n",
    )
    .unwrap();
    fs::write(project.join("DEVELOPMENT.md"), "# Dev\n").unwrap();

    let out = run_cli(
        &[
            "--docs-home",
            docs_home.path().to_str().unwrap(),
            "--project-path",
            project.to_str().unwrap(),
            "list",
            "--format",
            "json",
        ],
        &cmd::CmdOptions::default()
            .with_cwd(&project)
            .with_env("HOME", home.to_str().unwrap())
            .with_env("XDG_CONFIG_HOME", xdg.to_str().unwrap())
            .with_env_remove("AGENT_DOCS_HOME")
            .with_env_remove("PROJECT_PATH"),
    );

    assert!(out.success(), "stdout={} stderr={}", out.stdout, out.stderr);
    let documents = out.json()["documents"].as_array().unwrap().clone();
    assert_eq!(documents.len(), 1, "doc listed twice:\n{}", out.stdout);
    assert_eq!(documents[0]["source"], "project");
}

#[test]
fn root_resolution_failures_use_the_invoked_command_schema() {
    let temp = tempfile::TempDir::new().unwrap();
    let project = temp.path().join("project");
    let missing_docs_home = temp.path().join("missing-docs-home");
    let state_home = temp.path().join("state");
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&state_home).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&xdg).unwrap();
    let options = cmd::CmdOptions::default()
        .with_cwd(&project)
        .with_env("HOME", home.to_str().unwrap())
        .with_env("XDG_CONFIG_HOME", xdg.to_str().unwrap())
        .with_env_remove("AGENT_DOCS_HOME")
        .with_env_remove("PROJECT_PATH");

    for (suffix, schema) in [
        (
            ["audit", "--format", "json"].as_slice(),
            "cli.agent-docs.audit.v2",
        ),
        (
            ["preflight", "--intent", "project-dev", "--format", "json"].as_slice(),
            "cli.agent-docs.preflight.v2",
        ),
        (
            ["explain", "--format", "json"].as_slice(),
            "cli.agent-docs.explain.v1",
        ),
        (
            ["list", "--format", "json"].as_slice(),
            "cli.agent-docs.list.v1",
        ),
        (
            [
                "integration",
                "resolve",
                "--product",
                "claude",
                "--format",
                "json",
            ]
            .as_slice(),
            "cli.agent-docs.integration.resolve.v1",
        ),
        (
            [
                "session",
                "status",
                "--session-id",
                "root-resolution",
                "--product",
                "claude",
                "--state-home",
                state_home.to_str().unwrap(),
                "--format",
                "json",
            ]
            .as_slice(),
            "cli.agent-docs.session.status.v1",
        ),
    ] {
        let mut args = vec![
            "--docs-home",
            missing_docs_home.to_str().unwrap(),
            "--project-path",
            project.to_str().unwrap(),
        ];
        args.extend_from_slice(suffix);
        let out = run_cli(&args, &options);

        assert_eq!(out.code, 4, "stdout={} stderr={}", out.stdout, out.stderr);
        assert!(
            out.stderr.is_empty(),
            "stderr must be empty: {}",
            out.stderr
        );
        let json = out.json();
        assert_eq!(json["schema_version"], schema, "stdout={}", out.stdout);
        assert_eq!(json["ok"], false, "stdout={}", out.stdout);
        assert_eq!(json["error"]["code"], "root-resolution-failed");
    }
}

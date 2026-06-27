use std::fs;
use std::path::Path;

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use nils_test_support::git::{git, init_repo_main};
use pretty_assertions::assert_eq;

fn run(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    run_with_env_remove(dir, args, envs, &[])
}

fn run_with_env_remove(
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    remove_envs: &[&str],
) -> CmdOutput {
    let options = CmdOptions::new()
        .with_cwd(dir)
        .with_env_remove_many(remove_envs)
        .with_envs(envs);
    run_resolved("agent-out", args, &options)
}

#[test]
fn project_outputs_path_without_creating_directory_by_default() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let repo = init_repo_main();
    git(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/sympoies/nils-cli.git",
        ],
    );

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let repo_arg = repo.path().to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "project",
            "--topic",
            "Bug Sweep!",
            "--repo",
            &repo_arg,
            "--agent-home",
            &agent_home_arg,
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let path = output.stdout_text();
    let path = path.trim();
    assert!(
        path.starts_with(&format!(
            "{}/out/projects/sympoies__nils-cli/",
            agent_home.display()
        )),
        "unexpected path: {path}"
    );
    assert!(
        path.ends_with("-bug-sweep"),
        "topic was not sanitized in path: {path}"
    );
    assert!(
        !Path::new(path).exists(),
        "--mkdir was not passed, path should not exist"
    );
}

#[test]
fn project_mkdir_creates_generated_directory() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let agent_home_arg = agent_home.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "project",
            "--topic",
            "Artifacts",
            "--repo-slug",
            "owner/repo",
            "--agent-home",
            &agent_home_arg,
            "--mkdir",
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let path = output.stdout_text();
    let path = Path::new(path.trim());
    assert!(path.is_dir(), "expected --mkdir to create {path:?}");
}

#[test]
fn project_json_uses_versioned_envelope() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let agent_home_arg = agent_home.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "project",
            "--topic",
            "API Smoke",
            "--repo-slug",
            "sympoies/nils-cli",
            "--agent-home",
            &agent_home_arg,
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-out.project.v1");
    assert_eq!(value["command"], "agent-out project");
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["project_slug"], "sympoies__nils-cli");
    assert_eq!(value["result"]["topic"], "api-smoke");
    assert_eq!(value["result"]["created"], false);
    let run_id = value["result"]["run_id"].as_str().expect("run_id");
    assert!(
        run_id.starts_with("20") && run_id.ends_with("-api-smoke") && run_id.len() == 15 + 1 + 9,
        "unexpected run_id shape: {run_id}"
    );
}

#[test]
fn project_env_outputs_shell_assignments() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent home");
    let agent_home_arg = agent_home.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "project",
            "--topic",
            "Env Mode",
            "--repo-slug",
            "owner/repo",
            "--agent-home",
            &agent_home_arg,
            "--format",
            "env",
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("AGENT_OUT_PATH='"),
        "missing path assignment: {stdout}"
    );
    assert!(
        stdout.contains("AGENT_OUT_PROJECT_SLUG='owner__repo'"),
        "missing slug assignment: {stdout}"
    );
    assert!(
        stdout.contains("AGENT_OUT_TOPIC='env-mode'"),
        "missing topic assignment: {stdout}"
    );
}

#[test]
fn project_requires_agent_home_from_flag_or_env() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run_with_env_remove(
        tmp.path(),
        &[
            "project",
            "--topic",
            "missing home",
            "--repo-slug",
            "owner/repo",
            "--format",
            "json",
        ],
        &[],
        &["AGENT_HOME"],
    );

    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "missing-agent-home");
    assert!(
        !output.stdout_text().contains("sk-"),
        "json error should not contain secret-like material"
    );
}

#[test]
fn project_uses_agent_home_environment_when_flag_is_absent() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("env-home");
    let agent_home_env = agent_home.to_string_lossy().to_string();
    let envs = [("AGENT_HOME", agent_home_env.as_str())];
    let output = run(
        tmp.path(),
        &[
            "project",
            "--topic",
            "env home",
            "--repo-slug",
            "owner/repo",
        ],
        &envs,
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(
        output
            .stdout_text()
            .starts_with(&format!("{}/out/projects/", agent_home.display())),
        "AGENT_HOME env was not used: {}",
        output.stdout_text()
    );
}

#[test]
fn path_for_supports_state_out_projects_topic_contract() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let repo = init_repo_main();
    git(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/sympoies/nils-cli.git",
        ],
    );

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let repo_arg = repo.path().to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "path-for",
            "--domain",
            "projects",
            "--topic",
            "daily-brief",
            "--repo",
            &repo_arg,
            "--agent-home",
            &agent_home_arg,
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let path = output.stdout_text();
    let path = path.trim();
    assert!(
        path.starts_with(&format!(
            "{}/out/projects/sympoies__nils-cli/",
            agent_home.display()
        )),
        "unexpected path: {path}"
    );
    assert!(
        path.ends_with("-daily-brief"),
        "topic was not preserved in path: {path}"
    );
    assert!(
        !Path::new(path).exists(),
        "--mkdir was not passed, path should not exist"
    );
}

#[test]
fn path_for_accepts_repo_slug_in_repo_flag_and_creates_directory() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let agent_home_arg = agent_home.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "path-for",
            "--domain",
            "tools",
            "--repo",
            "sympoies/nils-cli",
            "--agent-home",
            &agent_home_arg,
            "--mkdir",
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-out.path-for.v1");
    assert_eq!(value["command"], "agent-out path-for");
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["domain"], "tools");
    assert_eq!(value["result"]["project_slug"], "sympoies__nils-cli");
    assert_eq!(value["result"]["topic"], "tools");
    assert_eq!(value["result"]["created"], true);
    let path = value["result"]["path"].as_str().expect("path");
    assert!(
        Path::new(path).is_dir(),
        "expected --mkdir to create {path}"
    );
}

#[test]
fn audit_separates_allowlisted_roots_from_violations() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let out = agent_home.join("out");
    fs::create_dir_all(out.join("projects")).expect("projects");
    fs::create_dir_all(out.join("plan-issue-delivery")).expect("plan root");
    fs::create_dir_all(out.join("random-scratch")).expect("scratch");
    fs::write(out.join("loose.txt"), "debug").expect("loose file");

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &["audit", "--agent-home", &agent_home_arg, "--format", "json"],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.agent-out.audit.v1");
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["summary"]["allowed_roots"], 2);
    assert_eq!(value["result"]["summary"]["violations"], 2);
}

#[test]
fn audit_strict_exits_nonzero_with_json_error_details() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let out = agent_home.join("out");
    fs::create_dir_all(out.join("scratch")).expect("scratch");

    let agent_home_arg = agent_home.to_string_lossy().to_string();
    let output = run(
        tmp.path(),
        &[
            "audit",
            "--agent-home",
            &agent_home_arg,
            "--strict",
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "audit-violations");
    assert_eq!(value["error"]["details"]["summary"]["violations"], 1);
}

#[test]
fn audit_missing_out_root_is_ok() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let agent_home = tmp.path().join("agent-home");
    let agent_home_arg = agent_home.to_string_lossy().to_string();

    let output = run(
        tmp.path(),
        &[
            "audit",
            "--agent-home",
            &agent_home_arg,
            "--strict",
            "--format",
            "json",
        ],
        &[],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["result"]["out_root_exists"], false);
    assert_eq!(value["result"]["summary"]["violations"], 0);
}

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["completion", "zsh"], &[]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(
        output.stdout_text().contains("#compdef agent-out"),
        "missing completion header: {}",
        output.stdout_text()
    );
}

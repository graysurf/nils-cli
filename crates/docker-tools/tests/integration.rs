use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::{StubBinDir, bin, write_exe};
use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn docker_tools_bin() -> PathBuf {
    bin::resolve("docker-tools")
}

fn run(args: &[&str], options: &CmdOptions) -> CmdOutput {
    cmd::run_with(&docker_tools_bin(), args, options)
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(output.code, code, "stderr: {}", output.stderr_text());
}

fn options_in(dir: &Path) -> CmdOptions {
    CmdOptions::default()
        .with_cwd(dir)
        .with_env_remove("ZSH_DOCKER_COMPOSE_CMD")
}

fn docker_stub(dir: &Path, body: &str) {
    write_exe(
        dir,
        "docker",
        &format!(
            r#"#!/bin/bash
set -euo pipefail
{body}
"#
        ),
    );
}

fn append_args_stub() -> &'static str {
    r#"printf '%s\n' "$*" >> "${DOCKER_LOG:?}""#
}

#[test]
fn main_no_args_prints_help_and_exits_zero() {
    let tmp = TempDir::new().expect("tempdir");
    let output = run(&[], &options_in(tmp.path()));

    assert_exit(&output, 0);
    let stdout = output.stdout_text();
    assert!(stdout.contains("Docker helper CLI"));
    assert!(stdout.contains("container"));
    assert!(stdout.contains("completion"));
}

#[test]
fn main_unknown_command_exits_64() {
    let tmp = TempDir::new().expect("tempdir");
    let output = run(&["nope"], &options_in(tmp.path()));

    assert_exit(&output, 64);
    assert!(output.stderr_text().contains("unrecognized subcommand"));
}

#[test]
fn completion_exports_bash_and_zsh_scripts() {
    let tmp = TempDir::new().expect("tempdir");

    let zsh = run(&["completion", "zsh"], &options_in(tmp.path()));
    assert_exit(&zsh, 0);
    let zsh_text = zsh.stdout_text();
    assert!(zsh_text.contains("#compdef docker-tools"));
    assert!(zsh_text.contains("container:Container helper commands"));

    let bash = run(&["completion", "bash"], &options_in(tmp.path()));
    assert_exit(&bash, 0);
    let bash_text = bash.stdout_text();
    assert!(bash_text.contains("_docker__tools()"));
    assert!(bash_text.contains("complete -F _docker__tools"));
}

#[test]
fn container_shell_prefers_probe_shell_command() {
    let tmp = TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let log = tmp.path().join("docker.log");
    fs::write(&log, "").expect("log");
    docker_stub(stubs.path(), append_args_stub());

    let output = run(
        &["container", "sh", "--root", "web"],
        &options_in(tmp.path())
            .with_path_prepend(stubs.path())
            .with_env("DOCKER_LOG", &log.to_string_lossy()),
    );

    assert_exit(&output, 0);
    let log_text = fs::read_to_string(&log).expect("read log");
    assert!(log_text.contains("exec -it -u root -- web sh -c"));
    assert!(log_text.contains("command -v zsh"));
}

#[test]
fn container_rm_defaults_to_force_and_can_remove_volumes() {
    let tmp = TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let log = tmp.path().join("docker.log");
    fs::write(&log, "").expect("log");
    docker_stub(stubs.path(), append_args_stub());

    let output = run(
        &["container", "rm", "-v", "old-one", "old-two"],
        &options_in(tmp.path())
            .with_path_prepend(stubs.path())
            .with_env("DOCKER_LOG", &log.to_string_lossy()),
    );

    assert_exit(&output, 0);
    assert_eq!(
        fs::read_to_string(&log).expect("read log"),
        "container rm -f -v -- old-one old-two\n"
    );
}

#[test]
fn compose_down_uses_docker_compose_v2_when_available() {
    let tmp = TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let log = tmp.path().join("docker.log");
    fs::write(&log, "").expect("log");
    docker_stub(
        stubs.path(),
        r#"if [[ "$*" == "compose version" ]]; then
  exit 0
fi
printf '%s\n' "$*" >> "${DOCKER_LOG:?}"
"#,
    );

    let output = run(
        &["compose", "down", "--all", "--yes", "--timeout", "10"],
        &options_in(tmp.path())
            .with_path_prepend(stubs.path())
            .with_env("DOCKER_LOG", &log.to_string_lossy()),
    );

    assert_exit(&output, 0);
    assert_eq!(
        fs::read_to_string(&log).expect("read log"),
        "compose down --remove-orphans --volumes --rmi all --timeout 10\n"
    );
}

#[test]
fn compose_down_falls_back_to_docker_compose_binary() {
    let tmp = TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let log = tmp.path().join("compose.log");
    fs::write(&log, "").expect("log");
    docker_stub(
        stubs.path(),
        r#"if [[ "$*" == "compose version" ]]; then
  exit 1
fi
exit 1
"#,
    );
    write_exe(
        stubs.path(),
        "docker-compose",
        r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${COMPOSE_LOG:?}"
"#,
    );

    let output = run(
        &["compose", "down"],
        &options_in(tmp.path())
            .with_path_prepend(stubs.path())
            .with_env("COMPOSE_LOG", &log.to_string_lossy()),
    );

    assert_exit(&output, 0);
    assert_eq!(fs::read_to_string(&log).expect("read log"), "down\n");
}

#[test]
fn compose_override_is_split_like_shell_words() {
    let tmp = TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let log = tmp.path().join("custom.log");
    fs::write(&log, "").expect("log");
    write_exe(
        stubs.path(),
        "custom-compose",
        r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "${COMPOSE_LOG:?}"
"#,
    );

    let output = run(
        &["compose", "down", "--yes"],
        &options_in(tmp.path())
            .with_path_prepend(stubs.path())
            .with_env(
                "ZSH_DOCKER_COMPOSE_CMD",
                "custom-compose --context 'dev box'",
            )
            .with_env("COMPOSE_LOG", &log.to_string_lossy()),
    );

    assert_exit(&output, 0);
    assert_eq!(
        fs::read_to_string(&log).expect("read log"),
        "--context dev box down\n"
    );
}

#[test]
fn compose_down_all_without_yes_is_noninteractive_usage_guard() {
    let tmp = TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    docker_stub(stubs.path(), append_args_stub());

    let output = run(
        &["compose", "down", "--all"],
        &options_in(tmp.path()).with_path_prepend(stubs.path()),
    );

    assert_exit(&output, 2);
    assert!(
        output
            .stderr_text()
            .contains("--all requires --yes in non-interactive shells")
    );
}

#[test]
fn run_zsh_mounts_pwd_and_applies_options() {
    let tmp = TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let log = tmp.path().join("docker.log");
    fs::write(&log, "").expect("log");
    docker_stub(stubs.path(), append_args_stub());

    let output = run(
        &[
            "run",
            "zsh",
            "--name",
            "try-it",
            "--user",
            "1000:1000",
            "ubuntu:latest",
        ],
        &options_in(tmp.path())
            .with_path_prepend(stubs.path())
            .with_env("DOCKER_LOG", &log.to_string_lossy()),
    );

    assert_exit(&output, 0);
    let cwd = tmp.path().canonicalize().expect("canonical cwd");
    let expected_prefix = format!(
        "run --rm -it --name try-it -u 1000:1000 -v {}:/work -w /work -- ubuntu:latest sh -c",
        cwd.to_string_lossy()
    );
    let log_text = fs::read_to_string(&log).expect("read log");
    assert!(
        log_text.starts_with(&expected_prefix),
        "log was: {log_text}"
    );
    assert!(log_text.contains("command -v zsh"));
}

#[test]
fn run_zsh_can_skip_mount_and_use_custom_workdir() {
    let tmp = TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let log = tmp.path().join("docker.log");
    fs::write(&log, "").expect("log");
    docker_stub(stubs.path(), append_args_stub());

    let output = run(
        &[
            "run",
            "zsh",
            "--no-mount",
            "--workdir",
            "/src",
            "--root",
            "alpine:latest",
        ],
        &options_in(tmp.path())
            .with_path_prepend(stubs.path())
            .with_env("DOCKER_LOG", &log.to_string_lossy()),
    );

    assert_exit(&output, 0);
    let log_text = fs::read_to_string(&log).expect("read log");
    assert!(log_text.contains("run --rm -it -u root -w /src -- alpine:latest sh -c"));
    assert!(!log_text.contains(":/work"));
}

use std::fs;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;

use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::fs as test_fs;
use nils_test_support::git::{InitRepoOptions, git, init_repo_at_with};
use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir, bin, prepend_path};
use opencode_cli::agent;
use pretty_assertions::assert_eq;

fn opencode_cli_bin() -> PathBuf {
    bin::resolve("opencode-cli")
}

fn run(args: &[&str], options: &CmdOptions) -> CmdOutput {
    cmd::run_with(&opencode_cli_bin(), args, options)
}

fn clean_options() -> CmdOptions {
    CmdOptions::default()
        .with_env_remove("OPENCODE_CLI_MODEL")
        .with_env_remove("OPENCODE_CLI_VARIANT")
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(
        output.code,
        code,
        "unexpected exit code\nstdout:\n{}\nstderr:\n{}",
        output.stdout_text(),
        output.stderr_text()
    );
}

fn write_opencode_arg_file_stub(stub_dir: &Path, out_dir: &Path) {
    fs::create_dir_all(stub_dir).expect("stub dir");
    fs::create_dir_all(out_dir).expect("out dir");
    test_fs::write_executable(
        &stub_dir.join("opencode"),
        r#"#!/usr/bin/env bash
set -euo pipefail
out_dir="${OPENCODE_STUB_OUT_DIR:?}"
mkdir -p "$out_dir"
i=0
for arg in "$@"; do
  printf '%s' "$arg" > "$out_dir/arg-$i"
  i=$((i+1))
done
exit 0
"#,
    );
}

fn read_arg(out_dir: &Path, index: usize) -> String {
    fs::read_to_string(out_dir.join(format!("arg-{index}"))).expect("read arg")
}

fn read_args(out_dir: &Path) -> Vec<String> {
    let mut args = Vec::new();
    for index in 0.. {
        let path = out_dir.join(format!("arg-{index}"));
        if !path.is_file() {
            break;
        }
        args.push(fs::read_to_string(path).expect("read arg"));
    }
    args
}

#[test]
fn agent_prompt_execs_opencode_with_expected_args() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let out_dir = dir.path().join("out");
    write_opencode_arg_file_stub(stubs.path(), &out_dir);

    let out_dir_value = out_dir.to_string_lossy().to_string();
    let _path = prepend_path(&lock, stubs.path());
    let _model = EnvGuard::set(&lock, "OPENCODE_CLI_MODEL", "m-test");
    let _variant = EnvGuard::set(&lock, "OPENCODE_CLI_VARIANT", "v-test");
    let _out = EnvGuard::set(&lock, "OPENCODE_STUB_OUT_DIR", &out_dir_value);

    let mut stdin = BufReader::new(Cursor::new(""));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = agent::prompt_with_io(
        &["hello".into(), "world".into()],
        &mut stdin,
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(read_arg(&out_dir, 0), "run");
    assert_eq!(read_arg(&out_dir, 1), "-m");
    assert_eq!(read_arg(&out_dir, 2), "m-test");
    assert_eq!(read_arg(&out_dir, 3), "--variant");
    assert_eq!(read_arg(&out_dir, 4), "v-test");
    assert_eq!(read_arg(&out_dir, 5), "--title");
    assert_eq!(read_arg(&out_dir, 6), "opencode-tools:prompt");
    assert_eq!(read_arg(&out_dir, 7), "--");
    assert_eq!(read_arg(&out_dir, 8), "hello world");
}

#[test]
fn agent_prompt_reads_stdin_when_no_args() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let out_dir = dir.path().join("out");
    write_opencode_arg_file_stub(stubs.path(), &out_dir);

    let out_dir_value = out_dir.to_string_lossy().to_string();
    let _path = prepend_path(&lock, stubs.path());
    let _model = EnvGuard::remove(&lock, "OPENCODE_CLI_MODEL");
    let _variant = EnvGuard::remove(&lock, "OPENCODE_CLI_VARIANT");
    let _out = EnvGuard::set(&lock, "OPENCODE_STUB_OUT_DIR", &out_dir_value);

    let mut stdin = BufReader::new(Cursor::new("from stdin\n"));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = agent::prompt_with_io(&[], &mut stdin, &mut stdout, &mut stderr);

    assert_eq!(code, 0);
    assert_eq!(String::from_utf8_lossy(&stdout), "Prompt: ");
    assert!(stderr.is_empty());
    assert_eq!(read_arg(&out_dir, 4), "from stdin");
}

#[test]
fn agent_advice_substitutes_arguments() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let zdotdir = dir.path().join("zdotdir");
    let prompts = zdotdir.join("prompts");
    fs::create_dir_all(&prompts).expect("prompts dir");
    fs::write(prompts.join("actionable-advice.md"), "Advice: $ARGUMENTS\n").expect("template");

    let stub_dir = dir.path().join("bin");
    let out_dir = dir.path().join("out");
    write_opencode_arg_file_stub(&stub_dir, &out_dir);

    let output = run(
        &["agent", "advice", "hello", "world"],
        &clean_options()
            .with_path_prepend(&stub_dir)
            .with_env("ZDOTDIR", &zdotdir.to_string_lossy())
            .with_env("OPENCODE_STUB_OUT_DIR", &out_dir.to_string_lossy()),
    );

    assert_exit(&output, 0);
    assert_eq!(read_arg(&out_dir, 4), "Advice: hello world\n");
}

#[test]
fn agent_knowledge_missing_template_prints_error_prefix() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let zdotdir = dir.path().join("zdotdir");
    fs::create_dir_all(zdotdir.join("prompts")).expect("prompts dir");

    let output = run(
        &["agent", "knowledge", "x"],
        &clean_options().with_env("ZDOTDIR", &zdotdir.to_string_lossy()),
    );

    assert_exit(&output, 1);
    assert!(
        output
            .stderr_text()
            .contains("opencode-tools: prompt template not found:")
    );
}

#[test]
fn agent_commit_uses_semantic_commit_prompt_and_extra_instructions() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo_at_with(
        &repo,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    fs::write(repo.join("src.txt"), "changed\n").expect("change");
    git(&repo, &["add", "src.txt"]);

    let zdotdir = dir.path().join("zdotdir");
    let prompts = zdotdir.join("prompts");
    fs::create_dir_all(&prompts).expect("prompts dir");
    fs::write(prompts.join("semantic-commit-staged.md"), "Commit prompt\n").expect("template");

    let stub_dir = dir.path().join("bin");
    let out_dir = dir.path().join("out");
    write_opencode_arg_file_stub(&stub_dir, &out_dir);
    test_fs::write_executable(&stub_dir.join("semantic-commit"), "#!/bin/sh\nexit 0\n");

    let output = run(
        &["agent", "commit", "-p", "Prefer terse subject"],
        &clean_options()
            .with_cwd(&repo)
            .with_path_prepend(&stub_dir)
            .with_env("ZDOTDIR", &zdotdir.to_string_lossy())
            .with_env("OPENCODE_STUB_OUT_DIR", &out_dir.to_string_lossy()),
    );

    assert_exit(&output, 0);
    let args = read_args(&out_dir);
    assert!(args.contains(&"opencode-tools:commit-with-scope".to_string()));
    let prompt = args.last().expect("prompt arg");
    assert!(prompt.contains("Commit prompt"));
    assert!(prompt.contains("push the committed changes"));
    assert!(prompt.contains("Additional instructions from user:"));
    assert!(prompt.contains("Prefer terse subject"));
}

#[test]
fn main_help_unknown_and_completion_paths_are_stable() {
    let output = run(&[], &CmdOptions::default());
    assert_exit(&output, 0);
    assert!(output.stdout_text().contains("opencode-cli"));

    let output = run(&["agent"], &clean_options());
    assert_exit(&output, 0);
    assert!(output.stdout_text().contains("Agent command group"));

    let output = run(&["not-a-real-command"], &clean_options());
    assert_exit(&output, 64);
    assert!(output.stderr_text().contains("unrecognized subcommand"));

    let zsh = run(&["completion", "zsh"], &clean_options());
    assert_exit(&zsh, 0);
    assert!(zsh.stdout_text().contains("#compdef opencode-cli"));

    let bash = run(&["completion", "bash"], &clean_options());
    assert_exit(&bash, 0);
    assert!(bash.stdout_text().contains("_opencode-cli()"));
    assert!(bash.stdout_text().contains("complete -F _opencode-cli"));
}

#[test]
fn published_binary_starts_without_opencode_for_help() {
    let status = Command::new(opencode_cli_bin())
        .arg("--help")
        .status()
        .expect("run help");
    assert!(status.success());
}

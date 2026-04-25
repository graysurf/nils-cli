use nils_common::process as shared_process;
use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::fs as test_fs;
use nils_test_support::git as test_git;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn stdout(output: &CmdOutput) -> String {
    output.stdout_text()
}

fn stderr(output: &CmdOutput) -> String {
    output.stderr_text()
}

fn assert_exit(output: &CmdOutput, code: i32) {
    assert_eq!(
        output.code,
        code,
        "unexpected exit code.\nstdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
}

fn real_git_path() -> String {
    shared_process::find_in_path("git")
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| panic!("git not found in PATH for tests"))
}

fn write_stub_git(dir: &Path) {
    let git = real_git_path();
    let script = format!(
        r#"#!/bin/sh
exec "{git}" "$@"
"#
    );
    let path = dir.join("git");
    test_fs::write_executable(&path, &script);
}

fn write_stub_semantic_commit(dir: &Path) {
    let script = r#"#!/bin/sh
exit 0
"#;
    let path = dir.join("semantic-commit");
    test_fs::write_executable(&path, script);
}

fn write_stub_codex(dir: &Path) {
    let script = r#"#!/bin/bash
set -euo pipefail
out_dir="${CODEX_STUB_OUT_DIR:?missing CODEX_STUB_OUT_DIR}"
i=0
for arg in "$@"; do
  printf '%s' "$arg" > "$out_dir/arg-$i"
  i=$((i+1))
done
"#;
    let path = dir.join("codex");
    test_fs::write_executable(&path, script);
}

fn init_repo(dir: &Path) {
    test_git::git(dir, &["init"]);
    test_git::git(dir, &["config", "user.name", "Test User"]);
    test_git::git(dir, &["config", "user.email", "test@example.com"]);
    test_git::git(dir, &["config", "commit.gpgsign", "false"]);
}

#[test]
fn agent_commit_fallback_creates_commit() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo(&repo);

    fs::write(repo.join("a.txt"), "hello").expect("write file");
    test_git::git(&repo, &["add", "a.txt"]);

    let stub_dir = dir.path().join("bin");
    fs::create_dir_all(&stub_dir).expect("stub dir");
    write_stub_git(&stub_dir);

    let stub_path = stub_dir.to_string_lossy().to_string();
    let options = CmdOptions::default()
        .with_cwd(&repo)
        .with_env("PATH", &stub_path)
        .with_stdin_bytes(b"\n\nmy subject\ny\n");
    let bin = codex_cli_bin();
    let output = cmd::run_with(&bin, &["agent", "commit"], &options);
    assert_exit(&output, 0);
    assert!(stderr(&output).contains("fallback mode"));

    assert_eq!(
        test_git::git(&repo, &["log", "-1", "--pretty=%s"]).trim(),
        "chore: my subject"
    );
}

#[test]
fn agent_commit_semantic_mode_executes_codex_with_template_and_push_note() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    init_repo(&repo);

    fs::write(repo.join("a.txt"), "hello").expect("write file");

    let zdotdir = dir.path().join("zdotdir");
    let prompts_dir = zdotdir.join("prompts");
    fs::create_dir_all(&prompts_dir).expect("prompts dir");
    fs::write(
        prompts_dir.join("semantic-commit-autostage.md"),
        "SEMANTIC_AUTOSTAGE\n",
    )
    .expect("write template");

    let stub_dir = dir.path().join("bin");
    fs::create_dir_all(&stub_dir).expect("stub dir");
    write_stub_git(&stub_dir);
    write_stub_semantic_commit(&stub_dir);
    write_stub_codex(&stub_dir);

    let out_dir = dir.path().join("out");
    fs::create_dir_all(&out_dir).expect("out dir");
    let out_dir_str = out_dir.to_string_lossy().to_string();
    let stub_path = stub_dir.to_string_lossy().to_string();
    let zdotdir_str = zdotdir.to_string_lossy().to_string();
    let options = CmdOptions::default()
        .with_cwd(&repo)
        .with_env("PATH", &stub_path)
        .with_env("CODEX_ALLOW_DANGEROUS_ENABLED", "true")
        .with_env("CODEX_CLI_MODEL", "m-test")
        .with_env("CODEX_CLI_REASONING", "low")
        .with_env("ZDOTDIR", &zdotdir_str)
        .with_env("CODEX_STUB_OUT_DIR", &out_dir_str);
    let bin = codex_cli_bin();
    let output = cmd::run_with(
        &bin,
        &[
            "agent",
            "commit",
            "--ephemeral",
            "--push",
            "--auto-stage",
            "extra",
            "words",
        ],
        &options,
    );
    assert_exit(&output, 0);

    let ephemeral = fs::read_to_string(out_dir.join("arg-8")).expect("ephemeral");
    assert_eq!(ephemeral, "--ephemeral");
    let prompt = fs::read_to_string(out_dir.join("arg-10")).expect("prompt");
    assert!(prompt.contains("SEMANTIC_AUTOSTAGE"));
    assert!(prompt.contains("Furthermore, please push the committed changes"));
    assert!(prompt.contains("Additional instructions from user:"));
    assert!(prompt.contains("extra words"));

    let arg5 = fs::read_to_string(out_dir.join("arg-5")).expect("model");
    assert_eq!(arg5, "m-test");
    let arg7 = fs::read_to_string(out_dir.join("arg-7")).expect("reasoning");
    assert_eq!(arg7, "model_reasoning_effort=\"low\"");
}

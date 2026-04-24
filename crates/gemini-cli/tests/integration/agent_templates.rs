use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use nils_test_support::fs as test_fs;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::{Path, PathBuf};

fn gemini_cli_bin() -> PathBuf {
    bin::resolve("gemini-cli")
}

fn run(args: &[&str], vars: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default();
    for (key, value) in vars {
        options = options.with_env(key, value);
    }
    let bin = gemini_cli_bin();
    cmd::run_with(&bin, args, &options)
}

fn run_with_path_prepend(args: &[&str], vars: &[(&str, &str)], path_prepend: &Path) -> CmdOutput {
    let mut options = CmdOptions::default().with_path_prepend(path_prepend);
    for (key, value) in vars {
        options = options.with_env(key, value);
    }
    let bin = gemini_cli_bin();
    cmd::run_with(&bin, args, &options)
}

fn run_with_stdin(args: &[&str], vars: &[(&str, &str)], stdin: &str) -> CmdOutput {
    let mut options = CmdOptions::default().with_stdin_str(stdin);
    for (key, value) in vars {
        options = options.with_env(key, value);
    }
    let bin = gemini_cli_bin();
    cmd::run_with(&bin, args, &options)
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

fn write_stub_gemini(stub_dir: &std::path::Path, out_dir: &std::path::Path) {
    fs::create_dir_all(stub_dir).expect("stub dir");
    fs::create_dir_all(out_dir).expect("out dir");
    let script = r#"#!/usr/bin/env bash
set -euo pipefail
out_dir="${GEMINI_STUB_OUT_DIR}"
mkdir -p "$out_dir"
i=0
for arg in "$@"; do
  printf '%s' "$arg" > "$out_dir/arg-$i"
  i=$((i+1))
done
exit 0
"#;
    let path = stub_dir.join("gemini");
    test_fs::write_executable(&path, script);
}

#[test]
fn agent_advice_substitutes_arguments() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let zdotdir = dir.path().join("zdotdir");
    let prompts = zdotdir.join("prompts");
    fs::create_dir_all(&prompts).expect("prompts dir");
    fs::write(prompts.join("actionable-advice.md"), "Advice: $ARGUMENTS\n").expect("template");

    let zdotdir_str = zdotdir.to_string_lossy().to_string();

    let stub_dir = dir.path().join("bin");
    let out_dir = dir.path().join("out");
    write_stub_gemini(&stub_dir, &out_dir);

    let out_dir_str = out_dir.to_string_lossy().to_string();

    let output = run_with_path_prepend(
        &["agent", "advice", "hello", "world"],
        &[
            ("GEMINI_ALLOW_DANGEROUS_ENABLED", "true"),
            ("ZDOTDIR", &zdotdir_str),
            ("GEMINI_STUB_OUT_DIR", &out_dir_str),
        ],
        &stub_dir,
    );
    assert_exit(&output, 0);

    let prompt_arg = fs::read_to_string(out_dir.join("arg-0")).expect("prompt arg");
    assert_eq!(prompt_arg, "--prompt=Advice: hello world\n");
}

#[test]
fn agent_knowledge_missing_template_prints_error_prefix() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let zdotdir = dir.path().join("zdotdir");
    let prompts = zdotdir.join("prompts");
    fs::create_dir_all(&prompts).expect("prompts dir");

    let zdotdir_str = zdotdir.to_string_lossy().to_string();

    let output = run(
        &["agent", "knowledge", "x"],
        &[
            ("GEMINI_ALLOW_DANGEROUS_ENABLED", "true"),
            ("ZDOTDIR", &zdotdir_str),
        ],
    );
    assert_eq!(output.code, 1);
    assert!(stderr(&output).contains("gemini-tools: prompt template not found:"));
}

#[test]
fn agent_advice_blank_question_exits_1_with_missing_question_message() {
    let output = run_with_stdin(
        &["agent", "advice"],
        &[("GEMINI_ALLOW_DANGEROUS_ENABLED", "true")],
        "\n",
    );
    assert_exit(&output, 1);
    assert!(stdout(&output).contains("Question: "));
    assert!(stderr(&output).contains("gemini-tools: missing question"));
}

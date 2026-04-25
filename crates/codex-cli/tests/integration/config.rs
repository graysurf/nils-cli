use nils_test_support::bin;
use nils_test_support::cmd::{self, CmdOptions, CmdOutput};
use pretty_assertions::assert_eq;
use std::path::PathBuf;

fn codex_cli_bin() -> PathBuf {
    bin::resolve("codex-cli")
}

fn run(args: &[&str], vars: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default();
    for (key, value) in vars {
        options = options.with_env(key, value);
    }
    let bin = codex_cli_bin();
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

#[test]
fn config_show_prints_effective_values() {
    let output = run(
        &["config", "show"],
        &[
            ("CODEX_CLI_MODEL", "m1"),
            ("CODEX_CLI_REASONING", "low"),
            ("CODEX_CLI_EPHEMERAL_ENABLED", "true"),
            ("CODEX_ALLOW_DANGEROUS_ENABLED", "true"),
            ("CODEX_SECRET_DIR", "/tmp/secrets"),
            ("CODEX_AUTH_FILE", "/tmp/auth.json"),
            ("CODEX_SECRET_CACHE_DIR", "/tmp/cache/secrets"),
            ("CODEX_PROMPT_SEGMENT_ENABLED", "true"),
            ("CODEX_AUTO_REFRESH_ENABLED", "true"),
            ("CODEX_AUTO_REFRESH_MIN_DAYS", "9"),
        ],
    );
    assert_exit(&output, 0);
    let out = stdout(&output);
    assert!(out.contains("CODEX_CLI_MODEL=m1\n"));
    assert!(out.contains("CODEX_CLI_REASONING=low\n"));
    assert!(out.contains("CODEX_CLI_EPHEMERAL_ENABLED=true\n"));
    assert!(out.contains("CODEX_ALLOW_DANGEROUS_ENABLED=true\n"));
    assert!(out.contains("CODEX_SECRET_DIR=/tmp/secrets\n"));
    assert!(out.contains("CODEX_AUTH_FILE=/tmp/auth.json\n"));
    assert!(out.contains("CODEX_SECRET_CACHE_DIR=/tmp/cache/secrets\n"));
    assert!(out.contains("CODEX_PROMPT_SEGMENT_ENABLED=true\n"));
    assert!(out.contains("CODEX_AUTO_REFRESH_ENABLED=true\n"));
    assert!(out.contains("CODEX_AUTO_REFRESH_MIN_DAYS=9\n"));
}

#[test]
fn config_show_prints_blank_paths_when_unresolvable() {
    let options = CmdOptions::default()
        .with_env_remove("HOME")
        .with_env_remove("ZDOTDIR")
        .with_env_remove("ZSH_SCRIPT_DIR")
        .with_env_remove("_ZSH_BOOTSTRAP_PRELOAD_PATH")
        .with_env_remove("ZSH_CACHE_DIR")
        .with_env_remove("CODEX_SECRET_DIR")
        .with_env_remove("CODEX_AUTH_FILE")
        .with_env_remove("CODEX_SECRET_CACHE_DIR");
    let bin = codex_cli_bin();
    let output = cmd::run_with(&bin, &["config", "show"], &options);
    assert_exit(&output, 0);

    let out = stdout(&output);
    assert!(out.contains("CODEX_SECRET_DIR=\n"));
    assert!(out.contains("CODEX_AUTH_FILE=\n"));
    assert!(out.contains("CODEX_SECRET_CACHE_DIR=\n"));
}

#[test]
fn config_set_model_prints_export() {
    let output = run(&["config", "set", "model", "gpt-test"], &[]);
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "export CODEX_CLI_MODEL='gpt-test'\n");
}

#[test]
fn config_set_reasoning_prints_export() {
    let output = run(&["config", "set", "reasoning", "high"], &[]);
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "export CODEX_CLI_REASONING='high'\n");
}

#[test]
fn config_set_dangerous_prints_export_for_true() {
    let output = run(&["config", "set", "dangerous", "true"], &[]);
    assert_exit(&output, 0);
    assert_eq!(
        stdout(&output),
        "export CODEX_ALLOW_DANGEROUS_ENABLED=true\n"
    );
}

#[test]
fn config_set_ephemeral_prints_export_for_true() {
    let output = run(&["config", "set", "ephemeral", "true"], &[]);
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "export CODEX_CLI_EPHEMERAL_ENABLED=true\n");
}

#[test]
fn config_set_unknown_key_exits_64() {
    let output = run(&["config", "set", "wat", "x"], &[]);
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("unknown key"));
}

#[test]
fn config_set_model_quotes_empty_value() {
    let output = run(&["config", "set", "model", ""], &[]);
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "export CODEX_CLI_MODEL=''\n");
}

#[test]
fn config_set_model_escapes_single_quotes() {
    let output = run(&["config", "set", "model", "a'b"], &[]);
    assert_exit(&output, 0);
    assert_eq!(stdout(&output), "export CODEX_CLI_MODEL='a'\"'\"'b'\n");
}

#[test]
fn config_set_dangerous_rejects_invalid_values() {
    let output = run(&["config", "set", "dangerous", "maybe"], &[]);
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("dangerous must be true|false"));
}

#[test]
fn config_set_ephemeral_rejects_invalid_values() {
    let output = run(&["config", "set", "ephemeral", "maybe"], &[]);
    assert_exit(&output, 64);
    assert!(stderr(&output).contains("ephemeral must be true|false"));
}

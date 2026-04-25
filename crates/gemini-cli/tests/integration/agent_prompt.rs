use gemini_cli::agent;
use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir, prepend_path};
use std::fs;
use std::io::{BufReader, Cursor};
use std::path::Path;

fn write_gemini_stub(stub: &StubBinDir) {
    let script = r#"#!/bin/sh
set -eu
out="${GEMINI_TEST_ARGV_LOG:?missing GEMINI_TEST_ARGV_LOG}"
: > "$out"
for a in "$@"; do
  echo "$a" >> "$out"
done
"#;
    stub.write_exe("gemini", script);
}

fn read_args(log_path: &Path) -> Vec<String> {
    match fs::read_to_string(log_path) {
        Ok(raw) => raw.lines().map(|line| line.to_string()).collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn agent_prompt_requires_dangerous_mode() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let args_log = dir.path().join("argv.log");
    write_gemini_stub(&stubs);

    let args_log_value = args_log.to_string_lossy().to_string();
    let _path = prepend_path(&lock, stubs.path());
    let _danger = EnvGuard::set(&lock, "GEMINI_ALLOW_DANGEROUS_ENABLED", "false");
    let _model = EnvGuard::set(&lock, "GEMINI_CLI_MODEL", "m");
    let _reasoning = EnvGuard::set(&lock, "GEMINI_CLI_REASONING", "low");
    let _argv = EnvGuard::set(&lock, "GEMINI_TEST_ARGV_LOG", &args_log_value);

    let mut stdin = BufReader::new(Cursor::new(""));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = agent::prompt_with_io(&["hello".into()], &mut stdin, &mut stdout, &mut stderr);
    assert_eq!(code, 1);
    assert!(
        String::from_utf8_lossy(&stderr)
            .contains("gemini-tools:prompt: disabled (set GEMINI_ALLOW_DANGEROUS_ENABLED=true)")
    );
    assert!(read_args(&args_log).is_empty());
}

#[test]
fn agent_prompt_execs_gemini_with_expected_args() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let args_log = dir.path().join("argv.log");
    write_gemini_stub(&stubs);

    let args_log_value = args_log.to_string_lossy().to_string();
    let _path = prepend_path(&lock, stubs.path());
    let _danger = EnvGuard::set(&lock, "GEMINI_ALLOW_DANGEROUS_ENABLED", "true");
    let _model = EnvGuard::set(&lock, "GEMINI_CLI_MODEL", "m-test");
    let _reasoning = EnvGuard::set(&lock, "GEMINI_CLI_REASONING", "high");
    let _argv = EnvGuard::set(&lock, "GEMINI_TEST_ARGV_LOG", &args_log_value);

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

    let args = read_args(&args_log);
    assert_eq!(
        args,
        vec![
            "--prompt=hello world",
            "--model",
            "m-test",
            "--approval-mode",
            "yolo",
        ]
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
    );
}

#[test]
fn agent_prompt_reads_stdin_when_no_args() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let stubs = StubBinDir::new();
    let args_log = dir.path().join("argv.log");
    write_gemini_stub(&stubs);

    let args_log_value = args_log.to_string_lossy().to_string();
    let _path = prepend_path(&lock, stubs.path());
    let _danger = EnvGuard::set(&lock, "GEMINI_ALLOW_DANGEROUS_ENABLED", "true");
    let _model = EnvGuard::set(&lock, "GEMINI_CLI_MODEL", "m");
    let _reasoning = EnvGuard::set(&lock, "GEMINI_CLI_REASONING", "medium");
    let _argv = EnvGuard::set(&lock, "GEMINI_TEST_ARGV_LOG", &args_log_value);

    let mut stdin = BufReader::new(Cursor::new("from stdin\n"));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = agent::prompt_with_io(&[], &mut stdin, &mut stdout, &mut stderr);
    assert_eq!(code, 0);
    assert_eq!(String::from_utf8_lossy(&stdout), "Prompt: ");
    assert!(stderr.is_empty());

    let args = read_args(&args_log);
    let prompt_flag = args.first().map(String::as_str).unwrap_or_default();
    assert_eq!(prompt_flag, "--prompt=from stdin");
}

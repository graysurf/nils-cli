use codex_cli::agent;

use std::io::{BufReader, Cursor};

use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir, prepend_path};
use pretty_assertions::assert_eq;
use serde_json::Value;

fn write_codex_stub(stub: &StubBinDir) -> tempfile::NamedTempFile {
    let args_log = tempfile::NamedTempFile::new().expect("args log");
    stub.write_exe(
        "codex",
        r#"#!/bin/bash
set -euo pipefail
out="${CODEX_TEST_ARGV_LOG:?missing CODEX_TEST_ARGV_LOG}"
: > "$out"
for a in "$@"; do
  echo "$a" >> "$out"
done
"#,
    );
    args_log
}

fn read_args(log: &tempfile::NamedTempFile) -> Vec<String> {
    std::fs::read_to_string(log.path())
        .expect("read args")
        .lines()
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn agent_prompt_requires_dangerous_mode() {
    let lock = GlobalStateLock::new();
    let stub = StubBinDir::new();
    let args_log = write_codex_stub(&stub);
    let _path = prepend_path(&lock, stub.path());

    let _danger = EnvGuard::remove(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED");
    let _ephemeral = EnvGuard::remove(&lock, "CODEX_CLI_EPHEMERAL_ENABLED");
    let _model = EnvGuard::set(&lock, "CODEX_CLI_MODEL", "m");
    let _reasoning = EnvGuard::set(&lock, "CODEX_CLI_REASONING", "low");
    let args_log_path = args_log.path().to_string_lossy().to_string();
    let _argv_log = EnvGuard::set(&lock, "CODEX_TEST_ARGV_LOG", &args_log_path);

    let mut stdin = BufReader::new(Cursor::new(""));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = agent::prompt_with_io(
        &["hello".into()],
        agent::exec::ExecOptions::default(),
        &mut stdin,
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(code, 1);
    assert!(
        String::from_utf8_lossy(&stderr)
            .contains("codex-tools:prompt: disabled (set CODEX_ALLOW_DANGEROUS_ENABLED=true)")
    );
    assert!(read_args(&args_log).is_empty());
}

#[test]
fn agent_prompt_execs_codex_with_expected_args() {
    let lock = GlobalStateLock::new();
    let stub = StubBinDir::new();
    let args_log = write_codex_stub(&stub);
    let _path = prepend_path(&lock, stub.path());

    let _danger = EnvGuard::set(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED", "true");
    let _ephemeral = EnvGuard::remove(&lock, "CODEX_CLI_EPHEMERAL_ENABLED");
    let _model = EnvGuard::set(&lock, "CODEX_CLI_MODEL", "gpt-test");
    let _reasoning = EnvGuard::set(&lock, "CODEX_CLI_REASONING", "high");
    let args_log_path = args_log.path().to_string_lossy().to_string();
    let _argv_log = EnvGuard::set(&lock, "CODEX_TEST_ARGV_LOG", &args_log_path);

    let mut stdin = BufReader::new(Cursor::new(""));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = agent::prompt_with_io(
        &["hello".into(), "world".into()],
        agent::exec::ExecOptions::default(),
        &mut stdin,
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    assert_eq!(
        read_args(&args_log),
        vec![
            "exec",
            "--dangerously-bypass-approvals-and-sandbox",
            "-s",
            "workspace-write",
            "-m",
            "gpt-test",
            "-c",
            "model_reasoning_effort=\"high\"",
            "--",
            "hello world",
        ]
        .into_iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
    );
}

#[test]
fn agent_prompt_refreshes_remote_auth_before_codex_exec() {
    let lock = GlobalStateLock::new();
    let stub = StubBinDir::new();
    let args_log = write_codex_stub(&stub);
    let _path = prepend_path(&lock, stub.path());

    let dir = tempfile::TempDir::new().expect("tempdir");
    let auth_file = dir.path().join("auth.json");
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(&cache).expect("cache dir");
    let ssh_args_file = dir.path().join("ssh-args.txt");
    stub.write_exe(
        "ssh",
        r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" > "$SSH_ARGS_FILE"
printf '%s\n' "$REMOTE_AUTH_PAYLOAD"
"#,
    );

    let _danger = EnvGuard::set(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED", "true");
    let _ephemeral = EnvGuard::remove(&lock, "CODEX_CLI_EPHEMERAL_ENABLED");
    let _model = EnvGuard::set(&lock, "CODEX_CLI_MODEL", "gpt-test");
    let _reasoning = EnvGuard::set(&lock, "CODEX_CLI_REASONING", "high");
    let args_log_path = args_log.path().to_string_lossy().to_string();
    let _argv_log = EnvGuard::set(&lock, "CODEX_TEST_ARGV_LOG", &args_log_path);
    let auth_file_path = auth_file.to_string_lossy().to_string();
    let cache_path = cache.to_string_lossy().to_string();
    let ssh_args_path = ssh_args_file.to_string_lossy().to_string();
    let _auth_file = EnvGuard::set(&lock, "CODEX_AUTH_FILE", &auth_file_path);
    let _secret_cache = EnvGuard::set(&lock, "CODEX_SECRET_CACHE_DIR", &cache_path);
    let _remote_ssh = EnvGuard::set(&lock, "CODEX_AUTH_REMOTE_SSH", "auth-host");
    let _remote_name = EnvGuard::set(&lock, "CODEX_AUTH_REMOTE_NAME", "team");
    let _remote_refresh = EnvGuard::set(&lock, "CODEX_AUTH_REMOTE_REFRESH", "true");
    let _remote_payload = EnvGuard::set(
        &lock,
        "REMOTE_AUTH_PAYLOAD",
        r#"{"tokens":{"access_token":"remote-access-token","id_token":"remote-id-token","account_id":"acct_001"},"last_refresh":"2025-01-20T12:34:56Z"}"#,
    );
    let _ssh_args = EnvGuard::set(&lock, "SSH_ARGS_FILE", &ssh_args_path);

    let mut stdin = BufReader::new(Cursor::new(""));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = agent::prompt_with_io(
        &["hello".into(), "world".into()],
        agent::exec::ExecOptions::default(),
        &mut stdin,
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let captured_args = std::fs::read_to_string(&ssh_args_file).expect("read ssh args");
    assert!(captured_args.contains("auth-host"));
    assert!(captured_args.contains("codex-cli auth remote export"));
    assert!(captured_args.contains("--name team"));
    assert!(captured_args.contains("--refresh"));

    let applied: Value =
        serde_json::from_str(&std::fs::read_to_string(&auth_file).expect("read auth"))
            .expect("auth json");
    assert_eq!(applied["tokens"]["access_token"], "remote-access-token");
    assert!(!read_args(&args_log).is_empty());
}

#[test]
fn agent_prompt_execs_codex_with_ephemeral_arg_when_requested() {
    let lock = GlobalStateLock::new();
    let stub = StubBinDir::new();
    let args_log = write_codex_stub(&stub);
    let _path = prepend_path(&lock, stub.path());

    let _danger = EnvGuard::set(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED", "true");
    let _ephemeral = EnvGuard::remove(&lock, "CODEX_CLI_EPHEMERAL_ENABLED");
    let _model = EnvGuard::set(&lock, "CODEX_CLI_MODEL", "gpt-test");
    let _reasoning = EnvGuard::set(&lock, "CODEX_CLI_REASONING", "high");
    let args_log_path = args_log.path().to_string_lossy().to_string();
    let _argv_log = EnvGuard::set(&lock, "CODEX_TEST_ARGV_LOG", &args_log_path);

    let mut stdin = BufReader::new(Cursor::new(""));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = agent::prompt_with_io(
        &["hello".into(), "world".into()],
        agent::exec::ExecOptions { ephemeral: true },
        &mut stdin,
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    assert_eq!(
        read_args(&args_log),
        vec![
            "exec",
            "--dangerously-bypass-approvals-and-sandbox",
            "-s",
            "workspace-write",
            "-m",
            "gpt-test",
            "-c",
            "model_reasoning_effort=\"high\"",
            "--ephemeral",
            "--",
            "hello world",
        ]
        .into_iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
    );
}

#[test]
fn agent_prompt_reads_stdin_when_no_args() {
    let lock = GlobalStateLock::new();
    let stub = StubBinDir::new();
    let args_log = write_codex_stub(&stub);
    let _path = prepend_path(&lock, stub.path());

    let _danger = EnvGuard::set(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED", "true");
    let _ephemeral = EnvGuard::remove(&lock, "CODEX_CLI_EPHEMERAL_ENABLED");
    let _model = EnvGuard::set(&lock, "CODEX_CLI_MODEL", "m");
    let _reasoning = EnvGuard::set(&lock, "CODEX_CLI_REASONING", "medium");
    let args_log_path = args_log.path().to_string_lossy().to_string();
    let _argv_log = EnvGuard::set(&lock, "CODEX_TEST_ARGV_LOG", &args_log_path);

    let mut stdin = BufReader::new(Cursor::new("from stdin\n"));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = agent::prompt_with_io(
        &[],
        agent::exec::ExecOptions::default(),
        &mut stdin,
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(code, 0);
    assert_eq!(String::from_utf8_lossy(&stdout), "Prompt: ");
    assert!(stderr.is_empty());

    let args = read_args(&args_log);
    assert_eq!(args.last().map(|s| s.as_str()), Some("from stdin"));
}

#[test]
fn agent_prompt_empty_stdin_exits_1_without_exec() {
    let lock = GlobalStateLock::new();
    let stub = StubBinDir::new();
    let args_log = write_codex_stub(&stub);
    let _path = prepend_path(&lock, stub.path());

    let mut stdin = BufReader::new(Cursor::new(""));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = agent::prompt_with_io(
        &[],
        agent::exec::ExecOptions::default(),
        &mut stdin,
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(code, 1);
    assert_eq!(String::from_utf8_lossy(&stdout), "Prompt: ");
    assert!(read_args(&args_log).is_empty());
}

#[test]
fn agent_prompt_blank_line_exits_1_with_missing_prompt_message() {
    let lock = GlobalStateLock::new();
    let stub = StubBinDir::new();
    let args_log = write_codex_stub(&stub);
    let _path = prepend_path(&lock, stub.path());

    let mut stdin = BufReader::new(Cursor::new("\n"));
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let code = agent::prompt_with_io(
        &[],
        agent::exec::ExecOptions::default(),
        &mut stdin,
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(code, 1);
    assert_eq!(String::from_utf8_lossy(&stdout), "Prompt: ");
    assert!(String::from_utf8_lossy(&stderr).contains("codex-tools: missing prompt"));
    assert!(read_args(&args_log).is_empty());
}

use codex_cli::runtime;
use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir, prepend_path};
use pretty_assertions::assert_eq;

#[test]
fn runtime_exec_contract_missing_prompt_is_rejected() {
    let mut stderr = Vec::new();
    let code = runtime::exec_dangerous("", "caller", &mut stderr);

    assert_eq!(code, 1);
    assert!(String::from_utf8_lossy(&stderr).contains("_codex_exec_dangerous: missing prompt"));
}

#[test]
fn runtime_exec_contract_disabled_policy_reports_validation_message() {
    let lock = GlobalStateLock::new();
    let _danger = EnvGuard::set(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED", "false");

    let (enabled, message) = runtime::allow_dangerous_status(Some("adapter"));
    assert!(!enabled);
    assert!(
        message
            .expect("message")
            .contains("adapter: disabled (set CODEX_ALLOW_DANGEROUS_ENABLED=true)")
    );

    let check = runtime::check_allow_dangerous(Some("adapter")).expect_err("disabled policy error");
    assert_eq!(check.code, "disabled-policy");
    assert!(!check.retryable);
}

#[test]
fn runtime_exec_contract_missing_binary_fails_without_panicking() {
    let lock = GlobalStateLock::new();
    let _danger = EnvGuard::set(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED", "true");
    let _path = EnvGuard::set(&lock, "PATH", "");

    let mut stderr = Vec::new();
    let code = runtime::exec_dangerous("ping", "caller", &mut stderr);

    assert_eq!(code, 1);
    assert!(String::from_utf8_lossy(&stderr).contains("failed to run codex exec"));
}

#[test]
fn runtime_exec_contract_success_path_uses_expected_command_shape() {
    let lock = GlobalStateLock::new();
    let stub = StubBinDir::new();
    let args_log = tempfile::NamedTempFile::new().expect("args log");
    let args_log_path = args_log.path().to_string_lossy().to_string();

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

    let _path = prepend_path(&lock, stub.path());
    let _danger = EnvGuard::set(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED", "true");
    let _ephemeral = EnvGuard::remove(&lock, "CODEX_CLI_EPHEMERAL_ENABLED");
    let _model = EnvGuard::set(&lock, "CODEX_CLI_MODEL", "gpt-test");
    let _reason = EnvGuard::set(&lock, "CODEX_CLI_REASONING", "high");
    let _argv_log = EnvGuard::set(&lock, "CODEX_TEST_ARGV_LOG", &args_log_path);

    let mut stderr = Vec::new();
    let code = runtime::exec_dangerous("hello world", "caller", &mut stderr);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let args = std::fs::read_to_string(args_log.path())
        .expect("read args")
        .lines()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();

    assert_eq!(
        args,
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
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
    );
}

#[test]
fn runtime_exec_contract_env_ephemeral_adds_exec_flag() {
    let lock = GlobalStateLock::new();
    let stub = StubBinDir::new();
    let args_log = tempfile::NamedTempFile::new().expect("args log");
    let args_log_path = args_log.path().to_string_lossy().to_string();

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

    let _path = prepend_path(&lock, stub.path());
    let _danger = EnvGuard::set(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED", "true");
    let _model = EnvGuard::set(&lock, "CODEX_CLI_MODEL", "gpt-test");
    let _reason = EnvGuard::set(&lock, "CODEX_CLI_REASONING", "high");
    let _ephemeral = EnvGuard::set(&lock, "CODEX_CLI_EPHEMERAL_ENABLED", "true");
    let _argv_log = EnvGuard::set(&lock, "CODEX_TEST_ARGV_LOG", &args_log_path);

    let mut stderr = Vec::new();
    let code = runtime::exec_dangerous("hello world", "caller", &mut stderr);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let args = std::fs::read_to_string(args_log.path())
        .expect("read args")
        .lines()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();

    assert_eq!(
        args,
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
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
    );
}

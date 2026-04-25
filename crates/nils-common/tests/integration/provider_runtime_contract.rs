use nils_common::provider_runtime::auth;
use nils_common::provider_runtime::config;
use nils_common::provider_runtime::exec;
use nils_common::provider_runtime::jwt;
use nils_common::provider_runtime::{
    ExecInvocation, ExecOptions, ExecProfile, HomePathSelection, PathsProfile, ProviderDefaults,
    ProviderEnvKeys, ProviderProfile,
};
use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir, prepend_path};
use pretty_assertions::assert_eq;
use std::fs;
use std::sync::atomic::AtomicBool;

const CODEX_SECRET_HOME: &[&str] = &[".config", "codex_secrets"];
const CODEX_AUTH_HOME: &[&str] = &[".agents", "auth.json"];
const GEMINI_SECRET_HOME_MODERN: &[&str] = &[".gemini", "secrets"];
const GEMINI_AUTH_HOME_MODERN: &[&str] = &[".gemini", "oauth_creds.json"];
const GEMINI_CACHE_HOME: &[&str] = &[".gemini", "cache", "secrets"];

static WARNED_CODEX: AtomicBool = AtomicBool::new(false);
static WARNED_GEMINI: AtomicBool = AtomicBool::new(false);

static CODEX_PROFILE: ProviderProfile = ProviderProfile {
    provider_name: "codex",
    env: ProviderEnvKeys {
        model: "CODEX_CLI_MODEL",
        reasoning: "CODEX_CLI_REASONING",
        allow_dangerous_enabled: "CODEX_ALLOW_DANGEROUS_ENABLED",
        secret_dir: "CODEX_SECRET_DIR",
        auth_file: "CODEX_AUTH_FILE",
        secret_cache_dir: "CODEX_SECRET_CACHE_DIR",
        prompt_segment_enabled: "CODEX_PROMPT_SEGMENT_ENABLED",
        auto_refresh_enabled: "CODEX_AUTO_REFRESH_ENABLED",
        auto_refresh_min_days: "CODEX_AUTO_REFRESH_MIN_DAYS",
    },
    defaults: ProviderDefaults {
        model: "gpt-5.1-codex-mini",
        reasoning: "medium",
        prompt_segment_enabled: "false",
        auto_refresh_enabled: "false",
        auto_refresh_min_days: "5",
    },
    paths: PathsProfile {
        feature_name: "codex",
        feature_tool_script: "codex-tools.zsh",
        secret_dir_home: HomePathSelection::ModernOnly(CODEX_SECRET_HOME),
        auth_file_home: HomePathSelection::ModernOnly(CODEX_AUTH_HOME),
        secret_cache_home: None,
    },
    exec: ExecProfile {
        default_caller_prefix: "codex",
        missing_prompt_label: "_codex_exec_dangerous",
        binary_name: "codex",
        failed_exec_message_prefix: "codex-tools: failed to run codex exec",
        invocation: ExecInvocation::CodexStyle,
        warned_invalid_allow_dangerous: &WARNED_CODEX,
    },
};

static GEMINI_PROFILE: ProviderProfile = ProviderProfile {
    provider_name: "gemini",
    env: ProviderEnvKeys {
        model: "GEMINI_CLI_MODEL",
        reasoning: "GEMINI_CLI_REASONING",
        allow_dangerous_enabled: "GEMINI_ALLOW_DANGEROUS_ENABLED",
        secret_dir: "GEMINI_SECRET_DIR",
        auth_file: "GEMINI_AUTH_FILE",
        secret_cache_dir: "GEMINI_SECRET_CACHE_DIR",
        prompt_segment_enabled: "GEMINI_PROMPT_SEGMENT_ENABLED",
        auto_refresh_enabled: "GEMINI_AUTO_REFRESH_ENABLED",
        auto_refresh_min_days: "GEMINI_AUTO_REFRESH_MIN_DAYS",
    },
    defaults: ProviderDefaults {
        model: "gemini-2.5-flash",
        reasoning: "medium",
        prompt_segment_enabled: "false",
        auto_refresh_enabled: "false",
        auto_refresh_min_days: "5",
    },
    paths: PathsProfile {
        feature_name: "gemini",
        feature_tool_script: "gemini-tools.zsh",
        secret_dir_home: HomePathSelection::ModernOnly(GEMINI_SECRET_HOME_MODERN),
        auth_file_home: HomePathSelection::ModernOnly(GEMINI_AUTH_HOME_MODERN),
        secret_cache_home: Some(GEMINI_CACHE_HOME),
    },
    exec: ExecProfile {
        default_caller_prefix: "gemini",
        missing_prompt_label: "_gemini_exec_dangerous",
        binary_name: "gemini",
        failed_exec_message_prefix: "gemini-tools: failed to run gemini exec",
        invocation: ExecInvocation::GeminiStyle,
        warned_invalid_allow_dangerous: &WARNED_GEMINI,
    },
};

const HEADER: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
const PAYLOAD_ALPHA: &str = "eyJzdWIiOiJ1c2VyXzEyMyIsImVtYWlsIjoiYWxwaGFAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF91c2VyX2lkIjoidXNlcl8xMjMiLCJlbWFpbCI6ImFscGhhQGV4YW1wbGUuY29tIn19";

fn token(payload: &str) -> String {
    format!("{HEADER}.{payload}.sig")
}

#[test]
fn provider_runtime_auth_identity_email_and_account_contract() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("auth.json");
    let content = format!(
        r#"{{"tokens":{{"id_token":"{}","account_id":"acct_001"}}}}"#,
        token(PAYLOAD_ALPHA)
    );
    fs::write(&path, content).expect("write auth json");

    assert_eq!(
        auth::identity_from_auth_file(&path).expect("identity"),
        Some("user_123".to_string())
    );
    assert_eq!(
        auth::email_from_auth_file(&path).expect("email"),
        Some("alpha@example.com".to_string())
    );
    assert_eq!(
        auth::account_id_from_auth_file(&path).expect("account id"),
        Some("acct_001".to_string())
    );
    assert_eq!(
        auth::identity_key_from_auth_file(&path).expect("identity key"),
        Some("user_123::acct_001".to_string())
    );

    let payload = jwt::decode_payload_json(&token(PAYLOAD_ALPHA)).expect("payload json");
    assert_eq!(
        jwt::identity_from_payload(&payload),
        Some("user_123".to_string())
    );
}

#[test]
fn provider_runtime_config_snapshot_reads_provider_specific_keys() {
    let lock = GlobalStateLock::new();

    let _model = EnvGuard::set(&lock, "CODEX_CLI_MODEL", "gpt-test");
    let _reasoning = EnvGuard::set(&lock, "CODEX_CLI_REASONING", "high");
    let _danger = EnvGuard::set(&lock, "CODEX_ALLOW_DANGEROUS_ENABLED", "true");
    let _prompt_segment = EnvGuard::set(&lock, "CODEX_PROMPT_SEGMENT_ENABLED", "true");

    let snapshot = config::snapshot(&CODEX_PROFILE);
    assert_eq!(snapshot.model, "gpt-test");
    assert_eq!(snapshot.reasoning, "high");
    assert_eq!(snapshot.allow_dangerous_enabled_raw, "true");
    assert_eq!(snapshot.prompt_segment_enabled, "true");
}

#[test]
fn provider_runtime_paths_use_modern_home_locations_only() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let home = dir.path().join("home");
    fs::create_dir_all(home.join(".config").join("gemini_secrets")).expect("prior secret dir");
    fs::create_dir_all(home.join(".agents")).expect("prior auth dir");
    fs::write(home.join(".agents").join("auth.json"), "{}").expect("prior auth file");

    let _home = EnvGuard::set(&lock, "HOME", home.to_str().expect("utf-8"));
    let _secret = EnvGuard::remove(&lock, "GEMINI_SECRET_DIR");
    let _auth = EnvGuard::remove(&lock, "GEMINI_AUTH_FILE");
    let _cache = EnvGuard::remove(&lock, "GEMINI_SECRET_CACHE_DIR");
    let _zcache = EnvGuard::remove(&lock, "ZSH_CACHE_DIR");

    assert_eq!(
        nils_common::provider_runtime::paths::resolve_secret_dir(&GEMINI_PROFILE)
            .expect("secret dir"),
        home.join(".gemini").join("secrets")
    );
    assert_eq!(
        nils_common::provider_runtime::paths::resolve_auth_file(&GEMINI_PROFILE)
            .expect("auth file"),
        home.join(".gemini").join("oauth_creds.json")
    );
    assert_eq!(
        nils_common::provider_runtime::paths::resolve_secret_cache_dir(&GEMINI_PROFILE)
            .expect("cache dir"),
        home.join(".gemini").join("cache").join("secrets")
    );
}

#[test]
fn provider_runtime_exec_rejects_missing_prompt() {
    let mut stderr = Vec::new();
    let code = exec::exec_dangerous(&CODEX_PROFILE, "", "caller", &mut stderr);

    assert_eq!(code, 1);
    assert!(String::from_utf8_lossy(&stderr).contains("_codex_exec_dangerous: missing prompt"));
}

#[test]
fn provider_runtime_exec_codex_command_shape_is_stable() {
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
    let _argv_log = EnvGuard::set(&lock, "CODEX_TEST_ARGV_LOG", &args_log_path);

    let mut stderr = Vec::new();
    let code = exec::exec_dangerous(&CODEX_PROFILE, "hello world", "caller", &mut stderr);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let args = fs::read_to_string(args_log.path())
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
fn provider_runtime_exec_codex_ephemeral_command_shape_is_stable() {
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
    let _argv_log = EnvGuard::set(&lock, "CODEX_TEST_ARGV_LOG", &args_log_path);

    let mut stderr = Vec::new();
    let code = exec::exec_dangerous_with_options(
        &CODEX_PROFILE,
        "hello world",
        "caller",
        &mut stderr,
        ExecOptions { ephemeral: true },
    );

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let args = fs::read_to_string(args_log.path())
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

#[test]
fn provider_runtime_exec_gemini_command_shape_is_stable() {
    let lock = GlobalStateLock::new();
    let stub = StubBinDir::new();
    let args_log = tempfile::NamedTempFile::new().expect("args log");
    let args_log_path = args_log.path().to_string_lossy().to_string();

    stub.write_exe(
        "gemini",
        r#"#!/bin/bash
set -euo pipefail
out="${GEMINI_TEST_ARGV_LOG:?missing GEMINI_TEST_ARGV_LOG}"
: > "$out"
for a in "$@"; do
  echo "$a" >> "$out"
done
"#,
    );

    let _path = prepend_path(&lock, stub.path());
    let _danger = EnvGuard::set(&lock, "GEMINI_ALLOW_DANGEROUS_ENABLED", "true");
    let _model = EnvGuard::set(&lock, "GEMINI_CLI_MODEL", "gemini-test");
    let _argv_log = EnvGuard::set(&lock, "GEMINI_TEST_ARGV_LOG", &args_log_path);

    let mut stderr = Vec::new();
    let code = exec::exec_dangerous(&GEMINI_PROFILE, "hello world", "caller", &mut stderr);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let args = fs::read_to_string(args_log.path())
        .expect("read args")
        .lines()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();

    assert_eq!(
        args,
        vec![
            "--prompt=hello world",
            "--model",
            "gemini-test",
            "--approval-mode",
            "yolo",
        ]
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
    );
}

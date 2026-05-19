use anyhow::{Context, Result, anyhow};
use nils_common::env as common_env;
use nils_common::process as common_process;
use std::env;
use std::path::PathBuf;
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn is_help(arg: &str) -> bool {
    matches!(arg, "help" | "--help" | "-h")
}

pub fn join_args(args: &[String]) -> String {
    args.join(" ")
}

pub fn env_or_default(name: &str, default: &str) -> String {
    common_env::env_or_default(name, default)
}

pub fn env_is_true(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|v| {
            let normalized = v.trim().to_ascii_lowercase();
            normalized == "y" || common_env::is_truthy(normalized.as_str())
        })
        .unwrap_or(false)
}

pub fn now_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn cmd_exists(cmd: &str) -> bool {
    common_process::cmd_exists(cmd)
}

pub fn run_capture(cmd: &str, args: &[&str]) -> Result<String> {
    let output = run_checked_output(cmd, args)?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn run_output(cmd: &str, args: &[&str]) -> Result<Output> {
    common_process::run_output(cmd, args)
        .map(|output| output.into_std_output())
        .with_context(|| format!("spawn {cmd}"))
}

fn run_checked_output(cmd: &str, args: &[&str]) -> Result<Output> {
    let output = run_output(cmd, args)?;
    if !output.status.success() {
        return Err(anyhow!(
            "{cmd} failed: {}{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    Ok(output)
}

pub fn zsh_root() -> Result<PathBuf> {
    if let Ok(v) = env::var("ZDOTDIR") {
        return Ok(PathBuf::from(v));
    }
    let home = env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/zsh"))
}

pub fn zsh_cache_dir() -> Result<PathBuf> {
    if let Ok(v) = env::var("ZSH_CACHE_DIR") {
        return Ok(PathBuf::from(v));
    }
    Ok(zsh_root()?.join("cache"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock, StubBinDir, prepend_path};
    use tempfile::TempDir;

    #[test]
    fn help_tokens_match_expected_aliases() {
        assert!(is_help("help"));
        assert!(is_help("--help"));
        assert!(is_help("-h"));
        assert!(!is_help("status"));
    }

    #[test]
    fn env_is_true_accepts_known_truthy_values() {
        let lock = GlobalStateLock::new();
        for value in ["1", " true ", "YES", "y", "On"] {
            let _guard = EnvGuard::set(&lock, "FZF_CLI_TEST_BOOL", value);
            assert!(
                env_is_true("FZF_CLI_TEST_BOOL"),
                "expected truthy value: {value}"
            );
        }
    }

    #[test]
    fn env_is_true_rejects_missing_and_unknown_values() {
        let lock = GlobalStateLock::new();
        let _unset = EnvGuard::remove(&lock, "FZF_CLI_TEST_BOOL");
        assert!(!env_is_true("FZF_CLI_TEST_BOOL"));

        for value in ["", "0", "false", "no", "off", "2", " maybe "] {
            let _guard = EnvGuard::set(&lock, "FZF_CLI_TEST_BOOL", value);
            assert!(
                !env_is_true("FZF_CLI_TEST_BOOL"),
                "expected falsey value: {value}"
            );
        }
    }

    #[test]
    fn git_repo_probe_semantics_success_and_failure_are_stable() {
        fn probe() -> bool {
            run_output("git", &["rev-parse", "--is-inside-work-tree"])
                .map(|output| output.status.success())
                .unwrap_or(false)
        }

        let lock = GlobalStateLock::new();

        let success_stubs = StubBinDir::new();
        success_stubs.write_exe(
            "git",
            r#"#!/bin/bash
set -euo pipefail
if [[ "${1:-}" == "rev-parse" && "${2:-}" == "--is-inside-work-tree" ]]; then
  exit 0
fi
exit 1
"#,
        );
        let _path_success = prepend_path(&lock, success_stubs.path());
        assert!(probe());
        drop(_path_success);

        let fail_stubs = StubBinDir::new();
        fail_stubs.write_exe(
            "git",
            r#"#!/bin/bash
set -euo pipefail
if [[ "${1:-}" == "rev-parse" && "${2:-}" == "--is-inside-work-tree" ]]; then
  exit 128
fi
exit 1
"#,
        );
        let _path_fail = prepend_path(&lock, fail_stubs.path());
        assert!(!probe());
        drop(_path_fail);

        let empty = TempDir::new().expect("tempdir");
        let empty_path = empty.path().to_string_lossy().to_string();
        let _path_missing = EnvGuard::set(&lock, "PATH", &empty_path);
        assert!(!probe());
    }
}

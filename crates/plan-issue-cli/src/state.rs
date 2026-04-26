//! Runtime state directory resolution for plan-issue.
//!
//! plan-issue writes its canonical artefacts under
//! `<state-dir>/out/plan-issue-delivery/...`. The state directory is
//! resolved (in order) from:
//!
//! 1. CLI override set via [`set_state_dir_override`] (driven by the
//!    `--state-dir` global flag).
//! 2. `PLAN_ISSUE_HOME` environment variable (fallback for adapters that
//!    pin a workspace via env, such as the CLI's own integration tests).
//! 3. `${XDG_STATE_HOME:-$HOME/.local/state}/plan-issue` default.
//!
//! Callers must read the resolved path through [`state_dir`]; the helper
//! is the single entry point for runtime-layout math elsewhere in the
//! crate (see `runtime_layout::runtime_root`, `task_spec`, `render`,
//! `execute`).

use std::path::PathBuf;
use std::sync::RwLock;

use nils_common::env as common_env;

/// Environment variable consulted when no `--state-dir` flag is passed.
pub const PLAN_ISSUE_STATE_HOME_ENV: &str = "PLAN_ISSUE_HOME";

static CLI_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Set or clear the CLI-level `--state-dir` override.
///
/// Called once near the top of `run_with_args` after `Cli::try_parse_from`.
/// Passing `None` resets the override (used by tests that need to fall
/// back to the env-var path).
pub fn set_state_dir_override(value: Option<PathBuf>) {
    let mut guard = CLI_OVERRIDE
        .write()
        .expect("plan-issue state-dir override write lock");
    *guard = value;
}

/// Resolve the active state directory using the documented chain.
///
/// The returned path is **not** guaranteed to exist; callers that emit
/// artefacts into it should `mkdir -p` before writing.
pub fn state_dir() -> PathBuf {
    if let Some(path) = CLI_OVERRIDE
        .read()
        .expect("plan-issue state-dir override read lock")
        .clone()
    {
        return path;
    }

    if let Some(value) = common_env::env_non_empty(PLAN_ISSUE_STATE_HOME_ENV) {
        return PathBuf::from(value);
    }

    xdg_default()
}

fn xdg_default() -> PathBuf {
    if let Some(value) = common_env::env_non_empty("XDG_STATE_HOME") {
        return PathBuf::from(value).join("plan-issue");
    }

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    home.join(".local").join("state").join("plan-issue")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock};

    fn reset() {
        set_state_dir_override(None);
    }

    #[test]
    fn cli_override_wins_over_env_and_xdg() {
        let lock = GlobalStateLock::new();
        let _env = EnvGuard::set(&lock, "PLAN_ISSUE_HOME", "/tmp/plan-issue-env");
        let _xdg = EnvGuard::set(&lock, "XDG_STATE_HOME", "/tmp/xdg");

        set_state_dir_override(Some(PathBuf::from("/tmp/cli-override")));
        let resolved = state_dir();
        reset();

        assert_eq!(resolved, PathBuf::from("/tmp/cli-override"));
    }

    #[test]
    fn env_used_when_override_unset() {
        let lock = GlobalStateLock::new();
        reset();
        let _env = EnvGuard::set(&lock, "PLAN_ISSUE_HOME", "/tmp/plan-issue-env");
        let _xdg = EnvGuard::set(&lock, "XDG_STATE_HOME", "/tmp/xdg");

        let resolved = state_dir();
        assert_eq!(resolved, PathBuf::from("/tmp/plan-issue-env"));
    }

    #[test]
    fn xdg_default_when_neither_override_nor_env_set() {
        let lock = GlobalStateLock::new();
        reset();
        let _env = EnvGuard::remove(&lock, "PLAN_ISSUE_HOME");
        let _xdg = EnvGuard::set(&lock, "XDG_STATE_HOME", "/tmp/xdg");

        let resolved = state_dir();
        assert_eq!(resolved, PathBuf::from("/tmp/xdg/plan-issue"));
    }
}

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{fs::OpenOptions, io::Write};

pub fn enabled() -> bool {
    cfg!(debug_assertions)
        && std::env::var("AGENTS_MACOS_AGENT_TEST_MODE")
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
}

pub fn timestamp() -> String {
    if enabled()
        && let Ok(value) = std::env::var("AGENTS_MACOS_AGENT_TEST_TIMESTAMP")
        && !value.trim().is_empty()
    {
        return value;
    }
    jiff::Timestamp::now().to_string()
}

pub fn backend_root_override() -> Option<PathBuf> {
    enabled()
        .then(|| std::env::var_os("NILS_MACOS_AGENT_BACKEND_ROOT").map(PathBuf::from))
        .flatten()
}

pub fn peekaboo_bin_override() -> Option<PathBuf> {
    enabled()
        .then(|| std::env::var_os("NILS_MACOS_AGENT_PEEKABOO_BIN").map(PathBuf::from))
        .flatten()
}

pub fn ssh_bin_override() -> Option<PathBuf> {
    enabled()
        .then(|| std::env::var_os("NILS_MACOS_AGENT_SSH_BIN").map(PathBuf::from))
        .flatten()
}

pub fn remote_root_override() -> Option<PathBuf> {
    enabled()
        .then(|| std::env::var_os("NILS_MACOS_AGENT_REMOTE_ROOT").map(PathBuf::from))
        .flatten()
}

pub fn lock_path_override() -> Option<PathBuf> {
    enabled()
        .then(|| std::env::var_os("NILS_MACOS_AGENT_LOCK_PATH").map(PathBuf::from))
        .flatten()
}

pub fn cleanup_failure() -> bool {
    enabled() && std::env::var_os("NILS_MACOS_AGENT_TEST_CLEANUP_FAIL").is_some()
}

pub fn record_runtime_action(action: &str) -> std::io::Result<()> {
    if !enabled() {
        return Ok(());
    }
    let Some(path) = std::env::var_os("NILS_MACOS_AGENT_TEST_RUNTIME_ACTION_TRACE") else {
        return Ok(());
    };
    let mut trace = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(trace, "{action}")
}

pub fn ssh_mcp_exit_timeout() -> Duration {
    if enabled()
        && let Ok(value) = std::env::var("NILS_MACOS_AGENT_TEST_SSH_MCP_EXIT_TIMEOUT_MS")
        && let Ok(milliseconds) = value.parse::<u64>()
    {
        return Duration::from_millis(milliseconds.clamp(1, 60_000));
    }
    Duration::from_secs(5)
}

pub fn verification_tool_override(program: &Path) -> Option<PathBuf> {
    if !enabled() || program.components().count() != 1 {
        return None;
    }
    let root = std::env::var_os("NILS_MACOS_AGENT_TEST_TOOL_DIR").map(PathBuf::from)?;
    let candidate = root.join(program);
    candidate.is_file().then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::enabled;
    #[cfg(not(debug_assertions))]
    use super::ssh_mcp_exit_timeout;
    #[cfg(not(debug_assertions))]
    use std::time::Duration;

    #[test]
    fn false_like_environment_values_do_not_enable_test_mode() {
        let previous = std::env::var_os("AGENTS_MACOS_AGENT_TEST_MODE");
        // SAFETY: this focused test restores the process environment before returning.
        unsafe { std::env::set_var("AGENTS_MACOS_AGENT_TEST_MODE", "0") };
        assert!(!enabled());
        // SAFETY: restore the value observed before this test.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("AGENTS_MACOS_AGENT_TEST_MODE", value),
                None => std::env::remove_var("AGENTS_MACOS_AGENT_TEST_MODE"),
            }
        }
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn release_builds_ignore_test_mode_environment() {
        let previous = std::env::var_os("AGENTS_MACOS_AGENT_TEST_MODE");
        let previous_timeout = std::env::var_os("NILS_MACOS_AGENT_TEST_SSH_MCP_EXIT_TIMEOUT_MS");
        // SAFETY: this focused test restores the process environment before returning.
        unsafe {
            std::env::set_var("AGENTS_MACOS_AGENT_TEST_MODE", "1");
            std::env::set_var("NILS_MACOS_AGENT_TEST_SSH_MCP_EXIT_TIMEOUT_MS", "1");
        }
        assert!(!enabled());
        assert_eq!(ssh_mcp_exit_timeout(), Duration::from_secs(5));
        // SAFETY: restore the value observed before this test.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("AGENTS_MACOS_AGENT_TEST_MODE", value),
                None => std::env::remove_var("AGENTS_MACOS_AGENT_TEST_MODE"),
            }
            match previous_timeout {
                Some(value) => {
                    std::env::set_var("NILS_MACOS_AGENT_TEST_SSH_MCP_EXIT_TIMEOUT_MS", value)
                }
                None => std::env::remove_var("NILS_MACOS_AGENT_TEST_SSH_MCP_EXIT_TIMEOUT_MS"),
            }
        }
    }
}

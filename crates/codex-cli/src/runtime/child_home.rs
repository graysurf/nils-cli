//! Private child `CODEX_HOME` primitives shared by the isolated one-shot
//! runtime and the Execution Capsule supervisor projection.
//!
//! Both runtimes need the same security-sensitive behavior: an owner-only
//! temporary home, a file-backed authentication symlink that never copies
//! credential bytes, control-environment removal, and a replacement warning
//! when the child substitutes the bridged auth file. Keeping one
//! implementation avoids maintaining two copies of that boundary.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

/// An owner-only temporary `CODEX_HOME` removed when dropped.
pub(crate) struct ChildHome {
    directory: tempfile::TempDir,
}

impl ChildHome {
    /// Create a unique private home, preferring `XDG_RUNTIME_DIR`.
    pub(crate) fn create(prefix: &str) -> Result<Self, String> {
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .and_then(|path| {
                tempfile::Builder::new()
                    .prefix(prefix)
                    .tempdir_in(path)
                    .ok()
            })
            .or_else(|| tempfile::Builder::new().prefix(prefix).tempdir().ok())
            .ok_or_else(|| "could not create temporary CODEX_HOME".to_string())?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        Ok(Self { directory })
    }

    /// Bridge file-backed authentication with a symlink. Credential bytes are
    /// never copied into the child home.
    pub(crate) fn bridge_auth(&self, source: Option<&Path>) -> Result<(), String> {
        if let Some(source) = source {
            symlink(source, self.directory.path().join("auth.json"))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub(crate) fn path(&self) -> &Path {
        self.directory.path()
    }
}

pub(crate) fn original_codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

pub(crate) fn auth_source(original_home: &Path) -> Option<PathBuf> {
    std::env::var_os("CODEX_AUTH_FILE")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| super::resolve_auth_file().filter(|path| path.is_file()))
        .or_else(|| Some(original_home.join("auth.json")).filter(|path| path.is_file()))
        .and_then(|path| fs::canonicalize(path).ok())
}

/// True when the child replaced the bridged auth symlink with a regular file,
/// meaning its credential write was not propagated back to the source.
pub(crate) fn auth_replaced(home: &Path) -> bool {
    let auth = home.join("auth.json");
    auth.exists()
        && fs::symlink_metadata(&auth)
            .map(|metadata| !metadata.file_type().is_symlink())
            .unwrap_or(false)
}

pub(crate) fn warn_if_auth_replaced(home: &Path, stderr: &mut impl Write) {
    if auth_replaced(home) {
        let _ = writeln!(
            stderr,
            "isolated-auth-write-not-propagated: child auth replacement was not copied back"
        );
    }
}

pub(crate) fn remove_control_environment(command: &mut Command) {
    let exact = [
        "CODEX_AUTH_FILE",
        "CODEX_SECRET_DIR",
        "CODEX_SECRET_CACHE_DIR",
        "CODEX_CLI_AGENT_RUNTIME",
    ];
    for name in exact {
        command.env_remove(name);
    }
    let prefixes = [
        "CODEX_AUTH_REMOTE_",
        "AGENT_DOCS_",
        "AGENT_SESSION_",
        "AGENT_HOOK_",
        "AGENT_EVIDENCE_",
    ];
    let names = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| {
            let name = name.to_string_lossy();
            prefixes.iter().any(|prefix| name.starts_with(prefix))
        })
        .collect::<Vec<OsString>>();
    for name in names {
        command.env_remove(name);
    }
}

/// Whitespace-separated words of `codex exec --help`, used to probe flags.
pub(crate) fn codex_exec_help_words() -> HashSet<String> {
    command_words(&["exec", "--help"])
}

/// Feature names reported by `codex features list`.
pub(crate) fn codex_feature_names() -> HashSet<String> {
    command_words(&["features", "list"])
}

fn command_words(args: &[&str]) -> HashSet<String> {
    Command::new("codex")
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

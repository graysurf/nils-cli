use std::env;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::HookError;

#[derive(Clone, Debug)]
pub struct Layout {
    pub config_path: PathBuf,
    pub state_root: PathBuf,
}

impl Layout {
    pub fn resolve(config: Option<PathBuf>, state_dir: Option<PathBuf>) -> Result<Self, HookError> {
        let config_path = match config {
            Some(path) => absolute("--config", path)?,
            None => config_home()?.join("agent-hook/config.toml"),
        };
        let state_root = match state_dir {
            Some(path) => absolute("--state-dir", path)?,
            None => state_home()?.join("agent-hook"),
        };
        Ok(Self {
            config_path,
            state_root,
        })
    }
}

pub fn agent_session_state_root() -> Result<PathBuf, HookError> {
    if let Some(value) = non_empty("AGENT_SESSION_STATE_DIR") {
        return absolute("AGENT_SESSION_STATE_DIR", PathBuf::from(value));
    }
    Ok(state_home()?.join("agent-session"))
}

pub fn ensure_private_state_dir(path: &Path, role: &str) -> Result<(), HookError> {
    let normalized_path = normalize_private_state_path(path);
    for ancestor in normalized_path
        .ancestors()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(HookError::data(
                    format!("{role}-untrusted"),
                    format!("{role} path contains a symlink"),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(HookError::runtime(
                    format!("{role}-unavailable"),
                    format!("{role} path metadata is unavailable"),
                ));
            }
        }
    }
    match fs::symlink_metadata(&normalized_path) {
        Ok(metadata) => {
            if !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(HookError::data(
                    format!("{role}-untrusted"),
                    format!("{role} directory owner, mode, or type is untrusted"),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(&normalized_path).map_err(|_| {
                HookError::runtime(
                    format!("{role}-create-failed"),
                    format!("{role} directory create failed"),
                )
            })?;
            fs::set_permissions(&normalized_path, fs::Permissions::from_mode(0o700)).map_err(
                |_| {
                    HookError::runtime(
                        format!("{role}-mode-failed"),
                        format!("{role} directory mode failed"),
                    )
                },
            )?;
            let metadata = fs::symlink_metadata(&normalized_path).map_err(|_| {
                HookError::runtime(
                    format!("{role}-unavailable"),
                    format!("{role} directory metadata is unavailable"),
                )
            })?;
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(HookError::data(
                    format!("{role}-untrusted"),
                    format!("{role} directory owner, mode, or type is untrusted"),
                ));
            }
        }
        Err(_) => {
            return Err(HookError::runtime(
                format!("{role}-unavailable"),
                format!("{role} directory metadata is unavailable"),
            ));
        }
    }
    Ok(())
}

fn normalize_private_state_path(path: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        normalize_darwin_private_state_path(path)
    }

    #[cfg(not(target_os = "macos"))]
    {
        path.to_path_buf()
    }
}

#[cfg(target_os = "macos")]
fn normalize_darwin_private_state_path(path: &Path) -> PathBuf {
    let Ok(darwin_var) = fs::canonicalize("/var") else {
        return path.to_path_buf();
    };
    if darwin_var != Path::new("/private/var") {
        return path.to_path_buf();
    }
    let Ok(suffix) = path.strip_prefix("/var") else {
        return path.to_path_buf();
    };
    Path::new("/private/var").join(suffix)
}

fn config_home() -> Result<PathBuf, HookError> {
    xdg_or_home("XDG_CONFIG_HOME", ".config")
}

fn state_home() -> Result<PathBuf, HookError> {
    xdg_or_home("XDG_STATE_HOME", ".local/state")
}

fn xdg_or_home(name: &str, fallback: &str) -> Result<PathBuf, HookError> {
    if let Some(value) = non_empty(name) {
        return absolute(name, PathBuf::from(value));
    }
    let home = non_empty("HOME").ok_or_else(|| {
        HookError::runtime("home-unavailable", "HOME is required for XDG fallback")
    })?;
    let home = absolute("HOME", PathBuf::from(home))?;
    Ok(home.join(fallback))
}

fn non_empty(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn absolute(label: &str, path: PathBuf) -> Result<PathBuf, HookError> {
    if !path.is_absolute() {
        return Err(HookError::data(
            "path-not-absolute",
            format!("{label} must be an absolute path"),
        ));
    }
    Ok(path)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::ensure_private_state_dir;

    #[test]
    fn ensure_private_state_dir_accepts_lexical_var_tmp_private_state_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(
            temp.path().starts_with("/var"),
            "tempdir should be lexically under /var: {}",
            temp.path().display()
        );
        let path = temp.path().join("private-state");

        ensure_private_state_dir(&path, "state-dir").expect("private state dir");

        assert!(path.is_dir(), "state dir should exist");
    }
}

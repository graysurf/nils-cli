use std::env;
use std::path::PathBuf;

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

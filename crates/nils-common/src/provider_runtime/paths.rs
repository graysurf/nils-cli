use std::env;
use std::path::{Path, PathBuf};

use super::profile::{HomePathSelection, ProviderProfile};

pub fn resolve_secret_dir(profile: &ProviderProfile) -> Option<PathBuf> {
    if let Some(dir) = resolve_secret_dir_from_env(profile) {
        return Some(dir);
    }

    if let Some(home) = home_dir() {
        return Some(resolve_home_path(&home, profile.paths.secret_dir_home));
    }

    let feature_dir = resolve_feature_dir(profile)?;
    if feature_dir.join("init.zsh").is_file()
        || feature_dir
            .join(profile.paths.feature_tool_script)
            .is_file()
    {
        return Some(feature_dir.join("secrets"));
    }
    Some(feature_dir)
}

pub fn resolve_secret_dir_from_env(profile: &ProviderProfile) -> Option<PathBuf> {
    env_path(profile.env.secret_dir)
}

pub fn resolve_auth_file(profile: &ProviderProfile) -> Option<PathBuf> {
    if let Some(path) = env_path(profile.env.auth_file) {
        return Some(path);
    }

    let home = home_dir()?;
    Some(resolve_home_path(&home, profile.paths.auth_file_home))
}

pub fn resolve_secret_cache_dir(profile: &ProviderProfile) -> Option<PathBuf> {
    if let Some(path) = env_path(profile.env.secret_cache_dir) {
        return Some(path);
    }

    if let Some(path) = env_path("ZSH_CACHE_DIR") {
        return Some(path.join(profile.paths.feature_name).join("secrets"));
    }

    if let Some(home_segments) = profile.paths.secret_cache_home
        && let Some(home) = home_dir()
    {
        return Some(join_segments(home, home_segments));
    }

    Some(
        resolve_zdotdir()?
            .join("cache")
            .join(profile.paths.feature_name)
            .join("secrets"),
    )
}

pub fn resolve_secret_timestamp_path(
    profile: &ProviderProfile,
    target_file: &Path,
) -> Option<PathBuf> {
    let cache_dir = resolve_secret_cache_dir(profile)?;
    Some(secret_timestamp_path(&cache_dir, target_file))
}

pub fn secret_timestamp_path(cache_dir: &Path, target_file: &Path) -> PathBuf {
    let file_name = target_file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("auth.json");
    cache_dir.join(format!("{file_name}.timestamp"))
}

pub fn resolve_feature_dir(profile: &ProviderProfile) -> Option<PathBuf> {
    let script_dir = resolve_script_dir()?;
    let feature_dir = script_dir
        .join("_features")
        .join(profile.paths.feature_name);
    if feature_dir.is_dir() {
        Some(feature_dir)
    } else {
        None
    }
}

pub fn resolve_script_dir() -> Option<PathBuf> {
    if let Some(path) = env_path("ZSH_SCRIPT_DIR") {
        return Some(path);
    }
    Some(resolve_zdotdir()?.join("scripts"))
}

pub fn resolve_zdotdir() -> Option<PathBuf> {
    if let Some(path) = env_path("ZDOTDIR") {
        return Some(path);
    }

    if let Some(preload) = env_path("_ZSH_BOOTSTRAP_PRELOAD_PATH")
        && let Some(parent) = parent_dir(&preload, 2)
    {
        return Some(parent);
    }

    let home = home_dir()?;
    Some(home.join(".config").join("zsh"))
}

fn resolve_home_path(home: &Path, selection: HomePathSelection) -> PathBuf {
    match selection {
        HomePathSelection::ModernOnly(segments) => join_segments(home.to_path_buf(), segments),
    }
}

fn join_segments(mut base: PathBuf, segments: &[&str]) -> PathBuf {
    for segment in segments {
        base.push(segment);
    }
    base
}

fn env_path(key: &str) -> Option<PathBuf> {
    let raw = env::var_os(key)?;
    if raw.is_empty() {
        return None;
    }
    Some(PathBuf::from(raw))
}

fn home_dir() -> Option<PathBuf> {
    crate::fs::home_dir()
}

fn parent_dir(path: &Path, levels: usize) -> Option<PathBuf> {
    let mut current = path;
    for _ in 0..levels {
        current = current.parent()?;
    }
    Some(current.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::secret_timestamp_path;
    use std::path::Path;

    #[test]
    fn secret_timestamp_path_defaults_file_name_when_missing() {
        let cache = Path::new("/tmp/cache");
        assert_eq!(
            secret_timestamp_path(cache, Path::new("/tmp/alpha.json")),
            cache.join("alpha.json.timestamp")
        );
        assert_eq!(
            secret_timestamp_path(cache, Path::new("")),
            cache.join("auth.json.timestamp")
        );
    }
}

use std::path::PathBuf;

pub fn resolve_zdotdir() -> Option<PathBuf> {
    std::env::var_os("ZDOTDIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            resolve_script_dir().and_then(|script_dir| script_dir.parent().map(PathBuf::from))
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".config/zsh"))
        })
}

pub fn resolve_script_dir() -> Option<PathBuf> {
    std::env::var_os("ZSH_SCRIPT_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub fn resolve_feature_dir() -> Option<PathBuf> {
    resolve_script_dir().map(|script_dir| script_dir.join("_features/opencode"))
}

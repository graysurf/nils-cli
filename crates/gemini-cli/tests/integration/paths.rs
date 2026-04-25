use gemini_cli::paths;
use nils_test_support::{EnvGuard, GlobalStateLock};
use std::fs;
fn set_env(lock: &GlobalStateLock, key: &str, value: impl AsRef<std::ffi::OsStr>) -> EnvGuard {
    let value = value.as_ref().to_string_lossy().into_owned();
    EnvGuard::set(lock, key, &value)
}

#[test]
fn paths_resolve_zdotdir_variants() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let zdotdir_env = dir.path().join("zdot_env");
    fs::create_dir_all(&zdotdir_env).expect("zdot_env");

    {
        let _zdotdir = set_env(&lock, "ZDOTDIR", zdotdir_env.as_os_str());
        let _preload = EnvGuard::remove(&lock, "_ZSH_BOOTSTRAP_PRELOAD_PATH");
        assert_eq!(paths::resolve_zdotdir().expect("zdotdir"), zdotdir_env);
    }

    {
        let _zdotdir = EnvGuard::remove(&lock, "ZDOTDIR");

        let preload = dir.path().join("a").join("b").join("preload.zsh");
        fs::create_dir_all(preload.parent().expect("parent")).expect("preload parent");

        let _preload = set_env(&lock, "_ZSH_BOOTSTRAP_PRELOAD_PATH", preload.as_os_str());

        let expected = dir.path().join("a");
        assert_eq!(paths::resolve_zdotdir().expect("zdotdir"), expected);
    }

    {
        let _zdotdir = EnvGuard::remove(&lock, "ZDOTDIR");
        let _preload = EnvGuard::remove(&lock, "_ZSH_BOOTSTRAP_PRELOAD_PATH");

        let home = dir.path().join("home");
        fs::create_dir_all(&home).expect("home");
        let _home = set_env(&lock, "HOME", home.as_os_str());

        assert_eq!(
            paths::resolve_zdotdir().expect("zdotdir"),
            home.join(".config").join("zsh")
        );
    }
}

#[test]
fn paths_resolve_secret_dir_from_feature_dir() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let override_dir = dir.path().join("override_secrets");
    fs::create_dir_all(&override_dir).expect("override dir");

    {
        let _secret = set_env(&lock, "GEMINI_SECRET_DIR", override_dir.as_os_str());
        assert_eq!(
            paths::resolve_secret_dir().expect("secret dir"),
            override_dir
        );
    }

    let script_dir = dir.path().join("scripts");
    fs::create_dir_all(&script_dir).expect("scripts dir");
    let feature_dir = script_dir.join("_features").join("gemini");
    fs::create_dir_all(&feature_dir).expect("feature dir");

    {
        let home = dir.path().join("home");
        fs::create_dir_all(&home).expect("home");
        let _secret = EnvGuard::remove(&lock, "GEMINI_SECRET_DIR");
        let _home = set_env(&lock, "HOME", home.as_os_str());
        assert_eq!(
            paths::resolve_secret_dir().expect("secret dir"),
            home.join(".gemini").join("secrets")
        );
    }

    {
        let _secret = EnvGuard::remove(&lock, "GEMINI_SECRET_DIR");
        let _home = EnvGuard::remove(&lock, "HOME");
        let _script_dir = set_env(&lock, "ZSH_SCRIPT_DIR", script_dir.as_os_str());

        fs::write(feature_dir.join("init.zsh"), "#").expect("init.zsh");
        assert_eq!(
            paths::resolve_secret_dir().expect("secret dir"),
            feature_dir.join("secrets")
        );

        fs::remove_file(feature_dir.join("init.zsh")).expect("remove init.zsh");
        assert_eq!(
            paths::resolve_secret_dir().expect("secret dir"),
            feature_dir
        );
    }
}

#[test]
fn paths_resolve_secret_cache_dir_variants() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let override_dir = dir.path().join("cache_override");
    {
        let _override = set_env(&lock, "GEMINI_SECRET_CACHE_DIR", override_dir.as_os_str());
        assert_eq!(
            paths::resolve_secret_cache_dir().expect("secret cache dir"),
            override_dir
        );
    }

    {
        let _override = EnvGuard::remove(&lock, "GEMINI_SECRET_CACHE_DIR");
        let cache_root = dir.path().join("cache_root");
        let _cache_root = set_env(&lock, "ZSH_CACHE_DIR", cache_root.as_os_str());

        assert_eq!(
            paths::resolve_secret_cache_dir().expect("secret cache dir"),
            cache_root.join("gemini").join("secrets")
        );
    }

    {
        let _override = EnvGuard::remove(&lock, "GEMINI_SECRET_CACHE_DIR");
        let _cache_root = EnvGuard::remove(&lock, "ZSH_CACHE_DIR");
        let home = dir.path().join("home");
        fs::create_dir_all(&home).expect("home");
        let _home = set_env(&lock, "HOME", home.as_os_str());

        assert_eq!(
            paths::resolve_secret_cache_dir().expect("secret cache dir"),
            home.join(".gemini").join("cache").join("secrets")
        );
    }

    {
        let _override = EnvGuard::remove(&lock, "GEMINI_SECRET_CACHE_DIR");
        let _cache_root = EnvGuard::remove(&lock, "ZSH_CACHE_DIR");
        let _home = EnvGuard::remove(&lock, "HOME");
        let zdotdir = dir.path().join("zdotdir");
        let _zdotdir = set_env(&lock, "ZDOTDIR", zdotdir.as_os_str());
        assert_eq!(
            paths::resolve_secret_cache_dir().expect("secret cache dir"),
            zdotdir.join("cache").join("gemini").join("secrets")
        );
    }
}

#[test]
fn paths_resolve_auth_file_prefers_env() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let auth_file = dir.path().join("auth.json");
    let _auth = set_env(&lock, "GEMINI_AUTH_FILE", auth_file.as_os_str());

    assert_eq!(paths::resolve_auth_file().expect("auth file"), auth_file);
}

#[test]
fn paths_resolve_script_dir_ignores_empty_env_override() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let zdotdir = dir.path().join("zdotdir");
    fs::create_dir_all(&zdotdir).expect("zdotdir");

    let _zdotdir = set_env(&lock, "ZDOTDIR", zdotdir.as_os_str());
    let _script = EnvGuard::set(&lock, "ZSH_SCRIPT_DIR", "");

    assert_eq!(
        paths::resolve_script_dir().expect("script dir"),
        zdotdir.join("scripts")
    );
}

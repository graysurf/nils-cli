use codex_cli::paths;
use nils_test_support::{EnvGuard, GlobalStateLock};
use pretty_assertions::assert_eq;
use std::fs;

#[test]
fn paths_resolve_zdotdir_variants() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let zdotdir_env = dir.path().join("zdot_env");
    fs::create_dir_all(&zdotdir_env).expect("zdot_env");

    {
        let _zdotdir = EnvGuard::set(&lock, "ZDOTDIR", zdotdir_env.to_str().expect("ZDOTDIR"));
        let _preload = EnvGuard::remove(&lock, "_ZSH_BOOTSTRAP_PRELOAD_PATH");
        assert_eq!(paths::resolve_zdotdir().expect("zdotdir"), zdotdir_env);
    }

    {
        let _zdotdir = EnvGuard::remove(&lock, "ZDOTDIR");

        let preload = dir.path().join("a").join("b").join("preload.zsh");
        fs::create_dir_all(preload.parent().expect("parent")).expect("preload parent");

        let _preload = EnvGuard::set(
            &lock,
            "_ZSH_BOOTSTRAP_PRELOAD_PATH",
            preload.to_str().expect("preload"),
        );

        let expected = dir.path().join("a");
        assert_eq!(paths::resolve_zdotdir().expect("zdotdir"), expected);
    }

    {
        let _zdotdir = EnvGuard::remove(&lock, "ZDOTDIR");
        let _preload = EnvGuard::remove(&lock, "_ZSH_BOOTSTRAP_PRELOAD_PATH");

        let home = dir.path().join("home");
        fs::create_dir_all(&home).expect("home");
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().expect("home"));

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
        let _secret = EnvGuard::set(
            &lock,
            "CODEX_SECRET_DIR",
            override_dir.to_str().expect("override"),
        );
        assert_eq!(
            paths::resolve_secret_dir().expect("secret dir"),
            override_dir
        );
    }

    let script_dir = dir.path().join("scripts");
    fs::create_dir_all(&script_dir).expect("scripts dir");
    let feature_dir = script_dir.join("_features").join("codex");
    fs::create_dir_all(&feature_dir).expect("feature dir");

    {
        let home = dir.path().join("home");
        fs::create_dir_all(&home).expect("home");
        let _secret = EnvGuard::remove(&lock, "CODEX_SECRET_DIR");
        let _home = EnvGuard::set(&lock, "HOME", home.to_str().expect("home"));
        assert_eq!(
            paths::resolve_secret_dir().expect("secret dir"),
            home.join(".config").join("codex_secrets")
        );
    }

    {
        let _secret = EnvGuard::remove(&lock, "CODEX_SECRET_DIR");
        let _home = EnvGuard::remove(&lock, "HOME");
        let _script_dir = EnvGuard::set(
            &lock,
            "ZSH_SCRIPT_DIR",
            script_dir.to_str().expect("script dir"),
        );

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
        let _override = EnvGuard::set(
            &lock,
            "CODEX_SECRET_CACHE_DIR",
            override_dir.to_str().expect("override"),
        );
        assert_eq!(
            paths::resolve_secret_cache_dir().expect("secret cache dir"),
            override_dir
        );
    }

    {
        let _override = EnvGuard::remove(&lock, "CODEX_SECRET_CACHE_DIR");
        let cache_root = dir.path().join("cache_root");
        let _cache_root = EnvGuard::set(
            &lock,
            "ZSH_CACHE_DIR",
            cache_root.to_str().expect("cache_root"),
        );

        assert_eq!(
            paths::resolve_secret_cache_dir().expect("secret cache dir"),
            cache_root.join("codex").join("secrets")
        );
    }

    {
        let _override = EnvGuard::remove(&lock, "CODEX_SECRET_CACHE_DIR");
        let _cache_root = EnvGuard::remove(&lock, "ZSH_CACHE_DIR");

        let zdotdir = dir.path().join("zdotdir");
        let _zdotdir = EnvGuard::set(&lock, "ZDOTDIR", zdotdir.to_str().expect("zdotdir"));

        assert_eq!(
            paths::resolve_secret_cache_dir().expect("secret cache dir"),
            zdotdir.join("cache").join("codex").join("secrets")
        );
    }
}

#[test]
fn paths_resolve_auth_file_prefers_env() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let auth_file = dir.path().join("auth.json");
    let _auth = EnvGuard::set(&lock, "CODEX_AUTH_FILE", auth_file.to_str().expect("auth"));

    assert_eq!(paths::resolve_auth_file().expect("auth file"), auth_file);
}

#[test]
fn paths_resolve_script_dir_ignores_empty_env_override() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let zdotdir = dir.path().join("zdotdir");
    fs::create_dir_all(&zdotdir).expect("zdotdir");

    let _zdotdir = EnvGuard::set(&lock, "ZDOTDIR", zdotdir.to_str().expect("zdotdir"));
    let _script = EnvGuard::set(&lock, "ZSH_SCRIPT_DIR", "");

    assert_eq!(
        paths::resolve_script_dir().expect("script dir"),
        zdotdir.join("scripts")
    );
}

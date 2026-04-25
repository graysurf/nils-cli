use gemini_cli::paths;
use gemini_cli::runtime;
use nils_test_support::{EnvGuard, GlobalStateLock};
use pretty_assertions::assert_eq;
use std::fs;

#[test]
fn runtime_paths_config_contract_resolve_secret_and_cache_dirs_with_existing_precedence() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let secret_override = dir.path().join("secrets_override");
    let cache_override = dir.path().join("cache_override");

    let _secret = EnvGuard::set(
        &lock,
        "GEMINI_SECRET_DIR",
        secret_override.to_str().expect("utf-8"),
    );
    let _cache = EnvGuard::set(
        &lock,
        "GEMINI_SECRET_CACHE_DIR",
        cache_override.to_str().expect("utf-8"),
    );

    assert_eq!(
        paths::resolve_secret_dir().expect("secret dir"),
        secret_override
    );
    assert_eq!(
        paths::resolve_secret_cache_dir().expect("secret cache dir"),
        cache_override
    );
}

#[test]
fn runtime_paths_config_contract_resolve_secret_and_auth_paths_use_modern_home_only() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let home = dir.path().join("home");
    fs::create_dir_all(home.join(".config").join("gemini_secrets")).expect("prior secret dir");
    fs::create_dir_all(home.join(".agents")).expect("prior auth dir");
    fs::write(home.join(".agents").join("auth.json"), "{}").expect("prior auth file");

    let _home = EnvGuard::set(&lock, "HOME", home.to_str().expect("utf-8"));
    let _secret = EnvGuard::remove(&lock, "GEMINI_SECRET_DIR");
    let _auth = EnvGuard::remove(&lock, "GEMINI_AUTH_FILE");

    assert_eq!(
        paths::resolve_secret_dir().expect("secret dir"),
        home.join(".gemini").join("secrets")
    );
    assert_eq!(
        paths::resolve_auth_file().expect("auth file"),
        home.join(".gemini").join("oauth_creds.json")
    );
}

#[test]
fn runtime_paths_config_contract_resolve_feature_dir_and_zdotdir_fallbacks() {
    let lock = GlobalStateLock::new();
    let dir = tempfile::TempDir::new().expect("tempdir");

    let _secret = EnvGuard::remove(&lock, "GEMINI_SECRET_DIR");
    let _cache = EnvGuard::remove(&lock, "GEMINI_SECRET_CACHE_DIR");
    let _zdot = EnvGuard::remove(&lock, "ZDOTDIR");
    let _preload = EnvGuard::remove(&lock, "_ZSH_BOOTSTRAP_PRELOAD_PATH");

    let home = dir.path().join("home");
    fs::create_dir_all(&home).expect("home");
    let _home = EnvGuard::set(&lock, "HOME", home.to_str().expect("utf-8"));

    assert_eq!(
        paths::resolve_zdotdir().expect("zdotdir"),
        home.join(".config").join("zsh")
    );

    let script_dir = dir.path().join("scripts");
    let feature_dir = script_dir.join("_features").join("gemini");
    fs::create_dir_all(&feature_dir).expect("feature dir");
    fs::write(feature_dir.join("init.zsh"), "#").expect("init");

    let _home = EnvGuard::remove(&lock, "HOME");
    let _script = EnvGuard::set(&lock, "ZSH_SCRIPT_DIR", script_dir.to_str().expect("utf-8"));

    assert_eq!(
        paths::resolve_secret_dir().expect("secret dir"),
        feature_dir.join("secrets")
    );
}

#[test]
fn runtime_paths_config_contract_snapshot_keeps_default_and_env_overrides() {
    let lock = GlobalStateLock::new();

    let _model = EnvGuard::set(&lock, "GEMINI_CLI_MODEL", "gemini-test");
    let _reasoning = EnvGuard::set(&lock, "GEMINI_CLI_REASONING", "high");
    let _danger = EnvGuard::set(&lock, "GEMINI_ALLOW_DANGEROUS_ENABLED", "true");
    let _prompt_segment_enabled = EnvGuard::set(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    let _refresh_enabled = EnvGuard::set(&lock, "GEMINI_AUTO_REFRESH_ENABLED", "true");
    let _refresh_days = EnvGuard::set(&lock, "GEMINI_AUTO_REFRESH_MIN_DAYS", "9");

    let snapshot = runtime::config_snapshot();
    assert_eq!(snapshot.model, "gemini-test");
    assert_eq!(snapshot.reasoning, "high");
    assert_eq!(snapshot.allow_dangerous_enabled_raw, "true");
    assert_eq!(snapshot.prompt_segment_enabled, "true");
    assert_eq!(snapshot.auto_refresh_enabled, "true");
    assert_eq!(snapshot.auto_refresh_min_days, "9");
}

use gemini_cli::config;
use nils_test_support::{EnvGuard, GlobalStateLock};

#[test]
fn config_show_with_io_prints_effective_values() {
    let lock = GlobalStateLock::new();
    let _model = EnvGuard::set(&lock, "GEMINI_CLI_MODEL", "m1");
    let _reasoning = EnvGuard::set(&lock, "GEMINI_CLI_REASONING", "low");
    let _dangerous = EnvGuard::set(&lock, "GEMINI_ALLOW_DANGEROUS_ENABLED", "true");
    let _secret = EnvGuard::set(&lock, "GEMINI_SECRET_DIR", "/tmp/secrets");
    let _auth = EnvGuard::set(&lock, "GEMINI_AUTH_FILE", "/tmp/auth.json");
    let _cache = EnvGuard::set(&lock, "GEMINI_SECRET_CACHE_DIR", "/tmp/cache/secrets");
    let _prompt_segment = EnvGuard::set(&lock, "GEMINI_PROMPT_SEGMENT_ENABLED", "true");
    let _auto_refresh = EnvGuard::set(&lock, "GEMINI_AUTO_REFRESH_ENABLED", "true");
    let _min_days = EnvGuard::set(&lock, "GEMINI_AUTO_REFRESH_MIN_DAYS", "9");

    let mut out: Vec<u8> = Vec::new();
    assert_eq!(config::show_with_io(&mut out), 0);
    let output = String::from_utf8_lossy(&out);
    assert!(output.contains("GEMINI_CLI_MODEL=m1\n"));
    assert!(output.contains("GEMINI_CLI_REASONING=low\n"));
    assert!(output.contains("GEMINI_ALLOW_DANGEROUS_ENABLED=true\n"));
    assert!(output.contains("GEMINI_SECRET_DIR=/tmp/secrets\n"));
    assert!(output.contains("GEMINI_AUTH_FILE=/tmp/auth.json\n"));
    assert!(output.contains("GEMINI_SECRET_CACHE_DIR=/tmp/cache/secrets\n"));
    assert!(output.contains("GEMINI_PROMPT_SEGMENT_ENABLED=true\n"));
    assert!(output.contains("GEMINI_AUTO_REFRESH_ENABLED=true\n"));
    assert!(output.contains("GEMINI_AUTO_REFRESH_MIN_DAYS=9\n"));
}

#[test]
fn config_show_with_io_prints_blank_paths_when_unresolvable() {
    let lock = GlobalStateLock::new();
    let _home = EnvGuard::remove(&lock, "HOME");
    let _zdotdir = EnvGuard::remove(&lock, "ZDOTDIR");
    let _script = EnvGuard::remove(&lock, "ZSH_SCRIPT_DIR");
    let _preload = EnvGuard::remove(&lock, "_ZSH_BOOTSTRAP_PRELOAD_PATH");
    let _cache_root = EnvGuard::remove(&lock, "ZSH_CACHE_DIR");
    let _secret = EnvGuard::remove(&lock, "GEMINI_SECRET_DIR");
    let _auth = EnvGuard::remove(&lock, "GEMINI_AUTH_FILE");
    let _secret_cache = EnvGuard::remove(&lock, "GEMINI_SECRET_CACHE_DIR");

    let mut out: Vec<u8> = Vec::new();
    assert_eq!(config::show_with_io(&mut out), 0);
    let output = String::from_utf8_lossy(&out);
    assert!(output.contains("GEMINI_SECRET_DIR=\n"));
    assert!(output.contains("GEMINI_AUTH_FILE=\n"));
    assert!(output.contains("GEMINI_SECRET_CACHE_DIR=\n"));
}

#[test]
fn config_set_with_io_model_quotes_value() {
    let _lock = GlobalStateLock::new();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();

    assert_eq!(
        config::set_with_io("model", "gemini-test", &mut out, &mut err),
        0
    );
    assert_eq!(
        String::from_utf8_lossy(&out),
        "export GEMINI_CLI_MODEL='gemini-test'\n"
    );
    assert!(err.is_empty());
}

#[test]
fn config_set_with_io_reason_alias_is_supported() {
    let _lock = GlobalStateLock::new();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();

    assert_eq!(config::set_with_io("reason", "high", &mut out, &mut err), 0);
    assert_eq!(
        String::from_utf8_lossy(&out),
        "export GEMINI_CLI_REASONING='high'\n"
    );
    assert!(err.is_empty());
}

#[test]
fn config_set_with_io_dangerous_rejects_invalid_values() {
    let _lock = GlobalStateLock::new();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();

    assert_eq!(
        config::set_with_io("dangerous", "maybe", &mut out, &mut err),
        64
    );
    assert!(out.is_empty());
    assert!(
        String::from_utf8_lossy(&err).contains("dangerous must be true|false"),
        "stderr was: {}",
        String::from_utf8_lossy(&err)
    );
}

#[test]
fn config_set_with_io_unknown_key_returns_64() {
    let _lock = GlobalStateLock::new();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();

    assert_eq!(config::set_with_io("wat", "x", &mut out, &mut err), 64);
    assert!(out.is_empty());
    let stderr = String::from_utf8_lossy(&err);
    assert!(stderr.contains("unknown key"));
    assert!(stderr.contains("model|reasoning|dangerous"));
}

#[test]
fn config_set_with_io_escapes_single_quotes() {
    let _lock = GlobalStateLock::new();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();

    assert_eq!(config::set_with_io("model", "a'b", &mut out, &mut err), 0);
    assert_eq!(
        String::from_utf8_lossy(&out),
        "export GEMINI_CLI_MODEL='a'\"'\"'b'\n"
    );
    assert!(err.is_empty());
}

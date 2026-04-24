use std::env;

use nils_test_support::{CwdGuard, EnvGuard, GlobalStateLock, StubBinDir, prepend_path};

#[test]
fn env_guard_restores_original_value() {
    let lock = GlobalStateLock::new();
    let key = "NILS_TEST_SUPPORT_ENV_GUARD";
    let original = env::var(key).ok();
    {
        let _guard = EnvGuard::set(&lock, key, "value");
        assert_eq!(env::var(key).ok().as_deref(), Some("value"));
    }
    assert_eq!(env::var(key).ok(), original);
}

#[test]
fn env_guard_remove_restores_original_value() {
    let lock = GlobalStateLock::new();
    let key = "NILS_TEST_SUPPORT_ENV_REMOVE";
    let original = env::var(key).ok();
    {
        let _base = EnvGuard::set(&lock, key, "original");
        {
            let _remove = EnvGuard::remove(&lock, key);
            assert!(env::var(key).is_err());
        }
        assert_eq!(env::var(key).ok().as_deref(), Some("original"));
    }
    assert_eq!(env::var(key).ok(), original);
}

#[test]
fn cwd_guard_restores_directory() {
    let lock = GlobalStateLock::new();
    let original = env::current_dir().expect("current dir");
    let temp = tempfile::TempDir::new().expect("tempdir");
    {
        let _guard = CwdGuard::set(&lock, temp.path()).expect("set cwd");
        let current = env::current_dir().expect("cwd");
        assert_eq!(
            std::fs::canonicalize(current).expect("canonical cwd"),
            std::fs::canonicalize(temp.path()).expect("canonical temp")
        );
    }
    assert_eq!(
        std::fs::canonicalize(env::current_dir().expect("cwd")).expect("canonical cwd"),
        std::fs::canonicalize(original).expect("canonical original")
    );
}

#[test]
fn prepend_path_restores_original() {
    let lock = GlobalStateLock::new();
    let original = env::var("PATH").ok();
    {
        let _base = EnvGuard::set(&lock, "PATH", "base");
        let stub_dir = StubBinDir::new();
        {
            let _guard = prepend_path(&lock, stub_dir.path());
            let current = env::var_os("PATH").expect("PATH");
            let mut paths = env::split_paths(&current);
            assert_eq!(paths.next().expect("first path"), stub_dir.path());
        }
        assert_eq!(env::var("PATH").ok().as_deref(), Some("base"));
    }
    assert_eq!(env::var("PATH").ok(), original);
}

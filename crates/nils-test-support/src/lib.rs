use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

pub mod bin;
pub mod cmd;
pub mod fixtures;
pub mod fs;
pub mod git;
pub mod help;
pub mod http;
pub mod stubs;
pub mod tempdir;

static GLOBAL_STATE_LOCK: Mutex<()> = Mutex::new(());

pub struct GlobalStateLock {
    _guard: MutexGuard<'static, ()>,
}

impl GlobalStateLock {
    pub fn new() -> Self {
        let guard = match GLOBAL_STATE_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        Self { _guard: guard }
    }
}

impl Default for GlobalStateLock {
    fn default() -> Self {
        Self::new()
    }
}

pub struct EnvGuard {
    key: String,
    original: Option<String>,
}

impl EnvGuard {
    /// Requires holding `GlobalStateLock` to avoid concurrent global mutations.
    pub fn set(lock: &GlobalStateLock, key: &str, value: &str) -> Self {
        let _ = lock;
        let original = env::var(key).ok();
        // SAFETY: tests mutate process environment only while holding GlobalStateLock.
        unsafe { env::set_var(key, value) };
        Self {
            key: key.to_string(),
            original,
        }
    }

    /// Requires holding `GlobalStateLock` to avoid concurrent global mutations.
    pub fn remove(lock: &GlobalStateLock, key: &str) -> Self {
        let _ = lock;
        let original = env::var(key).ok();
        // SAFETY: tests mutate process environment only while holding GlobalStateLock.
        unsafe { env::remove_var(key) };
        Self {
            key: key.to_string(),
            original,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => {
                // SAFETY: tests mutate process environment only while holding GlobalStateLock.
                unsafe { env::set_var(&self.key, value) };
            }
            None => {
                // SAFETY: tests mutate process environment only while holding GlobalStateLock.
                unsafe { env::remove_var(&self.key) };
            }
        }
    }
}

pub struct CwdGuard {
    original: PathBuf,
}

impl CwdGuard {
    /// Requires holding `GlobalStateLock` to avoid concurrent global mutations.
    pub fn set(lock: &GlobalStateLock, path: &Path) -> io::Result<Self> {
        let _ = lock;
        let original = env::current_dir()?;
        env::set_current_dir(path)?;
        Ok(Self { original })
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = env::set_current_dir(&self.original);
    }
}

pub struct StubBinDir {
    dir: tempfile::TempDir,
}

impl StubBinDir {
    pub fn new() -> Self {
        Self {
            dir: tempfile::TempDir::new().expect("tempdir"),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn path_str(&self) -> String {
        self.dir.path().to_string_lossy().to_string()
    }

    pub fn write_exe(&self, name: &str, content: &str) {
        write_exe(self.path(), name, content);
    }
}

impl Default for StubBinDir {
    fn default() -> Self {
        Self::new()
    }
}

pub fn write_exe(dir: &Path, name: &str, content: &str) {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write stub");
    let mut perms = std::fs::metadata(&path).expect("meta").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&path, perms).expect("chmod stub");
}

/// Requires holding `GlobalStateLock` to avoid concurrent global mutations.
pub fn prepend_path(lock: &GlobalStateLock, dir: &Path) -> EnvGuard {
    let _ = lock;
    let mut paths: Vec<PathBuf> =
        env::split_paths(&env::var_os("PATH").unwrap_or_default()).collect();
    paths.insert(0, dir.to_path_buf());
    let joined = env::join_paths(paths).expect("join paths");
    let joined = joined.to_string_lossy().to_string();
    EnvGuard::set(lock, "PATH", &joined)
}

#[cfg(test)]
mod tests {
    use super::{GLOBAL_STATE_LOCK, GlobalStateLock};

    #[test]
    fn global_state_lock_recovers_after_poison() {
        let _ = std::panic::catch_unwind(|| {
            let _guard = GLOBAL_STATE_LOCK.lock().expect("lock should be acquired");
            panic!("intentional poison for recovery test");
        });

        let _lock = GlobalStateLock::new();
    }
}

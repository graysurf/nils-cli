//! Temp directories that report cleanup failures instead of hiding them.
//!
//! `tempfile::TempDir`'s own `Drop` calls `remove_dir_all` and discards the
//! result. A directory that cannot be removed — a fixture left read-only, a
//! child process still writing into it — therefore leaks silently and forever,
//! and nothing in the test output or CI log ever mentions it. That silence is
//! why the workspace accumulated hundreds of gigabytes of `/tmp/.tmpXXXXXX`
//! before anyone noticed.
//!
//! `ScopedTempDir` uses `TempDir::close`, which surfaces that error, and fails
//! the test that produced it.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// A `TempDir` whose cleanup failure is a test failure rather than a leak.
pub struct ScopedTempDir {
    inner: Option<TempDir>,
}

impl ScopedTempDir {
    pub fn new() -> Self {
        Self {
            inner: Some(TempDir::new().expect("create scoped tempdir")),
        }
    }

    pub fn with_prefix(prefix: &str) -> Self {
        Self {
            inner: Some(
                tempfile::Builder::new()
                    .prefix(prefix)
                    .tempdir()
                    .expect("create scoped tempdir"),
            ),
        }
    }

    pub fn path(&self) -> &Path {
        self.inner
            .as_ref()
            .expect("scoped tempdir is live until dropped")
            .path()
    }

    pub fn path_buf(&self) -> PathBuf {
        self.path().to_path_buf()
    }

    /// Remove the directory now, returning the cleanup error rather than
    /// panicking. Use when a test asserts on cleanup behaviour itself.
    pub fn close(mut self) -> std::io::Result<()> {
        match self.inner.take() {
            Some(dir) => normalize_close(dir),
            None => Ok(()),
        }
    }
}

impl Default for ScopedTempDir {
    fn default() -> Self {
        Self::new()
    }
}

/// A directory a test deliberately moved or removed is not a leak.
fn normalize_close(dir: TempDir) -> std::io::Result<()> {
    match dir.close() {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

impl Drop for ScopedTempDir {
    fn drop(&mut self) {
        let Some(dir) = self.inner.take() else {
            return;
        };
        let path = dir.path().to_path_buf();
        let Err(err) = normalize_close(dir) else {
            return;
        };
        // Never mask an in-flight failure: while unwinding, the original panic
        // is the useful one, so report and let it through.
        if std::thread::panicking() {
            eprintln!(
                "scoped tempdir cleanup failed while panicking: {} ({err})",
                path.display()
            );
            return;
        }
        panic!("scoped tempdir was left behind: {} ({err})", path.display());
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::ScopedTempDir;

    #[test]
    fn close_removes_the_directory() {
        let dir = ScopedTempDir::new();
        let path = dir.path_buf();
        std::fs::write(path.join("file"), "x").expect("write");

        dir.close().expect("close should succeed");

        assert!(!path.exists(), "close must remove the directory");
    }

    #[test]
    fn a_directory_removed_by_the_test_is_not_reported_as_a_leak() {
        let dir = ScopedTempDir::new();
        let path = dir.path_buf();
        std::fs::remove_dir_all(&path).expect("remove");

        dir.close()
            .expect("an already-removed directory is not a leak");
    }

    #[test]
    fn close_reports_a_cleanup_failure_that_plain_drop_would_hide() {
        let dir = ScopedTempDir::new();
        let path = dir.path_buf();
        let locked = path.join("locked");
        std::fs::create_dir(&locked).expect("create");
        std::fs::write(locked.join("file"), "x").expect("write");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("chmod");

        // Root and CAP_DAC_OVERRIDE ignore the mode bits this fixture relies
        // on, so the assertion below only holds for an unprivileged user.
        if std::fs::write(locked.join("probe"), "x").is_ok() {
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700))
                .expect("restore");
            return;
        }

        let err = dir.close().expect_err("cleanup failure must surface");
        assert!(
            path.exists(),
            "the directory that could not be removed must still be reported as present: {err}"
        );

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("restore");
        std::fs::remove_dir_all(&path).expect("cleanup");
    }
}

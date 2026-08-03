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

/// A directory made read-only for part of a test, restored from `Drop`.
///
/// This is the class-2 hazard in `docs/specs/test-temp-directory-policy.md`, and
/// the distinction that makes it a hazard is the *target*: unlinking an entry
/// needs write permission on its **directory**, not on the entry, so a read-only
/// directory stops `remove_dir_all` from emptying it while a read-only file does
/// not. A fixture that chmods a directory and restores it with a plain statement
/// is therefore unremovable the moment anything between those two statements
/// panics — an unwinding assertion, or a spawn that fails under load — and
/// because cleanup stops at the first error the leftovers are exactly the
/// read-only subtree.
///
/// Restoring from `Drop` closes that window. Prefer this over a manual restore
/// for any directory a test makes read-only, and pair it with [`ScopedTempDir`]
/// so a cleanup that still fails is reported rather than leaked.
#[cfg(unix)]
pub struct RestoredMode {
    path: PathBuf,
    mode: u32,
}

#[cfg(unix)]
impl RestoredMode {
    /// Set `dir` to `mode`, restoring its current mode when the guard drops.
    pub fn set(dir: &Path, mode: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let permissions = std::fs::metadata(dir)
            .unwrap_or_else(|err| panic!("read mode of {}: {err}", dir.display()))
            .permissions();
        let guard = Self {
            path: dir.to_path_buf(),
            mode: permissions.mode() & 0o7777,
        };

        let mut updated = permissions;
        updated.set_mode(mode);
        std::fs::set_permissions(dir, updated)
            .unwrap_or_else(|err| panic!("set mode of {}: {err}", dir.display()));

        guard
    }

    /// Set `dir` to `0o500`: the owner may traverse and read it but not write,
    /// which is what a fixture proving "the code under test cannot write here"
    /// usually wants.
    pub fn read_only(dir: &Path) -> Self {
        Self::set(dir, 0o500)
    }

    /// Set `dir` to `0o555`, for a fixture that also needs group and other to
    /// traverse it.
    pub fn read_only_shared(dir: &Path) -> Self {
        Self::set(dir, 0o555)
    }
}

#[cfg(unix)]
impl Drop for RestoredMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;

        // Best-effort by design: the directory may legitimately be gone already,
        // and a restore failure must never replace the test's own verdict.
        let Ok(metadata) = std::fs::metadata(&self.path) else {
            return;
        };
        let mut permissions = metadata.permissions();
        permissions.set_mode(self.mode);
        let _ = std::fs::set_permissions(&self.path, permissions);
    }
}

#[cfg(test)]
mod tests {
    use super::{RestoredMode, ScopedTempDir};

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
        // This fixture is the hazard the guard exists for, so it uses the guard:
        // `expect_err` below panics if cleanup unexpectedly succeeds, and a plain
        // restore statement after it would be skipped, leaving the read-only
        // subtree behind.
        let restored = RestoredMode::read_only(&locked);

        // Root and CAP_DAC_OVERRIDE ignore the mode bits this fixture relies
        // on, so the assertion below only holds for an unprivileged user.
        if std::fs::write(locked.join("probe"), "x").is_ok() {
            return;
        }

        let err = dir.close().expect_err("cleanup failure must surface");
        assert!(
            path.exists(),
            "the directory that could not be removed must still be reported as present: {err}"
        );

        drop(restored);
        std::fs::remove_dir_all(&path).expect("cleanup");
    }
}

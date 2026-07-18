use std::ffi::{CString, OsStr};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Debug)]
pub enum OwnedFileLockError {
    Busy,
    Failed(io::Error),
}

/// Persistent-file advisory lock held by the acquiring file descriptor.
///
/// The pathname is never unlinked during normal release. The kernel releases
/// the lock when the guard drops or its process exits, so there is no stale
/// sentinel to remove and an obsolete guard cannot unlink a successor lock.
/// This coordinates local processes on the Unix filesystems supported by the
/// project; remote filesystem lock semantics are outside this contract.
#[derive(Debug)]
pub struct OwnedFileLock {
    file: File,
}

impl OwnedFileLock {
    pub fn acquire(path: &Path) -> Result<Self, OwnedFileLockError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(OwnedFileLockError::Failed)?;
        Self::acquire_file(file)
    }

    /// Lock an existing regular-file target without following a final symlink.
    pub fn acquire_existing(path: &Path) -> Result<Self, OwnedFileLockError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(OwnedFileLockError::Failed)?;
        Self::acquire_file(file)
    }

    /// Borrow the descriptor whose kernel lock this guard owns.
    pub fn file(&self) -> &File {
        &self.file
    }

    pub(crate) fn acquire_at(parent: &File, name: &OsStr) -> Result<Self, OwnedFileLockError> {
        let name = CString::new(name.as_bytes()).map_err(|_| {
            OwnedFileLockError::Failed(io::Error::new(
                io::ErrorKind::InvalidInput,
                "lock file name contains a NUL byte",
            ))
        })?;
        // SAFETY: `parent` is a live directory descriptor, `name` is
        // NUL-terminated, and a successful descriptor is immediately owned by
        // `File`.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o644,
            )
        };
        if fd < 0 {
            return Err(OwnedFileLockError::Failed(io::Error::last_os_error()));
        }
        // SAFETY: `fd` was returned uniquely by `openat` above.
        let file = unsafe { File::from_raw_fd(fd) };
        Self::acquire_file(file)
    }

    fn acquire_file(file: File) -> Result<Self, OwnedFileLockError> {
        // SAFETY: `file` owns a valid descriptor for the duration of this call
        // and remains stored in the guard while the advisory lock is held.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            Ok(Self { file })
        } else {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::WouldBlock {
                Err(OwnedFileLockError::Busy)
            } else {
                Err(OwnedFileLockError::Failed(source))
            }
        }
    }
}

impl Drop for OwnedFileLock {
    fn drop(&mut self) {
        // SAFETY: `self.file` still owns the descriptor that acquired the lock.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_path_persists_while_kernel_lock_releases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.lock");

        let first = OwnedFileLock::acquire(&path).expect("first lock");
        assert!(matches!(
            OwnedFileLock::acquire(&path),
            Err(OwnedFileLockError::Busy)
        ));
        drop(first);

        assert!(path.exists(), "advisory lock path must remain stable");
        OwnedFileLock::acquire(&path).expect("lock released after guard drop");
    }

    #[test]
    fn owned_file_lock_process_helper() {
        let Some(lock_path) = std::env::var_os("PLAN_TOOLING_LOCK_HELPER_PATH") else {
            return;
        };
        let ready_path =
            std::env::var_os("PLAN_TOOLING_LOCK_HELPER_READY").expect("helper ready marker");
        let _held = OwnedFileLock::acquire(Path::new(&lock_path)).expect("helper lock");
        std::fs::write(ready_path, b"ready").expect("write helper ready marker");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    #[test]
    fn process_exit_releases_advisory_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("process.lock");
        let ready_path = dir.path().join("ready");
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .arg("mutation_lock::tests::owned_file_lock_process_helper")
            .arg("--exact")
            .arg("--nocapture")
            .env("PLAN_TOOLING_LOCK_HELPER_PATH", &lock_path)
            .env("PLAN_TOOLING_LOCK_HELPER_READY", &ready_path)
            .spawn()
            .expect("spawn lock helper");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready_path.exists() && std::time::Instant::now() < deadline {
            if let Some(status) = child.try_wait().expect("poll lock helper") {
                panic!("lock helper exited before acquiring the lock: {status}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !ready_path.exists() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("lock helper did not acquire the lock before timeout");
        }

        assert!(matches!(
            OwnedFileLock::acquire(&lock_path),
            Err(OwnedFileLockError::Busy)
        ));
        child.kill().expect("terminate lock-owning process");
        child.wait().expect("reap lock-owning process");

        assert!(lock_path.exists(), "advisory lock path must persist");
        OwnedFileLock::acquire(&lock_path).expect("process exit released lock");
    }
}

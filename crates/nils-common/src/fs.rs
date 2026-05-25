use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Lexically normalize `path`: drop `.` components, collapse `..` components by
/// popping the previous segment, and preserve any root or prefix (Windows
/// drive / UNC). The result is purely syntactic — the filesystem is not
/// consulted.
///
/// Behavior matches the per-crate `normalize_absolute_path` / `normalize_path`
/// helpers that previously lived in `agent-scope-lock`, `web-evidence`, and
/// `agent-workflow-primitives::test_first_evidence`.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

pub const SECRET_FILE_MODE: u32 = 0o600;
const MAX_TEMP_PATH_ATTEMPTS: u32 = 10;

#[derive(Debug, Error)]
pub enum AtomicWriteError {
    #[error("failed to create parent directory {path}: {source}")]
    CreateParentDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to create temporary file {path}: {source}")]
    CreateTempFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to create unique temporary file for {target} after {attempts} attempts")]
    TempPathExhausted { target: PathBuf, attempts: u32 },
    #[error("failed to write temporary file {path}: {source}")]
    WriteTempFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to set permissions on {path}: {source}")]
    SetPermissions {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to replace {to} from {from}: {source}")]
    ReplaceFile {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub enum TimestampError {
    #[error("failed to create parent directory {path}: {source}")]
    CreateParentDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to write timestamp file {path}: {source}")]
    WriteFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to remove timestamp file {path}: {source}")]
    RemoveFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub enum WriteTextError {
    #[error("failed to create parent directory {path}: {source}")]
    CreateParentDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to write file {path}: {source}")]
    WriteFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub enum FileHashError {
    #[error("failed to open file for hashing {path}: {source}")]
    OpenFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to read file for hashing {path}: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Compute a lowercase SHA-256 digest for a file.
pub fn sha256_file(path: &Path) -> Result<String, FileHashError> {
    let mut file = File::open(path).map_err(|source| FileHashError::OpenFile {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];

    loop {
        let read = file
            .read(&mut buf)
            .map_err(|source| FileHashError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }

    Ok(hex_encode(&hasher.finalize()))
}

/// Write bytes to `path` using a temp file + replace.
///
/// The helper creates parent directories when needed and applies `mode` on Unix.
pub fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> Result<(), AtomicWriteError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| AtomicWriteError::CreateParentDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let mut attempt = 0u32;
    loop {
        let tmp_path = temp_path(path, attempt);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(mut file) => {
                file.write_all(contents)
                    .map_err(|source| AtomicWriteError::WriteTempFile {
                        path: tmp_path.clone(),
                        source,
                    })?;
                let _ = file.flush();
                set_permissions(&tmp_path, mode).map_err(|source| {
                    AtomicWriteError::SetPermissions {
                        path: tmp_path.clone(),
                        source,
                    }
                })?;
                drop(file);

                replace_file(&tmp_path, path).map_err(|source| AtomicWriteError::ReplaceFile {
                    from: tmp_path.clone(),
                    to: path.to_path_buf(),
                    source,
                })?;
                set_permissions(path, mode).map_err(|source| AtomicWriteError::SetPermissions {
                    path: path.to_path_buf(),
                    source,
                })?;
                return Ok(());
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                attempt += 1;
                if attempt > MAX_TEMP_PATH_ATTEMPTS {
                    return Err(AtomicWriteError::TempPathExhausted {
                        target: path.to_path_buf(),
                        attempts: attempt,
                    });
                }
            }
            Err(source) => {
                return Err(AtomicWriteError::CreateTempFile {
                    path: tmp_path,
                    source,
                });
            }
        }
    }
}

/// Persist a timestamp line.
///
/// Behavior:
/// - `Some(value)`: trims at first newline and writes if non-empty.
/// - `None` or empty value: removes the file, ignoring `NotFound`.
pub fn write_timestamp(path: &Path, iso: Option<&str>) -> Result<(), TimestampError> {
    if let Some(raw) = iso {
        let trimmed = raw.split(&['\n', '\r'][..]).next().unwrap_or("");
        if !trimmed.is_empty() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| TimestampError::CreateParentDir {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            fs::write(path, trimmed).map_err(|source| TimestampError::WriteFile {
                path: path.to_path_buf(),
                source,
            })?;
            return Ok(());
        }
    }

    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(TimestampError::RemoveFile {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Write UTF-8 text to `path`, creating parent directories when needed.
pub fn write_text(path: &Path, text: &str) -> Result<(), WriteTextError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| WriteTextError::CreateParentDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    fs::write(path, text).map_err(|source| WriteTextError::WriteFile {
        path: path.to_path_buf(),
        source,
    })
}

/// Replace `to` by renaming `from` to `to`.
///
/// Notes:
/// - On Unix, `rename` overwrites atomically when `from` and `to` are on the same filesystem.
/// - On Windows, `rename` fails when `to` exists. We fall back to remove + rename, which is not
///   atomic but matches the expected overwrite behavior for temp-file workflows.
pub fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    replace_file_impl(from, to)
}

/// Alias for `replace_file` (kept for readability at call sites).
pub fn rename_overwrite(from: &Path, to: &Path) -> io::Result<()> {
    replace_file(from, to)
}

#[cfg(unix)]
fn replace_file_impl(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn replace_file_impl(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(err) => {
            // Be conservative: do not delete `to` unless we can confirm `from` exists.
            if !from.exists() {
                return Err(err);
            }

            if !to.exists() {
                return Err(err);
            }

            match fs::remove_file(to) {
                Ok(()) => {}
                Err(remove_err) if remove_err.kind() == io::ErrorKind::NotFound => {}
                Err(remove_err) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("rename failed: {err} (remove failed: {remove_err})"),
                    ));
                }
            }

            fs::rename(from, to).map_err(|err2| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("rename failed: {err} ({err2})"),
                )
            })
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn replace_file_impl(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(unix)]
fn set_permissions(path: &Path, mode: u32) -> io::Result<()> {
    let perm = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, perm)
}

#[cfg(not(unix))]
fn set_permissions(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

// Atomic-write helper, off the render path: produces a unique tempfile
// name for the write-temp-then-rename pattern used by `atomic_write`.
// The Resolved Decision #9 determinism gate covers the render pipeline
// (agent-runtime-cli's `src/render/`), which never touches this helper
// — `agent-runtime render` writes through `std::fs::write` directly.
// Allowing `SystemTime::now()` exactly here keeps the gate green
// without weakening the rule for any new render-path code.
#[allow(clippy::disallowed_methods)]
fn temp_path(path: &Path, attempt: u32) -> PathBuf {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("tmp");
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let tmp_name = format!(".{filename}.tmp-{pid}-{nanos}-{attempt}");
    path.with_file_name(tmp_name)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffer_len: usize,
    total_len: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0u8; 64],
            buffer_len: 0,
            total_len: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);

        if self.buffer_len > 0 {
            let need = 64 - self.buffer_len;
            let take = need.min(data.len());
            self.buffer[self.buffer_len..self.buffer_len + take].copy_from_slice(&data[..take]);
            self.buffer_len += take;
            data = &data[take..];

            if self.buffer_len == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffer_len = 0;
            }
        }

        while data.len() >= 64 {
            let block: [u8; 64] = data[..64].try_into().expect("64-byte block");
            self.compress(&block);
            data = &data[64..];
        }

        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffer_len = data.len();
        }
    }

    fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total_len.wrapping_mul(8);

        self.buffer[self.buffer_len] = 0x80;
        self.buffer_len += 1;

        if self.buffer_len > 56 {
            self.buffer[self.buffer_len..].fill(0);
            let block = self.buffer;
            self.compress(&block);
            self.buffer = [0u8; 64];
            self.buffer_len = 0;
        }

        self.buffer[self.buffer_len..56].fill(0);
        self.buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buffer;
        self.compress(&block);

        let mut out = [0u8; 32];
        for (index, chunk) in out.chunks_exact_mut(4).enumerate() {
            chunk.copy_from_slice(&self.state[index].to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut schedule = [0u32; 64];
        for (index, word) in schedule.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                block[offset],
                block[offset + 1],
                block[offset + 2],
                block[offset + 3],
            ]);
        }

        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }

        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];

        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(ROUND_CONSTANTS[index])
                .wrapping_add(schedule[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

const ROUND_CONSTANTS: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn normalize_path_drops_curdir_components() {
        assert_eq!(
            normalize_path(Path::new("/a/./b/./c")),
            PathBuf::from("/a/b/c")
        );
    }

    #[test]
    fn normalize_path_collapses_parent_components() {
        assert_eq!(
            normalize_path(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
    }

    #[test]
    fn normalize_path_preserves_root_when_collapsing_past_start() {
        assert_eq!(normalize_path(Path::new("/../a")), PathBuf::from("/a"));
    }

    #[test]
    fn normalize_path_preserves_relative_paths() {
        assert_eq!(normalize_path(Path::new("a/b/c")), PathBuf::from("a/b/c"));
    }

    #[test]
    fn fs_replace_file_overwrites_existing_destination() {
        let dir = TempDir::new().expect("tempdir");
        let from = dir.path().join("from.tmp");
        let to = dir.path().join("to.txt");

        fs::write(&from, "new").expect("write from");
        fs::write(&to, "old").expect("write to");

        replace_file(&from, &to).expect("replace_file");

        assert!(!from.exists(), "from should be moved away");
        assert_eq!(fs::read_to_string(&to).expect("read to"), "new");
    }

    #[test]
    fn fs_sha256_file_matches_known_hash() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("blob.txt");
        fs::write(&path, b"hello\n").expect("write file");

        let digest = sha256_file(&path).expect("sha256");

        assert_eq!(
            digest,
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
    }

    #[test]
    fn fs_sha256_file_returns_structured_open_error() {
        let dir = TempDir::new().expect("tempdir");
        let missing = dir.path().join("missing.txt");

        let err = sha256_file(&missing).expect_err("missing file should fail");

        match err {
            FileHashError::OpenFile { path, .. } => assert_eq!(path, missing),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn fs_write_atomic_creates_parent_and_writes_contents() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("nested").join("secret.json");

        write_atomic(&path, br#"{"ok":true}"#, SECRET_FILE_MODE).expect("write_atomic");

        assert_eq!(
            fs::read_to_string(&path).expect("read content"),
            r#"{"ok":true}"#
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn fs_write_atomic_returns_structured_parent_error() {
        let dir = TempDir::new().expect("tempdir");
        let parent_file = dir.path().join("not-a-directory");
        let target = parent_file.join("secret.json");
        fs::write(&parent_file, "block parent dir creation").expect("seed file");

        let err = write_atomic(&target, b"{}", SECRET_FILE_MODE)
            .expect_err("parent dir creation should fail");

        match err {
            AtomicWriteError::CreateParentDir { path, .. } => assert_eq!(path, parent_file),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn fs_write_timestamp_trims_newlines_and_writes_value() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("stamp.txt");

        write_timestamp(&path, Some("2025-01-20T00:00:00Z\n")).expect("write timestamp");

        assert_eq!(
            fs::read_to_string(&path).expect("read timestamp"),
            "2025-01-20T00:00:00Z"
        );
    }

    #[test]
    fn fs_write_timestamp_creates_parent_for_write_path() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("nested").join("stamp.txt");

        write_timestamp(&path, Some("2025-01-20T00:00:00Z")).expect("write timestamp");

        assert_eq!(
            fs::read_to_string(&path).expect("read timestamp"),
            "2025-01-20T00:00:00Z"
        );
    }

    #[test]
    fn fs_write_timestamp_removes_file_when_value_missing_or_empty() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("stamp.txt");
        fs::write(&path, "present").expect("seed timestamp");

        write_timestamp(&path, None).expect("timestamp none");
        assert!(!path.exists(), "expected timestamp file removed");

        fs::write(&path, "present").expect("seed timestamp");
        write_timestamp(&path, Some("\n")).expect("timestamp empty");
        assert!(!path.exists(), "expected timestamp file removed");
    }

    #[test]
    fn fs_write_timestamp_ignores_missing_remove_target() {
        let dir = TempDir::new().expect("tempdir");
        let missing = dir.path().join("missing.timestamp");

        write_timestamp(&missing, None).expect("missing remove should not fail");
    }

    #[test]
    fn fs_write_timestamp_remove_path_does_not_create_parent_dir() {
        let dir = TempDir::new().expect("tempdir");
        let parent = dir.path().join("missing").join("cache");
        let missing = parent.join("auth.json.timestamp");

        write_timestamp(&missing, None).expect("missing remove should not fail");

        assert!(
            !parent.exists(),
            "remove path should not create parent directories"
        );
    }

    #[test]
    fn fs_write_text_creates_parent_and_writes_contents() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("nested").join("note.md");

        write_text(&path, "hello").expect("write_text");

        assert_eq!(fs::read_to_string(&path).expect("read text"), "hello");
    }

    #[test]
    fn fs_write_text_returns_structured_parent_error() {
        let dir = TempDir::new().expect("tempdir");
        let parent_file = dir.path().join("not-a-directory");
        let target = parent_file.join("note.md");
        fs::write(&parent_file, "block parent dir creation").expect("seed file");

        let err = write_text(&target, "hello").expect_err("parent dir creation should fail");

        match err {
            WriteTextError::CreateParentDir { path, .. } => assert_eq!(path, parent_file),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }
}

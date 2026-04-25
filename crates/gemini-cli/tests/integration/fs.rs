use nils_common::fs::{SECRET_FILE_MODE, sha256_file, write_atomic, write_timestamp};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "nils-gemini-cli-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp test dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn fs_sha256_file_matches_known_hash() {
    let dir = TestDir::new("fs-sha256");
    let path = dir.path().join("blob.txt");
    fs::write(&path, b"hello\n").expect("write");

    let digest = sha256_file(&path).expect("sha256");
    assert_eq!(
        digest,
        "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
    );
}

#[test]
fn fs_write_atomic_creates_parent_and_writes_contents() {
    let dir = TestDir::new("fs-atomic");
    let path = dir.path().join("nested").join("secret.json");
    write_atomic(&path, br#"{"ok":true}"#, SECRET_FILE_MODE).expect("write_atomic");

    let content = fs::read_to_string(&path).expect("read");
    assert_eq!(content, r#"{"ok":true}"#);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn fs_write_timestamp_trims_newlines_and_writes_value() {
    let dir = TestDir::new("fs-timestamp-trim");
    let path = dir.path().join("stamp.txt");
    write_timestamp(&path, Some("2025-01-20T00:00:00Z\n")).expect("timestamp");

    let content = fs::read_to_string(&path).expect("read timestamp");
    assert_eq!(content, "2025-01-20T00:00:00Z");
}

#[test]
fn fs_write_timestamp_removes_file_when_value_missing_or_empty() {
    let dir = TestDir::new("fs-timestamp-remove");
    let path = dir.path().join("stamp.txt");
    fs::write(&path, "present").expect("seed timestamp");

    write_timestamp(&path, None).expect("timestamp none");
    assert!(!path.exists(), "expected timestamp file removed");

    fs::write(&path, "present").expect("seed timestamp");
    write_timestamp(&path, Some("\n")).expect("timestamp empty");
    assert!(!path.exists(), "expected timestamp file removed");
}

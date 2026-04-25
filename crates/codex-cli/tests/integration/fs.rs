use nils_common::fs::{SECRET_FILE_MODE, sha256_file, write_atomic, write_timestamp};
use pretty_assertions::assert_eq;
use std::fs;

#[test]
fn fs_sha256_file_matches_known_hash() {
    let dir = tempfile::TempDir::new().expect("tempdir");
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
    let dir = tempfile::TempDir::new().expect("tempdir");
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
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("stamp.txt");
    write_timestamp(&path, Some("2025-01-20T00:00:00Z\n")).expect("timestamp");

    let content = fs::read_to_string(&path).expect("read timestamp");
    assert_eq!(content, "2025-01-20T00:00:00Z");
}

#[test]
fn fs_write_timestamp_removes_file_when_value_missing_or_empty() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("stamp.txt");
    fs::write(&path, "present").expect("seed timestamp");

    write_timestamp(&path, None).expect("timestamp none");
    assert!(!path.exists(), "expected timestamp file removed");

    fs::write(&path, "present").expect("seed timestamp");
    write_timestamp(&path, Some("\n")).expect("timestamp empty");
    assert!(!path.exists(), "expected timestamp file removed");
}

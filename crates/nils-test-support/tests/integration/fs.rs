use std::fs as std_fs;

use nils_test_support::fs;
use serde_json::json;

#[test]
fn write_text_creates_parents_and_writes_contents() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let path = temp.path().join("nested/dir/file.txt");
    let written = fs::write_text(&path, "hello\n");
    assert_eq!(std_fs::read_to_string(written).expect("read"), "hello\n");
}

#[test]
fn write_text_in_dir_joins_relative_path_and_writes_contents() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let written = fs::write_text_in_dir(temp.path(), "nested/dir/file.txt", "hello\n");
    assert_eq!(std_fs::read_to_string(written).expect("read"), "hello\n");
}

#[test]
fn write_bytes_preserves_raw_bytes() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let path = temp.path().join("bin/data.bin");
    let data = [0u8, 159, 146, 150, 255];
    let written = fs::write_bytes(&path, &data);
    assert_eq!(std_fs::read(written).expect("read"), data);
}

#[test]
fn write_json_writes_pretty_json() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let path = temp.path().join("config/settings.json");
    let value = json!({"ok": true, "count": 2});
    let written = fs::write_json(&path, &value);
    let actual = std_fs::read_to_string(written).expect("read");
    let expected = serde_json::to_string_pretty(&value).expect("json");
    assert_eq!(actual, expected);
}

#[test]
fn write_executable_writes_contents() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let path = temp.path().join("bin/run.sh");
    let written = fs::write_executable(&path, "#!/bin/sh\necho ok\n");
    assert_eq!(
        std_fs::read_to_string(written).expect("read"),
        "#!/bin/sh\necho ok\n"
    );
}

#[test]
fn write_executable_in_dir_joins_relative_path() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let written = fs::write_executable_in_dir(temp.path(), "bin/run.sh", "#!/bin/sh\necho ok\n");
    assert_eq!(
        std_fs::read_to_string(written).expect("read"),
        "#!/bin/sh\necho ok\n"
    );
}

#[cfg(unix)]
#[test]
fn write_executable_sets_unix_mode() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::TempDir::new().expect("tempdir");
    let path = temp.path().join("bin/tool");
    let written = fs::write_executable(&path, "echo ok\n");
    let mode = std_fs::metadata(written)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111);
}

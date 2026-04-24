use std::io::{Cursor, Read};

use api_testing_core::cli_io::read_response_bytes;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

struct FailRead;

impl Read for FailRead {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("boom"))
    }
}

#[test]
fn read_response_bytes_from_stdin() {
    let mut input = Cursor::new(b"hello world".to_vec());
    let bytes = read_response_bytes("-", &mut input).unwrap();
    assert_eq!(bytes, b"hello world");
}

#[test]
fn read_response_bytes_stdin_error() {
    let mut input = FailRead;
    let err = read_response_bytes("-", &mut input).unwrap_err();
    assert_eq!(err.to_string(), "error: failed to read response from stdin");
}

#[test]
fn read_response_bytes_from_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("response.json");
    std::fs::write(&path, b"{\"ok\":true}").unwrap();
    let path_str = path.to_string_lossy().to_string();

    let mut input = Cursor::new(Vec::new());
    let bytes = read_response_bytes(&path_str, &mut input).unwrap();
    assert_eq!(bytes, b"{\"ok\":true}");
}

#[test]
fn read_response_bytes_missing_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("missing.json");
    let path_str = path.to_string_lossy().to_string();

    let mut input = Cursor::new(Vec::new());
    let err = read_response_bytes(&path_str, &mut input).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("Response file not found: {}", path.display())
    );
}

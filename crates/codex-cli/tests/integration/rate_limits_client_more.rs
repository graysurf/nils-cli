use codex_cli::rate_limits::client::{UsageRequest, fetch_usage, read_tokens};
use nils_test_support::http::{HttpResponse, LoopbackServer};
use nils_test_support::{EnvGuard, GlobalStateLock};
use pretty_assertions::assert_eq;
use std::fs;

fn write_target(dir: &tempfile::TempDir, contents: &str) -> std::path::PathBuf {
    let path = dir.path().join("target.json");
    fs::write(&path, contents).expect("write target");
    path
}

#[test]
fn rate_limits_client_read_tokens_supports_root_account_id() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let target = write_target(
        &dir,
        r#"{"tokens":{"access_token":"tok"},"account_id":"acct"}"#,
    );
    let (token, account) = read_tokens(&target).expect("tokens");
    assert_eq!(token, "tok");
    assert_eq!(account.as_deref(), Some("acct"));
}

#[test]
fn rate_limits_client_fetch_usage_errors_include_body_preview() {
    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/wham/usage",
        HttpResponse::new(500, "hello\nworld\n"),
    );

    let dir = tempfile::TempDir::new().expect("tempdir");
    let target = write_target(
        &dir,
        r#"{"tokens":{"access_token":"tok","account_id":"acct"}}"#,
    );

    let request = UsageRequest {
        target_file: target,
        refresh_on_401: false,
        base_url: server.url(),
        connect_timeout_seconds: 1,
        max_time_seconds: 3,
    };

    let err = match fetch_usage(&request) {
        Ok(_) => panic!("expected fetch_usage to error"),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("HTTP 500"));
    assert!(err.contains("body:"));
    assert!(err.contains("hello world"));
}

#[test]
fn rate_limits_client_fetch_usage_errors_without_body_when_empty() {
    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/wham/usage", HttpResponse::new(500, ""));

    let dir = tempfile::TempDir::new().expect("tempdir");
    let target = write_target(&dir, r#"{"tokens":{"access_token":"tok"}}"#);

    let request = UsageRequest {
        target_file: target,
        refresh_on_401: false,
        base_url: server.url(),
        connect_timeout_seconds: 1,
        max_time_seconds: 3,
    };

    let err = match fetch_usage(&request) {
        Ok(_) => panic!("expected fetch_usage to error"),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("HTTP 500"));
    assert!(!err.contains("body:"));
}

#[test]
fn rate_limits_client_fetch_usage_invalid_json_is_error() {
    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/wham/usage", HttpResponse::new(200, "not-json"));

    let dir = tempfile::TempDir::new().expect("tempdir");
    let target = write_target(&dir, r#"{"tokens":{"access_token":"tok"}}"#);

    let request = UsageRequest {
        target_file: target,
        refresh_on_401: false,
        base_url: server.url(),
        connect_timeout_seconds: 1,
        max_time_seconds: 3,
    };

    let err = match fetch_usage(&request) {
        Ok(_) => panic!("expected fetch_usage to error"),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("invalid JSON"));
}

#[test]
fn rate_limits_client_fetch_usage_refreshes_on_401_when_enabled() {
    let lock = GlobalStateLock::new();
    let _auth = EnvGuard::remove(&lock, "CODEX_AUTH_FILE");
    let _secrets = EnvGuard::remove(&lock, "CODEX_SECRET_DIR");

    let server = LoopbackServer::new().expect("server");
    server.add_route("GET", "/wham/usage", HttpResponse::new(401, ""));

    let dir = tempfile::TempDir::new().expect("tempdir");
    let target = write_target(&dir, r#"{"tokens":{"access_token":"tok"}}"#);

    let request = UsageRequest {
        target_file: target,
        refresh_on_401: true,
        base_url: server.url(),
        connect_timeout_seconds: 1,
        max_time_seconds: 3,
    };

    let err = match fetch_usage(&request) {
        Ok(_) => panic!("expected fetch_usage to error"),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("HTTP 401"));

    let requests = server.take_requests();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.method == "GET" && r.path == "/wham/usage")
            .count(),
        2
    );
}

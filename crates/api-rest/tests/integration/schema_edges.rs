use std::path::Path;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
use nils_test_support::fs::{write_json, write_text};
use nils_test_support::http::{HttpResponse, RecordedRequest, TestServer};

fn api_rest_bin() -> std::path::PathBuf {
    resolve("api-rest")
}

fn run_api_rest(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default().with_cwd(cwd);
    for key in [
        "REST_URL",
        "REST_TOKEN_NAME",
        "REST_HISTORY_ENABLED",
        "REST_HISTORY_FILE",
        "REST_HISTORY_LOG_URL_ENABLED",
        "REST_ENV_DEFAULT",
        "REST_JWT_VALIDATE_ENABLED",
        "ACCESS_TOKEN",
        "SERVICE_TOKEN",
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        options = options.with_env_remove(key);
    }
    options = options.with_env("NO_PROXY", "127.0.0.1,localhost");
    options = options.with_env("no_proxy", "127.0.0.1,localhost");

    for (k, v) in envs {
        options = options.with_env(k, v);
    }

    run_with(&api_rest_bin(), args, &options)
}

fn start_server() -> TestServer {
    TestServer::new(|_req: &RecordedRequest| {
        HttpResponse::new(200, r#"{"ok":true}"#).with_header("Content-Type", "application/json")
    })
    .expect("start test server")
}

#[test]
fn call_sets_default_accept_and_json_content_type() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    write_json(
        &root.join("requests/body.request.json"),
        &serde_json::json!({
            "method": "POST",
            "path": "/headers",
            "headers": { "Content-Type": "text/plain" },
            "body": { "ok": true },
            "expect": { "status": 200 }
        }),
    );

    let server = start_server();

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            &server.url(),
            "--no-history",
            "requests/body.request.json",
        ],
        &[("REST_JWT_VALIDATE_ENABLED", "false")],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];
    assert_eq!(
        req.header_value("accept").as_deref(),
        Some("application/json")
    );
    assert_eq!(
        req.header_value("content-type").as_deref(),
        Some("application/json")
    );
}

#[test]
fn call_accept_overrides_default_and_authorization_ignored() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    write_json(
        &root.join("requests/accept.request.json"),
        &serde_json::json!({
            "method": "GET",
            "path": "/accept",
            "headers": {
                "Accept": "text/plain",
                "Authorization": "Bearer wrong"
            },
            "expect": { "status": 200 }
        }),
    );

    let server = start_server();

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            &server.url(),
            "--no-history",
            "requests/accept.request.json",
        ],
        &[
            ("ACCESS_TOKEN", "env-token"),
            ("REST_JWT_VALIDATE_ENABLED", "false"),
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];
    assert_eq!(req.header_value("accept").as_deref(), Some("text/plain"));
    assert_eq!(
        req.header_value("authorization").as_deref(),
        Some("Bearer env-token")
    );
}

#[test]
fn query_string_is_encoded_and_sorted_in_error() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    write_json(
        &root.join("requests/query.request.json"),
        &serde_json::json!({
            "method": "GET",
            "path": "/query",
            "query": {
                "b": ["2", "1"],
                "a": "hello world"
            },
            "expect": { "status": 200 }
        }),
    );

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            "http://127.0.0.1:9",
            "--no-history",
            "requests/query.request.json",
        ],
        &[("REST_JWT_VALIDATE_ENABLED", "false")],
    );
    assert_eq!(out.code, 1);
    let stderr = out.stderr_text();
    assert!(
        stderr
            .contains("HTTP request failed: GET http://127.0.0.1:9/query?a=hello%20world&b=2&b=1")
    );
}

#[test]
fn invalid_header_key_is_error() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    write_json(
        &root.join("requests/bad-header.request.json"),
        &serde_json::json!({
            "method": "GET",
            "path": "/bad",
            "headers": { "Bad Header": "x" },
            "expect": { "status": 200 }
        }),
    );

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            "http://127.0.0.1:9",
            "--no-history",
            "requests/bad-header.request.json",
        ],
        &[("REST_JWT_VALIDATE_ENABLED", "false")],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("invalid header key"));
}

#[test]
fn body_and_multipart_are_rejected() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    write_json(
        &root.join("requests/body-multipart.request.json"),
        &serde_json::json!({
            "method": "POST",
            "path": "/bad",
            "body": { "ok": true },
            "multipart": [],
            "expect": { "status": 200 }
        }),
    );

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            "http://127.0.0.1:9",
            "--no-history",
            "requests/body-multipart.request.json",
        ],
        &[("REST_JWT_VALIDATE_ENABLED", "false")],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("both body and multipart"));
}

#[test]
fn schema_rejects_query_object_values() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    write_text(
        &root.join("requests/bad-query.request.json"),
        r#"{"method":"GET","path":"/bad","query":{"q":{"x":1}}}"#,
    );

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            "http://127.0.0.1:9",
            "--no-history",
            "requests/bad-query.request.json",
        ],
        &[("REST_JWT_VALIDATE_ENABLED", "false")],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("query values must be scalars"));
}

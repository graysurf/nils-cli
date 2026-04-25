use std::path::Path;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
use nils_test_support::fs::{write_bytes, write_json};
use nils_test_support::http::{HttpResponse, RecordedRequest, TestServer};

fn api_rest_bin() -> std::path::PathBuf {
    resolve("api-rest")
}

fn run_api_rest(cwd: &Path, args: &[&str]) -> CmdOutput {
    let options = CmdOptions::default().with_cwd(cwd);
    run_with(&api_rest_bin(), args, &options)
}

fn start_server() -> TestServer {
    TestServer::new(
        |req: &RecordedRequest| match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/cleanup/main") => HttpResponse::new(200, r#"{"key":"abc"}"#)
                .with_header("Content-Type", "application/json"),
            ("GET", "/cleanup/fail") => HttpResponse::new(200, r#"{"key":"bad"}"#)
                .with_header("Content-Type", "application/json"),
            ("DELETE", "/files/images/abc") => HttpResponse::new(204, ""),
            ("DELETE", "/files/images/bad") => HttpResponse::new(500, "cleanup failed"),
            ("GET", "/expect/status") => HttpResponse::new(500, r#"{"ok":false}"#)
                .with_header("Content-Type", "application/json"),
            ("GET", "/expect/text") => {
                HttpResponse::new(500, "boom").with_header("Content-Type", "text/plain")
            }
            ("GET", "/expect/jq") => HttpResponse::new(200, r#"{"ok":false}"#)
                .with_header("Content-Type", "application/json"),
            ("POST", "/upload") => HttpResponse::new(200, r#"{"ok":true}"#)
                .with_header("Content-Type", "application/json"),
            ("GET", "/health") => HttpResponse::new(200, r#"{"ok":true}"#)
                .with_header("Content-Type", "application/json"),
            _ => HttpResponse::new(404, "not found"),
        },
    )
    .expect("start test server")
}

#[test]
fn call_expect_status_success_and_cleanup_runs() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/rest");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let server = start_server();

    let request_file = root.join("requests/cleanup.request.json");
    write_json(
        &request_file,
        &serde_json::json!({
            "method": "GET",
            "path": "/cleanup/main",
            "expect": { "status": 200 },
            "cleanup": {
                "method": "DELETE",
                "pathTemplate": "/files/images/{{key}}",
                "vars": { "key": ".key" },
                "expectStatus": 204
            }
        }),
    );

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            &server.url(),
            "requests/cleanup.request.json",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("\"key\""));

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].method, "GET");
    assert_eq!(reqs[0].path, "/cleanup/main");
    assert_eq!(reqs[1].method, "DELETE");
    assert_eq!(reqs[1].path, "/files/images/abc");
}

#[test]
fn call_expect_status_failure_exits_nonzero_and_keeps_json_body_stdout() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    let server = start_server();

    let request_file = root.join("requests/expect-status.request.json");
    write_json(
        &request_file,
        &serde_json::json!({
            "method": "GET",
            "path": "/expect/status",
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
            &server.url(),
            "requests/expect-status.request.json",
        ],
    );
    assert_eq!(out.code, 1);
    assert!(out.stdout_text().contains("\"ok\""));
    let stderr = out.stderr_text();
    assert!(stderr.contains("Expected HTTP status 200 but got 500."));
    assert!(!stderr.contains("Response body (non-JSON"));
}

#[test]
fn call_expect_jq_failure_exits_nonzero() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    let server = start_server();

    let request_file = root.join("requests/expect-jq.request.json");
    write_json(
        &request_file,
        &serde_json::json!({
            "method": "GET",
            "path": "/expect/jq",
            "expect": { "status": 200, "jq": ".ok == true" }
        }),
    );

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            &server.url(),
            "requests/expect-jq.request.json",
        ],
    );
    assert_eq!(out.code, 1);
    assert!(out.stdout_text().contains("\"ok\""));
    let stderr = out.stderr_text();
    assert!(stderr.contains("expect.jq failed: .ok == true"));
}

#[test]
fn call_multipart_upload_reads_file_relative_to_request_file() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    let server = start_server();

    let request_dir = root.join("requests");
    let request_file = request_dir.join("upload.request.json");
    let payload_file = request_dir.join("sample.txt");
    write_bytes(&payload_file, b"hello-multipart");

    write_json(
        &request_file,
        &serde_json::json!({
            "method": "POST",
            "path": "/upload",
            "multipart": [
                { "name": "file", "filePath": "./sample.txt", "contentType": "text/plain" }
            ],
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
            &server.url(),
            "requests/upload.request.json",
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let reqs = server.take_requests();
    let upload = reqs
        .iter()
        .find(|r| r.method == "POST" && r.path == "/upload")
        .expect("upload request recorded");

    let ct = upload.header_value("content-type").unwrap_or_default();
    assert!(ct.starts_with("multipart/"), "content-type={ct}");
    assert!(
        upload
            .body
            .windows(b"hello-multipart".len())
            .any(|w| w == b"hello-multipart"),
        "multipart body should include file content"
    );
}

#[test]
fn call_expect_status_failure_non_json_prints_body_to_stderr() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    let server = start_server();

    let request_file = root.join("requests/expect-text.request.json");
    write_json(
        &request_file,
        &serde_json::json!({
            "method": "GET",
            "path": "/expect/text",
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
            &server.url(),
            "requests/expect-text.request.json",
        ],
    );
    assert_eq!(out.code, 1);
    let stderr = out.stderr_text();
    assert!(stderr.contains("Expected HTTP status 200 but got 500."));
    assert!(stderr.contains("Response body (non-JSON; first"));
}

#[test]
fn call_cleanup_failure_exits_nonzero() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/rest");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let server = start_server();

    let request_file = root.join("requests/cleanup-fail.request.json");
    write_json(
        &request_file,
        &serde_json::json!({
            "method": "GET",
            "path": "/cleanup/fail",
            "expect": { "status": 200 },
            "cleanup": {
                "method": "DELETE",
                "pathTemplate": "/files/images/{{key}}",
                "vars": { "key": ".key" },
                "expectStatus": 204
            }
        }),
    );

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            &server.url(),
            "requests/cleanup-fail.request.json",
        ],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("cleanup failed: expected"));
}

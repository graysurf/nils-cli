//! Behavioral coverage for the capture flow branches the happy-path `cli`
//! suite does not exercise: text rendering, HEAD/empty/non-text/truncated body
//! handling, request-failure artifacts, input validation, relative output
//! directories, and the bash completion contract.

use std::fs;
use std::net::TcpListener;
use std::path::Path;

use nils_test_support::cmd::{CmdOutput, run_resolved_in_dir};
use nils_test_support::http::{HttpResponse, TestServer};
use pretty_assertions::assert_eq;

fn run(dir: &Path, args: &[&str]) -> CmdOutput {
    run_resolved_in_dir(
        "web-evidence",
        dir,
        args,
        &[
            ("http_proxy", ""),
            ("https_proxy", ""),
            ("HTTP_PROXY", ""),
            ("HTTPS_PROXY", ""),
        ],
        None,
    )
}

fn out_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// A loopback address with nothing listening, so a request reliably refuses.
fn closed_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    format!("http://{addr}/closed")
}

#[test]
fn capture_text_format_prints_human_summary() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let server = TestServer::new(|_| {
        HttpResponse::new(200, "plain body").with_header("Content-Type", "text/plain")
    })
    .expect("server");
    let out_dir = tmp.path().join("evidence");

    // Default format is text.
    let output = run(
        tmp.path(),
        &[
            "capture",
            &server.url(),
            "--out",
            &out_arg(&out_dir),
            "--label",
            "smoke",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(stdout.contains("web evidence captured:"), "stdout={stdout}");
    assert!(stdout.contains("label: smoke"), "stdout={stdout}");
    assert!(stdout.contains("status: 200 success"), "stdout={stdout}");
    assert!(stdout.contains("artifacts:"), "stdout={stdout}");
}

#[test]
fn capture_text_format_error_reports_artifact_dir() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let server = TestServer::new(|_| {
        HttpResponse::new(503, "down").with_header("Content-Type", "text/plain")
    })
    .expect("server");
    let out_dir = tmp.path().join("evidence");

    let output = run(
        tmp.path(),
        &["capture", &server.url(), "--out", &out_arg(&out_dir)],
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    let stderr = output.stderr_text();
    assert!(stderr.contains("web-evidence: error:"), "stderr={stderr}");
    assert!(stderr.contains("artifact dir:"), "stderr={stderr}");
}

#[test]
fn head_request_records_no_body_preview() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let server = TestServer::new(|_| {
        HttpResponse::new(200, "ignored for head").with_header("Content-Type", "text/plain")
    })
    .expect("server");
    let out_dir = tmp.path().join("evidence");

    let output = run(
        tmp.path(),
        &[
            "capture",
            &server.url(),
            "--out",
            &out_arg(&out_dir),
            "--method",
            "head",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let body = fs::read_to_string(out_dir.join("body-preview.redacted.txt")).expect("body");
    assert!(
        body.contains("HEAD request; no response body captured."),
        "body={body}"
    );
}

#[test]
fn empty_body_is_reported() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let server =
        TestServer::new(|_| HttpResponse::new(200, "").with_header("Content-Type", "text/plain"))
            .expect("server");
    let out_dir = tmp.path().join("evidence");

    let output = run(
        tmp.path(),
        &["capture", &server.url(), "--out", &out_arg(&out_dir)],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let body = fs::read_to_string(out_dir.join("body-preview.redacted.txt")).expect("body");
    assert!(body.contains("Response body was empty."), "body={body}");
}

#[test]
fn non_text_body_is_omitted() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let server = TestServer::new(|_| {
        HttpResponse::new(200, "\u{0}\u{1}binary")
            .with_header("Content-Type", "application/octet-stream")
    })
    .expect("server");
    let out_dir = tmp.path().join("evidence");

    let output = run(
        tmp.path(),
        &["capture", &server.url(), "--out", &out_arg(&out_dir)],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let body = fs::read_to_string(out_dir.join("body-preview.redacted.txt")).expect("body");
    assert!(
        body.contains("Non-text response body omitted."),
        "body={body}"
    );
    assert!(body.contains("application/octet-stream"), "body={body}");
}

#[test]
fn large_body_is_truncated_before_preview() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let server = TestServer::new(|_| {
        HttpResponse::new(200, "0123456789ABCDEF").with_header("Content-Type", "text/plain")
    })
    .expect("server");
    let out_dir = tmp.path().join("evidence");

    let output = run(
        tmp.path(),
        &[
            "capture",
            &server.url(),
            "--out",
            &out_arg(&out_dir),
            "--format",
            "json",
            "--max-body-bytes",
            "10",
            "--body-preview-bytes",
            "5",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["result"]["body_truncated"], true);
    assert_eq!(value["result"]["body_bytes_captured"], 10);

    let body = fs::read_to_string(out_dir.join("body-preview.redacted.txt")).expect("body");
    assert!(
        body.contains("[body preview truncated before redaction]"),
        "body={body}"
    );
}

#[test]
fn connection_failure_writes_failure_artifacts() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("evidence");

    let output = run(
        tmp.path(),
        &[
            "capture",
            &closed_url(),
            "--out",
            &out_arg(&out_dir),
            "--format",
            "json",
            "--timeout-seconds",
            "3",
        ],
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    let value = output.stdout_json();
    assert_eq!(value["ok"], false);
    let code = value["error"]["code"].as_str().expect("error code");
    assert!(
        !code.is_empty(),
        "expected a classified error code, got {value}"
    );
    assert_eq!(
        value["error"]["details"]["artifact_dir"].as_str().unwrap(),
        out_dir.to_string_lossy()
    );

    // The failure path still persists the redacted artifact bundle.
    assert!(out_dir.join("summary.json").is_file());
    assert!(out_dir.join("headers.redacted.json").is_file());
    let body = fs::read_to_string(out_dir.join("body-preview.redacted.txt")).expect("body");
    assert!(body.contains("No response body captured."), "body={body}");
}

#[test]
fn zero_timeout_is_usage_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("evidence");
    let output = run(
        tmp.path(),
        &[
            "capture",
            "https://example.test",
            "--out",
            &out_arg(&out_dir),
            "--format",
            "json",
            "--timeout-seconds",
            "0",
        ],
    );
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_json()["error"]["code"], "invalid-timeout");
}

#[test]
fn zero_max_body_bytes_is_usage_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("evidence");
    let output = run(
        tmp.path(),
        &[
            "capture",
            "https://example.test",
            "--out",
            &out_arg(&out_dir),
            "--format",
            "json",
            "--max-body-bytes",
            "0",
        ],
    );
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "invalid-max-body-bytes"
    );
}

#[test]
fn zero_body_preview_bytes_is_usage_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("evidence");
    let output = run(
        tmp.path(),
        &[
            "capture",
            "https://example.test",
            "--out",
            &out_arg(&out_dir),
            "--format",
            "json",
            "--body-preview-bytes",
            "0",
        ],
    );
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "invalid-body-preview-bytes"
    );
}

#[test]
fn malformed_url_is_usage_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("evidence");
    let output = run(
        tmp.path(),
        &[
            "capture",
            "not a url",
            "--out",
            &out_arg(&out_dir),
            "--format",
            "json",
        ],
    );
    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_json()["error"]["code"], "invalid-url");
}

#[test]
fn relative_out_dir_is_resolved_against_cwd() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let server =
        TestServer::new(|_| HttpResponse::new(200, "ok").with_header("Content-Type", "text/plain"))
            .expect("server");

    let output = run(
        tmp.path(),
        &["capture", &server.url(), "--out", "evidence-rel"],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(
        tmp.path().join("evidence-rel/summary.json").is_file(),
        "relative --out should resolve under the working directory"
    );
}

#[test]
fn completion_bash_exports_script() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["completion", "bash"]);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(
        output.stdout_text().contains("web-evidence"),
        "bash completion should mention the binary"
    );
}

use std::fs;
use std::path::Path;

use nils_test_support::cmd::{CmdOutput, run_resolved_in_dir};
use nils_test_support::http::{HttpResponse, TestServer};
use pretty_assertions::assert_eq;
use serde_json::Value;

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

fn combined_output(output: &CmdOutput) -> String {
    format!("{}{}", output.stdout_text(), output.stderr_text())
}

#[test]
fn help_includes_version_flag_and_examples() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["--help"]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("-V, --version"),
        "missing version flag: {stdout}"
    );
    assert!(stdout.contains("EXAMPLES:"), "missing examples: {stdout}");
}

#[test]
fn capture_success_writes_redacted_artifact_bundle_and_json() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let server = TestServer::new(|_| {
        HttpResponse::new(200, "hello access_token=secret-value sk-proj-supersecret")
            .with_header("Content-Type", "text/plain")
            .with_header("Set-Cookie", "sid=secret-cookie")
            .with_header("X-Trace", "trace-token=trace-secret")
    })
    .expect("server");
    let out_dir = tmp.path().join("evidence");
    let out_arg = out_arg(&out_dir);
    let url = format!("{}/page?token=query-secret&ok=1", server.url());

    let output = run(
        tmp.path(),
        &[
            "capture", &url, "--out", &out_arg, "--label", "smoke", "--format", "json",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.web-evidence.capture.v1");
    assert_eq!(value["command"], "web-evidence capture");
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["status_code"], 200);
    assert_eq!(value["result"]["status_class"], "success");
    assert_eq!(value["result"]["label"], "smoke");
    assert!(
        value["result"]["requested_url"]
            .as_str()
            .unwrap()
            .contains("token=%5BREDACTED%5D")
    );

    let summary = fs::read_to_string(out_dir.join("summary.json")).expect("summary");
    let headers = fs::read_to_string(out_dir.join("headers.redacted.json")).expect("headers");
    let body = fs::read_to_string(out_dir.join("body-preview.redacted.txt")).expect("body");
    let combined = format!("{summary}\n{headers}\n{body}\n{}", combined_output(&output));

    assert!(summary.contains("\"schema_version\": \"web-evidence.summary.v1\""));
    assert!(headers.contains("\"set-cookie\""));
    assert!(headers.contains("[REDACTED]"));
    assert!(body.contains("[REDACTED]"));
    assert!(!combined.contains("query-secret"));
    assert!(!combined.contains("secret-cookie"));
    assert!(!combined.contains("secret-value"));
    assert!(!combined.contains("sk-proj-supersecret"));
}

#[test]
fn http_status_failure_is_classified_and_keeps_artifacts() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let server = TestServer::new(|_| {
        HttpResponse::new(500, r#"{"error":"service down","password":"secret"}"#)
            .with_header("Content-Type", "application/json")
    })
    .expect("server");
    let out_dir = tmp.path().join("failure-evidence");
    let out_arg = out_arg(&out_dir);

    let output = run(
        tmp.path(),
        &[
            "capture",
            &server.url(),
            "--out",
            &out_arg,
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 1, "stdout={}", output.stdout_text());
    let value = output.stdout_json();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "http-status-error");
    assert_eq!(value["error"]["details"]["status_code"], 500);
    assert_eq!(
        value["error"]["details"]["artifact_dir"].as_str().unwrap(),
        out_dir.to_string_lossy()
    );
    assert!(out_dir.join("summary.json").is_file());
    assert!(out_dir.join("headers.redacted.json").is_file());
    assert!(out_dir.join("body-preview.redacted.txt").is_file());

    let summary: Value =
        serde_json::from_str(&fs::read_to_string(out_dir.join("summary.json")).expect("summary"))
            .expect("summary json");
    assert_eq!(summary["ok"], false);
    assert_eq!(summary["error"]["code"], "http-status-error");
    assert_eq!(summary["result"]["status_code"], 500);

    let body = fs::read_to_string(out_dir.join("body-preview.redacted.txt")).expect("body");
    assert!(body.contains("password"));
    assert!(!body.contains("secret"));
}

#[test]
fn invalid_url_scheme_returns_json_usage_error_without_network() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("invalid");
    let out_arg = out_arg(&out_dir);

    let output = run(
        tmp.path(),
        &[
            "capture",
            "file:///tmp/example",
            "--out",
            &out_arg,
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 64, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.web-evidence.capture.v1");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "unsupported-url-scheme");
    assert!(!out_dir.exists(), "invalid URL should not create artifacts");
}

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["completion", "zsh"]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(
        output.stdout_text().contains("#compdef web-evidence"),
        "missing completion header: {}",
        output.stdout_text()
    );
}

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

fn base_options(cwd: &Path) -> CmdOptions {
    let mut options = CmdOptions::default().with_cwd(cwd);
    for key in [
        "REST_URL",
        "REST_TOKEN_NAME",
        "REST_HISTORY_ENABLED",
        "REST_HISTORY_FILE",
        "REST_HISTORY_LOG_URL_ENABLED",
        "REST_ENV_DEFAULT",
        "REST_REPORT_DIR",
        "REST_REPORT_INCLUDE_COMMAND_ENABLED",
        "REST_REPORT_COMMAND_LOG_URL_ENABLED",
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
    options
}

fn run_api_rest(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    let mut options = base_options(cwd);
    for (k, v) in envs {
        options = options.with_env(k, v);
    }
    run_with(&api_rest_bin(), args, &options)
}

fn run_api_rest_with_stdin(
    cwd: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
    stdin: &str,
) -> CmdOutput {
    let mut options = base_options(cwd).with_stdin_str(stdin);
    for (k, v) in envs {
        options = options.with_env(k, v);
    }
    run_with(&api_rest_bin(), args, &options)
}

fn start_server() -> TestServer {
    TestServer::new(|_req: &RecordedRequest| {
        HttpResponse::new(200, r#"{"ok":true,"token":"secret"}"#)
            .with_header("Content-Type", "application/json")
    })
    .expect("start test server")
}

fn write_health_request(root: &Path) {
    write_json(
        &root.join("requests/health.request.json"),
        &serde_json::json!({
            "method": "GET",
            "path": "/health",
            "expect": { "status": 200 },
            "body": { "token": "secret" }
        }),
    );
}

#[test]
fn report_response_file_redacts_by_default() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    write_health_request(root);

    let response_file = root.join("response.json");
    write_text(&response_file, r#"{"ok":true,"token":"secret"}"#);

    let out_path = root.join("report.md");
    let out = run_api_rest(
        root,
        &[
            "report",
            "--case",
            "Health",
            "--request",
            "requests/health.request.json",
            "--response",
            response_file.to_string_lossy().as_ref(),
            "--out",
            out_path.to_string_lossy().as_ref(),
            "--url",
            "http://example.test",
        ],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out_path.is_file());

    let report = std::fs::read_to_string(&out_path).expect("read report");
    assert!(report.contains("Result: (response provided; request not executed)"));
    assert!(report.contains("Endpoint: --url http://example.test"));
    assert!(report.contains("<REDACTED>"));
    assert!(!report.contains("\"token\": \"secret\""));
}

#[test]
fn report_no_redact_and_no_command_url() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    write_health_request(root);

    let response_file = root.join("response.json");
    write_text(&response_file, r#"{"ok":true,"token":"secret"}"#);

    let out_path = root.join("report.md");
    let out = run_api_rest(
        root,
        &[
            "report",
            "--case",
            "Health",
            "--request",
            "requests/health.request.json",
            "--response",
            response_file.to_string_lossy().as_ref(),
            "--out",
            out_path.to_string_lossy().as_ref(),
            "--url",
            "http://example.test",
            "--no-redact",
            "--no-command-url",
        ],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let report = std::fs::read_to_string(&out_path).expect("read report");
    assert!(report.contains("\"token\": \"secret\""));
    assert!(report.contains("--url '<omitted>'"));
    assert!(report.contains("Endpoint: --url http://example.test"));
}

#[test]
fn report_reads_response_from_stdin() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    write_health_request(root);

    let out_path = root.join("report.md");
    let out = run_api_rest_with_stdin(
        root,
        &[
            "report",
            "--case",
            "Health",
            "--request",
            "requests/health.request.json",
            "--response",
            "-",
            "--out",
            out_path.to_string_lossy().as_ref(),
        ],
        &[],
        r#"{"ok":true}"#,
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out_path.is_file());
}

#[test]
fn report_run_mode_writes_default_path_and_passes() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    write_health_request(root);

    let server = start_server();

    let out = run_api_rest(
        root,
        &[
            "report",
            "--case",
            "Health",
            "--request",
            "requests/health.request.json",
            "--run",
            "--url",
            &server.url(),
            "--project-root",
            root.to_string_lossy().as_ref(),
            "--config-dir",
            "setup/rest",
        ],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let stdout = out.stdout_text();
    let report_path = stdout.trim();
    assert!(!report_path.is_empty(), "stdout={stdout}");
    assert!(std::path::Path::new(report_path).is_file());
    let report = std::fs::read_to_string(report_path).expect("read report");
    assert!(report.contains("Result: PASS"));
}

#[test]
fn report_response_jq_assertion_passes() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    write_json(
        &root.join("requests/jq.request.json"),
        &serde_json::json!({
            "method": "GET",
            "path": "/health",
            "expect": { "status": 200, "jq": ".ok == true" }
        }),
    );

    let response_file = root.join("response.json");
    write_text(&response_file, r#"{"ok":true}"#);

    let out_path = root.join("report.md");
    let out = run_api_rest(
        root,
        &[
            "report",
            "--case",
            "Health",
            "--request",
            "requests/jq.request.json",
            "--response",
            response_file.to_string_lossy().as_ref(),
            "--out",
            out_path.to_string_lossy().as_ref(),
            "--url",
            "http://example.test",
        ],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let report = std::fs::read_to_string(&out_path).expect("read report");
    assert!(report.contains("expect.status: 200 (NOT_EVALUATED)"));
    assert!(report.contains("expect.jq: .ok == true (PASS)"));
}

#[test]
fn report_response_jq_assertion_fails() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    write_json(
        &root.join("requests/jq.request.json"),
        &serde_json::json!({
            "method": "GET",
            "path": "/health",
            "expect": { "status": 200, "jq": ".ok == true" }
        }),
    );

    let response_file = root.join("response.json");
    write_text(&response_file, r#"{"ok":false}"#);

    let out_path = root.join("report.md");
    let out = run_api_rest(
        root,
        &[
            "report",
            "--case",
            "Health",
            "--request",
            "requests/jq.request.json",
            "--response",
            response_file.to_string_lossy().as_ref(),
            "--out",
            out_path.to_string_lossy().as_ref(),
            "--url",
            "http://example.test",
        ],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let report = std::fs::read_to_string(&out_path).expect("read report");
    assert!(report.contains("expect.jq: .ok == true (FAIL)"));
}

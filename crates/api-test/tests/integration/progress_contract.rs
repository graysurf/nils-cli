use std::path::{Path, PathBuf};

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
use nils_test_support::fs::write_text;
use nils_test_support::http::{HttpResponse, RecordedRequest, TestServer};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn api_test_bin() -> PathBuf {
    resolve("api-test")
}

fn start_server() -> TestServer {
    TestServer::new(
        |req: &RecordedRequest| match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/health") => HttpResponse::new(200, r#"{"ok":true}"#)
                .with_header("Content-Type", "application/json"),
            _ => HttpResponse::new(404, r#"{"error":"not_found"}"#)
                .with_header("Content-Type", "application/json"),
        },
    )
    .expect("start test server")
}

fn setup_suite(root: &Path, server: &TestServer) {
    std::fs::create_dir_all(root.join(".git")).expect("mkdir .git");
    write_text(
        &root.join("setup/rest/requests/health.request.json"),
        r#"{"method":"GET","path":"/health"}"#,
    );

    let suite_json = serde_json::json!({
      "version": 1,
      "name": "progress",
      "defaults": {
        "env": "local",
        "noHistory": true,
        "rest": { "url": server.url() }
      },
      "cases": [
        { "id": "rest.health", "type": "rest", "request": "setup/rest/requests/health.request.json" }
      ]
    });
    write_text(
        &root.join("tests/api/suites/progress.suite.json"),
        &serde_json::to_string_pretty(&suite_json).expect("suite json"),
    );
}

fn run_progress_suite(cwd: &Path, output_dir: &str, extra_env: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default().with_cwd(cwd);
    for key in [
        "ACCESS_TOKEN",
        "SERVICE_TOKEN",
        "REST_TOKEN_NAME",
        "GQL_JWT_NAME",
        "API_TEST_PROGRESS",
        "API_TEST_REST_URL",
        "API_TEST_GQL_URL",
        "API_TEST_GRPC_URL",
        "API_TEST_WS_URL",
        "NO_COLOR",
    ] {
        options = options.with_env_remove(key);
    }
    options = options.with_env("API_TEST_OUTPUT_DIR", output_dir);
    for (k, v) in extra_env {
        options = options.with_env(k, v);
    }

    run_with(&api_test_bin(), &["run", "--suite", "progress"], &options)
}

fn has_sgr_color_sequence(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() {
                let b = bytes[i];
                i += 1;
                if b == b'm' {
                    return true;
                }
                if b.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        i += 1;
    }
    false
}

#[test]
fn progress_auto_and_on_are_silent_in_non_tty_and_keep_stdout_json_clean() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let server = start_server();
    setup_suite(root, &server);

    let auto = run_progress_suite(root, "out/api-test-progress-auto", &[]);
    assert_eq!(
        auto.code,
        0,
        "stdout={}\nstderr={}",
        auto.stdout_text(),
        auto.stderr_text()
    );
    let auto_stdout = auto.stdout_text();
    let auto_stderr = auto.stderr_text();
    let auto_json: serde_json::Value =
        serde_json::from_slice(&auto.stdout).expect("auto stdout json");
    assert_eq!(auto_json["summary"]["total"], 1);
    assert!(!auto_stdout.contains("api-test "), "stdout leaked progress");
    assert!(
        !auto_stderr.contains("api-test "),
        "stderr showed progress in auto/non-tty"
    );
    assert!(
        auto_stderr.contains("api-test-runner: suite="),
        "missing run summary stderr line"
    );

    let on = run_progress_suite(
        root,
        "out/api-test-progress-on",
        &[("API_TEST_PROGRESS", "on")],
    );
    assert_eq!(
        on.code,
        0,
        "stdout={}\nstderr={}",
        on.stdout_text(),
        on.stderr_text()
    );
    let on_stdout = on.stdout_text();
    let on_stderr = on.stderr_text();
    let on_json: serde_json::Value = serde_json::from_slice(&on.stdout).expect("on stdout json");
    assert_eq!(on_json["summary"]["total"], 1);
    assert!(!on_stdout.contains("api-test "), "stdout leaked progress");
    assert!(
        !on_stderr.contains("api-test "),
        "non-TTY contract should keep progress quiet even when API_TEST_PROGRESS=on; stderr={on_stderr}"
    );
    assert!(
        on_stderr.contains("api-test-runner: suite="),
        "missing run summary stderr line"
    );
}

#[test]
fn progress_off_disables_progress_output() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let server = start_server();
    setup_suite(root, &server);

    let out = run_progress_suite(
        root,
        "out/api-test-progress-off",
        &[("API_TEST_PROGRESS", "off")],
    );
    assert_eq!(
        out.code,
        0,
        "stdout={}\nstderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let stdout = out.stdout_text();
    let stderr = out.stderr_text();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout json");
    assert_eq!(json["summary"]["total"], 1);
    assert!(!stdout.contains("api-test "), "stdout leaked progress");
    assert!(
        !stderr.contains("api-test "),
        "stderr showed progress while API_TEST_PROGRESS=off"
    );
}

#[test]
fn progress_with_no_color_keeps_json_clean_and_avoids_sgr_color_sequences() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let server = start_server();
    setup_suite(root, &server);

    let out = run_progress_suite(
        root,
        "out/api-test-progress-no-color",
        &[("API_TEST_PROGRESS", "on"), ("NO_COLOR", "1")],
    );
    assert_eq!(
        out.code,
        0,
        "stdout={}\nstderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let stdout = out.stdout_text();
    let stderr = out.stderr_text();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout json");
    assert_eq!(json["summary"]["total"], 1);
    assert!(!stdout.contains("api-test "), "stdout leaked progress");
    assert!(
        !has_sgr_color_sequence(&stderr),
        "stderr contains SGR color escapes under NO_COLOR: {stderr:?}"
    );
}

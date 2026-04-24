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
fn call_discovers_setup_dir_from_request_parent() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests/sub")).expect("mkdir requests");

    write_json(
        &root.join("requests/sub/health.request.json"),
        &serde_json::json!({
            "method": "GET",
            "path": "/health",
            "expect": { "status": 200 }
        }),
    );

    let server = start_server();
    let out = run_api_rest(
        root,
        &[
            "call",
            "--url",
            &server.url(),
            "requests/sub/health.request.json",
        ],
        &[
            ("REST_HISTORY_ENABLED", "true"),
            ("REST_JWT_VALIDATE_ENABLED", "false"),
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let history_file = root.join("setup/rest/.rest_history");
    assert!(history_file.is_file());
    assert!(!root.join("requests/sub/.rest_history").exists());
}

#[test]
fn history_discovers_setup_dir_from_cwd_upwards() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/rest");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");
    std::fs::create_dir_all(root.join("project/sub")).expect("mkdir project");

    let history_file = setup_dir.join(".rest_history");
    write_text(
        &history_file,
        "# stamp exit=0 setup_dir=.\napi-rest call \\\n  --config-dir 'setup/rest' \\\n  requests/health.request.json \\\n| jq .\n\n",
    );

    let out = run_api_rest(
        &root.join("project/sub"),
        &["history", "--tail", "1", "--command-only"],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("api-rest call"));
}

#[test]
fn call_with_missing_config_dir_errors() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    write_json(
        &root.join("requests/health.request.json"),
        &serde_json::json!({
            "method": "GET",
            "path": "/health",
            "expect": { "status": 200 }
        }),
    );

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "missing-dir",
            "--url",
            "http://127.0.0.1:9",
            "requests/health.request.json",
        ],
        &[("REST_JWT_VALIDATE_ENABLED", "false")],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr_text()
            .contains("Failed to resolve setup dir (try --config-dir).")
    );
}

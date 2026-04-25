use std::path::Path;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
use nils_test_support::fs::write_json;
use nils_test_support::http::{HttpResponse, RecordedRequest, TestServer};

fn api_rest_bin() -> std::path::PathBuf {
    resolve("api-rest")
}

fn run_api_rest(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default()
        .with_cwd(cwd)
        .with_env_remove_many(&[
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
        ])
        .with_env("NO_PROXY", "127.0.0.1,localhost")
        .with_env("no_proxy", "127.0.0.1,localhost");

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

fn write_health_request(root: &Path) {
    write_json(
        &root.join("requests/health.request.json"),
        &serde_json::json!({
            "method": "GET",
            "path": "/health",
            "expect": { "status": 200 }
        }),
    );
}

#[test]
fn history_written_with_command_snippet_and_url() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    write_health_request(root);

    let server = start_server();

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            &server.url(),
            "requests/health.request.json",
        ],
        &[("REST_HISTORY_ENABLED", "true")],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let history_file = root.join("setup/rest/.rest_history");
    assert!(history_file.is_file());
    let content = std::fs::read_to_string(&history_file).expect("read history");
    assert!(content.contains("exit=0"));
    assert!(content.contains("setup_dir=setup/rest"));
    assert!(content.contains("api-rest call"));
    assert!(content.contains("--config-dir 'setup/rest'"));
    assert!(content.contains("--url"));
    assert!(content.contains("requests/health.request.json"));
    assert!(content.contains("| jq ."));
}

#[test]
fn history_disabled_by_flag_and_env() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    write_health_request(root);

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
            "requests/health.request.json",
        ],
        &[("REST_HISTORY_ENABLED", "true")],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let history_file = root.join("setup/rest/.rest_history");
    assert!(!history_file.exists());

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            &server.url(),
            "requests/health.request.json",
        ],
        &[("REST_HISTORY_ENABLED", "false")],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(!history_file.exists());
}

#[test]
fn history_url_omitted_when_log_disabled() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    write_health_request(root);

    let server = start_server();
    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            &server.url(),
            "requests/health.request.json",
        ],
        &[("REST_HISTORY_LOG_URL_ENABLED", "false")],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let history_file = root.join("setup/rest/.rest_history");
    let content = std::fs::read_to_string(&history_file).expect("read history");
    assert!(content.contains("url=<omitted>"));
    assert!(!content.contains(&server.url()));
}

#[test]
fn history_records_service_token_env_source() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    write_health_request(root);

    let server = start_server();
    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            &server.url(),
            "requests/health.request.json",
        ],
        &[
            ("REST_HISTORY_ENABLED", "true"),
            ("SERVICE_TOKEN", "svc-token"),
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let history_file = root.join("setup/rest/.rest_history");
    let content = std::fs::read_to_string(&history_file).expect("read history");
    assert!(content.contains("auth=SERVICE_TOKEN"));
    assert!(!content.contains("auth=ACCESS_TOKEN"));
}

#[test]
fn history_file_override_writes_to_custom_path() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    write_health_request(root);

    let server = start_server();
    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--url",
            &server.url(),
            "requests/health.request.json",
        ],
        &[("REST_HISTORY_FILE", "custom/history.log")],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let default_file = root.join("setup/rest/.rest_history");
    assert!(!default_file.exists());

    let override_file = root.join("setup/rest/custom/history.log");
    assert!(override_file.is_file());
}

#[test]
fn history_command_outputs_tail_and_command_only() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/rest");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup");

    let history_file = setup_dir.join(".rest_history");
    std::fs::write(
        &history_file,
        "# stamp exit=0 setup_dir=.\napi-rest call \\\n  --config-dir 'setup/rest' \\\n  requests/one.request.json \\\n| jq .\n\n# stamp exit=0 setup_dir=.\napi-rest call \\\n  --config-dir 'setup/rest' \\\n  requests/two.request.json \\\n| jq .\n\n",
    )
    .expect("write history");

    let out = run_api_rest(
        root,
        &[
            "history",
            "--config-dir",
            "setup/rest",
            "--tail",
            "1",
            "--command-only",
        ],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let stdout = out.stdout_text();
    assert!(stdout.contains("api-rest call"));
    assert!(stdout.contains("requests/two.request.json"));
    assert!(!stdout.contains("stamp exit"));
}

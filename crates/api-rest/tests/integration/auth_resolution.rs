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
    for (key, _) in std::env::vars() {
        options = options.with_env_remove(&key);
    }
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
fn token_profile_cli_wins_over_env_and_file() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    write_health_request(root);

    write_text(
        &root.join("setup/rest/tokens.env"),
        "REST_TOKEN_NAME=file\nREST_TOKEN_FILE=file-token\nREST_TOKEN_ENV=env-token\nREST_TOKEN_CLI=cli-token\n",
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
            "--token",
            "cli",
            "requests/health.request.json",
        ],
        &[
            ("REST_TOKEN_NAME", "env"),
            ("REST_JWT_VALIDATE_ENABLED", "false"),
            ("REST_HISTORY_ENABLED", "false"),
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("\"ok\""));

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].header_value("authorization").unwrap_or_default(),
        "Bearer cli-token"
    );
}

#[test]
fn token_profile_env_wins_over_tokens_file() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    write_health_request(root);

    write_text(
        &root.join("setup/rest/tokens.env"),
        "REST_TOKEN_NAME=file\nREST_TOKEN_FILE=file-token\nREST_TOKEN_ENV=env-token\n",
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
            "requests/health.request.json",
        ],
        &[
            ("REST_TOKEN_NAME", "env"),
            ("REST_JWT_VALIDATE_ENABLED", "false"),
            ("REST_HISTORY_ENABLED", "false"),
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("\"ok\""));

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].header_value("authorization").unwrap_or_default(),
        "Bearer env-token"
    );
}

#[test]
fn token_profile_from_tokens_local_env_is_used_when_cli_and_env_missing() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    write_health_request(root);

    write_text(
        &root.join("setup/rest/tokens.env"),
        "REST_TOKEN_NAME=file\nREST_TOKEN_FILE=file-token\n",
    );
    write_text(
        &root.join("setup/rest/tokens.local.env"),
        "REST_TOKEN_NAME=local\nREST_TOKEN_LOCAL=local-token\n",
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
            "requests/health.request.json",
        ],
        &[
            ("REST_JWT_VALIDATE_ENABLED", "false"),
            ("REST_HISTORY_ENABLED", "false"),
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("\"ok\""));

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].header_value("authorization").unwrap_or_default(),
        "Bearer local-token"
    );
}

#[test]
fn unknown_token_profile_error_lists_available_suffixes() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    write_health_request(root);

    write_text(
        &root.join("setup/rest/tokens.env"),
        "REST_TOKEN_NAME=alpha\nREST_TOKEN_ALPHA=alpha-token\n",
    );
    write_text(
        &root.join("setup/rest/tokens.local.env"),
        "REST_TOKEN_BETA=beta-token\n",
    );

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--token",
            "missing",
            "requests/health.request.json",
        ],
        &[
            ("REST_JWT_VALIDATE_ENABLED", "false"),
            ("REST_HISTORY_ENABLED", "false"),
        ],
    );
    assert_eq!(out.code, 1);
    let stderr = out.stderr_text();
    assert!(stderr.contains("Token profile 'missing' is empty/missing"));
    assert!(stderr.contains("available: alpha beta"));
    assert!(!stderr.contains("available: name"));
}

#[test]
fn token_profile_selected_without_tokens_files_shows_available_none() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup");
    write_health_request(root);

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--token",
            "cli",
            "requests/health.request.json",
        ],
        &[
            ("REST_JWT_VALIDATE_ENABLED", "false"),
            ("REST_HISTORY_ENABLED", "false"),
        ],
    );
    assert_eq!(out.code, 1);
    let stderr = out.stderr_text();
    assert!(stderr.contains("Token profile 'cli' is empty/missing"));
    assert!(stderr.contains("available: none"));
}

#[test]
fn access_token_fallback_works_without_tokens_files_when_no_profile_selected() {
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
            ("ACCESS_TOKEN", "access-fallback-token"),
            ("REST_JWT_VALIDATE_ENABLED", "false"),
            ("REST_HISTORY_ENABLED", "false"),
        ],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("\"ok\""));

    let reqs = server.take_requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].header_value("authorization").unwrap_or_default(),
        "Bearer access-fallback-token"
    );
}

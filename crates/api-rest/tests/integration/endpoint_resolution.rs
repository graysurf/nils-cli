use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
use nils_test_support::fs::{write_json, write_text};
use nils_test_support::http::{HttpResponse, TestServer};

fn api_rest_bin() -> std::path::PathBuf {
    resolve("api-rest")
}

fn run_api_rest(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default().with_cwd(cwd);
    // Keep tests hermetic and deterministic: avoid inheriting user proxy or REST_* env vars.
    for key in [
        "REST_URL",
        "REST_TOKEN_NAME",
        "REST_HISTORY_ENABLED",
        "REST_HISTORY_FILE",
        "REST_HISTORY_LOG_URL_ENABLED",
        "REST_ENV_DEFAULT",
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

fn write_health_request(root: &Path) {
    let request_file = root.join("requests/health.request.json");
    write_json(
        &request_file,
        &serde_json::json!({
            "method": "GET",
            "path": "/health",
            "expect": { "status": 200 }
        }),
    );
}

fn start_json_server(body: &'static [u8]) -> (TestServer, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_for_handler = Arc::clone(&hits);
    let body = String::from_utf8_lossy(body).to_string();
    let server = TestServer::new(move |_req| {
        hits_for_handler.fetch_add(1, Ordering::SeqCst);
        HttpResponse::new(200, body.clone()).with_header("Content-Type", "application/json")
    })
    .expect("start test server");
    (server, hits)
}

#[test]
fn endpoint_precedence_url_wins_even_if_env_and_rest_url_set() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup/rest");
    write_health_request(root);

    let (server_url, server_url_hits) = start_json_server(br#"{"server":"explicit-url"}"#);
    let (server_rest_url, server_rest_url_hits) = start_json_server(br#"{"server":"rest-url"}"#);

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--no-history",
            "--url",
            &server_url.url(),
            "--env",
            "staging",
            "requests/health.request.json",
        ],
        &[("REST_URL", &server_rest_url.url())],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(
        out.stdout_text().contains("\"explicit-url\""),
        "stdout={}",
        out.stdout_text()
    );
    assert_eq!(server_url_hits.load(Ordering::SeqCst), 1);
    assert_eq!(server_rest_url_hits.load(Ordering::SeqCst), 0);
}

#[test]
fn endpoint_precedence_env_wins_over_rest_url() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/rest");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup/rest");
    write_health_request(root);

    let (server_env, server_env_hits) = start_json_server(br#"{"server":"env"}"#);
    let (server_rest_url, server_rest_url_hits) = start_json_server(br#"{"server":"rest-url"}"#);

    write_text(
        &setup_dir.join("endpoints.env"),
        &format!("REST_URL_STAGING={}\n", server_env.url()),
    );

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--no-history",
            "--env",
            "staging",
            "requests/health.request.json",
        ],
        &[("REST_URL", &server_rest_url.url())],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(
        out.stdout_text().contains("\"env\""),
        "stdout={}",
        out.stdout_text()
    );
    assert_eq!(server_env_hits.load(Ordering::SeqCst), 1);
    assert_eq!(server_rest_url_hits.load(Ordering::SeqCst), 0);
}

#[test]
fn env_as_url_https_does_not_require_endpoints_env() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup/rest");
    write_health_request(root);

    // Bind+close to find an unused local port; request should fail, but must not mention endpoints.env.
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let https_base = format!("https://127.0.0.1:{port}");

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--no-history",
            "--env",
            &https_base,
            "requests/health.request.json",
        ],
        &[],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr_text()
            .contains("HTTP request failed: GET https://127.0.0.1"),
        "stderr={}",
        out.stderr_text()
    );
    assert!(
        !out.stderr_text().contains("endpoints.env not found"),
        "stderr={}",
        out.stderr_text()
    );
}

#[test]
fn endpoint_precedence_rest_url_wins_over_rest_env_default() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/rest");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup/rest");
    write_health_request(root);

    let (server_rest_url, server_rest_url_hits) = start_json_server(br#"{"server":"rest-url"}"#);
    let (server_default, server_default_hits) = start_json_server(br#"{"server":"default-env"}"#);

    write_text(
        &setup_dir.join("endpoints.env"),
        &format!(
            "REST_ENV_DEFAULT=staging\nREST_URL_STAGING={}\n",
            server_default.url()
        ),
    );

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--no-history",
            "requests/health.request.json",
        ],
        &[("REST_URL", &server_rest_url.url())],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(
        out.stdout_text().contains("\"rest-url\""),
        "stdout={}",
        out.stdout_text()
    );
    assert_eq!(server_rest_url_hits.load(Ordering::SeqCst), 1);
    assert_eq!(server_default_hits.load(Ordering::SeqCst), 0);
}

#[test]
fn rest_env_default_selects_endpoint_from_files() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/rest");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup/rest");
    write_health_request(root);

    let (server_default, server_default_hits) = start_json_server(br#"{"server":"default-env"}"#);
    write_text(
        &setup_dir.join("endpoints.env"),
        &format!(
            "REST_ENV_DEFAULT=staging\nREST_URL_STAGING={}\n",
            server_default.url()
        ),
    );

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--no-history",
            "requests/health.request.json",
        ],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(
        out.stdout_text().contains("\"default-env\""),
        "stdout={}",
        out.stdout_text()
    );
    assert_eq!(server_default_hits.load(Ordering::SeqCst), 1);
}

#[test]
fn missing_endpoints_env_errors_for_named_env() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/rest")).expect("mkdir setup/rest");
    write_health_request(root);

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--no-history",
            "--env",
            "staging",
            "requests/health.request.json",
        ],
        &[],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr_text()
            .contains("endpoints.env not found (expected under setup/rest/)"),
        "stderr={}",
        out.stderr_text()
    );
}

#[test]
fn unknown_env_lists_available_suffixes_including_local_env() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup_dir = root.join("setup/rest");
    std::fs::create_dir_all(&setup_dir).expect("mkdir setup/rest");
    write_health_request(root);

    write_text(
        &setup_dir.join("endpoints.env"),
        "REST_URL_STAGING=http://example.invalid\n",
    );
    write_text(
        &setup_dir.join("endpoints.local.env"),
        "REST_URL_DEV=http://example.invalid\n",
    );

    let out = run_api_rest(
        root,
        &[
            "call",
            "--config-dir",
            "setup/rest",
            "--no-history",
            "--env",
            "prod",
            "requests/health.request.json",
        ],
        &[],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr_text().contains("Unknown --env 'prod'"),
        "stderr={}",
        out.stderr_text()
    );
    assert!(
        out.stderr_text().contains("available: dev staging"),
        "stderr={}",
        out.stderr_text()
    );
}

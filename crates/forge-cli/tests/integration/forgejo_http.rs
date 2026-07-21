//! Native HTTP coverage for named Forgejo providers. All endpoints are served
//! from loopback fixtures; these tests never contact a live forge.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use nils_test_support::http::{HttpResponse, TestServer};
use pretty_assertions::assert_eq;

use super::support::{StubEnv, parse_envelope, run_forge_cli};

const PROVIDER: &str = "forgejo-test";
const TOKEN_ENV: &str = "FORGE_CLI_TEST_FORGEJO_TOKEN";
const TOKEN: &str = "forgejo-test-secret-value";

fn configured_stub(base_url: &str) -> StubEnv {
    let stub = StubEnv::new().env(TOKEN_ENV, TOKEN);
    let out = run_forge_cli(
        &stub,
        &[
            "--format",
            "json",
            "provider",
            "add",
            PROVIDER,
            "--kind",
            "forgejo",
            "--base-url",
            base_url,
            "--token-env",
            TOKEN_ENV,
        ],
    );
    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    stub
}

fn version_response() -> HttpResponse {
    HttpResponse::new(200, r#"{"version":"15.0.5"}"#)
        .with_header("Content-Type", "application/json")
}

#[test]
fn forgejo_auth_status_discovers_version_and_uses_token_reference() {
    let server = TestServer::new(|request| match request.path.as_str() {
        "/api/v1/version" => version_response(),
        "/api/v1/user" => HttpResponse::new(200, r#"{"login":"alice"}"#),
        path => HttpResponse::new(404, format!("unexpected path: {path}")),
    })
    .expect("loopback server");
    let stub = configured_stub(&server.url());

    let out = run_forge_cli(
        &stub,
        &["--provider", PROVIDER, "--format", "json", "auth", "status"],
    );

    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.auth.status.v1");
    assert_eq!(envelope["data"]["provider"], PROVIDER);
    assert_eq!(envelope["data"]["user"], "alice");
    assert_eq!(envelope["data"]["scopes"], serde_json::json!([]));

    let requests = server.take_requests();
    assert_eq!(requests.len(), 2, "requests={requests:?}");
    assert_eq!(requests[0].path, "/api/v1/version");
    assert_eq!(requests[1].path, "/api/v1/user");
    let expected_authorization = format!("token {TOKEN}");
    for request in requests {
        assert_eq!(
            request.header_value("authorization").as_deref(),
            Some(expected_authorization.as_str())
        );
    }
}

#[test]
fn forgejo_repo_view_preserves_the_v1_envelope() {
    let server = TestServer::new(|request| match request.path.as_str() {
        "/api/v1/version" => version_response(),
        "/api/v1/repos/acme/widgets" => HttpResponse::new(
            200,
            r#"{
                "name":"widgets",
                "owner":{"login":"acme"},
                "html_url":"https://forge.example/acme/widgets",
                "default_branch":"main",
                "allow_merge_commits":true,
                "allow_squash_merge":true,
                "allow_rebase":false
            }"#,
        ),
        path => HttpResponse::new(404, format!("unexpected path: {path}")),
    })
    .expect("loopback server");
    let stub = configured_stub(&server.url());

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            PROVIDER,
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "repo",
            "view",
        ],
    );

    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.repo.view.v1");
    assert_eq!(envelope["data"]["provider"], PROVIDER);
    assert_eq!(envelope["data"]["owner"], "acme");
    assert_eq!(envelope["data"]["name"], "widgets");
    assert_eq!(envelope["data"]["default_branch"], "main");
    assert_eq!(
        envelope["data"]["merge_methods_allowed"],
        serde_json::json!(["squash", "merge"])
    );
}

#[test]
fn forgejo_issue_list_paginates_and_excludes_pull_requests() {
    let issue_page = Arc::new(AtomicUsize::new(0));
    let issue_page_for_server = Arc::clone(&issue_page);
    let server = TestServer::new(move |request| match request.path.as_str() {
        "/api/v1/version" => version_response(),
        "/api/v1/repos/acme/widgets/issues" => {
            let page = issue_page_for_server.fetch_add(1, Ordering::SeqCst);
            if page == 0 {
                let mut rows = vec![serde_json::json!({
                    "number": 1,
                    "html_url": "https://forge.example/acme/widgets/issues/1",
                    "state": "open",
                    "title": "first issue",
                    "labels": [{"name": "bug"}],
                    "user": {"login": "alice"},
                    "assignees": [{"login": "bob"}],
                    "pull_request": null
                })];
                rows.extend((2..=50).map(|number| {
                    serde_json::json!({
                        "number": number,
                        "html_url": format!("https://forge.example/acme/widgets/pulls/{number}"),
                        "state": "open",
                        "title": format!("pull request {number}"),
                        "pull_request": {"url": format!("https://forge.example/api/pulls/{number}")}
                    })
                }));
                HttpResponse::new(200, serde_json::to_string(&rows).expect("page JSON"))
            } else {
                HttpResponse::new(
                    200,
                    serde_json::json!([{
                        "number": 51,
                        "html_url": "https://forge.example/acme/widgets/issues/51",
                        "state": "closed",
                        "title": "second issue",
                        "labels": [],
                        "user": {"login": "carol"},
                        "assignees": [],
                        "pull_request": null
                    }])
                    .to_string(),
                )
            }
        }
        path => HttpResponse::new(404, format!("unexpected path: {path}")),
    })
    .expect("loopback server");
    let stub = configured_stub(&server.url());

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            PROVIDER,
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "issue",
            "list",
            "--state",
            "all",
            "--limit",
            "2",
        ],
    );

    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    let envelope = parse_envelope(&out.stdout);
    assert_eq!(envelope["schema_version"], "cli.forge-cli.issue.list.v1");
    assert_eq!(envelope["data"]["provider"], PROVIDER);
    assert_eq!(envelope["data"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(envelope["data"]["items"][0]["number"], 1);
    assert_eq!(envelope["data"]["items"][1]["number"], 51);
    assert_eq!(issue_page.load(Ordering::SeqCst), 2);
}

#[test]
fn forgejo_rejects_an_unknown_major_before_protected_calls() {
    let protected_calls = Arc::new(AtomicUsize::new(0));
    let protected_calls_for_server = Arc::clone(&protected_calls);
    let server = TestServer::new(move |request| match request.path.as_str() {
        "/api/v1/version" => HttpResponse::new(200, r#"{"version":"99.0.0"}"#),
        _ => {
            protected_calls_for_server.fetch_add(1, Ordering::SeqCst);
            HttpResponse::new(200, r#"{"login":"must-not-run"}"#)
        }
    })
    .expect("loopback server");
    let stub = configured_stub(&server.url());

    let out = run_forge_cli(
        &stub,
        &["--provider", PROVIDER, "--format", "json", "auth", "status"],
    );

    assert_eq!(out.code, 69, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(
        parse_envelope(&out.stdout)["error"]["code"],
        "forgejo_version_unsupported"
    );
    assert_eq!(protected_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn forgejo_enforces_request_deadline_and_response_body_bound() {
    let slow = TestServer::new(|_| {
        thread::sleep(Duration::from_millis(200));
        version_response()
    })
    .expect("slow loopback server");
    let slow_stub = configured_stub(&slow.url()).env("FORGE_CLI_FORGEJO_TIMEOUT_MS", "40");
    let timed_out = run_forge_cli(
        &slow_stub,
        &["--provider", PROVIDER, "--format", "json", "auth", "status"],
    );
    assert_eq!(timed_out.code, 69, "stderr={}", timed_out.stderr);
    assert_eq!(
        parse_envelope(&timed_out.stdout)["error"]["code"],
        "forgejo_timeout"
    );

    let oversized = TestServer::new(|_| HttpResponse::new(200, "x".repeat(1024 * 1024 + 1)))
        .expect("oversized loopback server");
    let oversized_stub = configured_stub(&oversized.url());
    let too_large = run_forge_cli(
        &oversized_stub,
        &["--provider", PROVIDER, "--format", "json", "auth", "status"],
    );
    assert_eq!(too_large.code, 69, "stderr={}", too_large.stderr);
    assert_eq!(
        parse_envelope(&too_large.stdout)["error"]["code"],
        "forgejo_response_too_large"
    );
}

#[test]
fn forgejo_allows_only_same_origin_redirects_with_credentials() {
    let same_origin = TestServer::new(|request| match request.path.as_str() {
        "/api/v1/version" => HttpResponse::new(302, "")
            .with_header("Location", "/api/v1/version-final")
            .with_header("Connection", "close"),
        "/api/v1/version-final" => version_response(),
        "/api/v1/user" => HttpResponse::new(200, r#"{"login":"alice"}"#),
        path => HttpResponse::new(404, format!("unexpected path: {path}")),
    })
    .expect("same-origin loopback server");
    let same_origin_stub = configured_stub(&same_origin.url());
    let allowed = run_forge_cli(
        &same_origin_stub,
        &["--provider", PROVIDER, "--format", "json", "auth", "status"],
    );
    assert_eq!(
        allowed.code, 0,
        "stdout={} stderr={}",
        allowed.stdout, allowed.stderr
    );
    let redirected = same_origin.take_requests();
    assert_eq!(redirected.len(), 3, "requests={redirected:?}");
    assert_eq!(redirected[1].path, "/api/v1/version-final");
    let expected_authorization = format!("token {TOKEN}");
    assert_eq!(
        redirected[1].header_value("authorization").as_deref(),
        Some(expected_authorization.as_str())
    );

    let foreign = TestServer::new(|_| HttpResponse::new(200, r#"{"version":"15.0.5"}"#))
        .expect("foreign loopback server");
    let foreign_url = foreign.url();
    let redirector = TestServer::new(move |_| {
        HttpResponse::new(302, "")
            .with_header("Location", &format!("{foreign_url}/api/v1/version"))
            .with_header("Connection", "close")
    })
    .expect("redirector loopback server");
    let redirect_stub = configured_stub(&redirector.url());
    let blocked = run_forge_cli(
        &redirect_stub,
        &["--provider", PROVIDER, "--format", "json", "auth", "status"],
    );
    assert_eq!(
        blocked.code, 69,
        "stdout={} stderr={}",
        blocked.stdout, blocked.stderr
    );
    assert_eq!(
        parse_envelope(&blocked.stdout)["error"]["code"],
        "forgejo_redirect_forbidden"
    );
    assert!(foreign.take_requests().is_empty());
    assert!(!format!("{}{}", blocked.stdout, blocked.stderr).contains(TOKEN));
}

#[test]
fn forgejo_redacts_token_from_remote_errors() {
    let server =
        TestServer::new(|_| HttpResponse::new(500, format!("remote reflected secret: {TOKEN}")))
            .expect("loopback server");
    let stub = configured_stub(&server.url());

    let out = run_forge_cli(
        &stub,
        &["--provider", PROVIDER, "--format", "json", "auth", "status"],
    );

    assert_eq!(out.code, 1, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(
        parse_envelope(&out.stdout)["error"]["code"],
        "backend_error"
    );
    assert!(!format!("{}{}", out.stdout, out.stderr).contains(TOKEN));
}

#[test]
fn forgejo_unsupported_mutations_fail_closed_without_http() {
    let server =
        TestServer::new(|_| HttpResponse::new(500, "must not be called")).expect("loopback server");
    let stub = configured_stub(&server.url());

    let out = run_forge_cli(
        &stub,
        &[
            "--provider",
            PROVIDER,
            "--repo",
            "acme/widgets",
            "--format",
            "json",
            "issue",
            "create",
            "--title",
            "must not run",
            "--body",
            "must not run",
        ],
    );

    assert_eq!(out.code, 64, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(
        parse_envelope(&out.stdout)["error"]["code"],
        "provider_unsupported"
    );
    assert!(server.take_requests().is_empty());
    assert!(!format!("{}{}", out.stdout, out.stderr).contains(TOKEN));
}

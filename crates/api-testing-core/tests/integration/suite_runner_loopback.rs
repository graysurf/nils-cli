use std::collections::HashSet;
use std::path::PathBuf;

use api_testing_core::suite::runner::{SuiteRunOptions, run_suite};
use api_testing_core::suite::safety::MSG_WRITE_CASES_DISABLED;
use api_testing_core::suite::schema::load_and_validate_suite;
use nils_test_support::fixtures::write_text;
use nils_test_support::http::{HttpResponse, LoopbackServer};
use tempfile::TempDir;

fn resolve_output_path(root: &std::path::Path, rel: &str) -> PathBuf {
    let path = PathBuf::from(rel);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

#[test]
fn suite_runner_loopback_runs_and_cleans_up() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();

    write_text(
        &root.join("requests/health.request.json"),
        r#"{"method":"GET","path":"/health","expect":{"status":200}}"#,
    );
    write_text(
        &root.join("requests/write.request.json"),
        r#"{"method":"POST","path":"/write","body":{"ok":true},"expect":{"status":200}}"#,
    );
    write_text(&root.join("ops/health.graphql"), "query Health { ok }\n");

    let suite_json = serde_json::json!({
        "version": 1,
        "defaults": { "noHistory": true },
        "cases": [
            {
                "id": "rest.health",
                "type": "rest",
                "request": "requests/health.request.json",
                "cleanup": {
                    "type": "rest",
                    "method": "DELETE",
                    "pathTemplate": "/cleanup/{{id}}",
                    "vars": { "id": ".data.id" }
                }
            },
            {
                "id": "rest.write",
                "type": "rest",
                "allowWrite": true,
                "request": "requests/write.request.json"
            },
            {
                "id": "graphql.health",
                "type": "graphql",
                "op": "ops/health.graphql"
            }
        ]
    });
    let suite_path = root.join("suite.json");
    write_text(
        &suite_path,
        &serde_json::to_string_pretty(&suite_json).unwrap(),
    );

    let server = LoopbackServer::new().expect("server");
    server.add_route(
        "GET",
        "/health",
        HttpResponse::new(200, r#"{"data":{"id":"123"}}"#),
    );
    server.add_route("POST", "/write", HttpResponse::new(200, r#"{"ok":true}"#));
    server.add_route("DELETE", "/cleanup/123", HttpResponse::new(204, ""));
    server.add_route(
        "POST",
        "/graphql",
        HttpResponse::new(200, r#"{"data":{"ok":true}}"#),
    );

    let loaded = load_and_validate_suite(&suite_path).expect("load suite");
    let gql_url = format!("{}/graphql", server.url());
    let options = SuiteRunOptions {
        required_tags: Vec::new(),
        only_ids: HashSet::new(),
        skip_ids: HashSet::new(),
        allow_writes_flag: false,
        fail_fast: false,
        output_dir_base: root.join("out-disabled"),
        env_rest_url: server.url(),
        env_gql_url: gql_url.clone(),
        env_grpc_url: String::new(),
        env_ws_url: String::new(),
        progress: None,
    };

    let run_disabled = run_suite(root, loaded.clone(), options).expect("run suite");
    let health = run_disabled
        .results
        .cases
        .iter()
        .find(|c| c.id == "rest.health")
        .expect("health case");
    if let Some(stderr_rel) = health.stderr_file.as_deref() {
        let stderr_path = resolve_output_path(root, stderr_rel);
        let contents = std::fs::read_to_string(&stderr_path).expect("stderr read");
        assert!(contents.contains("cleanup skipped (writes disabled)"));
    }
    assert_eq!(run_disabled.results.summary.total, 3);
    assert_eq!(run_disabled.results.summary.passed, 2);
    assert_eq!(run_disabled.results.summary.skipped, 1);
    assert_eq!(run_disabled.results.summary.failed, 0);
    assert_eq!(health.status, "passed");
    let stdout_rel = health.stdout_file.as_deref().expect("stdout file");
    let stdout_path = resolve_output_path(root, stdout_rel);
    let stdout_body = std::fs::read_to_string(&stdout_path).expect("stdout read");
    assert_eq!(stdout_body, r#"{"data":{"id":"123"}}"#);

    let command = health.command.as_deref().unwrap_or("");
    assert!(command.contains("'api-rest'"));
    assert!(command.contains("--no-history"));
    assert!(command.contains(&server.url()));
    assert!(command.contains("requests/health.request.json"));
    assert!(stdout_rel.starts_with("out-disabled/"));
    assert!(stdout_rel.ends_with("/rest.health.response.json"));
    let stderr_rel = health.stderr_file.as_deref().expect("stderr file");
    assert!(stderr_rel.starts_with("out-disabled/"));
    assert!(stderr_rel.ends_with("/rest.health.stderr.log"));

    let gql = run_disabled
        .results
        .cases
        .iter()
        .find(|c| c.id == "graphql.health")
        .expect("graphql case");
    assert_eq!(gql.status, "passed");
    let gql_command = gql.command.as_deref().unwrap_or("");
    assert!(gql_command.contains("'api-gql'"));
    assert!(gql_command.contains("--no-history"));
    assert!(gql_command.contains(&gql_url));
    assert!(gql_command.contains("ops/health.graphql"));

    let gql_stdout_rel = gql.stdout_file.as_deref().expect("gql stdout file");
    assert!(gql_stdout_rel.starts_with("out-disabled/"));
    assert!(gql_stdout_rel.ends_with("/graphql.health.response.json"));
    let gql_stdout_path = resolve_output_path(root, gql_stdout_rel);
    let gql_stdout_body = std::fs::read_to_string(&gql_stdout_path).expect("gql stdout read");
    assert_eq!(gql_stdout_body, r#"{"data":{"ok":true}}"#);

    let gql_stderr_rel = gql.stderr_file.as_deref().expect("gql stderr file");
    assert!(gql_stderr_rel.starts_with("out-disabled/"));
    assert!(gql_stderr_rel.ends_with("/graphql.health.stderr.log"));
    let gql_stderr_path = resolve_output_path(root, gql_stderr_rel);
    let gql_stderr_body = std::fs::read_to_string(&gql_stderr_path).expect("gql stderr read");
    assert_eq!(gql_stderr_body, "");

    let write_case = run_disabled
        .results
        .cases
        .iter()
        .find(|c| c.id == "rest.write")
        .expect("write case");
    assert_eq!(write_case.status, "skipped");
    assert_eq!(
        write_case.message.as_deref(),
        Some(MSG_WRITE_CASES_DISABLED)
    );

    let options = SuiteRunOptions {
        required_tags: Vec::new(),
        only_ids: HashSet::new(),
        skip_ids: HashSet::new(),
        allow_writes_flag: true,
        fail_fast: false,
        output_dir_base: root.join("out-enabled"),
        env_rest_url: server.url(),
        env_gql_url: gql_url,
        env_grpc_url: String::new(),
        env_ws_url: String::new(),
        progress: None,
    };

    let run_enabled = run_suite(root, loaded, options).expect("run suite writes enabled");
    assert_eq!(run_enabled.results.summary.failed, 0);
    assert_eq!(run_enabled.results.summary.passed, 3);

    let health_enabled = run_enabled
        .results
        .cases
        .iter()
        .find(|c| c.id == "rest.health")
        .expect("health case (writes enabled)");
    let health_stdout_rel = health_enabled.stdout_file.as_deref().expect("stdout file");
    let health_stdout_path = resolve_output_path(root, health_stdout_rel);
    let case_dir = health_stdout_path.parent().expect("case dir");
    let cleanup_request_path = case_dir.join("rest.health.cleanup.0.request.json");
    assert!(cleanup_request_path.is_file());
    let cleanup_request_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cleanup_request_path).unwrap()).unwrap();
    assert_eq!(
        cleanup_request_json,
        serde_json::json!({
            "method": "DELETE",
            "path": "/cleanup/123",
            "expect": { "status": 204 }
        })
    );
    let cleanup_response_path = case_dir.join("rest.health.cleanup.0.response.json");
    assert_eq!(std::fs::read_to_string(&cleanup_response_path).unwrap(), "");
    let cleanup_stderr_path = case_dir.join("rest.health.cleanup.0.stderr.log");
    assert_eq!(std::fs::read_to_string(&cleanup_stderr_path).unwrap(), "");

    let requests = server.take_requests();
    assert!(
        requests
            .iter()
            .any(|r| r.method == "DELETE" && r.path == "/cleanup/123")
    );
}

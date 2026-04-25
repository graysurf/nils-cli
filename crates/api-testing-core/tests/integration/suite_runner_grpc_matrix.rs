use std::collections::HashSet;

use api_testing_core::suite::runner::{SuiteRunOptions, run_suite};
use api_testing_core::suite::schema::load_and_validate_suite;
use nils_test_support::fs::{write_executable, write_text};
use nils_test_support::{EnvGuard, GlobalStateLock};
use tempfile::TempDir;

#[test]
fn suite_runner_executes_grpc_case_with_mock_transport() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).expect("git marker");

    let mock = root.join("grpcurl-mock.sh");
    write_executable(&mock, "#!/bin/sh\necho '{\"ok\":true}'\nexit 0\n");

    write_text(
        &root.join("requests/health.grpc.json"),
        r#"{
  "method": "health.HealthService/Check",
  "body": {"ping":"pong"},
  "expect": {"status": 0, "jq": ".ok == true"}
}"#,
    );

    write_text(
        &root.join("grpc.suite.json"),
        r#"{
  "version": 1,
  "defaults": {
    "grpc": { "url": "127.0.0.1:50051" }
  },
  "cases": [
    { "id": "grpc.health", "type": "grpc", "request": "requests/health.grpc.json" }
  ]
}"#,
    );

    let loaded = load_and_validate_suite(root.join("grpc.suite.json")).expect("load suite");
    let lock = GlobalStateLock::new();
    let mock_path = mock.to_string_lossy().to_string();
    let _grpcurl_bin = EnvGuard::set(&lock, "GRPCURL_BIN", &mock_path);

    let out = run_suite(
        root,
        loaded,
        SuiteRunOptions {
            required_tags: Vec::new(),
            only_ids: HashSet::new(),
            skip_ids: HashSet::new(),
            allow_writes_flag: false,
            fail_fast: false,
            output_dir_base: root.join("out"),
            env_rest_url: String::new(),
            env_gql_url: String::new(),
            env_grpc_url: String::new(),
            env_ws_url: String::new(),
            progress: None,
        },
    )
    .expect("run suite");

    assert_eq!(out.results.summary.total, 1);
    assert_eq!(out.results.summary.passed, 1);
    assert_eq!(out.results.summary.failed, 0);
    let case = &out.results.cases[0];
    assert_eq!(case.id, "grpc.health");
    assert_eq!(case.status, "passed");
    assert!(case.command.as_deref().unwrap_or("").contains("api-grpc"));
}

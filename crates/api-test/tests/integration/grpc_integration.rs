use std::path::Path;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
use nils_test_support::fs::{write_executable, write_text};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn api_test_bin() -> std::path::PathBuf {
    resolve("api-test")
}

fn run_api_test(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> CmdOutput {
    let mut opts = CmdOptions::default().with_cwd(cwd);
    for key in ["GRPCURL_BIN", "API_TEST_GRPC_URL", "API_TEST_OUTPUT_DIR"] {
        opts = opts.with_env_remove(key);
    }
    for (k, v) in env {
        opts = opts.with_env(k, v);
    }
    run_with(&api_test_bin(), args, &opts)
}

#[test]
fn api_test_run_supports_grpc_case() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).expect("git marker");

    let mock = root.join("grpcurl-mock.sh");
    write_executable(&mock, "#!/bin/sh\necho '{\"ok\":true}'\nexit 0\n");

    write_text(
        &root.join("setup/grpc/requests/health.grpc.json"),
        r#"{
  "method": "health.HealthService/Check",
  "body": {"ping":"pong"},
  "expect": {"status": 0, "jq": ".ok == true"}
}"#,
    );

    write_text(
        &root.join("tests/api/suites/grpc-smoke.suite.json"),
        r#"{
  "version": 1,
  "name": "grpc-smoke",
  "defaults": {
    "env": "local",
    "grpc": { "url": "127.0.0.1:50051" }
  },
  "cases": [
    { "id": "grpc.health", "type": "grpc", "request": "setup/grpc/requests/health.grpc.json" }
  ]
}"#,
    );

    let out = run_api_test(
        root,
        &[
            "run",
            "--suite",
            "grpc-smoke",
            "--out",
            "out/grpc/results.json",
        ],
        &[
            (
                "GRPCURL_BIN",
                mock.to_str().expect("mock path should be valid UTF-8"),
            ),
            ("API_TEST_OUTPUT_DIR", "out/api-test-runner-grpc"),
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("results json");
    assert_eq!(json["summary"]["total"], 1);
    assert_eq!(json["summary"]["passed"], 1);
    assert_eq!(json["summary"]["failed"], 0);
}

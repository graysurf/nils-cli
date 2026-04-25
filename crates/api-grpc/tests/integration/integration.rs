use std::path::{Path, PathBuf};

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
use nils_test_support::fs::{write_executable, write_json};
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn api_grpc_bin() -> PathBuf {
    resolve("api-grpc")
}

fn run_api_grpc(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default()
        .with_cwd(cwd)
        .with_env_remove_many(&[
            "GRPCURL_BIN",
            "GRPC_URL",
            "GRPC_ENV_DEFAULT",
            "GRPC_TOKEN_NAME",
            "GRPC_HISTORY_ENABLED",
            "GRPC_HISTORY_FILE",
            "GRPC_HISTORY_LOG_URL_ENABLED",
            "GRPC_JWT_VALIDATE_ENABLED",
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

    run_with(&api_grpc_bin(), args, &options)
}

fn write_health_request(path: &Path, with_expect: bool) {
    let expect = if with_expect {
        serde_json::json!({"status": 0, "jq": ".ok == true"})
    } else {
        serde_json::json!({"status": 0})
    };
    write_json(
        path,
        &serde_json::json!({
            "method": "health.HealthService/Check",
            "body": {"service":"payments"},
            "expect": expect
        }),
    );
}

#[test]
fn call_success_prints_response_and_writes_history() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/grpc")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    write_health_request(&root.join("requests/health.grpc.json"), true);

    let script = root.join("grpcurl-ok.sh");
    write_executable(&script, "#!/bin/sh\necho '{\"ok\":true}'\nexit 0\n");

    let script_str = script.to_string_lossy().to_string();
    let out = run_api_grpc(
        root,
        &[
            "call",
            "--config-dir",
            "setup/grpc",
            "--url",
            "127.0.0.1:50051",
            "requests/health.grpc.json",
        ],
        &[("GRPCURL_BIN", &script_str)],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("\"ok\":true"));

    let history = std::fs::read_to_string(root.join("setup/grpc/.grpc_history")).expect("history");
    assert!(history.contains("api-grpc call"));
    assert!(history.contains("--config-dir 'setup/grpc'"));
    assert!(history.contains("--url '127.0.0.1:50051'"));
    assert!(history.contains("requests/health.grpc.json"));
}

#[test]
fn history_tail_command_only_omits_metadata_lines() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup = root.join("setup/grpc");
    std::fs::create_dir_all(&setup).expect("mkdir setup");

    std::fs::write(
        setup.join(".grpc_history"),
        "# stamp exit=0 setup_dir=.\napi-grpc call \\\n  --config-dir 'setup/grpc' \\\n  requests/one.grpc.json \\\n| jq .\n\n# stamp exit=0 setup_dir=.\napi-grpc call \\\n  --config-dir 'setup/grpc' \\\n  requests/two.grpc.json \\\n| jq .\n\n",
    )
    .expect("write history");

    let out = run_api_grpc(
        root,
        &[
            "history",
            "--config-dir",
            "setup/grpc",
            "--tail",
            "1",
            "--command-only",
        ],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("requests/two.grpc.json"));
    assert!(!out.stdout_text().contains("stamp exit"));
}

#[test]
fn call_expect_failure_non_json_prints_response_preview_to_stderr() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/grpc")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    write_health_request(&root.join("requests/fail.grpc.json"), true);

    let script = root.join("grpcurl-text.sh");
    write_executable(&script, "#!/bin/sh\necho 'boom-body'\nexit 0\n");
    let script_str = script.to_string_lossy().to_string();

    let out = run_api_grpc(
        root,
        &[
            "call",
            "--config-dir",
            "setup/grpc",
            "--url",
            "127.0.0.1:50051",
            "--no-history",
            "requests/fail.grpc.json",
        ],
        &[("GRPCURL_BIN", &script_str)],
    );
    assert_eq!(out.code, 1);
    let stderr = out.stderr_text();
    assert!(stderr.contains("gRPC expect.jq requires a JSON response body"));
    assert!(stderr.contains("Response body (non-JSON; first 8192 bytes):"));
    assert!(stderr.contains("boom-body"));
}

#[test]
fn report_run_writes_markdown_report() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/grpc")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    std::fs::create_dir_all(root.join("out")).expect("mkdir out");

    write_health_request(&root.join("requests/health.grpc.json"), true);
    let script = root.join("grpcurl-ok.sh");
    write_executable(&script, "#!/bin/sh\necho '{\"ok\":true}'\nexit 0\n");
    let script_str = script.to_string_lossy().to_string();

    let out = run_api_grpc(
        root,
        &[
            "report",
            "--case",
            "grpc-health",
            "--request",
            "requests/health.grpc.json",
            "--run",
            "--url",
            "127.0.0.1:50051",
            "--config-dir",
            "setup/grpc",
            "--out",
            "out/grpc-health.md",
        ],
        &[("GRPCURL_BIN", &script_str)],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("out/grpc-health.md"));

    let markdown = std::fs::read_to_string(root.join("out/grpc-health.md")).expect("report file");
    assert!(markdown.contains("Test Case: grpc-health"));
    assert!(markdown.contains("Result: PASS"));
    assert!(markdown.contains("api-grpc call"));
    assert!(markdown.contains("### Assertions"));
}

#[test]
fn report_from_cmd_with_response_file_generates_report() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/grpc")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    std::fs::create_dir_all(root.join("responses")).expect("mkdir responses");
    std::fs::create_dir_all(root.join("out")).expect("mkdir out");

    write_health_request(&root.join("requests/health.grpc.json"), true);
    std::fs::write(root.join("responses/health.json"), "{\"ok\":true}\n").expect("response file");

    let snippet = "api-grpc call --config-dir setup/grpc --env local requests/health.grpc.json";
    let out = run_api_grpc(
        root,
        &[
            "report-from-cmd",
            "--response",
            "responses/health.json",
            "--out",
            "out/from-cmd.md",
            snippet,
        ],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let markdown = std::fs::read_to_string(root.join("out/from-cmd.md")).expect("report file");
    assert!(markdown.contains("Test Case: health"));
    assert!(markdown.contains("Result: (response provided; request not executed)"));
    assert!(markdown.contains("### gRPC Request"));
}

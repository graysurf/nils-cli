use std::collections::HashSet;
use std::net::TcpListener;
use std::path::PathBuf;
use std::thread;

use api_testing_core::suite::runner::{SuiteRunOptions, run_suite};
use api_testing_core::suite::schema::load_and_validate_suite;
use nils_test_support::fixtures::write_text;
use tempfile::TempDir;
use tungstenite::Message;

fn resolve_output_path(root: &std::path::Path, rel: &str) -> PathBuf {
    let path = PathBuf::from(rel);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn spawn_echo_server() -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind websocket listener");
    let addr = listener.local_addr().expect("listener addr");

    let handle = thread::spawn(move || {
        for _ in 0..2 {
            let Ok((stream, _)) = listener.accept() else {
                break;
            };
            let mut ws = tungstenite::accept(stream).expect("accept websocket handshake");
            loop {
                match ws.read() {
                    Ok(Message::Text(text)) => {
                        let response = if text.trim() == "ping" {
                            "{\"ok\":true}".to_string()
                        } else {
                            "boom".to_string()
                        };
                        ws.send(Message::Text(response.into()))
                            .expect("send response");
                    }
                    Ok(Message::Close(_)) => {
                        let _ = ws.close(None);
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }
    });

    (format!("ws://{addr}/ws"), handle)
}

#[test]
fn suite_runner_executes_websocket_cases_with_pass_and_fail() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).expect("git marker");

    let (url, handle) = spawn_echo_server();

    write_text(
        &root.join("requests/health.ws.json"),
        r#"{
  "steps": [
    {"type":"send","text":"ping"},
    {"type":"receive","expect":{"jq": ".ok == true"}},
    {"type":"close"}
  ]
}"#,
    );

    write_text(
        &root.join("requests/fail.ws.json"),
        r#"{
  "steps": [
    {"type":"send","text":"fail"},
    {"type":"receive","expect":{"textContains":"ok"}},
    {"type":"close"}
  ]
}"#,
    );

    write_text(
        &root.join("ws.suite.json"),
        &serde_json::to_string_pretty(&serde_json::json!({
            "version": 1,
            "defaults": {
                "websocket": {"url": url}
            },
            "cases": [
                {"id": "ws.health", "type": "websocket", "request": "requests/health.ws.json"},
                {"id": "ws.fail", "type": "websocket", "request": "requests/fail.ws.json"}
            ]
        }))
        .expect("serialize suite"),
    );

    let loaded = load_and_validate_suite(root.join("ws.suite.json")).expect("load suite");

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

    assert_eq!(out.results.summary.total, 2);
    assert_eq!(out.results.summary.passed, 1);
    assert_eq!(out.results.summary.failed, 1);

    let pass_case = out
        .results
        .cases
        .iter()
        .find(|c| c.id == "ws.health")
        .expect("pass case");
    assert_eq!(pass_case.status, "passed");
    assert!(
        pass_case
            .command
            .as_deref()
            .unwrap_or_default()
            .contains("api-websocket")
    );

    let pass_stdout_rel = pass_case.stdout_file.as_deref().expect("stdout file");
    let pass_stdout_path = resolve_output_path(root, pass_stdout_rel);
    let pass_stdout_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&pass_stdout_path).expect("stdout read"))
            .expect("stdout json");
    assert_eq!(pass_stdout_json["lastReceived"], "{\"ok\":true}");

    let fail_case = out
        .results
        .cases
        .iter()
        .find(|c| c.id == "ws.fail")
        .expect("fail case");
    assert_eq!(fail_case.status, "failed");
    let fail_stderr_rel = fail_case.stderr_file.as_deref().expect("stderr file");
    let fail_stderr_path = resolve_output_path(root, fail_stderr_rel);
    let fail_stderr = std::fs::read_to_string(&fail_stderr_path).expect("stderr read");
    assert!(fail_stderr.contains("textContains failed"));

    handle.join().expect("join websocket server");
}

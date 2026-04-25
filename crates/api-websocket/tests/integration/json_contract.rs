use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;

use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
use nils_test_support::fs::write_json;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tungstenite::Message;

fn api_websocket_bin() -> PathBuf {
    resolve("api-websocket")
}

fn run_api_websocket(cwd: &Path, args: &[&str]) -> CmdOutput {
    let options = CmdOptions::default().with_cwd(cwd).with_env_remove_many(&[
        "WS_URL",
        "WS_ENV_DEFAULT",
        "WS_TOKEN_NAME",
        "WS_HISTORY_ENABLED",
        "WS_HISTORY_FILE",
        "WS_HISTORY_LOG_URL_ENABLED",
        "WS_JWT_VALIDATE_ENABLED",
        "ACCESS_TOKEN",
        "SERVICE_TOKEN",
    ]);

    run_with(&api_websocket_bin(), args, &options)
}

fn spawn_echo_server() -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind websocket listener");
    let addr = listener.local_addr().expect("listener addr");

    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept websocket stream");
        let mut ws = tungstenite::accept(stream).expect("accept websocket handshake");
        loop {
            match ws.read() {
                Ok(Message::Text(text)) => {
                    let response = if text.trim() == "ping" {
                        "{\"ok\":true}".to_string()
                    } else {
                        text.to_string()
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
    });

    (format!("ws://{addr}/ws"), handle)
}

#[test]
fn call_json_success_contains_required_envelope_fields() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    let (url, handle) = spawn_echo_server();

    write_json(
        &root.join("requests/health.ws.json"),
        &serde_json::json!({
            "steps": [
                {"type": "send", "text": "ping"},
                {"type": "receive", "expect": {"jq": ".ok == true"}},
                {"type": "close"}
            ]
        }),
    );

    let out = run_api_websocket(
        root,
        &[
            "call",
            "--format",
            "json",
            "--url",
            &url,
            "requests/health.ws.json",
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let value: serde_json::Value = serde_json::from_str(&out.stdout_text()).expect("json output");

    assert_eq!(value["schema_version"], "cli.api-websocket.call.v1");
    assert_eq!(value["command"], "api-websocket call");
    assert_eq!(value["ok"], true);
    assert!(value.get("result").is_some());
    assert!(value["result"]["transcript"].is_array());
    assert_eq!(value["result"]["last_received"], "{\"ok\":true}");

    handle.join().expect("join websocket server");
}

#[test]
fn call_json_failure_contains_stable_error_envelope() {
    let tmp = TempDir::new().expect("tmp");
    let out = run_api_websocket(tmp.path(), &["call", "--format", "json", "missing.ws.json"]);

    assert_eq!(out.code, 1);
    let value: serde_json::Value = serde_json::from_str(&out.stdout_text()).expect("json output");

    assert_eq!(value["schema_version"], "cli.api-websocket.call.v1");
    assert_eq!(value["command"], "api-websocket call");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "request_not_found");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Request file not found")
    );
}

#[test]
fn history_json_success_uses_results_contract() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup = root.join("setup/websocket");
    std::fs::create_dir_all(&setup).expect("mkdir setup");
    std::fs::write(
        setup.join(".ws_history"),
        "# stamp exit=0 setup_dir=.\napi-websocket call \\\n  requests/a.ws.json \\\n| jq .\n\n",
    )
    .expect("write history");

    let out = run_api_websocket(
        root,
        &[
            "history",
            "--format",
            "json",
            "--config-dir",
            "setup/websocket",
            "--tail",
            "1",
        ],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let value: serde_json::Value = serde_json::from_str(&out.stdout_text()).expect("json output");

    assert_eq!(value["schema_version"], "cli.api-websocket.history.v1");
    assert_eq!(value["command"], "api-websocket history");
    assert_eq!(value["ok"], true);
    assert!(value.get("result").is_some());
    assert_eq!(value["result"]["count"], 1);
    assert_eq!(
        value["result"]["records"].as_array().map(|v| v.len()),
        Some(1)
    );
}

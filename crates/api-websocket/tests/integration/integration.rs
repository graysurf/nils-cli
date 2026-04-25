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

fn run_api_websocket(cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> CmdOutput {
    let mut options = CmdOptions::default().with_cwd(cwd);
    for key in [
        "WS_URL",
        "WS_ENV_DEFAULT",
        "WS_TOKEN_NAME",
        "WS_HISTORY_ENABLED",
        "WS_HISTORY_FILE",
        "WS_HISTORY_LOG_URL_ENABLED",
        "WS_JWT_VALIDATE_ENABLED",
        "WS_JWT_VALIDATE_STRICT",
        "WS_JWT_VALIDATE_LEEWAY_SECONDS",
        "WS_REPORT_INCLUDE_COMMAND_ENABLED",
        "WS_REPORT_COMMAND_LOG_URL_ENABLED",
        "ACCESS_TOKEN",
        "SERVICE_TOKEN",
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
                    } else if text.trim() == "plain" {
                        "boom-body".to_string()
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

fn write_request(path: &Path, send_text: &str, expect: serde_json::Value) {
    write_json(
        path,
        &serde_json::json!({
            "steps": [
                {"type": "send", "text": send_text},
                {"type": "receive", "expect": expect},
                {"type": "close"}
            ]
        }),
    );
}

#[test]
fn call_success_prints_response_and_writes_history() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/websocket")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    let (url, handle) = spawn_echo_server();
    std::fs::write(
        root.join("setup/websocket/endpoints.env"),
        format!("WS_URL_LOCAL={url}\n"),
    )
    .expect("write endpoints");

    write_request(
        &root.join("requests/health.ws.json"),
        "ping",
        serde_json::json!({"jq": ".ok == true"}),
    );

    let out = run_api_websocket(
        root,
        &[
            "call",
            "--config-dir",
            "setup/websocket",
            "--env",
            "local",
            "requests/health.ws.json",
        ],
        &[],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert_eq!(out.stdout_text(), "{\"ok\":true}");

    let history =
        std::fs::read_to_string(root.join("setup/websocket/.ws_history")).expect("history");
    assert!(history.contains("api-websocket call"));
    assert!(history.contains("--config-dir 'setup/websocket'"));
    assert!(history.contains("--env 'local'"));
    assert!(history.contains("requests/health.ws.json"));

    handle.join().expect("join websocket server");
}

#[test]
fn history_tail_command_only_omits_metadata_lines() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup = root.join("setup/websocket");
    std::fs::create_dir_all(&setup).expect("mkdir setup");

    std::fs::write(
        setup.join(".ws_history"),
        "# stamp exit=0 setup_dir=.\napi-websocket call \\\n  --config-dir 'setup/websocket' \\\n  requests/one.ws.json \\\n| jq .\n\n# stamp exit=0 setup_dir=.\napi-websocket call \\\n  --config-dir 'setup/websocket' \\\n  requests/two.ws.json \\\n| jq .\n\n",
    )
    .expect("write history");

    let out = run_api_websocket(
        root,
        &[
            "history",
            "--config-dir",
            "setup/websocket",
            "--tail",
            "1",
            "--command-only",
        ],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("requests/two.ws.json"));
    assert!(!out.stdout_text().contains("stamp exit"));
}

#[test]
fn call_expect_failure_non_json_prints_response_preview_to_stderr() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/websocket")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    let url = "ws://127.0.0.1:65535/ws".to_string();

    write_json(
        &root.join("requests/fail.ws.json"),
        &serde_json::json!({
            "steps": [
                {"type": "send", "text": "plain"},
                {"type": "receive"},
                {"type": "close"}
            ],
            "expect": {"jq": ".ok == true"}
        }),
    );

    let out = run_api_websocket(
        root,
        &[
            "call",
            "--config-dir",
            "setup/websocket",
            "--url",
            &url,
            "--no-history",
            "requests/fail.ws.json",
        ],
        &[],
    );
    assert_eq!(out.code, 1);
    let stderr = out.stderr_text();
    assert!(!stderr.is_empty());
}

#[test]
fn report_run_writes_markdown_report() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/websocket")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    std::fs::create_dir_all(root.join("out")).expect("mkdir out");

    let (url, handle) = spawn_echo_server();

    write_json(
        &root.join("requests/health.ws.json"),
        &serde_json::json!({
            "steps": [
                {"type": "send", "text": "ping"},
                {"type": "receive"},
                {"type": "close"}
            ],
            "expect": {"jq": ".ok == true"}
        }),
    );

    let out = run_api_websocket(
        root,
        &[
            "report",
            "--case",
            "ws-health",
            "--request",
            "requests/health.ws.json",
            "--run",
            "--url",
            &url,
            "--config-dir",
            "setup/websocket",
            "--out",
            "out/ws-health.md",
        ],
        &[],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert!(out.stdout_text().contains("out/ws-health.md"));

    let markdown = std::fs::read_to_string(root.join("out/ws-health.md")).expect("report file");
    assert!(markdown.contains("Test Case: ws-health"));
    assert!(markdown.contains("Result: PASS"));
    assert!(markdown.contains("api-websocket call"));
    assert!(markdown.contains("### Assertions"));
    assert!(markdown.contains("### WebSocket Request"));
    assert!(markdown.contains("### Transcript"));

    handle.join().expect("join websocket server");
}

#[test]
fn report_from_cmd_with_response_file_generates_report() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/websocket")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    std::fs::create_dir_all(root.join("responses")).expect("mkdir responses");
    std::fs::create_dir_all(root.join("out")).expect("mkdir out");

    write_request(
        &root.join("requests/health.ws.json"),
        "ping",
        serde_json::json!({"textContains": "ok"}),
    );

    std::fs::write(
        root.join("responses/transcript.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "target": "ws://example/ws",
            "transcript": [
                {"direction": "send", "payload": "ping"},
                {"direction": "receive", "payload": "{\"ok\":true}"}
            ],
            "lastReceived": "{\"ok\":true}"
        }))
        .expect("serialize transcript"),
    )
    .expect("write response file");

    let snippet =
        "api-websocket call --config-dir setup/websocket --env local requests/health.ws.json";
    let out = run_api_websocket(
        root,
        &[
            "report-from-cmd",
            "--response",
            "responses/transcript.json",
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
    assert!(markdown.contains("### WebSocket Request"));
}

#[test]
fn call_accepts_literal_ws_url_via_env_passthrough() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/websocket")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    let (url, handle) = spawn_echo_server();
    write_request(
        &root.join("requests/passthrough.ws.json"),
        "ping",
        serde_json::json!({"jq": ".ok == true"}),
    );

    let out = run_api_websocket(
        root,
        &[
            "call",
            "--config-dir",
            "setup/websocket",
            "--env",
            &url,
            "requests/passthrough.ws.json",
        ],
        &[],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert_eq!(out.stdout_text(), "{\"ok\":true}");
    handle.join().expect("join websocket server");
}

#[test]
fn call_json_expectation_failure_returns_stable_error_code() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/websocket")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    let (url, handle) = spawn_echo_server();
    write_json(
        &root.join("requests/fail-json.ws.json"),
        &serde_json::json!({
            "steps": [
                {"type": "send", "text": "ping"},
                {"type": "receive"},
                {"type": "close"}
            ],
            "expect": {"jq": ".ok == false"}
        }),
    );

    let out = run_api_websocket(
        root,
        &[
            "call",
            "--format",
            "json",
            "--config-dir",
            "setup/websocket",
            "--url",
            &url,
            "requests/fail-json.ws.json",
        ],
        &[],
    );

    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    let value: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("json failure envelope");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "expectation_failed");
    assert_eq!(value["command"], "api-websocket call");

    handle.join().expect("join websocket server");
}

#[test]
fn history_json_missing_file_returns_not_found_envelope() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup = root.join("setup/websocket");
    std::fs::create_dir_all(&setup).expect("mkdir setup");
    std::fs::write(setup.join("endpoints.env"), "").expect("write endpoints");

    let out = run_api_websocket(
        root,
        &[
            "history",
            "--format",
            "json",
            "--config-dir",
            "setup/websocket",
        ],
        &[],
    );

    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    let value: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("json history envelope");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "history_not_found");
}

#[test]
fn history_json_empty_file_returns_exit_three_and_error_envelope() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    let setup = root.join("setup/websocket");
    std::fs::create_dir_all(&setup).expect("mkdir setup");
    std::fs::write(setup.join(".ws_history"), "").expect("write empty history");

    let out = run_api_websocket(
        root,
        &[
            "history",
            "--format",
            "json",
            "--config-dir",
            "setup/websocket",
        ],
        &[],
    );

    assert_eq!(out.code, 3, "stderr={}", out.stderr_text());
    let value: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("json history envelope");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "history_empty");
}

#[test]
fn report_from_cmd_dry_run_prints_equivalent_report_command() {
    let tmp = TempDir::new().expect("tmp");
    let snippet =
        "api-websocket call --config-dir setup/websocket --env local requests/health.ws.json";
    let out = run_api_websocket(tmp.path(), &["report-from-cmd", "--dry-run", snippet], &[]);

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let stdout = out.stdout_text();
    assert!(stdout.contains("api-websocket report"));
    assert!(stdout.contains("--case"));
    assert!(stdout.contains("health"));
    assert!(stdout.contains("--request"));
    assert!(stdout.contains("requests/health.ws.json"));
    assert!(stdout.contains("--run"));
}

#[test]
fn report_from_cmd_rejects_stdin_when_response_uses_dash() {
    let tmp = TempDir::new().expect("tmp");
    let out = run_api_websocket(
        tmp.path(),
        &["report-from-cmd", "--response", "-", "--stdin"],
        &[],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr_text()
            .contains("stdin is reserved for the response body")
    );
}

#[test]
fn report_from_cmd_rejects_non_websocket_snippet() {
    let tmp = TempDir::new().expect("tmp");
    let out = run_api_websocket(
        tmp.path(),
        &[
            "report-from-cmd",
            "api-rest call requests/health.request.json",
        ],
        &[],
    );
    assert_eq!(out.code, 1);
    assert!(
        out.stderr_text()
            .contains("expected a WebSocket call snippet")
    );
}

#[test]
fn report_response_plain_text_builds_transcript_and_marks_failed_assertion() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/websocket")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    std::fs::create_dir_all(root.join("responses")).expect("mkdir responses");
    std::fs::create_dir_all(root.join("out")).expect("mkdir out");

    write_json(
        &root.join("requests/plain.ws.json"),
        &serde_json::json!({
            "steps": [
                {"type": "send", "text": "ping"},
                {"type": "receive"},
                {"type": "close"}
            ],
            "expect": {"textContains": "ok"}
        }),
    );
    std::fs::write(root.join("responses/plain.txt"), "not-json-body").expect("write response");

    let out = run_api_websocket(
        root,
        &[
            "report",
            "--case",
            "ws-plain",
            "--request",
            "requests/plain.ws.json",
            "--response",
            "responses/plain.txt",
            "--config-dir",
            "setup/websocket",
            "--env",
            "local",
            "--out",
            "out/ws-plain.md",
        ],
        &[],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let markdown = std::fs::read_to_string(root.join("out/ws-plain.md")).expect("report file");
    assert!(markdown.contains("Test Case: ws-plain"));
    assert!(markdown.contains("Result: (response provided; request not executed)"));
    assert!(markdown.contains("expect.textContains: ok"));
    assert!(markdown.contains("(FAIL)"));
    assert!(markdown.contains("### Transcript"));
}

#[test]
fn report_no_command_url_hides_url_in_command_snippet() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    std::fs::create_dir_all(root.join("responses")).expect("mkdir responses");
    std::fs::create_dir_all(root.join("out")).expect("mkdir out");

    write_json(
        &root.join("requests/hide-url.ws.json"),
        &serde_json::json!({
            "steps": [{"type": "receive"}]
        }),
    );
    std::fs::write(
        root.join("responses/transcript.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "target": "ws://example/ws",
            "transcript": [{"direction": "receive", "payload": "{\"ok\":true}"}],
            "lastReceived": "{\"ok\":true}"
        }))
        .expect("serialize transcript"),
    )
    .expect("write response");

    let out = run_api_websocket(
        root,
        &[
            "report",
            "--case",
            "hide-url",
            "--request",
            "requests/hide-url.ws.json",
            "--response",
            "responses/transcript.json",
            "--url",
            "ws://secret/ws",
            "--no-command-url",
            "--out",
            "out/hide-url.md",
        ],
        &[],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let markdown = std::fs::read_to_string(root.join("out/hide-url.md")).expect("report file");
    assert!(markdown.contains("## Command"));
    assert!(!markdown.contains("--url 'ws://secret/ws'"));
    assert!(markdown.contains("api-websocket call"));
}

#[test]
fn report_no_command_omits_command_section() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    std::fs::create_dir_all(root.join("responses")).expect("mkdir responses");
    std::fs::create_dir_all(root.join("out")).expect("mkdir out");

    write_json(
        &root.join("requests/no-command.ws.json"),
        &serde_json::json!({
            "steps": [{"type": "receive"}]
        }),
    );
    std::fs::write(
        root.join("responses/transcript.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "target": "ws://example/ws",
            "transcript": [{"direction": "receive", "payload": "{\"ok\":true}"}],
            "lastReceived": "{\"ok\":true}"
        }))
        .expect("serialize transcript"),
    )
    .expect("write response");

    let out = run_api_websocket(
        root,
        &[
            "report",
            "--case",
            "no-command",
            "--request",
            "requests/no-command.ws.json",
            "--response",
            "responses/transcript.json",
            "--no-command",
            "--out",
            "out/no-command.md",
        ],
        &[],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let markdown = std::fs::read_to_string(root.join("out/no-command.md")).expect("report file");
    assert!(!markdown.contains("## Command"));
}

#[test]
fn call_json_success_returns_schema_payload() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/websocket")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    let (url, handle) = spawn_echo_server();
    write_json(
        &root.join("requests/json-success.ws.json"),
        &serde_json::json!({
            "steps": [
                {"type": "send", "text": "ping"},
                {"type": "receive"},
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
            "--config-dir",
            "setup/websocket",
            "--url",
            &url,
            "requests/json-success.ws.json",
        ],
        &[],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let value: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("json success envelope");
    assert_eq!(value["schema_version"], "cli.api-websocket.call.v1");
    assert_eq!(value["command"], "api-websocket call");
    assert_eq!(value["ok"], true);
    assert_eq!(value["result"]["target"], url);
    assert_eq!(value["result"]["last_received"], "{\"ok\":true}");

    handle.join().expect("join websocket server");
}

#[test]
fn call_json_missing_request_returns_structured_error() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();

    let out = run_api_websocket(
        root,
        &["call", "--format", "json", "requests/not-found.ws.json"],
        &[],
    );

    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    let value: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("json error envelope");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "request_not_found");
}

#[test]
fn call_json_missing_token_profile_returns_auth_resolve_error() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/websocket")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    std::fs::write(
        root.join("setup/websocket/tokens.env"),
        "WS_TOKEN_DEFAULT=abc\nWS_TOKEN_QA=def\n",
    )
    .expect("write tokens");

    write_json(
        &root.join("requests/token-profile.ws.json"),
        &serde_json::json!({
            "steps": [
                {"type": "send", "text": "ping"},
                {"type": "receive"},
                {"type": "close"}
            ]
        }),
    );

    let url = "ws://127.0.0.1:65535/ws".to_string();
    let out = run_api_websocket(
        root,
        &[
            "call",
            "--format",
            "json",
            "--config-dir",
            "setup/websocket",
            "--url",
            &url,
            "--token",
            "missing",
            "requests/token-profile.ws.json",
        ],
        &[],
    );

    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    let value: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("json auth envelope");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "auth_resolve_error");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("available: default qa")
    );
}

#[test]
fn call_with_access_token_env_warns_non_strict_jwt_and_succeeds() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/websocket")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    write_json(
        &root.join("requests/non-strict-jwt.ws.json"),
        &serde_json::json!({
            "steps": [
                {"type": "send", "text": "ping"},
                {"type": "receive"},
                {"type": "close"}
            ]
        }),
    );

    let (url, handle) = spawn_echo_server();
    let out = run_api_websocket(
        root,
        &[
            "call",
            "--config-dir",
            "setup/websocket",
            "--url",
            &url,
            "requests/non-strict-jwt.ws.json",
        ],
        &[("ACCESS_TOKEN", "not.a.jwt")],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    assert_eq!(out.stdout_text(), "{\"ok\":true}");
    assert!(
        out.stderr_text()
            .contains("token for ACCESS_TOKEN is not a valid JWT")
    );

    handle.join().expect("join websocket server");
}

#[test]
fn call_json_strict_jwt_validation_returns_error() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("setup/websocket")).expect("mkdir setup");
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");

    write_json(
        &root.join("requests/strict-jwt.ws.json"),
        &serde_json::json!({
            "steps": [
                {"type": "send", "text": "ping"},
                {"type": "receive"},
                {"type": "close"}
            ]
        }),
    );

    let url = "ws://127.0.0.1:65535/ws".to_string();
    let out = run_api_websocket(
        root,
        &[
            "call",
            "--format",
            "json",
            "--config-dir",
            "setup/websocket",
            "--url",
            &url,
            "requests/strict-jwt.ws.json",
        ],
        &[
            ("ACCESS_TOKEN", "not.a.jwt"),
            ("WS_JWT_VALIDATE_STRICT", "true"),
        ],
    );

    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    let value: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("json jwt error envelope");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "jwt_validation_error");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("invalid JWT")
    );
}

#[test]
fn report_rejects_empty_case_name() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    std::fs::create_dir_all(root.join("responses")).expect("mkdir responses");
    write_json(
        &root.join("requests/empty-case.ws.json"),
        &serde_json::json!({
            "steps": [{"type": "receive"}]
        }),
    );
    std::fs::write(
        root.join("responses/transcript.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "transcript": [{"direction": "receive", "payload": "{\"ok\":true}"}]
        }))
        .expect("serialize transcript"),
    )
    .expect("write response");

    let out = run_api_websocket(
        root,
        &[
            "report",
            "--case",
            "",
            "--request",
            "requests/empty-case.ws.json",
            "--response",
            "responses/transcript.json",
        ],
        &[],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("--case is required"));
}

#[test]
fn report_missing_request_file_returns_error() {
    let tmp = TempDir::new().expect("tmp");
    let out = run_api_websocket(
        tmp.path(),
        &[
            "report",
            "--case",
            "missing-request",
            "--request",
            "requests/missing.ws.json",
            "--response",
            "responses/transcript.json",
        ],
        &[],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("Request file not found"));
}

#[test]
fn report_invalid_request_file_returns_parse_error() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    std::fs::create_dir_all(root.join("responses")).expect("mkdir responses");
    std::fs::write(root.join("requests/invalid.ws.json"), "{not-json").expect("write request");
    std::fs::write(root.join("responses/transcript.json"), "{}").expect("write response");

    let out = run_api_websocket(
        root,
        &[
            "report",
            "--case",
            "invalid-request",
            "--request",
            "requests/invalid.ws.json",
            "--response",
            "responses/transcript.json",
        ],
        &[],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("not valid JSON"));
}

#[test]
fn report_missing_response_file_returns_error() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    write_json(
        &root.join("requests/missing-response.ws.json"),
        &serde_json::json!({
            "steps": [{"type": "receive"}]
        }),
    );

    let out = run_api_websocket(
        root,
        &[
            "report",
            "--case",
            "missing-response",
            "--request",
            "requests/missing-response.ws.json",
            "--response",
            "responses/missing.json",
        ],
        &[],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("Response file not found"));
}

#[test]
fn report_response_derives_last_received_from_transcript_when_missing() {
    let tmp = TempDir::new().expect("tmp");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("requests")).expect("mkdir requests");
    std::fs::create_dir_all(root.join("responses")).expect("mkdir responses");
    std::fs::create_dir_all(root.join("out")).expect("mkdir out");

    write_json(
        &root.join("requests/derive-last.ws.json"),
        &serde_json::json!({
            "steps": [{"type": "receive"}],
            "expect": {"textContains": "ok"}
        }),
    );
    std::fs::write(
        root.join("responses/transcript.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "target": "ws://example/ws",
            "transcript": [
                {"direction": "send", "payload": "ping"},
                {"direction": "receive", "payload": "{\"ok\":true}"}
            ]
        }))
        .expect("serialize transcript"),
    )
    .expect("write response");

    let out = run_api_websocket(
        root,
        &[
            "report",
            "--case",
            "derive-last",
            "--request",
            "requests/derive-last.ws.json",
            "--response",
            "responses/transcript.json",
            "--out",
            "out/derive-last.md",
        ],
        &[],
    );
    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());

    let markdown = std::fs::read_to_string(root.join("out/derive-last.md")).expect("report file");
    assert!(markdown.contains("expect.textContains: ok"));
    assert!(markdown.contains("(PASS)"));
}

#[test]
fn report_from_cmd_dry_run_includes_response_url_token_and_out() {
    let tmp = TempDir::new().expect("tmp");
    let snippet = "api-websocket call --config-dir setup/websocket --url ws://localhost:9001/ws --token qa requests/health.ws.json";
    let out = run_api_websocket(
        tmp.path(),
        &[
            "report-from-cmd",
            "--dry-run",
            "--response",
            "responses/saved.json",
            "--out",
            "docs/report.md",
            snippet,
        ],
        &[],
    );

    assert_eq!(out.code, 0, "stderr={}", out.stderr_text());
    let stdout = out.stdout_text();
    assert!(stdout.contains("api-websocket report"));
    assert!(stdout.contains("--response 'responses/saved.json'"));
    assert!(stdout.contains("--out 'docs/report.md'"));
    assert!(stdout.contains("--url 'ws://localhost:9001/ws'"));
    assert!(stdout.contains("--token 'qa'"));
}

#[test]
fn report_from_cmd_invalid_snippet_returns_parse_error() {
    let tmp = TempDir::new().expect("tmp");
    let out = run_api_websocket(
        tmp.path(),
        &[
            "report-from-cmd",
            "--dry-run",
            "not a websocket call snippet",
        ],
        &[],
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr_text().contains("error:"));
}

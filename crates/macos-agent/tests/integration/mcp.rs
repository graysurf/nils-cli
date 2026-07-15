use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::common;

#[test]
fn stdio_mcp_filters_tools_denies_calls_and_keeps_protocol_stdout_clean() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    let request_log = cwd.path().join("upstream-requests.jsonl");
    let script = r#"#!/bin/sh
[ -z "${OPENAI_API_KEY:-}" ] || exit 90
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$MCP_REQUEST_LOG"
  case "$line" in
    *'"id":1'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"fake","version":"1"}}}' ;;
    *'"id":2'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"see"},{"name":"click"},{"name":"shell"},{"name":"browser"}]}}' ;;
    *'"id":4'*) printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":null}' ;;
  esac
done
"#;
    fs::write(&fake, script).expect("fake");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");
    let out_dir = cwd.path().join("mcp-journal");
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"shell\",\"arguments\":{\"token\":\"seed-mcp-secret\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":\"permissions\",\"arguments\":{\"action\":\"grant\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":99,\"reason\":\"seed-cancel-secret\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"shutdown\",\"params\":{}}\n",
    );
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake.to_str().expect("fake"),
        )
        .with_env("OPENAI_API_KEY", "seed-provider-key")
        .with_env(
            "MCP_REQUEST_LOG",
            request_log.to_str().expect("request log"),
        )
        .with_stdin_str(input);
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "mcp",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--tool-profile",
            "interact",
        ],
        options,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert!(out.stderr.is_empty());
    let frames = out
        .stdout_text()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("protocol JSON"))
        .collect::<Vec<_>>();
    assert_eq!(frames.len(), 5);
    let by_id = |id: i64| {
        frames
            .iter()
            .find(|frame| frame["id"] == id)
            .expect("response id")
    };
    let names = by_id(2)["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("name"))
        .collect::<Vec<_>>();
    assert_eq!(names, ["see", "click"]);
    assert_eq!(by_id(3)["error"]["code"], -32001);
    assert_eq!(by_id(5)["error"]["code"], -32001);
    assert_eq!(by_id(4)["result"], serde_json::Value::Null);
    let journal = fs::read_to_string(out_dir.join("steps.jsonl")).expect("journal");
    assert!(journal.contains("policy_blocked"));
    assert_eq!(journal.lines().count(), 6);
    assert!(!journal.contains("seed-mcp-secret"));
    assert!(!journal.contains("seed-cancel-secret"));
    assert!(!journal.contains("seed-provider-key"));
    let forwarded = fs::read_to_string(&request_log).expect("request log");
    assert!(!forwarded.contains("seed-mcp-secret"));
    assert!(!forwarded.contains("\"id\":3"));
    assert!(!forwarded.contains("\"id\":5"));
}

#[test]
fn idless_mutating_tool_call_is_rejected_before_upstream_execution() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    let request_log = cwd.path().join("upstream-requests.jsonl");
    fs::write(
        &fake,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$MCP_REQUEST_LOG"
  case "$line" in
    *'"id":1'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":null}' ;;
  esac
done
"#,
    )
    .expect("fake");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");
    let out_dir = cwd.path().join("idless-journal");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("NILS_MACOS_AGENT_PEEKABOO_BIN", fake.to_str().expect("fake"))
        .with_env(
            "MCP_REQUEST_LOG",
            request_log.to_str().expect("request log"),
        )
        .with_stdin_str(concat!(
            "{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"click\",\"arguments\":{\"token\":\"idless-secret-canary\"}}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"shutdown\",\"params\":{}}\n",
        ));
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "mcp",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--tool-profile",
            "interact",
        ],
        options,
    );
    assert_eq!(out.code, 0, "{}", out.stderr_text());
    let first: serde_json::Value =
        serde_json::from_str(out.stdout_text().lines().next().expect("policy response"))
            .expect("response JSON");
    assert_eq!(first["id"], serde_json::Value::Null);
    assert_eq!(first["error"]["code"], -32600);
    let forwarded = fs::read_to_string(&request_log).expect("request log");
    assert!(!forwarded.contains("idless-secret-canary"));
    assert!(!forwarded.contains("tools/call"));
    let journal = fs::read_to_string(out_dir.join("steps.jsonl")).expect("journal");
    assert!(journal.contains("policy_blocked"));
    assert!(!journal.contains("idless-secret-canary"));
}

#[test]
fn ssh_mcp_has_the_same_protocol_and_transfers_its_sanitized_journal() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake_peekaboo = cwd.path().join("peekaboo");
    let upstream = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"id":1'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"see"},{"name":"shell"}]}}' ;;
    *'"id":2'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":null}' ;;
  esac
done
"#;
    fs::write(&fake_peekaboo, upstream).expect("peekaboo");
    fs::set_permissions(&fake_peekaboo, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fake_ssh = cwd.path().join("ssh");
    let ssh = r#"#!/bin/sh
while [ "$1" = "-o" ]; do shift 2; done
[ "$1" = "--" ] && shift
shift
shift
exec "$NILS_MACOS_AGENT_REMOTE_BIN" "$@"
"#;
    fs::write(&fake_ssh, ssh).expect("ssh");
    fs::set_permissions(&fake_ssh, fs::Permissions::from_mode(0o755)).expect("chmod ssh");
    let out_dir = cwd.path().join("remote-mcp-journal");
    let remote_root = cwd.path().join("remote-sessions");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake_peekaboo.to_str().expect("peekaboo"),
        )
        .with_env("NILS_MACOS_AGENT_SSH_BIN", fake_ssh.to_str().expect("ssh"))
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_BIN",
            harness.macos_agent_bin().to_str().expect("agent"),
        )
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_ROOT",
            remote_root.to_str().expect("root"),
        )
        .with_stdin_str(concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":{}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":99}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"shutdown\",\"params\":{}}\n",
        ));
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "mcp",
            "--host",
            "fixture-role",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--tool-profile",
            "observe",
        ],
        options,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    assert!(out.stderr.is_empty());
    let responses = out
        .stdout_text()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("protocol response"))
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    let names = responses[0]["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("name"))
        .collect::<Vec<_>>();
    assert_eq!(names, ["see"]);
    assert_eq!(responses[1]["id"], 2);
    assert_eq!(responses[1]["result"], serde_json::Value::Null);
    assert!(out_dir.join("manifest.json").is_file());
    assert_eq!(
        fs::read_to_string(out_dir.join("steps.jsonl"))
            .expect("journal")
            .lines()
            .count(),
        3
    );
    assert_eq!(
        fs::read_dir(&remote_root)
            .map(|entries| entries.count())
            .unwrap_or(0),
        0
    );
}

#[test]
fn ssh_mcp_refuses_a_remote_backend_lock_mismatch_before_launch() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let marker = cwd.path().join("backend-launched");
    let fake_peekaboo = cwd.path().join("peekaboo");
    fs::write(
        &fake_peekaboo,
        "#!/bin/sh\n: > \"$MCP_BACKEND_MARKER\"\nexit 0\n",
    )
    .expect("peekaboo");
    fs::set_permissions(&fake_peekaboo, fs::Permissions::from_mode(0o755)).expect("chmod");

    let mut remote_lock: serde_json::Value = serde_json::from_slice(
        &fs::read(format!("{}/peekaboo-lock.json", env!("CARGO_MANIFEST_DIR")))
            .expect("embedded lock"),
    )
    .expect("lock JSON");
    remote_lock["commit"] = serde_json::Value::String("a".repeat(40));
    let remote_lock_path = cwd.path().join("remote-peekaboo-lock.json");
    fs::write(
        &remote_lock_path,
        serde_json::to_vec_pretty(&remote_lock).expect("encode lock"),
    )
    .expect("remote lock");

    let fake_ssh = cwd.path().join("ssh");
    fs::write(
        &fake_ssh,
        r#"#!/bin/sh
while [ "$1" = "-o" ]; do shift 2; done
[ "$1" = "--" ] && shift
shift
shift
export NILS_MACOS_AGENT_LOCK_PATH="$NILS_MACOS_AGENT_REMOTE_LOCK_PATH"
exec "$NILS_MACOS_AGENT_REMOTE_BIN" "$@"
"#,
    )
    .expect("ssh");
    fs::set_permissions(&fake_ssh, fs::Permissions::from_mode(0o755)).expect("chmod ssh");

    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake_peekaboo.to_str().expect("peekaboo"),
        )
        .with_env("MCP_BACKEND_MARKER", marker.to_str().expect("marker"))
        .with_env("NILS_MACOS_AGENT_SSH_BIN", fake_ssh.to_str().expect("ssh"))
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_BIN",
            harness.macos_agent_bin().to_str().expect("agent"),
        )
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_LOCK_PATH",
            remote_lock_path.to_str().expect("remote lock"),
        )
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_ROOT",
            cwd.path().join("remote-sessions").to_str().expect("root"),
        )
        .with_stdin_str("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"shutdown\",\"params\":{}}\n");
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "mcp",
            "--host",
            "fixture-role",
            "--out-dir",
            cwd.path().join("journal").to_str().expect("out"),
        ],
        options,
    );
    assert_eq!(out.code, 75, "{}", out.stderr_text());
    assert!(!marker.exists(), "mismatched remote backend was launched");
}

#[test]
fn ssh_mcp_transport_failure_terminates_the_remote_process_group() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let marker = cwd.path().join("orphan-survived");
    let fake_ssh = cwd.path().join("ssh");
    fs::write(
        &fake_ssh,
        r#"#!/bin/sh
while [ "$1" = "-o" ]; do shift 2; done
[ "$1" = "--" ] && shift
shift
shift
case " $* " in
  *" __remote-mcp "*)
    (sleep 0.2; : > "$MCP_ORPHAN_MARKER") </dev/null >/dev/null 2>&1 &
    exit 9
    ;;
esac
exec "$NILS_MACOS_AGENT_REMOTE_BIN" "$@"
"#,
    )
    .expect("ssh");
    fs::set_permissions(&fake_ssh, fs::Permissions::from_mode(0o755)).expect("chmod ssh");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("NILS_MACOS_AGENT_SSH_BIN", fake_ssh.to_str().expect("ssh"))
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_BIN",
            harness.macos_agent_bin().to_str().expect("agent"),
        )
        .with_env("MCP_ORPHAN_MARKER", marker.to_str().expect("marker"))
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_ROOT",
            cwd.path().join("remote-sessions").to_str().expect("root"),
        )
        .with_stdin_str("");
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "mcp",
            "--host",
            "fixture-role",
            "--out-dir",
            cwd.path().join("journal").to_str().expect("out"),
        ],
        options,
    );
    assert_eq!(out.code, 75, "{}", out.stderr_text());
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !marker.exists(),
        "failed SSH transport left an orphan process"
    );
}

#[test]
fn json_rpc_batches_are_rejected_atomically_before_upstream_execution() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let marker = cwd.path().join("upstream-invoked");
    let fake = cwd.path().join("peekaboo");
    fs::write(
        &fake,
        "#!/bin/sh\nwhile IFS= read -r line; do : > \"$MCP_BATCH_MARKER\"; done\n",
    )
    .expect("fake");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("NILS_MACOS_AGENT_PEEKABOO_BIN", fake.to_str().expect("fake"))
        .with_env("MCP_BATCH_MARKER", marker.to_str().expect("marker"))
        .with_stdin_str(
            "[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"shell\"}}]\n",
        );
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "mcp",
            "--out-dir",
            cwd.path().join("batch-journal").to_str().expect("out"),
        ],
        options,
    );
    assert_eq!(out.code, 0, "{}", out.stderr_text());
    let frame: serde_json::Value =
        serde_json::from_str(out.stdout_text().trim()).expect("protocol error");
    assert_eq!(frame["error"]["code"], -32600);
    assert!(!marker.exists(), "batch reached upstream");
}

#[test]
fn tool_level_is_error_results_are_journaled_as_failures() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    fs::write(
        &fake,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"id":1'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[]}}' ;;
    *'"id":2'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":null}' ;;
  esac
done
"#,
    )
    .expect("fake");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");
    let out_dir = cwd.path().join("tool-error-journal");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("NILS_MACOS_AGENT_PEEKABOO_BIN", fake.to_str().expect("fake"))
        .with_stdin_str(concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"see\",\"arguments\":{}}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"shutdown\",\"params\":{}}\n",
        ));
    let out = harness.run_with_options(
        cwd.path(),
        &["mcp", "--out-dir", out_dir.to_str().expect("out")],
        options,
    );
    assert_eq!(out.code, 0, "{}", out.stderr_text());
    let journal = fs::read_to_string(out_dir.join("steps.jsonl")).expect("journal");
    let first: serde_json::Value =
        serde_json::from_str(journal.lines().next().expect("first")).expect("step");
    assert_eq!(first["status"], "failed");
    assert_eq!(first["failure_class"], "upstream_mcp");
}

#[test]
fn cancellation_is_forwarded_while_a_request_is_in_flight() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    fs::write(
        &fake,
        r#"#!/bin/bash
while IFS= read -r line; do
  case "$line" in
    *'"id":1'*)
      if IFS= read -r -t 1 cancellation; then
        printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"cancelled":true}}'
      else
        printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"cancelled":false}}'
      fi
      ;;
    *'"id":2'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":null}' ;;
  esac
done
"#,
    )
    .expect("fake");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");
    let options = harness
        .cmd_options(cwd.path())
        .with_env("NILS_MACOS_AGENT_PEEKABOO_BIN", fake.to_str().expect("fake"))
        .with_stdin_str(concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"see\",\"arguments\":{}}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":1}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"shutdown\",\"params\":{}}\n",
        ));
    let started = Instant::now();
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "mcp",
            "--out-dir",
            cwd.path().join("cancel-journal").to_str().expect("out"),
        ],
        options,
    );
    assert_eq!(out.code, 0, "{}", out.stderr_text());
    assert!(started.elapsed() < Duration::from_millis(800));
    let first: serde_json::Value =
        serde_json::from_str(out.stdout_text().lines().next().expect("response"))
            .expect("response JSON");
    assert_eq!(first["result"]["cancelled"], true);
}

#[test]
fn nonzero_upstream_exit_is_not_reported_as_a_clean_mcp_session() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    fs::write(
        &fake,
        "#!/bin/sh\nwhile IFS= read -r line; do printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":null}'; done\nexit 9\n",
    )
    .expect("fake");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake.to_str().expect("fake"),
        )
        .with_stdin_str("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"shutdown\",\"params\":{}}\n");
    let out_dir = cwd.path().join("nonzero-journal");
    let out = harness.run_with_options(
        cwd.path(),
        &["mcp", "--out-dir", out_dir.to_str().expect("out")],
        options,
    );
    assert_eq!(out.code, 70, "{}", out.stderr_text());
    assert_terminal_failure_step(&out_dir, "mcp_upstream_exit");
}

#[test]
fn silent_upstream_is_terminated_at_the_mcp_response_deadline() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    fs::write(&fake, "#!/bin/sh\nIFS= read -r line\nsleep 2\n").expect("fake");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake.to_str().expect("fake"),
        )
        .with_env("NILS_MACOS_AGENT_TEST_MCP_RESPONSE_TIMEOUT_MS", "40")
        .with_stdin_str(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"see\",\"arguments\":{}}}\n",
        );
    let started = Instant::now();
    let out_dir = cwd.path().join("deadline-journal");
    let out = harness.run_with_options(
        cwd.path(),
        &["mcp", "--out-dir", out_dir.to_str().expect("out")],
        options,
    );
    assert_eq!(out.code, 70, "{}", out.stderr_text());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_terminal_failure_step(&out_dir, "mcp_response_timeout");
}

#[test]
fn blocked_upstream_stdin_is_bounded_by_the_mcp_deadline() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    fs::write(&fake, "#!/bin/sh\nsleep 2\n").expect("fake");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");
    let payload = "x".repeat(1024 * 1024);
    let input = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{{\"name\":\"see\",\"arguments\":{{\"payload\":\"{payload}\"}}}}}}\n"
    );
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake.to_str().expect("fake"),
        )
        .with_env("NILS_MACOS_AGENT_TEST_MCP_RESPONSE_TIMEOUT_MS", "40")
        .with_stdin_str(&input);
    let started = Instant::now();
    let out_dir = cwd.path().join("blocked-write-journal");
    let out = harness.run_with_options(
        cwd.path(),
        &["mcp", "--out-dir", out_dir.to_str().expect("out")],
        options,
    );
    assert_eq!(out.code, 70, "{}", out.stderr_text());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_terminal_failure_step(&out_dir, "mcp_write_timeout");
}

#[test]
fn malformed_json_rpc_requests_never_reach_upstream() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    let log = cwd.path().join("upstream.log");
    fs::write(
        &fake,
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$MCP_REQUEST_LOG"
  printf '%s\n' '{"jsonrpc":"2.0","id":9,"result":null}'
done
"#,
    )
    .expect("fake");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":true,\"method\":\"tools/list\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":7}\n",
        "{\"jsonrpc\":\"1.0\",\"id\":3,\"method\":\"tools/list\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"shutdown\",\"params\":{}}\n",
    );
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake.to_str().expect("fake"),
        )
        .with_env("MCP_REQUEST_LOG", log.to_str().expect("log"))
        .with_stdin_str(input);
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "mcp",
            "--out-dir",
            cwd.path()
                .join("invalid-request-journal")
                .to_str()
                .expect("out"),
        ],
        options,
    );
    assert_eq!(out.code, 0, "{}", out.stderr_text());
    assert_eq!(fs::read_to_string(&log).expect("log").lines().count(), 1);
    assert_eq!(out.stdout_text().lines().count(), 4);
}

#[test]
fn malformed_or_unknown_json_rpc_responses_fail_closed() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    for (name, response) in [
        ("wrong-version", r#"{"jsonrpc":"1.0","id":1,"result":null}"#),
        ("boolean-id", r#"{"jsonrpc":"2.0","id":true,"result":null}"#),
        (
            "both-result-error",
            r#"{"jsonrpc":"2.0","id":1,"result":null,"error":{"code":-1,"message":"bad"}}"#,
        ),
        ("neither", r#"{"jsonrpc":"2.0","id":1}"#),
        ("unknown-id", r#"{"jsonrpc":"2.0","id":99,"result":null}"#),
    ] {
        let fake = cwd.path().join(format!("peekaboo-{name}"));
        fs::write(
            &fake,
            format!("#!/bin/sh\nIFS= read -r line\nprintf '%s\\n' '{response}'\n"),
        )
        .expect("fake");
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");
        let options = harness
            .cmd_options(cwd.path())
            .with_env(
                "NILS_MACOS_AGENT_PEEKABOO_BIN",
                fake.to_str().expect("fake"),
            )
            .with_stdin_str(
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"shutdown\",\"params\":{}}\n",
            );
        let out_dir = cwd.path().join(format!("response-{name}"));
        let out = harness.run_with_options(
            cwd.path(),
            &["mcp", "--out-dir", out_dir.to_str().expect("out")],
            options,
        );
        assert_eq!(out.code, 70, "{name}: {}", out.stderr_text());
        assert_terminal_failure_step(&out_dir, "mcp_protocol");
    }
}

fn assert_terminal_failure_step(out_dir: &std::path::Path, expected_class: &str) {
    let journal = fs::read_to_string(out_dir.join("steps.jsonl")).expect("terminal step log");
    let terminal: serde_json::Value =
        serde_json::from_str(journal.lines().last().expect("terminal failure step"))
            .expect("terminal step JSON");
    assert!(matches!(
        terminal["status"].as_str(),
        Some("failed" | "unknown")
    ));
    assert_eq!(terminal["failure_class"], expected_class);
    assert_eq!(terminal["command"], "mcp");
    assert!(!journal.contains("idless-secret-canary"));
}

#[test]
fn ssh_mcp_preserves_remote_upstream_error_class_and_journal() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake_peekaboo = cwd.path().join("peekaboo");
    fs::write(
        &fake_peekaboo,
        "#!/bin/sh\nIFS= read -r line\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":null}'\nexit 9\n",
    )
    .expect("peekaboo");
    fs::set_permissions(&fake_peekaboo, fs::Permissions::from_mode(0o755)).expect("chmod");
    let fake_ssh = cwd.path().join("ssh");
    fs::write(
        &fake_ssh,
        r#"#!/bin/sh
while [ "$1" = "-o" ]; do shift 2; done
[ "$1" = "--" ] && shift
shift
shift
exec "$NILS_MACOS_AGENT_REMOTE_BIN" "$@"
"#,
    )
    .expect("ssh");
    fs::set_permissions(&fake_ssh, fs::Permissions::from_mode(0o755)).expect("chmod ssh");
    let out_dir = cwd.path().join("remote-failure-journal");
    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake_peekaboo.to_str().expect("peekaboo"),
        )
        .with_env("NILS_MACOS_AGENT_SSH_BIN", fake_ssh.to_str().expect("ssh"))
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_BIN",
            harness.macos_agent_bin().to_str().expect("agent"),
        )
        .with_env(
            "NILS_MACOS_AGENT_REMOTE_ROOT",
            cwd.path().join("remote-sessions").to_str().expect("root"),
        )
        .with_stdin_str("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"shutdown\",\"params\":{}}\n");
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "mcp",
            "--host",
            "fixture-role",
            "--out-dir",
            out_dir.to_str().expect("out"),
        ],
        options,
    );
    assert_eq!(out.code, 70, "{}", out.stderr_text());
    assert_eq!(out.stderr_json()["error"]["class"], "upstream");
    assert!(out_dir.join("manifest.json").is_file());
    assert!(out_dir.join("steps.jsonl").is_file());
}

use tempfile::TempDir;

use crate::common;
#[test]
fn click_retries_then_succeeds_when_policy_allows() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let counter_file = cwd.path().join("cliclick-counter.txt");

    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "AGENTS_MACOS_AGENT_STUB_COUNTER_FILE",
            counter_file.to_str().unwrap(),
        )
        .with_env("AGENTS_MACOS_AGENT_STUB_CLICLICK_FAIL_UNTIL", "1");

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "--timeout-ms",
            "15000",
            "--retries",
            "1",
            "--retry-delay-ms",
            "0",
            "input",
            "click",
            "--x",
            "10",
            "--y",
            "10",
        ],
        options,
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(
        payload["result"]["meta"]["attempts_used"],
        serde_json::json!(2)
    );
    let attempts = std::fs::read_to_string(&counter_file)
        .expect("counter file")
        .trim()
        .parse::<u32>()
        .expect("counter number");
    assert_eq!(attempts, 2, "expected one retry");
}

#[test]
fn click_without_retries_fails_on_first_transient_error() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");
    let counter_file = cwd.path().join("cliclick-counter.txt");

    let options = harness
        .cmd_options(cwd.path())
        .with_env(
            "AGENTS_MACOS_AGENT_STUB_COUNTER_FILE",
            counter_file.to_str().unwrap(),
        )
        .with_env("AGENTS_MACOS_AGENT_STUB_CLICLICK_FAIL_UNTIL", "1");

    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "--timeout-ms",
            "15000",
            "input",
            "click",
            "--x",
            "10",
            "--y",
            "10",
        ],
        options,
    );

    assert_eq!(out.code, 1);
    let payload: serde_json::Value =
        serde_json::from_str(&out.stderr_text()).expect("stderr should be json");
    assert_eq!(payload["error"]["category"], serde_json::json!("runtime"));
    assert_eq!(
        payload["error"]["operation"],
        serde_json::json!("input.click")
    );
    assert!(
        payload["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("input.click failed via `cliclick`")
    );
    assert!(payload["error"]["hints"].is_array());
}

#[test]
fn json_metadata_exposes_retry_and_timeout_policy() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("tempdir");

    let out = harness.run(
        cwd.path(),
        &[
            "--format",
            "json",
            "--dry-run",
            "--retries",
            "3",
            "--timeout-ms",
            "2500",
            "input",
            "click",
            "--x",
            "1",
            "--y",
            "2",
        ],
    );

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let payload: serde_json::Value =
        serde_json::from_str(&out.stdout_text()).expect("stdout should be json");
    assert_eq!(
        payload["result"]["policy"]["dry_run"],
        serde_json::json!(true)
    );
    assert_eq!(payload["result"]["policy"]["retries"], serde_json::json!(3));
    assert_eq!(
        payload["result"]["policy"]["retry_delay_ms"],
        serde_json::json!(150)
    );
    assert_eq!(
        payload["result"]["policy"]["timeout_ms"],
        serde_json::json!(2500)
    );
    assert_eq!(
        payload["result"]["meta"]["dry_run"],
        serde_json::json!(true)
    );
    assert_eq!(payload["result"]["meta"]["retries"], serde_json::json!(3));
    assert_eq!(
        payload["result"]["meta"]["timeout_ms"],
        serde_json::json!(2500)
    );
    assert_eq!(
        payload["result"]["meta"]["attempts_used"],
        serde_json::json!(0)
    );
}

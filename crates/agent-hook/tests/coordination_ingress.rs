mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use pretty_assertions::assert_eq;
use serde_json::json;

use support::Fixture;

fn policy(first_action: &str) -> String {
    format!(
        r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.2"

[[rules]]
id = "runtime.pre-tool-policy"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Write"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = {{ id = "decision.{first_action}.v1", reason_code = "policy-{first_action}"{message} }}

[[rules]]
id = "runtime.pre-tool-coordination"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Write"
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = {{ id = "agent-session.coordination.v1", reason_code = "coordination-admitted" }}
"#,
        message = if first_action == "block" {
            ", message = \"blocked before coordination\""
        } else {
            ""
        }
    )
}

fn install_coordination_handler(fixture: &Fixture) {
    install_coordination_handler_with(
        fixture,
        "#!/bin/sh\nset -eu\ndd of=\"$COORDINATION_CAPTURE\" status=none\n",
    );
}

fn install_coordination_handler_with(fixture: &Fixture, source: &str) {
    let hooks = fixture.home.join(".codex/hooks");
    fs::create_dir_all(&hooks).expect("hook directory");
    let handler = hooks.join("session-coordination-guard.py");
    fs::write(&handler, source).expect("coordination handler");
    fs::set_permissions(&handler, fs::Permissions::from_mode(0o700)).expect("handler mode");
}

#[test]
fn terminal_failure_runs_coordination_after_policy_to_close_the_operation() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.2"

[[rules]]
id = "runtime.post-tool-policy"
products = ["codex"]
events = ["PostToolUseFailure"]
matcher = "Write"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "post-policy-block", message = "post policy blocked" }

[[rules]]
id = "runtime.post-tool-coordination"
products = ["codex"]
events = ["PostToolUseFailure"]
matcher = "Write"
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.coordination.v1", reason_code = "coordination-completed" }
"#;
    let fixture = Fixture::new(policy);
    install_coordination_handler(&fixture);
    let capture = fixture.root.join("post-failure-coordination.json");
    let payload = json!({
        "hook_event_name": "PostToolUseFailure",
        "tool_name": "Write",
        "tool_use_id": "tool-call-1",
        "cwd": fixture.root,
        "tool_input": {"path": fixture.root.join("target.txt")},
        "tool_response": {"success": false}
    })
    .to_string();
    let output = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[(
            "COORDINATION_CAPTURE",
            capture.to_str().expect("capture path"),
        )],
    );
    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_json()["data"]["action"], "block");
    assert_eq!(
        output.stdout_json()["data"]["reasons"][1]["code"],
        "coordination-completed"
    );
    assert_eq!(
        fs::read_to_string(capture).expect("completion payload"),
        payload
    );
}

#[test]
fn coordination_shadow_is_side_effect_free_and_handler_block_is_authoritative() {
    let fixture = Fixture::new(&policy("allow"));
    install_coordination_handler(&fixture);
    let capture = fixture.root.join("shadow-coordination.json");
    let shadow = fixture.run_with_env(
        &[
            "dispatch",
            "--product",
            "codex",
            "--shadow",
            "--format",
            "json",
        ],
        Some(&pre_tool_payload(&fixture)),
        &[(
            "COORDINATION_CAPTURE",
            capture.to_str().expect("capture path"),
        )],
    );
    assert_eq!(shadow.code, 0, "stderr={}", shadow.stderr_text());
    assert_eq!(shadow.stdout_json()["data"]["action"], "allow");
    assert_eq!(
        shadow.stdout_json()["data"]["shadow"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert!(!capture.exists());

    install_coordination_handler_with(
        &fixture,
        "#!/bin/sh\nset -eu\nprintf '%s\\n' '{\"decision\":\"block\",\"reason\":\"claim-conflict\"}'\n",
    );
    let blocked = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&pre_tool_payload(&fixture)),
    );
    assert_eq!(blocked.code, 1, "stderr={}", blocked.stderr_text());
    assert_eq!(blocked.stdout_json()["data"]["action"], "block");
    assert_eq!(
        blocked.stdout_json()["data"]["reasons"][1]["code"],
        "coordination-admitted"
    );
}

#[test]
fn coordination_capability_has_no_configurable_command_or_advisory_mode() {
    let base = policy("allow");
    for (needle, replacement) in [
        ("mode = \"enforce\"", "mode = \"shadow\""),
        ("failure_posture = \"closed\"", "failure_posture = \"warn\""),
        ("override_class = \"locked\"", "override_class = \"free\""),
    ] {
        let index = base.rfind(needle).expect("coordination rule field");
        let mut invalid = base.clone();
        invalid.replace_range(index..index + needle.len(), replacement);
        let output = Fixture::new(&invalid).run(&["validate", "--format", "json"], None);
        assert_eq!(output.code, 65, "{needle} -> {replacement}");
        assert_eq!(
            output.stdout_json()["error"]["code"],
            "coordination-rule-not-locked"
        );
    }

    let arbitrary = base.replace(
        "{ id = \"agent-session.coordination.v1\", reason_code = \"coordination-admitted\" }",
        "{ id = \"agent-session.coordination.v1\", reason_code = \"coordination-admitted\", handler_id = \"arbitrary\" }",
    );
    let output = Fixture::new(&arbitrary).run(&["validate", "--format", "json"], None);
    assert_eq!(output.code, 65);
    assert_eq!(output.stdout_json()["error"]["code"], "policy-invalid");
}

fn pre_tool_payload(fixture: &Fixture) -> String {
    json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "tool_use_id": "tool-call-1",
        "cwd": fixture.root,
        "tool_input": {"path": fixture.root.join("target.txt")}
    })
    .to_string()
}

#[test]
fn coordination_runs_inside_the_owned_ingress_only_after_policy_allows() {
    let blocked = Fixture::new(&policy("block"));
    install_coordination_handler(&blocked);
    let blocked_capture = blocked.root.join("blocked-coordination.json");
    let blocked_output = blocked.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&pre_tool_payload(&blocked)),
        &[(
            "COORDINATION_CAPTURE",
            blocked_capture.to_str().expect("capture path"),
        )],
    );
    assert_eq!(
        blocked_output.code,
        1,
        "stderr={}",
        blocked_output.stderr_text()
    );
    assert_eq!(blocked_output.stdout_json()["data"]["action"], "block");
    assert!(
        !blocked_capture.exists(),
        "a blocked aggregate must not admit a coordination operation"
    );

    let allowed = Fixture::new(&policy("allow"));
    install_coordination_handler(&allowed);
    let allowed_capture = allowed.root.join("allowed-coordination.json");
    let allowed_payload = pre_tool_payload(&allowed);
    let allowed_output = allowed.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&allowed_payload),
        &[(
            "COORDINATION_CAPTURE",
            allowed_capture.to_str().expect("capture path"),
        )],
    );
    assert_eq!(
        allowed_output.code,
        0,
        "stderr={}",
        allowed_output.stderr_text()
    );
    assert_eq!(allowed_output.stdout_json()["data"]["action"], "allow");
    assert_eq!(
        fs::read_to_string(&allowed_capture).expect("captured provider payload"),
        allowed_payload
    );
    assert_eq!(
        allowed_output.stdout_json()["data"]["reasons"][1]["code"],
        "coordination-admitted"
    );
}

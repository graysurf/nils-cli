mod support;

use std::fs;

use pretty_assertions::assert_eq;
use support::Fixture;

const POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.read-only"
products = ["codex", "claude"]
events = ["PreToolUse"]
priority = 10
mode = "shadow"
failure_posture = "closed"
override_class = "locked"
capability = { id = "execution.read-only.v1", reason_code = "read-only-capability" }
"#;

#[test]
fn policy_accepts_the_closed_read_only_capability_id() {
    let fixture = Fixture::new(POLICY);
    let validate = fixture.run(&["validate", "--format", "json"], None);

    assert_eq!(validate.code, 0, "stderr={}", validate.stderr_text());
    assert_eq!(validate.stdout_json()["data"]["rule_count"], 1);

    let inventory = fixture.run(&["inventory", "--format", "json"], None);
    assert_eq!(inventory.code, 0, "stderr={}", inventory.stderr_text());
    assert_eq!(
        inventory.stdout_json()["data"]["rules"][0]["capability_id"],
        "execution.read-only.v1"
    );
}

#[test]
fn shadow_rejection_is_evidence_only_and_does_not_change_admission() {
    let fixture = Fixture::new(POLICY);
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": fixture.root,
        "tool_input": {"command": "echo this-is-not-a-trusted-producer"}
    })
    .to_string();
    let output = fixture.run(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(&payload),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_json()["data"]["action"], "allow");
    assert_eq!(
        output.stdout_json()["data"]["reasons"],
        serde_json::json!([])
    );
    assert_eq!(
        output.stdout_json()["data"]["shadow"][0],
        serde_json::json!({
            "rule_id": "runtime.read-only",
            "action": "block",
            "code": "read-only-command-unsupported"
        })
    );
}

#[test]
fn codex_and_claude_record_the_same_shadow_decision_for_equivalent_input() {
    let fixture = Fixture::new(POLICY);
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": fixture.root,
        "tool_input": {"command": "echo unsupported"}
    })
    .to_string();
    let decisions = ["codex", "claude"].map(|product| {
        fixture
            .run(
                &["dispatch", "--product", product, "--format", "json"],
                Some(&payload),
            )
            .stdout_json()["data"]["shadow"]
            .clone()
    });

    assert_eq!(decisions[0], decisions[1]);
}

#[test]
fn capability_cannot_be_enabled_before_the_cutover_contract_changes() {
    let policy = POLICY.replace("mode = \"shadow\"", "mode = \"enforce\"");
    let output = Fixture::new(&policy).run(&["validate", "--format", "json"], None);

    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "read-only-capability-not-shadow"
    );
}

#[test]
fn trace_records_typed_shadow_evidence_without_command_content() {
    let fixture = Fixture::new(POLICY);
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": fixture.root,
        "tool_input": {"command": "echo private-command-material"}
    })
    .to_string();
    let output = fixture.run(
        &[
            "dispatch",
            "--product",
            "codex",
            "--trace",
            "--format",
            "json",
        ],
        Some(&payload),
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let trace = fs::read_to_string(fixture.state_home.join("agent-hook/trace.jsonl"))
        .expect("redacted trace");
    assert!(!trace.contains("private-command-material"));
    let entry: serde_json::Value = serde_json::from_str(trace.trim()).expect("trace JSON");
    assert_eq!(
        entry["shadow"][0],
        serde_json::json!({
            "rule_id": "runtime.read-only",
            "action": "block",
            "code": "read-only-command-unsupported"
        })
    );
}

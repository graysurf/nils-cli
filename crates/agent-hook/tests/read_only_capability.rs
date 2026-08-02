mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

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

const FALLBACK_POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.21.1"

[[rules]]
id = "runtime.read-only"
products = ["codex", "claude"]
events = ["PreToolUse"]
matcher = "Bash"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "execution.read-only.v1", reason_code = "read-only-capability", fallback_handler_id = "pre-edit-intent-gate" }

[[rules]]
id = "runtime.codex.pre-edit"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Bash"
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "runtime-kit.handler.v1", handler_id = "pre-edit-intent-gate" }

[[rules]]
id = "runtime.claude.pre-edit"
products = ["claude"]
events = ["PreToolUse"]
matcher = "Bash"
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "runtime-kit.handler.v1", handler_id = "pre-edit-intent-gate" }
"#;

const UNRELATED_HANDLER_RULE: &str = r#"
[[rules]]
id = "runtime.checkout-lease"
products = ["codex", "claude"]
events = ["PreToolUse"]
matcher = "Bash"
priority = 30
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "runtime-kit.handler.v1", handler_id = "checkout-lease-guard" }
"#;

fn install_fallback_handler(fixture: &Fixture) {
    for hooks in [
        fixture.home.join(".codex/hooks"),
        fixture.home.join(".claude/hooks"),
    ] {
        fs::create_dir_all(&hooks).expect("hooks");
        let handler = hooks.join("pre-edit-intent-gate.py");
        fs::write(
            &handler,
            r#"#!/bin/sh
printf invoked > "$HOME/pre-edit-invoked"
if [ "${FALLBACK_HANDLER_BLOCK:-}" = 1 ]; then
  printf '%s\n' '{"decision":"block","reason":"project-dev-required"}'
fi
"#,
        )
        .expect("handler");
        fs::set_permissions(&handler, fs::Permissions::from_mode(0o700)).expect("handler mode");
        let unrelated = hooks.join("checkout-lease-guard.py");
        fs::write(
            &unrelated,
            r#"#!/bin/sh
printf invoked > "$HOME/checkout-lease-invoked"
"#,
        )
        .expect("unrelated handler");
        fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o700))
            .expect("unrelated handler mode");
    }
}

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
fn enforce_accepts_a_valid_same_release_read_only_descriptor() {
    let policy = POLICY.replace("mode = \"shadow\"", "mode = \"enforce\"");
    let fixture = Fixture::new(&policy);
    let producer = nils_test_support::bin::resolve("agent-docs")
        .canonicalize()
        .expect("canonical agent-docs binary");
    let command = format!(
        "builtin command {} --docs-home {} --project-path {} preflight --intent project-dev --format json",
        producer.display(),
        fixture.root.display(),
        fixture.root.display()
    );
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": fixture.root,
        "tool_input": {"command": command}
    })
    .to_string();
    let output = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
    );

    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    assert_eq!(output.stdout_json()["data"]["action"], "allow");
    assert_eq!(
        output.stdout_json()["data"]["reasons"],
        serde_json::json!([{
            "rule_id": "runtime.read-only",
            "code": "read-only-capability",
            "disposition": "allow"
        }])
    );
    assert!(output.stdout_json()["data"]["shadow"].is_null());
}

#[test]
fn enforce_rejects_unsupported_and_same_release_mutation_descriptors() {
    let policy = POLICY.replace("mode = \"shadow\"", "mode = \"enforce\"");
    let fixture = Fixture::new(&policy);
    let producer = nils_test_support::bin::resolve("agent-docs")
        .canonicalize()
        .expect("canonical agent-docs binary");
    let mutation = format!(
        "builtin command {} --docs-home {} --project-path {} init --force",
        producer.display(),
        fixture.root.display(),
        fixture.root.display()
    );
    for (command, expected_code) in [
        (
            "echo not-a-trusted-producer",
            "read-only-command-unsupported",
        ),
        (mutation.as_str(), "read-only-effect-rejected"),
    ] {
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "cwd": fixture.root,
            "tool_input": {"command": command}
        })
        .to_string();
        let output = fixture.run(
            &["dispatch", "--product", "claude", "--format", "json"],
            Some(&payload),
        );

        assert_eq!(
            output.code,
            1,
            "command={command}; stderr={}",
            output.stderr_text()
        );
        assert_eq!(output.stdout_json()["data"]["action"], "block");
        assert_eq!(
            output.stdout_json()["data"]["reasons"][0]["code"],
            expected_code,
            "command={command}"
        );
        assert!(output.stdout_json()["data"]["shadow"].is_null());
    }
}

#[test]
fn enforce_valid_read_only_bypasses_only_the_declared_fallback_handler() {
    let policy = format!("{FALLBACK_POLICY}{UNRELATED_HANDLER_RULE}");
    for product in ["codex", "claude"] {
        let fixture = Fixture::new(&policy);
        install_fallback_handler(&fixture);
        let producer = nils_test_support::bin::resolve("agent-docs")
            .canonicalize()
            .expect("canonical agent-docs binary");
        let command = format!(
            "builtin command {} --docs-home {} --project-path {} preflight --intent project-dev --format json",
            producer.display(),
            fixture.root.display(),
            fixture.root.display()
        );
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "cwd": fixture.root,
            "tool_input": {"command": command}
        })
        .to_string();
        let output = fixture.run_with_env(
            &["dispatch", "--product", product, "--format", "json"],
            Some(&payload),
            &[("FALLBACK_HANDLER_BLOCK", "1")],
        );

        assert_eq!(
            output.code,
            0,
            "product={product}; stdout={}; stderr={}",
            output.stdout_text(),
            output.stderr_text()
        );
        assert_eq!(output.stdout_json()["data"]["action"], "allow");
        assert_eq!(
            output.stdout_json()["data"]["reasons"],
            serde_json::json!([
                {
                    "rule_id": "runtime.read-only",
                    "code": "read-only-capability",
                    "disposition": "allow"
                },
                {
                    "rule_id": format!("runtime.{product}.pre-edit"),
                    "code": "read-only-capability-bypass",
                    "disposition": "allow"
                },
                {
                    "rule_id": "runtime.checkout-lease",
                    "code": "checkout-lease-guard",
                    "disposition": "allow"
                }
            ])
        );
        assert!(!fixture.home.join("pre-edit-invoked").exists());
        assert!(fixture.home.join("checkout-lease-invoked").is_file());
    }
}

#[test]
fn enforce_non_read_only_falls_through_to_the_declared_project_dev_handler() {
    for (block_handler, expected_code, expected_action) in [(false, 0, "allow"), (true, 1, "block")]
    {
        let fixture = Fixture::new(FALLBACK_POLICY);
        install_fallback_handler(&fixture);
        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "cwd": fixture.root,
            "tool_input": {"command": "printf changed > tracked.txt"}
        })
        .to_string();
        let block = if block_handler { "1" } else { "0" };
        let output = fixture.run_with_env(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
            &[("FALLBACK_HANDLER_BLOCK", block)],
        );

        assert_eq!(
            output.code,
            expected_code,
            "stderr={}",
            output.stderr_text()
        );
        assert_eq!(output.stdout_json()["data"]["action"], expected_action);
        assert_eq!(
            output.stdout_json()["data"]["reasons"][0],
            serde_json::json!({
                "rule_id": "runtime.read-only",
                "code": "read-only-command-unsupported:project-dev-fallback",
                "disposition": "allow"
            })
        );
        assert_eq!(
            output.stdout_json()["data"]["reasons"][1]["rule_id"],
            "runtime.codex.pre-edit"
        );
        assert!(fixture.home.join("pre-edit-invoked").is_file());
    }
}

#[test]
fn fallback_policy_rejects_unsafe_or_missing_project_dev_pairs() {
    let unsafe_handler = FALLBACK_POLICY.replace("pre-edit-intent-gate", "checkout-lease-guard");
    let missing_later_pair = FALLBACK_POLICY.replacen("priority = 20", "priority = 5", 1);
    for (policy, expected_code) in [
        (unsafe_handler, "read-only-fallback-handler-unsupported"),
        (missing_later_pair, "read-only-fallback-pair-invalid"),
    ] {
        let fixture = Fixture::new(&policy);
        let output = fixture.run(&["validate", "--format", "json"], None);

        assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
        assert_eq!(output.stdout_json()["error"]["code"], expected_code);
    }
}

#[test]
fn seventeen_enforced_read_only_rules_fit_the_dispatch_child_budget() {
    let rules = (0..17)
        .map(|index| {
            POLICY
                .replace("runtime.read-only", &format!("runtime.read-only-{index}"))
                .replace("priority = 10", &format!("priority = {index}"))
                .replace("mode = \"shadow\"", "mode = \"enforce\"")
                .replace(
                    "schema_version = \"agent-hook.policy.v1\"\nbundle_id = \"runtime-kit\"\nversion = \"2026.07.20.1\"\n\n",
                    "",
                )
        })
        .collect::<String>();
    let policy = format!(
        "schema_version = \"agent-hook.policy.v1\"\nbundle_id = \"runtime-kit\"\nversion = \"2026.07.20.1\"\n\n{rules}"
    );
    let fixture = Fixture::new(&policy);
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": fixture.root,
        "tool_input": {"command": "echo unsupported"}
    })
    .to_string();
    let output = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
    );

    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_json()["data"]["action"], "block");
    assert_eq!(
        output.stdout_json()["data"]["reasons"]
            .as_array()
            .map(Vec::len),
        Some(17)
    );
}

#[test]
fn eighteen_enforced_read_only_rules_exceed_the_dispatch_child_budget() {
    let rules = (0..18)
        .map(|index| {
            POLICY
                .replace("runtime.read-only", &format!("runtime.read-only-{index}"))
                .replace("priority = 10", &format!("priority = {index}"))
                .replace("mode = \"shadow\"", "mode = \"enforce\"")
                .replace(
                    "schema_version = \"agent-hook.policy.v1\"\nbundle_id = \"runtime-kit\"\nversion = \"2026.07.20.1\"\n\n",
                    "",
                )
        })
        .collect::<String>();
    let policy = format!(
        "schema_version = \"agent-hook.policy.v1\"\nbundle_id = \"runtime-kit\"\nversion = \"2026.07.20.1\"\n\n{rules}"
    );
    let fixture = Fixture::new(&policy);
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": fixture.root,
        "tool_input": {"command": "echo unsupported"}
    })
    .to_string();
    let output = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
    );

    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "dispatch-child-budget-exceeded"
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

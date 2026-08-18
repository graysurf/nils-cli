mod support;

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::json;

use support::Fixture;

const POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "dsh-runtime-kit-test"
version = "2026.08.18.1"

[[rules]]
id = "dsh.block-plus-one"
products = ["dsh"]
events = ["PreToolUse"]
matcher = "runtime_kit_plus_one"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "plus-one-blocked", message = "fixture denial" }
"#;

fn request(fixture: &Fixture) -> String {
    json!({
        "schema_version": "agent-hook.dsh-ingress.v1",
        "event": "tools/pre-execute",
        "call_id": "dsh-call-1",
        "cwd": fixture.root,
        "tool": {
            "name": "runtime_kit_plus_one",
            "arguments": {"value": 41}
        }
    })
    .to_string()
}

#[test]
fn dsh_pre_execute_is_normalized_and_evaluated_by_the_shared_policy_engine() {
    let fixture = Fixture::new(POLICY);
    let output = fixture.run(
        &["dispatch", "--product", "dsh", "--format", "json"],
        Some(&request(&fixture)),
    );

    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    let envelope = output.stdout_json();
    assert_eq!(envelope["schema_version"], "cli.agent-hook.dispatch.v1");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["product"], "dsh");
    assert_eq!(envelope["data"]["event"], "PreToolUse");
    assert_eq!(envelope["data"]["action"], "block");
    assert_eq!(
        envelope["data"]["reasons"],
        json!([{
            "rule_id": "dsh.block-plus-one",
            "code": "plus-one-blocked",
            "disposition": "block"
        }])
    );
}

#[test]
fn dsh_ingress_rejects_unknown_versions_and_fields() {
    let fixture = Fixture::new(POLICY);

    let wrong_version =
        request(&fixture).replace("agent-hook.dsh-ingress.v1", "agent-hook.dsh-ingress.v2");
    let output = fixture.run(
        &["dispatch", "--product", "dsh", "--format", "json"],
        Some(&wrong_version),
    );
    assert_eq!(output.code, 65);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "dsh-ingress-version-invalid"
    );

    let mut unknown_field: serde_json::Value =
        serde_json::from_str(&request(&fixture)).expect("request JSON");
    unknown_field["provider_extension"] = json!(true);
    let output = fixture.run(
        &["dispatch", "--product", "dsh", "--format", "json"],
        Some(&unknown_field.to_string()),
    );
    assert_eq!(output.code, 65);
    assert_eq!(output.stdout_json()["error"]["code"], "dsh-ingress-invalid");

    let mut unknown_nested_field: serde_json::Value =
        serde_json::from_str(&request(&fixture)).expect("request JSON");
    unknown_nested_field["tool"]["provider_extension"] = json!(true);
    let output = fixture.run(
        &["dispatch", "--product", "dsh", "--format", "json"],
        Some(&unknown_nested_field.to_string()),
    );
    assert_eq!(output.code, 65);
    assert_eq!(output.stdout_json()["error"]["code"], "dsh-ingress-invalid");
}

#[test]
fn dsh_ingress_enforces_field_boundaries_and_event_identity() {
    let fixture = Fixture::new(POLICY);
    let base: serde_json::Value = serde_json::from_str(&request(&fixture)).expect("request JSON");
    let mut cases = Vec::new();

    let mut unsupported_event = base.clone();
    unsupported_event["event"] = json!("tools/post-execute");
    cases.push((unsupported_event, "provider-event-unsupported"));

    for call_id in [String::new(), "x".repeat(257)] {
        let mut value = base.clone();
        value["call_id"] = json!(call_id);
        cases.push((value, "dsh-ingress-invalid"));
    }
    for tool_name in [String::new(), "x".repeat(257)] {
        let mut value = base.clone();
        value["tool"]["name"] = json!(tool_name);
        cases.push((value, "dsh-ingress-invalid"));
    }

    let mut relative_cwd = base.clone();
    relative_cwd["cwd"] = json!("relative/path");
    cases.push((relative_cwd, "dsh-ingress-invalid"));
    for arguments in [json!(null), json!([])] {
        let mut value = base.clone();
        value["tool"]["arguments"] = arguments;
        cases.push((value, "dsh-ingress-invalid"));
    }

    for (value, error_code) in cases {
        let output = fixture.run(
            &["dispatch", "--product", "dsh", "--format", "json"],
            Some(&value.to_string()),
        );
        assert_eq!(output.code, 65, "input={value}");
        assert_eq!(output.stdout_json()["error"]["code"], error_code);
    }

    let mismatch = fixture.run(
        &[
            "dispatch",
            "--product",
            "dsh",
            "--event",
            "tools/post-execute",
            "--format",
            "json",
        ],
        Some(&request(&fixture)),
    );
    assert_eq!(mismatch.code, 65);
    assert_eq!(
        mismatch.stdout_json()["error"]["code"],
        "provider-event-mismatch"
    );

    let mut boundary = base;
    boundary["call_id"] = json!("c".repeat(256));
    boundary["tool"]["name"] = json!("t".repeat(256));
    let accepted = fixture.run(
        &["dispatch", "--product", "dsh", "--format", "json"],
        Some(&boundary.to_string()),
    );
    assert_eq!(accepted.code, 0, "stderr={}", accepted.stderr_text());
    let envelope = accepted.stdout_json();
    let request_id = envelope["data"]["request_id"].clone();
    let config_digest = envelope["data"]["config_digest"].clone();
    let policy_digest = envelope["data"]["policy_digest"].clone();
    assert_eq!(
        envelope,
        json!({
            "schema_version": "cli.agent-hook.dispatch.v1",
            "ok": true,
            "data": {
                "schema_version": "agent-hook.normalized-decision.v1",
                "request_id": request_id,
                "product": "dsh",
                "event": "PreToolUse",
                "action": "allow",
                "reasons": [],
                "config_digest": config_digest,
                "policy_digest": policy_digest,
                "recovery_applied": false
            }
        })
    );
    assert!(
        envelope["data"]["request_id"]
            .as_str()
            .is_some_and(|value| value.starts_with("request:"))
    );
    for field in ["config_digest", "policy_digest"] {
        assert!(
            envelope["data"][field]
                .as_str()
                .is_some_and(|value| value.starts_with("sha256:")),
            "field={field}"
        );
    }
}

#[test]
fn dsh_tool_name_collisions_keep_dsh_cwd_only_target_semantics() {
    let fixture = Fixture::new(POLICY);

    for tool_name in ["Write", "Edit", "MultiEdit", "NotebookEdit", "apply_patch"] {
        let mut collision: serde_json::Value =
            serde_json::from_str(&request(&fixture)).expect("request JSON");
        collision["tool"]["name"] = json!(tool_name);
        collision["tool"]["arguments"] = json!({});
        let output = fixture.run(
            &["dispatch", "--product", "dsh", "--format", "json"],
            Some(&collision.to_string()),
        );
        assert_eq!(
            output.code,
            0,
            "tool={tool_name} stdout={} stderr={}",
            output.stdout_text(),
            output.stderr_text()
        );
        assert_eq!(output.stdout_json()["data"]["action"], "allow");
    }
}

#[test]
#[rustfmt::skip]
fn dsh_registration_is_bundle_owned_and_legacy_file_handlers_are_rejected() { // stale-audit: keep-contract
    let fixture = Fixture::new(POLICY);
    let dsh_config = fixture.home.join(".dsh/cordis.patch.yml");
    for action in ["--apply", "--repair", "--remove"] {
        let output = fixture.run(&["setup", "--product", "dsh", action, "--format", "json"], None);
        assert_eq!(output.code, 0, "action={action} stderr={}", output.stderr_text());
        assert_eq!(output.stdout_json()["data"]["status"], "unsupported", "action={action}");
        assert_eq!(output.stdout_json()["data"]["changed"], false, "action={action}");
        assert!(!dsh_config.exists(), "action={action} created an absent DSH config");
    }

    fs::create_dir_all(dsh_config.parent().expect("DSH config parent")).expect("DSH config dir");
    let sentinel = b"foreign: cordis-registration\n";
    fs::write(&dsh_config, sentinel).expect("DSH config sentinel");
    Fixture::set_private(&dsh_config);
    for action in ["--apply", "--repair", "--remove"] {
        let output = fixture.run(&["setup", "--product", "dsh", action, "--format", "json"], None);
        assert_eq!(output.code, 0, "action={action} stderr={}", output.stderr_text());
        assert_eq!(output.stdout_json()["data"]["status"], "unsupported", "action={action}");
        assert_eq!(output.stdout_json()["data"]["changed"], false, "action={action}");
        assert_eq!(fs::read(&dsh_config).expect("DSH config sentinel retained"), sentinel, "action={action}");
    }

    let setup = fixture.run(
        &["setup", "--product", "dsh", "--dry-run", "--format", "json"],
        None,
    );
    assert_eq!(setup.code, 0, "stderr={}", setup.stderr_text());
    assert_eq!(setup.stdout_json()["data"]["status"], "unsupported");
    assert_eq!(setup.stdout_json()["data"]["changed"], false);
    assert_eq!(setup.stdout_json()["data"]["apply_allowed"], false);
    assert_eq!(
        setup.stdout_json()["data"]["compatibility_owner"],
        "dsh-runtime-kit"
    );

    let doctor = fixture.run(&["doctor", "--product", "dsh", "--format", "json"], None);
    assert_eq!(doctor.code, 0, "stderr={}", doctor.stderr_text());
    assert_eq!(doctor.stdout_json()["data"][0]["dispatch_supported"], true);
    assert_eq!(
        doctor.stdout_json()["data"][0]["registration_owner"],
        "dsh-runtime-kit"
    );

    let codex_doctor = fixture.run(&["doctor", "--product", "codex", "--format", "json"], None);
    assert_eq!(
        codex_doctor.code,
        0,
        "stderr={}",
        codex_doctor.stderr_text()
    );
    assert!(
        codex_doctor.stdout_json()["data"][0]["dispatch_supported"].is_null(),
        "existing provider output must retain its v1 field set"
    );
    assert!(
        codex_doctor.stdout_json()["data"][0]["registration_owner"].is_null(),
        "existing provider output must retain its v1 field set"
    );

    let invalid_policy = POLICY.replace(
        "capability = { id = \"decision.block.v1\", reason_code = \"plus-one-blocked\", message = \"fixture denial\" }",
        "capability = { id = \"runtime-kit.handler.v1\", handler_id = \"pre-edit-intent-gate\" }",
    );
    let invalid = Fixture::new(&invalid_policy).run(&["validate", "--format", "json"], None);
    assert_eq!(invalid.code, 65);
    assert_eq!(
        invalid.stdout_json()["error"]["code"],
        "policy-capability-event-unsupported"
    );
}

#[test]
fn dsh_v1_rejects_capabilities_without_native_decision_or_target_semantics() {
    let unsupported = [
        "decision.warn.v1",
        "decision.context.v1",
        "agent-session.activity.v1",
        "agent-session.owner-liveness.v1",
        "agent-session.semantic-conflict.v1",
        "execution.read-only.v1",
        "agent-session.coordination.v1",
    ];

    for capability_id in unsupported {
        let capability = match capability_id {
            "decision.warn.v1" => {
                r#"capability = { id = "decision.warn.v1", reason_code = "warn", message = "warn" }"#
            }
            "decision.context.v1" => {
                r#"capability = { id = "decision.context.v1", reason_code = "context", text = "context" }"#
            }
            "agent-session.owner-liveness.v1" => {
                r#"capability = { id = "agent-session.owner-liveness.v1", reason_code = "owner", legacy_ttl_seconds = 60 }"# // stale-audit: keep-contract
            }
            "agent-session.activity.v1" => {
                r#"capability = { id = "agent-session.activity.v1", reason_code = "activity" }"#
            }
            "agent-session.semantic-conflict.v1" => {
                r#"capability = { id = "agent-session.semantic-conflict.v1", reason_code = "conflict" }"#
            }
            "execution.read-only.v1" => {
                r#"capability = { id = "execution.read-only.v1", reason_code = "read-only" }"#
            }
            "agent-session.coordination.v1" => {
                r#"capability = { id = "agent-session.coordination.v1", reason_code = "coordination" }"#
            }
            _ => unreachable!(),
        };
        let policy = POLICY.replace(
            "capability = { id = \"decision.block.v1\", reason_code = \"plus-one-blocked\", message = \"fixture denial\" }",
            capability,
        );
        let output = Fixture::new(&policy).run(&["validate", "--format", "json"], None);
        assert_eq!(output.code, 65, "capability={capability_id}");
        assert_eq!(
            output.stdout_json()["error"]["code"],
            "policy-capability-event-unsupported",
            "capability={capability_id}"
        );
    }
}

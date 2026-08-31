use std::io::Write;
use std::process::{Command, Stdio};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

fn request(payload: Value, source_id: &str, rules: Value) -> Value {
    json!({
        "schema_version": "agent-hook.data-policy.evaluate.v1",
        "phase": "final-result",
        "source_id": source_id,
        "sink_id": "session.persist",
        "identity": {
            "session_id": "synthetic-session",
            "workspace_digest": format!("sha256:{}", "a".repeat(64)),
            "workspace_generation": "generation-1",
            "call_id": "call-1",
            "root_call_id": "call-1",
            "turn": 1,
            "step": 1
        },
        "rules": rules,
        "payload": payload
    })
}

fn evaluate(input: &Value) -> (i32, String, Value) {
    evaluate_raw(&serde_json::to_vec(input).expect("request"))
}

fn evaluate_raw(input: &[u8]) -> (i32, String, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-hook"))
        .args(["data-policy", "evaluate", "--format", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agent-hook");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input)
        .expect("write request");
    let output = child.wait_with_output().expect("wait");
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let envelope = serde_json::from_str(&stdout).expect("json envelope");
    (output.status.code().expect("exit code"), stdout, envelope)
}

#[test]
fn rejects_input_above_one_mib_before_deserialization() {
    let oversized = vec![b'x'; 1024 * 1024 + 1];
    let (code, stdout, envelope) = evaluate_raw(&oversized);
    assert_eq!(code, 65);
    assert_eq!(envelope["error"]["code"], "provider-input-too-large");
    assert!(!stdout.contains(&"x".repeat(64)));
}

#[test]
fn redacts_sensitive_fields_without_echoing_the_candidate() {
    let sentinel = format!("{}_synthetic", "ghp");
    let input = request(
        json!({"token": sentinel, "safe": "kept"}),
        "tool.shell",
        json!([{"rule_id":"data.sensitive.redact", "class_id":"sensitive", "action":"redact"}]),
    );
    let (code, stdout, envelope) = evaluate(&input);
    assert_eq!(code, 0);
    assert!(!stdout.contains(&sentinel));
    assert_eq!(
        envelope["schema_version"],
        "cli.agent-hook.data-policy-evaluate.v1"
    );
    assert_eq!(
        envelope["data"]["schema_version"],
        "agent-hook.data-policy.decision.v1"
    );
    assert!(
        envelope["data"]["request_id"]
            .as_str()
            .is_some_and(|value| value.starts_with("request:") && value.len() == 40)
    );
    assert_eq!(envelope["data"]["action"], "redact");
    assert_eq!(
        envelope["data"]["audit"]["matched_rule_ids"],
        json!(["data.sensitive.redact"])
    );
    assert_eq!(
        envelope["data"]["replacement"]["token"],
        "[redacted:sensitive]"
    );
    assert_eq!(envelope["data"]["replacement"]["safe"], "kept");
}

#[test]
fn corpus_covers_structured_binary_streamed_and_portable_path_boundaries() {
    let rules = json!([
        {"rule_id":"data.sensitive.deny", "class_id":"sensitive", "action":"deny"},
        {"rule_id":"data.machine-path.quarantine", "class_id":"machine-local-path", "action":"quarantine"}
    ]);
    let cases = [
        (
            "structured-key",
            json!({"access_token":"synthetic-unlabeled-value"}),
            "deny",
        ),
        (
            "signature-text",
            json!("prefix ghp_syntheticvalue suffix"),
            "deny",
        ),
        (
            "streamed-signature",
            json!([{"text":"ghp_synth"}, {"text":"eticvalue"}]),
            "deny",
        ),
        (
            "binary-under-sensitive-key",
            json!({"api_key":"c3ludGhldGljLWJ5dGVz"}),
            "deny",
        ),
        (
            "inline-linux-path",
            json!("wrote /home/fixture/project/result.txt"),
            "quarantine",
        ),
        (
            "inline-macos-path",
            json!("wrote /Users/fixture/project/result.txt"),
            "quarantine",
        ),
        (
            "inline-windows-path",
            json!(r"wrote C:\Users\fixture\result.txt"),
            "quarantine",
        ),
        ("counter-key", json!({"token_count":42}), "allow"),
        (
            "short-prefix",
            json!("documentation mentions ghp_ only"),
            "allow",
        ),
        (
            "generic-home-root",
            json!("portable example /home/"),
            "allow",
        ),
        (
            "opaque-binary",
            json!({"type":"binary", "encoding":"base64", "data":"c3ludGhldGljLWJ5dGVz"}),
            "allow",
        ),
        (
            "unlabeled-value",
            json!({"note":"synthetic-unlabeled-value"}),
            "allow",
        ),
    ];

    for (name, payload, expected) in cases {
        let (_, stdout, envelope) = evaluate(&request(payload, "tool.native", rules.clone()));
        assert_eq!(envelope["data"]["action"], expected, "{name}");
        assert!(
            !stdout.contains("synthetic-unlabeled-value") || expected == "allow",
            "{name}"
        );
    }
}

#[test]
fn request_binding_changes_with_every_governed_dimension() {
    let rules = json!([
        {"rule_id":"data.sensitive.deny", "class_id":"sensitive", "action":"deny"},
        {"rule_id":"data.machine-path.allow", "class_id":"machine-local-path", "action":"allow"}
    ]);
    let first = request(json!({"safe":true}), "tool.native", rules.clone());
    let (_, _, first_envelope) = evaluate(&first);
    let variants = [
        ("session_id", json!("session-2")),
        (
            "workspace_digest",
            json!(format!("sha256:{}", "b".repeat(64))),
        ),
        ("workspace_generation", json!("generation-2")),
        ("call_id", json!("call-2")),
        ("root_call_id", json!("root-call-2")),
        ("parent_call_id", json!("parent-call-2")),
        ("turn", json!(2)),
        ("step", json!(2)),
    ];
    for (field, value) in variants {
        let mut changed = first.clone();
        changed["identity"][field] = value;
        let (_, _, changed_envelope) = evaluate(&changed);
        assert_ne!(
            first_envelope["data"]["request_id"], changed_envelope["data"]["request_id"],
            "request id must bind {field}"
        );
        assert_ne!(
            first_envelope["data"]["audit"]["binding_digest"],
            changed_envelope["data"]["audit"]["binding_digest"],
            "binding digest must bind {field}"
        );
    }
    let variants = [
        ("phase", json!("pre-call")),
        ("source_id", json!("tool.web")),
        ("sink_id", json!("tool.execute")),
    ];
    for (field, value) in variants {
        let mut changed = first.clone();
        changed[field] = value;
        let (_, _, changed_envelope) = evaluate(&changed);
        assert_ne!(
            first_envelope["data"]["audit"]["binding_digest"],
            changed_envelope["data"]["audit"]["binding_digest"],
            "binding digest must bind {field}"
        );
    }
    let mut changed_action = first.clone();
    changed_action["rules"][0]["action"] = json!("redact");
    let (_, _, changed_action_envelope) = evaluate(&changed_action);
    assert_ne!(
        first_envelope["data"]["audit"]["binding_digest"],
        changed_action_envelope["data"]["audit"]["binding_digest"],
        "binding digest must bind rule actions"
    );
    let mut changed_rule_id = first.clone();
    changed_rule_id["rules"][0]["rule_id"] = json!("data.sensitive.alternate");
    let (_, _, changed_rule_id_envelope) = evaluate(&changed_rule_id);
    assert_ne!(
        first_envelope["data"]["audit"]["binding_digest"],
        changed_rule_id_envelope["data"]["audit"]["binding_digest"],
        "binding digest must bind rule IDs"
    );
    let mut reordered = first.clone();
    reordered["rules"].as_array_mut().expect("rules").reverse();
    let (_, _, reordered_envelope) = evaluate(&reordered);
    assert_ne!(
        first_envelope["data"]["audit"]["binding_digest"],
        reordered_envelope["data"]["audit"]["binding_digest"],
        "binding digest must bind rule order"
    );
}

#[test]
fn quarantines_machine_paths_but_allows_provider_opaque_references() {
    let rules = json!([{"rule_id":"data.machine-path.quarantine", "class_id":"machine-local-path", "action":"quarantine"}]);
    let path = "/home/fixture/project/result.txt";
    let (code, stdout, envelope) = evaluate(&request(
        json!({"artifact": path}),
        "tool.web",
        rules.clone(),
    ));
    assert_eq!(code, 0);
    assert!(!stdout.contains(path));
    assert_eq!(envelope["data"]["action"], "quarantine");
    assert_eq!(envelope["data"]["replacement"]["quarantined"], true);

    let (_, _, opaque) = evaluate(&request(
        json!({"reference": path}),
        "provider.opaque-reference",
        rules,
    ));
    assert_eq!(opaque["data"]["action"], "allow");
}

#[test]
fn denies_protected_root_and_rejects_identity_or_rule_drift() {
    let mut protected = request(
        json!({"path_digest": format!("sha256:{}", "b".repeat(64))}),
        "filesystem.canonical-target",
        json!([{"rule_id":"data.protected-root.deny", "class_id":"protected-root", "action":"deny"}]),
    );
    protected["phase"] = json!("protected-root");
    let (_, stdout, envelope) = evaluate(&protected);
    assert_eq!(envelope["data"]["action"], "deny");
    assert!(!stdout.contains("synthetic-session"));

    let mut invalid = protected.clone();
    invalid["identity"]["workspace_digest"] = json!("not-a-digest");
    let (code, _, envelope) = evaluate(&invalid);
    assert_eq!(code, 65);
    assert_eq!(envelope["error"]["code"], "data-policy-identity-invalid");

    let mut duplicate = protected;
    duplicate["rules"] = json!([
        {"rule_id":"data.protected-root.deny", "class_id":"protected-root", "action":"deny"},
        {"rule_id":"data.protected-root.allow", "class_id":"protected-root", "action":"allow"}
    ]);
    let (code, _, envelope) = evaluate(&duplicate);
    assert_eq!(code, 65);
    assert_eq!(envelope["error"]["code"], "data-policy-rules-invalid");
}

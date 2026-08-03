//! Regression coverage for binary/broker capability compatibility during a live
//! upgrade (`sympoies/nils-cli#1409`, live-version lifecycle workstream).
//!
//! A fleet upgrade replaced the installed binary while an older session and its
//! broker were still live. The newer hook could not read the older broker's
//! coordination state and reported a generic `coordination-invalid`, which reads
//! as corruption and offers no recovery. Version drift is a distinct,
//! recoverable condition and must be classified as one.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use support::{Fixture, now_epoch};

const POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.prompt-conflict"
products = ["codex", "claude"]
events = ["UserPromptSubmit"]
priority = 100
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.semantic-conflict.v1", reason_code = "prompt-conflict" }

[[rules]]
id = "runtime.pre-edit-owner"
products = ["codex", "claude"]
events = ["PreToolUse"]
matcher = "Write|Edit|NotebookEdit|MultiEdit|apply_patch"
priority = 110
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.owner-liveness.v1", reason_code = "pre-edit-owner" }
"#;

fn write_registry(fixture: &Fixture, registry: &Value) {
    let coordination = fixture.session_state.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination directory");
    fs::set_permissions(&coordination, fs::Permissions::from_mode(0o700))
        .expect("coordination mode");
    let path = coordination.join("registry.json");
    fs::write(&path, serde_json::to_vec(registry).expect("registry JSON")).expect("registry");
    Fixture::set_private(&path);
}

fn write_heartbeat(fixture: &Fixture, session: &str, incarnation: &str, epoch: i64) {
    let heartbeat = fixture
        .session_state
        .join("sessions")
        .join(session)
        .join("coordination/heartbeat");
    fs::create_dir_all(heartbeat.parent().expect("heartbeat parent")).expect("heartbeat directory");
    fs::write(&heartbeat, format!("{incarnation}:{epoch}\n")).expect("heartbeat");
    Fixture::set_private(&heartbeat);
}

fn managed_env<'a>() -> [(&'a str, &'a str); 2] {
    [
        ("AGENT_SESSION_ID", "managed-session"),
        ("AGENT_SESSION_RUNTIME_ID", "managed-runtime"),
    ]
}

fn spool_events(fixture: &Fixture) -> Vec<Value> {
    let spool = fixture.session_state.join("observation/spool");
    let Ok(entries) = fs::read_dir(&spool) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    paths.sort();
    let mut events = Vec::new();
    for path in paths {
        let body = fs::read_to_string(&path).expect("segment");
        for line in body.lines().filter(|line| !line.trim().is_empty()) {
            events.push(serde_json::from_str(line).expect("event JSON"));
        }
    }
    events
}

/// A coordination registry written by a different release generation is version
/// drift, not corruption. The mutation lane still fails closed, but the
/// classification and its bounded recovery action have to be explicit.
#[test]
fn incompatible_registry_generation_is_typed_as_version_skew() {
    let fixture = Fixture::new(POLICY);
    write_registry(
        &fixture,
        &json!({
            "schema_version": "agent-session.coordination-registry.v9",
            "fingerprint_epoch": 1,
            "fingerprint_key": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "brokers": {},
            "claims": []
        }),
    );
    let envs = managed_env();
    let write = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "cwd": fixture.root,
        "tool_input": {"path": fixture.root.join("target.txt")}
    })
    .to_string();

    let mutation = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&write),
        &envs,
    );
    assert_eq!(
        mutation.code,
        65,
        "mutation must stay fail-closed across a version boundary: stdout={}",
        mutation.stdout_text()
    );
    let error = mutation.stdout_json();
    assert_eq!(
        error["error"]["code"], "runtime-version-skew",
        "version drift must not be reported as registry corruption: {error}"
    );
    assert!(
        error["error"]["details"]["recovery_action"]
            .as_str()
            .is_some_and(|value| value.contains("agent-session")),
        "the skew error must name one bounded recovery action: {error}"
    );

    // The conversation lane stays usable so the recovery can be requested.
    let prompt = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(r#"{"hook_event_name":"UserPromptSubmit","prompt":"status"}"#),
        &envs,
    );
    assert_eq!(prompt.code, 0, "stderr={}", prompt.stderr_text());
    let decision = prompt.stdout_json();
    assert_eq!(decision["data"]["action"], "warn", "{decision}");
    let codes: Vec<String> = decision["data"]["reasons"]
        .as_array()
        .expect("reasons")
        .iter()
        .map(|reason| reason["code"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        codes.contains(&"runtime-version-skew".to_string()),
        "degraded prompt must carry the skew classification: {decision}"
    );

    let skew: Vec<Value> = spool_events(&fixture)
        .into_iter()
        .filter(|event| event["code"] == "runtime-version-skew")
        .collect();
    assert!(
        !skew.is_empty(),
        "version skew must be recorded on the observation plane"
    );
    for event in &skew {
        assert!(
            event["recovery_action"]
                .as_str()
                .is_some_and(|value| value.contains("agent-session")),
            "skew evidence must retain its recovery action: {event}"
        );
    }
}

/// A malformed registry is still corruption. The new skew classification must
/// not swallow it.
#[test]
fn malformed_registry_remains_invalid_rather_than_version_skew() {
    let fixture = Fixture::new(POLICY);
    let coordination = fixture.session_state.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination directory");
    let path = coordination.join("registry.json");
    fs::write(&path, b"{").expect("malformed registry");
    Fixture::set_private(&path);

    let write = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "cwd": fixture.root,
        "tool_input": {"path": fixture.root.join("target.txt")}
    })
    .to_string();
    let mutation = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&write),
        &managed_env(),
    );
    assert_eq!(mutation.code, 65);
    assert_eq!(
        mutation.stdout_json()["error"]["code"],
        "coordination-invalid"
    );
}

/// The broker publishes the release that produced its capability state. A
/// release-generation difference is observable evidence for the upgrade path;
/// an ordinary patch difference is not a skew and must stay silent.
#[test]
fn broker_release_drift_is_observable_and_patch_drift_is_not() {
    let local = env!("CARGO_PKG_VERSION");
    let (major, rest) = local.split_once('.').expect("semantic version");
    let (minor, patch) = rest.split_once('.').expect("semantic version");
    let now = now_epoch();
    let payload = r#"{"hook_event_name":"UserPromptSubmit","prompt":"status"}"#;

    // A different minor generation is drift the upgrade path has to see.
    let skewed_minor = format!(
        "{major}.{}.0",
        minor.parse::<u64>().expect("numeric minor") + 1
    );
    // A patch difference within the same generation is compatible.
    let compatible_patch = format!(
        "{major}.{minor}.{}",
        patch.parse::<u64>().expect("numeric patch") + 1
    );

    for (broker_version, expect_skew) in [(skewed_minor, true), (compatible_patch, false)] {
        let fixture = Fixture::new(POLICY);
        write_registry(
            &fixture,
            &json!({
                "schema_version": "agent-session.coordination-registry.v1",
                "fingerprint_epoch": 1,
                "fingerprint_key": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "brokers": {
                    "managed-session": {
                        "session_id": "managed-session",
                        "incarnation": "managed-runtime",
                        "state": "ready",
                        "heartbeat_epoch": now,
                        "coordination_mode": "enforce",
                        "binary_version": broker_version
                    }
                },
                "claims": []
            }),
        );
        write_heartbeat(&fixture, "managed-session", "managed-runtime", now);

        let prompt = fixture.run_with_env(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(payload),
            &managed_env(),
        );
        assert_eq!(
            prompt.code,
            0,
            "a broker release difference must never break the conversation lane: stdout={}",
            prompt.stdout_text()
        );

        let recorded = spool_events(&fixture)
            .iter()
            .any(|event| event["code"] == "broker-release-skew");
        assert_eq!(
            recorded, expect_skew,
            "broker_version={broker_version} local={local} recorded={recorded}"
        );
    }
}

/// An older broker that never published a release is compatibility state,
/// not skew. It must not start reporting drift for every dispatch.
#[test]
fn broker_without_a_published_release_is_not_reported_as_drift() {
    let fixture = Fixture::new(POLICY);
    let now = now_epoch();
    write_registry(
        &fixture,
        &json!({
            "schema_version": "agent-session.coordination-registry.v1",
            "fingerprint_epoch": 1,
            "fingerprint_key": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "brokers": {
                "managed-session": {
                    "session_id": "managed-session",
                    "incarnation": "managed-runtime",
                    "state": "ready",
                    "heartbeat_epoch": now,
                    "coordination_mode": "enforce"
                }
            },
            "claims": []
        }),
    );
    write_heartbeat(&fixture, "managed-session", "managed-runtime", now);

    let prompt = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(r#"{"hook_event_name":"UserPromptSubmit","prompt":"status"}"#),
        &managed_env(),
    );
    assert_eq!(prompt.code, 0, "stderr={}", prompt.stderr_text());
    assert!(
        !spool_events(&fixture)
            .iter()
            .any(|event| event["code"] == "broker-release-skew"),
        "a broker record without a published release must not be reported as drift"
    );
}

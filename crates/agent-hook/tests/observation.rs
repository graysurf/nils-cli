//! Regression coverage for the centralized `agent-session.observation.v1`
//! control-plane event plane (`sympoies/nils-cli#1409`).
//!
//! Before this plane existed, hook evidence was split between a replaceable
//! `activity.diagnostic.json`, an opt-in `--trace` that the installed provider
//! ingress never passes, and unstructured serve stderr. Failures that happened
//! before normalization or policy load left no durable record at all, so a
//! degraded session could not be diagnosed after the fact.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use support::Fixture;

const POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.pre-edit"
products = ["codex", "claude"]
events = ["PreToolUse"]
matcher = "Write|Edit|NotebookEdit|MultiEdit|apply_patch"
priority = 100
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.allow.v1", reason_code = "pre-edit-allowed" }
"#;

fn spool_dir(fixture: &Fixture) -> PathBuf {
    fixture.session_state.join("observation/spool")
}

fn spool_events(fixture: &Fixture) -> Vec<Value> {
    read_spool_events(&spool_dir(fixture))
}

fn read_spool_events(directory: &Path) -> Vec<Value> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut segments: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|suffix| suffix == "jsonl"))
        .collect();
    segments.sort();
    let mut events = Vec::new();
    for segment in segments {
        let body = fs::read_to_string(&segment).expect("spool segment");
        for line in body.lines().filter(|line| !line.trim().is_empty()) {
            events.push(serde_json::from_str(line).expect("spool event JSON"));
        }
    }
    events
}

fn codes(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["code"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Every dispatch outcome, including one that fails before the provider payload
/// can be normalized, has to land in the shared spool. Logging only from inside
/// a successfully normalized decision cannot explain the failure classes that
/// actually broke live sessions.
#[test]
fn every_dispatch_terminal_outcome_records_one_observation_event() {
    let fixture = Fixture::new(POLICY);
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "cwd": fixture.root,
        "tool_input": {"path": fixture.root.join("target.txt")}
    })
    .to_string();

    let allowed = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
    );
    assert_eq!(allowed.code, 0, "stderr={}", allowed.stderr_text());

    let events = spool_events(&fixture);
    assert_eq!(
        events.len(),
        1,
        "one dispatch must record exactly one terminal event: {events:?}"
    );
    let event = &events[0];
    assert_eq!(event["schema_version"], "agent-session.observation.v1");
    assert_eq!(event["component"], "agent-hook");
    assert_eq!(event["stage"], "dispatch");
    assert_eq!(event["code"], "dispatch-completed");
    assert_eq!(event["severity"], "info");
    assert_eq!(event["disposition"], "allow");
    assert_eq!(event["product"], "codex");
    assert_eq!(event["event"], "PreToolUse");
    assert!(
        event["binary_version"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "binary version is required for skew diagnosis: {event}"
    );
    assert!(
        event["duration_ms"].is_u64(),
        "dispatch latency is required for health projection: {event}"
    );

    // A payload that cannot be parsed fails before normalization. That is the
    // exact class the old opt-in trace could never record.
    let malformed = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some("{"),
    );
    assert_ne!(malformed.code, 0);

    let events = spool_events(&fixture);
    assert_eq!(
        events.len(),
        2,
        "a pre-normalization failure must still be observable: {events:?}"
    );
    let failure = &events[1];
    assert_eq!(failure["stage"], "normalize");
    assert_eq!(failure["severity"], "error");
    assert_eq!(failure["disposition"], "error");
    assert_eq!(
        failure["code"], "provider-input-invalid",
        "the stable normalization failure code must be recorded: {failure}"
    );
}

/// The spool is the recovery-critical evidence path, so it must never depend on
/// a healthy serve daemon, broker, or coordination registry.
#[test]
fn observation_spool_is_written_without_a_serve_daemon_or_broker() {
    let fixture = Fixture::new(POLICY);
    assert!(
        !fixture.session_state.join("coordination").exists(),
        "fixture must start without any coordination state"
    );

    let allowed = fixture.run(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(r#"{"hook_event_name":"SessionStart","source":"startup"}"#),
    );
    assert_eq!(allowed.code, 0, "stderr={}", allowed.stderr_text());

    let directory = spool_dir(&fixture);
    let metadata = fs::symlink_metadata(&directory).expect("spool directory");
    assert!(metadata.is_dir());
    assert_eq!(
        metadata.permissions().mode() & 0o777,
        0o700,
        "spool directory must stay private"
    );
    let events = spool_events(&fixture);
    assert_eq!(codes(&events), vec!["dispatch-completed".to_string()]);

    for entry in fs::read_dir(&directory).expect("spool entries") {
        let path = entry.expect("spool entry").path();
        let metadata = fs::symlink_metadata(&path).expect("segment metadata");
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o600,
            "spool segment must stay private: {}",
            path.display()
        );
    }
}

/// The plane records classification only. Prompt text, provider identity,
/// command bodies, and filesystem paths must never reach it.
#[test]
fn observation_events_never_retain_provider_content_or_paths() {
    let fixture = Fixture::new(POLICY);
    let secret_target = fixture.root.join("secret-target.txt");
    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "provider-session-secret",
        "prompt": "prompt-body-secret",
        "cwd": fixture.root,
        "tool_input": {"path": secret_target}
    })
    .to_string();

    let dispatched = fixture.run_with_env(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(&payload),
        &[
            ("AGENT_SESSION_ID", "managed-session-secret"),
            ("AGENT_SESSION_RUNTIME_ID", "managed-runtime-secret"),
        ],
    );
    assert_eq!(dispatched.code, 0, "stderr={}", dispatched.stderr_text());

    let directory = spool_dir(&fixture);
    let mut body = String::new();
    for entry in fs::read_dir(&directory).expect("spool entries") {
        body.push_str(&fs::read_to_string(entry.expect("spool entry").path()).expect("segment"));
    }
    assert!(!body.is_empty(), "spool must contain the dispatch event");
    for secret in [
        "prompt-body-secret",
        "provider-session-secret",
        "managed-session-secret",
        "managed-runtime-secret",
        "secret-target.txt",
        fixture.root.to_str().expect("root UTF-8"),
    ] {
        assert!(!body.contains(secret), "observation plane leaked {secret}");
    }
}

/// Retention is a privacy budget, not an unbounded log. Segments rotate and old
/// segments are dropped so a repeated failure cannot grow without bound.
#[test]
fn observation_spool_stays_inside_its_bounded_segment_budget() {
    let fixture = Fixture::new(POLICY);
    for _ in 0..48 {
        let dispatched = fixture.run(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some("{"),
        );
        assert_ne!(dispatched.code, 0);
    }

    let directory = spool_dir(&fixture);
    let segments: Vec<PathBuf> = fs::read_dir(&directory)
        .expect("spool entries")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|suffix| suffix == "jsonl"))
        .collect();
    assert!(
        !segments.is_empty() && segments.len() <= 16,
        "segment count must stay bounded: {}",
        segments.len()
    );
    for segment in &segments {
        let length = fs::symlink_metadata(segment)
            .expect("segment metadata")
            .len();
        assert!(
            length <= 256 * 1024,
            "segment {} exceeded its byte budget: {length}",
            segment.display()
        );
    }
}

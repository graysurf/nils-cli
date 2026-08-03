//! Regression coverage for the fail-closed liveness defect in
//! `sympoies/nils-cli#1409`.
//!
//! Three confirmed defects are reproduced here:
//!
//! 1. A coordination subsystem failure blocked `UserPromptSubmit`, so the user
//!    could not even ask what was wrong. Plain conversation has to survive in a
//!    read-only lane while arbitrary mutation stays fail-closed.
//! 2. `Stop` kept re-entering the same failing gate until the provider's
//!    consecutive-block cap force-terminated the turn. Provider re-entry
//!    metadata has to produce one deterministic terminal result that still
//!    retains the claim/lease for external reconciliation.
//! 3. A capability that exited before draining its stdin was scored as a failed
//!    capability, so a `closed` plus `locked` rule denied at random depending on
//!    machine load (`sympoies/nils-cli#1420`). Whether a child read its input is
//!    not a verdict; its exit status is.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use support::Fixture;

/// Coordination-backed admission on both the conversation and mutation lanes.
const COORDINATION_POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
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

/// The typed coordination transaction on the terminal Stop delivery.
const STOP_COORDINATION_POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.stop-coordination"
products = ["codex", "claude"]
events = ["Stop"]
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.coordination.v1", reason_code = "coordination-admitted" }
"#;

fn break_coordination_registry(fixture: &Fixture) {
    let coordination = fixture.session_state.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination directory");
    fs::set_permissions(&coordination, fs::Permissions::from_mode(0o700))
        .expect("coordination mode");
    let registry = coordination.join("registry.json");
    fs::write(&registry, b"{").expect("malformed registry");
    Fixture::set_private(&registry);
}

fn managed_env<'a>() -> [(&'a str, &'a str); 2] {
    [
        ("AGENT_SESSION_ID", "managed-session"),
        ("AGENT_SESSION_RUNTIME_ID", "managed-runtime"),
    ]
}

fn reason_codes(decision: &Value) -> Vec<String> {
    decision["data"]["reasons"]
        .as_array()
        .map(|reasons| {
            reasons
                .iter()
                .map(|reason| reason["code"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The conversation lane must survive a coordination failure. Without this the
/// user cannot ask a question, read status, or request the repair that would fix
/// the very subsystem that is blocking them.
#[test]
fn coordination_failure_keeps_conversation_usable_and_mutation_closed() {
    let fixture = Fixture::new(COORDINATION_POLICY);
    break_coordination_registry(&fixture);
    let envs = managed_env();

    for product in ["codex", "claude"] {
        let prompt = fixture.run_with_env(
            &["dispatch", "--product", product, "--format", "json"],
            Some(r#"{"hook_event_name":"UserPromptSubmit","prompt":"why is this blocked"}"#),
            &envs,
        );
        assert_eq!(
            prompt.code,
            0,
            "{product} UserPromptSubmit must stay usable when coordination fails: stdout={} stderr={}",
            prompt.stdout_text(),
            prompt.stderr_text()
        );
        let decision = prompt.stdout_json();
        assert_eq!(decision["ok"], true, "{product} decision envelope");
        assert_eq!(
            decision["data"]["action"], "warn",
            "{product} prompt must degrade, not block: {decision}"
        );
        assert!(
            reason_codes(&decision).contains(&"coordination-degraded-read-only".to_string()),
            "{product} missing the typed degradation classification: {decision}"
        );
        let context = decision["data"]["context"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            context.contains("read-only"),
            "{product} degraded context must state the lane: {context}"
        );
        assert!(
            context.contains("agent-session"),
            "{product} degraded context must name one safe next action: {context}"
        );

        // The provider must receive a context response, never a prompt block.
        let rendered = fixture.run_with_env(
            &["dispatch", "--product", product, "--format", "provider"],
            Some(r#"{"hook_event_name":"UserPromptSubmit","prompt":"why is this blocked"}"#),
            &envs,
        );
        assert_eq!(rendered.code, 0, "stderr={}", rendered.stderr_text());
        let output = rendered.stdout_json();
        assert!(
            output.get("decision").is_none(),
            "{product} degraded prompt must not render a provider block: {output}"
        );
        assert_eq!(
            output["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit",
            "{product} degraded prompt must render additional context: {output}"
        );

        // The mutation lane keeps its fail-closed posture.
        let write = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Write",
            "cwd": fixture.root,
            "tool_input": {"path": fixture.root.join("target.txt")}
        })
        .to_string();
        let mutation = fixture.run_with_env(
            &["dispatch", "--product", product, "--format", "json"],
            Some(&write),
            &envs,
        );
        assert_ne!(
            mutation.code,
            0,
            "{product} mutation must stay fail-closed while coordination is broken: stdout={}",
            mutation.stdout_text()
        );
    }
}

/// The degraded prompt has to be observable and idempotent: the plane records
/// that this exact turn was degraded so a later repair can retry it at most
/// once, and repeated degradation does not multiply the recorded turn.
#[test]
fn degraded_prompt_records_one_replayable_turn_boundary() {
    let fixture = Fixture::new(COORDINATION_POLICY);
    break_coordination_registry(&fixture);
    let envs = managed_env();
    let payload =
        r#"{"hook_event_name":"UserPromptSubmit","prompt_id":"turn-secret","prompt":"status"}"#;

    for _ in 0..2 {
        let prompt = fixture.run_with_env(
            &["dispatch", "--product", "claude", "--format", "json"],
            Some(payload),
            &envs,
        );
        assert_eq!(prompt.code, 0, "stderr={}", prompt.stderr_text());
    }

    let spool = fixture.session_state.join("observation/spool");
    let mut body = String::new();
    for entry in fs::read_dir(&spool).expect("spool entries") {
        body.push_str(&fs::read_to_string(entry.expect("entry").path()).expect("segment"));
    }
    let degraded: Vec<Value> = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("event JSON"))
        .filter(|event| event["code"] == "coordination-degraded-read-only")
        .collect();
    assert_eq!(
        degraded.len(),
        2,
        "each degraded delivery must be observable: {degraded:?}"
    );
    let first = degraded[0]["correlation"]
        .as_str()
        .expect("correlation digest");
    let second = degraded[1]["correlation"]
        .as_str()
        .expect("correlation digest");
    assert_eq!(
        first, second,
        "the same provider turn must share one replay boundary"
    );
    assert!(
        first.starts_with("sha256:"),
        "the replay boundary must be a digest, not raw provider identity: {first}"
    );
    assert!(
        !body.contains("turn-secret"),
        "raw provider turn identity leaked into the plane"
    );
}

/// Stop re-entry is the loop. The provider tells us it is already inside a Stop
/// hook; blocking again cannot change the outcome and only burns the provider's
/// consecutive-block budget. The turn must end while the claim/lease stays held.
#[test]
fn stop_reentry_reaches_a_deterministic_terminal_result() {
    let fixture = Fixture::new(STOP_COORDINATION_POLICY);
    let hooks = fixture.home.join(".codex/hooks");
    fs::create_dir_all(&hooks).expect("codex hook directory");
    let claude_hooks = fixture.home.join(".claude/hooks");
    fs::create_dir_all(&claude_hooks).expect("claude hook directory");
    for directory in [&hooks, &claude_hooks] {
        let handler = directory.join("session-coordination-guard.py");
        fs::write(
            &handler,
            "#!/bin/sh\nset -eu\nprintf '%s\\n' '{\"decision\":\"block\",\"reason\":\"operation-uncertain\"}'\n",
        )
        .expect("coordination handler");
        fs::set_permissions(&handler, fs::Permissions::from_mode(0o700)).expect("handler mode");
    }
    let envs = managed_env();

    for product in ["codex", "claude"] {
        // The first Stop still gates: the operation really is uncertain.
        let first = fixture.run_with_env(
            &["dispatch", "--product", product, "--format", "json"],
            Some(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#),
            &envs,
        );
        assert_eq!(
            first.stdout_json()["data"]["action"],
            "block",
            "{product} first Stop must retain authoritative coordination gating: {}",
            first.stdout_text()
        );

        // The re-entry delivery must terminate instead of blocking again.
        let reentry = fixture.run_with_env(
            &["dispatch", "--product", product, "--format", "json"],
            Some(r#"{"hook_event_name":"Stop","stop_hook_active":true}"#),
            &envs,
        );
        assert_eq!(
            reentry.code,
            0,
            "{product} Stop re-entry must not block again: stdout={} stderr={}",
            reentry.stdout_text(),
            reentry.stderr_text()
        );
        let decision = reentry.stdout_json();
        assert_eq!(
            decision["data"]["action"], "warn",
            "{product} Stop re-entry must produce a terminal warning: {decision}"
        );
        let codes = reason_codes(&decision);
        assert!(
            codes.contains(&"stop-reentry-reconciliation-pending".to_string()),
            "{product} missing the terminal reconciliation classification: {decision}"
        );
        assert!(
            codes.contains(&"operation-uncertain".to_string())
                || codes.contains(&"coordination-admitted".to_string()),
            "{product} must retain the original gating evidence for diagnosis: {decision}"
        );
    }

    // The terminal exit is durable evidence, not a silent release.
    let spool = fixture.session_state.join("observation/spool");
    let mut body = String::new();
    for entry in fs::read_dir(&spool).expect("spool entries") {
        body.push_str(&fs::read_to_string(entry.expect("entry").path()).expect("segment"));
    }
    let pending: Vec<Value> = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("event JSON"))
        .filter(|event| event["code"] == "stop-reentry-reconciliation-pending")
        .collect();
    assert_eq!(
        pending.len(),
        2,
        "each terminal exit must record reconciliation-pending evidence: {pending:?}"
    );
    for event in &pending {
        assert_eq!(event["severity"], "warn");
        assert_eq!(event["disposition"], "reconciliation-pending");
        assert!(
            event["recovery_action"]
                .as_str()
                .is_some_and(|value| value.contains("agent-session")),
            "terminal exit must name the external reconciliation entry point: {event}"
        );
    }
}

/// The liveness rule cannot depend on recognizing the fault. Once the provider
/// reports Stop re-entry, a stage that fails outright must still terminate rather
/// than render another denial the gate cannot converge on.
#[test]
fn stop_reentry_terminates_even_for_an_unclassified_stage_failure() {
    let fixture = Fixture::new(COORDINATION_POLICY);
    // A policy digest mismatch is a hard load failure with no degradation lane.
    fs::write(
        &fixture.policy,
        "schema_version = \"agent-hook.policy.v1\"\n",
    )
    .expect("rewrite policy behind its recorded digest");
    Fixture::set_private(&fixture.policy);
    let envs = managed_env();

    let first = fixture.run_with_env(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#),
        &envs,
    );
    assert_ne!(
        first.code,
        0,
        "an unloadable policy must still fail closed on the first Stop: {}",
        first.stdout_text()
    );

    let reentry = fixture.run_with_env(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(r#"{"hook_event_name":"Stop","stop_hook_active":true}"#),
        &envs,
    );
    assert_eq!(
        reentry.code,
        0,
        "a re-entered Stop must terminate even when the failure is unclassified: stdout={} stderr={}",
        reentry.stdout_text(),
        reentry.stderr_text()
    );
    let decision = reentry.stdout_json();
    assert_eq!(decision["data"]["action"], "warn", "{decision}");
    let codes = reason_codes(&decision);
    assert!(
        codes.contains(&"stop-reentry-reconciliation-pending".to_string()),
        "{decision}"
    );
    assert_eq!(
        codes.len(),
        2,
        "the exact stage code must be retained beside the terminal classification: {decision}"
    );

    // The provider must receive a non-blocking response.
    let rendered = fixture.run_with_env(
        &["dispatch", "--product", "claude", "--format", "provider"],
        Some(r#"{"hook_event_name":"Stop","stop_hook_active":true}"#),
        &envs,
    );
    assert_eq!(rendered.code, 0, "stderr={}", rendered.stderr_text());
    let output = rendered.stdout_json();
    assert!(
        output.get("decision").is_none() && output.get("continue").is_none(),
        "a terminal exit must not render a provider block: {output}"
    );
}

/// A subsystem failure on Stop must degrade for the coordination transaction the
/// same way it already does for the activity observation, otherwise the first
/// Stop delivery deadlocks before re-entry metadata can even help.
#[test]
fn coordination_failure_on_stop_degrades_instead_of_deadlocking() {
    let fixture = Fixture::new(STOP_COORDINATION_POLICY);
    // No coordination consumer is installed, so the typed transaction cannot run.
    let envs = managed_env();

    for product in ["codex", "claude"] {
        let stop = fixture.run_with_env(
            &["dispatch", "--product", product, "--format", "json"],
            Some(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#),
            &envs,
        );
        assert_eq!(
            stop.code,
            0,
            "{product} unavailable coordination must not deadlock Stop: stdout={} stderr={}",
            stop.stdout_text(),
            stop.stderr_text()
        );
        let decision = stop.stdout_json();
        assert_eq!(decision["data"]["action"], "warn", "{decision}");
        assert!(
            reason_codes(&decision)
                .contains(&"coordination-stop-reconciliation-required".to_string()),
            "{product} missing the coordination Stop degradation: {decision}"
        );
    }
}

/// One enforced handler on `SessionStart`, fail-closed and locked, so a spurious
/// failure has nowhere to go but a block the session cannot override.
const HANDLER_POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.session-start"
products = ["codex", "claude"]
events = ["SessionStart"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "runtime-kit.handler.v1", handler_id = "session-start-healthcheck" }
"#;

/// Install a `SessionStart` handler and return a payload too large to fit in the
/// pipe buffer, so the parent's write is still pending when the handler runs.
fn stdin_closing_handler(fixture: &Fixture, body: &str) -> String {
    for provider in [".codex/hooks", ".claude/hooks"] {
        let hooks = fixture.home.join(provider);
        fs::create_dir_all(&hooks).expect("hooks directory");
        let handler = hooks.join("session-start-healthcheck.sh");
        fs::write(&handler, body).expect("handler");
        fs::set_permissions(&handler, fs::Permissions::from_mode(0o755)).expect("handler mode");
    }
    // A pipe holds 64 KiB by default, so a smaller payload would be buffered
    // whole and the race would never be observable. 128 KiB stays well inside
    // the 1 MiB provider input limit.
    let padding = "p".repeat(128 * 1024);
    format!(r#"{{"hook_event_name":"SessionStart","source":"startup","padding":"{padding}"}}"#)
}

/// A capability that exits without draining its stdin is not a failed
/// capability. The remaining write returns `EPIPE`, and counting that as an
/// input failure turned this fail-closed locked rule into a block the session
/// could not override — at random, because whether the child won the race
/// depended on machine load. Six tests in this crate flaked on it under
/// parallel load, each recovering on retry, so the suite hid it.
/// See `sympoies/nils-cli#1420`.
#[test]
fn a_capability_that_stops_reading_stdin_is_judged_by_its_exit_status() {
    let fixture = Fixture::new(HANDLER_POLICY);
    let payload = stdin_closing_handler(
        &fixture,
        // Close the read end, hold it closed while the parent is still writing,
        // then answer. `sleep` is what makes the EPIPE deterministic rather than
        // load-dependent; the defect it pins is the same one.
        "#!/bin/sh\nexec 0<&-\nsleep 0.3\nprintf '{}\\n'\n",
    );

    for product in ["codex", "claude"] {
        let started = fixture.run_with_env(
            &["dispatch", "--product", product, "--format", "json"],
            Some(&payload),
            &managed_env(),
        );
        assert_eq!(
            started.code,
            0,
            "{product} must not fail-close on a handler that closed its stdin: stdout={} stderr={}",
            started.stdout_text(),
            started.stderr_text()
        );
        let decision = started.stdout_json();
        assert_eq!(
            decision["data"]["action"], "allow",
            "{product} must honour the handler's own verdict: {decision}"
        );
        assert!(
            !reason_codes(&decision)
                .iter()
                .any(|code| code.ends_with("capability-failure-closed")),
            "{product} recorded a capability failure for a handler that succeeded: {decision}"
        );
    }
}

/// The tolerance is scoped to the pipe, not to the verdict: a handler that stops
/// reading and then fails must still fail-close, or the fix above would have
/// turned every early-exiting failure into an allow.
#[test]
fn a_capability_that_stops_reading_stdin_and_then_fails_still_fails_closed() {
    let fixture = Fixture::new(HANDLER_POLICY);
    let payload = stdin_closing_handler(&fixture, "#!/bin/sh\nexec 0<&-\nsleep 0.3\nexit 3\n");

    for product in ["codex", "claude"] {
        let started = fixture.run_with_env(
            &["dispatch", "--product", product, "--format", "json"],
            Some(&payload),
            &managed_env(),
        );
        let decision = started.stdout_json();
        assert_eq!(
            decision["data"]["action"], "block",
            "{product} must still fail-close a handler that failed: {decision}"
        );
        assert!(
            reason_codes(&decision)
                .contains(&"runtime.session-start:capability-failure-closed".to_string()),
            "{product} lost the fail-closed classification: {decision}"
        );
    }
}

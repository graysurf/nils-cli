//! Regression coverage for the stale `AGENT_SESSION_BIN` deadlock
//! (`sympoies/nils-cli#1414`).
//!
//! `AGENT_SESSION_BIN` outranks `PATH` in helper resolution, and a long-lived
//! tmux server keeps the environment it started with. A relocation therefore
//! poisoned every session created afterwards: each one fail-closed on its first
//! `UserPromptSubmit`, and the locked fail-closed rule meant the session could
//! not run the recovery commands either.
//!
//! The pre-existing case in `security_review.rs` looks like coverage but removes
//! every managed selector, so the helper is never resolved there. These tests run
//! inside a managed session, which is the only configuration where the defect
//! occurs.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use pretty_assertions::assert_eq;
use serde_json::Value;

use support::Fixture;

const ACTIVITY_POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "coord.activity.prompt-stop"
products = ["codex", "claude"]
events = ["UserPromptSubmit", "Stop"]
priority = 100
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.activity.v1", reason_code = "activity-recorded" }
"#;

/// Install a helper on `PATH` that records the activity event successfully.
fn working_helper(fixture: &Fixture) -> std::path::PathBuf {
    let directory = fixture.root.join("helper-bin");
    fs::create_dir_all(&directory).expect("helper directory");
    let helper = directory.join("agent-session");
    fs::write(&helper, "#!/bin/sh\nexit 0\n").expect("helper");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper mode");
    directory
}

fn spool_codes(fixture: &Fixture) -> Vec<String> {
    let spool = fixture.session_state.join("observation/spool");
    let Ok(entries) = fs::read_dir(&spool) else {
        return Vec::new();
    };
    let mut codes = Vec::new();
    for entry in entries {
        let path = entry.expect("spool entry").path();
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        for line in body.lines().filter(|line| !line.trim().is_empty()) {
            let event: Value = serde_json::from_str(line).expect("event JSON");
            codes.push(event["code"].as_str().unwrap_or_default().to_string());
        }
    }
    codes
}

/// A stale override inside a managed session must not deadlock the prompt. It
/// falls back to the daemon-pinned `PATH` and records why.
#[test]
fn a_stale_helper_override_falls_back_to_path_with_a_typed_classification() {
    let fixture = Fixture::new(ACTIVITY_POLICY);
    let helper_dir = working_helper(&fixture);
    let stale = fixture.root.join("removed-nils-cli/bin/agent-session");
    assert!(!stale.exists(), "the stale override must not resolve");

    for event in [
        r#"{"hook_event_name":"UserPromptSubmit","prompt":"status"}"#,
        r#"{"hook_event_name":"Stop","stop_hook_active":false}"#,
    ] {
        let dispatched = fixture.run_with_env(
            &["dispatch", "--product", "claude", "--format", "json"],
            Some(event),
            &[
                ("AGENT_SESSION_ID", "managed-session"),
                ("AGENT_SESSION_RUNTIME_ID", "managed-runtime"),
                ("AGENT_SESSION_BIN", stale.to_str().expect("stale path")),
                ("PATH", helper_dir.to_str().expect("helper dir")),
            ],
        );
        assert_eq!(
            dispatched.code,
            0,
            "a stale helper override must not deadlock {event}: stdout={} stderr={}",
            dispatched.stdout_text(),
            dispatched.stderr_text()
        );
        assert_eq!(
            dispatched.stdout_json()["data"]["reasons"][0]["code"],
            "activity-recorded",
            "the PATH-resolved helper must have recorded the event: {}",
            dispatched.stdout_text()
        );
    }

    let codes = spool_codes(&fixture);
    assert!(
        codes
            .iter()
            .any(|code| code == "activity-helper-unresolvable"),
        "the discarded override must not be silently masked: {codes:?}"
    );
}

/// An empty assignment is the normalized "resolve through the pinned PATH" value
/// that session creation now writes, so it must behave like an absent override.
#[test]
fn an_empty_helper_override_resolves_through_path() {
    let fixture = Fixture::new(ACTIVITY_POLICY);
    let helper_dir = working_helper(&fixture);

    let dispatched = fixture.run_with_env(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(r#"{"hook_event_name":"UserPromptSubmit","prompt":"status"}"#),
        &[
            ("AGENT_SESSION_ID", "managed-session"),
            ("AGENT_SESSION_RUNTIME_ID", "managed-runtime"),
            ("AGENT_SESSION_BIN", ""),
            ("PATH", helper_dir.to_str().expect("helper dir")),
        ],
    );
    assert_eq!(dispatched.code, 0, "stderr={}", dispatched.stderr_text());
    assert_eq!(
        dispatched.stdout_json()["data"]["reasons"][0]["code"],
        "activity-recorded"
    );
    assert!(
        !spool_codes(&fixture)
            .iter()
            .any(|code| code == "activity-helper-unresolvable"),
        "an empty override is normalized, not a discarded misconfiguration"
    );
}

/// A genuine override still wins, so build pinning keeps working.
#[test]
fn a_resolvable_helper_override_still_takes_effect() {
    let fixture = Fixture::new(ACTIVITY_POLICY);
    // PATH resolves to a helper that fails; the override resolves to one that
    // succeeds. Only honouring the override can produce an allow here.
    let path_dir = fixture.root.join("path-bin");
    fs::create_dir_all(&path_dir).expect("path directory");
    let failing = path_dir.join("agent-session");
    fs::write(&failing, "#!/bin/sh\nexit 65\n").expect("failing helper");
    fs::set_permissions(&failing, fs::Permissions::from_mode(0o700)).expect("failing mode");
    let override_dir = fixture.root.join("override-bin");
    fs::create_dir_all(&override_dir).expect("override directory");
    let pinned = override_dir.join("agent-session");
    fs::write(&pinned, "#!/bin/sh\nexit 0\n").expect("pinned helper");
    fs::set_permissions(&pinned, fs::Permissions::from_mode(0o700)).expect("pinned mode");

    let dispatched = fixture.run_with_env(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(r#"{"hook_event_name":"UserPromptSubmit","prompt":"status"}"#),
        &[
            ("AGENT_SESSION_ID", "managed-session"),
            ("AGENT_SESSION_RUNTIME_ID", "managed-runtime"),
            ("AGENT_SESSION_BIN", pinned.to_str().expect("pinned path")),
            ("PATH", path_dir.to_str().expect("path dir")),
        ],
    );
    assert_eq!(dispatched.code, 0, "stderr={}", dispatched.stderr_text());
    assert_eq!(
        dispatched.stdout_json()["data"]["reasons"][0]["code"],
        "activity-recorded",
        "a resolvable override must outrank PATH: {}",
        dispatched.stdout_text()
    );
}

/// When the helper cannot be resolved at all, the prompt still has to reach the
/// conversation lane instead of deadlocking, and `Stop` still has to terminate.
#[test]
fn a_completely_unresolvable_helper_degrades_instead_of_deadlocking() {
    let fixture = Fixture::new(ACTIVITY_POLICY);
    let empty_dir = fixture.root.join("empty-bin");
    fs::create_dir_all(&empty_dir).expect("empty directory");
    let envs = [
        ("AGENT_SESSION_ID", "managed-session"),
        ("AGENT_SESSION_RUNTIME_ID", "managed-runtime"),
        ("PATH", empty_dir.to_str().expect("empty dir")),
    ];

    let stop = fixture.run_with_env(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#),
        &envs,
    );
    assert_eq!(
        stop.code,
        0,
        "Stop must terminate when the helper is missing: stdout={}",
        stop.stdout_text()
    );
    assert_eq!(
        stop.stdout_json()["data"]["reasons"][0]["code"],
        "activity-stop-reconciliation-required"
    );

    // A re-entered Stop must not block again either.
    let reentry = fixture.run_with_env(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(r#"{"hook_event_name":"Stop","stop_hook_active":true}"#),
        &envs,
    );
    assert_eq!(reentry.code, 0, "stderr={}", reentry.stderr_text());
}

/// Install the managed Claude ingress so the provider reaches `converged`, which
/// is the state whose health claim `sympoies/nils-cli#1414` showed to be false.
fn converge_claude_ingress(fixture: &Fixture) {
    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    let digest = preview.stdout_json()["data"]["plan_digest"]
        .as_str()
        .expect("plan digest")
        .to_string();
    let applied = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--apply",
            "--expected-plan-digest",
            &digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());
}

/// `doctor` reported a converged provider while the capability binary could not
/// execute. Ingress registration is not capability health.
#[test]
fn doctor_fails_when_the_activity_helper_cannot_be_resolved() {
    let fixture = Fixture::new(ACTIVITY_POLICY);
    converge_claude_ingress(&fixture);
    let empty_dir = fixture.root.join("empty-bin");
    fs::create_dir_all(&empty_dir).expect("empty directory");

    let broken = fixture.run_with_env(
        &["doctor", "--product", "claude", "--format", "json"],
        None,
        &[("PATH", empty_dir.to_str().expect("empty dir"))],
    );
    assert_ne!(
        broken.code,
        0,
        "doctor must not report health while the capability cannot run: {}",
        broken.stdout_text()
    );
    assert_eq!(
        broken.stdout_json()["error"]["code"],
        "activity-helper-unresolvable",
        "{}",
        broken.stdout_text()
    );

    // With a resolvable helper the same converged provider reports health.
    let helper_dir = working_helper(&fixture);
    let healthy = fixture.run_with_env(
        &["doctor", "--product", "claude", "--format", "json"],
        None,
        &[("PATH", helper_dir.to_str().expect("helper dir"))],
    );
    assert_eq!(
        healthy.code,
        0,
        "a resolvable helper must clear the capability probe: {}",
        healthy.stdout_text()
    );
    assert_eq!(healthy.stdout_json()["data"][0]["status"], "converged");
}

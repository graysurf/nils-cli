mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::thread;
use std::time::Duration;

use pretty_assertions::assert_eq;
use serde_json::json;

use support::{Fixture, sha256};

const POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.locked-block"
products = ["codex"]
events = ["PreToolUse"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "locked-block", message = "blocked" }
"#;

struct Authorized {
    payload: String,
    command: String,
    snapshot: String,
    capability_file: std::path::PathBuf,
    capability_id: String,
}

fn authorize(
    fixture: &Fixture,
    name: &str,
    scope: &str,
    ttl_seconds: &str,
    envs: &[(&str, &str)],
) -> Authorized {
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "cwd": fixture.root,
        "tool_input": {"command": "edit"}
    })
    .to_string();
    let target = sha256(fixture.root.as_os_str().as_encoded_bytes());
    let command = sha256(b"edit");
    let snapshot = sha256(payload.as_bytes());
    let challenge_file = fixture.root.join(format!("{name}-challenge.json"));
    let capability_file = fixture.root.join(format!("{name}-capability.json"));
    let challenge = fixture.run_with_env(
        &[
            "recovery",
            "challenge",
            "--product",
            "codex",
            "--event",
            "PreToolUse",
            "--target-digest",
            &target,
            "--command-digest",
            &command,
            "--snapshot-digest",
            &snapshot,
            "--scope",
            scope,
            "--ttl-seconds",
            ttl_seconds,
            "--rule",
            "runtime.locked-block",
            "--out",
            challenge_file.to_str().expect("challenge path"),
            "--format",
            "json",
        ],
        None,
        envs,
    );
    assert_eq!(challenge.code, 0, "stderr={}", challenge.stderr_text());
    let challenge_digest = challenge.stdout_json()["result"]["challenge_digest"]
        .as_str()
        .expect("challenge digest")
        .to_string();
    let authorized = fixture.run_with_env(
        &[
            "recovery",
            "authorize",
            "--challenge-file",
            challenge_file.to_str().expect("challenge path"),
            "--expected-challenge-digest",
            &challenge_digest,
            "--out",
            capability_file.to_str().expect("capability path"),
            "--format",
            "json",
        ],
        None,
        envs,
    );
    assert_eq!(authorized.code, 0, "stderr={}", authorized.stderr_text());
    Authorized {
        payload,
        command,
        snapshot,
        capability_file,
        capability_id: authorized.stdout_json()["result"]["capability_id"]
            .as_str()
            .expect("capability id")
            .to_string(),
    }
}

fn dispatch(
    fixture: &Fixture,
    authorized: &Authorized,
    envs: &[(&str, &str)],
) -> nils_test_support::cmd::CmdOutput {
    fixture.run_with_env(
        &[
            "dispatch",
            "--product",
            "codex",
            "--capability-file",
            authorized
                .capability_file
                .to_str()
                .expect("capability path"),
            "--format",
            "json",
        ],
        Some(&authorized.payload),
        envs,
    )
}

#[test]
fn exact_one_shot_works_with_broken_config_and_rejects_replay() {
    let fixture = Fixture::new(POLICY);
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "cwd": fixture.root,
        "tool_input": {"command": "edit"}
    })
    .to_string();
    let target = sha256(fixture.root.as_os_str().as_encoded_bytes());
    let command = sha256(b"edit");
    let snapshot = sha256(payload.as_bytes());
    let challenge_file = fixture.root.join("challenge.json");
    let capability_file = fixture.root.join("capability.json");
    let challenge = fixture.run(
        &[
            "recovery",
            "challenge",
            "--product",
            "codex",
            "--event",
            "PreToolUse",
            "--target-digest",
            &target,
            "--command-digest",
            &command,
            "--snapshot-digest",
            &snapshot,
            "--rule",
            "runtime.locked-block",
            "--out",
            challenge_file.to_str().expect("challenge path"),
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(challenge.code, 0, "stderr={}", challenge.stderr_text());
    let challenge_digest = challenge.stdout_json()["result"]["challenge_digest"]
        .as_str()
        .expect("challenge digest")
        .to_string();
    let authorize = fixture.run(
        &[
            "recovery",
            "authorize",
            "--challenge-file",
            challenge_file.to_str().expect("challenge path"),
            "--expected-challenge-digest",
            &challenge_digest,
            "--out",
            capability_file.to_str().expect("capability path"),
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(authorize.code, 0, "stderr={}", authorize.stderr_text());

    fs::write(&fixture.config, "not valid toml = [").expect("break config");
    let allowed = fixture.run(
        &[
            "dispatch",
            "--product",
            "codex",
            "--capability-file",
            capability_file.to_str().expect("capability path"),
            "--format",
            "json",
        ],
        Some(&payload),
    );
    assert_eq!(allowed.code, 0, "stderr={}", allowed.stderr_text());
    assert_eq!(allowed.stdout_json()["result"]["recovery_applied"], true);

    let replay = fixture.run(
        &[
            "dispatch",
            "--product",
            "codex",
            "--capability-file",
            capability_file.to_str().expect("capability path"),
            "--format",
            "json",
        ],
        Some(&payload),
    );
    assert_eq!(replay.code, 65);
    assert_eq!(
        replay.stdout_json()["error"]["code"],
        "capability-replay-or-revoked"
    );
}

#[test]
fn one_shot_rejects_binding_drift_revocation_expiry_key_rotation_and_unsafe_mode() {
    let fixture = Fixture::new(POLICY);

    let drift = authorize(&fixture, "drift", "one-shot", "300", &[]);
    let mismatch = fixture.run(
        &[
            "recovery",
            "consume",
            "--capability-file",
            drift.capability_file.to_str().expect("capability path"),
            "--product",
            "codex",
            "--event",
            "PreToolUse",
            "--target-digest",
            &sha256(b"different-target"),
            "--command-digest",
            &drift.command,
            "--snapshot-digest",
            &drift.snapshot,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(mismatch.code, 65);
    assert_eq!(
        mismatch.stdout_json()["error"]["code"],
        "capability-binding-mismatch"
    );
    assert_eq!(
        dispatch(&fixture, &drift, &[]).code,
        0,
        "mismatch must not consume"
    );

    let revoked = authorize(&fixture, "revoked", "one-shot", "300", &[]);
    let revoke = fixture.run(
        &[
            "recovery",
            "revoke",
            "--capability-id",
            &revoked.capability_id,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(revoke.code, 0);
    let rejected = dispatch(&fixture, &revoked, &[]);
    assert_eq!(rejected.code, 65);
    assert_eq!(
        rejected.stdout_json()["error"]["code"],
        "capability-replay-or-revoked"
    );

    let expired = authorize(&fixture, "expired", "one-shot", "1", &[]);
    thread::sleep(Duration::from_millis(1_100));
    let rejected = dispatch(&fixture, &expired, &[]);
    assert_eq!(rejected.code, 65);
    assert_eq!(
        rejected.stdout_json()["error"]["code"],
        "capability-expired"
    );

    let rotated = authorize(&fixture, "rotated", "one-shot", "300", &[]);
    let key = fixture
        .state_home
        .join("agent-hook/recovery/authorization.key");
    fs::write(&key, b"rotated-key").expect("rotated key");
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).expect("private key");
    let rejected = dispatch(&fixture, &rotated, &[]);
    assert_eq!(rejected.code, 65);
    assert_eq!(
        rejected.stdout_json()["error"]["code"],
        "capability-key-rotated"
    );

    let unsafe_file = authorize(&fixture, "unsafe", "one-shot", "300", &[]);
    fs::set_permissions(
        &unsafe_file.capability_file,
        fs::Permissions::from_mode(0o644),
    )
    .expect("unsafe mode");
    let rejected = dispatch(&fixture, &unsafe_file, &[]);
    assert_eq!(rejected.code, 65);
    assert_eq!(
        rejected.stdout_json()["error"]["code"],
        "recovery-file-untrusted"
    );
}

#[test]
fn repair_window_is_session_bound_reusable_and_works_without_policy() {
    let fixture = Fixture::new(POLICY);
    let window = authorize(
        &fixture,
        "window",
        "repair-window",
        "300",
        &[("AGENT_SESSION_ID", "session-a")],
    );

    let wrong_session = dispatch(&fixture, &window, &[("AGENT_SESSION_ID", "session-b")]);
    assert_eq!(wrong_session.code, 65);
    assert_eq!(
        wrong_session.stdout_json()["error"]["code"],
        "capability-principal-mismatch"
    );

    fs::remove_file(&fixture.policy).expect("remove policy");
    for _ in 0..2 {
        let allowed = dispatch(&fixture, &window, &[("AGENT_SESSION_ID", "session-a")]);
        assert_eq!(allowed.code, 0, "stderr={}", allowed.stderr_text());
        assert_eq!(allowed.stdout_json()["result"]["recovery_applied"], true);
    }
}

#[test]
fn concurrent_one_shot_contenders_have_exactly_one_winner() {
    let fixture = Fixture::new(POLICY);
    let capability = authorize(&fixture, "concurrent", "one-shot", "300", &[]);
    let outputs = thread::scope(|scope| {
        let first = scope.spawn(|| dispatch(&fixture, &capability, &[]));
        let second = scope.spawn(|| dispatch(&fixture, &capability, &[]));
        [first.join().expect("first"), second.join().expect("second")]
    });
    let success = outputs.iter().filter(|output| output.code == 0).count();
    let replay = outputs
        .iter()
        .filter(|output| {
            output.code == 65
                && output.stdout_json()["error"]["code"] == "capability-replay-or-revoked"
        })
        .count();
    assert_eq!(success, 1);
    assert_eq!(replay, 1);
}

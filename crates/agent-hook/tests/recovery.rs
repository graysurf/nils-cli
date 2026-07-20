mod support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::thread;
use std::time::Duration;

use pretty_assertions::assert_eq;
use serde_json::json;

use support::{Fixture, sha256, target_binding_digest};

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

const TWO_BLOCK_POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
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
capability = { id = "decision.block.v1", reason_code = "first", message = "first" }

[[rules]]
id = "runtime.second-block"
products = ["codex"]
events = ["PreToolUse"]
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "second", message = "second" }
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
    authorize_for_target(fixture, name, scope, ttl_seconds, &fixture.root, envs)
}

fn authorize_for_target(
    fixture: &Fixture,
    name: &str,
    scope: &str,
    ttl_seconds: &str,
    target_path: &std::path::Path,
    envs: &[(&str, &str)],
) -> Authorized {
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "cwd": target_path,
        "tool_input": {"path": target_path, "command": "edit"}
    })
    .to_string();
    let target = target_binding_digest(target_path);
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
    assert_eq!(
        challenge.code,
        0,
        "stdout={} stderr={}",
        challenge.stdout_text(),
        challenge.stderr_text()
    );
    let challenge_digest = challenge.stdout_json()["data"]["challenge_digest"]
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
        capability_id: authorized.stdout_json()["data"]["capability_id"]
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
        "tool_input": {"path": fixture.root, "command": "edit"}
    })
    .to_string();
    let target = target_binding_digest(&fixture.root);
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
    assert_eq!(
        challenge.code,
        0,
        "stdout={} stderr={}",
        challenge.stdout_text(),
        challenge.stderr_text()
    );
    let challenge_digest = challenge.stdout_json()["data"]["challenge_digest"]
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
    assert_eq!(allowed.stdout_json()["data"]["recovery_applied"], true);

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
fn emergency_recovery_preserves_ungranted_rules_and_rejects_unknown_ids() {
    let fixture = Fixture::new(TWO_BLOCK_POLICY);
    let authorized = authorize(&fixture, "scoped", "one-shot", "300", &[]);
    fs::write(&fixture.config, "not valid toml = [").expect("break config");

    let blocked = dispatch(&fixture, &authorized, &[]);

    assert_eq!(blocked.code, 1, "stderr={}", blocked.stderr_text());
    assert_eq!(blocked.stdout_json()["data"]["action"], "block");
    assert_eq!(
        blocked.stdout_json()["data"]["reasons"][1]["rule_id"],
        "runtime.second-block"
    );
    assert_eq!(
        blocked.stdout_json()["data"]["reasons"][1]["code"],
        "recovery-manifest-block"
    );

    let clean = Fixture::new(POLICY);
    let digest = sha256(b"binding");
    let challenge = clean.run(
        &[
            "recovery",
            "challenge",
            "--product",
            "codex",
            "--event",
            "PreToolUse",
            "--target-digest",
            &digest,
            "--command-digest",
            &digest,
            "--snapshot-digest",
            &digest,
            "--rule",
            "runtime.unknown",
            "--out",
            clean
                .root
                .join("unknown.json")
                .to_str()
                .expect("challenge path"),
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(challenge.code, 65, "stderr={}", challenge.stderr_text());
    assert_eq!(
        challenge.stdout_json()["error"]["code"],
        "recovery-rule-unknown"
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
fn one_shot_rejects_recreated_target_at_the_same_absolute_path_without_consuming() {
    let fixture = Fixture::new(POLICY);
    let target = fixture.root.join("recreated-target");
    fs::create_dir_all(&target).expect("target");
    let original_target = fs::File::open(&target).expect("open original target");
    let original_inode = original_target.metadata().expect("original metadata").ino();
    let capability = authorize_for_target(&fixture, "recreated", "one-shot", "300", &target, &[]);

    fs::remove_dir(&target).expect("remove target");
    fs::create_dir(&target).expect("recreate target");
    assert_ne!(
        fs::metadata(&target).expect("replacement metadata").ino(),
        original_inode,
        "the test must retain the removed inode while creating its replacement"
    );
    let rejected = dispatch(&fixture, &capability, &[]);
    assert_eq!(rejected.code, 65, "stderr={}", rejected.stderr_text());
    assert_eq!(
        rejected.stdout_json()["error"]["code"],
        "capability-binding-mismatch"
    );

    let status = fixture.run(
        &[
            "recovery",
            "status",
            "--capability-id",
            &capability.capability_id,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(status.code, 0);
    assert_eq!(status.stdout_json()["data"]["status"], "authorized");
}

#[test]
fn recovery_binding_tracks_the_effective_symlink_target() {
    let fixture = Fixture::new(POLICY);
    let first_root = fixture.root.join("effective-first");
    let second_root = fixture.root.join("effective-second");
    fs::create_dir(&first_root).expect("first root");
    fs::create_dir(&second_root).expect("second root");
    let first_target = first_root.join("target.txt");
    let second_target = second_root.join("target.txt");
    fs::write(&first_target, "first\n").expect("first target");
    fs::write(&second_target, "second\n").expect("second target");
    let target_link = fixture.root.join("target-link");
    symlink(&first_target, &target_link).expect("first target symlink");

    let capability = authorize_for_target(
        &fixture,
        "symlink-target",
        "repair-window",
        "300",
        &target_link,
        &[("AGENT_SESSION_ID", "session-a")],
    );
    let allowed = dispatch(&fixture, &capability, &[("AGENT_SESSION_ID", "session-a")]);
    assert_eq!(allowed.code, 0, "stderr={}", allowed.stderr_text());
    assert_eq!(allowed.stdout_json()["data"]["recovery_applied"], true);

    fs::remove_file(&target_link).expect("remove first symlink");
    symlink(&second_target, &target_link).expect("second target symlink");
    let rejected = dispatch(&fixture, &capability, &[("AGENT_SESSION_ID", "session-a")]);
    assert_eq!(rejected.code, 65, "stderr={}", rejected.stderr_text());
    assert_eq!(
        rejected.stdout_json()["error"]["code"],
        "capability-binding-mismatch"
    );

    let status = fixture.run(
        &[
            "recovery",
            "status",
            "--capability-id",
            &capability.capability_id,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(status.code, 0);
    assert_eq!(status.stdout_json()["data"]["status"], "authorized");
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
        assert_eq!(allowed.stdout_json()["data"]["recovery_applied"], true);
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

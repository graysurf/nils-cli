mod support;

use std::fs;

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

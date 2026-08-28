mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use pretty_assertions::assert_eq;
use serde_json::json;

use support::{Fixture, now_epoch};

fn policy(first_action: &str) -> String {
    format!(
        r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.2"

[[rules]]
id = "runtime.pre-tool-policy"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Write"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = {{ id = "decision.{first_action}.v1", reason_code = "policy-{first_action}"{message} }}

[[rules]]
id = "runtime.pre-tool-coordination"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Write"
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = {{ id = "agent-session.coordination.v1", reason_code = "coordination-admitted" }}
"#,
        message = if first_action == "block" {
            ", message = \"blocked before coordination\""
        } else {
            ""
        }
    )
}

fn install_coordination_handler(fixture: &Fixture) {
    install_coordination_handler_with(
        fixture,
        "#!/bin/sh\nset -eu\ndd of=\"$COORDINATION_CAPTURE\" status=none\n",
    );
}

fn install_coordination_handler_with(fixture: &Fixture, source: &str) {
    let hooks = fixture.home.join(".codex/hooks");
    fs::create_dir_all(&hooks).expect("hook directory");
    let handler = hooks.join("session-coordination-guard.py");
    fs::write(&handler, source).expect("coordination handler");
    fs::set_permissions(&handler, fs::Permissions::from_mode(0o700)).expect("handler mode");
}

fn install_runtime_handler_with(fixture: &Fixture, name: &str, source: &str) {
    let hooks = fixture.home.join(".codex/hooks");
    fs::create_dir_all(&hooks).expect("hook directory");
    let handler = hooks.join(name);
    fs::write(&handler, source).expect("runtime handler");
    fs::set_permissions(&handler, fs::Permissions::from_mode(0o700)).expect("handler mode");
}

fn install_session_mode(fixture: &Fixture, mode: &str) {
    let session = fixture.session_state.join("sessions/trusted");
    fs::create_dir_all(&session).expect("session directory");
    let record = session.join("session.json");
    fs::write(
        &record,
        serde_json::to_vec(&json!({
            "schema_version": "agent-session.session.v1",
            "id": "trusted",
            "coordination_mode": mode,
            "runtime": {"launch_id": "trusted-incarnation"}
        }))
        .expect("session JSON"),
    )
    .expect("session record");
    Fixture::set_private(&record);
}

fn install_current_broker(fixture: &Fixture, mode: &str) {
    install_session_mode(fixture, mode);
    let now = now_epoch();
    let coordination = fixture.session_state.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination directory");
    let registry = coordination.join("registry.json");
    fs::write(
        &registry,
        serde_json::to_vec(&json!({
            "schema_version": "agent-session.coordination-registry.v1",
            "fingerprint_epoch": 1,
            "fingerprint_key": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "brokers": {
                "trusted": {
                    "session_id": "trusted",
                    "incarnation": "trusted-incarnation",
                    "coordination_mode": mode,
                    "state": "ready",
                    "heartbeat_epoch": now
                }
            },
            "claims": []
        }))
        .expect("registry JSON"),
    )
    .expect("registry");
    Fixture::set_private(&registry);
    let heartbeat = fixture
        .session_state
        .join("sessions/trusted/coordination/heartbeat");
    fs::create_dir_all(heartbeat.parent().expect("heartbeat parent")).expect("heartbeat directory");
    fs::write(&heartbeat, format!("trusted-incarnation:{now}\n")).expect("heartbeat");
    Fixture::set_private(&heartbeat);
}

fn bootstrap_policy(extra_rule: &str) -> String {
    format!(
        r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.31.1"

[[rules]]
id = "runtime.owner"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Bash"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = {{ id = "agent-session.owner-liveness.v1", reason_code = "owner", legacy_ttl_seconds = 300 }} # stale-audit: keep-contract

{extra_rule}

[[rules]]
id = "runtime.coordination"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Bash"
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = {{ id = "agent-session.coordination.v1", reason_code = "coordination-admitted" }}
"#
    )
}

fn activity_recovery_policy(include_coordination: bool) -> String {
    format!(
        r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.08.22.1"

[[rules]]
id = "runtime.activity"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Bash"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = {{ id = "agent-session.activity.v1", reason_code = "activity-recorded" }}

{coordination}
"#,
        coordination = if include_coordination {
            r#"[[rules]]
id = "runtime.coordination"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Bash"
priority = 900
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.coordination.v1", reason_code = "coordination-admitted" }"#
        } else {
            ""
        }
    )
}

#[test]
fn exact_recovery_reaches_authenticated_coordination_when_activity_is_stale() {
    let fixture = Fixture::new(&activity_recovery_policy(true));
    let activity = fixture.root.join("agent-session-stale-activity");
    fs::write(&activity, "#!/bin/sh\nexit 65\n").expect("stale activity helper");
    fs::set_permissions(&activity, fs::Permissions::from_mode(0o700))
        .expect("activity helper mode");
    install_coordination_handler(&fixture);
    let capture = fixture.root.join("recovery-coordination.json");
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "recover-after-rehydrate",
        "cwd": fixture.root,
        "tool_input": {
            "command": "main-agent self recover --idempotency-key recover-12345678 --format json"
        }
    })
    .to_string();
    let recovered = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[
            (
                "AGENT_SESSION_BIN",
                activity.to_str().expect("activity path"),
            ),
            ("AGENT_SESSION_ID", "trusted"),
            ("AGENT_SESSION_RUNTIME_ID", "stale-incarnation"),
            ("AGENT_SESSION_COORDINATION_MODE", "enforce"),
            (
                "COORDINATION_CAPTURE",
                capture.to_str().expect("capture path"),
            ),
        ],
    );
    assert_eq!(
        recovered.code,
        0,
        "exact recovery must reach the later authenticated coordination boundary: stdout={} stderr={}",
        recovered.stdout_text(),
        recovered.stderr_text()
    );
    assert_eq!(recovered.stdout_json()["data"]["action"], "allow");
    assert_eq!(
        recovered.stdout_json()["data"]["reasons"][0]["code"],
        "activity-recovery-deferred-to-coordination"
    );
    assert_eq!(
        recovered.stdout_json()["data"]["reasons"][1]["code"],
        "coordination-admitted"
    );
    assert_eq!(
        fs::read_to_string(capture).expect("captured recovery request"),
        payload
    );
}

#[test]
fn exact_recovery_reaches_coordination_when_the_activity_helper_is_unresolvable() {
    let fixture = Fixture::new(&activity_recovery_policy(true));
    install_coordination_handler(&fixture);
    let capture = fixture.root.join("missing-helper-recovery.json");
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "recover-without-activity-helper",
        "cwd": fixture.root,
        "tool_input": {
            "command": "main-agent self recover --idempotency-key recover-12345678 --format json"
        }
    })
    .to_string();
    let recovered = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[
            ("AGENT_SESSION_BIN", "/missing/agent-session"),
            ("AGENT_SESSION_ID", "trusted"),
            ("AGENT_SESSION_RUNTIME_ID", "stale-incarnation"),
            ("AGENT_SESSION_COORDINATION_MODE", "enforce"),
            ("PATH", "/usr/bin:/bin"),
            (
                "COORDINATION_CAPTURE",
                capture.to_str().expect("capture path"),
            ),
        ],
    );
    assert_eq!(
        recovered.code,
        0,
        "missing activity helper must defer exact recovery: stdout={} stderr={}",
        recovered.stdout_text(),
        recovered.stderr_text()
    );
    assert_eq!(recovered.stdout_json()["data"]["action"], "allow");
    assert_eq!(
        recovered.stdout_json()["data"]["reasons"][0]["code"],
        "activity-recovery-deferred-to-coordination"
    );
    assert_eq!(
        recovered.stdout_json()["data"]["reasons"][1]["code"],
        "coordination-admitted"
    );
    assert_eq!(
        fs::read_to_string(capture).expect("captured recovery request"),
        payload
    );
}

#[test]
fn activity_recovery_degradation_requires_exact_shape_and_coordination() {
    let fixture = Fixture::new(&activity_recovery_policy(true));
    let activity = fixture.root.join("agent-session-stale-activity");
    fs::write(&activity, "#!/bin/sh\nexit 65\n").expect("stale activity helper");
    fs::set_permissions(&activity, fs::Permissions::from_mode(0o700))
        .expect("activity helper mode");
    install_coordination_handler(&fixture);
    let capture = fixture.root.join("read-only-coordination.json");
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "read-only-after-activity-fault",
        "cwd": fixture.root,
        "tool_input": {"command": "pwd"}
    })
    .to_string();
    let admitted = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[
            (
                "AGENT_SESSION_BIN",
                activity.to_str().expect("activity path"),
            ),
            ("AGENT_SESSION_ID", "trusted"),
            ("AGENT_SESSION_RUNTIME_ID", "stale-incarnation"),
            ("AGENT_SESSION_COORDINATION_MODE", "enforce"),
            (
                "COORDINATION_CAPTURE",
                capture.to_str().expect("capture path"),
            ),
        ],
    );
    assert_eq!(admitted.code, 0, "stdout={}", admitted.stdout_text());
    assert_eq!(admitted.stdout_json()["data"]["action"], "warn");
    assert_eq!(
        admitted.stdout_json()["data"]["reasons"][0]["code"],
        "session-activity-failed"
    );
    assert_eq!(
        fs::read_to_string(&capture).expect("captured read-only request"),
        payload
    );

    let commands = [
        "main-agent self recover --idempotency-key short --format json",
        "main-agent self recover --idempotency-key recover-12345678 --format json; pwd",
    ];
    for command in commands {
        let fixture = Fixture::new(&activity_recovery_policy(true));
        let activity = fixture.root.join("agent-session-stale-activity");
        fs::write(&activity, "#!/bin/sh\nexit 65\n").expect("stale activity helper");
        fs::set_permissions(&activity, fs::Permissions::from_mode(0o700))
            .expect("activity helper mode");
        install_coordination_handler(&fixture);
        let capture = fixture.root.join("rejected-coordination.json");
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_use_id": "recovery-near-miss",
            "cwd": fixture.root,
            "tool_input": {"command": command}
        })
        .to_string();
        let rejected = fixture.run_with_env(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
            &[
                (
                    "AGENT_SESSION_BIN",
                    activity.to_str().expect("activity path"),
                ),
                ("AGENT_SESSION_ID", "trusted"),
                ("AGENT_SESSION_RUNTIME_ID", "stale-incarnation"),
                ("AGENT_SESSION_COORDINATION_MODE", "enforce"),
                (
                    "COORDINATION_CAPTURE",
                    capture.to_str().expect("capture path"),
                ),
            ],
        );
        assert_eq!(rejected.code, 1, "command={command}");
        assert_eq!(rejected.stdout_json()["data"]["action"], "block");
        assert_eq!(
            rejected.stdout_json()["data"]["reasons"][0]["code"],
            "runtime.activity:capability-failure-closed"
        );
        assert!(
            !capture.exists(),
            "near miss reached coordination: {command}"
        );
    }

    let fixture = Fixture::new(&activity_recovery_policy(false));
    let activity = fixture.root.join("agent-session-stale-activity");
    fs::write(&activity, "#!/bin/sh\nexit 65\n").expect("stale activity helper");
    fs::set_permissions(&activity, fs::Permissions::from_mode(0o700))
        .expect("activity helper mode");
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "recovery-without-transaction",
        "cwd": fixture.root,
        "tool_input": {
            "command": "main-agent self recover --idempotency-key recover-12345678 --format json"
        }
    })
    .to_string();
    let rejected = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[
            (
                "AGENT_SESSION_BIN",
                activity.to_str().expect("activity path"),
            ),
            ("AGENT_SESSION_ID", "trusted"),
            ("AGENT_SESSION_RUNTIME_ID", "stale-incarnation"),
        ],
    );
    assert_eq!(rejected.code, 1);
    assert_eq!(
        rejected.stdout_json()["data"]["reasons"][0]["code"],
        "runtime.activity:capability-failure-closed"
    );

    let fixture = Fixture::new(&activity_recovery_policy(true));
    let activity = fixture.root.join("agent-session-stale-activity");
    fs::write(&activity, "#!/bin/sh\nexit 65\n").expect("stale activity helper");
    fs::set_permissions(&activity, fs::Permissions::from_mode(0o700))
        .expect("activity helper mode");
    install_coordination_handler_with(&fixture, "#!/bin/sh\nexit 70\n");
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "recovery-with-failed-transaction",
        "cwd": fixture.root,
        "tool_input": {
            "command": "main-agent self recover --idempotency-key recover-12345678 --format json"
        }
    })
    .to_string();
    let rejected = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[
            (
                "AGENT_SESSION_BIN",
                activity.to_str().expect("activity path"),
            ),
            ("AGENT_SESSION_ID", "trusted"),
            ("AGENT_SESSION_RUNTIME_ID", "stale-incarnation"),
            ("AGENT_SESSION_COORDINATION_MODE", "enforce"),
        ],
    );
    assert_eq!(rejected.code, 1);
    assert_eq!(rejected.stdout_json()["data"]["action"], "block");
    assert_eq!(
        rejected.stdout_json()["data"]["reasons"][1]["code"],
        "runtime.coordination:capability-failure-closed"
    );
}

#[test]
fn activity_metadata_failure_obeys_the_trusted_coordination_mode() {
    for (mode, expected_code, expected_action) in [
        ("advisory", 0, "warn"),
        ("off", 0, "warn"),
        ("enforce", 1, "block"),
    ] {
        let fixture = Fixture::new(&activity_recovery_policy(true));
        install_current_broker(&fixture, mode);
        install_coordination_handler(&fixture);
        let activity = fixture.root.join("agent-session-stale-activity");
        fs::write(&activity, "#!/bin/sh\nexit 65\n").expect("stale activity helper");
        fs::set_permissions(&activity, fs::Permissions::from_mode(0o700))
            .expect("activity helper mode");
        let capture = fixture.root.join("coordination.json");
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_use_id": format!("activity-fault-{mode}"),
            "cwd": fixture.root,
            "tool_input": {"command": "touch changed"}
        })
        .to_string();
        let decision = fixture.run_with_env(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
            &[
                (
                    "AGENT_SESSION_BIN",
                    activity.to_str().expect("activity path"),
                ),
                ("AGENT_SESSION_ID", "trusted"),
                ("AGENT_SESSION_RUNTIME_ID", "trusted-incarnation"),
                ("AGENT_SESSION_COORDINATION_MODE", mode),
                (
                    "COORDINATION_CAPTURE",
                    capture.to_str().expect("capture path"),
                ),
            ],
        );
        assert_eq!(
            decision.code,
            expected_code,
            "mode={mode}: {}",
            decision.stdout_text()
        );
        assert_eq!(
            decision.stdout_json()["data"]["action"],
            expected_action,
            "mode={mode}: {}",
            decision.stdout_text()
        );
        if mode == "enforce" {
            assert!(!capture.exists(), "enforce fault reached coordination");
        } else {
            assert_eq!(
                decision.stdout_json()["data"]["reasons"],
                json!([
                    {
                        "rule_id": "runtime.activity",
                        "code": "session-activity-failed",
                        "disposition": "warn"
                    },
                    {
                        "rule_id": "runtime.activity",
                        "code": "activity-degraded-advisory-off",
                        "disposition": "warn"
                    },
                    {
                        "rule_id": "runtime.coordination",
                        "code": "coordination-admitted",
                        "disposition": "allow"
                    }
                ]),
                "mode={mode}"
            );
            assert!(capture.exists(), "mode={mode} skipped later authority");
        }
    }
}

fn install_foreign_owner(fixture: &Fixture) {
    install_session_mode(fixture, "enforce");
    let now = now_epoch();
    let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let fingerprint =
        nils_common::coordination_projection::worktree_fingerprint(1, key, &fixture.root)
            .expect("worktree fingerprint");
    let coordination = fixture.session_state.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination directory");
    let registry = coordination.join("registry.json");
    fs::write(
        &registry,
        serde_json::to_vec(&json!({
            "schema_version": "agent-session.coordination-registry.v1",
            "fingerprint_epoch": 1,
            "fingerprint_key": key,
            "brokers": {
                "trusted": {
                    "session_id": "trusted",
                    "incarnation": "trusted-incarnation",
                    "state": "ready",
                    "heartbeat_epoch": now
                },
                "peer": {
                    "session_id": "peer",
                    "incarnation": "peer-incarnation",
                    "state": "ready",
                    "heartbeat_epoch": now
                }
            },
            "claims": [{
                "schema_version": "agent-session.work-context.v1",
                "session_id": "peer",
                "session_incarnation": "peer-incarnation",
                "state": "active",
                "worktrees": [fingerprint],
                "repositories": ["owner/repo"],
                "provider_refs": [],
                "scopes": [],
                "expires_at_epoch": now + 300
            }]
        }))
        .expect("registry JSON"),
    )
    .expect("registry");
    Fixture::set_private(&registry);
    for (session, incarnation) in [
        ("trusted", "trusted-incarnation"),
        ("peer", "peer-incarnation"),
    ] {
        let heartbeat = fixture
            .session_state
            .join("sessions")
            .join(session)
            .join("coordination/heartbeat");
        fs::create_dir_all(heartbeat.parent().expect("heartbeat parent"))
            .expect("heartbeat directory");
        fs::write(&heartbeat, format!("{incarnation}:{now}\n")).expect("heartbeat");
        Fixture::set_private(&heartbeat);
    }
}

#[test]
fn exact_preclaim_bootstrap_defers_foreign_owner_to_typed_coordination() {
    let fixture = Fixture::new(&bootstrap_policy(""));
    install_coordination_handler(&fixture);
    install_foreign_owner(&fixture);

    let exact_payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_use_id": "bootstrap-exact",
        "cwd": fixture.root,
        "tool_input": {
            "command": "/trusted/bin/main-agent bootstrap --idempotency-key bootstrap-12345678 --format json"
        }
    })
    .to_string();
    let capture = fixture.root.join("bootstrap-coordination.json");
    let generic = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&exact_payload),
        &[
            ("AGENT_SESSION_ID", "trusted"),
            ("AGENT_SESSION_RUNTIME_ID", "trusted-incarnation"),
            ("AGENT_SESSION_COORDINATION_MODE", "enforce"),
            (
                "COORDINATION_CAPTURE",
                capture.to_str().expect("capture path"),
            ),
        ],
    );
    assert_eq!(generic.code, 1, "stderr={}", generic.stderr_text());
    assert_eq!(generic.stdout_json()["data"]["action"], "block");
    assert_eq!(
        generic.stdout_json()["data"]["reasons"][0]["code"],
        "owner-active-foreign"
    );
    assert_eq!(
        fs::read_to_string(&capture).expect("captured generic allow"),
        exact_payload
    );

    install_coordination_handler_with(
        &fixture,
        "#!/bin/sh\nset -eu\ndd of=\"$COORDINATION_CAPTURE\" status=none\nprintf '%s\\n' '{\"schema_version\":\"runtime-kit.session-coordination-bootstrap-authorization.v1\",\"authorization\":\"typed-main-agent-bootstrap-authorized\"}'\n",
    );
    let exact = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&exact_payload),
        &[
            ("AGENT_SESSION_ID", "trusted"),
            ("AGENT_SESSION_RUNTIME_ID", "trusted-incarnation"),
            ("AGENT_SESSION_COORDINATION_MODE", "enforce"),
            (
                "COORDINATION_CAPTURE",
                capture.to_str().expect("capture path"),
            ),
        ],
    );
    assert_eq!(exact.code, 0, "stderr={}", exact.stderr_text());
    assert_eq!(exact.stdout_json()["data"]["action"], "allow");
    assert_eq!(
        exact.stdout_json()["data"]["reasons"][0]["code"],
        "owner-liveness-superseded-by-typed-bootstrap"
    );
    assert_eq!(
        exact.stdout_json()["data"]["reasons"][1]["code"],
        "typed-main-agent-bootstrap-authorized"
    );
    assert_eq!(
        fs::read_to_string(&capture).expect("captured exact bootstrap"),
        exact_payload
    );

    fs::remove_file(&capture).expect("remove exact capture");
    let composed_payload =
        exact_payload.replace("--format json\"", "--format json; touch forbidden\"");
    let composed = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&composed_payload),
        &[
            ("AGENT_SESSION_ID", "trusted"),
            ("AGENT_SESSION_RUNTIME_ID", "trusted-incarnation"),
            ("AGENT_SESSION_COORDINATION_MODE", "enforce"),
            (
                "COORDINATION_CAPTURE",
                capture.to_str().expect("capture path"),
            ),
        ],
    );
    assert_eq!(composed.code, 1, "stderr={}", composed.stderr_text());
    assert_eq!(
        composed.stdout_json()["data"]["reasons"][0]["code"],
        "owner-active-foreign"
    );
    assert!(
        !capture.exists(),
        "composed shell input must not reach typed coordination"
    );

    fs::remove_file(
        fixture
            .home
            .join(".codex/hooks/session-coordination-guard.py"),
    )
    .expect("remove coordination handler");
    let unavailable = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&exact_payload),
        &[
            ("AGENT_SESSION_ID", "trusted"),
            ("AGENT_SESSION_RUNTIME_ID", "trusted-incarnation"),
            ("AGENT_SESSION_COORDINATION_MODE", "enforce"),
        ],
    );
    assert_eq!(unavailable.code, 1, "stderr={}", unavailable.stderr_text());
    assert_eq!(unavailable.stdout_json()["data"]["action"], "block");
    assert_eq!(
        unavailable.stdout_json()["data"]["reasons"][0]["code"],
        "owner-active-foreign"
    );
}

#[test]
fn typed_bootstrap_does_not_supersede_other_blocks_or_transforms() {
    let cases = [
        (
            "additional block",
            r#"[[rules]]
id = "runtime.additional-block"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Bash"
priority = 15
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "additional-block", message = "still blocked" }"#,
            "additional-block",
            "block",
        ),
        (
            "transform",
            r#"[[rules]]
id = "runtime.transform"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Bash"
priority = 15
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.transform.v1", reason_code = "bootstrap-transform", replacement = { command = "safe" } }"#,
            "bootstrap-transform",
            "transform",
        ),
    ];
    for (name, extra_rule, extra_code, extra_disposition) in cases {
        let fixture = Fixture::new(&bootstrap_policy(extra_rule));
        install_foreign_owner(&fixture);
        install_coordination_handler_with(
            &fixture,
            "#!/bin/sh\nset -eu\ndd of=\"$COORDINATION_CAPTURE\" status=none\nprintf '%s\\n' '{\"schema_version\":\"runtime-kit.session-coordination-bootstrap-authorization.v1\",\"authorization\":\"typed-main-agent-bootstrap-authorized\"}'\n",
        );
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_use_id": format!("bootstrap-{name}"),
            "cwd": fixture.root,
            "tool_input": {
                "command": "/trusted/bin/main-agent bootstrap --idempotency-key bootstrap-12345678 --format json"
            }
        })
        .to_string();
        let capture = fixture.root.join("bootstrap-coordination.json");
        let output = fixture.run_with_env(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
            &[
                ("AGENT_SESSION_ID", "trusted"),
                ("AGENT_SESSION_RUNTIME_ID", "trusted-incarnation"),
                ("AGENT_SESSION_COORDINATION_MODE", "enforce"),
                (
                    "COORDINATION_CAPTURE",
                    capture.to_str().expect("capture path"),
                ),
            ],
        );
        assert_eq!(
            output.code,
            1,
            "case={name} stderr={}",
            output.stderr_text()
        );
        let decision = output.stdout_json();
        assert_eq!(decision["data"]["action"], "block");
        let reasons = decision["data"]["reasons"].as_array().expect("reasons");
        assert!(
            reasons.iter().any(|reason| {
                reason["code"] == "owner-active-foreign" && reason["disposition"] == "block"
            }),
            "case={name} owner block must remain authoritative"
        );
        assert!(
            reasons.iter().any(|reason| {
                reason["code"] == extra_code && reason["disposition"] == extra_disposition
            }),
            "case={name} additional decision must remain intact"
        );
        assert!(
            !capture.exists(),
            "case={name} must not reach typed coordination"
        );
    }
}

#[test]
fn terminal_failure_runs_coordination_after_policy_to_close_the_operation() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.2"

[[rules]]
id = "runtime.post-tool-policy"
products = ["codex"]
events = ["PostToolUseFailure"]
matcher = "Write"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "post-policy-block", message = "post policy blocked" }

[[rules]]
id = "runtime.post-tool-coordination"
products = ["codex"]
events = ["PostToolUseFailure"]
matcher = "Write"
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.coordination.v1", reason_code = "coordination-completed" }
"#;
    let fixture = Fixture::new(policy);
    install_coordination_handler(&fixture);
    let capture = fixture.root.join("post-failure-coordination.json");
    let payload = json!({
        "hook_event_name": "PostToolUseFailure",
        "tool_name": "Write",
        "tool_use_id": "tool-call-1",
        "cwd": fixture.root,
        "tool_input": {"path": fixture.root.join("target.txt")},
        "tool_response": {"success": false}
    })
    .to_string();
    let output = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[(
            "COORDINATION_CAPTURE",
            capture.to_str().expect("capture path"),
        )],
    );
    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_json()["data"]["action"], "block");
    assert_eq!(
        output.stdout_json()["data"]["reasons"][1]["code"],
        "coordination-completed"
    );
    assert_eq!(
        fs::read_to_string(capture).expect("completion payload"),
        payload
    );
}

#[test]
fn coordination_shadow_is_side_effect_free_and_handler_block_is_authoritative() {
    let fixture = Fixture::new(&policy("allow"));
    install_coordination_handler(&fixture);
    let capture = fixture.root.join("shadow-coordination.json");
    let shadow = fixture.run_with_env(
        &[
            "dispatch",
            "--product",
            "codex",
            "--shadow",
            "--format",
            "json",
        ],
        Some(&pre_tool_payload(&fixture)),
        &[(
            "COORDINATION_CAPTURE",
            capture.to_str().expect("capture path"),
        )],
    );
    assert_eq!(shadow.code, 0, "stderr={}", shadow.stderr_text());
    assert_eq!(shadow.stdout_json()["data"]["action"], "allow");
    assert_eq!(
        shadow.stdout_json()["data"]["shadow"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert!(!capture.exists());

    install_coordination_handler_with(
        &fixture,
        "#!/bin/sh\nset -eu\nprintf '%s\\n' '{\"decision\":\"block\",\"reason\":\"claim-conflict\"}'\n",
    );
    let blocked = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&pre_tool_payload(&fixture)),
    );
    assert_eq!(blocked.code, 1, "stderr={}", blocked.stderr_text());
    assert_eq!(blocked.stdout_json()["data"]["action"], "block");
    assert_eq!(
        blocked.stdout_json()["data"]["reasons"][1]["code"],
        "coordination-admitted"
    );
}

#[test]
fn coordination_capability_has_no_configurable_command_or_advisory_mode() {
    let base = policy("allow");
    for (needle, replacement) in [
        ("mode = \"enforce\"", "mode = \"shadow\""),
        ("failure_posture = \"closed\"", "failure_posture = \"warn\""),
        ("override_class = \"locked\"", "override_class = \"free\""),
    ] {
        let index = base.rfind(needle).expect("coordination rule field");
        let mut invalid = base.clone();
        invalid.replace_range(index..index + needle.len(), replacement);
        let output = Fixture::new(&invalid).run(&["validate", "--format", "json"], None);
        assert_eq!(output.code, 65, "{needle} -> {replacement}");
        assert_eq!(
            output.stdout_json()["error"]["code"],
            "coordination-rule-not-locked"
        );
    }

    let arbitrary = base.replace(
        "{ id = \"agent-session.coordination.v1\", reason_code = \"coordination-admitted\" }",
        "{ id = \"agent-session.coordination.v1\", reason_code = \"coordination-admitted\", handler_id = \"arbitrary\" }",
    );
    let output = Fixture::new(&arbitrary).run(&["validate", "--format", "json"], None);
    assert_eq!(output.code, 65);
    assert_eq!(output.stdout_json()["error"]["code"], "policy-invalid");
}

#[test]
fn coordination_timeout_warns_only_for_trusted_advisory_mode() {
    for (mode, action, code) in [
        (
            "advisory",
            "warn",
            "runtime.pre-tool-coordination:capability-timeout-warn",
        ),
        (
            "enforce",
            "block",
            "runtime.pre-tool-coordination:capability-timeout-closed",
        ),
    ] {
        let fixture = Fixture::new(&policy("allow"));
        install_coordination_handler_with(&fixture, "#!/bin/sh\nsleep 30\n");
        install_session_mode(&fixture, mode);
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "trusted",
            "tool_use_id": "tool-call-timeout",
            "tool_name": "Write",
            "cwd": fixture.root,
            "tool_input": {"path": fixture.root.join("target.txt")}
        })
        .to_string();
        let output = fixture.run_with_env(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
            &[
                ("AGENT_SESSION_ID", "trusted"),
                ("AGENT_SESSION_RUNTIME_ID", "trusted-incarnation"),
                ("AGENT_SESSION_COORDINATION_MODE", mode),
            ],
        );
        assert_eq!(
            output.stdout_json()["data"]["action"],
            action,
            "mode={mode}"
        );
        assert_eq!(
            output.stdout_json()["data"]["reasons"][1]["code"],
            code,
            "mode={mode}"
        );
        if mode == "advisory" {
            assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
            assert!(
                output.stdout_json()["data"]["context"]
                    .as_str()
                    .is_some_and(|context| context.contains("incident:sha256:"))
            );
        } else {
            assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
        }
    }
}

fn pre_tool_payload(fixture: &Fixture) -> String {
    json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "tool_use_id": "tool-call-1",
        "cwd": fixture.root,
        "tool_input": {"path": fixture.root.join("target.txt")}
    })
    .to_string()
}

#[test]
fn coordination_never_admits_before_a_later_priority_handler_blocks() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.08.02.1"

[[rules]]
id = "runtime.pre-tool-coordination"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Write"
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.coordination.v1", reason_code = "coordination-admitted" }

[[rules]]
id = "runtime.later-project-dev-block"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Write"
priority = 30
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "runtime-kit.handler.v1", handler_id = "pre-edit-intent-gate" }
"#;
    let fixture = Fixture::new(policy);
    install_coordination_handler_with(
        &fixture,
        "#!/bin/sh\nset -eu\nprintf '%s\\n' 'admit:codex:trusted:tool-call-transaction:revision-7' >> \"$TRANSACTION_LOG\"\n",
    );
    install_runtime_handler_with(
        &fixture,
        "pre-edit-intent-gate.py",
        "#!/bin/sh\nset -eu\nprintf '%s\\n' 'block:codex:trusted:tool-call-transaction' >> \"$TRANSACTION_LOG\"\nprintf '%s\\n' '{\"decision\":\"block\",\"reason\":\"project-dev-prepared\"}'\n",
    );
    let transaction_log = fixture.root.join("transaction.log");
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "trusted",
        "tool_name": "Write",
        "tool_use_id": "tool-call-transaction",
        "cwd": fixture.root,
        "tool_input": {"path": fixture.root.join("target.txt")}
    })
    .to_string();

    let output = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[(
            "TRANSACTION_LOG",
            transaction_log.to_str().expect("transaction log path"),
        )],
    );

    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    assert_eq!(output.stdout_json()["data"]["action"], "block");
    assert_eq!(
        fs::read_to_string(transaction_log).expect("transaction call log"),
        "block:codex:trusted:tool-call-transaction\n",
        "coordination is an after-policy transaction and must not admit before any ordinary block"
    );
    assert_eq!(
        output.stdout_json()["data"]["reasons"],
        json!([{
            "rule_id": "runtime.later-project-dev-block",
            "code": "pre-edit-intent-gate",
            "disposition": "block"
        }]),
        "a skipped coordination transaction contributes no decision or rollback identity"
    );
}

#[test]
fn coordination_runs_inside_the_owned_ingress_only_after_policy_allows() {
    let blocked = Fixture::new(&policy("block"));
    install_coordination_handler(&blocked);
    let blocked_capture = blocked.root.join("blocked-coordination.json");
    let blocked_output = blocked.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&pre_tool_payload(&blocked)),
        &[(
            "COORDINATION_CAPTURE",
            blocked_capture.to_str().expect("capture path"),
        )],
    );
    assert_eq!(
        blocked_output.code,
        1,
        "stderr={}",
        blocked_output.stderr_text()
    );
    assert_eq!(blocked_output.stdout_json()["data"]["action"], "block");
    assert!(
        !blocked_capture.exists(),
        "a blocked aggregate must not admit a coordination operation"
    );

    let allowed = Fixture::new(&policy("allow"));
    install_coordination_handler(&allowed);
    let allowed_capture = allowed.root.join("allowed-coordination.json");
    let allowed_payload = pre_tool_payload(&allowed);
    let allowed_output = allowed.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&allowed_payload),
        &[(
            "COORDINATION_CAPTURE",
            allowed_capture.to_str().expect("capture path"),
        )],
    );
    assert_eq!(
        allowed_output.code,
        0,
        "stderr={}",
        allowed_output.stderr_text()
    );
    assert_eq!(allowed_output.stdout_json()["data"]["action"], "allow");
    assert_eq!(
        fs::read_to_string(&allowed_capture).expect("captured provider payload"),
        allowed_payload
    );
    assert_eq!(
        allowed_output.stdout_json()["data"]["reasons"][1]["code"],
        "coordination-admitted"
    );
}

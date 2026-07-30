mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use pretty_assertions::{assert_eq, assert_ne};

use support::Fixture;

const POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "session.activity"
products = ["codex", "claude"]
events = ["UserPromptSubmit", "PreToolUse"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.activity.v1", reason_code = "activity-recorded" }
"#;

#[test]
fn lifecycle_activity_uses_the_typed_cli_with_metadata_only_json() {
    let fixture = Fixture::new(POLICY);
    let fake = fixture.root.join("agent-session-fake");
    let args_path = fixture.root.join("activity.args");
    let input_path = fixture.root.join("activity.json");
    fs::write(
        &fake,
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > \"$CAPTURE_ARGS\"\ndd of=\"$CAPTURE_STDIN\" status=none\n",
    )
    .expect("fake agent-session");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).expect("fake mode");
    let envs = [
        ("AGENT_SESSION_BIN", fake.to_str().expect("fake path")),
        ("AGENT_SESSION_ID", "managed-session"),
        ("AGENT_SESSION_RUNTIME_ID", "managed-runtime"),
        ("CAPTURE_ARGS", args_path.to_str().expect("args path")),
        ("CAPTURE_STDIN", input_path.to_str().expect("stdin path")),
    ];

    let codex = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(
            r#"{"hook_event_name":"UserPromptSubmit","session_id":"provider-session-secret","turn_id":"provider-turn-secret","prompt":"prompt-secret","tool_input":{"command":"command-secret"}}"#,
        ),
        &envs,
    );
    assert_eq!(codex.code, 0, "stderr={}", codex.stderr_text());
    assert_eq!(
        fs::read_to_string(&args_path).expect("captured args"),
        "activity\nevent\n--stdin\nmanaged-session\n"
    );
    let codex_event = fs::read_to_string(&input_path).expect("captured event");
    let codex_json: serde_json::Value = serde_json::from_str(&codex_event).expect("event JSON");
    assert_eq!(codex_json["schema_version"], "agent-session.turn-event.v1");
    assert_eq!(codex_json["runtime_id"], "managed-runtime");
    assert_eq!(codex_json["provider"], "codex");
    assert_eq!(codex_json["kind"], "turn_started");
    assert_eq!(codex_json["confidence"], "observed");
    assert_eq!(codex_json["source_kind"], "provider_hook");
    assert!(
        codex_json["provider_session_id"]
            .as_str()
            .is_some_and(|value| value.starts_with("local:v1:"))
    );
    for secret in [
        "provider-session-secret",
        "provider-turn-secret",
        "prompt-secret",
        "command-secret",
        "prompt",
        "tool_input",
    ] {
        assert!(!codex_event.contains(secret), "leaked {secret}");
    }

    let claude = fixture.run_with_env(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(
            r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_use_id":"question-secret","session_id":"claude-session-secret","tool_input":{"questions":[{"question":"private-question"}]}}"#,
        ),
        &envs,
    );
    assert_eq!(claude.code, 0, "stderr={}", claude.stderr_text());
    let claude_event = fs::read_to_string(&input_path).expect("captured event");
    let claude_json: serde_json::Value = serde_json::from_str(&claude_event).expect("event JSON");
    assert_eq!(claude_json["provider"], "claude");
    assert_eq!(claude_json["kind"], "attention_requested");
    assert_eq!(claude_json["attention_kind"], "clarification");
    assert!(
        claude_json["attention_id"]
            .as_str()
            .is_some_and(|value| value.starts_with("local:v1:"))
    );
    for secret in [
        "question-secret",
        "claude-session-secret",
        "private-question",
        "questions",
        "tool_input",
    ] {
        assert!(!claude_event.contains(secret), "leaked {secret}");
    }
}

const STOP_POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "session.activity.stop"
products = ["claude"]
events = ["Stop"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.activity.v1", reason_code = "activity-recorded" }
"#;

const FAILURE_POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "session.activity"
products = ["codex", "claude"]
events = ["PreToolUse", "Stop"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.activity.v1", reason_code = "activity-recorded" }
"#;

const FAILURE_COORDINATION_POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "session.activity"
products = ["codex"]
events = ["Stop"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.activity.v1", reason_code = "activity-recorded" }

[[rules]]
id = "session.coordination"
products = ["codex"]
events = ["Stop"]
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.coordination.v1", reason_code = "coordination-admitted" }
"#;

#[test]
fn claude_prompt_id_is_correlated_as_the_provider_turn_id() {
    let fixture = Fixture::new(STOP_POLICY);
    let fake = fixture.root.join("agent-session-fake");
    let args_path = fixture.root.join("activity.args");
    let input_path = fixture.root.join("activity.json");
    fs::write(
        &fake,
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > \"$CAPTURE_ARGS\"\ndd of=\"$CAPTURE_STDIN\" status=none\n",
    )
    .expect("fake agent-session");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).expect("fake mode");
    let envs = [
        ("AGENT_SESSION_BIN", fake.to_str().expect("fake path")),
        ("AGENT_SESSION_ID", "managed-session"),
        ("AGENT_SESSION_RUNTIME_ID", "managed-runtime"),
        ("CAPTURE_ARGS", args_path.to_str().expect("args path")),
        ("CAPTURE_STDIN", input_path.to_str().expect("stdin path")),
    ];

    // Claude's real Stop payload carries `prompt_id`, never `turn_id`, so
    // correlation has to accept it or every Claude event stays uncorrelated.
    let stop = fixture.run_with_env(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(
            r#"{"hook_event_name":"Stop","session_id":"claude-session-secret","prompt_id":"claude-prompt-secret","stop_hook_active":false,"last_assistant_message":"message-secret"}"#,
        ),
        &envs,
    );
    assert_eq!(stop.code, 0, "stderr={}", stop.stderr_text());
    let stop_event = fs::read_to_string(&input_path).expect("captured event");
    let stop_json: serde_json::Value = serde_json::from_str(&stop_event).expect("event JSON");
    assert_eq!(stop_json["provider"], "claude");
    assert_eq!(stop_json["kind"], "stop_observed");
    assert!(
        stop_json["provider_turn_id"]
            .as_str()
            .is_some_and(|value| value.starts_with("local:v1:")),
        "prompt_id was not correlated: {stop_event}"
    );
    for secret in [
        "claude-prompt-secret",
        "claude-session-secret",
        "message-secret",
        "last_assistant_message",
        "prompt_id",
    ] {
        assert!(!stop_event.contains(secret), "leaked {secret}");
    }

    // An explicit `turn_id` still wins, so a provider that grows the documented
    // field keeps its own identity instead of the fallback.
    let explicit = fixture.run_with_env(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(
            r#"{"hook_event_name":"Stop","session_id":"claude-session-secret","turn_id":"explicit-turn","prompt_id":"claude-prompt-secret","stop_hook_active":false}"#,
        ),
        &envs,
    );
    assert_eq!(explicit.code, 0, "stderr={}", explicit.stderr_text());
    let explicit_event = fs::read_to_string(&input_path).expect("captured event");
    let explicit_json: serde_json::Value =
        serde_json::from_str(&explicit_event).expect("event JSON");
    assert_ne!(
        explicit_json["provider_turn_id"], stop_json["provider_turn_id"],
        "turn_id must not be shadowed by prompt_id"
    );
}

#[test]
fn failed_stop_activity_is_terminally_degraded_without_weakening_pre_tool_admission() {
    let fixture = Fixture::new(FAILURE_POLICY);
    let fake = fixture.root.join("agent-session-failing");
    fs::write(&fake, "#!/bin/sh\nexit 65\n").expect("fake agent-session");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).expect("fake mode");
    let envs = [
        ("AGENT_SESSION_BIN", fake.to_str().expect("fake path")),
        ("AGENT_SESSION_ID", "managed-session"),
        ("AGENT_SESSION_RUNTIME_ID", "managed-runtime"),
    ];

    for product in ["codex", "claude"] {
        let stop = fixture.run_with_env(
            &["dispatch", "--product", product, "--format", "json"],
            Some(r#"{"hook_event_name":"Stop"}"#),
            &envs,
        );
        assert_eq!(
            stop.code,
            0,
            "{product} failed terminal activity observation must not create an infinite Stop loop: stdout={} stderr={}",
            stop.stdout_text(),
            stop.stderr_text()
        );
        assert_eq!(stop.stdout_json()["data"]["action"], "warn");
        assert_eq!(
            stop.stdout_json()["data"]["reasons"][0]["code"],
            "activity-stop-reconciliation-required"
        );

        let provider = fixture.run_with_env(
            &["dispatch", "--product", product, "--format", "provider"],
            Some(r#"{"hook_event_name":"Stop"}"#),
            &envs,
        );
        assert_eq!(
            provider.code,
            0,
            "{product} provider rendering must preserve terminal admission: stdout={} stderr={}",
            provider.stdout_text(),
            provider.stderr_text()
        );
        if product == "codex" {
            assert_eq!(
                provider.stdout_json(),
                serde_json::json!({}),
                "Codex Stop does not support additionalContext"
            );
        } else {
            assert_eq!(
                provider.stdout_json()["hookSpecificOutput"]["hookEventName"],
                "Stop"
            );
            assert_eq!(
                provider.stdout_json()["hookSpecificOutput"]["additionalContext"],
                "activity-stop-reconciliation-required"
            );
        }
    }

    let pre_tool = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"true"}}"#),
        &envs,
    );
    assert_eq!(
        pre_tool.code, 1,
        "ordinary tool admission must remain fail closed when activity recording fails"
    );
    assert_eq!(pre_tool.stdout_json()["data"]["action"], "block");
    assert_eq!(
        pre_tool.stdout_json()["data"]["reasons"][0]["code"],
        "session.activity:capability-failure-closed"
    );
}

#[test]
fn failed_stop_activity_does_not_override_authoritative_coordination_block() {
    let fixture = Fixture::new(FAILURE_COORDINATION_POLICY);
    let fake = fixture.root.join("agent-session-failing");
    fs::write(&fake, "#!/bin/sh\nexit 65\n").expect("fake agent-session");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).expect("fake mode");

    let hooks = fixture.home.join(".codex/hooks");
    fs::create_dir_all(&hooks).expect("hook directory");
    let handler = hooks.join("session-coordination-guard.py");
    fs::write(
        &handler,
        "#!/bin/sh\nset -eu\nprintf '%s\\n' '{\"decision\":\"block\",\"reason\":\"claim-conflict\"}'\n",
    )
    .expect("coordination handler");
    fs::set_permissions(&handler, fs::Permissions::from_mode(0o700)).expect("handler mode");

    let blocked = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(r#"{"hook_event_name":"Stop"}"#),
        &[
            ("AGENT_SESSION_BIN", fake.to_str().expect("fake path")),
            ("AGENT_SESSION_ID", "managed-session"),
            ("AGENT_SESSION_RUNTIME_ID", "managed-runtime"),
        ],
    );
    assert_eq!(blocked.code, 1, "stderr={}", blocked.stderr_text());
    let decision = blocked.stdout_json();
    assert_eq!(decision["data"]["action"], "block");
    let reasons = decision["data"]["reasons"]
        .as_array()
        .expect("decision reasons");
    assert!(
        reasons.iter().any(|reason| {
            reason["code"] == "activity-stop-reconciliation-required"
                && reason["disposition"] == "warn"
        }),
        "missing activity warning: {decision}"
    );
    assert!(
        reasons.iter().any(|reason| {
            reason["code"] == "coordination-admitted" && reason["disposition"] == "block"
        }),
        "missing coordination block: {decision}"
    );
}

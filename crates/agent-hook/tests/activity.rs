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

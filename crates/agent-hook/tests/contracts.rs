mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use pretty_assertions::assert_eq;
use serde_json::json;

use support::{Fixture, now_epoch};

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

[[rules]]
id = "runtime.semantic-conflict"
products = ["codex", "claude"]
events = ["PreToolUse"]
priority = 110
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.semantic-conflict.v1", reason_code = "semantic-conflict" }
"#;

#[test]
fn strict_policy_accepts_grouped_matcher_and_inventory_hides_parameters() {
    let fixture = Fixture::new(POLICY);
    let validate = fixture.run(&["validate", "--format", "json"], None);
    assert_eq!(validate.code, 0, "stderr={}", validate.stderr_text());
    let envelope = validate.stdout_json();
    assert_eq!(envelope["schema_version"], "cli.agent-hook-validate.v1");
    assert_eq!(envelope["command"], "agent-hook validate");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["rule_count"], 2);

    let inventory = fixture.run(&["inventory", "--format", "json"], None);
    assert_eq!(inventory.code, 0);
    let text = inventory.stdout_text();
    assert!(text.contains("Write|Edit|NotebookEdit|MultiEdit|apply_patch"));
    assert!(!text.contains("replacement"));
    assert!(!text.contains("handler_id"));
}

#[test]
fn matcher_expression_rejects_regex_constructs() {
    let invalid = POLICY.replace(
        "Write|Edit|NotebookEdit|MultiEdit|apply_patch",
        "^(Write|Edit)$",
    );
    let fixture = Fixture::new(&invalid);
    let output = fixture.run(&["validate", "--format", "json"], None);
    assert_eq!(output.code, 65);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "matcher-expression-invalid"
    );
}

#[test]
fn session_start_source_is_the_exact_matcher_for_codex_and_claude() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.session-start"
products = ["codex", "claude"]
events = ["SessionStart"]
matcher = "startup|resume|clear"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.allow.v1", reason_code = "session-start-known" }
"#;
    let fixture = Fixture::new(policy);
    for product in ["codex", "claude"] {
        let known = fixture.run(
            &["dispatch", "--product", product, "--format", "json"],
            Some(r#"{"hook_event_name":"SessionStart","source":"resume"}"#),
        );
        assert_eq!(known.code, 0, "stderr={}", known.stderr_text());
        assert_eq!(
            known.stdout_json()["result"]["reasons"][0]["code"],
            "session-start-known"
        );

        let unknown = fixture.run(
            &["dispatch", "--product", product, "--format", "json"],
            Some(r#"{"hook_event_name":"SessionStart","source":"future-source"}"#),
        );
        assert_eq!(unknown.code, 0, "stderr={}", unknown.stderr_text());
        assert_eq!(unknown.stdout_json()["result"]["reasons"], json!([]));
    }
}

#[test]
fn forged_payload_conflict_is_ignored_but_registry_conflict_blocks() {
    let fixture = Fixture::new(POLICY);
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "cwd": fixture.root,
        "semantic_conflict": "definite"
    })
    .to_string();
    let forged = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
    );
    assert_eq!(forged.code, 0, "stderr={}", forged.stderr_text());
    assert_eq!(forged.stdout_json()["result"]["action"], "warn");

    let coordination = fixture.session_state.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination dir");
    fs::set_permissions(&coordination, fs::Permissions::from_mode(0o700))
        .expect("coordination mode");
    let now = now_epoch();
    let registry = json!({
        "schema_version": "agent-session.coordination-registry.v1",
        "fingerprint_epoch": 1,
        "fingerprint_key": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "brokers": {
            "current": {"session_id":"current","incarnation":"inc-current","state":"ready","heartbeat_epoch":now},
            "peer": {"session_id":"peer","incarnation":"inc-peer","state":"ready","heartbeat_epoch":now}
        },
        "claims": [
            {"session_id":"current","session_incarnation":"inc-current","state":"active","worktrees":["hmac-sha256:1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],"repositories":["owner/repo"],"provider_refs":[],"scopes":[],"expires_at_epoch":now+300},
            {"session_id":"peer","session_incarnation":"inc-peer","state":"active","worktrees":["hmac-sha256:1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],"repositories":["owner/repo"],"provider_refs":[],"scopes":[],"expires_at_epoch":now+300}
        ]
    });
    let registry_path = coordination.join("registry.json");
    fs::write(
        &registry_path,
        serde_json::to_vec(&registry).expect("registry JSON"),
    )
    .expect("registry");
    Fixture::set_private(&registry_path);

    let options = nils_test_support::cmd::CmdOptions::new()
        .with_cwd(&fixture.root)
        .with_env("HOME", fixture.home.to_str().expect("home"))
        .with_env(
            "XDG_CONFIG_HOME",
            fixture.config_home.to_str().expect("config"),
        )
        .with_env(
            "XDG_STATE_HOME",
            fixture.state_home.to_str().expect("state"),
        )
        .with_env(
            "AGENT_SESSION_STATE_DIR",
            fixture.session_state.to_str().expect("session"),
        )
        .with_env("AGENT_SESSION_ID", "current")
        .with_stdin_str(&payload);
    let backed = nils_test_support::cmd::run_resolved(
        "agent-hook",
        &["dispatch", "--product", "codex", "--format", "json"],
        &options,
    );
    assert_eq!(backed.code, 1, "stderr={}", backed.stderr_text());
    assert_eq!(backed.stdout_json()["result"]["action"], "block");
}

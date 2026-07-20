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
    assert_eq!(envelope["schema_version"], "cli.agent-hook.validate.v1");
    assert!(envelope.get("command").is_none());
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["rule_count"], 2);

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
            known.stdout_json()["data"]["reasons"][0]["code"],
            "session-start-known"
        );

        let unknown = fixture.run(
            &["dispatch", "--product", product, "--format", "json"],
            Some(r#"{"hook_event_name":"SessionStart","source":"future-source"}"#),
        );
        assert_eq!(unknown.code, 0, "stderr={}", unknown.stderr_text());
        assert_eq!(unknown.stdout_json()["data"]["reasons"], json!([]));
    }
}

#[test]
fn forged_payload_conflict_is_ignored_but_registry_conflict_blocks() {
    let fixture = Fixture::new(POLICY);
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "cwd": fixture.root,
        "tool_input": {"path": fixture.root.join("target.txt")},
        "semantic_conflict": "definite"
    })
    .to_string();
    let forged = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
    );
    assert_eq!(forged.code, 0, "stderr={}", forged.stderr_text());
    assert_eq!(forged.stdout_json()["data"]["action"], "warn");

    let coordination = fixture.session_state.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination dir");
    fs::set_permissions(&coordination, fs::Permissions::from_mode(0o700))
        .expect("coordination mode");
    let now = now_epoch();
    let registry_path = coordination.join("registry.json");
    for (session, incarnation) in [("current", "inc-current"), ("peer", "inc-peer")] {
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

    for (current_mode, peer_mode, expected_code, expected_action) in [
        ("advisory", "enforce", 0, "warn"),
        ("enforce", "advisory", 1, "block"),
        ("off", "enforce", 0, "allow"),
        ("enforce", "off", 0, "allow"),
    ] {
        let registry = json!({
            "schema_version": "agent-session.coordination-registry.v1",
            "fingerprint_epoch": 1,
            "fingerprint_key": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "brokers": {
                "current": {"session_id":"current","incarnation":"inc-current","state":"ready","heartbeat_epoch":now,"coordination_mode":current_mode},
                "peer": {"session_id":"peer","incarnation":"inc-peer","state":"ready","heartbeat_epoch":now,"coordination_mode":peer_mode}
            },
            "claims": [
                {"schema_version":"agent-session.work-context.v1","session_id":"current","session_incarnation":"inc-current","state":"active","worktrees":["hmac-sha256:1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],"repositories":["owner/repo"],"provider_refs":[],"scopes":[],"expires_at_epoch":now+300},
                {"schema_version":"agent-session.work-context.v1","session_id":"peer","session_incarnation":"inc-peer","state":"active","worktrees":["hmac-sha256:1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],"repositories":["owner/repo"],"provider_refs":[],"scopes":[],"expires_at_epoch":now+300}
            ]
        });
        fs::write(
            &registry_path,
            serde_json::to_vec(&registry).expect("registry JSON"),
        )
        .expect("registry");
        Fixture::set_private(&registry_path);
        if current_mode == "advisory" {
            let absent = dispatch_managed(&fixture, &payload, "advisory");
            assert_eq!(
                absent.code, 1,
                "an environment-only advisory mode must not downgrade without a durable record"
            );
            assert_eq!(absent.stdout_json()["data"]["action"], "block");

            write_session_record(&fixture, "current", "inc-current", "enforce");
            let mismatched = dispatch_managed(&fixture, &payload, "advisory");
            assert_eq!(
                mismatched.code, 1,
                "a broker/session/environment mode mismatch must fail closed"
            );
            assert_eq!(mismatched.stdout_json()["data"]["action"], "block");
        }
        write_session_record(&fixture, "current", "inc-current", current_mode);

        let backed = dispatch_managed(&fixture, &payload, current_mode);
        assert_eq!(
            backed.code,
            expected_code,
            "current={current_mode} peer={peer_mode} stderr={}",
            backed.stderr_text()
        );
        assert_eq!(
            backed.stdout_json()["data"]["action"],
            expected_action,
            "current={current_mode} peer={peer_mode}"
        );
    }
}

fn dispatch_managed(
    fixture: &Fixture,
    payload: &str,
    mode: &str,
) -> nils_test_support::cmd::CmdOutput {
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
        .with_env("AGENT_SESSION_RUNTIME_ID", "inc-current")
        .with_env("AGENT_SESSION_COORDINATION_MODE", mode)
        .with_stdin_str(payload);
    nils_test_support::cmd::run_resolved(
        "agent-hook",
        &["dispatch", "--product", "codex", "--format", "json"],
        &options,
    )
}

fn write_session_record(fixture: &Fixture, session: &str, incarnation: &str, mode: &str) {
    let directory = fixture.session_state.join("sessions").join(session);
    fs::create_dir_all(&directory).expect("session directory");
    let path = directory.join("session.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "schema_version": "agent-session.session.v1",
            "id": session,
            "coordination_mode": mode,
            "runtime": {"launch_id": incarnation}
        }))
        .expect("session JSON"),
    )
    .expect("session record");
    Fixture::set_private(&path);
}

#[test]
fn cli_help_version_and_completion_surface_are_complete() {
    let fixture = Fixture::new(POLICY);
    let help = fixture.run(&["--help"], None);
    assert_eq!(help.code, 0, "stderr={}", help.stderr_text());
    let help_text = help.stdout_text();
    for required in [
        "dispatch",
        "validate",
        "inventory",
        "doctor",
        "setup",
        "recovery",
        "completion",
        "-V, --version",
    ] {
        assert!(
            help_text.contains(required),
            "missing {required}: {help_text}"
        );
    }

    let version = fixture.run(&["--version"], None);
    assert_eq!(version.code, 0, "stderr={}", version.stderr_text());
    assert!(version.stdout_text().starts_with("agent-hook "));

    for shell in ["bash", "zsh"] {
        let completion = fixture.run(&["completion", shell], None);
        assert_eq!(completion.code, 0, "shell={shell}");
        let script = completion.stdout_text();
        assert!(script.contains("dispatch"), "shell={shell}");
        assert!(script.contains("--expected-plan-digest"), "shell={shell}");
    }
}

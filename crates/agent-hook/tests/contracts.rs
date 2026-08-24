mod support;

use std::ffi::OsStr;
use std::fs;
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};

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

        if matches!(current_mode, "advisory" | "off") {
            let missing = dispatch_managed_without_hint(&fixture, &payload);
            assert_eq!(
                missing.code, expected_code,
                "a truly missing hint must preserve durable mode current={current_mode}"
            );
            assert_eq!(
                missing.stdout_json()["data"]["action"],
                expected_action,
                "missing hint current={current_mode}"
            );

            for invalid_hint in ["unsupported", "advisory "] {
                let invalid = dispatch_managed(&fixture, &payload, invalid_hint);
                assert_eq!(
                    invalid.code, 1,
                    "present invalid Unicode hint must fail closed current={current_mode} hint={invalid_hint:?}"
                );
                assert_eq!(
                    invalid.stdout_json()["data"]["action"],
                    "block",
                    "present invalid Unicode hint current={current_mode} hint={invalid_hint:?}"
                );
            }

            let invalid = dispatch_managed_with_non_unicode_hint(&fixture, &payload);
            assert_eq!(
                invalid.code, 1,
                "present non-Unicode hint must fail closed current={current_mode}"
            );
            assert_eq!(
                invalid.stdout_json()["data"]["action"],
                "block",
                "present non-Unicode hint current={current_mode}"
            );
        }
    }
}

#[test]
fn invalid_runtime_mode_hint_never_tolerates_coordination_store_failure() {
    let fixture = Fixture::new(POLICY);
    let coordination = fixture.session_state.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination directory");
    let registry = coordination.join("registry.json");
    fs::write(&registry, b"{").expect("malformed registry");
    Fixture::set_private(&registry);
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "cwd": fixture.root,
        "tool_input": {"path": fixture.root.join("target.txt")}
    })
    .to_string();

    for (mode, expected_action) in [("advisory", "warn"), ("off", "allow")] {
        write_session_record(&fixture, "current", "inc-current", mode);

        for trusted in [
            dispatch_managed_without_hint(&fixture, &payload),
            dispatch_managed(&fixture, &payload, mode),
        ] {
            assert_eq!(
                trusted.code, 0,
                "missing and exact hints must preserve trusted durable mode={mode}"
            );
            assert_eq!(
                trusted.stdout_json()["data"]["action"],
                expected_action,
                "trusted durable mode={mode}"
            );
        }

        for invalid_hint in ["unsupported", "off\n"] {
            let invalid = dispatch_managed(&fixture, &payload, invalid_hint);
            assert_eq!(
                invalid.code, 65,
                "invalid Unicode hint must not tolerate malformed coordination state mode={mode} hint={invalid_hint:?}"
            );
            assert_eq!(
                invalid.stdout_json()["error"]["code"],
                "coordination-invalid"
            );
        }

        let invalid = dispatch_managed_with_non_unicode_hint(&fixture, &payload);
        assert_eq!(
            invalid.code, 65,
            "non-Unicode hint must not tolerate malformed coordination state mode={mode}"
        );
        assert_eq!(
            invalid.stdout_json()["error"]["code"],
            "coordination-invalid"
        );
    }
}

fn dispatch_managed(
    fixture: &Fixture,
    payload: &str,
    mode: &str,
) -> nils_test_support::cmd::CmdOutput {
    run_managed(
        payload,
        managed_options(fixture).with_env("AGENT_SESSION_COORDINATION_MODE", mode),
    )
}

fn dispatch_managed_without_hint(
    fixture: &Fixture,
    payload: &str,
) -> nils_test_support::cmd::CmdOutput {
    run_managed(
        payload,
        managed_options(fixture).with_env_remove("AGENT_SESSION_COORDINATION_MODE"),
    )
}

fn dispatch_managed_with_non_unicode_hint(
    fixture: &Fixture,
    payload: &str,
) -> nils_test_support::cmd::CmdOutput {
    run_managed(
        payload,
        managed_options(fixture).with_env_os(
            OsStr::new("AGENT_SESSION_COORDINATION_MODE"),
            OsStr::from_bytes(b"advisory\xff"),
        ),
    )
}

fn managed_options(fixture: &Fixture) -> nils_test_support::cmd::CmdOptions {
    nils_test_support::cmd::CmdOptions::new()
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
}

fn run_managed(
    payload: &str,
    options: nils_test_support::cmd::CmdOptions,
) -> nils_test_support::cmd::CmdOutput {
    let options = options.with_stdin_str(payload);
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
        "finish-line",
        "workspace-lease",
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

    let finish_line_help = fixture.run(&["finish-line", "--help"], None);
    assert_eq!(
        finish_line_help.code,
        0,
        "stderr={}",
        finish_line_help.stderr_text()
    );
    let finish_line_help_text = finish_line_help.stdout_text();
    assert!(finish_line_help_text.contains("Probe or supervise one foreground DSH Bash command"));
    assert!(finish_line_help_text.contains("classify exact validation targets"));
    for required in ["open", "begin", "run", "stop", "status"] {
        assert!(
            finish_line_help_text.contains(required),
            "finish-line help missing {required}: {finish_line_help_text}"
        );
    }
    for removed in ["complete", "waive", "approve", "revoke"] {
        assert!(
            !finish_line_help_text.contains(removed),
            "finish-line help retains removed {removed}: {finish_line_help_text}"
        );
    }
    assert!(
        !finish_line_help_text.contains("quiesce"),
        "internal quiesce leaked into public finish-line help: {finish_line_help_text}"
    );
    assert!(
        !finish_line_help_text.contains("release"),
        "internal release leaked into public finish-line help: {finish_line_help_text}"
    );
    let begin_help = fixture.run(&["finish-line", "begin", "--help"], None);
    assert_eq!(begin_help.code, 0, "stderr={}", begin_help.stderr_text());
    assert!(begin_help.stdout_text().contains("[default: json]"));
    assert!(!begin_help.stdout_text().contains("text output (default)"));
    let run_help = fixture.run(&["finish-line", "run", "--help"], None);
    assert_eq!(run_help.code, 0, "stderr={}", run_help.stderr_text());
    assert!(run_help.stdout_text().contains("[default: json]"));
    assert!(!run_help.stdout_text().contains("text output (default)"));

    let workspace_help = fixture.run(&["workspace-lease", "--help"], None);
    assert_eq!(
        workspace_help.code,
        0,
        "stderr={}",
        workspace_help.stderr_text()
    );
    let workspace_help_text = workspace_help.stdout_text();
    for required in ["bind", "begin", "complete", "renew", "release"] {
        assert!(
            workspace_help_text.contains(required),
            "workspace-lease help missing {required}: {workspace_help_text}"
        );
    }
    let workspace_bind_help = fixture.run(&["workspace-lease", "bind", "--help"], None);
    assert_eq!(workspace_bind_help.code, 0);
    assert!(
        workspace_bind_help
            .stdout_text()
            .contains("[default: json]")
    );

    for shell in ["bash", "zsh"] {
        let completion = fixture.run(&["completion", shell], None);
        assert_eq!(completion.code, 0, "shell={shell}");
        let script = completion.stdout_text();
        assert!(
            !script.contains("quiesce"),
            "internal quiesce leaked anywhere in public completion; shell={shell}"
        );
        assert!(script.contains("dispatch"), "shell={shell}");
        assert!(script.contains("workspace-lease"), "shell={shell}");
        assert!(script.contains("renew"), "shell={shell}");
        assert!(script.contains("--expected-plan-digest"), "shell={shell}");
        let (scope_start, scope_end) = if shell == "bash" {
            (
                "        agent__hook__finish__line)\n",
                "        agent__hook__finish__line__begin)\n",
            )
        } else {
            (
                "_agent-hook__subcmd__finish-line_commands() {\n",
                "_agent-hook__subcmd__finish-line__subcmd__begin_commands() {\n",
            )
        };
        let start = script
            .find(scope_start)
            .expect("finish-line completion start");
        let relative_end = script[start + scope_start.len()..]
            .find(scope_end)
            .expect("finish-line completion end");
        let finish_line_scope = &script[start..start + scope_start.len() + relative_end];
        for required in ["open", "begin", "run", "stop", "status"] {
            assert!(
                finish_line_scope.contains(required),
                "missing {required}; shell={shell}"
            );
        }
        for removed in ["complete", "waive", "approve", "revoke"] {
            assert!(
                !finish_line_scope.contains(removed),
                "retains removed {removed}; shell={shell}"
            );
        }
        assert!(
            !finish_line_scope.contains("quiesce"),
            "internal quiesce leaked into public completion; shell={shell}"
        );
        assert!(
            !finish_line_scope.contains("release"),
            "internal release leaked into public completion; shell={shell}"
        );
        if shell == "zsh" {
            assert!(
                finish_line_scope.contains("foreground DSH Bash command"),
                "finish-line run description drifted; shell={shell}"
            );
        }
        assert!(!finish_line_scope.contains("text output (default)"));
    }
}

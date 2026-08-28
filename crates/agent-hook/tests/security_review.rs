mod support;

use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Stdio};

use pretty_assertions::{assert_eq, assert_ne};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use support::{Fixture, now_epoch};

const BLOCK_POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.locked-block"
products = ["codex"]
events = ["PreToolUse", "PermissionRequest"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "locked-block", message = "blocked" }
"#;

fn owner_policy() -> String {
    format!(
        r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.owner"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Write|Edit|NotebookEdit|apply_patch"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = {{ id = "agent-session.owner-liveness.v1", reason_code = "owner", {} = 300 }}
"#,
        concat!("leg", "acy_ttl_seconds")
    )
}

fn coordination_policy() -> String {
    format!(
        r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.semantic"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Write|Edit|NotebookEdit|apply_patch"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = {{ id = "agent-session.semantic-conflict.v1", reason_code = "semantic-conflict" }}

[[rules]]
id = "runtime.owner"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Write|Edit|NotebookEdit|apply_patch"
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = {{ id = "agent-session.owner-liveness.v1", reason_code = "owner", {} = 300 }}

[[rules]]
id = "runtime.coordination"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Write|Edit|NotebookEdit|apply_patch"
priority = 30
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = {{ id = "agent-session.coordination.v1", reason_code = "coordination" }}
"#,
        concat!("leg", "acy_ttl_seconds")
    )
}

#[test]
fn provider_json_rejects_duplicate_security_relevant_keys_at_every_depth() {
    let fixture = Fixture::new(BLOCK_POLICY);
    let root = fixture.root.to_string_lossy();
    let cases = [
        format!(
            r#"{{"hook_event_name":"PreToolUse","hook_event_name":"Stop","tool_name":"Write","cwd":{root:?}}}"#
        ),
        format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Write","tool_name":"Edit","cwd":{root:?}}}"#
        ),
        format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Write","cwd":{root:?},"tool_input":{{"path":{root:?},"path":{root:?}}}}}"#
        ),
        format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Write","cwd":{root:?},"tool_input":{{"command":"one","command":"two"}}}}"#
        ),
    ];

    for payload in cases {
        let output = fixture.run(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
        );
        assert_eq!(
            output.code,
            65,
            "payload={payload} stderr={}",
            output.stderr_text()
        );
        assert_eq!(
            output.stdout_json()["error"]["code"],
            "provider-input-duplicate-key"
        );
    }
}

#[test]
fn codex_pre_tool_and_permission_blocks_use_supported_denial_output() {
    let fixture = Fixture::new(BLOCK_POLICY);
    for event in ["PreToolUse", "PermissionRequest"] {
        let payload = json!({
            "hook_event_name": event,
            "tool_name": "Write",
            "cwd": fixture.root,
            "tool_input": {"path": fixture.root.join("target.txt")}
        })
        .to_string();
        let output = fixture.run(
            &["dispatch", "--product", "codex", "--format", "provider"],
            Some(&payload),
        );
        assert_eq!(
            output.code,
            0,
            "event={event} stderr={}",
            output.stderr_text()
        );
        let rendered = output.stdout_json();
        assert_eq!(rendered["hookSpecificOutput"]["hookEventName"], event);
        if event == "PreToolUse" {
            assert_eq!(rendered["hookSpecificOutput"]["permissionDecision"], "deny");
            assert_eq!(
                rendered["hookSpecificOutput"]["permissionDecisionReason"],
                "agent-hook:locked-block"
            );
        } else {
            assert_eq!(
                rendered["hookSpecificOutput"]["decision"]["behavior"],
                "deny"
            );
            assert_eq!(
                rendered["hookSpecificOutput"]["decision"]["message"],
                "agent-hook:locked-block"
            );
        }
        assert!(rendered.get("continue").is_none());
        assert!(rendered.get("stopReason").is_none());
    }
}

#[test]
fn provider_modes_cannot_reduce_locked_rule_authority() {
    for mode in ["shadow", "disabled"] {
        let fixture = Fixture::new(BLOCK_POLICY);
        let mut config = fs::read_to_string(&fixture.config).expect("config");
        config.push_str(&format!("\n[providers.codex]\nmode = \"{mode}\"\n"));
        fs::write(&fixture.config, config).expect("config");
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Write",
            "cwd": fixture.root,
            "tool_input": {"path": fixture.root.join("target.txt")}
        })
        .to_string();
        let output = fixture.run(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
        );
        assert_eq!(
            output.code,
            1,
            "mode={mode} stderr={}",
            output.stderr_text()
        );
        assert_eq!(output.stdout_json()["data"]["action"], "block");
    }
}

#[test]
fn config_and_policy_reject_group_or_world_writable_opened_files() {
    for (role, mode) in [("config", 0o660), ("policy", 0o666)] {
        let fixture = Fixture::new(BLOCK_POLICY);
        let path = if role == "config" {
            &fixture.config
        } else {
            &fixture.policy
        };
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("unsafe mode");
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Write",
            "cwd": fixture.root,
            "tool_input": {"path": fixture.root.join("target.txt")}
        })
        .to_string();
        let commands: [(&[&str], Option<&str>); 4] = [
            (&["validate", "--format", "json"], None),
            (&["inventory", "--format", "json"], None),
            (
                &[
                    "setup",
                    "--product",
                    "codex",
                    "--dry-run",
                    "--format",
                    "json",
                ],
                None,
            ),
            (
                &["dispatch", "--product", "codex", "--format", "json"],
                Some(payload.as_str()),
            ),
        ];
        for (command, stdin) in commands {
            let output = fixture.run(command, stdin);
            assert_eq!(output.code, 65, "role={role} command={command:?}");
            assert_eq!(
                output.stdout_json()["error"]["code"],
                format!("{role}-untrusted")
            );
        }
    }
}

#[test]
fn state_root_symlink_is_rejected_without_mutating_its_target() {
    let fixture = Fixture::new(BLOCK_POLICY);
    let sentinel = fixture.root.join("sentinel-state");
    fs::create_dir(&sentinel).expect("sentinel directory");
    fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o755)).expect("sentinel mode");
    fs::write(sentinel.join("keep"), b"unchanged").expect("sentinel file");
    let linked = fixture.root.join("linked-state");
    std::os::unix::fs::symlink(&sentinel, &linked).expect("state symlink");
    let linked_arg = linked.to_str().expect("linked state");
    let digest = support::sha256(b"binding");
    let challenge = fixture.root.join("symlink-challenge.json");
    let challenge_arg = challenge.to_str().expect("challenge path");
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"Write",
        "cwd":fixture.root,
        "tool_input":{"path":fixture.root.join("target.txt")}
    })
    .to_string();
    let cases = [
        (
            vec![
                "--state-dir",
                linked_arg,
                "setup",
                "--product",
                "codex",
                "--apply",
                "--format",
                "json",
            ],
            None,
            "setup-state-dir-untrusted",
        ),
        (
            vec![
                "--state-dir",
                linked_arg,
                "dispatch",
                "--product",
                "codex",
                "--trace",
                "--format",
                "json",
            ],
            Some(payload.as_str()),
            "trace-dir-untrusted",
        ),
        (
            vec![
                "--state-dir",
                linked_arg,
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
                "runtime.locked-block",
                "--out",
                challenge_arg,
                "--format",
                "json",
            ],
            None,
            "recovery-state-dir-untrusted",
        ),
    ];
    for (args, stdin, expected) in cases {
        let output = fixture.run(&args, stdin);
        assert_eq!(
            output.code,
            65,
            "args={args:?} stderr={}",
            output.stderr_text()
        );
        assert_eq!(output.stdout_json()["error"]["code"], expected);
        assert_eq!(
            fs::metadata(&sentinel)
                .expect("sentinel metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::read(sentinel.join("keep")).expect("sentinel file"),
            b"unchanged"
        );
        assert_eq!(
            fs::read_dir(&sentinel).expect("sentinel entries").count(),
            1
        );
    }
}

#[test]
fn registry_liveness_requires_private_incarnation_bound_nonfuture_heartbeat() {
    let fixture = Fixture::new(&owner_policy());
    let target = fixture.root.join("owned-checkout");
    fs::create_dir_all(&target).expect("target");
    let now = now_epoch();
    let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    write_registry(
        &fixture,
        key,
        json!({
            "peer": {"session_id":"peer","incarnation":"inc-peer","state":"ready","heartbeat_epoch":now}
        }),
        json!([{
            "schema_version":"agent-session.work-context.v1",
            "session_id":"peer",
            "session_incarnation":"inc-peer",
            "state":"active",
            "worktrees":[fingerprint(key, 1, &target)],
            "repositories":["owner/repo"],
            "provider_refs":[],
            "scopes":[],
            "expires_at_epoch":now+300
        }]),
    );
    let heartbeat = heartbeat_path(&fixture, "peer");
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"Write",
        "cwd":target,
        "tool_input":{"path":target.join("file.txt")}
    })
    .to_string();

    for heartbeat_value in [
        None,
        Some("wrong:0".to_string()),
        Some(format!("inc-peer:{}", now + 60)),
    ] {
        if let Some(ref value) = heartbeat_value {
            fs::create_dir_all(heartbeat.parent().expect("heartbeat parent"))
                .expect("heartbeat dir");
            fs::write(&heartbeat, format!("{value}\n")).expect("heartbeat");
            fs::set_permissions(&heartbeat, fs::Permissions::from_mode(0o600))
                .expect("heartbeat mode");
        } else if heartbeat.exists() {
            fs::remove_file(&heartbeat).expect("remove heartbeat");
        }
        let output = fixture.run(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
        );
        let envelope = output.stdout_json();
        let code = envelope["data"]["reasons"][0]["code"]
            .as_str()
            .expect("reason");
        assert_ne!(
            code, "owner-active-foreign",
            "heartbeat={heartbeat_value:?}"
        );
        assert!(code.starts_with("owner-stale") || code.contains("untrusted"));
    }
}

#[test]
fn matching_session_id_requires_exact_runtime_incarnation_for_self_ownership() {
    let fixture = Fixture::new(&owner_policy());
    let target = fixture.root.join("incarnation-owned-checkout");
    fs::create_dir_all(&target).expect("target");
    let now = now_epoch();
    let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    write_registry(
        &fixture,
        key,
        json!({
            "current": {
                "session_id":"current",
                "incarnation":"inc-current",
                "state":"ready",
                "heartbeat_epoch":now
            }
        }),
        json!([{
            "schema_version":"agent-session.work-context.v1",
            "session_id":"current",
            "session_incarnation":"inc-current",
            "state":"active",
            "worktrees":[fingerprint(key, 1, &target)],
            "repositories":["owner/repo"],
            "provider_refs":[],
            "scopes":[],
            "expires_at_epoch":now+300
        }]),
    );
    let heartbeat = heartbeat_path(&fixture, "current");
    fs::create_dir_all(heartbeat.parent().expect("heartbeat parent")).expect("heartbeat dir");
    fs::write(&heartbeat, format!("inc-current:{now}\n")).expect("heartbeat");
    Fixture::set_private(&heartbeat);
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"Write",
        "cwd":target,
        "tool_input":{"path":target.join("file.txt")}
    })
    .to_string();

    let exact = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[
            ("AGENT_SESSION_ID", "current"),
            ("AGENT_SESSION_RUNTIME_ID", "inc-current"),
        ],
    );
    assert_eq!(exact.code, 0, "stderr={}", exact.stderr_text());
    assert_eq!(exact.stdout_json()["data"]["action"], "allow");
    assert_eq!(
        exact.stdout_json()["data"]["reasons"][0]["code"],
        "owner-active-self"
    );

    let missing = fixture.run_with_env_and_removals(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[("AGENT_SESSION_ID", "current")],
        &["AGENT_SESSION_RUNTIME_ID"],
    );
    assert_eq!(missing.code, 1, "stderr={}", missing.stderr_text());
    assert_eq!(missing.stdout_json()["data"]["action"], "block");
    assert_eq!(
        missing.stdout_json()["data"]["reasons"][0]["code"],
        "owner-active-foreign"
    );

    let mismatched = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[
            ("AGENT_SESSION_ID", "current"),
            ("AGENT_SESSION_RUNTIME_ID", "inc-other"),
        ],
    );
    assert_eq!(mismatched.code, 1, "stderr={}", mismatched.stderr_text());
    assert_eq!(mismatched.stdout_json()["data"]["action"], "block");
    assert_eq!(
        mismatched.stdout_json()["data"]["reasons"][0]["code"],
        "owner-active-foreign"
    );
}

#[test]
fn fully_unmanaged_process_bypasses_coordination_capabilities() {
    let fixture = Fixture::new(&coordination_policy());
    let target = fixture.root.join("unmanaged-checkout");
    fs::create_dir_all(&target).expect("target");
    let now = now_epoch();
    let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let fallback_state = fixture.state_home.join("agent-session");
    let brokers = json!({
        "peer": {
            "session_id":"peer",
            "incarnation":"inc-peer",
            "state":"ready",
            "heartbeat_epoch":now
        }
    });
    let claims = json!([{
        "schema_version":"agent-session.work-context.v1",
        "session_id":"peer",
        "session_incarnation":"inc-peer",
        "state":"active",
        "worktrees":[fingerprint(key, 1, &target)],
        "repositories":["owner/repo"],
        "provider_refs":[],
        "scopes":[],
        "expires_at_epoch":now+300
    }]);
    write_registry_at(&fallback_state, key, brokers.clone(), claims.clone());
    write_registry_at(&fixture.session_state, key, brokers, claims);
    for state_root in [&fallback_state, &fixture.session_state] {
        let heartbeat = heartbeat_path_at(state_root, "peer");
        fs::create_dir_all(heartbeat.parent().expect("heartbeat parent")).expect("heartbeat dir");
        fs::write(&heartbeat, format!("inc-peer:{now}\n")).expect("heartbeat");
        Fixture::set_private(&heartbeat);
    }
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"Write",
        "cwd":target,
        "tool_input":{"path":target.join("file.txt")}
    })
    .to_string();

    let managed_selectors = [
        ("AGENT_SESSION_ID", "partial-session".to_string()),
        (
            "AGENT_SESSION_RUNTIME_ID",
            "partial-incarnation".to_string(),
        ),
        (
            "AGENT_SESSION_STATE_DIR",
            fixture.session_state.to_string_lossy().into_owned(),
        ),
        ("AGENT_SESSION_COORDINATION_MODE", "enforce".to_string()),
        (
            "AGENT_SESSION_CAPABILITY_FILE",
            "/nonexistent/partial-capability".to_string(),
        ),
        (
            "AGENT_SESSION_CHECKPOINT_FILE",
            "/nonexistent/partial-checkpoint".to_string(),
        ),
    ];
    let selector_names = managed_selectors
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>();
    for (selector, configured_value) in &managed_selectors {
        let removals = selector_names
            .iter()
            .copied()
            .filter(|name| name != selector)
            .collect::<Vec<_>>();
        for value in [configured_value.as_str(), ""] {
            let output = fixture.run_with_env_and_removals(
                &["dispatch", "--product", "codex", "--format", "json"],
                Some(&payload),
                &[(*selector, value)],
                &removals,
            );
            assert_ne!(output.code, 0, "selector={selector} value={value:?}");
            assert!(
                !output.stdout_text().contains("coordination-unmanaged"),
                "selector={selector} value={value:?} stdout={}",
                output.stdout_text()
            );
            if !value.is_empty() {
                let decision = output.stdout_json();
                assert_eq!(decision["data"]["action"], "block");
                assert!(
                    decision["data"]["reasons"]
                        .as_array()
                        .is_some_and(|reasons| {
                            reasons.iter().any(|reason| {
                                reason["code"] == "owner-active-foreign"
                                    && reason["disposition"] == "block"
                            })
                        })
                );
            }
        }
    }

    let removals = selector_names;
    let output = fixture.run_with_env_and_removals(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[],
        &removals,
    );
    assert_eq!(
        output.code,
        0,
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    let decision = output.stdout_json();
    assert_eq!(decision["data"]["action"], "allow");
    let reasons = decision["data"]["reasons"]
        .as_array()
        .expect("decision reasons");
    assert_eq!(reasons.len(), 2);
    assert!(reasons.iter().all(|reason| {
        reason["code"] == "coordination-unmanaged" && reason["disposition"] == "allow"
    }));

    let registry = fallback_state.join("coordination/registry.json");
    fs::write(&registry, b"{").expect("malformed registry");
    Fixture::set_private(&registry);
    let malformed = fixture.run_with_env_and_removals(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[("AGENT_SESSION_BIN", "/nonexistent/agent-session")],
        &removals,
    );
    assert_eq!(malformed.code, 0, "stdout={}", malformed.stdout_text());
    assert_eq!(malformed.stdout_json()["data"]["action"], "allow");
}

#[test]
fn fresh_sidecar_heartbeat_outweighs_an_old_registry_projection_timestamp() {
    let fixture = Fixture::new(&owner_policy());
    let target = fixture.root.join("long-running-checkout");
    fs::create_dir_all(&target).expect("target");
    let now = now_epoch();
    let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    write_registry(
        &fixture,
        key,
        json!({
            "peer": {"session_id":"peer","incarnation":"inc-peer","state":"ready","heartbeat_epoch":now-120}
        }),
        json!([{
            "schema_version":"agent-session.work-context.v1",
            "session_id":"peer",
            "session_incarnation":"inc-peer",
            "state":"active",
            "worktrees":[fingerprint(key, 1, &target)],
            "repositories":["owner/repo"],
            "provider_refs":[],
            "scopes":[],
            "expires_at_epoch":now+300
        }]),
    );
    let heartbeat = heartbeat_path(&fixture, "peer");
    fs::create_dir_all(heartbeat.parent().expect("heartbeat parent")).expect("heartbeat dir");
    fs::write(&heartbeat, format!("inc-peer:{now}\n")).expect("heartbeat");
    fs::set_permissions(&heartbeat, fs::Permissions::from_mode(0o600)).expect("heartbeat mode");
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"Write",
        "cwd":target,
        "tool_input":{"path":target.join("file.txt")}
    })
    .to_string();
    let output = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
    );
    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["data"]["reasons"][0]["code"],
        "owner-active-foreign"
    );
}

#[test]
fn forged_advisory_environment_cannot_downgrade_untrusted_coordination_state() {
    let fixture = Fixture::new(&owner_policy());
    let coordination = fixture.session_state.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination directory");
    let registry = coordination.join("registry.json");
    fs::write(&registry, b"{").expect("malformed registry");
    Fixture::set_private(&registry);
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"Write",
        "cwd":fixture.root,
        "tool_input":{"path":fixture.root.join("file.txt")}
    })
    .to_string();

    for mode in ["advisory", "off"] {
        let output = fixture.run_with_env(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
            &[
                ("AGENT_SESSION_ID", "forged"),
                ("AGENT_SESSION_RUNTIME_ID", "forged-incarnation"),
                ("AGENT_SESSION_COORDINATION_MODE", mode),
            ],
        );
        assert_eq!(
            output.code,
            65,
            "mode={mode} stdout={}",
            output.stdout_text()
        );
        assert_eq!(
            output.stdout_json()["error"]["code"],
            "coordination-invalid"
        );
    }

    let session = fixture.session_state.join("sessions/trusted");
    fs::create_dir_all(&session).expect("session directory");
    let record = session.join("session.json");
    fs::write(
        &record,
        serde_json::to_vec(&json!({
            "schema_version":"agent-session.session.v1",
            "id":"trusted",
            "coordination_mode":"advisory",
            "runtime":{"launch_id":"trusted-incarnation"}
        }))
        .expect("session JSON"),
    )
    .expect("session record");
    Fixture::set_private(&record);
    let trusted = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[
            ("AGENT_SESSION_ID", "trusted"),
            ("AGENT_SESSION_RUNTIME_ID", "trusted-incarnation"),
            ("AGENT_SESSION_COORDINATION_MODE", "advisory"),
        ],
    );
    assert_eq!(trusted.code, 0, "stdout={}", trusted.stdout_text());
    assert_eq!(trusted.stdout_json()["data"]["action"], "warn");
}

#[test]
fn inventory_and_runtime_share_provider_specific_effective_modes() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.shared"
products = ["codex", "claude"]
events = ["PreToolUse"]
priority = 10
mode = "enforce"
failure_posture = "open"
override_class = "free"
capability = { id = "decision.allow.v1", reason_code = "shared-allow" }
"#;
    let fixture = Fixture::new(policy);
    let mut config = fs::read_to_string(&fixture.config).expect("config");
    config.push_str("\n[providers.codex]\nmode = \"disabled\"\n");
    fs::write(&fixture.config, config).expect("config");

    let inventory = fixture.run(&["inventory", "--format", "json"], None);
    assert_eq!(inventory.code, 0, "stderr={}", inventory.stderr_text());
    let rule = &inventory.stdout_json()["data"]["rules"][0];
    assert_eq!(rule["effective_modes"]["codex"], "disabled");
    assert_eq!(rule["effective_modes"]["claude"], "enforce");

    for (product, expected_reasons) in [("codex", 0), ("claude", 1)] {
        let payload = json!({
            "hook_event_name":"PreToolUse",
            "tool_name":"Write",
            "cwd":fixture.root,
            "tool_input":{"path":fixture.root.join("target.txt")}
        })
        .to_string();
        let output = fixture.run(
            &["dispatch", "--product", product, "--format", "json"],
            Some(&payload),
        );
        assert_eq!(output.code, 0, "product={product}");
        assert_eq!(
            output.stdout_json()["data"]["reasons"]
                .as_array()
                .expect("reasons")
                .len(),
            expected_reasons
        );
    }
}

#[test]
fn transform_conflict_cannot_be_repopulated_by_a_later_transform() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "transform.one"
products = ["codex"]
events = ["PreToolUse"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.transform.v1", reason_code = "one", replacement = { value = "one" } }

[[rules]]
id = "transform.two"
products = ["codex"]
events = ["PreToolUse"]
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.transform.v1", reason_code = "two", replacement = { value = "two" } }

[[rules]]
id = "transform.three"
products = ["codex"]
events = ["PreToolUse"]
priority = 30
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.transform.v1", reason_code = "three", replacement = { value = "three" } }
"#;
    let fixture = Fixture::new(policy);
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"Write",
        "cwd":fixture.root,
        "tool_input":{"path":fixture.root.join("target.txt")}
    })
    .to_string();
    let output = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
    );
    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    let decision = &output.stdout_json()["data"];
    assert_eq!(decision["action"], "block");
    assert!(decision["replacement"].is_null());
    assert!(decision["provider_output"].is_null());
    assert_eq!(
        decision["reasons"]
            .as_array()
            .expect("reasons")
            .iter()
            .filter(|reason| reason["code"] == "transform-conflict")
            .count(),
        1
    );
}

#[test]
fn explicit_mutation_target_cannot_be_masked_by_the_execution_checkout() {
    let (fixture, checkout_a, checkout_b) = same_repository_foreign_owner_fixture();
    assert_foreign_target_blocks(
        &fixture,
        &checkout_a,
        "Write",
        json!({"path":checkout_b.join("foreign.txt")}),
    );
}

#[test]
fn trusted_semantic_commit_repo_binds_coordination_to_the_claimed_target() {
    let bash_owner_policy = owner_policy().replace(
        "matcher = \"Write|Edit|NotebookEdit|apply_patch\"",
        "matcher = \"Write|Edit|NotebookEdit|apply_patch|Bash\"",
    );
    let (fixture, checkout_a, checkout_b) = foreign_owner_fixture_with(&bash_owner_policy, false);
    let release = fixture.root.join("release");
    fs::create_dir(&release).expect("release directory");
    let agent_hook = release.join("agent-hook");
    fs::copy(env!("CARGO_BIN_EXE_agent-hook"), &agent_hook).expect("copied agent-hook");
    fs::set_permissions(&agent_hook, fs::Permissions::from_mode(0o700)).expect("agent-hook mode");
    let semantic_commit = release.join("semantic-commit");
    fs::write(&semantic_commit, "#!/bin/sh\nexit 0\n").expect("semantic-commit companion");
    fs::set_permissions(&semantic_commit, fs::Permissions::from_mode(0o700))
        .expect("semantic-commit mode");
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"Bash",
        "cwd":checkout_a,
        "tool_input":{"command":format!(
            "{} commit --repo {} --type fix --subject bounded --body-bullet Bounded",
            semantic_commit.display(),
            checkout_b.display()
        )}
    })
    .to_string();
    let mut child = Command::new(&agent_hook);
    child
        .args(["dispatch", "--product", "codex", "--format", "json"])
        .current_dir(&fixture.root)
        .env_clear()
        .env("HOME", &fixture.home)
        .env("PATH", "/usr/bin:/bin")
        .env("XDG_CONFIG_HOME", &fixture.config_home)
        .env("XDG_STATE_HOME", &fixture.state_home)
        .env("AGENT_SESSION_STATE_DIR", &fixture.session_state)
        .env("AGENT_SESSION_ID", "current")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = child.spawn().expect("copied agent-hook spawn");
    child
        .stdin
        .take()
        .expect("agent-hook stdin")
        .write_all(payload.as_bytes())
        .expect("agent-hook input");
    let raw = child.wait_with_output().expect("agent-hook output");
    let output: Value = serde_json::from_slice(&raw.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid agent-hook JSON: {error}; status={} stderr={}",
            raw.status,
            String::from_utf8_lossy(&raw.stderr)
        )
    });
    assert_eq!(
        raw.status.code(),
        Some(1),
        "explicit --repo must bind the claimed target: {output}"
    );
    assert_eq!(output["data"]["action"], "block");
    assert_eq!(output["data"]["reasons"][0]["code"], "owner-active-foreign");
}

#[test]
fn notebook_edit_uses_notebook_path_for_owner_liveness() {
    let (fixture, checkout_a, checkout_b) = same_repository_foreign_owner_fixture();
    assert_foreign_target_blocks(
        &fixture,
        &checkout_a,
        "NotebookEdit",
        json!({"notebook_path":checkout_b.join("foreign.ipynb")}),
    );
}

#[test]
fn symlinked_mutation_targets_resolve_to_the_effective_foreign_owner() {
    let (fixture, checkout_a, checkout_b) = same_repository_foreign_owner_fixture();
    let foreign_file = checkout_b.join("foreign.txt");
    let foreign_notebook = checkout_b.join("foreign.ipynb");
    let foreign_directory = checkout_b.join("foreign-directory");
    fs::write(&foreign_file, "foreign\n").expect("foreign file");
    fs::write(&foreign_notebook, "{}\n").expect("foreign notebook");
    fs::create_dir(&foreign_directory).expect("foreign directory");

    let file_link = checkout_a.join("file-link");
    let notebook_link = checkout_a.join("notebook-link");
    let directory_link = checkout_a.join("directory-link");
    symlink(&foreign_file, &file_link).expect("file symlink");
    symlink(&foreign_notebook, &notebook_link).expect("notebook symlink");
    symlink(&foreign_directory, &directory_link).expect("directory symlink");

    for (tool_name, tool_input) in [
        ("Write", json!({"path":file_link})),
        ("Edit", json!({"file_path":file_link})),
        ("NotebookEdit", json!({"notebook_path":notebook_link})),
        (
            "Write",
            json!({"path":directory_link.join("new-directory/not-yet-created.txt")}),
        ),
        (
            "apply_patch",
            json!({"command":format!(
                "*** Begin Patch\n*** Update File: {}\n@@\n-old\n+new\n*** End Patch",
                file_link.display()
            )}),
        ),
    ] {
        assert_foreign_target_blocks(&fixture, &checkout_a, tool_name, tool_input);
    }
}

#[test]
fn ambiguous_symlink_targets_fail_closed_before_policy_evaluation() {
    let (fixture, checkout_a, _) = same_repository_foreign_owner_fixture();
    let dangling = checkout_a.join("dangling-link");
    let cyclic = checkout_a.join("cyclic-link");
    symlink(checkout_a.join("missing-target"), &dangling).expect("dangling symlink");
    symlink(&cyclic, &cyclic).expect("cyclic symlink");

    for target in [dangling, cyclic] {
        let payload = json!({
            "hook_event_name":"PreToolUse",
            "tool_name":"Write",
            "cwd":checkout_a,
            "tool_input":{"path":target}
        })
        .to_string();
        let output = fixture.run_with_env(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
            &[("AGENT_SESSION_ID", "current")],
        );
        assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
        assert_eq!(
            output.stdout_json()["error"]["code"],
            "provider-target-untrusted"
        );
    }
}

#[test]
fn symlinked_and_malformed_git_markers_fail_closed() {
    let mut outcomes = Vec::new();
    for marker in ["symlink", "malformed"] {
        let fixture = Fixture::new(BLOCK_POLICY);
        let checkout = fixture.root.join("checkout");
        fs::create_dir(&checkout).expect("checkout");
        let git_marker = checkout.join(".git");
        match marker {
            "symlink" => {
                let git_admin = fixture.root.join("git-admin");
                fs::create_dir(&git_admin).expect("git admin");
                symlink(&git_admin, &git_marker).expect("symlinked .git marker");
            }
            "malformed" => {
                fs::write(&git_marker, "not-a-gitdir-marker\n").expect("malformed .git marker")
            }
            _ => unreachable!(),
        }
        let payload = json!({
            "hook_event_name":"PreToolUse",
            "tool_name":"Write",
            "cwd":checkout,
            "tool_input":{"path":checkout.join("target.txt")}
        })
        .to_string();
        let output = fixture.run(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
        );

        outcomes.push((
            marker,
            output.code,
            output.stdout_json()["error"]["code"].clone(),
        ));
    }
    assert_eq!(
        outcomes,
        vec![
            ("symlink", 65, json!("provider-target-untrusted")),
            ("malformed", 65, json!("provider-target-untrusted")),
        ]
    );
}

#[test]
fn apply_patch_checks_single_and_every_multi_file_target() {
    let (fixture, checkout_a, checkout_b) = same_repository_foreign_owner_fixture();
    for patch in [
        format!(
            "*** Begin Patch\n*** Update File: {}\n@@\n-old\n+new\n*** Update File: {}\n@@\n-old\n+new\n*** End Patch",
            checkout_a.join("self.txt").display(),
            checkout_b.join("foreign.txt").display()
        ),
        format!(
            "*** Begin Patch\n*** Update File: {}\n@@\n-old\n+new\n*** End Patch",
            checkout_b.join("foreign.txt").display()
        ),
        format!(
            "*** Begin Patch\n*** Add File: {}\n+new\n*** End Patch",
            checkout_b.join("foreign-add.txt").display()
        ),
        format!(
            "*** Begin Patch\n*** Delete File: {}\n*** End Patch",
            checkout_b.join("foreign-delete.txt").display()
        ),
        format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-old\n+new\n*** End Patch",
            checkout_a.join("self.txt").display(),
            checkout_b.join("foreign-move.txt").display()
        ),
    ] {
        assert_foreign_target_blocks(
            &fixture,
            &checkout_a,
            "apply_patch",
            json!({"command":patch}),
        );
    }
}

#[test]
fn incomplete_apply_patch_target_mapping_fails_closed() {
    let (fixture, checkout_a, _) = same_repository_foreign_owner_fixture();
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"apply_patch",
        "cwd":checkout_a,
        "tool_input":{"command":"*** Begin Patch\n*** Update File:\n@@\n-old\n+new\n*** End Patch"}
    })
    .to_string();
    let output = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[("AGENT_SESSION_ID", "current")],
    );
    assert_eq!(output.code, 65);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "provider-target-untrusted"
    );
}

#[test]
fn undocumented_codex_apply_patch_alias_fails_closed() {
    let (fixture, checkout_a, checkout_b) = same_repository_foreign_owner_fixture();
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"apply_patch",
        "cwd":checkout_a,
        "tool_input":{
            "patch":format!(
                "*** Begin Patch\n*** Update File: {}\n@@\n-old\n+new\n*** End Patch",
                checkout_b.join("foreign.txt").display()
            )
        }
    })
    .to_string();
    let output = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[("AGENT_SESSION_ID", "current")],
    );
    assert_eq!(output.code, 65);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "provider-target-untrusted"
    );
}

fn same_repository_foreign_owner_fixture() -> (Fixture, std::path::PathBuf, std::path::PathBuf) {
    same_repository_foreign_owner_fixture_with(&owner_policy())
}

fn same_repository_foreign_owner_fixture_with(
    policy: &str,
) -> (Fixture, std::path::PathBuf, std::path::PathBuf) {
    foreign_owner_fixture_with(policy, true)
}

fn foreign_owner_fixture_with(
    policy: &str,
    shared_repository_context: bool,
) -> (Fixture, std::path::PathBuf, std::path::PathBuf) {
    let fixture = Fixture::new(policy);
    let checkout_a = fixture.root.join("checkout-a");
    let checkout_b = fixture.root.join("checkout-b");
    for checkout in [&checkout_a, &checkout_b] {
        fs::create_dir_all(checkout).expect("checkout");
        let status = Command::new("git")
            .args(["init", "-q"])
            .arg(checkout)
            .status()
            .expect("git init");
        assert!(status.success());
    }
    let now = now_epoch();
    let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let claims = if shared_repository_context {
        json!([
            {"schema_version":"agent-session.work-context.v1","session_id":"current","session_incarnation":"inc-current","state":"active","worktrees":[fingerprint(key,1,&checkout_a)],"repositories":["owner/repo"],"provider_refs":[],"scopes":[],"expires_at_epoch":now+300},
            {"schema_version":"agent-session.work-context.v1","session_id":"peer","session_incarnation":"inc-peer","state":"active","worktrees":[fingerprint(key,1,&checkout_b)],"repositories":["owner/repo"],"provider_refs":[],"scopes":[],"expires_at_epoch":now+300}
        ])
    } else {
        json!([
            {"schema_version":"agent-session.work-context.v1","session_id":"peer","session_incarnation":"inc-peer","state":"active","worktrees":[fingerprint(key,1,&checkout_b)],"repositories":["owner/peer"],"provider_refs":[],"scopes":[],"expires_at_epoch":now+300}
        ])
    };
    write_registry(
        &fixture,
        key,
        json!({
            "current": {"session_id":"current","incarnation":"inc-current","state":"ready","heartbeat_epoch":now},
            "peer": {"session_id":"peer","incarnation":"inc-peer","state":"ready","heartbeat_epoch":now}
        }),
        claims,
    );
    for (session, incarnation) in [("current", "inc-current"), ("peer", "inc-peer")] {
        let heartbeat = heartbeat_path(&fixture, session);
        fs::create_dir_all(heartbeat.parent().expect("heartbeat parent")).expect("heartbeat dir");
        fs::write(&heartbeat, format!("{incarnation}:{now}\n")).expect("heartbeat");
        fs::set_permissions(&heartbeat, fs::Permissions::from_mode(0o600)).expect("heartbeat mode");
    }
    (fixture, checkout_a, checkout_b)
}

fn assert_foreign_target_blocks(
    fixture: &Fixture,
    execution: &Path,
    tool_name: &str,
    tool_input: Value,
) {
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":tool_name,
        "cwd":execution,
        "tool_input":tool_input
    })
    .to_string();
    let output = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[("AGENT_SESSION_ID", "current")],
    );
    assert_eq!(
        output.code,
        1,
        "tool={tool_name} stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    let decision = output.stdout_json();
    assert_eq!(decision["data"]["action"], "block");
    assert_eq!(
        decision["data"]["reasons"][0]["code"],
        "owner-active-foreign"
    );
}

fn write_registry(fixture: &Fixture, key: &str, brokers: Value, claims: Value) {
    write_registry_at(&fixture.session_state, key, brokers, claims);
}

fn write_registry_at(state_root: &Path, key: &str, brokers: Value, claims: Value) {
    let coordination = state_root.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination dir");
    let path = coordination.join("registry.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "schema_version":"agent-session.coordination-registry.v1",
            "fingerprint_epoch":1,
            "fingerprint_key":key,
            "brokers":brokers,
            "claims":claims
        }))
        .expect("registry JSON"),
    )
    .expect("registry");
    Fixture::set_private(&path);
}

fn heartbeat_path(fixture: &Fixture, session: &str) -> std::path::PathBuf {
    heartbeat_path_at(&fixture.session_state, session)
}

fn heartbeat_path_at(state_root: &Path, session: &str) -> std::path::PathBuf {
    state_root
        .join("sessions")
        .join(session)
        .join("coordination/heartbeat")
}

fn fingerprint(key: &str, epoch: u64, path: &Path) -> String {
    let canonical = fs::canonicalize(path).expect("canonical checkout");
    let hash = hmac_sha256(key.as_bytes(), canonical.as_os_str().as_encoded_bytes());
    format!(
        "hmac-sha256:{epoch}:{}",
        hash.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut normalized = [0_u8; BLOCK];
    if key.len() > BLOCK {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; BLOCK];
    let mut outer_pad = [0x5c_u8; BLOCK];
    for index in 0..BLOCK {
        inner_pad[index] ^= normalized[index];
        outer_pad[index] ^= normalized[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner);
    outer.finalize().into()
}

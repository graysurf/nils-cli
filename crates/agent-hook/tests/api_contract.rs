mod support;

use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::process::Command;

use pretty_assertions::assert_eq;
use serde_json::json;

use support::Fixture;

const ALLOW_POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.allow"
products = ["codex"]
events = ["PreToolUse"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.allow.v1", reason_code = "allowed" }
"#;

#[test]
fn inherited_git_worktree_selection_cannot_make_target_validation_order_dependent() {
    let fixture = Fixture::new(ALLOW_POLICY);
    fs::remove_file(&fixture.config).expect("remove config after fixture setup");
    let agent_hook = fs::canonicalize(env!("CARGO_MANIFEST_DIR")).expect("agent-hook root");
    let project_root = agent_hook
        .parent()
        .and_then(std::path::Path::parent)
        .expect("project root");
    let agent_session = project_root.join("crates/agent-session");
    let git_dir_output = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .expect("git dir lookup");
    assert!(git_dir_output.status.success());
    let git_dir = std::str::from_utf8(&git_dir_output.stdout)
        .expect("git dir UTF-8")
        .trim();
    let git_work_tree = agent_hook.to_string_lossy();
    let targets = [
        agent_hook.join("Cargo.toml"),
        agent_session.join("Cargo.toml"),
    ];
    let mut selection_override = Vec::new();

    for order in [[0, 1], [1, 0]] {
        let payload = two_target_apply_patch_payload(project_root, &targets, order);
        let output = fixture.run_with_env(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
            &[
                ("GIT_DIR", git_dir),
                ("GIT_WORK_TREE", git_work_tree.as_ref()),
            ],
        );
        selection_override.push((
            order,
            output.code,
            output.stdout_json()["error"]["code"].clone(),
        ));
    }

    let mut discovery_ceiling = Vec::new();
    for order in [[0, 1], [1, 0]] {
        let payload = two_target_apply_patch_payload(project_root, &targets, order);
        let output = fixture.run_with_env(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
            &[("GIT_CEILING_DIRECTORIES", git_work_tree.as_ref())],
        );
        discovery_ceiling.push((
            order,
            output.code,
            output.stdout_json()["error"]["code"].clone(),
        ));
    }

    eprintln!(
        "inherited repository selection outcomes: selection_override={selection_override:?} discovery_ceiling={discovery_ceiling:?}"
    );

    assert_eq!(
        (selection_override, discovery_ceiling),
        (
            vec![
                ([0, 1], 65, json!("provider-target-untrusted")),
                ([1, 0], 65, json!("provider-target-untrusted")),
            ],
            vec![
                ([0, 1], 65, json!("provider-target-untrusted")),
                ([1, 0], 65, json!("provider-target-untrusted")),
            ],
        ),
        "repository-selection environment must fail closed independently of target order"
    );
}

#[test]
fn inherited_git_repository_environment_does_not_reject_targetless_events() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "targetless.session-start"
products = ["codex", "claude"]
events = ["SessionStart"]
matcher = "resume"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.allow.v1", reason_code = "session-start-known" }

[[rules]]
id = "targetless.stop-failure"
products = ["claude"]
events = ["StopFailure"]
matcher = "rate_limit"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.allow.v1", reason_code = "rate-limited" }
"#;
    let fixture = Fixture::new(policy);
    let inherited = inherited_git_repository_environment(&fixture.root);
    let mut actual = Vec::new();
    let mut expected = Vec::new();

    for (variable, value) in &inherited {
        for product in ["codex", "claude"] {
            let output = fixture.run_with_env(
                &["dispatch", "--product", product, "--format", "json"],
                Some(r#"{"hook_event_name":"SessionStart","source":"resume"}"#),
                &[(variable, value)],
            );
            let envelope = output.stdout_json();
            actual.push((
                *variable,
                product,
                "SessionStart",
                output.code,
                if output.code == 0 {
                    envelope["data"]["reasons"][0]["code"].clone()
                } else {
                    envelope["error"]["code"].clone()
                },
            ));
            expected.push((
                *variable,
                product,
                "SessionStart",
                0,
                json!("session-start-known"),
            ));
        }

        let output = fixture.run_with_env(
            &["dispatch", "--product", "claude", "--format", "json"],
            Some(r#"{"hook_event_name":"StopFailure","error":"rate_limit"}"#),
            &[(variable, value)],
        );
        let envelope = output.stdout_json();
        actual.push((
            *variable,
            "claude",
            "StopFailure",
            output.code,
            if output.code == 0 {
                envelope["data"]["reasons"][0]["code"].clone()
            } else {
                envelope["error"]["code"].clone()
            },
        ));
        expected.push((*variable, "claude", "StopFailure", 0, json!("rate-limited")));
    }

    eprintln!("targetless inherited Git environment outcomes: {actual:?}");
    assert_eq!(actual, expected);
}

#[test]
fn inherited_git_repository_environment_still_rejects_execution_binding() {
    let fixture = Fixture::new(ALLOW_POLICY);
    let payload = json!({
        "hook_event_name":"SessionStart",
        "source":"resume",
        "cwd":fixture.root,
    })
    .to_string();
    let mut outcomes = Vec::new();

    for (variable, value) in inherited_git_repository_environment(&fixture.root) {
        let output = fixture.run_with_env(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
            &[(variable, value.as_str())],
        );
        outcomes.push((
            variable,
            output.code,
            output.stdout_json()["error"]["code"].clone(),
        ));
    }

    assert_eq!(
        outcomes,
        [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_CEILING_DIRECTORIES",
            "GIT_DISCOVERY_ACROSS_FILESYSTEM",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
        ]
        .map(|variable| (variable, 65, json!("provider-target-untrusted")))
    );
}

fn inherited_git_repository_environment(root: &std::path::Path) -> [(&'static str, String); 7] {
    let git_path = root.join("inherited-git-dir");
    [
        ("GIT_DIR", git_path.to_string_lossy().into_owned()),
        ("GIT_WORK_TREE", root.to_string_lossy().into_owned()),
        ("GIT_COMMON_DIR", git_path.to_string_lossy().into_owned()),
        (
            "GIT_CEILING_DIRECTORIES",
            root.to_string_lossy().into_owned(),
        ),
        ("GIT_DISCOVERY_ACROSS_FILESYSTEM", "0".to_string()),
        (
            "GIT_CONFIG_PARAMETERS",
            "'agent-hook.targetless=true'".to_string(),
        ),
        ("GIT_CONFIG_COUNT", "0".to_string()),
    ]
}

fn two_target_apply_patch_payload(
    project_root: &std::path::Path,
    targets: &[std::path::PathBuf; 2],
    order: [usize; 2],
) -> String {
    let mut patch = String::from("*** Begin Patch\n");
    for index in order {
        patch.push_str(&format!(
            "*** Update File: {}\n@@\n-old\n+new\n",
            targets[index].display()
        ));
    }
    patch.push_str("*** End Patch");
    json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"apply_patch",
        "cwd":project_root,
        "tool_input":{"command":patch}
    })
    .to_string()
}

#[test]
fn workspace_json_envelope_parse_and_unavailable_exit_contract_is_canonical() {
    let fixture = Fixture::new(ALLOW_POLICY);
    let success = fixture.run(&["validate", "--format", "json"], None);
    assert_eq!(success.code, 0);
    let envelope = success.stdout_json();
    assert_eq!(envelope["schema_version"], "cli.agent-hook.validate.v1");
    assert_eq!(envelope["ok"], true);
    assert!(envelope.get("data").is_some());
    assert!(envelope.get("result").is_none());
    assert!(envelope.get("command").is_none());

    for args in [
        vec!["not-a-command", "--format=json"],
        vec!["not-a-command", "--format", "json"],
    ] {
        let parse = fixture.run(&args, None);
        assert_eq!(parse.code, 64);
        let envelope = parse.stdout_json();
        assert_eq!(envelope["schema_version"], "cli.agent-hook.error.v1");
        assert_eq!(envelope["ok"], false);
        assert_eq!(envelope["error"]["code"], "unknown-subcommand");
    }

    for args in [
        vec!["dispatch", "--product", "invalid", "--format=json"],
        vec!["validate", "--format", "json", "--invalid-option"],
    ] {
        let parse = fixture.run(&args, None);
        assert_eq!(parse.code, 64);
        assert_eq!(parse.stdout_json()["error"]["code"], "parse-error");
    }

    let root_help = fixture.run(&[], None);
    assert_eq!(root_help.code, 2);
    assert!(
        root_help.stdout_text().contains("Usage:") || root_help.stderr_text().contains("Usage:")
    );

    let state_root = fixture.state_home.join("agent-hook");
    fs::create_dir_all(&state_root).expect("state root");
    fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700))
        .expect("private state root");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(state_root.join("setup.lock"))
        .expect("setup lock");
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let unavailable = fixture.run(
        &["setup", "--product", "codex", "--apply", "--format", "json"],
        None,
    );
    assert_eq!(unavailable.code, 69);
    assert_eq!(
        unavailable.stdout_json()["error"]["code"],
        "setup-lock-timeout"
    );
}

#[test]
fn codex_native_event_matrix_accepts_only_documented_events() {
    let accepted = [
        "SessionStart",
        "UserPromptSubmit",
        "PermissionRequest",
        "PreToolUse",
        "PostToolUse",
        "PreCompact",
        "PostCompact",
        "SubagentStart",
        "SubagentStop",
        "Stop",
    ];
    let mut policy = String::from(
        "schema_version = \"agent-hook.policy.v1\"\nbundle_id = \"runtime-kit\"\nversion = \"2026.07.20.1\"\n",
    );
    for (index, event) in accepted.iter().enumerate() {
        policy.push_str(&format!(
            "\n[[rules]]\nid = \"codex.event-{index}\"\nproducts = [\"codex\"]\nevents = [\"{event}\"]\npriority = {index}\nmode = \"enforce\"\nfailure_posture = \"closed\"\noverride_class = \"locked\"\ncapability = {{ id = \"decision.allow.v1\", reason_code = \"event-{index}\" }}\n"
        ));
    }
    let fixture = Fixture::new(&policy);
    let validate = fixture.run(&["validate", "--format", "json"], None);
    assert_eq!(validate.code, 0, "stderr={}", validate.stderr_text());
    for event in accepted {
        let payload = json!({"hook_event_name":event,"cwd":fixture.root}).to_string();
        let output = fixture.run(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
        );
        assert_eq!(
            output.code,
            0,
            "event={event} stderr={}",
            output.stderr_text()
        );
    }
    let setup = fixture.run(
        &["setup", "--product", "codex", "--apply", "--format", "json"],
        None,
    );
    assert_eq!(setup.code, 0, "stderr={}", setup.stderr_text());
    let rendered =
        fs::read_to_string(fixture.home.join(".codex/config.toml")).expect("codex config");
    for event in accepted {
        assert!(
            rendered.contains(&format!("[[hooks.{event}]]")),
            "event={event}"
        );
    }

    for rejected in ["PostToolUseFailure", "StopFailure", "Notification"] {
        let payload = json!({"hook_event_name":rejected,"cwd":fixture.root}).to_string();
        let output = fixture.run(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
        );
        assert_eq!(output.code, 65, "event={rejected}");
        assert_eq!(
            output.stdout_json()["error"]["code"],
            "provider-event-unsupported"
        );
    }
}

#[test]
fn subagent_matcher_uses_agent_type_for_codex_and_claude() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.subagent"
products = ["codex", "claude"]
events = ["SubagentStop"]
matcher = "reviewer"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "subagent-block", message = "blocked" }
"#;
    let fixture = Fixture::new(policy);
    for product in ["codex", "claude"] {
        let payload = json!({
            "hook_event_name":"SubagentStop",
            "agent_type":"reviewer",
            "cwd":fixture.root
        })
        .to_string();
        let output = fixture.run(
            &["dispatch", "--product", product, "--format", "json"],
            Some(&payload),
        );
        assert_eq!(
            output.code,
            1,
            "product={product} stderr={}",
            output.stderr_text()
        );
        assert_eq!(output.stdout_json()["data"]["action"], "block");
    }
}

#[test]
fn provider_native_transform_and_dispatch_failure_are_typed_per_product() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.transform"
products = ["codex", "claude"]
events = ["PreToolUse"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.transform.v1", reason_code = "rewrite", replacement = { path = "rewritten.txt", command = "safe" } }

[[rules]]
id = "runtime.block"
products = ["codex", "claude"]
events = ["PermissionRequest", "SubagentStop", "Stop"]
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "native-block", message = "blocked" }
"#;
    let fixture = Fixture::new(policy);
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"Write",
        "cwd":fixture.root,
        "tool_input":{"path":fixture.root.join("target.txt"),"command":"unsafe"}
    })
    .to_string();
    for product in ["codex", "claude"] {
        let transformed = fixture.run(
            &["dispatch", "--product", product, "--format", "provider"],
            Some(&payload),
        );
        assert_eq!(
            transformed.code,
            0,
            "product={product} stderr={}",
            transformed.stderr_text()
        );
        assert_eq!(
            transformed.stdout_json()["hookSpecificOutput"]["updatedInput"],
            json!({"path":"rewritten.txt","command":"safe"})
        );
        assert_eq!(
            transformed.stdout_json()["hookSpecificOutput"]["permissionDecision"],
            "allow"
        );
    }

    for product in ["codex", "claude"] {
        let permission = json!({
            "hook_event_name":"PermissionRequest",
            "tool_name":"Write",
            "cwd":fixture.root,
            "tool_input":{"path":fixture.root.join("target.txt")}
        })
        .to_string();
        let denied = fixture.run(
            &["dispatch", "--product", product, "--format", "provider"],
            Some(&permission),
        );
        assert_eq!(denied.code, 0, "product={product}");
        let native = denied.stdout_json();
        assert_eq!(
            native["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
        assert_eq!(native["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(
            native["hookSpecificOutput"]["decision"]["message"],
            "agent-hook:native-block"
        );
        assert!(
            native["hookSpecificOutput"]
                .get("permissionDecision")
                .is_none()
        );

        for event in ["Stop", "SubagentStop"] {
            let payload = json!({"hook_event_name":event,"cwd":fixture.root}).to_string();
            let blocked = fixture.run(
                &["dispatch", "--product", product, "--format", "provider"],
                Some(&payload),
            );
            assert_eq!(blocked.code, 0, "product={product} event={event}");
            let native = blocked.stdout_json();
            assert_eq!(native["decision"], "block");
            assert_eq!(native["reason"], "agent-hook:native-block");
            assert!(native.get("continue").is_none());
            assert!(native.get("stopReason").is_none());
        }
    }

    let malformed = fixture.run(
        &["dispatch", "--product", "codex", "--format", "provider"],
        Some("{"),
    );
    assert_eq!(malformed.code, 2);
    assert!(malformed.stdout_text().is_empty());
    assert!(malformed.stderr_text().contains("provider-input-invalid"));

    fs::remove_file(&fixture.policy).expect("remove policy");
    for product in ["codex", "claude"] {
        let failed = fixture.run(
            &["dispatch", "--product", product, "--format", "provider"],
            Some(&payload),
        );
        assert_eq!(failed.code, 0, "product={product}");
        assert_eq!(
            failed.stdout_json()["hookSpecificOutput"]["permissionDecision"],
            "deny"
        );
    }
}

#[test]
fn documented_claude_transform_and_elicitation_outputs_are_native() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "claude.permission-transform"
products = ["claude"]
events = ["PermissionRequest"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.transform.v1", reason_code = "permission-rewrite", replacement = { command = "safe" } }

[[rules]]
id = "claude.output-transform"
products = ["claude"]
events = ["PostToolUse"]
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.transform.v1", reason_code = "output-rewrite", replacement = { stdout = "redacted" } }

[[rules]]
id = "claude.elicitation-decline"
products = ["claude"]
events = ["Elicitation", "ElicitationResult"]
priority = 30
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "elicitation-declined", message = "declined" }
"#;
    let fixture = Fixture::new(policy);
    for (event, output_field, expected) in [
        (
            "PermissionRequest",
            "updatedInput",
            json!({"command":"safe"}),
        ),
        (
            "PostToolUse",
            "updatedToolOutput",
            json!({"stdout":"redacted"}),
        ),
    ] {
        let payload = json!({"hook_event_name":event,"cwd":fixture.root}).to_string();
        let output = fixture.run(
            &["dispatch", "--product", "claude", "--format", "provider"],
            Some(&payload),
        );
        assert_eq!(
            output.code,
            0,
            "event={event} stderr={}",
            output.stderr_text()
        );
        let native = output.stdout_json();
        assert_eq!(native["hookSpecificOutput"]["hookEventName"], event);
        if event == "PermissionRequest" {
            assert_eq!(
                native["hookSpecificOutput"]["decision"]["behavior"],
                "allow"
            );
            assert_eq!(
                native["hookSpecificOutput"]["decision"][output_field],
                expected
            );
        } else {
            assert_eq!(native["hookSpecificOutput"][output_field], expected);
        }
    }

    for event in ["Elicitation", "ElicitationResult"] {
        let payload = json!({"hook_event_name":event,"cwd":fixture.root}).to_string();
        let output = fixture.run(
            &["dispatch", "--product", "claude", "--format", "provider"],
            Some(&payload),
        );
        assert_eq!(
            output.code,
            0,
            "event={event} stderr={}",
            output.stderr_text()
        );
        let native = output.stdout_json();
        assert_eq!(native["hookSpecificOutput"]["hookEventName"], event);
        assert_eq!(native["hookSpecificOutput"]["action"], "decline");
        assert!(native.get("continue").is_none());
    }
}

#[test]
fn provider_matchers_use_documented_native_fields_and_reject_unsupported_events() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "codex.pre-compact"
products = ["codex"]
events = ["PreCompact"]
matcher = "manual"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "codex-pre", message = "blocked" }

[[rules]]
id = "codex.post-compact"
products = ["codex"]
events = ["PostCompact"]
matcher = "auto"
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "codex-post", message = "blocked" }

[[rules]]
id = "claude.pre-compact"
products = ["claude"]
events = ["PreCompact"]
matcher = "manual"
priority = 30
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "claude-pre", message = "blocked" }

[[rules]]
id = "claude.elicitation"
products = ["claude"]
events = ["Elicitation", "ElicitationResult"]
matcher = "registry"
priority = 40
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "claude-elicit", message = "blocked" }
"#;
    let fixture = Fixture::new(policy);
    for (product, event, field, value) in [
        ("codex", "PreCompact", "trigger", "manual"),
        ("codex", "PostCompact", "trigger", "auto"),
        ("claude", "PreCompact", "trigger", "manual"),
        ("claude", "Elicitation", "mcp_server_name", "registry"),
        ("claude", "ElicitationResult", "mcp_server_name", "registry"),
    ] {
        let mut payload = json!({"hook_event_name":event,"cwd":fixture.root});
        payload[field] = json!(value);
        if event == "ElicitationResult" {
            payload["elicitation_id"] = json!("request-1");
        }
        let output = fixture.run(
            &["dispatch", "--product", product, "--format", "json"],
            Some(&payload.to_string()),
        );
        assert_eq!(
            output.code,
            1,
            "product={product} event={event} stderr={}",
            output.stderr_text()
        );
        assert_eq!(output.stdout_json()["data"]["action"], "block");
    }

    let unsupported = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "codex.stop-filter"
products = ["codex"]
events = ["Stop"]
matcher = "ignored"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.block.v1", reason_code = "invalid", message = "blocked" }
"#;
    let invalid = Fixture::new(unsupported).run(&["validate", "--format", "json"], None);
    assert_eq!(invalid.code, 65);
    assert_eq!(
        invalid.stdout_json()["error"]["code"],
        "policy-matcher-unsupported"
    );
}

#[test]
fn claude_stop_failure_matcher_uses_native_error_field_only() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "claude.stop-failure"
products = ["claude"]
events = ["StopFailure"]
matcher = "rate_limit"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.allow.v1", reason_code = "rate-limited" }
"#;
    let fixture = Fixture::new(policy);
    let native = fixture.run(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(r#"{"hook_event_name":"StopFailure","error":"rate_limit"}"#),
    );
    assert_eq!(native.code, 0, "stderr={}", native.stderr_text());
    assert_eq!(
        native.stdout_json()["data"]["reasons"][0]["code"],
        "rate-limited"
    );

    let undocumented_alias = fixture.run(
        &["dispatch", "--product", "claude", "--format", "json"],
        Some(r#"{"hook_event_name":"StopFailure","error_type":"rate_limit"}"#),
    );
    assert_eq!(undocumented_alias.code, 0);
    assert_eq!(
        undocumented_alias.stdout_json()["data"]["reasons"],
        json!([])
    );
}

#[test]
fn provider_event_capability_matrix_rejects_unenforceable_actions() {
    let products = [
        (
            "codex",
            &[
                "SessionStart",
                "UserPromptSubmit",
                "PermissionRequest",
                "PreToolUse",
                "PostToolUse",
                "PreCompact",
                "PostCompact",
                "SubagentStart",
                "SubagentStop",
                "Stop",
            ][..],
        ),
        (
            "claude",
            &[
                "SessionStart",
                "UserPromptSubmit",
                "PermissionRequest",
                "PreToolUse",
                "PostToolUse",
                "PostToolUseFailure",
                "PreCompact",
                "SubagentStart",
                "SubagentStop",
                "Stop",
                "StopFailure",
                "Notification",
                "Elicitation",
                "ElicitationResult",
            ][..],
        ),
    ];
    let capabilities = [
        (
            "allow",
            r#"{ id = "decision.allow.v1", reason_code = "allowed" }"#,
        ),
        (
            "warn",
            r#"{ id = "decision.warn.v1", reason_code = "warned", message = "warning" }"#,
        ),
        (
            "block",
            r#"{ id = "decision.block.v1", reason_code = "blocked", message = "blocked" }"#,
        ),
        (
            "context",
            r#"{ id = "decision.context.v1", reason_code = "context", text = "context" }"#,
        ),
        (
            "transform",
            r#"{ id = "decision.transform.v1", reason_code = "transformed", replacement = { value = "replacement" } }"#,
        ),
        (
            "activity",
            r#"{ id = "agent-session.activity.v1", reason_code = "activity" }"#,
        ),
        (
            "owner-liveness",
            r#"{ id = "agent-session.owner-liveness.v1", reason_code = "owner" }"#,
        ),
        (
            "semantic-conflict",
            r#"{ id = "agent-session.semantic-conflict.v1", reason_code = "conflict" }"#,
        ),
        (
            "runtime-handler",
            r#"{ id = "runtime-kit.handler.v1", handler_id = "session-start-healthcheck" }"#,
        ),
    ];
    let mut mismatches = Vec::new();
    for (product, events) in products {
        for event in events {
            for &(capability, capability_toml) in &capabilities {
                let expected = capability_is_compatible(product, event, capability);
                let policy = format!(
                    r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "matrix.rule"
products = ["{product}"]
events = ["{event}"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = {capability_toml}
"#,
                );
                let output = Fixture::new(&policy).run(&["validate", "--format", "json"], None);
                let actual = output.code == 0;
                if actual != expected {
                    mismatches.push(format!(
                        "{product}/{event}/{capability}: expected {expected}, code={} body={}",
                        output.code,
                        output.stdout_text()
                    ));
                } else if !expected {
                    assert_eq!(
                        output.stdout_json()["error"]["code"],
                        "policy-capability-event-unsupported"
                    );
                }
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

fn capability_is_compatible(product: &str, event: &str, capability: &str) -> bool {
    if matches!(capability, "allow" | "activity" | "runtime-handler") {
        return true;
    }
    let context = match product {
        "codex" => matches!(
            event,
            "SessionStart" | "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "SubagentStart"
        ),
        "claude" => matches!(
            event,
            "SessionStart"
                | "UserPromptSubmit"
                | "PreToolUse"
                | "PostToolUse"
                | "PostToolUseFailure"
                | "SubagentStart"
                | "SubagentStop"
                | "Stop"
        ),
        _ => false,
    };
    let block = match product {
        "codex" => !matches!(event, "SubagentStart"),
        "claude" => matches!(
            event,
            "UserPromptSubmit"
                | "PermissionRequest"
                | "PreToolUse"
                | "PostToolUse"
                | "PostToolUseFailure"
                | "PreCompact"
                | "SubagentStop"
                | "Stop"
                | "Elicitation"
                | "ElicitationResult"
        ),
        _ => false,
    };
    let transform = match product {
        "codex" => event == "PreToolUse",
        "claude" => matches!(event, "PreToolUse" | "PermissionRequest" | "PostToolUse"),
        _ => false,
    };
    match capability {
        "warn" | "context" => context,
        "block" => block,
        "transform" => transform,
        "owner-liveness" | "semantic-conflict" => context && block,
        _ => false,
    }
}

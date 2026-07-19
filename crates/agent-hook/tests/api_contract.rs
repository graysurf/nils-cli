mod support;

use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

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

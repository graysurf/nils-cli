mod support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::time::{Duration, Instant};

use pretty_assertions::assert_eq;
use serde_json::json;

use support::{Fixture, now_epoch};

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
fn coordination_irrelevant_dispatch_does_not_read_the_registry() {
    let fixture = Fixture::new(ALLOW_POLICY);
    let coordination = fixture.session_state.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination dir");
    let registry = coordination.join("registry.json");
    fs::write(
        &registry,
        br#"{"schema_version":"agent-session.coordination-registry.v1","fingerprint_epoch":1,"fingerprint_key":"0123456789abcdef0123456789abcdef","brokers":{},"claims":[]}"#,
    )
    .expect("registry");
    Fixture::set_private(&registry);
    let old = libc::timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    // SAFETY: registry is a valid NUL-terminated path and the timespec array is live.
    let path = std::ffi::CString::new(registry.as_os_str().as_encoded_bytes()).expect("path");
    assert_eq!(
        unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), [old, old].as_ptr(), 0) },
        0
    );
    let before = fs::metadata(&registry).expect("metadata").atime();
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"Read",
        "cwd":fixture.root
    })
    .to_string();
    let output = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let after = fs::metadata(&registry).expect("metadata").atime();
    assert_eq!(
        after, before,
        "coordination-irrelevant dispatch read registry.json"
    );
}

#[test]
fn executable_capabilities_share_one_cardinality_and_deadline_budget() {
    let mut policy = String::from(
        "schema_version = \"agent-hook.policy.v1\"\nbundle_id = \"runtime-kit\"\nversion = \"2026.07.20.1\"\n",
    );
    for index in 0..70 {
        policy.push_str(&format!(
            "\n[[rules]]\nid = \"runtime.child-{index}\"\nproducts = [\"codex\"]\nevents = [\"SessionStart\"]\npriority = {index}\nmode = \"enforce\"\nfailure_posture = \"closed\"\noverride_class = \"locked\"\ncapability = {{ id = \"runtime-kit.handler.v1\", handler_id = \"session-start-healthcheck\" }}\n"
        ));
    }
    let fixture = Fixture::new(&policy);
    let hooks = fixture.home.join(".codex/hooks");
    fs::create_dir_all(&hooks).expect("hooks");
    let handler = hooks.join("session-start-healthcheck.sh");
    fs::write(
        &handler,
        b"#!/bin/sh\nprintf 'start\\n' >> \"$AGENT_HOOK_CHILD_LOG\"\nprintf '{}\\n'\n",
    )
    .expect("handler");
    fs::set_permissions(&handler, fs::Permissions::from_mode(0o755)).expect("handler mode");
    let log = fixture.root.join("child.log");
    let log_arg = log.to_string_lossy().into_owned();
    let output = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(r#"{"hook_event_name":"SessionStart","source":"startup"}"#),
        &[("AGENT_HOOK_CHILD_LOG", log_arg.as_str())],
    );
    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "dispatch-child-budget-exceeded"
    );
    let starts = fs::read_to_string(&log).unwrap_or_default().lines().count();
    assert!(
        starts <= 16,
        "started {starts} children after the dispatch budget"
    );
}

#[test]
fn child_deadline_is_dispatch_wide_and_below_provider_timeout() {
    let mut policy = String::from(
        "schema_version = \"agent-hook.policy.v1\"\nbundle_id = \"runtime-kit\"\nversion = \"2026.07.20.1\"\n",
    );
    for index in 0..3 {
        policy.push_str(&format!(
            "\n[[rules]]\nid = \"runtime.slow-{index}\"\nproducts = [\"codex\"]\nevents = [\"SessionStart\"]\npriority = {index}\nmode = \"enforce\"\nfailure_posture = \"closed\"\noverride_class = \"locked\"\ncapability = {{ id = \"runtime-kit.handler.v1\", handler_id = \"session-start-healthcheck\" }}\n"
        ));
    }
    let fixture = Fixture::new(&policy);
    let hooks = fixture.home.join(".codex/hooks");
    fs::create_dir_all(&hooks).expect("hooks");
    let handler = hooks.join("session-start-healthcheck.sh");
    fs::write(&handler, b"#!/bin/sh\nsleep 1\nprintf '{}\\n'\n").expect("handler");
    fs::set_permissions(&handler, fs::Permissions::from_mode(0o755)).expect("handler mode");
    let started = Instant::now();
    let output = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(r#"{"hook_event_name":"SessionStart","source":"startup"}"#),
    );
    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "dispatch-deadline-exceeded"
    );
    assert!(started.elapsed() < Duration::from_millis(2_800));
}

#[test]
fn handler_descendants_cannot_retain_pipes_beyond_the_dispatch_deadline() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.descendant"
products = ["codex"]
events = ["SessionStart"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "runtime-kit.handler.v1", handler_id = "session-start-healthcheck" }
"#;
    let fixture = Fixture::new(policy);
    let hooks = fixture.home.join(".codex/hooks");
    fs::create_dir_all(&hooks).expect("hooks");
    let handler = hooks.join("session-start-healthcheck.sh");
    fs::write(&handler, b"#!/bin/sh\nsleep 30 &\nprintf '{}\\n'\n").expect("handler");
    fs::set_permissions(&handler, fs::Permissions::from_mode(0o755)).expect("handler mode");

    let started = Instant::now();
    let output = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(r#"{"hook_event_name":"SessionStart","source":"startup"}"#),
    );

    assert!(
        matches!(output.code, 0 | 1),
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    assert!(started.elapsed() < Duration::from_millis(2_800));
}

#[test]
fn setup_never_emits_a_provider_candidate_larger_than_its_own_read_limit() {
    let events = [
        "SessionStart",
        "PermissionRequest",
        "PreToolUse",
        "PostToolUse",
        "PreCompact",
        "PostCompact",
        "SubagentStart",
        "SubagentStop",
    ];
    let mut policy = String::from(
        "schema_version = \"agent-hook.policy.v1\"\nbundle_id = \"runtime-kit\"\nversion = \"2026.07.20.1\"\n",
    );
    for index in 0..256 {
        let matcher = (0..8)
            .map(|atom| format!("m{index}-{atom}-{}", "x".repeat(116)))
            .collect::<Vec<_>>()
            .join("|");
        policy.push_str(&format!(
            "\n[[rules]]\nid = \"runtime.large-{index}\"\nproducts = [\"codex\"]\nevents = [{}]\nmatcher = \"{matcher}\"\npriority = {index}\nmode = \"enforce\"\nfailure_posture = \"closed\"\noverride_class = \"locked\"\ncapability = {{ id = \"decision.allow.v1\", reason_code = \"large-{index}\" }}\n",
            events.iter().map(|event| format!("\"{event}\"")).collect::<Vec<_>>().join(", ")
        ));
    }
    assert!(policy.len() < 1024 * 1024);
    let fixture = Fixture::new(&policy);
    let output = fixture.run(
        &["setup", "--product", "codex", "--apply", "--format", "json"],
        None,
    );
    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "provider-config-candidate-too-large"
    );
    assert!(!fixture.home.join(".codex/config.toml").exists());
}

#[test]
fn performance_fixture_clock_is_sane() {
    assert!(now_epoch() > 0);
}

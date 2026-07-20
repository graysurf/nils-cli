mod support;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use serde_json::json;

use support::Fixture;

const POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.session-start"
products = ["codex", "claude"]
events = ["SessionStart"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "runtime-kit.handler.v1", handler_id = "session-start-healthcheck" }

[[rules]]
id = "runtime.pre-edit"
products = ["codex", "claude"]
events = ["PreToolUse"]
matcher = "Write|Edit|NotebookEdit|MultiEdit|apply_patch"
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.allow.v1", reason_code = "pre-edit" }
"#;

const COORDINATION_POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.2"

[[rules]]
id = "runtime.pre-edit"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Write"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.allow.v1", reason_code = "pre-edit" }

[[rules]]
id = "runtime.pre-edit-coordination"
products = ["codex"]
events = ["PreToolUse"]
matcher = "Write"
priority = 20
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "agent-session.coordination.v1", reason_code = "coordination-admitted" }
"#;

#[test]
fn codex_removal_preview_digest_is_fresh_and_restores_from_a_new_setup_preview() {
    let fixture = Fixture::new(POLICY);
    let install_preview = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(
        install_preview.code,
        0,
        "stderr={}",
        install_preview.stderr_text()
    );
    let install_digest = install_preview.stdout_json()["data"]["plan_digest"]
        .as_str()
        .expect("install digest")
        .to_string();
    let installed = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--apply",
            "--expected-plan-digest",
            &install_digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(installed.code, 0, "stderr={}", installed.stderr_text());

    let codex = fixture.home.join(".codex");
    let config = codex.join("config.toml");
    let mut config_bytes = fs::read(&config).expect("installed config");
    config_bytes.extend_from_slice(
        b"\n[[hooks.Stop]]\n\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"keep-user-hook\"\ntimeout = 7\n",
    );
    fs::write(&config, &config_bytes).expect("config with unrelated hook");
    let compatibility = codex.join("hooks.json");
    fs::write(
        &compatibility,
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"agent-hook dispatch --product codex","timeout":60}]}]}}"#,
    )
    .expect("compatibility config");
    Fixture::set_private(&compatibility);

    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--remove",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    let data = &preview.stdout_json()["data"];
    assert_eq!(data["action"], "remove-dry-run");
    assert_eq!(data["changed"], false);
    assert_eq!(data["would_change"], true);
    assert_eq!(data["apply_allowed"], false);
    assert_eq!(fs::read(&config).expect("preview config"), config_bytes);
    assert!(compatibility.is_file());
    let removal_digest = data["plan_digest"]
        .as_str()
        .expect("removal digest")
        .to_string();
    assert_ne!(removal_digest, install_digest);

    config_bytes.extend_from_slice(b"# keep-concurrent-comment\n");
    fs::write(&config, &config_bytes).expect("concurrent provider edit");
    let stale = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--remove",
            "--expected-plan-digest",
            &removal_digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(stale.code, 65, "stderr={}", stale.stderr_text());
    assert_eq!(
        stale.stdout_json()["error"]["code"],
        "setup-plan-digest-mismatch"
    );
    assert_eq!(fs::read(&config).expect("stale config"), config_bytes);
    assert!(compatibility.is_file());

    let fresh = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--remove",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(fresh.code, 0, "stderr={}", fresh.stderr_text());
    let fresh_digest = fresh.stdout_json()["data"]["plan_digest"]
        .as_str()
        .expect("fresh removal digest")
        .to_string();
    assert_ne!(fresh_digest, removal_digest);
    let removed = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--remove",
            "--expected-plan-digest",
            &fresh_digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(removed.code, 0, "stderr={}", removed.stderr_text());
    let removed_config = fs::read_to_string(&config).expect("removed config");
    assert!(removed_config.contains("keep-user-hook"));
    assert!(removed_config.contains("# keep-concurrent-comment"));
    assert!(!removed_config.contains("agent-hook dispatch"));
    assert!(!compatibility.exists());

    let restore_preview = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(
        restore_preview.code,
        0,
        "stderr={}",
        restore_preview.stderr_text()
    );
    let restore_digest = restore_preview.stdout_json()["data"]["plan_digest"]
        .as_str()
        .expect("restore digest")
        .to_string();
    let restored = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--apply",
            "--expected-plan-digest",
            &restore_digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(restored.code, 0, "stderr={}", restored.stderr_text());
    let restored = fs::read_to_string(&config).expect("restored config");
    assert!(restored.contains("keep-user-hook"));
    assert!(restored.contains("agent-hook dispatch --product codex"));
}

#[test]
fn claude_removal_preview_digest_preserves_unrelated_hooks_and_restores() {
    let fixture = Fixture::new(POLICY);
    let installed = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--apply",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(installed.code, 0, "stderr={}", installed.stderr_text());
    let settings = fixture.home.join(".claude/settings.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&settings).expect("settings")).expect("JSON");
    let hooks = value["hooks"].as_object_mut().expect("hooks object");
    hooks
        .get_mut("SessionStart")
        .and_then(serde_json::Value::as_array_mut)
        .expect("session start groups")
        .push(json!({"hooks":[{
            "type":"command",
            "command":"$HOME/.claude/hooks/session-start-healthcheck.sh",
            "timeout":5
        }]}));
    hooks.insert(
        "Stop".to_string(),
        json!([{"hooks":[{"type":"command","command":"keep-user-hook","timeout":7}]}]),
    );
    let original = serde_json::to_vec_pretty(&value).expect("render settings");
    fs::write(&settings, &original).expect("settings with compatibility and unrelated hooks");

    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--remove",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    let data = &preview.stdout_json()["data"];
    assert_eq!(data["action"], "remove-dry-run");
    assert_eq!(data["changed"], false);
    assert_eq!(data["would_change"], true);
    assert_eq!(data["apply_allowed"], false);
    assert_eq!(fs::read(&settings).expect("preview settings"), original);
    let removal_digest = data["plan_digest"]
        .as_str()
        .expect("removal digest")
        .to_string();

    let removed = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--remove",
            "--expected-plan-digest",
            &removal_digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(removed.code, 0, "stderr={}", removed.stderr_text());
    let removed_text = fs::read_to_string(&settings).expect("removed settings");
    assert!(removed_text.contains("keep-user-hook"));
    assert!(!removed_text.contains("agent-hook dispatch"));
    assert!(!removed_text.contains("session-start-healthcheck.sh"));

    let restore_preview = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(
        restore_preview.code,
        0,
        "stderr={}",
        restore_preview.stderr_text()
    );
    let restore_digest = restore_preview.stdout_json()["data"]["plan_digest"]
        .as_str()
        .expect("restore digest")
        .to_string();
    let restored = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--apply",
            "--expected-plan-digest",
            &restore_digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(restored.code, 0, "stderr={}", restored.stderr_text());
    let restored = fs::read_to_string(&settings).expect("restored settings");
    assert!(restored.contains("keep-user-hook"));
    assert!(restored.contains("agent-hook dispatch --product claude"));
}

#[test]
fn codex_setup_preserves_coordination_hook_when_capability_is_absent() {
    let fixture = Fixture::new(POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("codex dir");
    let config = codex.join("config.toml");
    fs::write(
        &config,
        r#"[[hooks.PreToolUse]]
matcher = "Write"

[[hooks.PreToolUse.hooks]]
type = "command"
command = "AGENT_RUNTIME_PRODUCT=codex \"${CODEX_HOME:-$HOME/.codex}/hooks/session-coordination-guard.py\""
timeout = 60
statusMessage = "agent-runtime-kit: Admit managed session mutation"
"#,
    )
    .expect("coordination config");
    Fixture::set_private(&config);
    let original = fs::read(&config).expect("original config");

    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    assert_eq!(preview.stdout_json()["data"]["status"], "unrelated");
    assert_eq!(preview.stdout_json()["data"]["unrelated_count"], 1);
    assert_eq!(fs::read(&config).expect("preview config"), original);

    let applied = fixture.run(
        &["setup", "--product", "codex", "--apply", "--format", "json"],
        None,
    );
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());
    let installed = fs::read_to_string(&config).expect("installed config");
    assert_eq!(
        installed.matches("session-coordination-guard.py").count(),
        1
    );
    assert_eq!(
        installed
            .matches("agent-hook dispatch --product codex")
            .count(),
        2
    );

    let removed = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--remove",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(removed.code, 0, "stderr={}", removed.stderr_text());
    let removed = fs::read_to_string(&config).expect("removed config");
    assert_eq!(removed.matches("session-coordination-guard.py").count(), 1);
    assert!(!removed.contains("agent-hook dispatch"));
}

#[test]
fn codex_setup_migrates_coordination_into_one_exact_owned_ingress() {
    let fixture = Fixture::new(COORDINATION_POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("codex dir");
    let config = codex.join("config.toml");
    fs::write(
        &config,
        r#"# keep-comment
[[hooks.PreToolUse]]
matcher = "Write"

[[hooks.PreToolUse.hooks]]
type = "command"
command = "AGENT_RUNTIME_PRODUCT=codex \"${CODEX_HOME:-$HOME/.codex}/hooks/session-coordination-guard.py\""
timeout = 60
statusMessage = "agent-runtime-kit: Admit managed session mutation"

[[hooks.PreToolUse.hooks]]
type = "command"
command = "keep-user-hook"
timeout = 7
"#,
    )
    .expect("compatibility coordination config");
    Fixture::set_private(&config);

    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    assert_eq!(
        preview.stdout_json()["data"]["status"],
        "compatibility-only"
    );
    assert_eq!(
        preview.stdout_json()["data"]["owned_groups"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    let plan_digest = preview.stdout_json()["data"]["plan_digest"]
        .as_str()
        .expect("plan digest")
        .to_string();

    let applied = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--apply",
            "--expected-plan-digest",
            &plan_digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());
    let installed = fs::read_to_string(&config).expect("installed config");
    assert!(installed.contains("keep-user-hook"));
    assert!(!installed.contains("session-coordination-guard.py"));
    assert_eq!(
        installed
            .matches("agent-hook dispatch --product codex")
            .count(),
        1
    );
    assert!(installed.contains("timeout = 60"));

    let removed = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--remove",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(removed.code, 0, "stderr={}", removed.stderr_text());
    let removed = fs::read_to_string(&config).expect("removed config");
    assert!(removed.contains("keep-user-hook"));
    assert!(!removed.contains("agent-hook dispatch"));
    assert!(!removed.contains("session-coordination-guard.py"));
}

#[test]
fn claude_setup_preserves_unrelated_hooks_and_migrates_grouped_compatibility_handlers() {
    let fixture = Fixture::new(POLICY);
    let claude = fixture.home.join(".claude");
    fs::create_dir_all(&claude).expect("claude dir");
    let settings = claude.join("settings.json");
    fs::write(
        &settings,
        r#"{"keep":"metadata","hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"$HOME/.claude/hooks/session-start-healthcheck.sh","timeout":5}]}],"Stop":[{"hooks":[{"type":"command","command":"keep-user-hook","timeout":7}]}]}}"#,
    )
    .expect("settings");
    Fixture::set_private(&settings);

    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(
        preview.code,
        0,
        "stdout={} stderr={}",
        preview.stdout_text(),
        preview.stderr_text()
    );
    let result = &preview.stdout_json()["data"];
    assert_eq!(result["status"], "compatibility-only");
    assert_eq!(result[concat!("leg", "acy_residue_count")], 1);
    assert_eq!(result["would_change"], true);
    let digest = result["plan_digest"]
        .as_str()
        .expect("plan digest")
        .to_string();

    let blocked = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--apply",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(blocked.code, 65);
    assert_eq!(
        blocked.stdout_json()["error"]["code"],
        "setup-plan-digest-required"
    );

    let applied = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--apply",
            "--expected-plan-digest",
            &digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(
        applied.code,
        0,
        "stdout={} stderr={}",
        applied.stdout_text(),
        applied.stderr_text()
    );
    assert_eq!(applied.stdout_json()["data"]["configured"], true);
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(&settings).expect("settings")).expect("JSON");
    assert_eq!(value["keep"], "metadata");
    let text = value.to_string();
    assert!(text.contains("keep-user-hook"));
    assert!(!text.contains("session-start-healthcheck.sh"));
    assert!(text.contains("agent-hook dispatch --product claude"));
    assert!(text.contains("Write|Edit|NotebookEdit|MultiEdit|apply_patch"));

    let removed = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--remove",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(
        removed.code,
        0,
        "stdout={} stderr={}",
        removed.stdout_text(),
        removed.stderr_text()
    );
    let text = fs::read_to_string(&settings).expect("settings");
    assert!(text.contains("keep-user-hook"));
    assert!(!text.contains("agent-hook dispatch"));
}

#[test]
fn claude_setup_upgrades_the_exact_prior_owned_ingress_without_duplicates() {
    let fixture = Fixture::new(POLICY);
    let claude = fixture.home.join(".claude");
    fs::create_dir_all(&claude).expect("claude dir");
    let settings = claude.join("settings.json");
    fs::write(
        &settings,
        r#"{"keep":"metadata","hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"agent-hook dispatch --product claude","timeout":10}]}],"PreToolUse":[{"matcher":"Write|Edit|NotebookEdit|MultiEdit|apply_patch","hooks":[{"type":"command","command":"agent-hook dispatch --product claude","timeout":10}]}],"Stop":[{"hooks":[{"type":"command","command":"keep-user-hook","timeout":7}]}]}}"#,
    )
    .expect("prior settings");
    Fixture::set_private(&settings);

    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    assert_eq!(preview.stdout_json()["data"]["status"], "drifted");
    assert_eq!(preview.stdout_json()["data"]["owned_count"], 2);
    assert_eq!(preview.stdout_json()["data"]["unrelated_count"], 1);
    let digest = preview.stdout_json()["data"]["plan_digest"]
        .as_str()
        .expect("plan digest")
        .to_string();

    let applied = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--apply",
            "--expected-plan-digest",
            &digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(&settings).expect("settings")).expect("JSON");
    let text = value.to_string();
    assert_eq!(
        text.matches("agent-hook dispatch --product claude").count(),
        2
    );
    assert_eq!(text.matches(r#""timeout":60"#).count(), 2);
    assert!(text.contains("keep-user-hook"));

    let removed = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--remove",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(removed.code, 0, "stderr={}", removed.stderr_text());
    let text = fs::read_to_string(&settings).expect("removed settings");
    assert!(text.contains("keep-user-hook"));
    assert!(!text.contains("agent-hook dispatch"));
}

#[test]
fn codex_setup_preserves_comments_and_renders_session_start() {
    let fixture = Fixture::new(POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("codex dir");
    let config = codex.join("config.toml");
    fs::write(&config, "# keep-comment\nmodel = \"gpt-test\"\n").expect("config");
    Fixture::set_private(&config);
    let applied = fixture.run(
        &["setup", "--product", "codex", "--apply", "--format", "json"],
        None,
    );
    assert_eq!(
        applied.code,
        0,
        "stdout={} stderr={}",
        applied.stdout_text(),
        applied.stderr_text()
    );
    let text = fs::read_to_string(&config).expect("config");
    assert!(text.contains("# keep-comment"));
    assert!(text.contains("[[hooks.SessionStart]]"));
    assert!(text.contains("matcher = \"Write|Edit|NotebookEdit|MultiEdit|apply_patch\""));
    assert_eq!(
        text.matches("agent-hook dispatch --product codex").count(),
        2
    );

    let repeated = fixture.run(
        &["setup", "--product", "codex", "--apply", "--format", "json"],
        None,
    );
    assert_eq!(repeated.code, 0);
    assert_eq!(repeated.stdout_json()["data"]["changed"], false);

    let removed = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--remove",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(removed.code, 0);
    let removed_text = fs::read_to_string(&config).expect("removed config");
    assert!(removed_text.contains("# keep-comment"));
    assert!(removed_text.contains("model = \"gpt-test\""));
    assert!(!removed_text.contains("agent-hook dispatch"));
}

#[test]
fn codex_runtime_kit_trust_boundary_repair_requires_review_and_preserves_tables() {
    let fixture = Fixture::new(POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("codex dir");
    let config = codex.join("config.toml");
    let original = r#"# >>> agent-runtime-kit:hooks >>>
[[hooks.Stop]]

[[hooks.Stop.hooks]]
type = "command"
command = "keep-user-hook"
timeout = 7

[hooks.state]

[hooks.state."config.toml:stop:1:0"]
trusted_hash = "sha256:trusted"

[projects."/foreign/project"]
trust_level = "trusted"
# <<< agent-runtime-kit:hooks <<<
"#;
    fs::write(&config, original).expect("trust-saved config");
    Fixture::set_private(&config);

    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    let preview_result = &preview.stdout_json()["data"];
    assert_eq!(preview_result["status"], "drifted");
    assert_eq!(preview_result["would_change"], true);
    assert_eq!(preview_result["apply_allowed"], false);
    assert_eq!(
        fs::read_to_string(&config).expect("preview retains config"),
        original
    );
    let digest = preview_result["plan_digest"]
        .as_str()
        .expect("plan digest")
        .to_string();

    let unreviewed = fixture.run(
        &["setup", "--product", "codex", "--apply", "--format", "json"],
        None,
    );
    assert_eq!(unreviewed.code, 65);
    assert_eq!(
        unreviewed.stdout_json()["error"]["code"],
        "setup-plan-digest-required"
    );
    assert_eq!(
        fs::read_to_string(&config).expect("unreviewed config retained"),
        original
    );

    let repaired = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--apply",
            "--expected-plan-digest",
            &digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(repaired.code, 0, "stderr={}", repaired.stderr_text());
    let repaired = fs::read_to_string(&config).expect("repaired config");
    let runtime_end = repaired
        .find("# <<< agent-runtime-kit:hooks <<<")
        .expect("runtime-kit closing marker");
    let hooks_state = repaired.find("[hooks.state]").expect("hooks trust table");
    let projects = repaired
        .find("[projects.\"/foreign/project\"]")
        .expect("project trust table");
    assert!(runtime_end < hooks_state);
    assert!(hooks_state < projects);
    assert!(
        repaired
            .contains("[hooks.state.\"config.toml:stop:1:0\"]\ntrusted_hash = \"sha256:trusted\"")
    );
    assert!(repaired.contains("[projects.\"/foreign/project\"]\ntrust_level = \"trusted\""));
    assert!(repaired.contains("command = \"keep-user-hook\""));
    assert!(repaired.contains("agent-hook dispatch --product codex"));

    let repeated = fixture.run(
        &["setup", "--product", "codex", "--apply", "--format", "json"],
        None,
    );
    assert_eq!(repeated.code, 0, "stderr={}", repeated.stderr_text());
    assert_eq!(repeated.stdout_json()["data"]["changed"], false);
}

#[test]
fn codex_runtime_kit_trust_boundary_rejects_ambiguous_markers_without_writes() {
    let runtime_start = "# >>> agent-runtime-kit:hooks >>>";
    let runtime_end = "# <<< agent-runtime-kit:hooks <<<";
    for (name, marker_layout) in [
        ("orphan-start", format!("{runtime_start}\n")),
        ("orphan-end", format!("{runtime_end}\n")),
        (
            "duplicate-start",
            format!("{runtime_start}\n{runtime_start}\n{runtime_end}\n"),
        ),
        (
            "duplicate-end",
            format!("{runtime_start}\n{runtime_end}\n{runtime_end}\n"),
        ),
        ("reversed", format!("{runtime_end}\n{runtime_start}\n")),
    ] {
        let fixture = Fixture::new(POLICY);
        let codex = fixture.home.join(".codex");
        fs::create_dir_all(&codex).expect("codex dir");
        let config = codex.join("config.toml");
        let invalid = format!(
            "{marker_layout}[hooks.state]\n\n[hooks.state.\"config.toml:stop:1:0\"]\ntrusted_hash = \"sha256:trusted\"\n"
        );
        fs::write(&config, &invalid).expect("ambiguous config");
        Fixture::set_private(&config);

        let preview = fixture.run(
            &[
                "setup",
                "--product",
                "codex",
                "--dry-run",
                "--format",
                "json",
            ],
            None,
        );
        assert_eq!(
            preview.code,
            65,
            "case={name} stderr={}",
            preview.stderr_text()
        );
        assert_eq!(
            preview.stdout_json()["error"]["code"],
            "provider-config-invalid",
            "case={name}"
        );
        assert_eq!(
            fs::read_to_string(&config).expect("invalid config retained"),
            invalid,
            "case={name}"
        );
    }
}

#[test]
fn codex_runtime_kit_trust_boundary_rejects_unsafe_suffixes_without_writes() {
    let cases = [
        (
            "noncanonical-project-header",
            "[\"projects\".\"/foreign/project\"]\ntrust_level = \"trusted\"\n",
        ),
        (
            "non-trust-table-after-trust",
            "[projects.\"/foreign/project\"]\ntrust_level = \"trusted\"\n\n[[hooks.Stop]]\n\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"keep-user-hook\"\ntimeout = 7\n",
        ),
    ];
    for (name, suffix) in cases {
        let fixture = Fixture::new(POLICY);
        let codex = fixture.home.join(".codex");
        fs::create_dir_all(&codex).expect("codex dir");
        let config = codex.join("config.toml");
        let invalid = format!(
            "# >>> agent-runtime-kit:hooks >>>\n[[hooks.PreToolUse]]\n\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"keep-user-hook\"\ntimeout = 7\n\n{suffix}# <<< agent-runtime-kit:hooks <<<\n"
        );
        fs::write(&config, &invalid).expect("unsafe trust suffix");
        Fixture::set_private(&config);

        let preview = fixture.run(
            &[
                "setup",
                "--product",
                "codex",
                "--dry-run",
                "--format",
                "json",
            ],
            None,
        );
        assert_eq!(
            preview.code,
            65,
            "case={name} stderr={}",
            preview.stderr_text()
        );
        assert_eq!(
            preview.stdout_json()["error"]["code"],
            "provider-config-invalid",
            "case={name}"
        );
        assert_eq!(
            fs::read_to_string(&config).expect("unsafe config retained"),
            invalid,
            "case={name}"
        );
    }
}

#[test]
fn codex_owned_marker_text_inside_multiline_values_is_unrelated_and_byte_exact() {
    for quotes in ["\"\"\"", "'''"] {
        let fixture = Fixture::new(POLICY);
        let codex = fixture.home.join(".codex");
        fs::create_dir_all(&codex).expect("codex dir");
        let config = codex.join("config.toml");
        let original = format!(
            "note = {quotes}\n# >>> agent-hook:provider-ingress:v1 >>>\n[[hooks.SessionStart]]\n# <<< agent-hook:provider-ingress:v1 <<<\n{quotes}\n"
        );
        fs::write(&config, &original).expect("multiline config");
        Fixture::set_private(&config);

        let preview = fixture.run(
            &[
                "setup",
                "--product",
                "codex",
                "--dry-run",
                "--format",
                "json",
            ],
            None,
        );
        assert_eq!(
            preview.code,
            0,
            "quotes={quotes} stdout={} stderr={}",
            preview.stdout_text(),
            preview.stderr_text()
        );
        assert_eq!(
            fs::read_to_string(&config).expect("preview bytes"),
            original
        );

        let applied = fixture.run(
            &["setup", "--product", "codex", "--apply", "--format", "json"],
            None,
        );
        assert_eq!(applied.code, 0, "quotes={quotes}");
        assert!(
            fs::read_to_string(&config)
                .expect("applied config")
                .contains(&original),
            "multiline user value must remain byte-exact"
        );

        let removed = fixture.run(
            &[
                "setup",
                "--product",
                "codex",
                "--remove",
                "--format",
                "json",
            ],
            None,
        );
        assert_eq!(removed.code, 0, "quotes={quotes}");
        assert_eq!(
            fs::read_to_string(&config).expect("removed config"),
            original
        );
    }
}

#[test]
fn codex_owned_block_nested_in_foreign_manager_requires_review_and_preserves_bytes() {
    let fixture = Fixture::new(POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("codex dir");
    let config = codex.join("config.toml");

    let seeded = fixture.run(
        &["setup", "--product", "codex", "--apply", "--format", "json"],
        None,
    );
    assert_eq!(seeded.code, 0, "stderr={}", seeded.stderr_text());
    let seeded = fs::read_to_string(&config).expect("seeded config");
    let start = seeded
        .find("# >>> agent-hook:provider-ingress:v1 >>>")
        .expect("owned start");
    let end = seeded
        .find("# <<< agent-hook:provider-ingress:v1 <<<")
        .expect("owned end")
        + "# <<< agent-hook:provider-ingress:v1 <<<\n".len();
    let owned = &seeded[start..end];
    let original = format!(
        "# >>> foreign-manager:hooks >>>\n# foreign-owned-metadata\n{owned}# <<< foreign-manager:hooks <<<\n"
    );
    fs::write(&config, &original).expect("foreign managed config");

    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    assert_eq!(preview.stdout_json()["data"]["status"], "drifted");
    assert_eq!(preview.stdout_json()["data"]["apply_allowed"], false);
    assert_eq!(
        fs::read_to_string(&config).expect("preview config"),
        original
    );
    let digest = preview.stdout_json()["data"]["plan_digest"]
        .as_str()
        .expect("plan digest")
        .to_string();

    let unreviewed = fixture.run(
        &["setup", "--product", "codex", "--apply", "--format", "json"],
        None,
    );
    assert_eq!(unreviewed.code, 65);
    assert_eq!(
        unreviewed.stdout_json()["error"]["code"],
        "setup-plan-digest-required"
    );
    assert_eq!(
        fs::read_to_string(&config).expect("blocked config"),
        original
    );

    let repaired = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--apply",
            "--expected-plan-digest",
            &digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(repaired.code, 0, "stderr={}", repaired.stderr_text());
    let repaired = fs::read_to_string(&config).expect("repaired config");
    let foreign_end = repaired
        .find("# <<< foreign-manager:hooks <<<")
        .expect("foreign end");
    let owned_start = repaired
        .find("# >>> agent-hook:provider-ingress:v1 >>>")
        .expect("owned start");
    assert!(foreign_end < owned_start);
    assert!(repaired.contains(
        "# >>> foreign-manager:hooks >>>\n# foreign-owned-metadata\n# <<< foreign-manager:hooks <<<"
    ));

    let ambiguous = original.replace("# <<< foreign-manager:hooks <<<\n", "");
    fs::write(&config, &ambiguous).expect("ambiguous foreign boundary");
    let rejected = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(rejected.code, 65);
    assert_eq!(
        rejected.stdout_json()["error"]["code"],
        "provider-config-invalid"
    );
    assert_eq!(
        fs::read_to_string(&config).expect("ambiguous config retained"),
        ambiguous
    );
}

fn seed_codex_owned_config(fixture: &Fixture) -> (std::path::PathBuf, String) {
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("codex dir");
    let config = codex.join("config.toml");
    let seeded = fixture.run(
        &["setup", "--product", "codex", "--apply", "--format", "json"],
        None,
    );
    assert_eq!(seeded.code, 0, "stderr={}", seeded.stderr_text());
    let seeded = fs::read_to_string(&config).expect("seeded config");
    (config, seeded)
}

#[test]
fn codex_foreign_manager_block_inside_owned_span_fails_closed_for_every_action() {
    for action in ["--dry-run", "--apply", "--repair", "--remove"] {
        let fixture = Fixture::new(POLICY);
        let (config, seeded) = seed_codex_owned_config(&fixture);
        let owned_end = seeded
            .find("# <<< agent-hook:provider-ingress:v1 <<<")
            .expect("owned end");
        let foreign = concat!(
            "# >>> foreign-manager:hooks >>>\n",
            "foreign_metadata = \"must-survive\"\n",
            "# <<< foreign-manager:hooks <<<\n",
        );
        let original = format!("{}{foreign}{}", &seeded[..owned_end], &seeded[owned_end..]);
        fs::write(&config, &original).expect("inverse-contained foreign block");

        let rejected = fixture.run(
            &["setup", "--product", "codex", action, "--format", "json"],
            None,
        );
        assert_eq!(
            rejected.code,
            65,
            "action={action} stdout={} stderr={}",
            rejected.stdout_text(),
            rejected.stderr_text()
        );
        assert_eq!(
            rejected.stdout_json()["error"]["code"],
            "provider-config-invalid",
            "action={action}"
        );
        assert_eq!(
            fs::read_to_string(&config).expect("foreign bytes retained"),
            original,
            "action={action}"
        );
    }
}

#[test]
fn codex_foreign_marker_scan_rejects_later_malformed_ranges_for_every_action() {
    let malformed_layouts = [
        ("orphan", "# >>> later-orphan:hooks >>>\n"),
        (
            "reversed",
            "# <<< later-reversed:hooks <<<\n# >>> later-reversed:hooks >>>\n",
        ),
        (
            "duplicate",
            "# >>> later-duplicate:hooks >>>\n# >>> later-duplicate:hooks >>>\n# <<< later-duplicate:hooks <<<\n",
        ),
    ];
    for (layout_name, malformed) in malformed_layouts {
        for action in ["--dry-run", "--apply", "--repair", "--remove"] {
            let fixture = Fixture::new(POLICY);
            let (config, seeded) = seed_codex_owned_config(&fixture);
            let owned_start = seeded
                .find("# >>> agent-hook:provider-ingress:v1 >>>")
                .expect("owned start");
            let owned_end = seeded
                .find("# <<< agent-hook:provider-ingress:v1 <<<")
                .expect("owned end")
                + "# <<< agent-hook:provider-ingress:v1 <<<\n".len();
            let owned = &seeded[owned_start..owned_end];
            let original = format!(
                "# >>> completed-foreign:hooks >>>\n{owned}# <<< completed-foreign:hooks <<<\n{malformed}"
            );
            fs::write(&config, &original).expect("later malformed foreign markers");

            let rejected = fixture.run(
                &["setup", "--product", "codex", action, "--format", "json"],
                None,
            );
            assert_eq!(
                rejected.code,
                65,
                "layout={layout_name} action={action} stdout={} stderr={}",
                rejected.stdout_text(),
                rejected.stderr_text()
            );
            assert_eq!(
                rejected.stdout_json()["error"]["code"],
                "provider-config-invalid",
                "layout={layout_name} action={action}"
            );
            assert_eq!(
                fs::read_to_string(&config).expect("malformed layout retained"),
                original,
                "layout={layout_name} action={action}"
            );
        }
    }
}

#[test]
fn codex_first_install_rejects_malformed_foreign_markers_for_every_action() {
    let malformed_layouts = [
        ("orphaned", "# >>> orphaned-manager:hooks >>>\n"),
        (
            "reversed",
            "# <<< reversed-manager:hooks <<<\n# >>> reversed-manager:hooks >>>\n",
        ),
        (
            "crossed",
            "# >>> crossed-a:hooks >>>\n# >>> crossed-b:hooks >>>\n# <<< crossed-a:hooks <<<\n# <<< crossed-b:hooks <<<\n",
        ),
        (
            "duplicate",
            "# >>> duplicate-manager:hooks >>>\n# <<< duplicate-manager:hooks <<<\n# >>> duplicate-manager:hooks >>>\n# <<< duplicate-manager:hooks <<<\n",
        ),
        (
            "partial",
            "# >>> partial-a:hooks >>>\n# <<< partial-b:hooks <<<\n",
        ),
    ];
    for (layout_name, malformed) in malformed_layouts {
        for action in ["--dry-run", "--apply", "--repair", "--remove"] {
            let fixture = Fixture::new(POLICY);
            let codex = fixture.home.join(".codex");
            fs::create_dir_all(&codex).expect("codex dir");
            let config = codex.join("config.toml");
            let original = format!("model = \"gpt-test\"\n{malformed}");
            fs::write(&config, &original).expect("unowned malformed foreign layout");
            Fixture::set_private(&config);

            let rejected = fixture.run(
                &["setup", "--product", "codex", action, "--format", "json"],
                None,
            );
            assert_eq!(
                rejected.code,
                65,
                "layout={layout_name} action={action} stdout={} stderr={}",
                rejected.stdout_text(),
                rejected.stderr_text()
            );
            assert_eq!(
                rejected.stdout_json()["error"]["code"],
                "provider-config-invalid",
                "layout={layout_name} action={action}"
            );
            assert_eq!(
                fs::read_to_string(&config).expect("malformed foreign bytes retained"),
                original,
                "layout={layout_name} action={action}"
            );
        }
    }
}

#[test]
fn codex_first_install_preserves_balanced_foreign_block_and_remains_removable() {
    let fixture = Fixture::new(POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("codex dir");
    let config = codex.join("config.toml");
    let foreign = concat!(
        "# >>> balanced-manager:hooks >>>\n",
        "# foreign-owned-metadata\n",
        "# <<< balanced-manager:hooks <<<\n",
    );
    let original = format!("model = \"gpt-test\"\n{foreign}");
    fs::write(&config, &original).expect("balanced foreign block");
    Fixture::set_private(&config);

    let applied = fixture.run(
        &["setup", "--product", "codex", "--apply", "--format", "json"],
        None,
    );
    assert_eq!(
        applied.code,
        0,
        "stdout={} stderr={}",
        applied.stdout_text(),
        applied.stderr_text()
    );
    let installed = fs::read_to_string(&config).expect("installed config");
    assert!(installed.contains(foreign));
    let foreign_end = installed
        .find("# <<< balanced-manager:hooks <<<")
        .expect("foreign end");
    let owned_start = installed
        .find("# >>> agent-hook:provider-ingress:v1 >>>")
        .expect("owned start");
    assert!(foreign_end < owned_start);

    let removed = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--remove",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(
        removed.code,
        0,
        "stdout={} stderr={}",
        removed.stdout_text(),
        removed.stderr_text()
    );
    assert_eq!(
        fs::read_to_string(&config).expect("removed config"),
        original
    );
}

#[test]
fn provider_setup_rejects_duplicate_json_keys_recursively_for_every_action() {
    let fixtures = [
        ("root", br#"{"keep":1,"keep":2}"#.as_slice()),
        (
            "nested",
            br#"{"metadata":{"keep":1,"keep":2}}"#.as_slice(),
        ),
        (
            "hooks-array",
            br#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep-user-hook","command":"shadow-user-hook"}]}]}}"#
                .as_slice(),
        ),
    ];
    for product in ["claude", "codex"] {
        for (fixture_name, duplicate) in fixtures {
            for action in ["--dry-run", "--apply", "--repair", "--remove"] {
                let fixture = Fixture::new(POLICY);
                let provider_home = fixture.home.join(format!(".{product}"));
                fs::create_dir_all(&provider_home).expect("provider dir");
                let config = provider_home.join(if product == "claude" {
                    "settings.json"
                } else {
                    "hooks.json"
                });
                fs::write(&config, duplicate).expect("duplicate JSON");
                Fixture::set_private(&config);

                let rejected = fixture.run(
                    &["setup", "--product", product, action, "--format", "json"],
                    None,
                );
                assert_eq!(
                    rejected.code,
                    65,
                    "product={product} fixture={fixture_name} action={action} stdout={} stderr={}",
                    rejected.stdout_text(),
                    rejected.stderr_text()
                );
                assert_eq!(
                    rejected.stdout_json()["error"]["code"],
                    "provider-config-invalid",
                    "product={product} fixture={fixture_name} action={action}"
                );
                assert_eq!(
                    fs::read(&config).expect("unchanged duplicate JSON"),
                    duplicate,
                    "product={product} fixture={fixture_name} action={action}"
                );
                if product == "codex" {
                    assert!(!provider_home.join("config.toml").exists());
                }
            }
        }
    }
}

#[test]
fn doctor_classifies_missing_compatibility_converged_dual_drifted_and_unsupported() {
    let fixture = Fixture::new(POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("codex dir");
    let config = codex.join("config.toml");

    let missing = fixture.run(&["doctor", "--product", "codex", "--format", "json"], None);
    assert_eq!(missing.code, 0, "stderr={}", missing.stderr_text());
    assert_eq!(missing.stdout_json()["data"][0]["status"], "missing");

    let prior_registration = r#"# keep-comment
[[hooks.Stop]]

[[hooks.Stop.hooks]]
type = "command"
command = "agent-session activity hook --agent codex"
timeout = 5
"#;
    fs::write(&config, prior_registration).expect("compatibility config");
    Fixture::set_private(&config);
    let compatibility_doctor =
        fixture.run(&["doctor", "--product", "codex", "--format", "json"], None);
    assert_eq!(compatibility_doctor.code, 0);
    assert_eq!(
        compatibility_doctor.stdout_json()["data"][0]["status"],
        "compatibility-only"
    );

    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    let digest = preview.stdout_json()["data"]["plan_digest"]
        .as_str()
        .expect("plan digest")
        .to_string();
    let applied = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--apply",
            "--expected-plan-digest",
            &digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());
    let converged = fixture.run(&["doctor", "--product", "codex", "--format", "json"], None);
    assert_eq!(converged.stdout_json()["data"][0]["status"], "converged");

    let mut dual = fs::read_to_string(&config).expect("converged config");
    dual.push_str(prior_registration);
    fs::write(&config, dual).expect("dual config");
    let dual_doctor = fixture.run(&["doctor", "--product", "codex", "--format", "json"], None);
    assert_eq!(dual_doctor.stdout_json()["data"][0]["status"], "dual");

    let drifted = fs::read_to_string(&config).expect("dual config").replacen(
        "timeout = 60",
        "timeout = 59",
        1,
    );
    fs::write(&config, drifted).expect("drifted config");
    let drifted_doctor = fixture.run(&["doctor", "--product", "codex", "--format", "json"], None);
    assert_eq!(drifted_doctor.stdout_json()["data"][0]["status"], "drifted");

    let unsupported = fixture.run(&["doctor", "--product", "hermes", "--format", "json"], None);
    assert_eq!(unsupported.code, 0);
    assert_eq!(
        unsupported.stdout_json()["data"][0]["status"],
        "unsupported"
    );
    assert_eq!(unsupported.stdout_json()["data"][0]["supported"], false);
}

#[test]
fn claude_setup_preserves_group_and_handler_metadata_as_unrelated() {
    let fixture = Fixture::new(POLICY);
    let claude = fixture.home.join(".claude");
    fs::create_dir_all(&claude).expect("claude dir");
    let settings = claude.join("settings.json");
    fs::write(
        &settings,
        r#"{"keep":"root","hooks":{"SessionStart":[{"label":"keep-group","hooks":[{"type":"command","command":"$HOME/.claude/hooks/session-start-healthcheck.sh","timeout":5,"statusMessage":"keep-handler"}]}]}}"#,
    )
    .expect("settings");
    Fixture::set_private(&settings);

    let applied = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--apply",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());
    let first = fs::read_to_string(&settings).expect("applied settings");
    assert!(first.contains("keep-group"));
    assert!(first.contains("keep-handler"));
    assert!(first.contains("session-start-healthcheck.sh"));
    assert_eq!(
        first
            .matches("agent-hook dispatch --product claude")
            .count(),
        2
    );

    let repeated = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--apply",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(repeated.code, 0);
    assert_eq!(repeated.stdout_json()["data"]["changed"], false);
    assert_eq!(
        fs::read_to_string(&settings).expect("steady settings"),
        first
    );

    let removed = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--remove",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(removed.code, 0);
    let final_text = fs::read_to_string(&settings).expect("removed settings");
    assert!(final_text.contains("keep-group"));
    assert!(final_text.contains("keep-handler"));
    assert!(final_text.contains("session-start-healthcheck.sh"));
    assert!(!final_text.contains("agent-hook dispatch"));
}

#[test]
fn setup_fails_closed_for_malformed_symlink_and_unsafe_mode_without_writes() {
    let fixture = Fixture::new(POLICY);
    let claude = fixture.home.join(".claude");
    fs::create_dir_all(&claude).expect("claude dir");
    let settings = claude.join("settings.json");

    let malformed = b"{not-json";
    fs::write(&settings, malformed).expect("malformed settings");
    Fixture::set_private(&settings);
    let rejected = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--apply",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(rejected.code, 65);
    assert_eq!(
        rejected.stdout_json()["error"]["code"],
        "provider-config-invalid"
    );
    assert_eq!(fs::read(&settings).expect("malformed retained"), malformed);

    fs::remove_file(&settings).expect("remove malformed");
    let target = fixture.root.join("user-settings.json");
    fs::write(&target, b"{\"keep\":true}").expect("target");
    Fixture::set_private(&target);
    symlink(&target, &settings).expect("settings symlink");
    let symlink_rejected = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(symlink_rejected.code, 65);
    assert_eq!(
        symlink_rejected.stdout_json()["error"]["code"],
        "provider-config-untrusted"
    );
    assert_eq!(
        fs::read(&target).expect("target retained"),
        b"{\"keep\":true}"
    );

    fs::remove_file(&settings).expect("remove symlink");
    fs::write(&settings, b"{\"keep\":true}").expect("unsafe settings");
    fs::set_permissions(&settings, fs::Permissions::from_mode(0o666)).expect("unsafe mode");
    let mode_rejected = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--apply",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(mode_rejected.code, 65);
    assert_eq!(
        mode_rejected.stdout_json()["error"]["code"],
        "provider-config-untrusted"
    );
    assert_eq!(
        fs::read(&settings).expect("unsafe retained"),
        b"{\"keep\":true}"
    );
}

#[test]
fn codex_setup_rejects_stale_review_and_malformed_owned_markers_without_writes() {
    let fixture = Fixture::new(POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("codex dir");
    let config = codex.join("config.toml");
    let prior_registration = b"[[hooks.Stop]]\n\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"agent-session activity hook --agent codex\"\ntimeout = 5\n";
    fs::write(&config, prior_registration).expect("compatibility config");
    Fixture::set_private(&config);
    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 0);
    let digest = preview.stdout_json()["data"]["plan_digest"]
        .as_str()
        .expect("plan digest")
        .to_string();
    let newer = [
        prior_registration.as_slice(),
        b"# concurrent-review-change\n",
    ]
    .concat();
    fs::write(&config, &newer).expect("newer config");
    let stale = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--apply",
            "--expected-plan-digest",
            &digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(stale.code, 65);
    assert_eq!(
        stale.stdout_json()["error"]["code"],
        "setup-plan-digest-mismatch"
    );
    assert_eq!(fs::read(&config).expect("newer retained"), newer);

    let malformed =
        b"# >>> agent-hook:provider-ingress:v1 >>>\n# >>> agent-hook:provider-ingress:v1 >>>\n";
    fs::write(&config, malformed).expect("malformed markers");
    let rejected = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(rejected.code, 65);
    assert_eq!(
        rejected.stdout_json()["error"]["code"],
        "provider-owned-marker-invalid"
    );
    assert_eq!(fs::read(&config).expect("markers retained"), malformed);
}

#[test]
fn setup_preserves_standard_shape_lookalike_user_hooks_for_every_action() {
    let fixture = Fixture::new(POLICY);

    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("codex dir");
    let codex_config = codex.join("config.toml");
    let codex_lookalike = "wrapper --label session-start-healthcheck.sh";
    fs::write(
        &codex_config,
        format!(
            "[[hooks.Stop]]\n\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"{codex_lookalike}\"\ntimeout = 5\n"
        ),
    )
    .expect("codex config");
    Fixture::set_private(&codex_config);

    for action in ["--apply", "--repair", "--remove"] {
        let output = fixture.run(
            &["setup", "--product", "codex", action, "--format", "json"],
            None,
        );
        assert_eq!(
            output.code,
            0,
            "action={action} stderr={}",
            output.stderr_text()
        );
        assert!(
            fs::read_to_string(&codex_config)
                .expect("codex config")
                .contains(codex_lookalike),
            "action {action} removed a user hook"
        );
    }

    let claude = fixture.home.join(".claude");
    fs::create_dir_all(&claude).expect("claude dir");
    let claude_settings = claude.join("settings.json");
    let lookalikes = json!([
        {"type":"command","command":"wrapper --label session-start-healthcheck.sh","timeout":5},
        {"type":"command","command":"printf 'agent-session activity hook'","timeout":5}
    ]);
    fs::write(
        &claude_settings,
        serde_json::to_vec(&json!({"hooks":{"Stop":[{"hooks":lookalikes.clone()}]}}))
            .expect("settings JSON"),
    )
    .expect("claude settings");
    Fixture::set_private(&claude_settings);

    for action in ["--apply", "--repair", "--remove"] {
        let output = fixture.run(
            &["setup", "--product", "claude", action, "--format", "json"],
            None,
        );
        assert_eq!(
            output.code,
            0,
            "action={action} stderr={}",
            output.stderr_text()
        );
        let settings: serde_json::Value =
            serde_json::from_slice(&fs::read(&claude_settings).expect("claude settings"))
                .expect("settings JSON");
        assert_eq!(settings["hooks"]["Stop"][0]["hooks"], lookalikes);
    }
}

#[test]
fn claude_duplicate_owned_handler_requires_the_reviewed_plan_digest() {
    let fixture = Fixture::new(POLICY);
    let claude = fixture.home.join(".claude");
    fs::create_dir_all(&claude).expect("claude dir");
    let settings = claude.join("settings.json");

    let initial = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--apply",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(initial.code, 0, "stderr={}", initial.stderr_text());
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&settings).expect("settings")).expect("settings JSON");
    let first_handler = value["hooks"]["SessionStart"][0]["hooks"][0].clone();
    value["hooks"]["SessionStart"][0]["hooks"]
        .as_array_mut()
        .expect("handler array")
        .push(first_handler);
    fs::write(
        &settings,
        serde_json::to_vec_pretty(&value).expect("settings JSON"),
    )
    .expect("duplicate settings");
    let duplicate = fs::read(&settings).expect("duplicate bytes");

    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "claude",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    assert_eq!(preview.stdout_json()["data"]["status"], "drifted");
    assert_eq!(preview.stdout_json()["data"]["apply_allowed"], false);

    for action in ["--apply", "--repair"] {
        let rejected = fixture.run(
            &["setup", "--product", "claude", action, "--format", "json"],
            None,
        );
        assert_eq!(rejected.code, 65, "action={action}");
        assert_eq!(
            rejected.stdout_json()["error"]["code"],
            "setup-plan-digest-required"
        );
        assert_eq!(fs::read(&settings).expect("unchanged settings"), duplicate);
    }
}

#[test]
fn codex_json_only_install_migrates_atomically_to_one_managed_representation() {
    let fixture = Fixture::new(POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("codex dir");
    let hooks_json = codex.join("hooks.json");
    fs::write(
        &hooks_json,
        br#"{"keep":"metadata","hooks":{"Stop":[{"hooks":[{"type":"command","command":"agent-session activity hook --agent codex","timeout":5},{"type":"command","command":"keep-user-hook","timeout":7}]}]}}"#,
    )
    .expect("hooks json");
    Fixture::set_private(&hooks_json);

    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    assert_eq!(
        preview.stdout_json()["data"]["status"],
        "compatibility-only"
    );
    assert_eq!(preview.stdout_json()["data"]["apply_allowed"], false);
    let digest = preview.stdout_json()["data"]["plan_digest"]
        .as_str()
        .expect("plan digest")
        .to_string();

    let applied = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--apply",
            "--expected-plan-digest",
            &digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());
    let json_text = fs::read_to_string(&hooks_json).expect("hooks json");
    assert!(json_text.contains("keep-user-hook"));
    assert!(!json_text.contains("agent-session activity hook"));
    let toml_text = fs::read_to_string(codex.join("config.toml")).expect("config toml");
    assert!(toml_text.contains("agent-hook dispatch --product codex"));
    assert!(toml_text.contains(
        "notify = [\"agent-session\", \"activity\", \"notify\", \"--agent\", \"codex\"]"
    ));

    let doctor = fixture.run(&["doctor", "--product", "codex", "--format", "json"], None);
    assert_eq!(doctor.code, 0);
    assert_eq!(doctor.stdout_json()["data"][0]["status"], "converged");

    let removed = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--remove",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(removed.code, 0, "stderr={}", removed.stderr_text());
    assert!(
        fs::read_to_string(&hooks_json)
            .expect("hooks json")
            .contains("keep-user-hook")
    );
    assert!(!codex.join("config.toml").exists());
}

#[test]
fn doctor_reports_unrelated_for_an_unowned_provider_hook_configuration() {
    let fixture = Fixture::new(POLICY);
    let claude = fixture.home.join(".claude");
    fs::create_dir_all(&claude).expect("claude dir");
    let settings = claude.join("settings.json");
    fs::write(
        &settings,
        br#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep-user-hook","timeout":7}]}]}}"#,
    )
    .expect("settings");
    Fixture::set_private(&settings);

    let doctor = fixture.run(&["doctor", "--product", "claude", "--format", "json"], None);
    assert_eq!(doctor.code, 0, "stderr={}", doctor.stderr_text());
    assert_eq!(doctor.stdout_json()["data"][0]["status"], "unrelated");
}

#[test]
fn remove_preserves_unowned_provider_bytes_and_file_presence() {
    let cases = [
        ("codex", "config.toml", b"".as_slice()),
        ("codex", "config.toml", b"model = \"gpt-5\"\n".as_slice()),
        ("claude", "settings.json", b"{}".as_slice()),
        (
            "claude",
            "settings.json",
            br#"{"keep":"minified","hooks":{"Stop":[{"hooks":[{"type":"command","command":"user-hook","timeout":7}]}]}}"#
                .as_slice(),
        ),
    ];
    for (product, filename, original) in cases {
        let fixture = Fixture::new(POLICY);
        let directory = fixture.home.join(if product == "codex" {
            ".codex"
        } else {
            ".claude"
        });
        fs::create_dir_all(&directory).expect("provider directory");
        let path = directory.join(filename);
        fs::write(&path, original).expect("provider config");
        Fixture::set_private(&path);

        let removed = fixture.run(
            &[
                "setup",
                "--product",
                product,
                "--remove",
                "--format",
                "json",
            ],
            None,
        );

        assert_eq!(
            removed.code,
            0,
            "product={product} stderr={}",
            removed.stderr_text()
        );
        assert_eq!(removed.stdout_json()["data"]["changed"], false);
        assert!(path.exists(), "product={product} removed an unowned file");
        assert_eq!(fs::read(&path).expect("preserved bytes"), original);
    }
}

#[test]
fn codex_user_notify_is_composed_and_restored_exactly_on_remove() {
    let fixture = Fixture::new(POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("codex directory");
    let config = codex.join("config.toml");
    let original = b"notify = [\"user-notifier\", \"--flag\"]\nmodel = \"gpt-5\"\n";
    fs::write(&config, original).expect("Codex config");
    Fixture::set_private(&config);
    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    assert_eq!(preview.stdout_json()["data"]["apply_allowed"], false);
    let digest = preview.stdout_json()["data"]["plan_digest"]
        .as_str()
        .expect("plan digest")
        .to_string();
    let applied = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--apply",
            "--expected-plan-digest",
            &digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr_text());
    let composed = fs::read_to_string(&config).expect("composed config");
    assert!(composed.contains("--forward-notify-argv-json"));

    let removed = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--remove",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(removed.code, 0, "stderr={}", removed.stderr_text());
    assert_eq!(fs::read(&config).expect("restored config"), original);
}

fn computer_use_notify_path(home: &Path) -> PathBuf {
    home.join(
        ".codex/computer-use/Codex Computer Use.app/Contents/SharedSupport/SkyComputerUseClient.app/Contents/MacOS/SkyComputerUseClient",
    )
}

fn computer_use_wrapper(base: &[String], previous: &[String]) -> Vec<String> {
    let mut wrapper = base.to_vec();
    wrapper.push("--previous-notify".to_string());
    wrapper.push(serde_json::to_string(previous).expect("previous notify JSON"));
    wrapper
}

fn accumulated_computer_use_chain(base: &[String], pairs: usize) -> Vec<String> {
    let owned = ["agent-session", "activity", "notify", "--agent", "codex"]
        .map(str::to_string)
        .to_vec();
    let mut chain = base.to_vec();
    for _ in 0..pairs {
        let mut composed = owned.clone();
        composed.push("--forward-notify-argv-json".to_string());
        composed.push(serde_json::to_string(&chain).expect("forwarded notify JSON"));
        chain = computer_use_wrapper(base, &composed);
    }
    chain
}

fn write_codex_notify(path: &Path, notify: &[String]) {
    let mut document = toml_edit::DocumentMut::new();
    let mut array = toml_edit::Array::new();
    array.extend(notify.iter().map(String::as_str));
    document["notify"] = toml_edit::value(array);
    fs::write(path, document.to_string()).expect("Codex notify config");
    Fixture::set_private(path);
}

fn read_codex_notify(path: &Path) -> Vec<String> {
    let document = fs::read_to_string(path)
        .expect("Codex config")
        .parse::<toml_edit::DocumentMut>()
        .expect("Codex TOML");
    document["notify"]
        .as_array()
        .expect("notify array")
        .iter()
        .map(|value| value.as_str().expect("notify string").to_string())
        .collect()
}

#[test]
fn codex_computer_use_owned_chain_is_normalized_idempotently() {
    let fixture = Fixture::new(POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("Codex directory");
    let config = codex.join("config.toml");
    let helper = computer_use_notify_path(&fixture.home);
    fs::create_dir_all(helper.parent().expect("Computer Use helper parent"))
        .expect("Computer Use helper directory");
    fs::write(&helper, "#!/usr/bin/env sh\nexit 0\n").expect("Computer Use helper");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))
        .expect("executable Computer Use helper");
    let base = vec![
        helper.to_string_lossy().into_owned(),
        "turn-ended".to_string(),
    ];
    write_codex_notify(&config, &accumulated_computer_use_chain(&base, 2));

    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
    assert_eq!(preview.stdout_json()["data"]["status"], "drifted");
    assert_eq!(preview.stdout_json()["data"]["apply_allowed"], false);
    let digest = preview.stdout_json()["data"]["plan_digest"]
        .as_str()
        .expect("plan digest")
        .to_string();

    let repaired = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--repair",
            "--expected-plan-digest",
            &digest,
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(repaired.code, 0, "stderr={}", repaired.stderr_text());
    assert_eq!(repaired.stdout_json()["data"]["apply_allowed"], true);
    let owned = ["agent-session", "activity", "notify", "--agent", "codex"]
        .map(str::to_string)
        .to_vec();
    assert_eq!(
        read_codex_notify(&config),
        computer_use_wrapper(&base, &owned)
    );

    let repeated = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--repair",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(repeated.code, 0, "stderr={}", repeated.stderr_text());
    assert_eq!(repeated.stdout_json()["data"]["changed"], false);

    let removed = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--remove",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(removed.code, 0, "stderr={}", removed.stderr_text());
    assert_eq!(read_codex_notify(&config), base);
}

#[test]
fn codex_computer_use_owned_remove_does_not_require_a_live_helper() {
    for helper_state in ["missing", "non-executable"] {
        let fixture = Fixture::new(POLICY);
        let codex = fixture.home.join(".codex");
        fs::create_dir_all(&codex).expect("Codex directory");
        let config = codex.join("config.toml");
        let helper = computer_use_notify_path(&fixture.home);
        if helper_state == "non-executable" {
            fs::create_dir_all(helper.parent().expect("Computer Use helper parent"))
                .expect("Computer Use helper directory");
            fs::write(&helper, "#!/usr/bin/env sh\nexit 0\n")
                .expect("non-executable Computer Use helper");
        }
        let base = vec![
            helper.to_string_lossy().into_owned(),
            "turn-ended".to_string(),
        ];
        let owned = ["agent-session", "activity", "notify", "--agent", "codex"]
            .map(str::to_string)
            .to_vec();
        write_codex_notify(&config, &computer_use_wrapper(&base, &owned));

        let removed = fixture.run(
            &[
                "setup",
                "--product",
                "codex",
                "--remove",
                "--format",
                "json",
            ],
            None,
        );
        assert_eq!(
            removed.code,
            0,
            "state={helper_state} stderr={}",
            removed.stderr_text()
        );
        assert_eq!(read_codex_notify(&config), base, "state={helper_state}");
    }
}

#[test]
fn codex_computer_use_owned_audit_rejects_a_symlinked_ancestor() {
    let fixture = Fixture::new(POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("Codex directory");
    let config = codex.join("config.toml");
    let helper = computer_use_notify_path(&fixture.home);
    let external_root = fixture.root.join("external-computer-use");
    let external_helper = external_root.join(
        "Codex Computer Use.app/Contents/SharedSupport/SkyComputerUseClient.app/Contents/MacOS/SkyComputerUseClient",
    );
    fs::create_dir_all(external_helper.parent().expect("external helper parent"))
        .expect("external helper directory");
    fs::write(&external_helper, "#!/usr/bin/env sh\nexit 0\n").expect("external helper");
    fs::set_permissions(&external_helper, fs::Permissions::from_mode(0o755))
        .expect("executable external helper");
    symlink(&external_root, codex.join("computer-use")).expect("Computer Use root symlink");
    let base = vec![
        helper.to_string_lossy().into_owned(),
        "turn-ended".to_string(),
    ];
    let owned = ["agent-session", "activity", "notify", "--agent", "codex"]
        .map(str::to_string)
        .to_vec();
    write_codex_notify(&config, &computer_use_wrapper(&base, &owned));

    let untrusted = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(untrusted.code, 0, "stderr={}", untrusted.stderr_text());
    assert_eq!(untrusted.stdout_json()["data"]["apply_allowed"], false);

    fs::remove_file(codex.join("computer-use")).expect("remove Computer Use root symlink");
    fs::create_dir_all(helper.parent().expect("Computer Use helper parent"))
        .expect("Computer Use helper directory");
    fs::write(&helper, "#!/usr/bin/env sh\nexit 0\n").expect("Computer Use helper");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))
        .expect("executable Computer Use helper");
    let trusted = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(trusted.code, 0, "stderr={}", trusted.stderr_text());
    assert_eq!(trusted.stdout_json()["data"]["apply_allowed"], true);
}

#[test]
fn codex_computer_use_owned_audit_bounds_wrapper_depth_and_size() {
    for (pairs, normalized) in [(4, true), (5, false)] {
        let fixture = Fixture::new(POLICY);
        let codex = fixture.home.join(".codex");
        fs::create_dir_all(&codex).expect("Codex directory");
        let config = codex.join("config.toml");
        let helper = computer_use_notify_path(&fixture.home);
        fs::create_dir_all(helper.parent().expect("Computer Use helper parent"))
            .expect("Computer Use helper directory");
        fs::write(&helper, "#!/usr/bin/env sh\nexit 0\n").expect("Computer Use helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))
            .expect("executable Computer Use helper");
        let base = vec![
            helper.to_string_lossy().into_owned(),
            "turn-ended".to_string(),
        ];
        write_codex_notify(&config, &accumulated_computer_use_chain(&base, pairs));
        let preview = fixture.run(
            &[
                "setup",
                "--product",
                "codex",
                "--dry-run",
                "--format",
                "json",
            ],
            None,
        );
        if !normalized {
            assert_eq!(preview.code, 65, "pairs={pairs}");
            assert_eq!(
                preview.stdout_json()["error"]["code"],
                "provider-notification-config-conflict"
            );
            continue;
        }
        assert_eq!(preview.code, 0, "stderr={}", preview.stderr_text());
        let digest = preview.stdout_json()["data"]["plan_digest"]
            .as_str()
            .expect("plan digest")
            .to_string();
        let repaired = fixture.run(
            &[
                "setup",
                "--product",
                "codex",
                "--repair",
                "--expected-plan-digest",
                &digest,
                "--format",
                "json",
            ],
            None,
        );
        assert_eq!(repaired.code, 0, "stderr={}", repaired.stderr_text());
        let notify = read_codex_notify(&config);
        assert_eq!(notify[0], base[0]);
        assert_eq!(notify.len(), 4);
    }
}

#[test]
fn codex_computer_use_owned_audit_rejects_a_shallow_oversized_chain() {
    let fixture = Fixture::new(POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("Codex directory");
    let config = codex.join("config.toml");
    let helper = computer_use_notify_path(&fixture.home);
    fs::create_dir_all(helper.parent().expect("Computer Use helper parent"))
        .expect("Computer Use helper directory");
    fs::write(&helper, "#!/usr/bin/env sh\nexit 0\n").expect("Computer Use helper");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))
        .expect("executable Computer Use helper");
    let base = vec![
        helper.to_string_lossy().into_owned(),
        "turn-ended".to_string(),
    ];
    let mut owned = ["agent-session", "activity", "notify", "--agent", "codex"]
        .map(str::to_string)
        .to_vec();
    owned.push("--forward-notify-argv-json".to_string());
    let oversized_argument = "x".repeat(17 * 1024);
    owned.push(
        serde_json::to_string(&["user-notifier", oversized_argument.as_str()])
            .expect("oversized forwarded notify JSON"),
    );
    write_codex_notify(&config, &computer_use_wrapper(&base, &owned));
    let before = fs::read(&config).expect("config before preview");

    let preview = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    assert_eq!(preview.code, 65);
    assert_eq!(
        preview.stdout_json()["error"]["code"],
        "provider-notification-config-conflict"
    );
    assert_eq!(fs::read(&config).expect("config after preview"), before);
}

mod support;

use std::fs;

use pretty_assertions::assert_eq;

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

#[test]
fn claude_setup_preserves_unrelated_hooks_and_migrates_grouped_legacy() {
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
    let result = &preview.stdout_json()["result"];
    assert_eq!(result["status"], "legacy");
    assert_eq!(result["legacy_residue_count"], 1);
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
    assert_eq!(applied.stdout_json()["result"]["configured"], true);
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
}

mod support;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

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
        "timeout = 10",
        "timeout = 9",
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

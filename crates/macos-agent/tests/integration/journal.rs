use std::fs;
use std::os::unix::fs::PermissionsExt;

use tempfile::TempDir;

use crate::common;

#[test]
fn summarize_review_and_guarded_replay_are_cli_accessible() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' '{\"success\":true}'\n").expect("fake");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");
    let out_dir = cwd.path().join("journal");
    let options = harness.cmd_options(cwd.path()).with_env(
        "NILS_MACOS_AGENT_PEEKABOO_BIN",
        fake.to_str().expect("fake"),
    );
    let initial = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "exec",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--",
            "see",
            "--json",
        ],
        options.clone(),
    );
    assert_eq!(initial.code, 0, "{}", initial.stderr_text());

    let summary = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "journal",
            "summarize",
            "--out-dir",
            out_dir.to_str().expect("out"),
        ],
        options.clone(),
    );
    assert_eq!(summary.code, 0);
    assert_eq!(summary.stdout_json()["result"]["total_steps"], 1);

    let plan = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "journal",
            "replay-plan",
            "--out-dir",
            out_dir.to_str().expect("out"),
        ],
        options.clone(),
    );
    assert_eq!(plan.code, 0);
    assert_eq!(plan.stdout_json()["result"]["steps"][0]["eligible"], true);

    let replay = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "journal",
            "replay-step",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--step",
            "step-000001",
        ],
        options,
    );
    assert_eq!(replay.code, 0, "{}", replay.stderr_text());
    let steps = fs::read_to_string(out_dir.join("steps.jsonl")).expect("steps");
    assert!(steps.contains("\"parent_id\":\"step-000001\""));
}

#[test]
fn local_family_lists_are_safe_and_replay_eligible() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' '{\"success\":true}'\n").expect("fake");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");

    for (family, subcommand) in [("app", "list"), ("window", "list")] {
        let out_dir = cwd.path().join(format!("{family}-list-journal"));
        let options = harness.cmd_options(cwd.path()).with_env(
            "NILS_MACOS_AGENT_PEEKABOO_BIN",
            fake.to_str().expect("fake"),
        );
        let observed = harness.run_with_options(
            cwd.path(),
            &[
                "--format",
                "json",
                "exec",
                "--out-dir",
                out_dir.to_str().expect("out"),
                "--",
                family,
                subcommand,
            ],
            options.clone(),
        );
        assert_eq!(observed.code, 0, "{}", observed.stderr_text());

        let step: serde_json::Value = serde_json::from_str(
            fs::read_to_string(out_dir.join("steps.jsonl"))
                .expect("steps")
                .lines()
                .next()
                .expect("step"),
        )
        .expect("step json");
        assert_eq!(step["replay_class"], "safe");
        assert_eq!(step["replay_argv"], serde_json::json!([family, subcommand]));

        let plan = harness.run_with_options(
            cwd.path(),
            &[
                "--format",
                "json",
                "journal",
                "replay-plan",
                "--out-dir",
                out_dir.to_str().expect("out"),
            ],
            options,
        );
        assert_eq!(plan.code, 0, "{}", plan.stderr_text());
        assert_eq!(plan.stdout_json()["result"]["steps"][0]["eligible"], true);
    }
}

#[test]
fn seeded_secrets_and_private_paths_do_not_survive_journal_persistence() {
    let root = TempDir::new().expect("root");
    let mut journal = macos_agent::journal::Journal::open(
        root.path(),
        macos_agent::cli::RuntimeMode::App,
        "ssh",
        macos_agent::cli::EvidenceMode::Debug,
        None,
    )
    .expect("journal");
    journal
        .record_step(macos_agent::journal::StepInput {
            parent_id: None,
            intent: Some("inspect /Users/private-person/Library token=seed-canary".into()),
            expected: Some("title seed-private-title disappears".into()),
            argv: vec!["type".into(), "seed-private-value".into()],
            status: macos_agent::journal::StepStatus::Failed,
            failure_class: Some("private error /home/private-person/.ssh/id_ed25519".into()),
            duration_ms: 1,
            retries: 0,
            precondition_refs: vec![],
            postcondition_refs: vec![],
            snapshot_lineage: None,
            artifact_refs: vec![],
        })
        .expect("step");
    journal.close().expect("close");
    let all = fs::read_to_string(root.path().join("steps.jsonl")).expect("steps");
    for forbidden in [
        "seed-private-value",
        "private-person",
        "id_ed25519",
        "seed-canary",
        "seed-private-title",
    ] {
        assert!(!all.contains(forbidden), "leaked {forbidden}: {all}");
    }
}

#[test]
fn debug_exec_artifact_redacts_nested_payloads_and_private_paths_end_to_end() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    fs::write(
        &fake,
        r#"#!/bin/sh
cat <<'JSON'
{"success":true,"data":{"title":"debug-title-canary","text":"debug-text-canary","value":"debug-value-canary","token":"sk-debug-token-canary","user":"private-user-canary","host":"private-host-canary","path":"/Users/private-person/Library/debug-path-canary","nested":[{"clipboard":"debug-clipboard-canary"}]}}
JSON
"#,
    )
    .expect("fake");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");
    let out_dir = cwd.path().join("debug-artifact-journal");
    let options = harness.cmd_options(cwd.path()).with_env(
        "NILS_MACOS_AGENT_PEEKABOO_BIN",
        fake.to_str().expect("fake"),
    );
    let out = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "exec",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--evidence-mode",
            "debug",
            "--",
            "see",
            "--json",
        ],
        options,
    );
    assert_eq!(out.code, 0, "{}", out.stderr_text());
    let index = fs::read_to_string(out_dir.join("artifacts/index.json")).expect("artifact index");
    let index_json: serde_json::Value = serde_json::from_str(&index).expect("index JSON");
    let relative = index_json["artifacts"][0]["relative_path"]
        .as_str()
        .expect("debug artifact path");
    let artifact = fs::read_to_string(out_dir.join(relative)).expect("debug artifact");
    let steps = fs::read_to_string(out_dir.join("steps.jsonl")).expect("steps");
    let persisted = format!("{index}\n{artifact}\n{steps}");
    for forbidden in [
        "debug-title-canary",
        "debug-text-canary",
        "debug-value-canary",
        "sk-debug-token-canary",
        "private-user-canary",
        "private-host-canary",
        "private-person",
        "debug-path-canary",
        "debug-clipboard-canary",
    ] {
        assert!(
            !persisted.contains(forbidden),
            "debug persistence leaked {forbidden}: {persisted}"
        );
    }
    assert!(artifact.contains("<suppressed>"));
}

#[test]
fn replay_refuses_when_the_effective_backend_binary_changes() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let first = cwd.path().join("peekaboo-first");
    let second = cwd.path().join("peekaboo-second");
    for (path, marker) in [(&first, "first"), (&second, "second")] {
        fs::write(
            path,
            format!("#!/bin/sh\nprintf '%s\\n' '{{\"success\":true,\"marker\":\"{marker}\"}}'\n"),
        )
        .expect("fake");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let out_dir = cwd.path().join("backend-bound-journal");
    let first_options = harness.cmd_options(cwd.path()).with_env(
        "NILS_MACOS_AGENT_PEEKABOO_BIN",
        first.to_str().expect("first"),
    );
    let initial = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "exec",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--",
            "see",
            "--json",
        ],
        first_options,
    );
    assert_eq!(initial.code, 0, "{}", initial.stderr_text());

    let second_options = harness.cmd_options(cwd.path()).with_env(
        "NILS_MACOS_AGENT_PEEKABOO_BIN",
        second.to_str().expect("second"),
    );
    let replay = harness.run_with_options(
        cwd.path(),
        &[
            "--error-format",
            "json",
            "journal",
            "replay-step",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--step",
            "step-000001",
        ],
        second_options,
    );
    assert_eq!(replay.code, 78, "{}", replay.stderr_text());
}

#[test]
fn production_exec_records_snapshot_lineage_for_guarded_conditional_replay() {
    let harness = common::MacosAgentHarness::new();
    let cwd = TempDir::new().expect("cwd");
    let fake = cwd.path().join("peekaboo");
    fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' '{\"success\":true}'\n").expect("fake");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");
    let out_dir = cwd.path().join("conditional-journal");
    let options = harness.cmd_options(cwd.path()).with_env(
        "NILS_MACOS_AGENT_PEEKABOO_BIN",
        fake.to_str().expect("fake"),
    );
    let initial = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "exec",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--expected",
            "button toggles",
            "--",
            "click",
            "--snapshot",
            "snapshot-1",
            "--on",
            "B1",
        ],
        options.clone(),
    );
    assert_eq!(initial.code, 0, "{}", initial.stderr_text());
    let step: serde_json::Value = serde_json::from_str(
        fs::read_to_string(out_dir.join("steps.jsonl"))
            .expect("steps")
            .lines()
            .next()
            .expect("step"),
    )
    .expect("step JSON");
    assert_eq!(step["snapshot_lineage"], "snapshot-1");
    assert_eq!(step["replay_class"], "conditional");

    let replay = harness.run_with_options(
        cwd.path(),
        &[
            "--format",
            "json",
            "journal",
            "replay-step",
            "--out-dir",
            out_dir.to_str().expect("out"),
            "--step",
            "step-000001",
            "--confirm-conditional",
            "--current-snapshot",
            "snapshot-1",
            "--expected",
            "button remains toggled",
        ],
        options,
    );
    assert_eq!(replay.code, 0, "{}", replay.stderr_text());
    assert!(
        fs::read_to_string(out_dir.join("steps.jsonl"))
            .expect("steps")
            .contains("\"parent_id\":\"step-000001\"")
    );
}

#[test]
fn journal_append_permission_failure_is_typed_and_never_reports_a_step() {
    let root = TempDir::new().expect("root");
    let mut journal = macos_agent::journal::Journal::open(
        root.path(),
        macos_agent::cli::RuntimeMode::App,
        "local",
        macos_agent::cli::EvidenceMode::Minimal,
        None,
    )
    .expect("journal");
    let steps = root.path().join("steps.jsonl");
    fs::write(&steps, b"").expect("empty step log");
    fs::set_permissions(&steps, fs::Permissions::from_mode(0o400)).expect("read-only step log");
    let result = journal.record_step(macos_agent::journal::StepInput {
        parent_id: None,
        intent: Some("permission fault injection".into()),
        expected: None,
        argv: vec!["see".into()],
        status: macos_agent::journal::StepStatus::Passed,
        failure_class: None,
        duration_ms: 1,
        retries: 0,
        precondition_refs: vec![],
        postcondition_refs: vec![],
        snapshot_lineage: None,
        artifact_refs: vec![],
    });
    fs::set_permissions(&steps, fs::Permissions::from_mode(0o600)).expect("restore permissions");
    let error = result.expect_err("read-only journal must fail closed");
    assert_eq!(error.exit_code(), 74);
    assert!(fs::read(&steps).expect("step log").is_empty());
}

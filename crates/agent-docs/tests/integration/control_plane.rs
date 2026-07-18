use std::fs;
use std::path::Path;

use super::common::TestEnv;

fn catalog() -> &'static str {
    r#"
[[document]]
context = "project-dev"
scope = "project"
path = "DEVELOPMENT.md"
required = true

[path_classes]
production = ["src/**"]
test = ["tests/**"]
docs = ["docs/**", "**/*.md"]
generated = ["build/**"]
unmatched = "unknown"
"#
}

#[test]
fn session_activation_is_scoped_and_rejects_stale_catalog_state() {
    let env = TestEnv::new();
    env.write_project_catalog(catalog())
        .write_project_doc("DEVELOPMENT.md", "# Development\n");
    let state_home = env.project_path("PRIVATE_STATE_HOME_SENTINEL");
    let state = state_home.to_str().unwrap();

    let activate = env.run(&[
        "session",
        "activate",
        "--session-id",
        "session-alpha",
        "--product",
        "codex",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--format",
        "json",
    ]);
    assert_eq!(activate.code, 0, "stderr: {}", activate.stderr);
    let json = activate.json();
    assert_eq!(json["schema_version"], "cli.agent-docs.session.activate.v1");
    assert_eq!(json["data"]["active_intents"][0], "project-dev");

    let status = env.run(&[
        "session",
        "status",
        "--session-id",
        "session-alpha",
        "--product",
        "codex",
        "--state-home",
        state,
        "--format",
        "json",
    ]);
    assert_eq!(status.code, 0, "stderr: {}", status.stderr);
    let status_json = status.json();
    assert_eq!(status_json["data"]["active_intents"][0], "project-dev");
    let record_file = status_json["data"]["record_file"].as_str().unwrap();
    assert!(!Path::new(record_file).is_absolute());
    assert!(!status.stdout.contains("PRIVATE_STATE_HOME_SENTINEL"));
    assert!(!status.stdout.contains(state));
    assert!(!record_file.contains("session-alpha"));
    let persisted = fs::read_to_string(state_home.join(record_file)).unwrap();
    assert!(!persisted.contains("session-alpha"));
    assert!(!persisted.contains(env.project.to_str().unwrap()));

    let verify = env.run(&[
        "session",
        "verify",
        "--session-id",
        "session-alpha",
        "--product",
        "codex",
        "--state-home",
        state,
        "--require-intent",
        "project-dev",
        "--format",
        "json",
    ]);
    assert_eq!(verify.code, 0, "stderr: {}", verify.stderr);
    assert_eq!(verify.json()["data"]["verified"], true);

    env.write_project_doc("DEVELOPMENT.md", "# Development\n\nChanged.\n");
    let stale = env.run(&[
        "session",
        "verify",
        "--session-id",
        "session-alpha",
        "--product",
        "codex",
        "--state-home",
        state,
        "--require-intent",
        "project-dev",
        "--format",
        "json",
    ]);
    assert_eq!(
        stale.code, 65,
        "stdout: {} stderr: {}",
        stale.stdout, stale.stderr
    );
    assert_eq!(stale.json()["error"]["code"], "stale-activation");
}

#[test]
fn hermes_can_record_shared_state_without_claiming_hook_support() {
    let env = TestEnv::new();
    env.write_project_catalog(catalog())
        .write_project_doc("DEVELOPMENT.md", "# Development\n");
    let state_home = env.project_path(".state");
    let out = env.run(&[
        "session",
        "activate",
        "--session-id",
        "session-hermes",
        "--product",
        "hermes",
        "--state-home",
        state_home.to_str().unwrap(),
        "--intent",
        "project-dev",
        "--format",
        "json",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert_eq!(out.json()["data"]["product"], "hermes");
}

#[test]
fn session_records_are_product_isolated_and_context_bound() {
    let env = TestEnv::new();
    env.write_project_catalog(catalog())
        .write_project_doc("DEVELOPMENT.md", "# Development\n");
    let state_home = env.project_path(".state");
    let state = state_home.to_str().unwrap();
    let activate = env.run(&[
        "session",
        "activate",
        "--session-id",
        "session-bound",
        "--product",
        "codex",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--format",
        "json",
    ]);
    assert_eq!(activate.code, 0, "stderr: {}", activate.stderr);

    let claude = env.run(&[
        "session",
        "status",
        "--session-id",
        "session-bound",
        "--product",
        "claude",
        "--state-home",
        state,
        "--format",
        "json",
    ]);
    assert_eq!(claude.code, 65);
    assert_eq!(claude.json()["error"]["code"], "missing-activation");

    let record_file = activate.json()["data"]["record_file"]
        .as_str()
        .unwrap()
        .to_string();
    let record: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(state_home.join(&record_file)).unwrap()).unwrap();
    for (field, replacement) in [
        ("product", "claude"),
        ("session_hash", "sha256:wrong-session"),
        ("project_hash", "sha256:wrong-project"),
    ] {
        let mut corrupted_record = record.clone();
        corrupted_record[field] = serde_json::Value::String(replacement.to_string());
        fs::write(
            state_home.join(&record_file),
            serde_json::to_vec_pretty(&corrupted_record).unwrap(),
        )
        .unwrap();

        let corrupted = env.run(&[
            "session",
            "status",
            "--session-id",
            "session-bound",
            "--product",
            "codex",
            "--state-home",
            state,
            "--format",
            "json",
        ]);
        assert_eq!(
            corrupted.code, 65,
            "field={field} stdout: {}",
            corrupted.stdout
        );
        assert_eq!(
            corrupted.json()["error"]["code"],
            "context-mismatch",
            "field={field}"
        );
    }
}

#[test]
fn concurrent_session_activation_preserves_intents_and_session_scope() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "DEVELOPMENT.md"
required = true

[[document]]
context = "task-tools"
scope = "project"
path = "TOOLS.md"
required = true
"#,
    )
    .write_project_doc("DEVELOPMENT.md", "# Development\n")
    .write_project_doc("TOOLS.md", "# Tools\n");
    let state_home = env.project_path(".state");
    let state = state_home.to_str().unwrap();
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            env.run(&[
                "session",
                "activate",
                "--session-id",
                "concurrent-session",
                "--product",
                "codex",
                "--state-home",
                state,
                "--intent",
                "project-dev",
                "--format",
                "json",
            ])
        });
        let second = scope.spawn(|| {
            env.run(&[
                "session",
                "activate",
                "--session-id",
                "concurrent-session",
                "--product",
                "codex",
                "--state-home",
                state,
                "--intent",
                "task-tools",
                "--format",
                "json",
            ])
        });
        assert_eq!(first.join().unwrap().code, 0);
        assert_eq!(second.join().unwrap().code, 0);
    });

    let status = env.run(&[
        "session",
        "status",
        "--session-id",
        "concurrent-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--format",
        "json",
    ]);
    assert_eq!(status.code, 0, "stderr: {}", status.stderr);
    assert_eq!(
        status.json()["data"]["active_intents"],
        serde_json::json!(["project-dev", "task-tools"])
    );
    let isolated = env.run(&[
        "session",
        "status",
        "--session-id",
        "other-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--format",
        "json",
    ]);
    assert_eq!(isolated.code, 65);
    assert_eq!(isolated.json()["error"]["code"], "missing-activation");
}

#[test]
fn session_activation_reclaims_stale_directory_lock() {
    let env = TestEnv::new();
    env.write_project_catalog(catalog())
        .write_project_doc("DEVELOPMENT.md", "# Development\n");
    let state_home = env.project_path(".state");
    let state = state_home.to_str().unwrap();
    let args = [
        "session",
        "activate",
        "--session-id",
        "stale-lock-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--format",
        "json",
    ];
    let initial = env.run(&args);
    assert_eq!(initial.code, 0, "stderr: {}", initial.stderr);
    let relative_record = initial.json()["data"]["record_file"]
        .as_str()
        .unwrap()
        .to_string();
    let lock = state_home.join(relative_record).with_extension("json.lock");
    fs::create_dir(&lock).unwrap();
    fs::write(
        lock.join("owner.json"),
        r#"{"pid":0,"created_at_unix_seconds":0}"#,
    )
    .unwrap();

    let recovered = env.run(&args);
    assert_eq!(recovered.code, 0, "stderr: {}", recovered.stderr);
    assert!(!lock.exists());
}

#[test]
fn session_prepare_activates_and_reports_stable_result() {
    let env = TestEnv::new();
    env.write_project_catalog(catalog())
        .write_project_doc("DEVELOPMENT.md", "# Development\n");
    let state_home = env.project_path(".state");
    let state = state_home.to_str().unwrap();

    let prepare = env.run(&[
        "session",
        "prepare",
        "--session-id",
        "session-prepare",
        "--product",
        "codex",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--format",
        "json",
    ]);
    assert_eq!(prepare.code, 0, "stderr: {}", prepare.stderr);
    let json = prepare.json();
    assert_eq!(json["schema_version"], "cli.agent-docs.session.prepare.v1");
    assert_eq!(json["data"]["verified"], true);
    assert_eq!(json["data"]["active_intents"][0], "project-dev");
    assert_eq!(json["data"]["prepared_intents"][0], "project-dev");
    assert_eq!(json["data"]["reason"], "prepared");

    // Re-preparing the same intent is idempotent and reported as already-current.
    let again = env.run(&[
        "session",
        "prepare",
        "--session-id",
        "session-prepare",
        "--product",
        "codex",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--format",
        "json",
    ]);
    assert_eq!(again.code, 0, "stderr: {}", again.stderr);
    let again_json = again.json();
    assert_eq!(again_json["data"]["reason"], "already-current");
    assert_eq!(
        again_json["data"]["prepared_intents"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // A prepared intent verifies as active.
    let verify = env.run(&[
        "session",
        "verify",
        "--session-id",
        "session-prepare",
        "--product",
        "codex",
        "--state-home",
        state,
        "--require-intent",
        "project-dev",
        "--format",
        "json",
    ]);
    assert_eq!(verify.code, 0, "stderr: {}", verify.stderr);
    assert_eq!(verify.json()["data"]["verified"], true);

    // An undeclared intent fails closed with a stable reason code.
    let undeclared = env.run(&[
        "session",
        "prepare",
        "--session-id",
        "session-prepare",
        "--product",
        "codex",
        "--state-home",
        state,
        "--intent",
        "browser-test",
        "--format",
        "json",
    ]);
    assert_eq!(undeclared.code, 65, "stderr: {}", undeclared.stderr);
    assert_eq!(undeclared.json()["error"]["code"], "undeclared-intent");
}

fn phase_catalog() -> &'static str {
    r#"
[[document]]
context = "project-dev"
scope = "project"
path = "DEVELOPMENT.md"
required = true

[[document]]
context = "project-dev"
scope = "project"
path = "EDIT.md"
required = true
phase = "edit"

[[document]]
context = "project-dev"
scope = "project"
path = "DELIVERY.md"
required = true
phase = "delivery"
"#
}

#[test]
fn phase_scoped_prepare_and_verify_pass() {
    let env = TestEnv::new();
    // Only the no-phase and edit-phase docs exist; the delivery doc is absent.
    env.write_project_catalog(phase_catalog())
        .write_project_doc("DEVELOPMENT.md", "# Development\n")
        .write_project_doc("EDIT.md", "# Edit\n");
    let state_home = env.project_path(".state");
    let state = state_home.to_str().unwrap();

    let prepare = env.run(&[
        "session",
        "prepare",
        "--session-id",
        "phase-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--phase",
        "edit",
        "--format",
        "json",
    ]);
    assert_eq!(prepare.code, 0, "stderr: {}", prepare.stderr);
    let json = prepare.json();
    assert_eq!(json["data"]["phase"], "edit", "{json}");
    assert_eq!(json["data"]["reason"], "prepared");

    let verify = env.run(&[
        "session",
        "verify",
        "--session-id",
        "phase-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--require-intent",
        "project-dev",
        "--phase",
        "edit",
        "--format",
        "json",
    ]);
    assert_eq!(verify.code, 0, "stderr: {}", verify.stderr);
    assert_eq!(verify.json()["data"]["verified"], true);
}

#[test]
fn full_prepare_covers_a_phase_scoped_verify() {
    let env = TestEnv::new();
    // A full prepare requires every doc across all phases to be present.
    env.write_project_catalog(phase_catalog())
        .write_project_doc("DEVELOPMENT.md", "# Development\n")
        .write_project_doc("EDIT.md", "# Edit\n")
        .write_project_doc("DELIVERY.md", "# Delivery\n");
    let state_home = env.project_path(".state");
    let state = state_home.to_str().unwrap();

    let prepare = env.run(&[
        "session",
        "prepare",
        "--session-id",
        "full-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--format",
        "json",
    ]);
    assert_eq!(prepare.code, 0, "stderr: {}", prepare.stderr);

    // A full (no-phase) prepare covers every phase's subset.
    let verify = env.run(&[
        "session",
        "verify",
        "--session-id",
        "full-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--require-intent",
        "project-dev",
        "--phase",
        "edit",
        "--format",
        "json",
    ]);
    assert_eq!(verify.code, 0, "stderr: {}", verify.stderr);
    assert_eq!(verify.json()["data"]["verified"], true);
}

#[test]
fn phase_prepare_does_not_satisfy_a_different_phase_verify() {
    let env = TestEnv::new();
    env.write_project_catalog(phase_catalog())
        .write_project_doc("DEVELOPMENT.md", "# Development\n")
        .write_project_doc("EDIT.md", "# Edit\n");
    let state_home = env.project_path(".state");
    let state = state_home.to_str().unwrap();

    let prepare = env.run(&[
        "session",
        "prepare",
        "--session-id",
        "cross-phase-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--phase",
        "edit",
        "--format",
        "json",
    ]);
    assert_eq!(prepare.code, 0, "stderr: {}", prepare.stderr);

    let verify = env.run(&[
        "session",
        "verify",
        "--session-id",
        "cross-phase-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--require-intent",
        "project-dev",
        "--phase",
        "delivery",
        "--format",
        "json",
    ]);
    assert_eq!(
        verify.code, 65,
        "verifying an unprepared phase must fail: stdout: {} stderr: {}",
        verify.stdout, verify.stderr
    );
    assert_eq!(verify.json()["error"]["code"], "missing-intent");
}

#[test]
fn phase_prepare_with_missing_phase_doc_is_phase_unsatisfied() {
    let env = TestEnv::new();
    // EDIT.md (required for the edit phase) is missing.
    env.write_project_catalog(phase_catalog())
        .write_project_doc("DEVELOPMENT.md", "# Development\n");
    let state_home = env.project_path(".state");
    let state = state_home.to_str().unwrap();

    let prepare = env.run(&[
        "session",
        "prepare",
        "--session-id",
        "unsatisfied-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--phase",
        "edit",
        "--format",
        "json",
    ]);
    assert_eq!(
        prepare.code, 65,
        "stdout: {} stderr: {}",
        prepare.stdout, prepare.stderr
    );
    assert_eq!(prepare.json()["error"]["code"], "phase-unsatisfied");
}

#[test]
fn no_phase_verify_is_not_satisfied_by_a_phase_only_prepare() {
    let env = TestEnv::new();
    env.write_project_catalog(phase_catalog())
        .write_project_doc("DEVELOPMENT.md", "# Development\n")
        .write_project_doc("EDIT.md", "# Edit\n");
    let state_home = env.project_path(".state");
    let state = state_home.to_str().unwrap();

    let prepare = env.run(&[
        "session",
        "prepare",
        "--session-id",
        "phase-only-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--phase",
        "edit",
        "--format",
        "json",
    ]);
    assert_eq!(prepare.code, 0, "stderr: {}", prepare.stderr);

    // A phase-only prepare must NOT satisfy a no-phase verify: a full verify
    // requires the full (no-phase) activation, which was never written.
    let verify = env.run(&[
        "session",
        "verify",
        "--session-id",
        "phase-only-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--require-intent",
        "project-dev",
        "--format",
        "json",
    ]);
    assert_eq!(
        verify.code, 65,
        "stdout: {} stderr: {}",
        verify.stdout, verify.stderr
    );
    assert_eq!(verify.json()["error"]["code"], "missing-intent");
}

#[test]
fn phase_scoped_verify_detects_stale_activation() {
    let env = TestEnv::new();
    env.write_project_catalog(phase_catalog())
        .write_project_doc("DEVELOPMENT.md", "# Development\n")
        .write_project_doc("EDIT.md", "# Edit\n");
    let state_home = env.project_path(".state");
    let state = state_home.to_str().unwrap();

    let prepare = env.run(&[
        "session",
        "prepare",
        "--session-id",
        "stale-phase-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--phase",
        "edit",
        "--format",
        "json",
    ]);
    assert_eq!(prepare.code, 0, "stderr: {}", prepare.stderr);

    // Mutating the edit-phase required doc invalidates the phase-scoped
    // fingerprint, so a later phase verify must fail closed.
    env.write_project_doc("EDIT.md", "# Edit\n\nChanged.\n");
    let stale = env.run(&[
        "session",
        "verify",
        "--session-id",
        "stale-phase-session",
        "--product",
        "codex",
        "--state-home",
        state,
        "--require-intent",
        "project-dev",
        "--phase",
        "edit",
        "--format",
        "json",
    ]);
    assert_eq!(
        stale.code, 65,
        "stdout: {} stderr: {}",
        stale.stdout, stale.stderr
    );
    assert_eq!(stale.json()["error"]["code"], "stale-activation");
}

#[test]
fn session_rejects_malformed_phase() {
    let env = TestEnv::new();
    env.write_project_catalog(phase_catalog())
        .write_project_doc("DEVELOPMENT.md", "# Development\n")
        .write_project_doc("EDIT.md", "# Edit\n");
    let state_home = env.project_path(".state");
    let state = state_home.to_str().unwrap();

    // Every mutation-capable session subcommand that accepts --phase must reject
    // a malformed value with the same stable, fail-closed error code.
    for (command, intent_flag) in [
        ("prepare", "--intent"),
        ("activate", "--intent"),
        ("verify", "--require-intent"),
    ] {
        let out = env.run(&[
            "session",
            command,
            "--session-id",
            "malformed-phase-session",
            "--product",
            "codex",
            "--state-home",
            state,
            intent_flag,
            "project-dev",
            "--phase",
            "bad phase",
            "--format",
            "json",
        ]);
        assert_eq!(
            out.code, 65,
            "command={command} stdout: {} stderr: {}",
            out.stdout, out.stderr
        );
        assert_eq!(
            out.json()["error"]["code"],
            "invalid-phase",
            "command={command}"
        );
    }
}

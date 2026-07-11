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

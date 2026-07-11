use std::fs;

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
    let state_home = env.project_path(".state");
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
    assert!(!record_file.contains("session-alpha"));
    let persisted = fs::read_to_string(record_file).unwrap();
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

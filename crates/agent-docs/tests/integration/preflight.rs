//! Task 4.1 — `preflight --intent X` emits the resolved doc set, each doc's
//! content, and the per-repo validation contract in a documented, versioned
//! JSON shape (the cross-repo contract the kit pins).

use super::common::TestEnv;

fn env_with_contract() -> TestEnv {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\nmarker = \"## Validation\"\n\n[[validation]]\ncontext = \"project-dev\"\ncommands = [\"cargo test --workspace\", \"cargo clippy --all-targets -- -D warnings\"]\ndescription = \"Run before declaring done.\"\n",
    );
    env.write_project_doc(
        "DEVELOPMENT.md",
        "# Dev\n\n## Validation\n\nrun cargo test\n",
    );
    env
}

#[test]
fn preflight_json_shape_is_versioned_and_complete() {
    let env = env_with_contract();
    let out = env.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();

    assert_eq!(json["schema_version"], "agent-docs.preflight.v1");
    assert_eq!(json["intent"], "project-dev");
    assert!(json["docs_home"].is_string());
    assert!(json["project_path"].is_string());

    let doc = &json["documents"][0];
    assert_eq!(doc["status"], "present");
    assert_eq!(doc["required"], true);
    assert_eq!(doc["when_satisfied"], true);
    assert_eq!(doc["validation"]["valid"], true);
    // Content is emitted so a hook can inject it.
    assert!(
        doc["content"].as_str().unwrap().contains("## Validation"),
        "doc content should be emitted: {}",
        out.stdout
    );

    let contract = &json["validation"];
    assert_eq!(contract["declared"], true);
    let commands = contract["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0], "cargo test --workspace");
    assert_eq!(contract["description"], "Run before declaring done.");

    let summary = &json["summary"];
    assert_eq!(summary["required_total"], 1);
    assert_eq!(summary["satisfied_required"], 1);
}

#[test]
fn preflight_text_summarizes_without_dumping_content() {
    let env = env_with_contract();
    let out = env.run(&["preflight", "--intent", "project-dev"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("PREFLIGHT: intent=project-dev"));
    assert!(out.stdout.contains("validation contract"));
    assert!(out.stdout.contains("cargo test --workspace"));
}

#[test]
fn preflight_unknown_intent_resolves_empty() {
    let env = env_with_contract();
    let out = env.run(&[
        "preflight",
        "--intent",
        "no-such-intent",
        "--format",
        "json",
    ]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    assert_eq!(json["documents"].as_array().unwrap().len(), 0);
    assert_eq!(json["validation"]["declared"], false);
}

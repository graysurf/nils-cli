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

#[test]
fn preflight_require_declared_intent_rejects_unknown_text() {
    let env = env_with_contract();
    let out = env.run(&[
        "preflight",
        "--intent",
        "no-such-intent",
        "--require-declared-intent",
    ]);

    assert_eq!(
        out.code, 65,
        "stdout:\n{}\nstderr:\n{}",
        out.stdout, out.stderr
    );
    assert!(
        out.stdout.trim().is_empty(),
        "text failures should not write stdout: {}",
        out.stdout
    );
    assert!(
        out.stderr
            .contains("error: undeclared intent `no-such-intent`"),
        "stderr should name the undeclared intent: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("available intents: project-dev"),
        "stderr should list available intents: {}",
        out.stderr
    );
}

#[test]
fn preflight_require_declared_intent_rejects_unknown_json() {
    let env = env_with_contract();
    let out = env.run(&[
        "preflight",
        "--intent",
        "no-such-intent",
        "--require-declared-intent",
        "--format",
        "json",
    ]);

    assert_eq!(
        out.code, 65,
        "stdout:\n{}\nstderr:\n{}",
        out.stdout, out.stderr
    );
    assert!(
        out.stderr.trim().is_empty(),
        "json failures should not write stderr: {}",
        out.stderr
    );
    let json = out.json();
    assert_eq!(json["schema_version"], "cli.agent-docs.preflight.v1");
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "undeclared-intent");
    assert_eq!(json["error"]["details"]["intent"], "no-such-intent");
    assert!(
        json["error"]["details"]["available_intents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "project-dev"),
        "available intents should include project-dev: {}",
        out.stdout
    );
}

#[test]
fn preflight_require_declared_intent_accepts_optional_or_skipped_doc_intent() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"optional-tools\"\nscope = \"project\"\npath = \"OPTIONAL.md\"\nrequired = false\nwhen = \"path-exists:missing-marker\"\n",
    );

    let out = env.run(&[
        "preflight",
        "--intent",
        "optional-tools",
        "--require-declared-intent",
        "--format",
        "json",
    ]);

    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    assert_eq!(json["intent"], "optional-tools");
    assert_eq!(json["documents"].as_array().unwrap().len(), 1);
    assert_eq!(json["documents"][0]["required"], false);
    assert_eq!(json["documents"][0]["when_satisfied"], false);
}

#[test]
fn preflight_require_declared_intent_accepts_validation_only_intent() {
    let env = TestEnv::new();
    env.write_project_catalog(
        "[[validation]]\ncontext = \"release-check\"\ncommands = [\"cargo test -p nils-agent-docs\"]\n",
    );

    let out = env.run(&[
        "preflight",
        "--intent",
        "release-check",
        "--require-declared-intent",
        "--format",
        "json",
    ]);

    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    assert_eq!(json["documents"].as_array().unwrap().len(), 0);
    assert_eq!(json["validation"]["declared"], true);
    assert_eq!(
        json["validation"]["commands"][0],
        "cargo test -p nils-agent-docs"
    );
}

#[test]
fn preflight_require_declared_intent_preserves_strict_required_doc_failure() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n",
    );

    let out = env.run(&[
        "preflight",
        "--intent",
        "project-dev",
        "--require-declared-intent",
        "--strict",
    ]);

    assert_eq!(
        out.code, 1,
        "stdout:\n{}\nstderr:\n{}",
        out.stdout, out.stderr
    );
    assert!(
        out.stdout.contains("PREFLIGHT: intent=project-dev"),
        "strict required-doc failures still render the preflight report: {}",
        out.stdout
    );
}

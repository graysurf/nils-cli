//! `explain`, `list`, and `remove` — the catalog inspection/management surface.

use super::common::TestEnv;

fn populated() -> TestEnv {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n\n[[validation]]\ncontext = \"project-dev\"\ncommands = [\"bash scripts/ci/all.sh\"]\n",
    );
    env.write_project_doc("DEVELOPMENT.md", "# Dev\n");
    env
}

#[test]
fn explain_intent_shows_docs_and_contract() {
    let env = populated();
    let out = env.run(&["explain", "--intent", "project-dev"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("INTENT: project-dev"));
    assert!(out.stdout.contains("DEVELOPMENT.md"));
    assert!(out.stdout.contains("validation contract"));
}

#[test]
fn explain_without_intent_lists_intents() {
    let env = populated();
    let out = env.run(&["explain"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    assert!(out.stdout.contains("INTENTS:"));
    assert!(out.stdout.contains("project-dev"));
}

#[test]
fn list_json_has_expected_shape() {
    let env = populated();
    let out = env.run(&["list", "--format", "json"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    assert!(
        json["intents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "project-dev")
    );
    assert_eq!(json["documents"].as_array().unwrap().len(), 1);
    assert_eq!(json["validations"].as_array().unwrap().len(), 1);
}

#[test]
fn list_and_explain_filter_by_product() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"SHARED.md\"\n\n[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"CODEX.md\"\nproduct = \"codex\"\n\n[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"CLAUDE.md\"\nproduct = \"claude\"\n\n[[validation]]\ncontext = \"project-dev\"\ncommands = [\"shared-check\"]\n\n[[validation]]\ncontext = \"project-dev\"\ncommands = [\"codex-check\"]\nproduct = \"codex\"\n\n[[validation]]\ncontext = \"project-dev\"\ncommands = [\"claude-check\"]\nproduct = \"claude\"\n\n[[validation]]\ncontext = \"claude-only\"\ncommands = [\"claude-only-check\"]\nproduct = \"claude\"\n",
    );
    env.write_home_doc("SHARED.md", "# Shared\n");
    env.write_home_doc("CODEX.md", "# Codex\n");
    env.write_home_doc("CLAUDE.md", "# Claude\n");

    let list = env.run(&["list", "--product", "codex", "--format", "json"]);
    assert!(list.success(), "stderr: {}", list.stderr);
    let list_json = list.json();
    let docs = list_json["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 2, "{list_json}");
    assert!(
        docs.iter()
            .any(|doc| doc["path"].as_str().unwrap().ends_with("SHARED.md"))
    );
    assert!(
        docs.iter()
            .any(|doc| doc["path"].as_str().unwrap().ends_with("CODEX.md"))
    );
    assert!(
        !docs
            .iter()
            .any(|doc| doc["path"].as_str().unwrap().ends_with("CLAUDE.md"))
    );
    let commands = &list_json["validations"][0]["commands"];
    assert!(
        commands
            .as_array()
            .unwrap()
            .iter()
            .any(|cmd| cmd == "shared-check")
    );
    assert!(
        commands
            .as_array()
            .unwrap()
            .iter()
            .any(|cmd| cmd == "codex-check")
    );
    assert!(
        !commands
            .as_array()
            .unwrap()
            .iter()
            .any(|cmd| cmd == "claude-check")
    );
    let validation_contexts: Vec<&str> = list_json["validations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|validation| validation["context"].as_str().unwrap())
        .collect();
    assert!(
        validation_contexts.contains(&"project-dev"),
        "{validation_contexts:?}"
    );
    assert!(
        !validation_contexts.contains(&"claude-only"),
        "{validation_contexts:?}"
    );
    let list_intents: Vec<&str> = list_json["intents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|intent| intent.as_str().unwrap())
        .collect();
    assert!(list_intents.contains(&"project-dev"), "{list_intents:?}");
    assert!(!list_intents.contains(&"claude-only"), "{list_intents:?}");

    let explain_intents = env.run(&["explain", "--product", "codex", "--format", "json"]);
    assert!(
        explain_intents.success(),
        "stderr: {}",
        explain_intents.stderr
    );
    let explain_intents_json = explain_intents.json();
    let explain_intent_names: Vec<&str> = explain_intents_json["intents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|intent| intent.as_str().unwrap())
        .collect();
    assert!(
        explain_intent_names.contains(&"project-dev"),
        "{explain_intent_names:?}"
    );
    assert!(
        !explain_intent_names.contains(&"claude-only"),
        "{explain_intent_names:?}"
    );

    let explain = env.run(&[
        "explain",
        "--intent",
        "project-dev",
        "--product",
        "claude",
        "--format",
        "json",
    ]);
    assert!(explain.success(), "stderr: {}", explain.stderr);
    let explain_json = explain.json();
    let explain_docs = explain_json["documents"].as_array().unwrap();
    assert_eq!(explain_docs.len(), 2, "{explain_json}");
    assert!(
        explain_docs
            .iter()
            .any(|doc| doc["path"].as_str().unwrap().ends_with("CLAUDE.md"))
    );
    assert!(
        !explain_docs
            .iter()
            .any(|doc| doc["path"].as_str().unwrap().ends_with("CODEX.md"))
    );
    let explain_commands = explain_json["validation"]["commands"].as_array().unwrap();
    assert!(explain_commands.iter().any(|cmd| cmd == "shared-check"));
    assert!(explain_commands.iter().any(|cmd| cmd == "claude-check"));
    assert!(!explain_commands.iter().any(|cmd| cmd == "codex-check"));
}

#[test]
fn remove_deletes_a_project_entry() {
    let env = TestEnv::new();
    env.write_project_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"EXTRA.md\"\nrequired = true\n\n[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"KEEP.md\"\n",
    );
    let out = env.run(&[
        "remove",
        "--context",
        "project-dev",
        "--scope",
        "project",
        "--path",
        "EXTRA.md",
        "--format",
        "json",
    ]);
    assert!(out.success(), "stderr: {}", out.stderr);
    assert_eq!(out.json()["outcome"], "removed");
    assert_eq!(out.json()["remaining_documents"], 1);

    let body = std::fs::read_to_string(env.project_path("AGENT_DOCS.toml")).unwrap();
    assert!(
        !body.contains("EXTRA.md"),
        "EXTRA.md should be gone:\n{body}"
    );
    assert!(body.contains("KEEP.md"), "KEEP.md should remain:\n{body}");

    // Removing again reports not-found.
    let again = env.run(&[
        "remove",
        "--context",
        "project-dev",
        "--scope",
        "project",
        "--path",
        "EXTRA.md",
        "--format",
        "json",
    ]);
    assert_eq!(again.json()["outcome"], "not-found");
}

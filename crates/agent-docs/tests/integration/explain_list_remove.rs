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

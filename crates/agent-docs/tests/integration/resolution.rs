//! Task 1.2 — resolution is driven entirely by the catalog. With no catalog
//! there are no required docs (no hardcoded builtins), and duplicate resolved
//! paths are de-duplicated.

use super::common::TestEnv;

#[test]
fn no_catalog_means_no_required_docs() {
    // Nothing declared anywhere: the old hardcoded builtins (AGENTS.md,
    // DEVELOPMENT.md, cli-tools.md) must NOT appear.
    let env = TestEnv::new();
    let out = env.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    assert_eq!(
        json["documents"].as_array().expect("documents").len(),
        0,
        "no catalog should resolve zero documents: {}",
        out.stdout
    );
    assert_eq!(json["summary"]["required_total"], 0);
    // Strict passes because nothing is required.
    let strict = env.run(&["preflight", "--intent", "project-dev", "--strict"]);
    assert!(strict.success(), "stderr: {}", strict.stderr);
}

#[test]
fn declared_required_doc_is_resolved_from_catalog() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n",
    );
    env.write_project_doc("DEVELOPMENT.md", "# Dev\n\nsteps\n");
    let out = env.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    let docs = json["documents"].as_array().expect("documents");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["status"], "present");
    assert_eq!(docs[0]["required"], true);
    assert_eq!(json["summary"]["satisfied_required"], 1);
}

#[test]
fn missing_required_doc_fails_strict() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n",
    );
    // DEVELOPMENT.md not created.
    let out = env.run(&["preflight", "--intent", "project-dev", "--strict"]);
    assert_eq!(
        out.code, 1,
        "stdout:\n{}\nstderr:\n{}",
        out.stdout, out.stderr
    );
}

#[test]
fn duplicate_resolved_paths_are_deduped() {
    // The same document declared in both the home default and the project
    // override resolves to the same path and must be listed once.
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n",
    );
    env.write_project_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\nnotes = \"project override\"\n",
    );
    env.write_project_doc("DEVELOPMENT.md", "# Dev\n");
    let out = env.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    let docs = json["documents"].as_array().expect("documents");
    assert_eq!(
        docs.len(),
        1,
        "duplicate path should be deduped: {}",
        out.stdout
    );
    // Project override wins (its notes/source survive dedupe).
    assert_eq!(docs[0]["source"], "project");
}

#[test]
fn intent_scopes_documents() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n\n[[document]]\ncontext = \"task-tools\"\nscope = \"home\"\npath = \"cli-tools.md\"\nrequired = true\n",
    );
    env.write_project_doc("DEVELOPMENT.md", "# Dev\n");
    env.write_home_doc("cli-tools.md", "# Tools\n");
    let pd = env.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    assert_eq!(pd.json()["documents"].as_array().unwrap().len(), 1);
    let tt = env.run(&["preflight", "--intent", "task-tools", "--format", "json"]);
    assert_eq!(tt.json()["documents"].as_array().unwrap().len(), 1);
    assert_eq!(tt.json()["documents"][0]["context"], "task-tools");
}

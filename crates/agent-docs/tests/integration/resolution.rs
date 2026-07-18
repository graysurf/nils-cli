//! Task 1.2 — resolution is driven entirely by the catalog. With no catalog
//! there are no required docs (no hardcoded builtins), and duplicate resolved
//! paths are de-duplicated.

use std::fs;

use nils_test_support::{cmd, git as test_git};
use tempfile::TempDir;

use super::common::{TestEnv, run_cli};

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
    env.write_project_catalog(
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
    env.write_project_catalog(
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
fn linked_worktree_dedupes_home_and_project_documents_with_project_precedence() {
    let temp = TempDir::new().unwrap();
    let docs_home =
        test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    let project = temp.path().join("linked");
    test_git::worktree_add_branch(docs_home.path(), &project, "linked");
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&xdg).unwrap();
    fs::write(
        docs_home.path().join("AGENT_DOCS.toml"),
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\nnotes = \"home\"\n\n[[validation]]\ncontext = \"project-dev\"\ncommands = [\"home-check\"]\nmarker = \".home-ok\"\n",
    )
    .unwrap();
    fs::write(
        project.join("AGENT_DOCS.toml"),
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\nnotes = \"project\"\n\n[[validation]]\ncontext = \"project-dev\"\ncommands = [\"project-check\"]\nmarker = \".project-ok\"\n",
    )
    .unwrap();
    fs::write(project.join("DEVELOPMENT.md"), "# Dev\n").unwrap();

    let out = run_cli(
        &[
            "--docs-home",
            docs_home.path().to_str().unwrap(),
            "--project-path",
            project.to_str().unwrap(),
            "preflight",
            "--intent",
            "project-dev",
            "--format",
            "json",
        ],
        &cmd::CmdOptions::default()
            .with_cwd(&project)
            .with_env("HOME", home.to_str().unwrap())
            .with_env("XDG_CONFIG_HOME", xdg.to_str().unwrap())
            .with_env_remove("AGENT_DOCS_HOME")
            .with_env_remove("PROJECT_PATH"),
    );

    assert!(out.success(), "stdout={} stderr={}", out.stdout, out.stderr);
    let json = out.json();
    assert_eq!(json["documents"].as_array().unwrap().len(), 1);
    assert_eq!(json["documents"][0]["source"], "project");
    assert_eq!(
        json["validation"]["commands"],
        serde_json::json!(["home-check", "project-check"])
    );
    assert_eq!(json["validation"]["marker"], ".project-ok");
}

#[test]
fn intent_scopes_documents() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n\n[[document]]\ncontext = \"task-tools\"\nscope = \"home\"\npath = \"cli-tools.md\"\nrequired = true\n",
    );
    env.write_project_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n",
    );
    env.write_project_doc("DEVELOPMENT.md", "# Dev\n");
    env.write_home_doc("cli-tools.md", "# Tools\n");
    let pd = env.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    assert_eq!(pd.json()["documents"].as_array().unwrap().len(), 1);
    let tt = env.run(&["preflight", "--intent", "task-tools", "--format", "json"]);
    assert_eq!(tt.json()["documents"].as_array().unwrap().len(), 1);
    assert_eq!(tt.json()["documents"][0]["context"], "task-tools");
}

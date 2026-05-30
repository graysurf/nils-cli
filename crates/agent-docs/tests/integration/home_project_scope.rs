//! Repo-scope gate: a `scope = "project"` document entry and a (scopeless,
//! repo-local) `[[validation]]` contract declared in the docs-home catalog are
//! repo-only requirements of the docs-home repository. They must NOT leak into
//! an unrelated git project, but they DO apply when the docs-home and the
//! project are the same repository.

use std::fs;
use std::path::Path;

use nils_test_support::{cmd, git as test_git};

use super::common::{CliOutput, run_cli};

const HOME_CATALOG: &str = "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n";

fn options(cwd: &Path) -> cmd::CmdOptions {
    cmd::CmdOptions::default()
        .with_cwd(cwd)
        .with_env_remove("AGENT_DOCS_HOME")
        .with_env_remove("PROJECT_PATH")
}

fn preflight(docs_home: &Path, project: &Path, extra: &[&str]) -> CliOutput {
    let mut args = vec![
        "--docs-home",
        docs_home.to_str().unwrap(),
        "--project-path",
        project.to_str().unwrap(),
        "preflight",
        "--intent",
        "project-dev",
    ];
    args.extend_from_slice(extra);
    run_cli(&args, &options(project))
}

// A docs-home catalog declaring a project-dev validation contract. Validation
// entries carry no scope and are repo-local, so this contract must not leak
// into an unrelated project.
const HOME_VALIDATION_CATALOG: &str = "[[validation]]\ncontext = \"project-dev\"\ncommands = [\"bash scripts/ci/all.sh\"]\nmarker = \".cache/agent-validation/project-dev.ok\"\n";

fn explain(docs_home: &Path, project: &Path) -> CliOutput {
    run_cli(
        &[
            "--docs-home",
            docs_home.to_str().unwrap(),
            "--project-path",
            project.to_str().unwrap(),
            "explain",
            "--intent",
            "project-dev",
            "--format",
            "json",
        ],
        &options(project),
    )
}

#[test]
fn home_project_scope_entry_is_skipped_for_an_unrelated_repo() {
    // docs-home is one git repo; the project is a different git repo.
    let docs_home =
        test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    fs::write(docs_home.path().join("AGENT_DOCS.toml"), HOME_CATALOG).unwrap();

    let project = test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    // Even though this unrelated project happens to have a DEVELOPMENT.md, the
    // docs-home catalog's project-scope entry must not be imposed on it.
    fs::write(project.path().join("DEVELOPMENT.md"), "# Dev\n").unwrap();

    let out = preflight(docs_home.path(), project.path(), &["--format", "json"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    let docs = json["documents"].as_array().expect("documents");
    assert!(
        docs.is_empty(),
        "docs-home project-scope entry leaked into an unrelated repo: {}",
        out.stdout
    );
    assert_eq!(json["summary"]["required_total"], 0);

    // Strict passes: nothing is required in the unrelated project.
    let strict = preflight(docs_home.path(), project.path(), &["--strict"]);
    assert_eq!(
        strict.code, 0,
        "stdout:\n{}\nstderr:\n{}",
        strict.stdout, strict.stderr
    );
}

#[test]
fn home_project_scope_entry_applies_within_the_docs_home_repo() {
    // docs-home and the project are the SAME repo: the project-scope entry is a
    // requirement of this very repository and must apply.
    let repo = test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    fs::write(repo.path().join("AGENT_DOCS.toml"), HOME_CATALOG).unwrap();

    let out = preflight(repo.path(), repo.path(), &["--format", "json"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    let docs = json["documents"].as_array().expect("documents");
    assert_eq!(
        docs.len(),
        1,
        "the docs-home's own project-scope entry should apply within its repo: {}",
        out.stdout
    );
    assert_eq!(docs[0]["required"], true);
    assert_eq!(docs[0]["status"], "missing");
    assert_eq!(json["summary"]["required_total"], 1);

    // Strict fails because the repo's own required doc is missing.
    let strict = preflight(repo.path(), repo.path(), &["--strict"]);
    assert_eq!(
        strict.code, 1,
        "stdout:\n{}\nstderr:\n{}",
        strict.stdout, strict.stderr
    );
}

#[test]
fn home_validation_contract_is_skipped_for_an_unrelated_repo() {
    // docs-home declares a validation contract; the project is a different repo
    // that declares none of its own. The home contract must not leak in.
    let docs_home =
        test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    fs::write(
        docs_home.path().join("AGENT_DOCS.toml"),
        HOME_VALIDATION_CATALOG,
    )
    .unwrap();

    let project = test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());

    let out = explain(docs_home.path(), project.path());
    assert!(out.success(), "stderr: {}", out.stderr);
    let validation = &out.json()["validation"];
    assert_eq!(
        validation["declared"], false,
        "home validation contract leaked into an unrelated repo: {}",
        out.stdout
    );
    assert_eq!(
        validation["commands"].as_array().expect("commands").len(),
        0
    );
}

#[test]
fn home_validation_contract_applies_within_the_docs_home_repo() {
    // docs-home and the project are the SAME repo: the validation contract is
    // this repository's own and must apply.
    let repo = test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    fs::write(repo.path().join("AGENT_DOCS.toml"), HOME_VALIDATION_CATALOG).unwrap();

    let out = explain(repo.path(), repo.path());
    assert!(out.success(), "stderr: {}", out.stderr);
    let validation = &out.json()["validation"];
    assert_eq!(
        validation["declared"], true,
        "the repo's own validation contract should apply: {}",
        out.stdout
    );
    assert_eq!(validation["commands"][0], "bash scripts/ci/all.sh");
    assert_eq!(
        validation["marker"],
        ".cache/agent-validation/project-dev.ok"
    );
}

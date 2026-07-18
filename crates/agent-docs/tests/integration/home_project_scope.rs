//! Repo-scope gate: a `scope = "project"` document entry and a (scopeless,
//! repo-local) `[[validation]]` contract declared in the docs-home catalog are
//! repo-only requirements of the docs-home repository. They must NOT leak into
//! an unrelated git project, but they DO apply when the docs-home and the
//! project are the same repository.

use std::fs;
use std::path::Path;

use nils_test_support::{cmd, git as test_git};
use tempfile::TempDir;

use super::common::{CliOutput, run_cli};

const HOME_CATALOG: &str = "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\n";

fn run_isolated(args: &[&str], cwd: &Path) -> CliOutput {
    let isolation = TempDir::new_in(cwd.parent().expect("test path has a parent")).unwrap();
    let home = isolation.path().join("home");
    let xdg = isolation.path().join("xdg");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&xdg).unwrap();
    let options = cmd::CmdOptions::default()
        .with_cwd(cwd)
        .with_env("HOME", home.to_str().unwrap())
        .with_env("XDG_CONFIG_HOME", xdg.to_str().unwrap())
        .with_env_remove("AGENT_DOCS_HOME")
        .with_env_remove("PROJECT_PATH");
    run_cli(args, &options)
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
    run_isolated(&args, project)
}

// A docs-home catalog declaring a project-dev validation contract. Validation
// entries carry no scope and are repo-local, so this contract must not leak
// into an unrelated project.
const HOME_VALIDATION_CATALOG: &str = "[[validation]]\ncontext = \"project-dev\"\ncommands = [\"bash scripts/ci/all.sh\"]\nmarker = \".cache/agent-validation/project-dev.ok\"\n";

fn explain(docs_home: &Path, project: &Path) -> CliOutput {
    run_isolated(
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
        project,
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
fn require_declared_intent_rejects_home_project_scope_entry_for_an_unrelated_repo() {
    let docs_home =
        test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    fs::write(docs_home.path().join("AGENT_DOCS.toml"), HOME_CATALOG).unwrap();

    let project = test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());

    let out = preflight(
        docs_home.path(),
        project.path(),
        &["--require-declared-intent", "--format", "json"],
    );
    assert_eq!(
        out.code, 65,
        "docs-home project-scope intent leaked into an unrelated repo: stdout:\n{}\nstderr:\n{}",
        out.stdout, out.stderr
    );
    let json = out.json();
    assert_eq!(json["schema_version"], "cli.agent-docs.preflight.v2");
    assert_eq!(json["error"]["details"]["intent"], "project-dev");
    assert!(
        json["error"]["details"]["available_intents"]
            .as_array()
            .expect("available intents")
            .is_empty(),
        "available intents should be scoped to the unrelated project: {}",
        out.stdout
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
fn require_declared_intent_rejects_home_validation_contract_for_an_unrelated_repo() {
    let docs_home =
        test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    fs::write(
        docs_home.path().join("AGENT_DOCS.toml"),
        HOME_VALIDATION_CATALOG,
    )
    .unwrap();

    let project = test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());

    let out = preflight(
        docs_home.path(),
        project.path(),
        &["--require-declared-intent", "--format", "json"],
    );
    assert_eq!(
        out.code, 65,
        "docs-home validation intent leaked into an unrelated repo: stdout:\n{}\nstderr:\n{}",
        out.stdout, out.stderr
    );
    assert!(
        out.json()["error"]["details"]["available_intents"]
            .as_array()
            .expect("available intents")
            .is_empty()
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

#[test]
fn git_environment_cannot_spoof_docs_home_repository_comparison() {
    let isolation = TempDir::new().unwrap();
    let home = isolation.path().join("home");
    let xdg = isolation.path().join("xdg");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&xdg).unwrap();
    let docs_home =
        test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    fs::write(docs_home.path().join("AGENT_DOCS.toml"), HOME_CATALOG).unwrap();
    let project = test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    fs::write(
        project.path().join("DEVELOPMENT.md"),
        "SPOOFED_SCOPE_MARKER\n",
    )
    .unwrap();

    let out = run_cli(
        &[
            "--docs-home",
            docs_home.path().to_str().unwrap(),
            "--project-path",
            project.path().to_str().unwrap(),
            "preflight",
            "--intent",
            "project-dev",
            "--format",
            "json",
        ],
        &cmd::CmdOptions::default()
            .with_cwd(project.path())
            .with_env("HOME", home.to_str().unwrap())
            .with_env("XDG_CONFIG_HOME", xdg.to_str().unwrap())
            .with_env("GIT_DIR", docs_home.path().join(".git").to_str().unwrap())
            .with_env("GIT_WORK_TREE", project.path().to_str().unwrap())
            .with_env(
                "GIT_COMMON_DIR",
                docs_home.path().join(".git").to_str().unwrap(),
            )
            .with_env_remove("AGENT_DOCS_HOME")
            .with_env_remove("PROJECT_PATH"),
    );

    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    assert!(out.json()["documents"].as_array().unwrap().is_empty());
    assert!(!out.stdout.contains("SPOOFED_SCOPE_MARKER"));
}

fn assert_project_scope_is_skipped(docs_home: &Path, project: &Path, marker: &str) {
    fs::write(docs_home.join("AGENT_DOCS.toml"), HOME_CATALOG).unwrap();
    fs::write(project.join("DEVELOPMENT.md"), marker).unwrap();

    let out = preflight(docs_home, project, &["--format", "json"]);
    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    assert!(out.json()["documents"].as_array().unwrap().is_empty());
    assert_eq!(out.json()["summary"]["required_total"], 0);
    assert!(!out.stdout.contains(marker));
    assert!(!out.stderr.contains(marker));
}

#[test]
fn home_project_scope_is_skipped_for_distinct_non_git_roots() {
    let isolation = TempDir::new().unwrap();
    let docs_home = isolation.path().join("docs-home");
    let project = isolation.path().join("project");
    fs::create_dir_all(&docs_home).unwrap();
    fs::create_dir_all(&project).unwrap();

    assert_project_scope_is_skipped(&docs_home, &project, "NON_GIT_SCOPE_MARKER");
}

#[test]
fn home_project_scope_is_skipped_from_git_home_to_non_git_project() {
    let docs_home =
        test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    let project = TempDir::new().unwrap();

    assert_project_scope_is_skipped(
        docs_home.path(),
        project.path(),
        "GIT_HOME_NON_GIT_PROJECT_SCOPE_MARKER",
    );
}

#[test]
fn home_project_scope_is_skipped_from_non_git_home_to_git_project() {
    let docs_home = TempDir::new().unwrap();
    let project = test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());

    assert_project_scope_is_skipped(
        docs_home.path(),
        project.path(),
        "NON_GIT_HOME_GIT_PROJECT_SCOPE_MARKER",
    );
}

#[test]
fn home_project_scope_applies_when_non_git_roots_are_equal() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("AGENT_DOCS.toml"), HOME_CATALOG).unwrap();

    let out = preflight(root.path(), root.path(), &["--format", "json"]);
    assert_eq!(out.code, 0, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(out.json()["documents"].as_array().unwrap().len(), 1);
    assert_eq!(out.json()["summary"]["required_total"], 1);
    assert_eq!(out.json()["documents"][0]["status"], "missing");
}

#[test]
fn metadata_enumeration_applies_both_root_and_product_filters() {
    let docs_home =
        test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    let project = test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    fs::write(
        docs_home.path().join("AGENT_DOCS.toml"),
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"CODEX.md\"\nproduct = \"codex\"\n\n[[document]]\ncontext = \"task-tools\"\nscope = \"project\"\npath = \"CLAUDE.md\"\nproduct = \"claude\"\n\n[[validation]]\ncontext = \"project-dev\"\ncommands = [\"codex-check\"]\nproduct = \"codex\"\n\n[[validation]]\ncontext = \"claude-only\"\ncommands = [\"claude-check\"]\nproduct = \"claude\"\n",
    )
    .unwrap();

    let list = run_isolated(
        &[
            "--docs-home",
            docs_home.path().to_str().unwrap(),
            "--project-path",
            project.path().to_str().unwrap(),
            "list",
            "--product",
            "claude",
            "--format",
            "json",
        ],
        project.path(),
    );
    assert!(
        list.success(),
        "stdout={} stderr={}",
        list.stdout,
        list.stderr
    );
    assert_eq!(list.json()["intents"], serde_json::json!([]));
    assert_eq!(list.json()["validations"], serde_json::json!([]));

    let explain = run_isolated(
        &[
            "--docs-home",
            docs_home.path().to_str().unwrap(),
            "--project-path",
            project.path().to_str().unwrap(),
            "explain",
            "--product",
            "claude",
            "--format",
            "json",
        ],
        project.path(),
    );
    assert!(
        explain.success(),
        "stdout={} stderr={}",
        explain.stdout,
        explain.stderr
    );
    assert_eq!(explain.json()["intents"], serde_json::json!([]));
}

#[test]
fn require_declared_intent_respects_the_requested_product() {
    let repo = test_git::init_repo_with(test_git::InitRepoOptions::new().with_initial_commit());
    fs::write(
        repo.path().join("AGENT_DOCS.toml"),
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"CODEX.md\"\nproduct = \"codex\"\n",
    )
    .unwrap();

    let out = run_isolated(
        &[
            "--docs-home",
            repo.path().to_str().unwrap(),
            "--project-path",
            repo.path().to_str().unwrap(),
            "preflight",
            "--intent",
            "project-dev",
            "--product",
            "claude",
            "--require-declared-intent",
            "--format",
            "json",
        ],
        repo.path(),
    );

    assert_eq!(out.code, 65, "stdout={} stderr={}", out.stdout, out.stderr);
    assert_eq!(
        out.json()["error"]["details"]["available_intents"],
        serde_json::json!([])
    );
}

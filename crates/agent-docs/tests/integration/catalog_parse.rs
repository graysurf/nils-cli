//! Task 1.1 — the catalog model + parser load contexts and docs from data, and
//! invalid catalogs produce precise (section/index/field) errors.

use super::common::TestEnv;

const EXIT_CONFIG: i32 = 3;

#[test]
fn unknown_document_field_is_a_precise_error() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"DEVELOPMENT.md\"\nbogus = true\n",
    );
    let out = env.run(&["list"]);
    assert_eq!(out.code, EXIT_CONFIG, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("document[0].bogus") && out.stderr.contains("unsupported field"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn unsupported_scope_reports_field() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"world\"\npath = \"DEVELOPMENT.md\"\n",
    );
    let out = env.run(&["list"]);
    assert_eq!(out.code, EXIT_CONFIG, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("document[0].scope") && out.stderr.contains("unsupported scope"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn global_scope_rejected_in_project_catalog() {
    let env = TestEnv::new();
    env.write_project_catalog(
        "[[document]]\ncontext = \"task-tools\"\nscope = \"global\"\npath = \"x.md\"\n",
    );
    let out = env.run(&["list"]);
    assert_eq!(out.code, EXIT_CONFIG, "stderr: {}", out.stderr);
    assert!(
        out.stderr
            .contains("global scope is allowed only in the home catalog"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn missing_required_field_reports_field() {
    let env = TestEnv::new();
    env.write_home_catalog("[[document]]\nscope = \"home\"\npath = \"DEVELOPMENT.md\"\n");
    let out = env.run(&["list"]);
    assert_eq!(out.code, EXIT_CONFIG, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("document[0].context") && out.stderr.contains("missing required field"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn invalid_when_predicate_reports_field() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"DEVELOPMENT.md\"\nwhen = \"file-exists:Cargo.toml\"\n",
    );
    let out = env.run(&["list"]);
    assert_eq!(out.code, EXIT_CONFIG, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("document[0].when") && out.stderr.contains("unsupported atom"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn validation_requires_commands() {
    let env = TestEnv::new();
    env.write_home_catalog("[[validation]]\ncontext = \"project-dev\"\n");
    let out = env.run(&["list"]);
    assert_eq!(out.code, EXIT_CONFIG, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("validation[0].commands")
            && out.stderr.contains("missing required field"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn toml_syntax_error_includes_line_and_column() {
    let env = TestEnv::new();
    env.write_home_catalog("[[document]\ncontext = \"x\"\n");
    let out = env.run(&["list"]);
    assert_eq!(out.code, EXIT_CONFIG, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("[parse]") && out.stderr.contains(":1:"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn valid_catalog_loads_documents_as_data() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"task-tools\"\nscope = \"home\"\npath = \"core/policies/cli-tools.md\"\nrequired = true\nnotes = \"tool selection\"\n",
    );
    env.write_home_doc("core/policies/cli-tools.md", "# CLI tools\n\nguidance\n");
    let out = env.run(&["list", "--format", "json"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    let docs = json["documents"].as_array().expect("documents array");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["context"], "task-tools");
    assert_eq!(docs[0]["scope"], "home");
}

#[test]
fn product_field_accepts_string_and_list_forms() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"CODEX.md\"\nproduct = \"codex\"\n\n[[validation]]\ncontext = \"project-dev\"\ncommands = [\"codex-check\"]\nproduct = [\"codex\", \"claude\"]\n",
    );
    env.write_home_doc("CODEX.md", "# Codex\n");

    let out = env.run(&["list", "--format", "json"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    assert_eq!(json["documents"][0]["products"][0], "codex");
    assert_eq!(json["validations"].as_array().unwrap().len(), 1);
}

#[test]
fn invalid_product_field_is_a_precise_error() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"BAD.md\"\nproduct = \"vscode\"\n",
    );

    let out = env.run(&["list"]);
    assert_eq!(out.code, EXIT_CONFIG, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("document[0].product") && out.stderr.contains("unsupported product"),
        "stderr: {}",
        out.stderr
    );
}

//! Task 1.1 — the catalog model + parser load contexts and docs from data, and
//! invalid catalogs produce precise (section/index/field) errors.

use nils_test_support::cmd;

use super::common::{TestEnv, run_cli};

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

    let docs_home = env.docs_home.to_str().expect("UTF-8 docs-home");
    let options = cmd::CmdOptions::default().with_cwd(&env.docs_home);
    let out = run_cli(
        &[
            "--docs-home",
            docs_home,
            "--project-path",
            docs_home,
            "list",
            "--format",
            "json",
        ],
        &options,
    );
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

#[test]
fn phase_field_accepts_string_and_list_forms() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"ONE.md\"\nphase = \"edit\"\n\n[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"MANY.md\"\nphase = [\"edit\", \"review\"]\n",
    );
    env.write_home_doc("ONE.md", "# One\n");
    env.write_home_doc("MANY.md", "# Many\n");

    let out = env.run(&["list", "--format", "json"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    let docs = json["documents"].as_array().unwrap();
    let one = docs
        .iter()
        .find(|doc| doc["path"].as_str().unwrap().ends_with("ONE.md"))
        .unwrap();
    assert_eq!(one["phases"][0], "edit", "{json}");
    let many = docs
        .iter()
        .find(|doc| doc["path"].as_str().unwrap().ends_with("MANY.md"))
        .unwrap();
    assert_eq!(
        many["phases"],
        serde_json::json!(["edit", "review"]),
        "{json}"
    );
}

#[test]
fn no_phase_document_omits_phases_field() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"PLAIN.md\"\n",
    );
    env.write_home_doc("PLAIN.md", "# Plain\n");

    let out = env.run(&["list", "--format", "json"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    let doc = &json["documents"][0];
    assert!(
        doc.get("phases").is_none(),
        "a document with no phase must omit the phases field: {json}"
    );
}

#[test]
fn invalid_phase_field_is_a_precise_error() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"BAD.md\"\nphase = \"bad phase\"\n",
    );

    let out = env.run(&["list"]);
    assert_eq!(out.code, EXIT_CONFIG, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("document[0].phase") && out.stderr.contains("unsupported character"),
        "stderr: {}",
        out.stderr
    );
}

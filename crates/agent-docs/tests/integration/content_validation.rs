//! Task 2.2 — content validation: a zero-byte or marker-less required doc is
//! reported invalid (and fails strict audit/preflight), even though it exists.

use super::common::TestEnv;

fn marker_catalog() -> &'static str {
    "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\nmarker = \"## Validation\"\n"
}

#[test]
fn zero_byte_required_doc_is_invalid() {
    let env = TestEnv::new();
    env.write_home_catalog(marker_catalog());
    env.write_project_doc("DEVELOPMENT.md", "");
    let out = env.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    let json = out.json();
    let doc = &json["documents"][0];
    assert_eq!(doc["status"], "present");
    assert_eq!(doc["validation"]["non_empty"], false);
    assert_eq!(doc["validation"]["valid"], false);
    assert_eq!(json["summary"]["invalid_required"], 1);

    let strict = env.run(&["preflight", "--intent", "project-dev", "--strict"]);
    assert_eq!(strict.code, 1, "empty doc should fail strict");
}

#[test]
fn marker_less_required_doc_is_invalid() {
    let env = TestEnv::new();
    env.write_home_catalog(marker_catalog());
    env.write_project_doc("DEVELOPMENT.md", "# Dev\n\nno validation section here\n");
    let out = env.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    let doc = &out.json()["documents"][0];
    assert_eq!(doc["validation"]["marker_present"], false);
    assert_eq!(doc["validation"]["valid"], false);
}

#[test]
fn valid_doc_passes() {
    let env = TestEnv::new();
    env.write_home_catalog(marker_catalog());
    env.write_project_doc(
        "DEVELOPMENT.md",
        "# Dev\n\n## Validation\n\nrun the tests\n",
    );
    let out = env.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    let doc = &out.json()["documents"][0];
    assert_eq!(doc["validation"]["marker_present"], true);
    assert_eq!(doc["validation"]["valid"], true);

    let strict = env.run(&["preflight", "--intent", "project-dev", "--strict"]);
    assert!(
        strict.success(),
        "valid doc should pass strict: {}",
        strict.stderr
    );
}

#[test]
fn audit_flags_invalid_required_doc() {
    let env = TestEnv::new();
    env.write_home_catalog(marker_catalog());
    env.write_project_doc("DEVELOPMENT.md", "");
    let out = env.run(&["audit", "--target", "project", "--format", "json"]);
    let json = out.json();
    assert!(
        json["problems"].as_u64().unwrap() >= 1,
        "audit json: {}",
        out.stdout
    );
    let strict = env.run(&["audit", "--target", "project", "--strict"]);
    assert_eq!(
        strict.code, 1,
        "strict audit should fail: {}",
        strict.stdout
    );
}

#[test]
fn preflight_product_filter_excludes_other_product_required_doc() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"home\"\npath = \"CODEX.md\"\nrequired = true\nproduct = \"codex\"\n",
    );

    let claude = env.run(&[
        "preflight",
        "--intent",
        "project-dev",
        "--product",
        "claude",
        "--strict",
        "--format",
        "json",
    ]);
    assert!(claude.success(), "stderr: {}", claude.stderr);
    assert_eq!(claude.json()["summary"]["required_total"], 0);

    let codex = env.run(&[
        "preflight",
        "--intent",
        "project-dev",
        "--product",
        "codex",
        "--strict",
        "--format",
        "json",
    ]);
    assert_eq!(
        codex.code, 1,
        "stdout:\n{}\nstderr:\n{}",
        codex.stdout, codex.stderr
    );
    assert_eq!(codex.json()["summary"]["missing_required"], 1);
}

#[test]
fn audit_product_filter_excludes_other_product_required_doc() {
    let env = TestEnv::new();
    env.write_project_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"CODEX.md\"\nrequired = true\nproduct = \"codex\"\n",
    );

    let claude = env.run(&[
        "audit",
        "--target",
        "project",
        "--product",
        "claude",
        "--strict",
        "--format",
        "json",
    ]);
    assert!(claude.success(), "stderr: {}", claude.stderr);
    assert_eq!(claude.json()["problems"], 0);

    let codex = env.run(&[
        "audit",
        "--target",
        "project",
        "--product",
        "codex",
        "--strict",
        "--format",
        "json",
    ]);
    assert_eq!(
        codex.code, 1,
        "stdout:\n{}\nstderr:\n{}",
        codex.stdout, codex.stderr
    );
    let codex_json = codex.json();
    assert_eq!(codex_json["schema_version"], "agent-docs.audit.v2");
    let codex_docs = codex_json["documents"].as_array().unwrap();
    assert_eq!(codex_docs.len(), 1);
    assert_eq!(codex_docs[0]["products"], serde_json::json!(["codex"]));
    assert_eq!(codex_json["problems"], 1);
}

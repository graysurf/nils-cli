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

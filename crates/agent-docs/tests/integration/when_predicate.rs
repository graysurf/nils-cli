//! Task 2.1 — `when` predicates. A docs-only repo auto-skips a code doc with no
//! manual opt-out; a repo carrying the marker requires it. `||` / `&&` compose.

use super::common::TestEnv;

fn code_doc_catalog() -> &'static str {
    "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\nwhen = \"path-exists:Cargo.toml || path-exists:package.json\"\n"
}

#[test]
fn docs_only_repo_auto_skips_code_doc() {
    let env = TestEnv::new();
    env.write_home_catalog(code_doc_catalog());
    env.write_project_catalog(code_doc_catalog());
    // No Cargo.toml / package.json, and DEVELOPMENT.md absent. The predicate is
    // false, so the doc is not required and strict passes with no opt-out.
    let out = env.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    assert!(out.success(), "stderr: {}", out.stderr);
    let json = out.json();
    let doc = &json["documents"][0];
    assert_eq!(doc["when_satisfied"], false);
    assert_eq!(doc["required"], false);
    assert_eq!(json["summary"]["required_total"], 0);

    let strict = env.run(&["preflight", "--intent", "project-dev", "--strict"]);
    assert!(
        strict.success(),
        "docs-only repo should pass strict: {}",
        strict.stderr
    );
}

#[test]
fn code_repo_requires_the_code_doc() {
    let env = TestEnv::new();
    env.write_home_catalog(code_doc_catalog());
    env.write_project_catalog(code_doc_catalog());
    env.write_project_doc("Cargo.toml", "[package]\nname = \"x\"\n");
    // Marker present but DEVELOPMENT.md missing -> required and unsatisfied.
    let out = env.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    let json = out.json();
    assert_eq!(json["documents"][0]["when_satisfied"], true);
    assert_eq!(json["documents"][0]["required"], true);

    let strict = env.run(&["preflight", "--intent", "project-dev", "--strict"]);
    assert_eq!(
        strict.code, 1,
        "code repo missing the doc should fail strict"
    );
}

#[test]
fn and_composition_requires_all_atoms() {
    let env = TestEnv::new();
    env.write_home_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\nwhen = \"path-exists:Cargo.toml && path-exists:src/**\"\n",
    );
    env.write_project_catalog(
        "[[document]]\ncontext = \"project-dev\"\nscope = \"project\"\npath = \"DEVELOPMENT.md\"\nrequired = true\nwhen = \"path-exists:Cargo.toml && path-exists:src/**\"\n",
    );
    // Only Cargo.toml, no src/** -> AND clause false -> not required.
    env.write_project_doc("Cargo.toml", "[package]\n");
    let out = env.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    assert_eq!(out.json()["documents"][0]["when_satisfied"], false);

    // Add src/lib.rs -> both atoms true -> required.
    env.write_project_doc("src/lib.rs", "fn main() {}\n");
    let out2 = env.run(&["preflight", "--intent", "project-dev", "--format", "json"]);
    assert_eq!(out2.json()["documents"][0]["when_satisfied"], true);
}
